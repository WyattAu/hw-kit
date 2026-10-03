//! NUMA memory placement hooks over `mmap` + `mbind` (REQ-HW-003, feature
//! `numa`).
//!
//! The v1 surface is deliberately small (spec §Decisions, owner decision #3):
//!
//! - [`numa_bind_region`] — the raw `unsafe` hook over `mbind(2)`, for
//!   policies beyond the two blessed ones. Every safety obligation is on
//!   the caller and documented at the site.
//! - [`alloc_on_node`] / [`alloc_preferred_on`] — safe wrappers for the two
//!   blessed policies ([`MpolMode::Bind`], [`MpolMode::Preferred`]). The
//!   mapping is `mmap`'d anonymous memory, `mbind`'d, then touched once so
//!   pages are physically placed before the caller sees them.
//!
//! There is no policy engine and no allocator swap: the caller owns policy,
//! hw-kit owns the syscall shape, the typed errors, and the RAII teardown
//! ([`NodeRegion`] drop = `munmap`).
//!
//! # Kernel interface note
//!
//! `mbind` is invoked through its stable syscall number (`SYS_mbind`)
//! rather than a libc symbol: the syscall ABI is fixed across libc
//! implementations (glibc/musl) and the arguments are plain values
//! (address, length, mode, node bitmap, bitmap width, flags).

use std::io;
use std::ops::{Deref, DerefMut};
use std::slice;

use crate::error::NumaError;

/// `MPOL_PREFERRED` (linux uapi `include/uapi/linux/mempolicy.h`).
const MPOL_PREFERRED: u64 = 1;
/// `MPOL_BIND` (linux uapi).
const MPOL_BIND: u64 = 2;

/// A NUMA node bitmask (dynamic little-endian words, like [`CpuSet`](crate::cpu::CpuSet)).
///
/// Invariant: trailing all-zero words are never stored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct NodeSet {
    words: Vec<u64>,
}

impl NodeSet {
    /// Highest node number representable in a hw-kit node mask. Real
    /// machines top out far below this (`MAX_NUMNODES` ≤ 4096 in every
    /// shipping kernel configuration); beyond it the crate fails closed.
    pub const MAX_NODES: u32 = 4096;

    /// An empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A set with exactly one node.
    #[must_use]
    pub fn single(node: crate::cpu::NodeId) -> Self {
        let mut set = Self::new();
        set.insert(node);
        set
    }

    /// Adds a node; returns `false` (no panic, no clamp) when the id is at
    /// or beyond [`NodeSet::MAX_NODES`].
    pub fn insert(&mut self, node: crate::cpu::NodeId) -> bool {
        if node.0 >= Self::MAX_NODES {
            return false;
        }
        let bit = node.0 as usize;
        let word = bit / 64;
        if word >= self.words.len() {
            self.words.resize(word + 1, 0);
        }
        if let Some(slot) = self.words.get_mut(word) {
            *slot |= 1_u64 << (bit % 64);
        }
        true
    }

    /// Whether the set contains the node.
    #[must_use]
    pub fn contains(&self, node: crate::cpu::NodeId) -> bool {
        let bit = node.0 as usize;
        match self.words.get(bit / 64) {
            Some(word) => word & (1_u64 << (bit % 64)) != 0,
            None => false,
        }
    }

    /// Whether the set has no members.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.words.iter().all(|w| *w == 0)
    }

    /// The number of bits the kernel will read for this mask.
    pub(crate) fn bit_width(&self) -> usize {
        self.words.len() * 64
    }

    /// The backing words for the FFI boundary.
    pub(crate) fn raw_words(&self) -> &[u64] {
        &self.words
    }
}

impl FromIterator<crate::cpu::NodeId> for NodeSet {
    fn from_iter<I: IntoIterator<Item = crate::cpu::NodeId>>(iter: I) -> Self {
        let mut set = Self::new();
        for node in iter {
            set.insert(node);
        }
        set
    }
}

/// NUMA placement policy: a node set plus an `mbind` mode.
///
/// Only the two blessed modes are constructible ([`NumaPolicy::bind_to`],
/// [`NumaPolicy::prefer`]); everything else goes through the raw
/// [`numa_bind_region`] hook with the caller's own safety argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NumaPolicy {
    /// The node set the policy names.
    pub nodes: NodeSet,
    /// The `mbind` mode.
    pub mode: MpolMode,
}

impl NumaPolicy {
    /// Hard placement: `MPOL_BIND` — pages come from the node set or the
    /// allocation fails; no silent migration to other nodes.
    #[must_use]
    pub fn bind_to(node: crate::cpu::NodeId) -> Self {
        Self {
            nodes: NodeSet::single(node),
            mode: MpolMode::Bind,
        }
    }

    /// Best-effort placement: `MPOL_PREFERRED` — the kernel tries the node
    /// first and only falls back elsewhere when it must.
    #[must_use]
    pub fn prefer(node: crate::cpu::NodeId) -> Self {
        Self {
            nodes: NodeSet::single(node),
            mode: MpolMode::Preferred,
        }
    }

    fn value(&self) -> u64 {
        match self.mode {
            MpolMode::Bind => MPOL_BIND,
            MpolMode::Preferred => MPOL_PREFERRED,
        }
    }
}

/// The blessed `mbind` modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MpolMode {
    /// `MPOL_BIND`: the node set is a hard constraint.
    Bind,
    /// `MPOL_PREFERRED`: the node set is a placement hint.
    Preferred,
}

/// Binds an existing memory region to a NUMA policy (the raw hook).
///
/// # Safety
///
/// - `addr` must be the base address of a live memory mapping of at least
///   `len` bytes that the caller owns (e.g. from `mmap` or
///   [`NodeRegion::as_mut_ptr`]); it should be page-aligned for predictable
///   application of the policy.
/// - `len` must be non-zero.
/// - The region must not be concurrently faulted by another thread during
///   the call: `mbind` with `MPOL_MF_MOVE`-less flags applies to pages
///   faulted after the call, and racing faulters defeat the placement
///   contract the caller is trying to establish.
///
/// The kernel validates the node mask; a mask naming no usable node
/// surfaces as [`NumaError::Syscall`] (`EINVAL`), never as a guessed
/// placement.
///
/// # Examples
///
/// ```no_run
/// use hw_kit::cpu::NodeId;
/// use hw_kit::numa::{alloc_on_node, numa_bind_region, NumaPolicy};
///
/// // Safe sugar: map + mbind(MPOL_BIND) + touch.
/// let mut region = alloc_on_node(64 * 1024, NodeId(0)).expect("node 0 exists");
///
/// // Raw hook: re-bind a region you already own under a different policy.
/// let policy = NumaPolicy::prefer(NodeId(1));
/// unsafe {
///     numa_bind_region(region.as_mut_ptr(), region.len(), &policy)
///         .expect("mbind");
/// }
/// ```
pub unsafe fn numa_bind_region(
    addr: *mut u8,
    len: usize,
    policy: &NumaPolicy,
) -> Result<(), NumaError> {
    if addr.is_null() {
        return Err(NumaError::InvalidRegion {
            reason: "null address".to_string(),
        });
    }
    if len == 0 {
        return Err(NumaError::InvalidRegion {
            reason: "zero length".to_string(),
        });
    }
    if policy.nodes.is_empty() {
        return Err(NumaError::InvalidRegion {
            reason: "empty node set".to_string(),
        });
    }
    let words = policy.nodes.raw_words();
    // SAFETY: `SYS_mbind`'s ABI is (addr, len, mode, nodemask*, maxnode,
    // flags); every argument is passed with its exact width — `addr` and
    // `nodemask` as pointer-width values, `maxnode` the bit width of the
    // mask the pointer names, flags 0 (no MPOL_MF_* motion). The caller's
    // documented obligations cover the pointer/length validity; the kernel
    // reads only `words` for `maxnode` bits and does not retain anything.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_mbind,
            addr as usize,
            len,
            policy.value(),
            words.as_ptr() as usize,
            policy.nodes.bit_width(),
            0_u64,
        )
    };
    if rc < 0 {
        return Err(NumaError::Syscall {
            call: "mbind",
            source: io::Error::last_os_error(),
        });
    }
    Ok(())
}

/// Maps `len` bytes of anonymous memory hard-bound to `node`
/// (`MPOL_BIND`), touches every page so placement is physical, and returns
/// an owning [`NodeRegion`] (drop = `munmap`).
///
/// The touch is deliberate: `mbind` governs pages faulted *after* the call,
/// so a lazily-faulted region would silently place by first-touch. The
/// linear pass costs time exactly once, up front — where a placement
/// violation would be cheapest to see.
///
/// # Examples
///
/// ```no_run
/// use hw_kit::cpu::NodeId;
/// use hw_kit::numa::alloc_on_node;
///
/// let region = alloc_on_node(2 * 1024 * 1024, NodeId(0)).expect("bound to node 0");
/// assert_eq!(region.len(), 2 * 1024 * 1024);
/// // Drop = munmap.
/// drop(region);
/// ```
pub fn alloc_on_node(len: usize, node: crate::cpu::NodeId) -> Result<NodeRegion, NumaError> {
    alloc_with_policy(len, &NumaPolicy::bind_to(node), node)
}

/// Maps `len` bytes of anonymous memory with `MPOL_PREFERRED` placement on
/// `node` — the kernel's best-effort form of [`alloc_on_node`].
pub fn alloc_preferred_on(len: usize, node: crate::cpu::NodeId) -> Result<NodeRegion, NumaError> {
    alloc_with_policy(len, &NumaPolicy::prefer(node), node)
}

fn alloc_with_policy(
    len: usize,
    policy: &NumaPolicy,
    node: crate::cpu::NodeId,
) -> Result<NodeRegion, NumaError> {
    if len == 0 {
        return Err(NumaError::InvalidRegion {
            reason: "zero length".to_string(),
        });
    }
    if node.0 >= NodeSet::MAX_NODES {
        return Err(NumaError::UnsupportedTarget(format!(
            "node {node} exceeds the crate's node-mask capacity ({})",
            NodeSet::MAX_NODES
        )));
    }
    let ptr = map_anonymous(len)?;
    let region = NodeRegion { ptr, len, node };
    // Bind before the first touch: pages faulted below land under the
    // policy. On failure the region's drop runs (munmap) — no leak.
    // SAFETY: `ptr` is a fresh, live mapping of exactly `len` bytes owned
    // by `region`; the policy is non-empty (checked in the hook), so every
    // obligation in `numa_bind_region`'s `# Safety` contract is met.
    unsafe { numa_bind_region(ptr, len, policy) }?;
    // SAFETY: `ptr` is a live mapping of `len` bytes (mapped above, bound
    // and owned by `region`); writing `len` zero bytes touches every page
    // so the binding is physical before the caller sees the region.
    unsafe { std::ptr::write_bytes(ptr, 0, len) };
    Ok(region)
}

fn map_anonymous(len: usize) -> Result<*mut u8, NumaError> {
    // SAFETY: mmap of anonymous private memory with fd -1 / offset 0 is the
    // documented, self-contained form; a failed mmap is MAP_FAILED and
    // nothing is mapped on that path.
    let raw = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    if raw == libc::MAP_FAILED {
        return Err(NumaError::Syscall {
            call: "mmap",
            source: io::Error::last_os_error(),
        });
    }
    Ok(raw.cast::<u8>())
}

/// An owning, node-bound memory region (drop = `munmap`).
#[derive(Debug)]
pub struct NodeRegion {
    ptr: *mut u8,
    len: usize,
    node: crate::cpu::NodeId,
}

impl NodeRegion {
    /// The region length in bytes (as requested — the kernel may round the
    /// mapping up to whole pages internally).
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the region is empty (never true for a constructed region —
    /// zero lengths are rejected typed).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The node the region was bound to.
    #[must_use]
    pub fn node(&self) -> crate::cpu::NodeId {
        self.node
    }

    /// Base pointer (for handing slices to I/O syscalls, ring setup, …).
    #[must_use]
    pub fn as_ptr(&self) -> *const u8 {
        self.ptr
    }

    /// Mutable base pointer.
    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.ptr
    }
}

impl Deref for NodeRegion {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        // SAFETY: `ptr` is a live mapping of exactly `len` bytes for the
        // region's whole lifetime (drop is the only munmap), and the
        // returned borrow cannot outlive `&self`.
        unsafe { slice::from_raw_parts(self.ptr, self.len) }
    }
}

impl DerefMut for NodeRegion {
    fn deref_mut(&mut self) -> &mut [u8] {
        // SAFETY: as above, with exclusive access guaranteed by `&mut self`.
        unsafe { slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for NodeRegion {
    fn drop(&mut self) {
        // SAFETY: `ptr` came from `mmap` (length `len`) and is unmapped
        // exactly once, here, after which the region is gone.
        unsafe { libc::munmap(self.ptr.cast(), self.len) };
    }
}

// SAFETY: a `NodeRegion` owns one process-private anonymous mapping with
// no shared state — moving it across threads moves ownership of the
// mapping, exactly as moving a `Vec<u8>` would.
unsafe impl Send for NodeRegion {}
// SAFETY: ordinary byte access is the only operation on the mapping; there
// is no shared mutable state to synchronize (see `Send` above).
unsafe impl Sync for NodeRegion {}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]
    use super::*;
    use crate::cpu::NodeId;

    /// Exercises the RAII slice-view surface (`Deref`/`DerefMut`/`len`/
    /// `as_ptr`/drop-munmap) over a plain anonymous mapping — the region
    /// type logic is independent of where the pages came from. (The
    /// syscall-bound placement path is covered by the env-gated
    /// integration test; this one runs everywhere.)
    #[test]
    fn node_region_slice_view_round_trip() {
        const LEN: usize = 3 * 4096;
        // SAFETY: ordinary anonymous private mmap; failure is MAP_FAILED
        // and the test fails there — nothing is unmapped twice.
        let raw = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                LEN,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        assert_ne!(raw, libc::MAP_FAILED);
        let mut region = NodeRegion {
            ptr: raw.cast::<u8>(),
            len: LEN,
            node: NodeId(0),
        };

        assert_eq!(region.len(), LEN);
        assert!(!region.is_empty());
        assert_eq!(region.node(), NodeId(0));
        assert_eq!(region.as_ptr(), raw.cast::<u8>());

        region[0] = 0xAB;
        region[LEN - 1] = 0xCD;
        assert_eq!(region[0], 0xAB);
        assert_eq!(region.deref()[1], 0);
        assert_eq!(region.deref()[LEN - 1], 0xCD);
        region.deref_mut()[1] = 0x11;
        assert_eq!(region[1], 0x11);

        // Drop munmaps exactly once (a double munmap would be UB; ASAN/
        // valgrind would catch it — the kernel tolerates only the one).
        drop(region);
    }
}
