//! REQ-HW-001 integration smoke against the live host — the committed
//! fixture trees (including the malformed ones) are exercised in the lib
//! unit tests (`src/topology.rs`); this file asserts the *live* parse is
//! structurally sound, which is exactly what cannot be asserted hermetically.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use hw_kit::{discover, CpuTopology, TopologyError};

/// The live `/sys` parse succeeds and satisfies the structural invariants:
/// unique ascending ids, every sibling set self-inclusive, package ids
/// consistent with the per-CPU facts.
#[test]
fn live_host_discovery_smoke() {
    let topo: CpuTopology = match discover() {
        Ok(topo) => topo,
        Err(TopologyError::Io { .. }) => {
            panic!("/sys/devices/system/cpu must exist on a Linux host")
        }
        Err(other) => panic!("live /sys parse failed: {other}"),
    };

    assert!(
        !topo.cpus.is_empty(),
        "a Linux host reports at least one cpu"
    );

    let ids: Vec<u32> = topo.cpus.iter().map(|c| c.id.0).collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(ids, sorted, "cpus must be unique and ascending");

    for cpu in &topo.cpus {
        assert!(
            cpu.siblings.contains(cpu.id),
            "cpu{} sibling set must include itself (sysfs semantics)",
            cpu.id.0
        );
    }

    // Node membership agrees both ways (field + helper).
    for cpu in &topo.cpus {
        assert_eq!(
            cpu.node,
            topo.node_of(cpu.id),
            "node_of must mirror CpuInfo::node"
        );
    }

    // Every package id in the per-CPU facts appears in the package list.
    for pkg in &topo.packages {
        assert!(
            topo.cpus.iter().any(|c| &c.package == pkg),
            "package {pkg} unrepresented"
        );
    }
}

/// An explicit root that is not sysfs is a typed Io error — the same typed
/// surface a drifted `/sys` mount would produce.
#[test]
fn nonexistent_root_is_typed_io() {
    let err = hw_kit::discover_at(std::path::Path::new("/nonexistent/sys-root")).unwrap_err();
    assert!(matches!(err, TopologyError::Io { .. }), "{err:?}");
}
