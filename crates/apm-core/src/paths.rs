//! Volume-relative paths and the integer identifiers shared by every layer.
//!
//! Ids are plain `i64` aliases because they are integers on the wire.
//!
//! [`RelPath`] is the only way the code base refers to a location inside a volume. It is always
//! normalised: `/`-separated, no leading or trailing slash, no empty, `.` or `..` components, no NUL.
//! The empty string is the volume root. Because of that, a validated value can be joined onto a
//! volume root without any further escaping checks (symlinks are the job of the server's
//! `VolumeRoot`, which resolves a `RelPath` against a volume).
//!
//! Names that are not valid UTF-8 cannot be represented, so the scanner skips such entries.
//! Bytes are preserved as given: no Unicode normalisation or case folding happens here
//! (use [`crate::names::normalize_name`] for comparisons).
//!
//! Request DTOs should use `RelPath` directly (with `#[ts(type = "string")]`), so malformed paths are
//! rejected while the JSON is deserialised.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Primary key of an indexed file.
pub type FileId = i64;

/// Primary key of a materialized plan.
pub type PlanId = i64;

/// Primary key of a server-side selection set.
pub type SelectionId = i64;

/// Primary key of a background task.
pub type TaskId = i64;

/// Primary key of an item in the trash.
pub type TrashItemId = i64;

/// Primary key of a mounted volume.
pub type VolumeId = i64;

/// Why a string is not a valid [`RelPath`] or path component.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    #[error("path must be relative to the volume root")]
    Absolute,
    #[error("path must not contain `..` components")]
    ParentTraversal,
    #[error("path must not contain NUL bytes")]
    Nul,
    #[error("`{0}` is not a valid file or folder name")]
    InvalidComponent(String),
}

/// A validated, normalised path inside a volume. See the module docs for the invariants.
///
/// Deserialising goes through [`RelPath::new`], so a request carrying `../x` or `/x` is rejected.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RelPath(String);

impl RelPath {
    /// The volume root (empty path).
    pub fn root() -> Self {
        Self(String::new())
    }

    /// Parses and normalises `raw`. Repeated and trailing slashes and `.` components are dropped;
    /// absolute paths, `..` components and NUL bytes are rejected.
    pub fn new(raw: &str) -> Result<Self, PathError> {
        if raw.contains('\0') {
            return Err(PathError::Nul);
        }
        if raw.starts_with('/') {
            return Err(PathError::Absolute);
        }
        let mut out = String::with_capacity(raw.len());
        for component in raw.split('/') {
            match component {
                "" | "." => {}
                ".." => return Err(PathError::ParentTraversal),
                other => {
                    if !out.is_empty() {
                        out.push('/');
                    }
                    out.push_str(other);
                }
            }
        }
        Ok(Self(out))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_root(&self) -> bool {
        self.0.is_empty()
    }

    /// Appends one file or folder name (no slashes, not empty, not `.` or `..`).
    pub fn join(&self, name: &str) -> Result<Self, PathError> {
        if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\0']) {
            return Err(PathError::InvalidComponent(name.to_owned()));
        }
        Ok(if self.is_root() {
            Self(name.to_owned())
        } else {
            Self(format!("{}/{name}", self.0))
        })
    }

    /// Appends another relative path. Infallible because both sides are already valid.
    pub fn join_rel(&self, other: &RelPath) -> Self {
        match (self.is_root(), other.is_root()) {
            (_, true) => self.clone(),
            (true, false) => other.clone(),
            (false, false) => Self(format!("{}/{}", self.0, other.0)),
        }
    }

    /// The containing folder; `None` for the root.
    pub fn parent(&self) -> Option<Self> {
        if self.is_root() {
            return None;
        }
        Some(match self.0.rfind('/') {
            Some(i) => Self(self.0[..i].to_owned()),
            None => Self::root(),
        })
    }

    /// Parents from the nearest one up to and including the root; empty for the root itself.
    pub fn ancestors(&self) -> impl Iterator<Item = RelPath> {
        std::iter::successors(self.parent(), |p| p.parent())
    }

    /// The last component; `None` for the root.
    pub fn file_name(&self) -> Option<&str> {
        if self.is_root() {
            None
        } else {
            self.0.rsplit('/').next()
        }
    }

    pub fn components(&self) -> impl Iterator<Item = &str> {
        self.0.split('/').filter(|c| !c.is_empty())
    }

    /// Number of components (0 for the root).
    pub fn depth(&self) -> usize {
        self.components().count()
    }

    /// Component-wise prefix test: `a/b` starts with `a` and with itself, `ab` does not start with `a`.
    /// Every path starts with the root.
    pub fn starts_with(&self, base: &RelPath) -> bool {
        base.is_root()
            || self.0 == base.0
            || (self.0.starts_with(&base.0) && self.0.as_bytes().get(base.0.len()) == Some(&b'/'))
    }

    /// The path relative to `base`; `None` if `self` is not inside (or equal to) `base`.
    pub fn strip_prefix(&self, base: &RelPath) -> Option<Self> {
        if !self.starts_with(base) {
            return None;
        }
        if base.is_root() {
            return Some(self.clone());
        }
        Some(Self(
            self.0[base.0.len()..].trim_start_matches('/').to_owned(),
        ))
    }
}

impl fmt::Display for RelPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for RelPath {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl FromStr for RelPath {
    type Err = PathError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl TryFrom<String> for RelPath {
    type Error = PathError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(&value)
    }
}

impl TryFrom<&str> for RelPath {
    type Error = PathError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<RelPath> for String {
    fn from(value: RelPath) -> Self {
        value.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn rel(s: &str) -> RelPath {
        RelPath::new(s).expect("valid path")
    }

    #[test]
    fn normalises_slashes_and_dots() {
        assert_eq!(rel("a//b/./c/").as_str(), "a/b/c");
        assert_eq!(rel("./a").as_str(), "a");
        assert!(rel("").is_root());
        assert!(rel("./").is_root());
        assert_eq!(rel("a b/é.jpg").as_str(), "a b/é.jpg");
    }

    #[test]
    fn rejects_escapes() {
        assert_eq!(RelPath::new("/etc/passwd"), Err(PathError::Absolute));
        assert_eq!(RelPath::new("/"), Err(PathError::Absolute));
        assert_eq!(RelPath::new("a/../b"), Err(PathError::ParentTraversal));
        assert_eq!(RelPath::new(".."), Err(PathError::ParentTraversal));
        assert_eq!(RelPath::new("a/.."), Err(PathError::ParentTraversal));
        assert_eq!(RelPath::new("a\0b"), Err(PathError::Nul));
    }

    #[test]
    fn dots_inside_names_are_fine() {
        assert_eq!(rel("a/..b/c..").as_str(), "a/..b/c..");
        assert_eq!(rel(".hidden/.x").as_str(), ".hidden/.x");
    }

    #[test]
    fn join_validates_a_single_component() {
        let base = rel("a");
        assert_eq!(base.join("b").unwrap().as_str(), "a/b");
        assert_eq!(RelPath::root().join("b").unwrap().as_str(), "b");
        for bad in ["", ".", "..", "x/y", "a\0"] {
            assert!(
                matches!(base.join(bad), Err(PathError::InvalidComponent(_))),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn join_rel_handles_roots() {
        assert_eq!(rel("a").join_rel(&rel("b/c")).as_str(), "a/b/c");
        assert_eq!(RelPath::root().join_rel(&rel("b")).as_str(), "b");
        assert_eq!(rel("a").join_rel(&RelPath::root()).as_str(), "a");
        assert!(RelPath::root().join_rel(&RelPath::root()).is_root());
    }

    #[test]
    fn parent_name_and_ancestors() {
        let p = rel("a/b/c.jpg");
        assert_eq!(p.file_name(), Some("c.jpg"));
        assert_eq!(p.parent(), Some(rel("a/b")));
        assert_eq!(rel("a").parent(), Some(RelPath::root()));
        assert_eq!(RelPath::root().parent(), None);
        assert_eq!(RelPath::root().file_name(), None);
        let all: Vec<_> = p.ancestors().map(|a| a.to_string()).collect();
        assert_eq!(all, ["a/b", "a", ""]);
        assert_eq!(RelPath::root().ancestors().count(), 0);
        assert_eq!(p.depth(), 3);
        assert_eq!(RelPath::root().depth(), 0);
        assert_eq!(p.components().collect::<Vec<_>>(), ["a", "b", "c.jpg"]);
    }

    #[test]
    fn prefix_is_component_wise() {
        assert!(rel("a/b").starts_with(&rel("a")));
        assert!(rel("a/b").starts_with(&rel("a/b")));
        assert!(rel("a/b").starts_with(&RelPath::root()));
        assert!(!rel("ab").starts_with(&rel("a")));
        assert!(!rel("a").starts_with(&rel("a/b")));
        assert_eq!(rel("a/b/c").strip_prefix(&rel("a")), Some(rel("b/c")));
        assert_eq!(rel("a/b").strip_prefix(&rel("a/b")), Some(RelPath::root()));
        assert_eq!(rel("a/b").strip_prefix(&RelPath::root()), Some(rel("a/b")));
        assert_eq!(rel("ab/c").strip_prefix(&rel("a")), None);
    }

    #[test]
    fn ordering_groups_a_folder_with_its_children() {
        let mut v = [rel("b"), rel("a/z"), rel("a"), rel("a/b")];
        v.sort();
        let got: Vec<_> = v.iter().map(RelPath::as_str).collect();
        assert_eq!(got, ["a", "a/b", "a/z", "b"]);
    }

    #[test]
    fn serde_validates_on_deserialise() {
        let p: RelPath = serde_json::from_str("\"a//b/\"").unwrap();
        assert_eq!(p.as_str(), "a/b");
        assert_eq!(serde_json::to_string(&p).unwrap(), "\"a/b\"");
        assert!(serde_json::from_str::<RelPath>("\"../x\"").is_err());
        assert!(serde_json::from_str::<RelPath>("\"/x\"").is_err());
    }

    proptest! {
        #[test]
        fn new_never_panics_and_results_are_canonical(raw in "\\PC{0,40}") {
            if let Ok(p) = RelPath::new(&raw) {
                let s = p.as_str();
                prop_assert!(!s.starts_with('/') && !s.ends_with('/'));
                prop_assert!(!s.contains("//") && !s.contains('\0'));
                prop_assert!(p.components().all(|c| c != "." && c != ".." && !c.is_empty()));
                prop_assert_eq!(RelPath::new(s).unwrap(), p.clone());
                prop_assert!(p.starts_with(&RelPath::root()));
                if let Some(parent) = p.parent() {
                    prop_assert!(p.starts_with(&parent));
                    prop_assert_eq!(parent.join(p.file_name().unwrap()).unwrap(), p.clone());
                }
            }
        }

        #[test]
        fn strip_prefix_roundtrips(a in "[a-c]{1,2}(/[a-c]{1,2}){0,3}", b in "[a-c]{1,2}(/[a-c]{1,2}){0,3}") {
            let (a, b) = (rel(&a), rel(&b));
            let joined = a.join_rel(&b);
            prop_assert_eq!(joined.strip_prefix(&a), Some(b.clone()));
            prop_assert!(joined.starts_with(&a));
        }
    }
}
