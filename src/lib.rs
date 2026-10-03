//! hw-kit — hardware topology and control for the `WyattAu` estate
//! (L1 substrate: CPU/NUMA discovery, core pinning, NUMA binding,
//! hugepage reservation). **Linux-only, by declaration** — everything above
//! this layer may assume the platform.
//!
//! hw-kit is a *wrapper, not a platform library* (spec §Semantics): typed
//! errors and fail-closed validation over raw syscalls and sysfs — no
//! policy engine, no daemon, no cgroup manipulation, no libnuma, no hwloc.
//!
//! # Surface
//!
//! - [`discover`] — CPU topology (cores, packages, NUMA nodes, SMT sibling
//!   sets) parsed from `/sys/devices/system/cpu` with pure `std`
//!   (REQ-HW-001, feature `hw-discovery`, default). Absent/unparseable
//!   entries are typed errors or explicit `Option` — never guessed.
//! - [`pin_current`] / [`pin_thread`] (single-core forms
//!   [`pin_current_core`] / [`pin_thread_core`]) — `sched_setaffinity`
//!   pinning with post-pin read-back verification, and
//!   [`verify_isolated`] — best-effort isolation validation over
//!   `/proc/*/status` (REQ-HW-002, feature `libc`).
//! - [`numa_bind_region`] — the raw documented `mmap`+`mbind` hook, plus
//!   the two blessed safe wrappers [`alloc_on_node`] /
//!   [`alloc_preferred_on`] (REQ-HW-003, feature `numa`).
//! - [`hugepage_reserve`] — `mmap(MAP_HUGETLB)` with size-class selection;
//!   exhaustion is a typed error, **never a silent 4 KiB fallback**
//!   (REQ-HW-004, feature `hugepage`).
//!
//! # Feature map
//!
//! | feature | default | enables |
//! |---|---|---|
//! | `hw-discovery` | ✅ | [`discover`] — pure `std`, zero dependencies |
//! | `libc` | — | pinning + isolation surface (implies the `libc` dep) |
//! | `numa` | — | [`numa`] module (implies `libc`) |
//! | `hugepage` | — | [`hugepage`] module (implies `libc`) |
//!
//! REQ-HW-006: `libc` is the crate's only dependency, feature-gated; the
//! default feature set builds with zero dependencies, and CI asserts the
//! dependency tree (`deps-are-libc-only`).
//!
//! # Quickstart
//!
//! ```
//! let topo = hw_kit::discover().expect("host topology");
//!
//! // SMT siblings of the first CPU, kernel-format:
//! let first = topo.cpus.first().expect("at least one cpu");
//! println!("{} siblings: {}", first.id, first.siblings.allowed_list());
//!
//! // Nothing here guesses: a host without a NUMA node tree reports
//! // `node: None`, not node 0.
//! assert!(topo.cpus.iter().all(|c| c.node.is_some() == topo.node_of(c.id).is_some()));
//! ```
//!
//! # Isolation semantics (advisory)
//!
//! [`verify_isolated`] reports foreign tasks whose affinity overlaps a set.
//! A clean report is *evidence at scan time*, not a guarantee: the crate
//! never enforces, `/proc` entries it cannot read are counted as skipped
//! blind spots, and the calling task is excluded (every other task —
//! including sibling threads of the same process — counts as foreign). See
//! the [`isolation`] module docs; fail-closed means overlap *is* reported.
//!
//! # Platform
//!
//! Linux only, by declaration: the whole library targets Linux and every
//! other target fails to compile with a [`compile_error!`] explaining why —
//! the `uring-kit` posture (REQ-HW-005). The platform matrix in CI builds
//! gnu + musl and asserts the compile failure on non-Linux targets.
//!
//! # Safety
//!
//! This crate contains inherent `unsafe` (FFI into real syscalls).
//! `unsafe_code` is therefore *not* forbidden crate-wide — the documented
//! posture (mirroring `uring-kit`) is: every `unsafe` block carries a
//! `// SAFETY:` justification (`clippy::undocumented_unsafe_blocks = deny`,
//! `unsafe_op_in_unsafe_fn = deny`) and every site is audited here:
//!
//! 1. **`sched_getaffinity` — mask read** (`affinity.rs`). *Invariant:* the
//!    bitmap buffer is live, correctly sized, pre-zeroed; the kernel writes
//!    at most the passed byte length; trailing words stay zero so
//!    `CpuSet::from_words` yields a normalized mask.
//! 2. **`sched_setaffinity` — mask write** (`affinity.rs`). *Invariant:*
//!    the bitmap is live for the call; its exact byte length is passed; the
//!    kernel does not retain the pointer. A dead target returns `ESRCH`,
//!    mapped to the typed `NoSuchTask`.
//! 3. **`alloc_on_node` / `alloc_preferred_on` — `mmap`** (`numa.rs`).
//!    *Invariant:* anonymous private memory with fd −1 / offset 0; failure
//!    is `MAP_FAILED` with nothing mapped, surfaced as a typed error.
//! 4. **`SYS_mbind` raw syscall** (`numa.rs`). *Invariant:* arguments are
//!    passed at their exact ABI widths — address, length, mode, node
//!    bitmap, bitmap bit-width, flags 0; the kernel reads only the
//!    described bytes and retains nothing. The caller's obligations for the
//!    raw hook are in `numa_bind_region`'s `# Safety` section.
//! 5. **`numa_bind_region` call from `alloc_with_policy`** (`numa.rs`).
//!    *Invariant:* the region was just mapped at `len` bytes and is owned;
//!    the policy is non-empty — the hook's contract is met at the only
//!    internal call site.
//! 6. **populate touch** (`numa.rs`). *Invariant:* `ptr::write_bytes` over
//!    the freshly mapped, owned `len` bytes touches each page once so the
//!    `mbind` policy is physically applied before hand-off.
//! 7. **`hugepage_reserve` — `mmap(MAP_HUGETLB | MAP_POPULATE)`**
//!    (`hugepage.rs`). *Invariant:* anonymous private hugetlb mapping with
//!    explicit size class; failure is `MAP_FAILED` (typed — no fallback);
//!    success means the kernel populated every page (no later `SIGBUS`).
//! 8. **`munmap` in `NodeRegion`/`HugeRegion` drop** (`numa.rs`,
//!    `hugepage.rs`). *Invariant:* the pointer came from the crate's own
//!    `mmap` at exactly the stored length and is unmapped exactly once.
//! 9. **`Deref`/`DerefMut` slice views** (`numa.rs`, `hugepage.rs`).
//!    *Invariant:* the slice borrows for `&self`/`&mut self`, inside the
//!    mapping's lifetime; `u8` alignment holds for any mmap.
//! 10. **`Send`/`Sync` for `NodeRegion`/`HugeRegion`** (`numa.rs`,
//!     `hugepage.rs`). *Invariant:* the type owns one process-private
//!     anonymous mapping with no shared state; moving it moves ownership,
//!     exactly like a `Vec<u8>`.
//!
//! miri cannot execute the FFI paths; the pure-`std` sysfs and `/proc`
//! parsing *is* miri-checked in CI (spec §Gate plan).

#![cfg_attr(not(target_os = "linux"), allow(unused))]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic
    )
)]

#[cfg(not(target_os = "linux"))]
compile_error!("hw-kit is Linux-only; this crate intentionally does not build elsewhere");

#[cfg(all(target_os = "linux", feature = "libc"))]
pub mod affinity;
pub mod cpu;
pub mod error;
#[cfg(all(target_os = "linux", feature = "hugepage"))]
pub mod hugepage;
pub mod isolation;
#[cfg(all(target_os = "linux", feature = "numa"))]
pub mod numa;
#[cfg(feature = "hw-discovery")]
pub mod topology;

pub use cpu::{CoreId, CpuId, CpuSet, CpuSetParseError, NodeId, PackageId, SmallCpuList};
pub use error::{HugetlbError, HwError, NumaError, PinError, TopologyError};
pub use isolation::{verify_isolated, verify_isolated_at, IsolationReport, OverlappingTask};

#[cfg(all(target_os = "linux", feature = "libc"))]
pub use affinity::{
    current_set, pin_current, pin_current_core, pin_thread, pin_thread_core,
    verify_current_isolated,
};
#[cfg(all(target_os = "linux", feature = "hugepage"))]
pub use hugepage::{hugepage_reserve, HugeRegion, HugeSize};
#[cfg(all(target_os = "linux", feature = "numa"))]
pub use numa::{
    alloc_on_node, alloc_preferred_on, numa_bind_region, MpolMode, NodeRegion, NodeSet, NumaPolicy,
};
#[cfg(feature = "hw-discovery")]
pub use topology::{discover, discover_at, CpuInfo, CpuTopology, NodeInfo};
