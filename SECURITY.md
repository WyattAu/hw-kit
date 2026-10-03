# Security Policy — hw-kit

## Supported versions

| Version | Supported |
|---------|-----------|
| 0.1.x   | ✅        |

## Reporting a vulnerability

Report privately via [GitHub security advisories] for this repository, or
email **wyatt_au@protonmail.com**. Do **not** open a public issue for
security reports.

You will receive an acknowledgement within **72 hours**. Coordinated
disclosure: we ask for up to 90 days before public disclosure while a
patch ships.

## Scope notes

`hw-kit` is a thin FFI wrapper over Linux placement facilities
(`sched_setaffinity`, `mmap`/`mbind`, `MAP_HUGETLB`) plus pure-`std`
sysfs/`/proc` parsing. Security considerations for integrators:

- **The raw NUMA hook is `unsafe` on purpose** (`numa_bind_region`): its
  `# Safety` contract is the caller's obligation — a wrong `addr`/`len`
  there is memory-unsafety the kernel cannot catch. Prefer the safe
  wrappers (`alloc_on_node`, `alloc_preferred_on`) unless you have a
  policy the two blessed modes cannot express.
- **`IsolationReport` is advisory, not enforcement.** A clean report is
  evidence at scan time; `/proc` blind spots are counted in
  `skipped_entries` and never folded into "clean". Do not treat a clean
  scan as a security boundary.
- **No silent fallbacks.** Hugepage exhaustion is a typed error; a
  `hugepage_reserve` success is huge-page-backed memory, full stop. Any
  report of an `Ok` against an empty pool is a critical bug (CI asserts
  the opposite).
- The isolation scan reads `/proc` and never writes cgroup controllers;
  pinning changes only the calling/target task's affinity mask.
- Dependency surface is `libc` only (feature-gated; the default build has
  zero dependencies) — enforced by CI (`deps-are-libc-only`,
  cargo-deny bans, cargo-vet).
- Every `unsafe` block carries a `// SAFETY:` justification
  (lint-enforced) and is audited in `src/lib.rs` § Safety.

[GitHub security advisories]:
    https://github.com/WyattAu/hw-kit/security/advisories/new
