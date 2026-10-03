//! REQ-HW-003 NUMA binding tests (feature `numa`).
//!
//! Posture (spec §Gate plan — "failure-path coverage is a hard gate"):
//! - **fail-closed arms run everywhere, always** — bad regions, empty node
//!   sets, impossible nodes: every typed `NumaError` arm is exercised
//!   without hardware;
//! - the **success path** (`mbind` on a real node) is env-gated
//!   (`HW_KIT_HW_TEST=1`) for the self-hosted hardware runner.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
// Region byte access intentionally indexes (first and last byte).
#![allow(clippy::indexing_slicing)]

use std::ptr::NonNull;

use hw_kit::cpu::NodeId;
use hw_kit::error::NumaError;
use hw_kit::numa::{alloc_on_node, alloc_preferred_on, numa_bind_region, NodeSet, NumaPolicy};
use proptest::prelude::*;

fn dangling_ptr() -> *mut u8 {
    // Non-null, unaligned-to-nothing value for validation-path tests: the
    // kernel validates the address range without dereferencing it.
    NonNull::<u8>::dangling().as_ptr()
}

fn gated(name: &str) -> bool {
    if let Ok("1") = std::env::var("HW_KIT_HW_TEST").as_deref() {
        true
    } else {
        println!("hw-kit [skip] {name}: set HW_KIT_HW_TEST=1 (hardware runner) to run");
        false
    }
}

/// REQ-HW-003 — `numa_bind_fails_closed_on_bad_policy` (always-runnable).
#[test]
fn numa_bind_fails_closed_on_bad_policy() {
    // Null address.
    // SAFETY: none of this test's calls reach the kernel — each is rejected
    // in the hook's validation path before the syscall.
    let err =
        unsafe { numa_bind_region(std::ptr::null_mut(), 4096, &NumaPolicy::bind_to(NodeId(0))) }
            .unwrap_err();
    assert!(matches!(err, NumaError::InvalidRegion { .. }), "{err:?}");

    // Zero length.
    // SAFETY: rejected in validation (zero length) before any syscall.
    let err = unsafe { numa_bind_region(dangling_ptr(), 0, &NumaPolicy::bind_to(NodeId(0))) }
        .unwrap_err();
    assert!(matches!(err, NumaError::InvalidRegion { .. }), "{err:?}");

    // Empty node set — the kernel's EINVAL would be indistinct; this is not.
    let empty = NumaPolicy {
        nodes: NodeSet::new(),
        mode: hw_kit::numa::MpolMode::Bind,
    };
    // SAFETY: rejected in validation (empty node set) before any syscall.
    let err = unsafe { numa_bind_region(dangling_ptr(), 4096, &empty) }.unwrap_err();
    assert!(matches!(err, NumaError::InvalidRegion { .. }), "{err:?}");
}

/// Fail-closed, always-runnable: an address range with no mapping behind it
/// reaches the kernel and comes back as a typed `mbind` failure.
#[test]
fn numa_bind_unmapped_range_is_typed_syscall() {
    // SAFETY: the dangling pointer is never dereferenced — mbind validates
    // the address range in kernel space and reports ENOMEM, which surfaces
    // typed. (No userspace memory is read during validation.)
    let err = unsafe { numa_bind_region(dangling_ptr(), 4096, &NumaPolicy::bind_to(NodeId(0))) }
        .unwrap_err();
    assert!(
        matches!(err, NumaError::Syscall { call: "mbind", .. }),
        "{err:?}"
    );
}

/// Fail-closed, always-runnable: a node beyond the crate's mask capacity is
/// rejected before any syscall — never clamped, never guessed.
#[test]
fn alloc_beyond_node_capacity_is_typed() {
    let err = alloc_on_node(4096, NodeId(NodeSet::MAX_NODES)).unwrap_err();
    assert!(matches!(err, NumaError::UnsupportedTarget(_)), "{err:?}");
}

/// Fail-closed, always-runnable (guarded): binding to a node no real host
/// has (3000 ≫ any shipping `MAX_NUMNODES`) is the kernel's EINVAL surfaced
/// typed. Skipped (honestly) on a hypothetical 3000-node machine.
#[test]
fn alloc_on_impossible_node_is_typed() {
    if max_possible_node() >= 3000 {
        println!(
            "hw-kit [skip] host reports {} possible nodes",
            max_possible_node()
        );
        return;
    }
    let err = alloc_on_node(4096, NodeId(3000)).unwrap_err();
    assert!(
        matches!(err, NumaError::Syscall { call: "mbind", .. }),
        "{err:?}"
    );
}

/// Fail-closed, always-runnable: zero length.
#[test]
fn alloc_zero_length_is_typed() {
    let err = alloc_on_node(0, NodeId(0)).unwrap_err();
    assert!(matches!(err, NumaError::InvalidRegion { .. }), "{err:?}");
}

/// Gated success path: `mbind(MPOL_BIND)` against a real node, placement
/// cross-checked against `/proc/self/numa_maps` when readable.
#[test]
fn alloc_on_node_places_physically() {
    if !gated("alloc_on_node_places_physically") {
        return;
    }
    let len = 1024 * 1024;
    let mut region = alloc_on_node(len, NodeId(0)).expect("node 0 exists on every NUMA host");
    assert_eq!(region.len(), len);
    assert_eq!(region.node(), NodeId(0));

    // The touch in alloc_on_node made the bytes writable before hand-off.
    region[0] = 0xAB;
    region[len - 1] = 0xCD;
    assert_eq!(region[0], 0xAB);

    // Best-effort evidence from the kernel's own accounting: the VMA that
    // backs the region carries a bind policy for node 0.
    if let Ok(maps) = std::fs::read_to_string("/proc/self/numa_maps") {
        let base = region.as_ptr() as usize;
        let line = maps
            .lines()
            .find(|l| {
                l.split(' ')
                    .next()
                    .and_then(|a| usize::from_str_radix(a.trim_start_matches("0x"), 16).ok())
                    == Some(base)
            })
            .expect("region vma listed in numa_maps");
        assert!(
            line.contains("bind:0"),
            "expected bind:0 policy, got: {line}"
        );
    } else {
        println!("hw-kit [note] /proc/self/numa_maps unreadable; placement evidence skipped");
    }

    let pref = alloc_preferred_on(len, NodeId(0)).expect("preferred placement");
    assert_eq!(pref.node(), NodeId(0));
}

fn max_possible_node() -> u32 {
    let text = std::fs::read_to_string("/sys/devices/system/node/possible").unwrap_or_default();
    text.trim()
        .split('-')
        .next_back()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// NodeSet mirrors CpuSet's fail-closed insertion semantics over
    /// arbitrary ids: accepted exactly below capacity, never clamped.
    #[test]
    fn node_set_insert_is_fail_closed(raw in any::<u16>()) {
        let mut set = NodeSet::new();
        let inserted = set.insert(NodeId(u32::from(raw)));
        prop_assert_eq!(inserted, u32::from(raw) < NodeSet::MAX_NODES);
        prop_assert_eq!(set.contains(NodeId(u32::from(raw))), inserted);
        if !inserted {
            prop_assert!(set.is_empty());
        }
    }
}
