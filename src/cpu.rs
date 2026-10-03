//! Pure-`std` CPU/node identifier and affinity-mask types.
//!
//! This module has no dependency on `libc` (REQ-HW-006): identifiers are
//! plain newtypes, and [`CpuSet`] is a dynamic 64-bit-word bitmask whose
//! `0-3,8` "allowed list" format round-trips with `/proc` and sysfs. The
//! module is miri-eligible and unit-tested without a kernel.

use std::fmt;

/// A logical CPU number as it appears in sysfs (`/sys/devices/system/cpu/cpuN`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CpuId(pub u32);

impl CpuId {
    /// The raw numeric id.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Display for CpuId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cpu{}", self.0)
    }
}

/// A (physical) core id.
///
/// In sysfs, `topology/core_id` is unique within a package; hw-kit also uses
/// the type as the ergonomic single-core handle for
/// [`pin_current_core`](crate::affinity::pin_current_core). Construction is
/// validated (fail-closed) against the crate's mask capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CoreId(u16);

impl CoreId {
    /// Highest core id the crate accepts; anything at or beyond
    /// [`CpuSet::MAX_CPUS`] cannot be represented in an affinity mask and is
    /// rejected with a typed error rather than clamped.
    pub const MAX: u16 = (CpuSet::MAX_CPUS - 1) as u16;

    /// Validates a raw core id (fail-closed).
    ///
    /// Ids at or beyond [`CoreId::MAX`] yield
    /// [`HwError::UnsupportedTarget`](crate::error::HwError::UnsupportedTarget) — never a silent clamp.
    pub fn new(raw: u16) -> Result<Self, crate::error::HwError> {
        if u32::from(raw) >= CpuSet::MAX_CPUS {
            return Err(crate::error::HwError::UnsupportedTarget(format!(
                "core id {raw} exceeds the crate's cpu-mask capacity ({})",
                CpuSet::MAX_CPUS
            )));
        }
        Ok(Self(raw))
    }

    /// The raw id.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl std::convert::TryFrom<u16> for CoreId {
    type Error = crate::error::HwError;

    fn try_from(raw: u16) -> Result<Self, Self::Error> {
        CoreId::new(raw)
    }
}

impl From<CoreId> for CpuId {
    fn from(value: CoreId) -> Self {
        CpuId(u32::from(value.0))
    }
}

impl fmt::Display for CoreId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "core{}", self.0)
    }
}

/// A NUMA node number (`/sys/devices/system/node/nodeN`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NodeId(pub u32);

impl NodeId {
    /// The raw numeric id.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "node{}", self.0)
    }
}

/// A physical package (socket) id (`topology/physical_package_id`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PackageId(pub u32);

impl PackageId {
    /// The raw numeric id.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Display for PackageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "package{}", self.0)
    }
}

/// An affinity mask: a dynamic little-endian bitmap of CPU numbers.
///
/// Invariant: trailing all-zero words are never stored, so two sets with the
/// same members always compare equal via the derived [`PartialEq`].
///
/// Ids at or beyond [`CpuSet::MAX_CPUS`] (the kernel's own `CONFIG_NR_CPUS`
/// ceiling on every shipping configuration) are rejected: [`insert`] returns
/// `false`, and range constructors clip. Syscall-boundary code re-checks and
/// reports typed errors — a caller who ignores `insert`'s return value gets a
/// typed error from the pin, not a silently different mask.
///
/// [`insert`]: CpuSet::insert
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct CpuSet {
    words: Vec<u64>,
}

impl CpuSet {
    /// Highest CPU number representable in a hw-kit mask. Matches the
    /// kernel's maximum `CONFIG_NR_CPUS` (8192); beyond this the crate
    /// fails closed instead of pretending.
    pub const MAX_CPUS: u32 = 8192;

    /// An empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A set with exactly one CPU.
    #[must_use]
    pub fn single(cpu: CpuId) -> Self {
        let mut set = Self::new();
        set.insert(cpu);
        set
    }

    /// The inclusive range `a..=b`.
    ///
    /// If `a > b` the set is empty (documented, not guessed); ids at or
    /// beyond [`CpuSet::MAX_CPUS`] are clipped.
    #[must_use]
    pub fn from_range(a: CpuId, b: CpuId) -> Self {
        let mut set = Self::new();
        if a.0 > b.0 {
            return set;
        }
        for raw in a.0..=b.0.min(Self::MAX_CPUS - 1) {
            set.insert(CpuId(raw));
        }
        set
    }

    /// Adds a CPU; returns `false` (without panicking or clamping) when the
    /// id is at or beyond [`CpuSet::MAX_CPUS`].
    pub fn insert(&mut self, cpu: CpuId) -> bool {
        if cpu.0 >= Self::MAX_CPUS {
            return false;
        }
        let bit = cpu.0 as usize;
        let word = bit / 64;
        if word >= self.words.len() {
            self.words.resize(word + 1, 0);
        }
        if let Some(slot) = self.words.get_mut(word) {
            *slot |= 1_u64 << (bit % 64);
        }
        true
    }

    /// Whether the set contains the CPU.
    #[must_use]
    pub fn contains(&self, cpu: CpuId) -> bool {
        let bit = cpu.0 as usize;
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

    /// The number of CPUs in the set.
    #[must_use]
    pub fn len(&self) -> usize {
        self.words.iter().map(|w| w.count_ones() as usize).sum()
    }

    /// The highest member, if any.
    #[must_use]
    pub fn highest(&self) -> Option<CpuId> {
        for (idx, word) in self.words.iter().enumerate().rev() {
            if *word != 0 {
                let bit = 63 - word.leading_zeros();
                return Some(CpuId((idx as u32) * 64 + bit));
            }
        }
        None
    }

    /// Ascending iteration over the members.
    pub fn iter(&self) -> impl Iterator<Item = CpuId> + '_ {
        self.words.iter().enumerate().flat_map(|(idx, word)| {
            (0..64).filter_map(move |bit| {
                if word & (1_u64 << bit) != 0 {
                    Some(CpuId((idx as u32) * 64 + bit))
                } else {
                    None
                }
            })
        })
    }

    /// Whether the two sets share at least one CPU.
    #[must_use]
    pub fn overlaps(&self, other: &CpuSet) -> bool {
        self.words
            .iter()
            .zip(other.words.iter())
            .any(|(a, b)| a & b != 0)
    }

    /// Kernel-style `"0-3,8"` rendering (for logs and `/proc` interop).
    #[must_use]
    pub fn allowed_list(&self) -> String {
        format_ranges(self.iter().map(|c| c.0))
    }

    /// Parses a kernel-style `"0-3,8"` list (the format of `Cpus_allowed_list`
    /// in `/proc/*/status` and of `thread_siblings_list` in sysfs).
    pub fn parse_allowed_list(text: &str) -> Result<Self, CpuSetParseError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(CpuSetParseError::EmptyList);
        }
        let mut set = Self::new();
        for token in text.split(',') {
            let token = token.trim();
            let (lo, hi) = match token.split_once('-') {
                Some((lo, hi)) => (lo, hi),
                None => (token, token),
            };
            let lo = lo
                .parse::<u32>()
                .map_err(|_| CpuSetParseError::InvalidToken(token.to_string()))?;
            let hi = hi
                .parse::<u32>()
                .map_err(|_| CpuSetParseError::InvalidToken(token.to_string()))?;
            if lo > hi {
                return Err(CpuSetParseError::InvalidToken(token.to_string()));
            }
            if hi >= Self::MAX_CPUS {
                return Err(CpuSetParseError::OutOfRange { cpu: hi });
            }
            for raw in lo..=hi {
                set.insert(CpuId(raw));
            }
        }
        Ok(set)
    }

    /// Builds from raw 64-bit words (syscall boundary); trailing zero words
    /// are trimmed so the member-equality invariant holds.
    #[cfg(feature = "libc")]
    pub(crate) fn from_words(mut words: Vec<u64>) -> Self {
        while words.last() == Some(&0) {
            words.pop();
        }
        Self { words }
    }

    /// The backing words (little-endian bitmap) for the FFI boundary.
    #[cfg(feature = "libc")]
    pub(crate) fn raw_words(&self) -> &[u64] {
        &self.words
    }
}

impl FromIterator<CpuId> for CpuSet {
    fn from_iter<I: IntoIterator<Item = CpuId>>(iter: I) -> Self {
        let mut set = Self::new();
        for cpu in iter {
            set.insert(cpu);
        }
        set
    }
}

impl Extend<CpuId> for CpuSet {
    fn extend<I: IntoIterator<Item = CpuId>>(&mut self, iter: I) {
        for cpu in iter {
            self.insert(cpu);
        }
    }
}

impl IntoIterator for CpuSet {
    type Item = CpuId;
    type IntoIter = std::vec::IntoIter<CpuId>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter().collect::<Vec<_>>().into_iter()
    }
}

/// Failure to parse a kernel-style CPU list.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CpuSetParseError {
    /// The list was empty (or whitespace).
    EmptyList,
    /// A comma-separated token was not `N` or `A-B`.
    InvalidToken(String),
    /// A member at or beyond [`CpuSet::MAX_CPUS`] appeared.
    OutOfRange {
        /// The offending id.
        cpu: u32,
    },
}

impl fmt::Display for CpuSetParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CpuSetParseError::EmptyList => write!(f, "empty cpu list"),
            CpuSetParseError::InvalidToken(token) => write!(f, "invalid cpu list token `{token}`"),
            CpuSetParseError::OutOfRange { cpu } => {
                write!(
                    f,
                    "cpu {cpu} exceeds the mask capacity ({})",
                    CpuSet::MAX_CPUS
                )
            }
        }
    }
}

impl std::error::Error for CpuSetParseError {}

/// Renders ascending CPU numbers as a kernel-style `"0-3,8"` list.
pub(crate) fn format_ranges(cpus: impl Iterator<Item = u32>) -> String {
    let mut out = String::new();
    let mut run_start: Option<u32> = None;
    let mut prev: Option<u32> = None;
    for raw in cpus {
        match (run_start, prev) {
            (None, _) => run_start = Some(raw),
            (Some(_), Some(last)) if raw == last + 1 => {}
            (Some(start), _) => {
                push_run(&mut out, start, prev.unwrap_or(start));
                run_start = Some(raw);
            }
        }
        prev = Some(raw);
    }
    if let Some(start) = run_start {
        push_run(&mut out, start, prev.unwrap_or(start));
    }
    out
}

fn push_run(out: &mut String, start: u32, end: u32) {
    if !out.is_empty() {
        out.push(',');
    }
    if start == end {
        out.push_str(&start.to_string());
    } else {
        out.push_str(&format!("{start}-{end}"));
    }
}

/// A small, ordered CPU list: the inline representation covers every
/// shipping SMT sibling set (≤ 8 entries); longer lists spill to the heap.
///
/// Used for [`CpuInfo::siblings`](crate::topology::CpuInfo::siblings) —
/// discovery is a cold path, so the inline fast path is a courtesy, not a
/// performance claim.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SmallCpuList {
    inline: [CpuId; Self::INLINE],
    len: usize,
    spill: Option<Vec<CpuId>>,
}

impl SmallCpuList {
    const INLINE: usize = 8;

    /// An empty list.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inline: [CpuId(0); Self::INLINE],
            len: 0,
            spill: None,
        }
    }

    /// Appends a CPU.
    pub fn push(&mut self, cpu: CpuId) {
        if let Some(spill) = self.spill.as_mut() {
            spill.push(cpu);
            return;
        }
        if let Some(slot) = self.inline.get_mut(self.len) {
            *slot = cpu;
            self.len += 1;
        } else {
            let mut spilled: Vec<CpuId> = self.inline.get(..Self::INLINE).unwrap_or(&[]).to_vec();
            spilled.push(cpu);
            self.spill = Some(spilled);
        }
    }

    /// Number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        match &self.spill {
            Some(spill) => spill.len(),
            None => self.len,
        }
    }

    /// Whether the list has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether the list contains the CPU.
    #[must_use]
    pub fn contains(&self, cpu: CpuId) -> bool {
        self.as_slice().contains(&cpu)
    }

    /// The entries, ascending (insertion order — the parser feeds it sorted).
    #[must_use]
    pub fn as_slice(&self) -> &[CpuId] {
        match &self.spill {
            Some(spill) => spill,
            None => self.inline.get(..self.len).unwrap_or(&[]),
        }
    }

    /// Iterates the entries.
    pub fn iter(&self) -> std::slice::Iter<'_, CpuId> {
        self.as_slice().iter()
    }

    /// Kernel-style `"0-3,8"` rendering.
    #[must_use]
    pub fn allowed_list(&self) -> String {
        format_ranges(self.iter().map(|c| c.0))
    }
}

impl Default for SmallCpuList {
    fn default() -> Self {
        Self::new()
    }
}

impl FromIterator<CpuId> for SmallCpuList {
    fn from_iter<I: IntoIterator<Item = CpuId>>(iter: I) -> Self {
        let mut list = Self::new();
        for cpu in iter {
            list.push(cpu);
        }
        list
    }
}

impl<'a> IntoIterator for &'a SmallCpuList {
    type Item = &'a CpuId;
    type IntoIter = std::slice::Iter<'a, CpuId>;

    fn into_iter(self) -> Self::IntoIter {
        self.as_slice().iter()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn set_membership_and_equality() {
        let mut a = CpuSet::new();
        assert!(a.is_empty());
        assert!(a.insert(CpuId(3)));
        assert!(a.insert(CpuId(0)));
        assert!(a.contains(CpuId(3)));
        assert!(!a.contains(CpuId(1)));
        assert_eq!(a.len(), 2);
        assert_eq!(a.highest(), Some(CpuId(3)));

        let b = CpuSet::from_iter([CpuId(0), CpuId(3)]);
        assert_eq!(a, b, "word layout must be normalized");
    }

    #[test]
    fn insert_rejects_beyond_capacity_fail_closed() {
        let mut set = CpuSet::new();
        assert!(!set.insert(CpuId(CpuSet::MAX_CPUS)));
        assert!(!set.insert(CpuId(u32::MAX)));
        assert!(set.is_empty());
    }

    #[test]
    fn range_is_inclusive_and_documented() {
        let set = CpuSet::from_range(CpuId(2), CpuId(5));
        assert_eq!(set.len(), 4);
        assert!(set.contains(CpuId(2)) && set.contains(CpuId(5)));

        assert!(
            CpuSet::from_range(CpuId(5), CpuId(2)).is_empty(),
            "a > b is empty"
        );
    }

    #[test]
    fn range_clips_at_capacity() {
        let set = CpuSet::from_range(CpuId(CpuSet::MAX_CPUS - 2), CpuId(u32::MAX));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn allowed_list_formatting() {
        let set = CpuSet::from_iter([CpuId(8), CpuId(0), CpuId(1), CpuId(2), CpuId(3)]);
        assert_eq!(set.allowed_list(), "0-3,8");
        assert_eq!(CpuSet::single(CpuId(7)).allowed_list(), "7");
    }

    #[test]
    fn allowed_list_parse_round_trip() {
        let set = CpuSet::parse_allowed_list("0-3,8,10-11").unwrap();
        assert_eq!(set.allowed_list(), "0-3,8,10-11");
        assert_eq!(set.len(), 7);
    }

    #[test]
    fn parse_rejects_malformed_fail_closed() {
        assert_eq!(
            CpuSet::parse_allowed_list(""),
            Err(CpuSetParseError::EmptyList)
        );
        assert_eq!(
            CpuSet::parse_allowed_list("   "),
            Err(CpuSetParseError::EmptyList)
        );
        assert_eq!(
            CpuSet::parse_allowed_list("0,x"),
            Err(CpuSetParseError::InvalidToken("x".to_string()))
        );
        assert_eq!(
            CpuSet::parse_allowed_list("5-2"),
            Err(CpuSetParseError::InvalidToken("5-2".to_string()))
        );
        assert_eq!(
            CpuSet::parse_allowed_list(&format!("{}", CpuSet::MAX_CPUS)),
            Err(CpuSetParseError::OutOfRange {
                cpu: CpuSet::MAX_CPUS
            })
        );
    }

    #[test]
    fn overlap_detection() {
        let a = CpuSet::from_range(CpuId(0), CpuId(3));
        let b = CpuSet::from_range(CpuId(3), CpuId(7));
        let c = CpuSet::from_range(CpuId(4), CpuId(7));
        assert!(a.overlaps(&b));
        assert!(b.overlaps(&a));
        assert!(!a.overlaps(&c));
    }

    #[test]
    fn core_id_validation_is_typed() {
        assert_eq!(CoreId::new(0).unwrap().get(), 0);
        assert_eq!(CoreId::new(CoreId::MAX).unwrap().get(), CoreId::MAX);
        let err = CoreId::new(u16::MAX).unwrap_err();
        assert!(matches!(err, crate::error::HwError::UnsupportedTarget(_)));
    }

    #[test]
    fn small_list_stays_inline_then_spills() {
        let mut list = SmallCpuList::new();
        for i in 0..12_u32 {
            list.push(CpuId(i));
        }
        assert_eq!(list.len(), 12);
        for (idx, cpu) in list.iter().enumerate() {
            assert_eq!(cpu.0, idx as u32);
        }
        assert!(list.contains(CpuId(11)));
        assert!(!list.contains(CpuId(12)));
        assert_eq!(list.allowed_list(), "0-11");
    }

    #[test]
    fn small_list_short_form_is_exact() {
        let list = SmallCpuList::from_iter([CpuId(2), CpuId(0)]);
        assert_eq!(list.as_slice(), &[CpuId(2), CpuId(0)]);
        assert_eq!(list.allowed_list(), "2,0");
    }

    #[test]
    fn set_iteration_is_ascending() {
        let set = CpuSet::from_iter([CpuId(70), CpuId(2), CpuId(64), CpuId(1)]);
        let ids: Vec<u32> = set.iter().map(|c| c.0).collect();
        assert_eq!(ids, vec![1, 2, 64, 70]);
    }
}
