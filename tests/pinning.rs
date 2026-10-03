//! REQ-HW-002 pinning integration tests (feature `libc`).
//!
//! Posture (spec §Gate plan):
//! - the **round-trip** (`pinning_sets_affinity_then_reports_overlap`) is
//!   env-gated (`HW_KIT_PIN_TEST=1`) — it mutates the calling thread's
//!   affinity and is meant for the self-hosted hardware runner;
//! - the **fail-closed error arms** run everywhere, always: they must not
//!   mutate state and must not require hardware.
//!
//! Every test pins to an already-allowed CPU and restores the original
//! mask — CI-safe by construction.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use hw_kit::affinity::current_set;
use hw_kit::cpu::{CoreId, CpuId, CpuSet};
use hw_kit::error::PinError;
use hw_kit::{pin_current, pin_current_core, pin_thread};

/// Restores the calling thread's original affinity even on early return.
struct AffinityGuard(CpuSet);

impl AffinityGuard {
    fn capture() -> Self {
        AffinityGuard(current_set().expect("read original affinity"))
    }
}

impl Drop for AffinityGuard {
    fn drop(&mut self) {
        pin_current(&self.0).expect("restore original affinity");
    }
}

fn gated(name: &str) -> bool {
    if let Ok("1") = std::env::var("HW_KIT_PIN_TEST").as_deref() {
        true
    } else {
        println!("hw-kit [skip] {name}: set HW_KIT_PIN_TEST=1 (hardware runner) to run");
        false
    }
}

/// REQ-HW-002 — `pinning_sets_affinity_then_reports_overlap`
/// (env-gated, self-hosted runner per spec §Gate plan).
#[test]
fn pinning_sets_affinity_then_reports_overlap() {
    if !gated("pinning_sets_affinity_then_reports_overlap") {
        return;
    }
    let guard = AffinityGuard::capture();

    // Pick an already-allowed CPU so the pin cannot fail for placement
    // reasons; the point is the round trip, not the choice of core.
    let original = current_set().expect("affinity");
    let target = original.iter().next().expect("at least one allowed cpu");
    let core = CoreId::new(target.0.try_into().expect("cpu fits u16")).expect("in-range core");

    pin_current_core(core).expect("pin to an allowed core");
    let after = current_set().expect("affinity after pin");
    assert_eq!(
        after,
        CpuSet::single(target),
        "effective mask must equal the pin"
    );

    // The verification scan runs against the pinned set. The calling task
    // must never be its own offender; foreign tasks may overlap (the test
    // harness has sibling threads) — the report is advisory by contract.
    let report = hw_kit::verify_isolated(&CpuSet::single(target)).expect("scan");
    assert!(report.scanned_pids >= 1, "the scan saw the machine's tasks");
    let my_tid = current_tid();
    assert!(
        !report
            .overlapping_tasks
            .iter()
            .any(|t| t.pid as u32 == std::process::id() && Some(t.tid) == my_tid),
        "the calling task must be excluded from its own scan: {:?}",
        report.overlapping_tasks
    );

    // `ensure_clean` is the verdict path; either way it is typed.
    match report.ensure_clean() {
        Ok(()) => println!("hw-kit: clean report at scan time"),
        Err(PinError::NotIsolated { offenders }) => {
            println!("hw-kit: advisory overlap (expected under a test harness): {offenders:?}");
        }
        Err(other) => panic!("unexpected verdict error: {other}"),
    }

    // Single-core TID pin round trip on our own thread id.
    let tid = my_tid.expect("tid from /proc/thread-self");
    pin_thread(tid, &CpuSet::single(target)).expect("tid pin round trip");
    assert_eq!(current_set().unwrap(), CpuSet::single(target));
    drop(guard); // restore in every path
    assert_eq!(
        current_set().unwrap(),
        original,
        "restoration must be exact"
    );
}

/// Fail-closed, always-runnable: an empty set is rejected typed, before any
/// syscall — the kernel's EINVAL would be indistinct, the crate's error is
/// not.
#[test]
fn pinning_rejects_empty_set_typed() {
    let err = pin_current(&CpuSet::new()).unwrap_err();
    assert!(matches!(err, PinError::EmptySet), "{err:?}");
}

/// Fail-closed, always-runnable: pinning a TID that cannot exist (above the
/// kernel's `pid_max` ceiling of 2^22) is the typed `ESRCH` mapping, never
/// a raw syscall error.
#[test]
fn pinning_dead_tid_is_typed_esrch() {
    // 2_000_000_000 > every fathomable pid_max; no state is mutated.
    let err = pin_thread(2_000_000_000, &CpuSet::single(CpuId(0))).unwrap_err();
    assert!(
        matches!(err, PinError::NoSuchTask { tid: 2_000_000_000 }),
        "{err:?}"
    );
}

/// Fail-closed, always-runnable (guarded): pinning a CPU outside the
/// kernel's possible set is `EINVAL`, surfaced as a typed syscall error —
/// and the read-back verification means no state can silently change.
#[test]
fn pinning_impossible_cpu_fails_typed() {
    let possible = possible_cpu_count();
    // The crate's ceiling (8192) exceeds every kernel's NR_CPUS, so a CPU
    // just below it cannot be possible on this host unless the host really
    // is that large — in which case the test is meaningless, not failing.
    if possible >= hw_kit::CpuSet::MAX_CPUS {
        println!("hw-kit [skip] host really has {possible} possible cpus");
        return;
    }
    let absurd = CpuSet::single(CpuId(hw_kit::CpuSet::MAX_CPUS - 1));
    let guard = AffinityGuard::capture();
    let err = pin_current(&absurd).unwrap_err();
    drop(guard);
    assert!(
        matches!(
            err,
            PinError::Syscall {
                call: "sched_setaffinity",
                ..
            }
        ),
        "expected typed EINVAL, got {err:?}"
    );
}

/// `CoreId` validation composes with pinning: an out-of-range core is a typed
/// umbrella error (the crate taxonomy carrying through the API surface).
#[test]
fn out_of_range_core_is_typed_hwerror() {
    let err = CoreId::new(u16::MAX).unwrap_err();
    assert!(
        matches!(err, hw_kit::HwError::UnsupportedTarget(_)),
        "{err:?}"
    );
}

fn current_tid() -> Option<i32> {
    let stat = std::fs::read_to_string("/proc/thread-self/stat").ok()?;
    stat.split(' ').next()?.parse::<i32>().ok()
}

fn possible_cpu_count() -> u32 {
    // `/sys/devices/system/cpu/possible` is "0-N".
    let text = std::fs::read_to_string("/sys/devices/system/cpu/possible").unwrap_or_default();
    text.trim()
        .split('-')
        .next_back()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}
