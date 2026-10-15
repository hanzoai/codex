//! The version this process runs as: one value, set by the binary, read everywhere.
//!
//! Every crate here asks `get()` where upstream compiled in its own
//! `env!("CARGO_PKG_VERSION")`, and the binary (hanzo-dev) hands its version to
//! `set` before anything reads it. So the release's number lives in the binary
//! crate alone: a new release recompiles that crate and nothing beneath it.
//! Unset (a library test), it is this workspace's version.

use std::fmt;
use std::ops::Deref;
use std::sync::OnceLock;

static RELEASE: OnceLock<&'static str> = OnceLock::new();

/// Fix the version for this process. The first call wins.
pub fn set(version: &'static str) {
    let _ = RELEASE.set(version);
}

/// The version this process runs as: the one the binary set, or the tree's own
/// (a local build, a library test).
pub fn get() -> &'static str {
    RELEASE.get().copied().unwrap_or(env!("CARGO_PKG_VERSION"))
}

/// The version where code names it as a constant: it derefs to [`get`] and
/// displays as it, so a `&str` parameter takes [`VERSION`] unchanged.
#[derive(Debug)]
pub struct Version;

pub const VERSION: &Version = &Version;

impl Deref for Version {
    type Target = str;

    fn deref(&self) -> &str {
        get()
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn takes(version: &str) -> &str {
        version
    }

    /// Unset, the version is the tree's; set, it is the release, through every
    /// way a caller reads it; and the first value set is the one that stays.
    #[test]
    fn one_value_through_every_reader() {
        assert_eq!(get(), env!("CARGO_PKG_VERSION"));
        set("9.9.9");
        set("1.1.1");
        assert_eq!(get(), "9.9.9");
        assert_eq!(takes(VERSION), "9.9.9");
        assert_eq!(format!("{VERSION}"), "9.9.9");
        assert_eq!(VERSION.to_string(), "9.9.9");
    }
}
