//! Hugepage reservation via `mmap(MAP_HUGETLB)` (REQ-HW-004, feature
//! `hugepage`).
//!
//! [`hugepage_reserve`] maps huge-page-backed memory with an explicit size
//! class ([`HugeSize::Default2M`] / [`HugeSize::Default1G`]). **There is no
//! silent fallback to 4 KiB pages**: a kernel without the requested pool —
//! or with an exhausted one — is a typed [`HugetlbError`], because a silent
//! fallback would corrupt the latency assumptions of every consumer
//! downstream (spec §Semantics).
//!
//! Size classes are verified against `/sys/kernel/mm/hugepages` *before*
//! the `mmap`: a missing class directory is
//! [`HugetlbError::UnsupportedSize`], a pool with zero reserved pages is
//! [`HugetlbError::Unavailable`] — both without even attempting the map.
//! When the probe cannot run (no `/sys/kernel/mm/hugepages` at all) the
//! `mmap` decides, and its `ENOMEM`/`ENOSYS`/`EINVAL` land in
//! [`HugetlbError::Unavailable`] all the same.
//!
//! `MAP_POPULATE` is always passed: a "reserved" region that could still
//! `SIGBUS` on first touch would be a landmine, not a reservation.

use std::ops::{Deref, DerefMut};
use std::slice;

use crate::error::HugetlbError;

/// linux uapi `MAP_HUGETLB` (`include/uapi/asm-generic/mman-common.h`).
const MAP_HUGETLB: i32 = 0x040_000;
/// linux uapi `MAP_HUGE_SHIFT` — the size-class bits of the mmap flags.
const MAP_HUGE_SHIFT: i32 = 26;

/// Hugepage size classes.
///
/// `"Default"` names the kernel's default huge-page size classes; the mmap
/// flag is always explicit (the class bit is set either way) so behavior
/// does not depend on which pool the admin happened to make the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HugeSize {
    /// The 2 MiB class (`hugepages-2048kB`, `MAP_HUGE_2MB`).
    Default2M,
    /// The 1 GiB class (`hugepages-1048576kB`, `MAP_HUGE_1GB`).
    Default1G,
}

impl HugeSize {
    /// Human-readable class name used in typed errors.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            HugeSize::Default2M => "2MiB",
            HugeSize::Default1G => "1GiB",
        }
    }

    /// The size class's page size in KiB (the sysfs directory suffix).
    #[must_use]
    pub fn size_kib(self) -> u64 {
        match self {
            HugeSize::Default2M => 2048,
            HugeSize::Default1G => 1048576,
        }
    }

    fn mmap_flag(self) -> i32 {
        // uapi: MAP_HUGE_2MB = 21 << MAP_HUGE_SHIFT, MAP_HUGE_1GB =
        // 30 << MAP_HUGE_SHIFT (the field is log2(page size)).
        let log2 = match self {
            HugeSize::Default2M => 21,
            HugeSize::Default1G => 30,
        };
        log2 << MAP_HUGE_SHIFT
    }
}

/// `hugepages-<N>kB` → `N`; anything else is `None` (other entries —
/// `surplus_hugepages`, `nr_overcommit_hugepages*` — are not size classes).
fn parse_hugepage_dir_name(name: &str) -> Option<u64> {
    let raw = name.strip_prefix("hugepages-")?.strip_suffix("kB")?;
    raw.parse::<u64>().ok()
}

/// The pool's reserved page count for `size_class`, when the sysfs tree
/// says anything at all. `Ok(None)` = cannot know (let the mmap decide).
fn probe_pool(class: HugeSize) -> Result<Option<u64>, ()> {
    let dir = std::path::Path::new("/sys/kernel/mm/hugepages");
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Ok(None), // no hugepage sysfs at all: mmap decides
    };
    let mut found = false;
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().into_owned();
        if parse_hugepage_dir_name(&name) == Some(class.size_kib()) {
            found = true;
            break;
        }
    }
    if !found {
        // The hugepages tree exists but does not carry this class: that is
        // "kernel has no such pool", distinct from "pool exhausted".
        return Err(());
    }
    let nr_path = dir
        .join(format!("hugepages-{}kB", class.size_kib()))
        .join("nr_hugepages");
    match std::fs::read_to_string(&nr_path) {
        Ok(text) => match text.trim().parse::<u64>() {
            Ok(0) => Ok(Some(0)), // pool exists, zero pages: exhaustion up front
            Ok(_) => Ok(Some(1)),
            Err(_) => Ok(None), // unreadable count: let the mmap decide
        },
        Err(_) => Ok(None),
    }
}

/// Reserves `len` bytes of huge-page-backed memory with size class
/// `size_class` and returns an owning [`HugeRegion`] (drop = `munmap`).
///
/// `len` must be a non-zero multiple of the class's page size; anything
/// else is a typed error ([`HugetlbError::ZeroLength`],
/// [`HugetlbError::UnalignedLength`]) — the kernel would silently round the
/// mapping up, and hw-kit does not adopt silently-rounded contracts.
///
/// # Examples
///
/// ```no_run
/// use hw_kit::hugepage::{hugepage_reserve, HugeSize};
///
/// match hugepage_reserve(2 * 1024 * 1024, HugeSize::Default2M) {
///     Ok(region) => println!("{} huge-backed bytes", region.len()),
///     // Exhaustion is typed — never a silent 4 KiB fallback.
///     Err(e) => eprintln!("hugepages unavailable: {e}"),
/// }
/// ```
pub fn hugepage_reserve(len: usize, size_class: HugeSize) -> Result<HugeRegion, HugetlbError> {
    if len == 0 {
        return Err(HugetlbError::ZeroLength);
    }
    let page = size_class.size_kib() * 1024;
    if len as u64 % page != 0 {
        return Err(HugetlbError::UnalignedLength { len, page });
    }
    match probe_pool(size_class) {
        Ok(Some(0)) => {
            return Err(HugetlbError::Unavailable {
                size_class: size_class.name(),
                source: None,
            })
        }
        Err(()) => {
            return Err(HugetlbError::UnsupportedSize {
                size_class: size_class.name(),
            })
        }
        Ok(Some(_) | None) => {}
    }
    if len as u64 % page != 0 {
        return Err(HugetlbError::UnalignedLength { len, page });
    }

    let flags = libc::MAP_PRIVATE
        | libc::MAP_ANONYMOUS
        | MAP_HUGETLB
        | size_class.mmap_flag()
        | libc::MAP_POPULATE;
    // SAFETY: mmap of anonymous private hugetlb memory with fd -1 / offset
    // 0; a failure is MAP_FAILED with nothing mapped. With MAP_POPULATE the
    // kernel faults every page up front, so a successful return means the
    // full reservation is resident (a short pool fails here, typed — it can
    // never surface later as a SIGBUS on first touch).
    let raw = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            flags,
            -1,
            0,
        )
    };
    if raw == libc::MAP_FAILED {
        let source = std::io::Error::last_os_error();
        return Err(match source.raw_os_error() {
            Some(libc::ENOMEM | libc::ENOSPC | libc::EPERM | libc::ENOSYS | libc::EINVAL) => {
                HugetlbError::Unavailable {
                    size_class: size_class.name(),
                    source: Some(source),
                }
            }
            _ => HugetlbError::Syscall {
                call: "mmap",
                source,
            },
        });
    }
    Ok(HugeRegion {
        ptr: raw.cast::<u8>(),
        len,
    })
}

/// An owning huge-page-backed region (drop = `munmap`).
#[derive(Debug)]
pub struct HugeRegion {
    ptr: *mut u8,
    len: usize,
}

impl HugeRegion {
    /// The region length in bytes (as requested; the mapping is backed by
    /// whole huge pages).
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the region is empty (never true for a constructed region).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Base pointer.
    #[must_use]
    pub fn as_ptr(&self) -> *const u8 {
        self.ptr
    }

    /// Mutable base pointer.
    pub fn as_mut_ptr(&mut self) -> *mut u8 {
        self.ptr
    }
}

impl Deref for HugeRegion {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        // SAFETY: `ptr` is a live mapping of exactly `len` bytes for the
        // region's whole lifetime (drop is the only munmap), and the
        // returned borrow cannot outlive `&self`.
        unsafe { slice::from_raw_parts(self.ptr, self.len) }
    }
}

impl DerefMut for HugeRegion {
    fn deref_mut(&mut self) -> &mut [u8] {
        // SAFETY: as above, with exclusive access guaranteed by `&mut self`.
        unsafe { slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for HugeRegion {
    fn drop(&mut self) {
        // SAFETY: `ptr` came from `mmap` (length `len`) and is unmapped
        // exactly once, here, after which the region is gone.
        unsafe { libc::munmap(self.ptr.cast(), self.len) };
    }
}

// SAFETY: a `HugeRegion` owns one process-private anonymous mapping with
// no shared state — moving it across threads moves ownership of the
// mapping, exactly as moving a `Vec<u8>` would.
unsafe impl Send for HugeRegion {}
// SAFETY: ordinary byte access is the only operation on the mapping; there
// is no shared mutable state to synchronize (see `Send` above).
unsafe impl Sync for HugeRegion {}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn dir_names_parse_to_kib_sizes() {
        assert_eq!(parse_hugepage_dir_name("hugepages-2048kB"), Some(2048));
        assert_eq!(
            parse_hugepage_dir_name("hugepages-1048576kB"),
            Some(1048576)
        );
        assert_eq!(parse_hugepage_dir_name("nr_hugepages"), None);
        assert_eq!(parse_hugepage_dir_name("hugepages-garbage"), None);
        assert_eq!(parse_hugepage_dir_name("hugepages-2048KB"), None);
    }

    #[test]
    fn size_classes_report_name_and_size() {
        assert_eq!(HugeSize::Default2M.name(), "2MiB");
        assert_eq!(HugeSize::Default1G.size_kib(), 1048576);
    }

    #[test]
    fn mmap_flags_are_explicit_per_class() {
        // uapi encoding: log2(page size) << MAP_HUGE_SHIFT.
        assert_eq!(HugeSize::Default2M.mmap_flag(), 21 << MAP_HUGE_SHIFT);
        assert_eq!(HugeSize::Default1G.mmap_flag(), 30 << MAP_HUGE_SHIFT);
    }

    /// Exercises the `HugeRegion` RAII slice-view surface over a plain
    /// anonymous mapping — the owning-region logic is independent of the
    /// backing page size (the huge-backed success path is covered by the
    /// env-gated integration test).
    #[test]
    fn huge_region_slice_view_round_trip() {
        const LEN: usize = 2 * 4096;
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
        let mut region = HugeRegion {
            ptr: raw.cast::<u8>(),
            len: LEN,
        };

        assert_eq!(region.len(), LEN);
        assert!(!region.is_empty());
        assert_eq!(region.as_ptr(), raw.cast::<u8>());

        region[0] = 0x42;
        assert_eq!(region.deref()[0], 0x42);
        region.deref_mut()[LEN - 1] = 0x24;
        assert_eq!(region[LEN - 1], 0x24);

        drop(region); // drop = munmap, exactly once
    }
}
