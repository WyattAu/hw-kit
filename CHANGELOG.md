# Changelog

All notable changes to this project are documented here. Format: [Keep a
Changelog](https://keepachangelog.com/) — versions follow [semver](https://semver.org).

## [0.1.0] - 2026-10-03

### Added

- Initial release, per `specs/hw-kit.md` (engineering-standards): the
  estate's CPU-topology and placement substrate — Linux-only by
  declaration (`compile_error!` on non-Linux, the uring-kit posture).
- `discover` / `discover_at` (REQ-HW-001, feature `hw-discovery`,
  default): pure-`std` parse of `/sys/devices/system/cpu` +
  `/sys/devices/system/node` into a typed `CpuTopology` (packages, cores,
  NUMA nodes, SMT sibling sets). Absent/unparseable entries are typed
  errors or explicit `Option` — never guessed defaults.
- `pin_current` / `pin_thread` (+ single-core forms `pin_current_core` /
  `pin_thread_core`) (REQ-HW-002, feature `libc`): `sched_setaffinity`
  with post-pin read-back verification (`AffinityMismatch` is fail-closed)
  and typed `ESRCH` (`NoSuchTask`). `current_set` exposes the effective
  mask.
- `verify_isolated` / `verify_isolated_at` / `verify_current_isolated`
  (REQ-HW-002): best-effort isolation validation over `/proc/*/status`
  `Cpus_allowed_list`, including per-thread entries — advisory report
  (`IsolationReport` with skipped-entry blind-spot accounting,
  self-exclusion, `ensure_clean` verdict). v1 is `/proc`-only (owner
  decision #4).
- `numa_bind_region` (raw documented `unsafe` hook) + `alloc_on_node` /
  `alloc_preferred_on` (the two blessed policies, owner decision #3)
  (REQ-HW-003, feature `numa`): `mmap` + `mbind(MPOL_BIND`/`MPOL_PREFERRED)`
  with a placement touch, RAII `NodeRegion` (drop = `munmap`), typed
  `NumaError`s.
- `hugepage_reserve` (REQ-HW-004, feature `hugepage`): `mmap(MAP_HUGETLB)`
  with explicit 2 MiB / 1 GiB size classes and `MAP_POPULATE` —
  exhaustion/absence is a typed `HugetlbError`; **no silent 4 KiB
  fallback**.
- Error taxonomy: domain-typed `TopologyError` / `PinError` / `NumaError` /
  `HugetlbError`, all convertible into the crate-wide `HwError`
  (`#[non_exhaustive]`; hand-written `Display`/`Error` impls — REQ-HW-006
  keeps the dependency surface at `libc` only).
- Tests: hermetic fixture-tree parsing (incl. malformed sysfs and `/proc`
  trees), fail-closed error-arm coverage for every typed arm, proptests
  over `CoreId`/`CpuSet`/`NodeSet`/`SmallCpuList`, env-gated hardware tests
  (`HW_KIT_PIN_TEST`, `HW_KIT_HW_TEST`, `HW_KIT_HUGEPAGE_TEST`) — Tier B
  posture per spec §Decisions; see `COVERAGE-NOTES.md`.
- CI: shared Tier-B gate + dedicated miri (pure-std parsing), musl build,
  non-Linux build-**failure** assertion, `deps-are-libc-only` cargo-tree
  gate, and the self-hosted-runner hardware job (non-blocking until
  provisioned; tracked as a repo issue per spec §Risk register).

[0.1.0]: https://github.com/WyattAu/hw-kit/releases/tag/v0.1.0
