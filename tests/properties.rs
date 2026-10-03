//! Property tests (proptest): the pure-`std` validation core is total over
//! arbitrary inputs — fail-closed typing, never panics, never clamps.

#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(clippy::panic)] // proptest's failure protocol panics by design

use hw_kit::cpu::{CoreId, CpuId, CpuSet};
use hw_kit::HwError;
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1024))]

    /// CoreId validation is exact over arbitrary u16s: accepted exactly when
    /// the id is below the crate's mask capacity, rejected with the typed
    /// umbrella error otherwise — never clamped, never guessed.
    #[test]
    fn core_id_validation_is_exact_over_arbitrary_u16s(raw in any::<u16>()) {
        match CoreId::new(raw) {
            Ok(core) => {
                prop_assert!(u32::from(raw) < CpuSet::MAX_CPUS);
                prop_assert_eq!(core.get(), raw);
                prop_assert_eq!(u32::from(raw), CpuId::from(core).get());
            }
            Err(HwError::UnsupportedTarget(_)) => {
                prop_assert!(u32::from(raw) >= CpuSet::MAX_CPUS);
            }
            Err(other) => prop_assert!(false, "wrong error class for {raw}: {other}"),
        }
    }

    /// `allowed_list` round-trips: any set built from arbitrary small ids
    /// formats to the kernel list syntax and reparses to an equal set.
    /// (The empty set formats to the empty list, which the parser rejects —
    /// "no such thing as an empty allowed list" is the kernel's own rule.)
    #[test]
    fn allowed_list_round_trip(
        raws in prop::collection::vec(0..CpuSet::MAX_CPUS as u16, 0..64),
    ) {
        let set: CpuSet = raws.iter().map(|r| hw_kit::CpuId(u32::from(*r))).collect();
        let text = set.allowed_list();
        if set.is_empty() {
            prop_assert_eq!(text, "");
            return Ok(());
        }
        let reparsed = CpuSet::parse_allowed_list(&text)
            .unwrap_or_else(|e| panic!("own output must parse: {text} ({e})"));
        prop_assert_eq!(set.len(), reparsed.len());
        prop_assert_eq!(&set, &reparsed);
    }

    /// Ranges are exact: `from_range(lo, hi)` contains precisely `lo..=hi`
    /// (clipped at capacity); a reversed range is the documented empty set.
    #[test]
    fn ranges_are_exact(a in 0..CpuSet::MAX_CPUS as u16, b in 0..CpuSet::MAX_CPUS as u16) {
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        let set = CpuSet::from_range(hw_kit::CpuId(u32::from(lo)), hw_kit::CpuId(u32::from(hi)));
        prop_assert_eq!(set.len() as u64, u64::from(hi) - u64::from(lo) + 1);
        for r in lo..=hi {
            prop_assert!(set.contains(hw_kit::CpuId(u32::from(r))));
        }
        if lo > 0 {
            prop_assert!(!set.contains(hw_kit::CpuId(u32::from(lo - 1))));
        }
        let reversed = CpuSet::from_range(hw_kit::CpuId(u32::from(hi)), hw_kit::CpuId(u32::from(lo)));
        if a == b {
            prop_assert_eq!(reversed, set);
        } else {
            // Documented: first arg greater than second ⇒ empty, never a wrap.
            prop_assert!(reversed.is_empty());
        }
    }

    /// Overlap is symmetric and precise over arbitrary id pairs.
    #[test]
    fn overlap_is_symmetric(
        a in prop::collection::vec(0..1024_u16, 0..16),
        b in prop::collection::vec(0..1024_u16, 0..16),
    ) {
        let sa: CpuSet = a.iter().map(|r| hw_kit::CpuId(u32::from(*r))).collect();
        let sb: CpuSet = b.iter().map(|r| hw_kit::CpuId(u32::from(*r))).collect();
        prop_assert_eq!(sa.overlaps(&sb), sb.overlaps(&sa));
        prop_assert_eq!(sa.overlaps(&sb), a.iter().any(|x| b.contains(x)));
    }

    /// SmallCpuList keeps insertion order and reports membership exactly,
    /// across the inline→spill boundary (arbitrary lengths).
    #[test]
    fn small_cpu_list_is_exact(
        raws in prop::collection::vec(0..4096_u16, 0..40),
    ) {
        let list: hw_kit::SmallCpuList = raws.iter().map(|r| hw_kit::CpuId(u32::from(*r))).collect();
        prop_assert_eq!(list.len(), raws.len());
        for (slice_at, raw) in list.iter().zip(raws.iter()) {
            prop_assert_eq!(slice_at.get(), u32::from(*raw));
        }
        if let Some(last) = raws.last() {
            prop_assert!(list.contains(hw_kit::CpuId(u32::from(*last))));
        }
    }
}
