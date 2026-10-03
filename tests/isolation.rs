//! REQ-HW-002 isolation-verification tests over committed `/proc` fixtures.
//!
//! Hermetic by design: correctness asserts run against
//! `tests/fixtures/proc/*` trees (never the live host). The live-host scan
//! is a smoke assert only (`Ok`, nonempty scan) — the live report's
//! contents depend on the machine and are advisory by contract.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use hw_kit::cpu::{CpuId, CpuSet};
use hw_kit::error::PinError;
use hw_kit::isolation::verify_isolated_at;
use hw_kit::IsolationReport;

mod support;

fn scan(fixture: &str, set: &CpuSet) -> Result<IsolationReport, PinError> {
    verify_isolated_at(&support::proc(fixture), set)
}

fn set_of(text: &str) -> CpuSet {
    CpuSet::parse_allowed_list(text).unwrap()
}

/// The unpinned/clean fixture: nothing overlaps the queried set, so the
/// report is empty — "unpinned = empty" holds where it is true, and the
/// report says what it saw.
#[test]
fn clean_fixture_reports_empty() {
    let report = scan("clean", &set_of("60-62")).expect("fixture scan");
    assert!(report.is_clean(), "{:?}", report.overlapping_tasks);
    assert_eq!(report.overlapping_tasks.len(), 0);
    assert!(
        report.scanned_pids >= 2,
        "saw the fixture tasks: {}",
        report.scanned_pids
    );
    assert!(report.scanned_tasks >= 2);
    assert_eq!(report.skipped_entries, 0);
    report.ensure_clean().expect("clean verdict is Ok");
}

/// Overlap *is* reported — leaders and threads, with per-thread precision:
/// the overlapping thread of 201 is named even though its leader is clean.
#[test]
fn overlap_is_reported_leader_and_threads() {
    let report = scan("overlap", &set_of("4,5")).expect("fixture scan");
    assert!(!report.is_clean());

    let mut keys: Vec<(i32, i32)> = report
        .overlapping_tasks
        .iter()
        .map(|t| (t.pid, t.tid))
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, vec![(201, 201), (350, 350), (401, 402)], "{keys:?}");

    let by_key = |pid: i32, tid: i32| {
        report
            .overlapping_tasks
            .iter()
            .find(|t| t.pid == pid && t.tid == tid)
            .expect("offender present")
    };
    assert_eq!(by_key(201, 201).allowed, "4,5");
    assert_eq!(by_key(201, 201).comm, "qmaster");
    assert_eq!(by_key(401, 402).comm, "apphot", "thread-level comm");
    // The clean sibling thread (202) of an overlapping leader is NOT named.
    assert!(!report.overlapping_tasks.iter().any(|t| t.tid == 202));

    // The verdict path carries the offenders, typed.
    match report.ensure_clean() {
        Err(PinError::NotIsolated { offenders }) => assert_eq!(offenders.len(), 3),
        other => panic!("expected NotIsolated, got {other:?}"),
    }
}

/// Querying a set only a *thread* overlaps still names that thread.
#[test]
fn thread_only_overlap_is_named() {
    let report = scan("overlap", &set_of("6")).expect("fixture scan");
    // 6 is in thread 202's mask ("6-7") — and 402 is "5", 201 leader "4,5".
    let keys: Vec<(i32, i32)> = report
        .overlapping_tasks
        .iter()
        .map(|t| (t.pid, t.tid))
        .collect();
    assert_eq!(keys, vec![(201, 202)], "{keys:?}");
}

/// Unreadable entries are counted as skipped blind spots — never folded
/// into "clean" silently (absence of evidence is not evidence of isolation).
#[test]
fn unreadable_status_is_counted_not_hidden() {
    let report = scan("gappy", &set_of("0-7")).expect("fixture scan");
    assert!(
        report.skipped_entries >= 1,
        "the status-less task is a blind spot"
    );
    // 301 (allowed 0-7) still overlaps and is reported.
    assert!(report.overlapping_tasks.iter().any(|t| t.pid == 301));
}

/// A readable status without `Cpus_allowed_list` is interface drift →
/// typed error, fail-closed (spec: never a guessed default).
#[test]
fn malformed_status_is_typed_not_skipped() {
    let err = scan("malformed", &set_of("0-63")).unwrap_err();
    match err {
        PinError::MalformedProc { path, reason } => {
            assert!(path.contains("501/status"), "{path}");
            assert!(reason.contains("Cpus_allowed_list"), "{reason}");
        }
        other => panic!("expected MalformedProc, got {other:?}"),
    }
}

/// A missing `/proc` root is a typed scan failure.
#[test]
fn missing_proc_root_is_typed_scan_error() {
    let err = verify_isolated_at(&support::proc("does-not-exist"), &set_of("0")).unwrap_err();
    assert!(matches!(err, PinError::Scan { .. }), "{err:?}");
}

/// Live-host smoke: the scan runs against the real `/proc` and reports a
/// nonempty scan. (Contents are advisory — a shared CI host is full of
/// overlapping tasks.)
#[test]
fn live_proc_scan_smoke() {
    let report = hw_kit::verify_isolated(&CpuSet::single(CpuId(0))).expect("live scan");
    assert!(report.scanned_pids >= 1, "a Linux host always has tasks");
}
