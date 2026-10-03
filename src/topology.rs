//! CPU topology discovery from sysfs (REQ-HW-001).
//!
//! [`discover`] parses `/sys/devices/system/cpu` (and the sibling
//! `/sys/devices/system/node` NUMA tree under the same sysfs root) with pure
//! `std` — no libnuma, no hwloc. The result is a typed [`CpuTopology`]; an
//! absent or unparseable entry is a typed [`TopologyError`] or an explicit
//! [`Option`] — never a guessed default.
//!
//! Parsing is structural and deliberately minimal so kernel layout drift
//! turns into typed errors, not wrong data (spec §Risk register). The unit
//! tests below run against committed fixture trees
//! (`tests/fixtures/sysfs/*`), including malformed ones, so the parser is
//! exercised hermetically — and under miri, since this module is pure `std`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::cpu::{CoreId, CpuId, CpuSet, NodeId, PackageId, SmallCpuList};
use crate::error::TopologyError;

/// Sysfs root of the CPU topology tree.
const SYS_CPU: &str = "devices/system/cpu";
/// Sysfs root of the NUMA node tree (sibling of the CPU tree).
const SYS_NODE: &str = "devices/system/node";

/// The parsed CPU topology of the machine (REQ-HW-001).
///
/// Discovery is a cold path: the surface deliberately uses plain [`Vec`]s
/// (spec §Decisions, owner-reviewed) — the hot path is the consumer's
/// memory, not this parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuTopology {
    /// Every CPU, ascending by id.
    pub cpus: Vec<CpuInfo>,
    /// Distinct package (socket) ids, ascending.
    pub packages: Vec<PackageId>,
    /// NUMA nodes, ascending by id. Empty when the sysfs root has no node
    /// tree (then [`CpuInfo::node`] is [`None`] for every CPU).
    pub numa_nodes: Vec<NodeInfo>,
}

impl CpuTopology {
    /// The NUMA node a CPU belongs to, if the node tree reported one.
    #[must_use]
    pub fn node_of(&self, cpu: CpuId) -> Option<NodeId> {
        self.numa_nodes
            .iter()
            .find(|n| n.cpus.contains(cpu))
            .map(|n| n.id)
    }
}

/// One CPU's placement facts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuInfo {
    /// The logical CPU number (`cpuN`).
    pub id: CpuId,
    /// The package (socket) the CPU sits in.
    pub package: PackageId,
    /// The physical core (unique within a package).
    pub core: CoreId,
    /// The NUMA node, when the sysfs node tree covers this CPU.
    /// Explicitly [`Option`] — absence is information, not a default.
    pub node: Option<NodeId>,
    /// SMT sibling set (includes this CPU itself, as sysfs reports it).
    pub siblings: SmallCpuList,
}

/// One NUMA node's membership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeInfo {
    /// The node number (`nodeN`).
    pub id: NodeId,
    /// The CPUs home to this node.
    pub cpus: CpuSet,
}

/// Discovers the topology of the running host from `/sys`.
///
/// # Examples
///
/// ```
/// let topo = hw_kit::discover().expect("host topology");
/// assert!(!topo.cpus.is_empty(), "a Linux host reports at least one cpu");
/// println!("{} cpus across {} package(s)", topo.cpus.len(), topo.packages.len());
/// ```
pub fn discover() -> Result<CpuTopology, TopologyError> {
    discover_at(Path::new("/sys"))
}

/// Discovers the topology under an explicit sysfs root.
///
/// The test seam that keeps discovery hermetic: unit and integration tests
/// feed committed fixture trees instead of the live host.
pub fn discover_at(sys_root: &Path) -> Result<CpuTopology, TopologyError> {
    let cpu_root = sys_root.join(SYS_CPU);
    let mut cpus: Vec<CpuInfo> = Vec::new();
    for (name, path) in list_dirs(&cpu_root)? {
        let Some(raw) = name.strip_prefix("cpu") else {
            continue; // cpuidle/, hotplug/, power/, … are not cpu entries
        };
        let Ok(id) = raw.parse::<u32>() else {
            continue;
        };
        cpus.push(parse_cpu(&path, CpuId(id))?);
    }
    if cpus.is_empty() {
        return Err(TopologyError::Malformed {
            path: cpu_root.display().to_string(),
            reason: "no cpu entries found".to_string(),
        });
    }
    cpus.sort_by_key(|c| c.id);

    let mut packages: Vec<PackageId> = cpus.iter().map(|c| c.package).collect();
    packages.sort();
    packages.dedup();

    let numa_nodes = parse_nodes(&sys_root.join(SYS_NODE))?;

    // Attach node membership: a CPU no node claims stays `None` (explicit).
    for cpu in &mut cpus {
        cpu.node = numa_nodes
            .iter()
            .find(|n| n.cpus.contains(cpu.id))
            .map(|n| n.id);
    }

    Ok(CpuTopology {
        cpus,
        packages,
        numa_nodes,
    })
}

fn parse_nodes(node_root: &Path) -> Result<Vec<NodeInfo>, TopologyError> {
    let mut nodes = Vec::new();
    for (name, path) in match list_dirs(node_root) {
        Ok(entries) => entries,
        Err(TopologyError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            return Ok(nodes); // UMA host (or kernel without NUMA): explicit absence
        }
        Err(e) => return Err(e),
    } {
        let Some(raw) = name.strip_prefix("node") else {
            continue;
        };
        let Ok(id) = raw.parse::<u32>() else {
            continue;
        };
        let cpulist_path = path.join("cpulist");
        let Some(text) = read_field(&cpulist_path)? else {
            return Err(TopologyError::MissingField {
                entry: format!("node{id}"),
                field: "cpulist".to_string(),
            });
        };
        let cpus = CpuSet::parse_allowed_list(&text)
            .map_err(|e| malformed(&cpulist_path, &e.to_string()))?;
        nodes.push(NodeInfo {
            id: NodeId(id),
            cpus,
        });
    }
    nodes.sort_by_key(|n| n.id);
    Ok(nodes)
}

fn parse_cpu(cpu_dir: &Path, id: CpuId) -> Result<CpuInfo, TopologyError> {
    let topo_dir = cpu_dir.join("topology");

    let package_raw = required_field(&topo_dir, cpu_dir, "physical_package_id")?;
    let package = PackageId(
        package_raw
            .parse::<u32>()
            .map_err(|_| malformed(&topo_dir.join("physical_package_id"), &package_raw))?,
    );

    let core_raw = required_field(&topo_dir, cpu_dir, "core_id")?;
    let core = core_raw
        .parse::<u16>()
        .ok()
        .and_then(|raw| CoreId::new(raw).ok())
        .ok_or_else(|| {
            malformed(
                &topo_dir.join("core_id"),
                &format!("`{core_raw}` is not a representable core number"),
            )
        })?;

    // Sibling sets: `thread_siblings_list` is the long-standing name;
    // `core_cpus_list` is the newer spelling. One of them is required.
    let siblings = match read_field(&topo_dir.join("thread_siblings_list"))? {
        Some(text) if !text.is_empty() => text,
        Some(_) => {
            return Err(malformed(
                &topo_dir.join("thread_siblings_list"),
                "empty sibling list",
            ))
        }
        None => match read_field(&topo_dir.join("core_cpus_list"))? {
            Some(text) if !text.is_empty() => text,
            Some(_) => {
                return Err(malformed(
                    &topo_dir.join("core_cpus_list"),
                    "empty sibling list",
                ))
            }
            None => {
                return Err(TopologyError::MissingField {
                    entry: cpu_dir.display().to_string(),
                    field: "thread_siblings_list".to_string(),
                })
            }
        },
    };
    let set = CpuSet::parse_allowed_list(&siblings)
        .map_err(|e| malformed(&topo_dir.join("thread_siblings_list"), &e.to_string()))?;
    let siblings: SmallCpuList = set.iter().collect();

    Ok(CpuInfo {
        id,
        package,
        core,
        node: None, // filled in by discover_at from the node tree
        siblings,
    })
}

/// Directories directly under `dir`, sorted by name for determinism.
fn list_dirs(dir: &Path) -> Result<Vec<(String, PathBuf)>, TopologyError> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).map_err(|source| TopologyError::Io {
        path: dir.display().to_string(),
        source,
    })? {
        let Ok(entry) = entry else {
            continue; // entry raced away between list and stat
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.path().is_dir() {
            out.push((name, entry.path()));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Reads and trims a sysfs attribute. `Ok(None)` is ENOENT (callers map it
/// to a typed `MissingField`); any other IO error is a typed `Io`.
fn read_field(path: &Path) -> Result<Option<String>, TopologyError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text.trim().to_string())),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(TopologyError::Io {
            path: path.display().to_string(),
            source,
        }),
    }
}

fn required_field(topo_dir: &Path, cpu_dir: &Path, field: &str) -> Result<String, TopologyError> {
    match read_field(&topo_dir.join(field))? {
        Some(text) if !text.is_empty() => Ok(text),
        Some(_) => Err(malformed(&topo_dir.join(field), "empty value")),
        None => Err(TopologyError::MissingField {
            entry: cpu_dir.display().to_string(),
            field: field.to_string(),
        }),
    }
}

fn malformed(path: &Path, reason: &str) -> TopologyError {
    TopologyError::Malformed {
        path: path.display().to_string(),
        reason: reason.to_string(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    /// Committed sysfs fixture root: hermetic, malformed-inclusive trees
    /// (also exercised by `tests/topology.rs` against the live host).
    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/sysfs")
            .join(name)
            .join("sys")
    }

    /// REQ-HW-001 — `topology_parses_synthetic_sysfs`: the committed
    /// two-package/two-node fixture parses into the exact typed facts.
    #[test]
    fn topology_parses_synthetic_sysfs() {
        let topo = discover_at(&fixture("two-node")).unwrap();
        assert_eq!(topo.cpus.len(), 4);
        assert_eq!(
            topo.cpus.iter().map(|c| c.id.0).collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
        assert_eq!(topo.packages, vec![PackageId(0), PackageId(1)]);

        let cpu0 = &topo.cpus[0];
        assert_eq!(cpu0.package, PackageId(0));
        assert_eq!(cpu0.core, CoreId::new(0).unwrap());
        assert_eq!(cpu0.node, Some(NodeId(0)));
        assert_eq!(cpu0.siblings.allowed_list(), "0,2");

        let cpu3 = &topo.cpus[3];
        assert_eq!(cpu3.package, PackageId(1));
        assert_eq!(cpu3.core, CoreId::new(1).unwrap());
        assert_eq!(cpu3.node, Some(NodeId(1)));
        assert_eq!(cpu3.siblings.allowed_list(), "1,3");

        assert_eq!(topo.numa_nodes.len(), 2);
        assert_eq!(topo.numa_nodes[0].id, NodeId(0));
        assert_eq!(topo.numa_nodes[0].cpus.allowed_list(), "0,2");
        assert_eq!(topo.numa_nodes[1].cpus.allowed_list(), "1,3");
        assert_eq!(topo.node_of(CpuId(2)), Some(NodeId(0)));
    }

    #[test]
    fn range_form_lists_parse() {
        let topo = discover_at(&fixture("ranges")).unwrap();
        assert_eq!(topo.numa_nodes[0].cpus.allowed_list(), "0-3");
        assert_eq!(topo.cpus[0].siblings.allowed_list(), "0-1");
        assert_eq!(topo.cpus[2].core, CoreId::new(1).unwrap());
    }

    #[test]
    fn core_cpus_list_fallback_is_honored() {
        let topo = discover_at(&fixture("ranges")).unwrap();
        // cpu4 carries only `core_cpus_list` (the newer sysfs name).
        assert_eq!(topo.cpus[4].siblings.allowed_list(), "4");
    }

    #[test]
    fn uma_host_without_node_tree_yields_none_not_a_guess() {
        let topo = discover_at(&fixture("uma-no-node")).unwrap();
        assert!(topo.numa_nodes.is_empty());
        assert!(topo.cpus.iter().all(|c| c.node.is_none()));
    }

    #[test]
    fn malformed_entries_are_typed_never_guessed() {
        // cpu0's physical_package_id reads "zero".
        let err = discover_at(&fixture("malformed")).unwrap_err();
        match err {
            TopologyError::Malformed { path, reason } => {
                assert!(path.contains("cpu0/topology/physical_package_id"), "{path}");
                assert_eq!(reason, "zero");
            }
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[test]
    fn missing_required_field_is_typed() {
        // cpu1 lacks topology/core_id entirely.
        let err = discover_at(&fixture("missing-field")).unwrap_err();
        assert!(
            matches!(err, TopologyError::MissingField { ref field, .. } if field == "core_id"),
            "{err:?}"
        );
    }

    #[test]
    fn empty_sibling_list_is_typed() {
        let err = discover_at(&fixture("empty-siblings")).unwrap_err();
        match err {
            TopologyError::Malformed { reason, .. } => {
                assert!(reason.contains("empty sibling list"), "{reason}");
            }
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[test]
    fn absent_cpu_tree_is_a_typed_io_error() {
        let err = discover_at(&fixture("does-not-exist")).unwrap_err();
        assert!(matches!(err, TopologyError::Io { .. }), "{err:?}");
    }

    #[test]
    fn missing_node_cpulist_is_typed() {
        let err = discover_at(&fixture("node-without-cpulist")).unwrap_err();
        assert!(
            matches!(err, TopologyError::MissingField { ref field, .. } if field == "cpulist"),
            "{err:?}"
        );
    }
}
