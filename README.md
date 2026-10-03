# hw-kit

Hardware topology and control for Rust — CPU/NUMA discovery, core pinning,
NUMA binding, hugepage reservation. **Linux-only, by declaration** (L1
substrate: everything above it may assume the platform).

hw-kit is a *wrapper, not a platform library*: typed errors and fail-closed
validation over raw syscalls and sysfs — no policy engine, no daemon, no
cgroup manipulation, no libnuma, no hwloc. Parsing is pure `std`; `libc`
(the crate's only dependency) is feature-gated.

- **Topology discovery** (`hw-discovery`, default): cores, packages, NUMA
  nodes, and SMT sibling sets parsed from `/sys/devices/system/cpu` with
  zero dependencies. Absent or unparseable entries are typed errors or
  explicit `Option` — never guessed defaults.
- **Core pinning** (`libc`): `sched_setaffinity` for the current thread or
  any TID, with post-pin read-back verification — a silently narrowed mask
  (cpuset, offline CPUs) is a typed error, not a success. Typed `ESRCH`
  for dead targets.
- **Isolation verification** (`libc`): best-effort scans of
  `/proc/*/status` (`Cpus_allowed_list`, including per-thread entries)
  that report every foreign task overlapping a set. Advisory by contract:
  overlap *is* reported; absence of evidence is never isolation.
- **NUMA hooks** (`numa`): the documented raw `mmap`+`mbind` hook plus
  exactly two blessed safe wrappers — `MPOL_BIND` (hard placement) and
  `MPOL_PREFERRED` (hint) — with RAII regions (drop = `munmap`).
- **Hugepages** (`hugepage`): `mmap(MAP_HUGETLB)` with explicit 2 MiB /
  1 GiB size classes. Exhaustion is a typed error — **there is no silent
  fallback to 4 KiB pages**, because a silent fallback would corrupt the
  latency assumptions of every consumer downstream.
- **Lint posture**: `unsafe` is inherent here (FFI substrate) and *not*
  forbidden — instead every unsafe block carries a `// SAFETY:`
  justification (`clippy::undocumented_unsafe_blocks = deny`,
  `unsafe_op_in_unsafe_fn = deny`) and the crate docs audit every site
  (see `src/lib.rs` § Safety).

## Install

```toml
[dependencies]
hw-kit = "0.1"
```

Any non-Linux target fails to compile with an explicit `compile_error!` —
the platform is part of the contract, not an accident.

## Features

| feature | default | pulls | surface |
|---|---|---|---|
| `hw-discovery` | ✅ | nothing (pure `std`) | `discover`, `discover_at`, `CpuTopology` |
| `libc` | — | `libc` | `pin_current`, `pin_thread`, `pin_current_core`, `pin_thread_core`, `current_set`, `verify_isolated`, `verify_current_isolated` |
| `numa` | — | `libc` | `numa_bind_region` (raw, `unsafe`), `alloc_on_node`, `alloc_preferred_on`, `NodeRegion`, `NumaPolicy`, `NodeSet` |
| `hugepage` | — | `libc` | `hugepage_reserve`, `HugeRegion`, `HugeSize` |

REQ-HW-006: `libc` is the only runtime dependency, and the default feature
set builds with **zero** dependencies. CI asserts the tree
(`deps-are-libc-only` job: all features → exactly `libc`; default features
→ nothing).

## Example

```no_run
# fn main() -> Result<(), Box<dyn std::error::Error>> {
use hw_kit::{CoreId, CpuSet, CpuId, pin_current_core};

// 1. Discover the machine (pure std).
let topo = hw_kit::discover()?;
for node in &topo.numa_nodes {
    println!("node{}: {}", node.id, node.cpus.allowed_list());
}

// 2. Pin this thread to an allowed core, with read-back verification.
let original = hw_kit::current_set()?;
let core = CoreId::new(original.iter().next().expect("cpu").0 as u16)?;
pin_current_core(core)?;
assert_eq!(hw_kit::current_set()?, CpuSet::single(CpuId(core.get() as u32)));

// 3. Advisory isolation check: who else is on our core?
let report = hw_kit::verify_isolated(&CpuSet::single(CpuId(core.get() as u32)))?;
for task in &report.overlapping_tasks {
    println!("foreign task on our core: {task}");
}
hw_kit::pin_current(&original)?;
# Ok(())
# }
```

NUMA and hugepages (feature-gated):

```no_run
# fn main() -> Result<(), Box<dyn std::error::Error>> {
use hw_kit::cpu::NodeId;
use hw_kit::hugepage::{hugepage_reserve, HugeSize};
use hw_kit::numa::alloc_on_node;

// Hard-bound, physically placed at return (mapped, mbind(MPOL_BIND), touched).
let region = hw_kit::numa::alloc_on_node(64 * 1024, NodeId(0))?;
println!("{} bytes bound to {}", region.len(), region.node());

// Hugepage-backed or nothing — exhaustion is typed, never a 4 KiB fallback.
match hugepage_reserve(2 * 1024 * 1024, HugeSize::Default2M) {
    Ok(hp) => println!("{} huge-backed bytes", hp.len()),
    Err(e) => println!("no hugepages: {e}"),
}
# Ok(())
# }
```

## Error taxonomy

Domain-typed and `#[non_exhaustive]`, all convertible into the crate-wide
[`HwError`](https://docs.rs/hw-kit): `TopologyError` (Io / MissingField /
Malformed), `PinError` (EmptySet / Syscall / NoSuchTask / AffinityMismatch /
NotIsolated / MalformedProc / Scan), `NumaError` (InvalidRegion / Syscall /
UnsupportedTarget), `HugetlbError` (ZeroLength / UnalignedLength /
UnsupportedSize / Unavailable / Syscall). Every error arm has at least one
always-runnable test (see `COVERAGE-NOTES.md` — the "guaranteed-refused
endpoint" doctrine).

## Platform

Linux kernel ≥ 3.17 (`/proc/thread-self` for scan self-exclusion);
NUMA/hugepage facilities are probed, never assumed. GNU and musl are both
built and tested in CI; non-Linux targets fail with the `compile_error!`
gate (asserted in CI, not left to chance).

## Testing posture

- Hermetic: topology and isolation parsing run against committed fixture
  trees (`tests/fixtures/sysfs/*`, `tests/fixtures/proc/*`), including
  malformed ones — the live host is only ever smoke-tested.
- Property-tested: `CoreId` validation over arbitrary `u16`s,
  allowed-list round trips, range exactness (`tests/properties.rs`).
- Env-gated hardware tests (`HW_KIT_PIN_TEST`, `HW_KIT_HW_TEST`,
  `HW_KIT_HUGEPAGE_TEST`) run on the self-hosted runner job and skip
  gracefully everywhere else; the failure-path gate runs everywhere. See
  [COVERAGE-NOTES.md](COVERAGE-NOTES.md) (Tier B posture).
- miri checks the pure-`std` parsing modules; the FFI modules are n/a
  (rationale recorded).

## Safety

See the audit of all ten unsafe sites in
[`src/lib.rs`](src/lib.rs) § Safety, and
[SECURITY.md](SECURITY.md) for the threat model.

## License

MIT OR Apache-2.0
