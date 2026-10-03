# hw-kit — COVERAGE-NOTES.md
#
# Estate doctrine (engineering-standards COVERAGE.md): environment-bound
# code paths are documented here as env-gated exceptions, and the CI floor
# measures everything that is measurable on a shared runner.

## Tier

**Tier B** (owner decision #1, spec §Decisions — "env-bound coverage
reality"). hw-kit is a syscall-thin wrapper whose *success* paths require
hardware facilities a shared runner does not have; the crate's
parse/validate/error-taxonomy logic — the logic that can actually rot — is
measured at full floor on host CI.

## Env-gated integration tests

| variable | gates | what it unlocks |
|---|---|---|
| `HW_KIT_PIN_TEST=1` | `tests/pinning.rs::pinning_sets_affinity_then_reports_overlap` | sched_setaffinity round trip (pin → read-back → isolation scan → restore) |
| `HW_KIT_HW_TEST=1` | `tests/numa.rs::alloc_on_node_places_physically` | mmap+mbind success path, cross-checked against `/proc/self/numa_maps` |
| `HW_KIT_HUGEPAGE_TEST=1` | `tests/hugepage.rs::one_gib_reservation_round_trip` | 1 GiB hugepage pool reservation |

Gated tests **skip gracefully** (typed skip notes on stdout) when the
variable is unset — and additionally guard on facility presence
(`/sys/devices/system/cpu/possible`, `/sys/kernel/mm/hugepages/*`,
node counts) so a provisioned runner that lacks a facility still fails
nothing. CI runs them on the `hardware` job (`[self-hosted, linux]`,
`continue-on-error: true` — non-blocking until the runner is provisioned;
**provisioning the runner is a pre-publication acceptance criterion**,
tracked as a repo issue).

## Failure-path coverage is a hard gate

Per spec §Gate plan ("guaranteed-refused endpoint" doctrine), **every typed
error arm has an always-runnable test**:

| error arm | always-runnable test |
|---|---|
| `TopologyError::Io` | `absent_cpu_tree_is_a_typed_io_error` (lib), `nonexistent_root_is_typed_io` |
| `TopologyError::MissingField` | `missing_required_field_is_typed`, `missing_node_cpulist_is_typed` (lib) |
| `TopologyError::Malformed` | `malformed_entries_are_typed_never_guessed`, `empty_sibling_list_is_typed` (lib) |
| `PinError::EmptySet` | `pinning_rejects_empty_set_typed` |
| `PinError::Syscall` | `pinning_impossible_cpu_fails_typed`, `numa_bind_unmapped_range_is_typed_syscall` |
| `PinError::NoSuchTask` | `pinning_dead_tid_is_typed_esrch` |
| `PinError::AffinityMismatch` | read-back verification in `pin_current`/`pin_thread`; mismatch requires a narrowing host (cpuset/offline CPUs) — exercised on the hardware runner by the gated round trip's assertion contract; the arm is one branch in an always-run function (covered by llvm-cov line metrics) |
| `PinError::NotIsolated` | `overlap_is_reported_leader_and_threads` → `ensure_clean` |
| `PinError::MalformedProc` | `malformed_status_is_typed_not_skipped` |
| `PinError::Scan` | `missing_proc_root_is_typed_scan_error` |
| `NumaError::InvalidRegion` | `numa_bind_fails_closed_on_bad_policy`, `alloc_zero_length_is_typed` |
| `NumaError::Syscall` | `numa_bind_unmapped_range_is_typed_syscall`, `alloc_on_impossible_node_is_typed` |
| `NumaError::UnsupportedTarget` | `alloc_beyond_node_capacity_is_typed` |
| `HugetlbError::ZeroLength` | `zero_length_is_typed` |
| `HugetlbError::UnalignedLength` | `unaligned_length_is_typed` |
| `HugetlbError::UnsupportedSize` | `one_gib_class_is_typed_where_absent` |
| `HugetlbError::Unavailable` | `hugepage_exhaustion_is_typed_no_silent_fallback` |
| `HwError::UnsupportedTarget` | `out_of_range_core_is_typed_hwerror` + proptest `core_id_validation_is_exact_over_arbitrary_u16s` |

## Gate applicability (recorded rationale)

- **miri** — the FFI modules (`affinity`, `numa`, `hugepage`) execute real
  syscalls miri cannot model: n/a for those. The pure-`std` sysfs and
  `/proc` parsing *is* miri-eligible and runs in CI
  (`miri-parse` job, `-Zmiri-disable-isolation` for fixture file access).
- **loom** — n/a: no lock-free shared-memory primitives (the crate's
  concurrency surface is `sched_setaffinity`, which the kernel serializes).
- **criterion** — optional per §Gate plan: discovery and pinning are
  one-shot cold-path operations; the hot path is the consumer's memory.
  Deviation from the Tier-A latency-relevant default recorded here.

## Isolation report scope

v1 is `/proc`-only (owner decision #4): the scan reads
`/proc/<pid>/task/<tid>/status` `Cpus_allowed_list`; cgroup cpuset
controllers are not consulted. The advisory semantics (skipped-entry
blind spots, self-exclusion, sibling-thread counting) are documented in
`src/isolation.rs` and asserted by the fixture tests.
