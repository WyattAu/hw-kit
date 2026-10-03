//! Best-effort isolation verification over `/proc` (REQ-HW-002, v1 scope).
//!
//! [`verify_isolated`] scans `/proc/*/status` — and, per task, the
//! per-thread `Cpus_allowed_list` entries under `/proc/<pid>/task/*/status`
//! — and reports every **foreign** task whose CPU affinity overlaps a given
//! [`CpuSet`]. The scan is pure `std` (no libc on this path): statuses are
//! read as text and parsed with [`CpuSet::parse_allowed_list`].
//!
//! # Advisory, not enforcement
//!
//! This is **best-effort verification, not enforcement**: a clean report at
//! time *T* does not guarantee isolation at *T+1* — a task can migrate onto
//! the set the instant after the scan, and tasks whose status is unreadable
//! (kernel threads with hidden `/proc` mounts, permission filters, races
//! with task exit) are counted in [`IsolationReport::skipped_entries`] but
//! cannot be assessed. Fail-closed here means: *overlap is reported*;
//! absence of evidence is never reported as isolation. Callers wanting a
//! verdict use [`IsolationReport::ensure_clean`].
//!
//! # v1 scope (owner decision #4)
//!
//! The scan is `/proc`-only: cgroup cpuset controllers are **not** read in
//! v1 (semantics §Pinning). Under containerized deployments a cpuset can
//! restrict effective placement below what this report sees; the post-pin
//! read-back in `pin_current` (see the `affinity` module) is the
//! authoritative check for the pinning thread itself.
//!
//! # Self-exclusion
//!
//! The calling task (`/proc/thread-self`) is excluded from the offender
//! list — it trivially overlaps whatever set it is pinned to. **Every other
//! task, including sibling threads of the caller's own process, counts as
//! foreign**: control-plane threads squatting on a pinned core are exactly
//! what this report exists to surface.

use std::fs;
use std::path::Path;
use std::time::SystemTime;

use crate::cpu::CpuSet;
use crate::error::PinError;

/// A foreign task whose affinity overlaps the scanned set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlappingTask {
    /// Thread-group id (`/proc/<pid>`).
    pub pid: i32,
    /// Task id (`/proc/<pid>/task/<tid>`); equals `pid` for the leader.
    pub tid: i32,
    /// `comm` from the status file.
    pub comm: String,
    /// The task's `Cpus_allowed_list`, verbatim.
    pub allowed: String,
}

impl std::fmt::Display for OverlappingTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "pid {} tid {} ({}) allowed={}",
            self.pid, self.tid, self.comm, self.allowed
        )
    }
}

/// The result of one isolation scan (advisory — see the module docs).
#[derive(Debug, Clone)]
pub struct IsolationReport {
    /// Foreign tasks whose affinity overlaps the scanned set, in scan order.
    pub overlapping_tasks: Vec<OverlappingTask>,
    /// Numeric `/proc/<pid>` entries seen (the scan width).
    pub scanned_pids: usize,
    /// Statuses successfully read and assessed (leaders + threads).
    pub scanned_tasks: usize,
    /// Entries that could not be assessed (unreadable status — permission
    /// filter or task exit race). Never silently zero-evidence: a large
    /// skip count means a large blind spot.
    pub skipped_entries: usize,
    /// When the scan ran (ordering evidence for log comparison).
    pub scanned_at: SystemTime,
}

impl IsolationReport {
    /// Whether no foreign task overlapped the set at scan time.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.overlapping_tasks.is_empty()
    }

    /// Turns the report into a verdict: [`PinError::NotIsolated`] (with the
    /// offenders attached) when the scan was not clean.
    pub fn ensure_clean(&self) -> Result<(), PinError> {
        if self.is_clean() {
            return Ok(());
        }
        Err(PinError::NotIsolated {
            offenders: self.overlapping_tasks.clone(),
        })
    }
}

/// Scans the host's `/proc` for foreign tasks whose affinity overlaps `set`.
///
/// # Examples
///
/// ```
/// use hw_kit::{CpuId, CpuSet};
///
/// // Advisory: the report is returned whether clean or not.
/// let set = CpuSet::single(CpuId(0));
/// let report = hw_kit::verify_isolated(&set).expect("scan");
/// println!("{} overlapping task(s), {} skipped",
///     report.overlapping_tasks.len(), report.skipped_entries);
/// ```
pub fn verify_isolated(set: &CpuSet) -> Result<IsolationReport, PinError> {
    verify_isolated_at(Path::new("/proc"), set)
}

/// [`verify_isolated`] against an explicit `/proc` root — the test seam that
/// keeps the scanner hermetic.
pub fn verify_isolated_at(proc_root: &Path, set: &CpuSet) -> Result<IsolationReport, PinError> {
    let scanned_at = SystemTime::now();
    let self_ids = current_task_ids();
    let mut report = IsolationReport {
        overlapping_tasks: Vec::new(),
        scanned_pids: 0,
        scanned_tasks: 0,
        skipped_entries: 0,
        scanned_at,
    };

    let entries = fs::read_dir(proc_root).map_err(|source| PinError::Scan {
        path: proc_root.display().to_string(),
        source,
    })?;
    for entry in entries {
        let Ok(entry) = entry else {
            report.skipped_entries += 1;
            continue;
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(pid) = name.parse::<i32>() else {
            continue; // self/, thread-self/, acpi/, irq/, … are not tasks
        };
        report.scanned_pids += 1;

        let task_dir = proc_root.join(&name).join("task");
        let mut leader_scanned = false;
        if let Ok(threads) = fs::read_dir(&task_dir) {
            for thread in threads {
                let Ok(thread) = thread else {
                    report.skipped_entries += 1;
                    continue;
                };
                let tname = thread.file_name().to_string_lossy().into_owned();
                let Ok(tid) = tname.parse::<i32>() else {
                    continue;
                };
                if tid == pid {
                    // The leader's status is visited here like any
                    // thread; the top-level scan below only runs when
                    // the thread listing was unreadable (raced).
                    leader_scanned = true;
                }
                assess_status(
                    &task_dir.join(&tname).join("status"),
                    pid,
                    tid,
                    set,
                    self_ids,
                    &mut report,
                )?;
            }
        } else { /* thread listing unavailable; leader scan below still runs */
        }

        if !leader_scanned {
            assess_status(
                &proc_root.join(&name).join("status"),
                pid,
                pid,
                set,
                self_ids,
                &mut report,
            )?;
        }
    }
    Ok(report)
}

/// Reads one status file and records the outcome in the report. Read
/// failures (permission filters, task exit races) are *skips* — documented
/// blind spots, not errors; an entry that was read but cannot be parsed is
/// a typed [`PinError::MalformedProc`] (interface drift must not hide).
fn assess_status(
    status_path: &Path,
    pid: i32,
    tid: i32,
    set: &CpuSet,
    self_ids: Option<(i32, i32)>,
    report: &mut IsolationReport,
) -> Result<(), PinError> {
    if self_ids == Some((pid, tid)) {
        // The calling task trivially overlaps its own pinned set.
        return Ok(());
    }
    let text = if let Ok(text) = fs::read_to_string(status_path) {
        text
    } else {
        report.skipped_entries += 1;
        return Ok(());
    };
    let parsed = parse_status(&text).map_err(|reason| PinError::MalformedProc {
        path: status_path.display().to_string(),
        reason,
    })?;
    let allowed =
        CpuSet::parse_allowed_list(&parsed.allowed).map_err(|e| PinError::MalformedProc {
            path: status_path.display().to_string(),
            reason: format!("bad Cpus_allowed_list: {e}"),
        })?;
    report.scanned_tasks += 1;
    if allowed.overlaps(set) {
        report.overlapping_tasks.push(OverlappingTask {
            pid,
            tid,
            comm: parsed.comm,
            allowed: parsed.allowed,
        });
    }
    Ok(())
}

#[derive(Debug)]
struct ParsedStatus {
    comm: String,
    allowed: String,
}

/// Extracts `Name:` and `Cpus_allowed_list:` from a status file body.
/// Pure text work — unit-tested inline, miri-eligible.
fn parse_status(text: &str) -> Result<ParsedStatus, String> {
    let mut comm: Option<String> = None;
    let mut allowed: Option<String> = None;
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("Name:") {
            comm = Some(v.trim().to_string());
        } else if let Some(v) = line.strip_prefix("Cpus_allowed_list:") {
            allowed = Some(v.trim().to_string());
        }
    }
    match (comm, allowed) {
        (Some(comm), Some(allowed)) => Ok(ParsedStatus { comm, allowed }),
        (None, _) => Err("missing `Name:` field".to_string()),
        (_, None) => Err("missing `Cpus_allowed_list:` field".to_string()),
    }
}

/// The calling task's `(pid, tid)` via `/proc/thread-self/stat` (field 0 is
/// the tid; the pid comes from std). `None` when the kernel predates
/// `thread-self` (Linux < 3.17) — then nothing is excluded, which can only
/// over-report, never under-report.
fn current_task_ids() -> Option<(i32, i32)> {
    let stat = fs::read_to_string("/proc/thread-self/stat").ok()?;
    let tid = stat.split(' ').next()?.parse::<i32>().ok()?;
    Some((std::process::id() as i32, tid))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::cpu::CpuId;

    fn status_of(comm: &str, allowed: &str) -> String {
        format!("State:\tS (sleeping)\nName:\t{comm}\nCpus_allowed_list:\t{allowed}\n")
    }

    #[test]
    fn status_parses_name_and_allowed_list() {
        let s = parse_status(&status_of("kworker/u8:1", "0-3")).unwrap();
        assert_eq!(s.comm, "kworker/u8:1");
        assert_eq!(s.allowed, "0-3");
    }

    #[test]
    fn status_missing_allowed_list_is_fail_closed() {
        let err = parse_status("Name:\tinit\nState:\tS (sleeping)\n").unwrap_err();
        assert!(err.contains("Cpus_allowed_list"), "{err}");
    }

    #[test]
    fn status_missing_name_is_fail_closed() {
        let err = parse_status("Cpus_allowed_list:\t0-3\n").unwrap_err();
        assert!(err.contains("Name:"), "{err}");
    }

    #[test]
    fn malformed_allowed_list_rejects_with_reason() {
        let text = status_of("x", "not-a-list");
        let parsed = parse_status(&text).unwrap();
        let err = CpuSet::parse_allowed_list(&parsed.allowed).unwrap_err();
        assert!(matches!(err, crate::cpu::CpuSetParseError::InvalidToken(_)));
    }

    #[test]
    fn overlap_detection_over_parsed_mask() {
        let text = status_of("worker", "4,5");
        let parsed = parse_status(&text).unwrap();
        let mask = CpuSet::parse_allowed_list(&parsed.allowed).unwrap();
        assert!(mask.overlaps(&CpuSet::single(CpuId(5))));
        assert!(!mask.overlaps(&CpuSet::single(CpuId(0))));
    }
}
