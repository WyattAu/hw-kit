//! REQ-HW-004 hugepage reservation tests (feature `hugepage`).
//!
//! Posture: the typed failure path runs **everywhere, always** — the test
//! asserts the typed error, not success, whenever `/proc/sys`-adjacent
//! state (`/sys/kernel/mm/hugepages`) says the pool is empty or absent.
//! The success path is env-gated (`HW_KIT_HUGEPAGE_TEST=1`) for the
//! self-hosted hardware runner. There is no silent 4 KiB fallback to assert
//! against: a success on an empty pool would *be* the bug.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Region byte access intentionally indexes (byte 0 and the last byte).
#![allow(clippy::indexing_slicing)]

use hw_kit::error::HugetlbError;
use hw_kit::hugepage::{hugepage_reserve, HugeSize};

const MIB2: usize = 2 * 1024 * 1024;

fn gated(name: &str) -> bool {
    if let Ok("1") = std::env::var("HW_KIT_HUGEPAGE_TEST").as_deref() {
        true
    } else {
        println!("hw-kit [skip] {name}: set HW_KIT_HUGEPAGE_TEST=1 (hardware runner) to run");
        false
    }
}

/// The 2 MiB pool's reserved page count, when the host reports one.
fn nr_hugepages_2m() -> Option<u64> {
    let text =
        std::fs::read_to_string("/sys/kernel/mm/hugepages/hugepages-2048kB/nr_hugepages").ok()?;
    text.trim().parse().ok()
}

fn has_1g_pool() -> bool {
    std::path::Path::new("/sys/kernel/mm/hugepages/hugepages-1048576kB").exists()
}

/// REQ-HW-004 — `hugepage_exhaustion_is_typed_no_silent_fallback`.
///
/// On a host whose 2 MiB pool has zero pages (every shared CI runner), the
/// reservation **must fail typed** — `Ok` here would mean a silent
/// fallback happened and is a hard failure of this test.
#[test]
fn hugepage_exhaustion_is_typed_no_silent_fallback() {
    match nr_hugepages_2m() {
        Some(0) => {
            match hugepage_reserve(MIB2, HugeSize::Default2M) {
                Err(HugetlbError::Unavailable {
                    size_class: "2MiB", ..
                }) => {
                    // Typed exhaustion — the contract.
                }
                Err(other) => panic!("exhaustion surfaced as the wrong typed arm: {other}"),
                Ok(_) => panic!(
                    "reserved a hugepage against a zero pool — silent 4 KiB fallback \
                     (REQ-HW-004 violated)"
                ),
            }
        }
        Some(nr) => {
            // Pool has pages: the reservation must actually work, and the
            // memory must be real.
            let region = hugepage_reserve(MIB2, HugeSize::Default2M)
                .unwrap_or_else(|e| panic!("pool reports {nr} free pages, reserve failed: {e}"));
            assert_eq!(region.len(), MIB2);
            let mut region = region;
            region[0] = 0xAB;
            assert_eq!(region[0], 0xAB);
        }
        None => {
            // Host does not report the pool at all: mmap must still decide,
            // typed. Most such hosts fail with Unavailable (ENOSYS-family).
            match hugepage_reserve(MIB2, HugeSize::Default2M) {
                Err(typed) => println!("hw-kit [note] unprobed host fails typed: {typed}"),
                Ok(_) => panic!(
                    "reserved hugepages on a host that cannot probe its pool — silent \
                     4 KiB fallback (REQ-HW-004 violated)"
                ),
            }
        }
    }
}

/// Fail-closed, always-runnable: a zero length is a typed error before any
/// syscall.
#[test]
fn zero_length_is_typed() {
    let err = hugepage_reserve(0, HugeSize::Default2M).unwrap_err();
    assert!(matches!(err, HugetlbError::ZeroLength), "{err:?}");
}

/// Fail-closed, always-runnable: an unaligned length is typed — the kernel
/// would silently round the mapping up, and hw-kit does not adopt
/// silently-rounded contracts.
#[test]
fn unaligned_length_is_typed() {
    let err = hugepage_reserve(4096, HugeSize::Default2M).unwrap_err();
    match err {
        HugetlbError::UnalignedLength { len, page } => {
            assert_eq!(len, 4096);
            assert_eq!(page, 2048 * 1024);
        }
        other => panic!("expected UnalignedLength, got {other:?}"),
    }
}

/// Fail-closed, always-runnable (host-dependent assertion): the 1 GiB class
/// is only reservable where the kernel has a 1 GiB pool; everywhere else it
/// must be one of the two typed "no such pool" arms — never a fallback.
#[test]
fn one_gib_class_is_typed_where_absent() {
    if has_1g_pool() {
        println!("hw-kit [note] host has a 1GiB pool; absence arm not applicable here");
        return;
    }
    let len = 1024 * 1024 * 1024;
    match hugepage_reserve(len, HugeSize::Default1G) {
        Err(HugetlbError::UnsupportedSize { size_class: "1GiB" }) => {}
        Err(HugetlbError::Unavailable {
            size_class: "1GiB", ..
        }) => {}
        Err(other) => panic!("expected a typed 1GiB-pool arm, got {other:?}"),
        Ok(_) => panic!(
            "reserved 1GiB hugepages on a host with no 1GiB pool — silent fallback \
             (REQ-HW-004 violated)"
        ),
    }
}

/// Gated success path for the 1 GiB class (hardware runner with a
/// provisioned 1 GiB pool).
#[test]
fn one_gib_reservation_round_trip() {
    if !gated("one_gib_reservation_round_trip") || !has_1g_pool() {
        println!("hw-kit [skip] one_gib_reservation_round_trip: needs HW_KIT_HUGEPAGE_TEST=1 and a 1GiB pool");
        return;
    }
    let nr = std::fs::read_to_string("/sys/kernel/mm/hugepages/hugepages-1048576kB/nr_hugepages")
        .ok()
        .and_then(|t| t.trim().parse::<u64>().ok())
        .unwrap_or(0);
    if nr == 0 {
        println!("hw-kit [skip] 1GiB pool exists but holds 0 pages");
        return;
    }
    let region = hugepage_reserve(1024 * 1024 * 1024, HugeSize::Default1G).expect("1GiB reserve");
    assert_eq!(region.len(), 1024 * 1024 * 1024);
}
