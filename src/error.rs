//! The typed error taxonomy of the estate's hardware substrate.
//!
//! Every fallible operation in hw-kit returns a domain-specific error
//! ([`TopologyError`], [`PinError`], [`NumaError`], [`HugetlbError`]); all of
//! them convert into the crate-wide umbrella [`HwError`]. Errors are
//! `#[non_exhaustive]`: new variants may be added without a semver break, so
//! callers must always keep a catch-all arm.
//!
//! # Implementation note — no `thiserror`
//!
//! REQ-HW-006 fixes the runtime dependency surface at "`libc` only", and the
//! `deps_are_libc_only` CI job fails the tree on any additional edge.
//! `Display`/`std::error::Error` impls are therefore hand-written below. The
//! `source()` chains follow one rule: a syscall-shaped variant exposes the
//! raw [`io::Error`] as its source so `errno` inspection keeps working.

use std::io;

use crate::isolation::OverlappingTask;

/// Crate-wide umbrella error: every domain error converts into this type.
#[derive(Debug)]
#[non_exhaustive]
pub enum HwError {
    /// Topology discovery failed ([`TopologyError`], REQ-HW-001).
    TopologyParse(TopologyError),
    /// A raw syscall returned an error (`errno` is available on `source`).
    SyscallFailed {
        /// Name of the failing syscall (e.g. `"sched_setaffinity"`).
        call: &'static str,
        /// The kernel-reported error.
        source: io::Error,
    },
    /// The requested target or facility does not exist on this host (CPU out
    /// of the supported mask range, node beyond the node-mask capacity, …).
    UnsupportedTarget(String),
    /// The scanned set is not isolated; foreign tasks overlap it (REQ-HW-002).
    NotIsolated {
        /// Every foreign task whose affinity overlaps the scanned set.
        offenders: Vec<OverlappingTask>,
    },
    /// Hugepage reservation is unavailable on this host. hw-kit never falls
    /// back to base pages silently (REQ-HW-004).
    HugepageUnavailable(String),
    /// Pinning/isolation failure ([`PinError`], REQ-HW-002).
    Pin(PinError),
    /// NUMA binding failure ([`NumaError`], REQ-HW-003).
    Numa(NumaError),
    /// Hugepage failure other than plain unavailability ([`HugetlbError`],
    /// REQ-HW-004).
    Hugetlb(HugetlbError),
}

impl From<TopologyError> for HwError {
    fn from(value: TopologyError) -> Self {
        HwError::TopologyParse(value)
    }
}

impl From<PinError> for HwError {
    fn from(value: PinError) -> Self {
        HwError::Pin(value)
    }
}

impl From<NumaError> for HwError {
    fn from(value: NumaError) -> Self {
        HwError::Numa(value)
    }
}

impl From<HugetlbError> for HwError {
    fn from(value: HugetlbError) -> Self {
        match value {
            // The exhaustion arm keeps its dedicated umbrella spelling so the
            // no-silent-fallback contract is greppable end to end.
            HugetlbError::Unavailable { size_class, .. } => {
                HwError::HugepageUnavailable(format!("hugepage size class {size_class}"))
            }
            other => HwError::Hugetlb(other),
        }
    }
}

impl std::fmt::Display for HwError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HwError::TopologyParse(e) => write!(f, "topology parse failed: {e}"),
            HwError::SyscallFailed { call, source } => {
                write!(f, "syscall `{call}` failed: {source}")
            }
            HwError::UnsupportedTarget(what) => write!(f, "unsupported target: {what}"),
            HwError::NotIsolated { offenders } => {
                write!(
                    f,
                    "pinned set is not isolated: {} offender(s)",
                    offenders.len()
                )
            }
            HwError::HugepageUnavailable(why) => {
                write!(f, "hugepage reservation unavailable: {why}")
            }
            HwError::Pin(e) => write!(f, "pinning failed: {e}"),
            HwError::Numa(e) => write!(f, "NUMA binding failed: {e}"),
            HwError::Hugetlb(e) => write!(f, "hugepage reservation failed: {e}"),
        }
    }
}

impl std::error::Error for HwError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            HwError::TopologyParse(e) => Some(e),
            HwError::SyscallFailed { source, .. } => Some(source),
            HwError::UnsupportedTarget(_)
            | HwError::NotIsolated { .. }
            | HwError::HugepageUnavailable(_) => None,
            HwError::Pin(e) => Some(e),
            HwError::Numa(e) => Some(e),
            HwError::Hugetlb(e) => Some(e),
        }
    }
}

/// Topology discovery failures (REQ-HW-001).
///
/// Produced by [`discover`](crate::discover)/[`discover_at`](crate::discover_at)
/// when the `/sys` tree is absent, drifted, or carries entries that cannot be
/// interpreted. hw-kit never guesses a default for an entry it cannot parse.
#[derive(Debug)]
#[non_exhaustive]
pub enum TopologyError {
    /// A sysfs path could not be read (missing mount, permission, race).
    Io {
        /// The path that could not be read.
        path: String,
        /// The underlying IO error.
        source: io::Error,
    },
    /// A required sysfs field is absent (e.g. `topology/core_id`).
    MissingField {
        /// The entry the field was expected under (e.g. `cpu3`).
        entry: String,
        /// The missing field name.
        field: String,
    },
    /// A sysfs entry exists but its contents cannot be interpreted.
    Malformed {
        /// The path whose contents were malformed.
        path: String,
        /// What was wrong with the contents.
        reason: String,
    },
}

impl std::fmt::Display for TopologyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TopologyError::Io { path, source } => write!(f, "cannot read `{path}`: {source}"),
            TopologyError::MissingField { entry, field } => {
                write!(f, "missing required sysfs field `{field}` under `{entry}`")
            }
            TopologyError::Malformed { path, reason } => {
                write!(f, "malformed sysfs entry `{path}`: {reason}")
            }
        }
    }
}

impl std::error::Error for TopologyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TopologyError::Io { source, .. } => Some(source),
            TopologyError::MissingField { .. } | TopologyError::Malformed { .. } => None,
        }
    }
}

/// Pinning and isolation-verification failures (REQ-HW-002).
#[derive(Debug)]
#[non_exhaustive]
pub enum PinError {
    /// An empty CPU set was handed to a pinning call; the kernel would reject
    /// it with `EINVAL`, so hw-kit rejects it typed and up front.
    EmptySet,
    /// The syscall failed; `ESRCH` is reported as [`PinError::NoSuchTask`]
    /// instead.
    Syscall {
        /// Name of the failing syscall.
        call: &'static str,
        /// The kernel-reported error (`EINVAL`, `EPERM`, …).
        source: io::Error,
    },
    /// The target TID does not exist (typed `ESRCH`; a foreign task may have
    /// exited between the caller obtaining the TID and the pin).
    NoSuchTask {
        /// The TID that does not exist.
        tid: i32,
    },
    /// The kernel's post-pin effective affinity differs from the requested
    /// mask (read-back verification; a narrowed mask means placement
    /// assumptions would silently break).
    AffinityMismatch {
        /// The mask that was requested.
        requested: String,
        /// The mask the kernel reports after the pin.
        effective: String,
    },
    /// The scan found foreign tasks whose affinity overlaps the pinned set.
    /// Advisory: a clean report is evidence, not a guarantee (see the
    /// [`isolation`](crate::isolation) module docs).
    NotIsolated {
        /// Every foreign task overlapping the set at scan time.
        offenders: Vec<OverlappingTask>,
    },
    /// A `/proc` entry that was successfully read could not be interpreted
    /// (kernel interface drift). Fail-closed: hw-kit does not silently skip
    /// entries it read but could not assess.
    MalformedProc {
        /// The `/proc` path that could not be parsed.
        path: String,
        /// What was wrong with the contents.
        reason: String,
    },
    /// The isolation scan could not enumerate `/proc` at all.
    Scan {
        /// The `/proc` root that could not be read.
        path: String,
        /// The underlying IO error.
        source: io::Error,
    },
}

impl std::fmt::Display for PinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PinError::EmptySet => write!(f, "refusing to pin to an empty CPU set"),
            PinError::Syscall { call, source } => write!(f, "syscall `{call}` failed: {source}"),
            PinError::NoSuchTask { tid } => write!(f, "no such task: tid {tid} (ESRCH)"),
            PinError::AffinityMismatch {
                requested,
                effective,
            } => write!(
                f,
                "post-pin affinity mismatch: requested {requested}, effective {effective}"
            ),
            PinError::NotIsolated { offenders } => {
                write!(
                    f,
                    "set not isolated; {} overlapping task(s)",
                    offenders.len()
                )
            }
            PinError::MalformedProc { path, reason } => {
                write!(f, "malformed /proc entry `{path}`: {reason}")
            }
            PinError::Scan { path, source } => write!(f, "cannot scan `{path}`: {source}"),
        }
    }
}

impl std::error::Error for PinError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PinError::Syscall { source, .. } | PinError::Scan { source, .. } => Some(source),
            PinError::EmptySet
            | PinError::NoSuchTask { .. }
            | PinError::AffinityMismatch { .. }
            | PinError::NotIsolated { .. }
            | PinError::MalformedProc { .. } => None,
        }
    }
}

/// NUMA binding failures (REQ-HW-003).
#[derive(Debug)]
#[non_exhaustive]
pub enum NumaError {
    /// The region or policy is invalid (null pointer, zero length, empty
    /// node set). Typed and checked before any syscall.
    InvalidRegion {
        /// What was invalid about the call.
        reason: String,
    },
    /// `mmap`/`mbind` failed; `EINVAL` from `mbind` typically means the node
    /// mask names no usable node, `ENOMEM` that the kernel could not satisfy
    /// the policy.
    Syscall {
        /// Name of the failing syscall (`"mmap"`, `"mbind"`).
        call: &'static str,
        /// The kernel-reported error.
        source: io::Error,
    },
    /// The node id lies beyond the crate's node-mask capacity.
    UnsupportedTarget(String),
}

impl std::fmt::Display for NumaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NumaError::InvalidRegion { reason } => write!(f, "invalid region: {reason}"),
            NumaError::Syscall { call, source } => write!(f, "syscall `{call}` failed: {source}"),
            NumaError::UnsupportedTarget(what) => write!(f, "unsupported target: {what}"),
        }
    }
}

impl std::error::Error for NumaError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            NumaError::Syscall { source, .. } => Some(source),
            NumaError::InvalidRegion { .. } | NumaError::UnsupportedTarget(_) => None,
        }
    }
}

/// Hugepage reservation failures (REQ-HW-004).
///
/// There is **no silent fallback to 4 KiB pages**: every failure mode here is
/// a typed error, and a successful [`hugepage_reserve`](crate::hugepage::hugepage_reserve)
/// is huge-page-backed or nothing.
#[derive(Debug)]
#[non_exhaustive]
pub enum HugetlbError {
    /// `len` was zero.
    ZeroLength,
    /// `len` is not a multiple of the size class's page size.
    UnalignedLength {
        /// The requested length.
        len: usize,
        /// The hugepage size it must be a multiple of (bytes).
        page: u64,
    },
    /// The kernel has no pool for the requested size class (e.g. no
    /// `hugepages-1048576kB` entry in `/sys/kernel/mm/hugepages`).
    UnsupportedSize {
        /// The size class that is not present (`"2MiB"` / `"1GiB"`).
        size_class: &'static str,
    },
    /// The hugepage pool is exhausted or hugetlb is unavailable entirely.
    /// Never a silent 4 KiB fallback.
    Unavailable {
        /// The size class that could not be reserved.
        size_class: &'static str,
        /// The kernel-reported error, when reservation was attempted.
        source: Option<io::Error>,
    },
    /// The `mmap` failed for a reason other than pool exhaustion.
    Syscall {
        /// Name of the failing syscall.
        call: &'static str,
        /// The kernel-reported error.
        source: io::Error,
    },
}

impl std::fmt::Display for HugetlbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HugetlbError::ZeroLength => write!(f, "refusing a zero-length reservation"),
            HugetlbError::UnalignedLength { len, page } => {
                write!(f, "len {len} is not a multiple of the hugepage size {page}")
            }
            HugetlbError::UnsupportedSize { size_class } => {
                write!(
                    f,
                    "kernel has no hugepage pool for the {size_class} size class"
                )
            }
            HugetlbError::Unavailable { size_class, source } => match source {
                Some(e) => write!(
                    f,
                    "hugepage pool exhausted or unavailable ({size_class} class): {e}"
                ),
                None => write!(
                    f,
                    "hugepage pool for the {size_class} class has no reserved pages"
                ),
            },
            HugetlbError::Syscall { call, source } => {
                write!(f, "syscall `{call}` failed: {source}")
            }
        }
    }
}

impl std::error::Error for HugetlbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            HugetlbError::Unavailable {
                source: Some(e), ..
            }
            | HugetlbError::Syscall { source: e, .. } => Some(e),
            HugetlbError::ZeroLength
            | HugetlbError::UnalignedLength { .. }
            | HugetlbError::UnsupportedSize { .. }
            | HugetlbError::Unavailable { source: None, .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use std::error::Error as _;

    /// Every Display arm renders, and the `source()` chains follow the
    /// documented rule (syscall-shaped variants expose the io::Error).
    #[test]
    fn hwerror_variants_render_and_chain() {
        let io_err = io::Error::from_raw_os_error(libc::EINVAL);

        let e = HwError::TopologyParse(TopologyError::Malformed {
            path: "/sys/x".to_string(),
            reason: "zero".to_string(),
        });
        assert_eq!(
            e.to_string(),
            "topology parse failed: malformed sysfs entry `/sys/x`: zero"
        );
        assert!(std::error::Error::source(&e).is_some());

        let e = HwError::SyscallFailed {
            call: "sched_setaffinity",
            source: io_err,
        };
        assert_eq!(
            e.to_string(),
            "syscall `sched_setaffinity` failed: Invalid argument (os error 22)"
        );
        assert!(e.source().is_some());

        let e = HwError::UnsupportedTarget("node 9".to_string());
        assert_eq!(e.to_string(), "unsupported target: node 9");
        assert!(e.source().is_none());

        let e = HwError::NotIsolated {
            offenders: Vec::new(),
        };
        assert_eq!(e.to_string(), "pinned set is not isolated: 0 offender(s)");
        assert!(e.source().is_none());

        let e = HwError::HugepageUnavailable("2MiB pool empty".to_string());
        assert_eq!(
            e.to_string(),
            "hugepage reservation unavailable: 2MiB pool empty"
        );
        assert!(e.source().is_none());

        let e = HwError::Pin(PinError::EmptySet);
        assert_eq!(
            e.to_string(),
            "pinning failed: refusing to pin to an empty CPU set"
        );
        assert!(e.source().is_some());

        let e = HwError::Numa(NumaError::InvalidRegion {
            reason: "zero length".to_string(),
        });
        assert_eq!(
            e.to_string(),
            "NUMA binding failed: invalid region: zero length"
        );
        assert!(e.source().is_some());

        let e = HwError::Hugetlb(HugetlbError::ZeroLength);
        assert_eq!(
            e.to_string(),
            "hugepage reservation failed: refusing a zero-length reservation"
        );
        assert!(e.source().is_some());
    }

    #[test]
    fn topology_error_rendering() {
        let io_err = io::Error::from_raw_os_error(libc::ENOENT);
        let e = TopologyError::Io {
            path: "/sys".to_string(),
            source: io_err,
        };
        assert_eq!(
            e.to_string(),
            "cannot read `/sys`: No such file or directory (os error 2)"
        );
        assert!(e.source().is_some());

        let e = TopologyError::MissingField {
            entry: "cpu3".to_string(),
            field: "core_id".to_string(),
        };
        assert_eq!(
            e.to_string(),
            "missing required sysfs field `core_id` under `cpu3`"
        );
        assert!(e.source().is_none());
    }

    #[test]
    fn pin_error_rendering_and_offender_display() {
        let io_err = io::Error::from_raw_os_error(libc::EINVAL);
        assert_eq!(
            PinError::Syscall {
                call: "sched_getaffinity",
                source: io_err
            }
            .to_string(),
            "syscall `sched_getaffinity` failed: Invalid argument (os error 22)"
        );
        assert_eq!(
            PinError::NoSuchTask { tid: 7 }.to_string(),
            "no such task: tid 7 (ESRCH)"
        );
        assert_eq!(
            PinError::AffinityMismatch {
                requested: "0-3".to_string(),
                effective: "0".to_string()
            }
            .to_string(),
            "post-pin affinity mismatch: requested 0-3, effective 0"
        );
        let e = PinError::NotIsolated {
            offenders: vec![OverlappingTask {
                pid: 201,
                tid: 202,
                comm: "qmaster".to_string(),
                allowed: "4,5".to_string(),
            }],
        };
        assert_eq!(e.to_string(), "set not isolated; 1 overlapping task(s)");
        assert!(e.source().is_none());

        let io_err = io::Error::from_raw_os_error(libc::ENOENT);
        assert_eq!(
            PinError::Scan {
                path: "/proc".to_string(),
                source: io_err
            }
            .to_string(),
            "cannot scan `/proc`: No such file or directory (os error 2)"
        );
        assert_eq!(
            PinError::MalformedProc {
                path: "/proc/1/status".to_string(),
                reason: "missing `Cpus_allowed_list:` field".to_string()
            }
            .to_string(),
            "malformed /proc entry `/proc/1/status`: missing `Cpus_allowed_list:` field"
        );
        assert_eq!(
            OverlappingTask {
                pid: 350,
                tid: 350,
                comm: "noise".to_string(),
                allowed: "4".to_string()
            }
            .to_string(),
            "pid 350 tid 350 (noise) allowed=4"
        );
    }

    #[test]
    fn numa_error_rendering() {
        let io_err = io::Error::from_raw_os_error(libc::EINVAL);
        assert_eq!(
            NumaError::Syscall {
                call: "mbind",
                source: io_err
            }
            .to_string(),
            "syscall `mbind` failed: Invalid argument (os error 22)"
        );
        assert!(NumaError::Syscall {
            call: "mbind",
            source: io::Error::from_raw_os_error(libc::EINVAL)
        }
        .source()
        .is_some());
        assert_eq!(
            NumaError::UnsupportedTarget("node 4096".to_string()).to_string(),
            "unsupported target: node 4096"
        );
    }

    #[test]
    fn hugetlb_error_rendering_and_sources() {
        assert_eq!(
            HugetlbError::UnalignedLength {
                len: 4096,
                page: 2 * 1024 * 1024
            }
            .to_string(),
            "len 4096 is not a multiple of the hugepage size 2097152"
        );
        assert_eq!(
            HugetlbError::UnsupportedSize { size_class: "1GiB" }.to_string(),
            "kernel has no hugepage pool for the 1GiB size class"
        );
        assert_eq!(
            HugetlbError::Unavailable {
                size_class: "2MiB",
                source: None
            }
            .to_string(),
            "hugepage pool for the 2MiB class has no reserved pages"
        );
        assert!(HugetlbError::Unavailable {
            size_class: "2MiB",
            source: None
        }
        .source()
        .is_none());

        let io_err = io::Error::from_raw_os_error(libc::ENOMEM);
        assert_eq!(
            HugetlbError::Unavailable { size_class: "2MiB", source: Some(io_err) }.to_string(),
            "hugepage pool exhausted or unavailable (2MiB class): Cannot allocate memory (os error 12)"
        );
        assert!(HugetlbError::Unavailable {
            size_class: "2MiB",
            source: Some(io::Error::from_raw_os_error(libc::ENOMEM))
        }
        .source()
        .is_some());

        let io_err = io::Error::from_raw_os_error(libc::EIO);
        assert_eq!(
            HugetlbError::Syscall {
                call: "mmap",
                source: io_err
            }
            .to_string(),
            "syscall `mmap` failed: Input/output error (os error 5)"
        );
    }

    #[test]
    fn from_impls_land_in_the_right_umbrella_arms() {
        let e: HwError = HugetlbError::Unavailable {
            size_class: "2MiB",
            source: None,
        }
        .into();
        assert!(matches!(e, HwError::HugepageUnavailable(_)), "{e:?}");

        let e: HwError = HugetlbError::ZeroLength.into();
        assert!(matches!(e, HwError::Hugetlb(_)), "{e:?}");

        let e: HwError = PinError::EmptySet.into();
        assert!(matches!(e, HwError::Pin(_)), "{e:?}");

        let e: HwError = NumaError::InvalidRegion {
            reason: "x".to_string(),
        }
        .into();
        assert!(matches!(e, HwError::Numa(_)), "{e:?}");

        let e: HwError = TopologyError::MissingField {
            entry: "cpu0".to_string(),
            field: "core_id".to_string(),
        }
        .into();
        assert!(matches!(e, HwError::TopologyParse(_)), "{e:?}");
    }
}
