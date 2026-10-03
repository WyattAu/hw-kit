//! Thread pinning via `sched_setaffinity` (REQ-HW-002, feature `libc`).
//!
//! [`pin_current`] / [`pin_thread`] set a task's CPU affinity; both
//! **read the mask back** with `sched_getaffinity` and fail closed with
//! [`PinError::AffinityMismatch`] if the kernel applied anything other than
//! exactly the requested set (spec §Risk register: a cgroup-cpuset-narrowed
//! or offline-mask pin must not look like success).
//!
//! Error mapping is typed: `ESRCH` → [`PinError::NoSuchTask`], everything
//! else → [`PinError::Syscall`] with the raw errno preserved on `source`.

use std::io;

use crate::cpu::{CoreId, CpuSet};
use crate::error::PinError;

/// The calling task's current effective affinity (`sched_getaffinity(0)`).
///
/// Also the post-condition reader behind the pinning calls' fail-closed
/// verification, and the set that
/// [`verify_current_isolated`] scans.
pub fn current_set() -> Result<CpuSet, PinError> {
    // Start at glibc's classic 1024-CPU mask and grow on EINVAL (kernels
    // sized beyond 1024 CPUs report the needed size that way). The ceiling
    // is the crate's documented mask capacity.
    let mut bits: usize = 1024;
    loop {
        let mut words = vec![0_u64; bits / 64];
        // SAFETY: `words` is a live, correctly sized little-endian bitmap;
        // `sched_getaffinity` writes at most `words.len() * 8` bytes into it
        // and returns how much it copied. PID 0 means the calling thread.
        let rc = unsafe {
            libc::sched_getaffinity(
                0,
                std::mem::size_of_val(&words[..]),
                words.as_mut_ptr().cast(),
            )
        };
        if rc == 0 {
            return Ok(CpuSet::from_words(words));
        }
        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::EINVAL) if bits < CpuSet::MAX_CPUS as usize => bits *= 2,
            _ => {
                return Err(PinError::Syscall {
                    call: "sched_getaffinity",
                    source: err,
                })
            }
        }
    }
}

/// Pins the **calling thread** (not the process — `sched_setaffinity(0)` is
/// thread-scoped) to `set`.
///
/// Fail-closed by construction:
/// - an empty set is [`PinError::EmptySet`] (never sent to the kernel);
/// - the mask is read back post-pin and any difference from the requested
///   set is [`PinError::AffinityMismatch`] — a silently narrowed mask (cpuset,
///   offline CPUs) is an error, not a success.
///
/// # Examples
///
/// ```no_run
/// use hw_kit::{CoreId, pin_current_core};
///
/// // Pin this thread to core 2, then undo it.
/// let original = hw_kit::affinity::current_set().expect("affinity");
/// pin_current_core(CoreId::new(2).expect("in-range core")).expect("pin");
/// // ... latency-critical work on core 2 ...
/// hw_kit::pin_current(&original).expect("restore");
/// ```
pub fn pin_current(set: &CpuSet) -> Result<(), PinError> {
    sched_setaffinity_checked(0, set)?;
    let effective = current_set()?;
    if effective != *set {
        return Err(PinError::AffinityMismatch {
            requested: set.allowed_list(),
            effective: effective.allowed_list(),
        });
    }
    Ok(())
}

/// Pins the task with kernel id `tid` to `set` (a thread id from `gettid`/
/// `/proc/<pid>/task`, or a pid for the group leader).
///
/// `ESRCH` (the task exited before the pin landed) is the typed
/// [`PinError::NoSuchTask`]. The mask is verified by reading the target's
/// affinity back; a task that changes its own mask concurrently makes the
/// call fail closed with [`PinError::AffinityMismatch`].
pub fn pin_thread(tid: i32, set: &CpuSet) -> Result<(), PinError> {
    sched_setaffinity_checked(tid, set)?;
    let effective = read_set_of(tid)?;
    if effective != *set {
        return Err(PinError::AffinityMismatch {
            requested: set.allowed_list(),
            effective: effective.allowed_list(),
        });
    }
    Ok(())
}

/// Pins the calling thread to the single core `core` — the ergonomic
/// single-core form of [`pin_current`].
pub fn pin_current_core(core: CoreId) -> Result<(), PinError> {
    pin_current(&CpuSet::single(core.into()))
}

/// Pins the task `tid` to the single core `core` — the ergonomic
/// single-core form of [`pin_thread`].
pub fn pin_thread_core(tid: i32, core: CoreId) -> Result<(), PinError> {
    pin_thread(tid, &CpuSet::single(core.into()))
}

/// Scans `/proc` against the **calling thread's current effective
/// affinity** — the zero-argument form of
/// [`verify_isolated`](crate::isolation::verify_isolated) for the common
/// "was anything else put on my core?" check after a pin.
///
/// Same advisory semantics: the report is evidence at scan time, foreign
/// sibling threads count, the calling task itself is excluded.
///
/// # Examples
///
/// ```no_run
/// let report = hw_kit::affinity::verify_current_isolated().expect("scan");
/// if let Err(hw_kit::PinError::NotIsolated { offenders }) = report.ensure_clean() {
///     for task in offenders {
///         eprintln!("foreign task on our core: {task}");
///     }
/// }
/// ```
pub fn verify_current_isolated() -> Result<crate::isolation::IsolationReport, PinError> {
    let set = current_set()?;
    crate::isolation::verify_isolated(&set)
}

fn sched_setaffinity_checked(tid: i32, set: &CpuSet) -> Result<(), PinError> {
    if set.is_empty() {
        return Err(PinError::EmptySet);
    }
    let words = set.raw_words();
    // SAFETY: `words` is a live little-endian bitmap whose byte length is
    // passed exactly; the kernel reads only that many bytes and never
    // retains the pointer beyond the call. `tid` is caller-supplied; a dead
    // target surfaces as ESRCH → NoSuchTask.
    let rc = unsafe {
        libc::sched_setaffinity(tid, std::mem::size_of_val(words), words.as_ptr().cast())
    };
    if rc != 0 {
        let err = io::Error::last_os_error();
        return Err(match err.raw_os_error() {
            Some(libc::ESRCH) => PinError::NoSuchTask { tid },
            _ => PinError::Syscall {
                call: "sched_setaffinity",
                source: err,
            },
        });
    }
    Ok(())
}

fn read_set_of(tid: i32) -> Result<CpuSet, PinError> {
    let mut words = vec![0_u64; CpuSet::MAX_CPUS as usize / 64];
    // SAFETY: as in `current_set` — live, correctly sized bitmap; the
    // kernel writes at most its byte length. A dead target returns ESRCH.
    let rc = unsafe {
        libc::sched_getaffinity(
            tid,
            std::mem::size_of_val(words.as_slice()),
            words.as_mut_ptr().cast(),
        )
    };
    if rc == 0 {
        return Ok(CpuSet::from_words(words));
    }
    let err = io::Error::last_os_error();
    Err(match err.raw_os_error() {
        Some(libc::ESRCH) => PinError::NoSuchTask { tid },
        _ => PinError::Syscall {
            call: "sched_getaffinity",
            source: err,
        },
    })
}
