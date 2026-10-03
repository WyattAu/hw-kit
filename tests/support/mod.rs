//! Shared fixture-path helpers for hw-kit's test targets.
//!
//! Committed fixture trees keep discovery and scanning hermetic: tests
//! never parse the live host's `/sys` or `/proc` for correctness asserts.

#![allow(dead_code)] // each test target uses a subset

use std::path::PathBuf;

/// Root of the crate.
pub fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// A committed sysfs fixture tree root (`tests/fixtures/sysfs/<name>/sys`).
pub fn sysfs(name: &str) -> PathBuf {
    crate_root()
        .join("tests/fixtures/sysfs")
        .join(name)
        .join("sys")
}

/// A committed `/proc` fixture tree root (`tests/fixtures/proc/<name>`).
pub fn proc(name: &str) -> PathBuf {
    crate_root().join("tests/fixtures/proc").join(name)
}
