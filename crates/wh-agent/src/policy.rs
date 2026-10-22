//! What a session's agent may read, write and do.
//!
//! Every `fs/read_text_file`, `fs/write_text_file` and
//! `session/request_permission` the agent sends is routed to the
//! [`SessionPolicy`] the session was created with. The client never touches
//! the file system on its own.

use std::collections::BTreeSet;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::acp;

#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("access denied: {0}")]
    Denied(String),
    #[error("file not found: {}", .0.display())]
    NotFound(PathBuf),
    #[error("{0}")]
    Io(io::Error),
}

impl From<io::Error> for PolicyError {
    fn from(err: io::Error) -> Self {
        PolicyError::Io(err)
    }
}

/// A `session/request_permission` from the agent, flattened.
#[derive(Debug, Clone, PartialEq)]
pub struct PermissionRequest {
    pub tool_call_id: String,
    pub title: Option<String>,
    pub kind: Option<acp::ToolKind>,
    /// The tool's raw input as the agent reported it (e.g. a Bash command).
    pub raw_input: Option<serde_json::Value>,
    /// Files the tool call says it touches.
    pub locations: Vec<PathBuf>,
    pub options: Vec<PermissionChoice>,
}

/// One of the options the agent offers in a permission request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionChoice {
    pub id: String,
    pub name: String,
    pub kind: acp::PermissionOptionKind,
}

impl PermissionRequest {
    pub(crate) fn from_acp(request: &acp::RequestPermissionRequest) -> Self {
        let fields = &request.tool_call.fields;
        PermissionRequest {
            tool_call_id: request.tool_call.tool_call_id.0.to_string(),
            title: fields.title.clone(),
            kind: fields.kind,
            raw_input: fields.raw_input.clone(),
            locations: fields
                .locations
                .iter()
                .flatten()
                .map(|location| location.path.clone())
                .collect(),
            options: request
                .options
                .iter()
                .map(|option| PermissionChoice {
                    id: option.option_id.0.to_string(),
                    name: option.name.clone(),
                    kind: option.kind,
                })
                .collect(),
        }
    }

    /// Every file this request names: its locations plus the usual path keys
    /// of the raw input (`file_path`, `path`, `notebook_path`).
    pub fn paths(&self) -> Vec<PathBuf> {
        let mut paths = self.locations.clone();
        if let Some(input) = self.raw_input.as_ref().and_then(|v| v.as_object()) {
            for key in ["file_path", "path", "notebook_path"] {
                if let Some(path) = input.get(key).and_then(|v| v.as_str()) {
                    paths.push(PathBuf::from(path));
                }
            }
        }
        paths
    }
}

/// A policy's answer to a [`PermissionRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionDecision {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
    /// Pick this option id verbatim.
    Select(String),
    /// Answer with the `cancelled` outcome.
    Cancel,
}

impl PermissionDecision {
    /// The option to answer with, or `None` for the `cancelled` outcome.
    ///
    /// A kind the agent did not offer falls back to another option of the
    /// same polarity (`AllowAlways` → `AllowOnce`, `RejectOnce` →
    /// `RejectAlways`); with no rejecting option at all a rejection becomes
    /// `cancelled`, which agents must treat as "not allowed". An allow never
    /// falls back to a reject or vice versa.
    pub fn resolve<'a>(&self, options: &'a [PermissionChoice]) -> Option<&'a PermissionChoice> {
        use acp::PermissionOptionKind as K;
        let by_kind = |kinds: &[K]| {
            kinds
                .iter()
                .find_map(|kind| options.iter().find(|o| o.kind == *kind))
        };
        match self {
            PermissionDecision::AllowOnce => by_kind(&[K::AllowOnce, K::AllowAlways]),
            PermissionDecision::AllowAlways => by_kind(&[K::AllowAlways, K::AllowOnce]),
            PermissionDecision::RejectOnce => by_kind(&[K::RejectOnce, K::RejectAlways]),
            PermissionDecision::RejectAlways => by_kind(&[K::RejectAlways, K::RejectOnce]),
            PermissionDecision::Select(id) => options.iter().find(|o| &o.id == id),
            PermissionDecision::Cancel => None,
        }
    }
}

/// Decides what the agent of one session may do. Called on the connection's
/// dispatch thread: keep implementations quick and never block on the
/// session's own events.
pub trait SessionPolicy: Send + Sync {
    /// The full text of `path` (the client applies `line`/`limit` itself).
    fn read_text_file(&self, path: &Path) -> Result<String, PolicyError>;
    fn write_text_file(&self, path: &Path, content: &str) -> Result<(), PolicyError>;
    fn decide_permission(&self, request: &PermissionRequest) -> PermissionDecision;
}

/// The compiler role's sandbox: reads anywhere under `root`, writes only to
/// an explicit set of files, and every other permission request rejected.
///
/// Agents that edit files with their own tools instead of `fs/*` (as
/// `claude-agent-acp` does, see NOTES.md) ask permission first; such a
/// request is allowed only when it is an edit (or read) tool call and every
/// file it names is writable (or under `root`). Anything else — shell, fetch,
/// unknown tools, or an edit naming no file — is rejected.
///
/// Paths are resolved through symlinks and `..` before checking, so neither
/// can escape the root or alias a writable file.
#[derive(Debug, Clone)]
pub struct ScopedPolicy {
    root: PathBuf,
    writable: BTreeSet<PathBuf>,
}

impl ScopedPolicy {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        ScopedPolicy {
            root: root.into(),
            writable: BTreeSet::new(),
        }
    }

    pub fn allow_write(mut self, path: impl Into<PathBuf>) -> Self {
        self.writable.insert(path.into());
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn writable(&self) -> impl Iterator<Item = &Path> {
        self.writable.iter().map(PathBuf::as_path)
    }

    fn may_read(&self, path: &Path) -> Result<PathBuf, PolicyError> {
        let resolved = resolve(path)?;
        let root = resolve(&self.root)?;
        if resolved.starts_with(&root) {
            Ok(resolved)
        } else {
            Err(PolicyError::Denied(format!(
                "{} is outside the readable root {}",
                path.display(),
                self.root.display()
            )))
        }
    }

    fn may_write(&self, path: &Path) -> Result<PathBuf, PolicyError> {
        let resolved = resolve(path)?;
        let allowed = self
            .writable
            .iter()
            .any(|w| resolve(w).is_ok_and(|w| w == resolved));
        if allowed {
            Ok(resolved)
        } else {
            Err(PolicyError::Denied(format!(
                "writing {} is not allowed in this session",
                path.display()
            )))
        }
    }
}

impl SessionPolicy for ScopedPolicy {
    fn read_text_file(&self, path: &Path) -> Result<String, PolicyError> {
        let resolved = self.may_read(path)?;
        std::fs::read_to_string(&resolved).map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => PolicyError::NotFound(path.to_owned()),
            _ => PolicyError::Io(err),
        })
    }

    fn write_text_file(&self, path: &Path, content: &str) -> Result<(), PolicyError> {
        let resolved = self.may_write(path)?;
        std::fs::write(resolved, content)?;
        Ok(())
    }

    fn decide_permission(&self, request: &PermissionRequest) -> PermissionDecision {
        let paths = request.paths();
        let allowed = !paths.is_empty()
            && match request.kind {
                Some(acp::ToolKind::Edit) => paths.iter().all(|p| self.may_write(p).is_ok()),
                Some(acp::ToolKind::Read) => paths.iter().all(|p| self.may_read(p).is_ok()),
                _ => false,
            };
        tracing::info!(
            tool_call = %request.tool_call_id,
            title = ?request.title,
            kind = ?request.kind,
            ?paths,
            allowed,
            "scoped policy decided permission request"
        );
        if allowed {
            PermissionDecision::AllowOnce
        } else {
            PermissionDecision::RejectOnce
        }
    }
}

/// Canonicalize `path`; a file that does not exist yet resolves through its
/// (existing) parent. Relative paths are rejected: ACP paths are absolute.
fn resolve(path: &Path) -> Result<PathBuf, PolicyError> {
    if !path.is_absolute() {
        return Err(PolicyError::Denied(format!(
            "{} is not an absolute path",
            path.display()
        )));
    }
    match path.canonicalize() {
        Ok(resolved) => Ok(resolved),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            let (Some(parent), Some(Component::Normal(name))) =
                (path.parent(), path.components().next_back())
            else {
                return Err(PolicyError::NotFound(path.to_owned()));
            };
            let parent = parent.canonicalize().map_err(|err| match err.kind() {
                io::ErrorKind::NotFound => PolicyError::NotFound(path.to_owned()),
                _ => PolicyError::Io(err),
            })?;
            Ok(parent.join(name))
        }
        Err(err) => Err(PolicyError::Io(err)),
    }
}

/// Apply ACP's optional 1-based `line` and `limit` to a whole file.
pub(crate) fn slice_lines(text: &str, line: Option<u32>, limit: Option<u32>) -> String {
    if line.is_none() && limit.is_none() {
        return text.to_owned();
    }
    let skip = line.map_or(0, |l| l.saturating_sub(1) as usize);
    let take = limit.map_or(usize::MAX, |l| l as usize);
    text.split_inclusive('\n').skip(skip).take(take).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slices_lines() {
        let text = "a\nb\nc\nd";
        assert_eq!(slice_lines(text, None, None), text);
        assert_eq!(slice_lines(text, Some(2), None), "b\nc\nd");
        assert_eq!(slice_lines(text, Some(2), Some(2)), "b\nc\n");
        assert_eq!(slice_lines(text, None, Some(1)), "a\n");
        assert_eq!(slice_lines(text, Some(9), None), "");
    }

    fn choice(id: &str, kind: acp::PermissionOptionKind) -> PermissionChoice {
        PermissionChoice {
            id: id.into(),
            name: id.into(),
            kind,
        }
    }

    #[test]
    fn decisions_fall_back_within_polarity() {
        use acp::PermissionOptionKind as K;
        let options = [
            choice("allow", K::AllowOnce),
            choice("never", K::RejectAlways),
        ];
        let id = |d: PermissionDecision| d.resolve(&options).map(|o| o.id.as_str());
        assert_eq!(id(PermissionDecision::AllowAlways), Some("allow"));
        assert_eq!(id(PermissionDecision::RejectOnce), Some("never"));
        assert_eq!(id(PermissionDecision::Cancel), None);
        assert_eq!(id(PermissionDecision::Select("nope".into())), None);

        let allow_only = [choice("allow", K::AllowOnce)];
        assert!(
            PermissionDecision::RejectOnce
                .resolve(&allow_only)
                .is_none()
        );
    }

    #[test]
    fn scoped_policy_confines_reads_and_writes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("crate");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/lib.rs"), "pub fn x() {}").unwrap();
        std::fs::write(dir.path().join("secret"), "s").unwrap();
        let hole = root.join("src/hole.rs");
        let policy = ScopedPolicy::new(&root).allow_write(&hole);

        assert!(policy.read_text_file(&root.join("src/lib.rs")).is_ok());
        assert!(matches!(
            policy.read_text_file(&root.join("src/../../secret")),
            Err(PolicyError::Denied(_))
        ));
        assert!(matches!(
            policy.read_text_file(&root.join("missing.rs")),
            Err(PolicyError::NotFound(_))
        ));
        assert!(matches!(
            policy.read_text_file(Path::new("relative.rs")),
            Err(PolicyError::Denied(_))
        ));

        policy.write_text_file(&hole, "fn hole() {}").unwrap();
        assert_eq!(std::fs::read_to_string(&hole).unwrap(), "fn hole() {}");
        policy
            .write_text_file(&root.join("src/./hole.rs"), "fn hole2() {}")
            .unwrap();
        assert!(matches!(
            policy.write_text_file(&root.join("src/lib.rs"), "evil"),
            Err(PolicyError::Denied(_))
        ));
        assert_eq!(
            std::fs::read_to_string(root.join("src/lib.rs")).unwrap(),
            "pub fn x() {}"
        );

        #[cfg(unix)]
        {
            let link = root.join("src/alias.rs");
            std::os::unix::fs::symlink(root.join("src/lib.rs"), &link).unwrap();
            assert!(policy.write_text_file(&link, "evil").is_err());
        }
    }

    #[test]
    fn scoped_policy_allows_only_edits_of_writable_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let hole = root.join("hole.rs");
        let policy = ScopedPolicy::new(&root).allow_write(&hole);
        let request = |kind, input: serde_json::Value| PermissionRequest {
            tool_call_id: "t".into(),
            title: None,
            kind: Some(kind),
            raw_input: Some(input),
            locations: Vec::new(),
            options: Vec::new(),
        };
        let decide = |r: PermissionRequest| policy.decide_permission(&r);
        let path = |p: &Path| serde_json::json!({ "file_path": p });

        assert_eq!(
            decide(request(acp::ToolKind::Edit, path(&hole))),
            PermissionDecision::AllowOnce
        );
        assert_eq!(
            decide(request(acp::ToolKind::Edit, path(&root.join("other.rs")))),
            PermissionDecision::RejectOnce
        );
        assert_eq!(
            decide(request(acp::ToolKind::Edit, serde_json::json!({}))),
            PermissionDecision::RejectOnce
        );
        assert_eq!(
            decide(request(acp::ToolKind::Read, path(&root.join("x.rs")))),
            PermissionDecision::AllowOnce
        );
        assert_eq!(
            decide(request(
                acp::ToolKind::Execute,
                serde_json::json!({"command": "ls", "path": hole})
            )),
            PermissionDecision::RejectOnce
        );
    }
}
