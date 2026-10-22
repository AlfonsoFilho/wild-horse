//! The document store: the single source of truth every view edits.
//!
//! Every change — typing, a graph gesture, an inspector field, an agent
//! write, a file changed on disk — is a [`Transaction`] of text edits tagged
//! with its [`Origin`]. Undo and redo work over the one global log, so a
//! graph edit and a typed edit are undone the same way.

use std::collections::BTreeMap;
use std::ops::Range;

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum Origin {
    Text,
    Graph,
    Inspector,
    Agent,
    Disk,
}

/// Replace `range` (byte offsets in the text *before* the transaction) with `text`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TextEdit {
    pub range: Range<usize>,
    pub text: String,
}

impl TextEdit {
    pub fn insert(at: usize, text: impl Into<String>) -> Self {
        Self { range: at..at, text: text.into() }
    }

    pub fn replace(range: Range<usize>, text: impl Into<String>) -> Self {
        Self { range, text: text.into() }
    }

    pub fn delete(range: Range<usize>) -> Self {
        Self { range, text: String::new() }
    }
}

/// Edits to one file. Ranges must not overlap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileEdit {
    pub path: String,
    pub edits: Vec<TextEdit>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Transaction {
    pub id: u64,
    pub origin: Origin,
    pub label: String,
    pub edits: Vec<FileEdit>,
    /// Edits that revert this transaction (ranges in the post-transaction text).
    pub inverse: Vec<FileEdit>,
    /// Typing coalesces into one undo step per `(origin, key)` burst.
    #[serde(skip)]
    pub coalesce: Option<(String, std::time::Instant)>,
}

impl Transaction {
    /// Byte ranges in the post-transaction text that this transaction wrote,
    /// per file — used to highlight what an agent or the graph changed.
    pub fn changed_ranges(&self) -> BTreeMap<String, Vec<Range<usize>>> {
        let mut out: BTreeMap<String, Vec<Range<usize>>> = BTreeMap::new();
        for fe in &self.inverse {
            out.entry(fe.path.clone()).or_default().extend(fe.edits.iter().map(|e| e.range.clone()));
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocError {
    UnknownFile(String),
    BadRange { path: String, range: Range<usize> },
    Overlap { path: String },
    Conflict { path: String, merged_with_markers: String },
}

impl std::fmt::Display for DocError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DocError::UnknownFile(p) => write!(f, "unknown file `{p}`"),
            DocError::BadRange { path, range } => write!(f, "edit range {range:?} is outside `{path}` or not on a char boundary"),
            DocError::Overlap { path } => write!(f, "overlapping edits in `{path}`"),
            DocError::Conflict { path, .. } => write!(f, "conflicting concurrent edits in `{path}`"),
        }
    }
}

impl std::error::Error for DocError {}

#[derive(Debug, Clone)]
struct Buffer {
    text: String,
    /// Text as last loaded from or saved to disk.
    saved: String,
}

#[derive(Debug, Clone, Default)]
pub struct DocStore {
    files: BTreeMap<String, Buffer>,
    undo: Vec<Transaction>,
    redo: Vec<Transaction>,
    next_id: u64,
    version: u64,
}

impl DocStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Monotonic version, bumped by every applied transaction.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Open (or reopen) a file. Reopening replaces the buffer, so history
    /// that touched the old buffer is dropped.
    pub fn open(&mut self, path: impl Into<String>, text: impl Into<String>) {
        let path = path.into();
        let text = text.into();
        if self.files.contains_key(&path) {
            self.forget(&path);
        }
        self.files.insert(path, Buffer { saved: text.clone(), text });
        self.version += 1;
    }

    /// Close a file; undo/redo steps that touch it can no longer apply and are dropped.
    pub fn close(&mut self, path: &str) {
        self.files.remove(path);
        self.forget(path);
        self.version += 1;
    }

    /// Drop every history entry that edits `path`.
    fn forget(&mut self, path: &str) {
        let touches = |t: &Transaction| t.edits.iter().chain(&t.inverse).any(|f| f.path == path);
        self.undo.retain(|t| !touches(t));
        self.redo.retain(|t| !touches(t));
    }

    pub fn paths(&self) -> impl Iterator<Item = &str> {
        self.files.keys().map(String::as_str)
    }

    pub fn text(&self, path: &str) -> Option<&str> {
        self.files.get(path).map(|b| b.text.as_str())
    }

    pub fn is_dirty(&self, path: &str) -> bool {
        self.files.get(path).is_some_and(|b| b.text != b.saved)
    }

    /// Mark a file as saved with its current text (the caller wrote it to disk).
    pub fn mark_saved(&mut self, path: &str) {
        if let Some(b) = self.files.get_mut(path) {
            b.saved = b.text.clone();
        }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn history(&self) -> &[Transaction] {
        &self.undo
    }

    /// Apply a transaction; on success it becomes undoable as one step.
    pub fn apply(&mut self, origin: Origin, label: impl Into<String>, edits: Vec<FileEdit>) -> Result<&Transaction, DocError> {
        let inverse = self.apply_raw(&edits)?;
        self.next_id += 1;
        self.undo.push(Transaction { id: self.next_id, origin, label: label.into(), edits, inverse, coalesce: None });
        self.redo.clear();
        Ok(self.undo.last().unwrap())
    }

    /// Like [`apply`](Self::apply), but merged into the previous undo step when
    /// that step has the same origin and `key` (e.g. one code block) and was
    /// made less than [`COALESCE_WINDOW`] ago — so ⌘Z undoes a burst of typing,
    /// not a character. Breaks on whitespace-to-word boundaries are left to the caller.
    pub fn apply_coalescing(&mut self, origin: Origin, label: impl Into<String>, edits: Vec<FileEdit>, key: &str) -> Result<&Transaction, DocError> {
        let now = std::time::Instant::now();
        let mergeable = self.redo.is_empty()
            && self.undo.last().is_some_and(|t| {
                t.origin == origin
                    && t.coalesce.as_ref().is_some_and(|(k, at)| k == key && now.duration_since(*at) < COALESCE_WINDOW)
                    && same_files(&t.edits, &edits)
            });
        if !mergeable {
            self.apply(origin, label, edits)?;
            let tx = self.undo.last_mut().unwrap();
            tx.coalesce = Some((key.to_string(), now));
            return Ok(self.undo.last().unwrap());
        }
        let prev = self.undo.pop().unwrap();
        // text before `prev`: undo it on a copy
        let mut before: BTreeMap<String, String> = BTreeMap::new();
        for fe in &prev.inverse {
            let cur = self.files[&fe.path].text.clone();
            before.insert(fe.path.clone(), apply_edits_to(&cur, &fe.edits));
        }
        if let Err(e) = self.apply_raw(&edits) {
            self.undo.push(prev);
            return Err(e);
        }
        let mut merged = Vec::new();
        let mut inverse = Vec::new();
        for (path, old) in &before {
            let new = &self.files[path].text;
            merged.push(FileEdit { path: path.clone(), edits: diff_edits(old, new) });
            inverse.push(FileEdit { path: path.clone(), edits: diff_edits(new, old) });
        }
        self.undo.push(Transaction { edits: merged, inverse, coalesce: Some((key.to_string(), now)), ..prev });
        Ok(self.undo.last().unwrap())
    }

    /// Undo the last step. A step that can no longer apply (its file changed
    /// outside the store's history) is dropped instead.
    pub fn undo(&mut self) -> Option<&Transaction> {
        let tx = self.undo.pop()?;
        if self.apply_raw(&tx.inverse).is_err() {
            self.redo.clear();
            return None;
        }
        // `tx.edits` are in the coordinates of the restored text, so redo re-applies them as is
        self.redo.push(tx);
        self.redo.last()
    }

    pub fn redo(&mut self) -> Option<&Transaction> {
        let tx = self.redo.pop()?;
        let Ok(inverse) = self.apply_raw(&tx.edits) else {
            self.redo.clear();
            return None;
        };
        self.undo.push(Transaction { inverse, ..tx });
        self.undo.last()
    }

    /// Replace a whole file with `new_text` written by someone who started
    /// from `base` (an agent that read the file earlier, or the disk). The
    /// change is three-way merged onto the current buffer so concurrent edits
    /// survive, then applied as minimal edits.
    pub fn merge_write(&mut self, origin: Origin, label: impl Into<String>, path: &str, base: &str, new_text: &str) -> Result<Option<&Transaction>, DocError> {
        let current = self.text(path).ok_or_else(|| DocError::UnknownFile(path.to_string()))?.to_string();
        let merged = if base == current {
            new_text.to_string()
        } else {
            match diffy::merge(base, &current, new_text) {
                Ok(m) => m,
                Err(with_markers) => {
                    return Err(DocError::Conflict { path: path.to_string(), merged_with_markers: with_markers });
                }
            }
        };
        let edits = diff_edits(&current, &merged);
        if edits.is_empty() {
            return Ok(None);
        }
        self.apply(origin, label, vec![FileEdit { path: path.to_string(), edits }]).map(Some)
    }

    /// The file changed on disk: merge the disk change onto unsaved edits.
    pub fn reload_from_disk(&mut self, path: &str, disk_text: &str) -> Result<Option<&Transaction>, DocError> {
        let saved = self.files.get(path).ok_or_else(|| DocError::UnknownFile(path.to_string()))?.saved.clone();
        if saved == disk_text {
            return Ok(None);
        }
        let r = self.merge_write(Origin::Disk, "file changed on disk", path, &saved, disk_text)?.is_some();
        self.files.get_mut(path).unwrap().saved = disk_text.to_string();
        Ok(if r { self.undo.last() } else { None })
    }

    /// Apply edits without touching history; returns their inverse.
    fn apply_raw(&mut self, edits: &[FileEdit]) -> Result<Vec<FileEdit>, DocError> {
        // validate everything before mutating anything
        for fe in edits {
            let buf = self.files.get(&fe.path).ok_or_else(|| DocError::UnknownFile(fe.path.clone()))?;
            let mut sorted: Vec<&TextEdit> = fe.edits.iter().collect();
            sorted.sort_by_key(|e| (e.range.start, e.range.end));
            for w in sorted.windows(2) {
                if w[0].range.end > w[1].range.start {
                    return Err(DocError::Overlap { path: fe.path.clone() });
                }
            }
            for e in &fe.edits {
                let ok = e.range.start <= e.range.end
                    && e.range.end <= buf.text.len()
                    && buf.text.is_char_boundary(e.range.start)
                    && buf.text.is_char_boundary(e.range.end);
                if !ok {
                    return Err(DocError::BadRange { path: fe.path.clone(), range: e.range.clone() });
                }
            }
        }
        let mut inverse = Vec::new();
        for fe in edits {
            let buf = self.files.get_mut(&fe.path).unwrap();
            let mut sorted: Vec<&TextEdit> = fe.edits.iter().collect();
            sorted.sort_by_key(|e| (e.range.start, e.range.end));
            let mut out = String::with_capacity(buf.text.len());
            let mut inv = Vec::new();
            let mut cursor = 0;
            for e in sorted {
                out.push_str(&buf.text[cursor..e.range.start]);
                let new_start = out.len();
                out.push_str(&e.text);
                inv.push(TextEdit { range: new_start..out.len(), text: buf.text[e.range.clone()].to_string() });
                cursor = e.range.end;
            }
            out.push_str(&buf.text[cursor..]);
            buf.text = out;
            inverse.push(FileEdit { path: fe.path.clone(), edits: inv });
        }
        self.version += 1;
        Ok(inverse)
    }
}

/// Consecutive edits closer together than this become one undo step.
pub const COALESCE_WINDOW: std::time::Duration = std::time::Duration::from_millis(1200);

fn same_files(a: &[FileEdit], b: &[FileEdit]) -> bool {
    let fa: std::collections::BTreeSet<&str> = a.iter().map(|f| f.path.as_str()).collect();
    let fb: std::collections::BTreeSet<&str> = b.iter().map(|f| f.path.as_str()).collect();
    fa == fb
}

/// Apply non-overlapping edits to a string (ranges in `text`).
pub fn apply_edits_to(text: &str, edits: &[TextEdit]) -> String {
    let mut sorted: Vec<&TextEdit> = edits.iter().collect();
    sorted.sort_by_key(|e| e.range.start);
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for e in sorted {
        out.push_str(&text[cursor..e.range.start]);
        out.push_str(&e.text);
        cursor = e.range.end;
    }
    out.push_str(&text[cursor..]);
    out
}

/// Minimal edits turning `old` into `new` (line-level diff, refined to the
/// changed span inside each hunk so unchanged prose is never rewritten).
pub fn diff_edits(old: &str, new: &str) -> Vec<TextEdit> {
    use similar::{DiffTag, TextDiff};
    let diff = TextDiff::from_lines(old, new);
    let mut edits = Vec::new();
    let old_lines: Vec<usize> = line_offsets(old);
    let new_lines: Vec<usize> = line_offsets(new);
    for op in diff.ops() {
        if op.tag() == DiffTag::Equal {
            continue;
        }
        let o = op.old_range();
        let n = op.new_range();
        let o_start = old_lines[o.start];
        let o_end = old_lines[o.end];
        let n_start = new_lines[n.start];
        let n_end = new_lines[n.end];
        // shrink to the differing middle of the hunk
        let a = &old[o_start..o_end];
        let b = &new[n_start..n_end];
        let prefix = common_prefix(a, b);
        let suffix = common_suffix(&a[prefix..], &b[prefix..]);
        edits.push(TextEdit {
            range: o_start + prefix..o_end - suffix,
            text: b[prefix..b.len() - suffix].to_string(),
        });
    }
    edits
}

fn line_offsets(s: &str) -> Vec<usize> {
    let mut v = vec![0];
    for (i, c) in s.char_indices() {
        if c == '\n' {
            v.push(i + 1);
        }
    }
    if *v.last().unwrap() != s.len() {
        v.push(s.len());
    }
    v
}

fn common_prefix(a: &str, b: &str) -> usize {
    let mut n = 0;
    for (x, y) in a.chars().zip(b.chars()) {
        if x != y {
            break;
        }
        n += x.len_utf8();
    }
    n
}

fn common_suffix(a: &str, b: &str) -> usize {
    let mut n = 0;
    for (x, y) in a.chars().rev().zip(b.chars().rev()) {
        if x != y {
            break;
        }
        n += x.len_utf8();
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> DocStore {
        let mut s = DocStore::new();
        s.open("a.wh.md", "hello world\nsecond line\n");
        s
    }

    #[test]
    fn apply_undo_redo() {
        let mut s = store();
        s.apply(Origin::Graph, "edit", vec![FileEdit {
            path: "a.wh.md".into(),
            edits: vec![TextEdit::replace(0..5, "howdy"), TextEdit::insert(12, "the ")],
        }])
        .unwrap();
        assert_eq!(s.text("a.wh.md").unwrap(), "howdy world\nthe second line\n");
        s.undo().unwrap();
        assert_eq!(s.text("a.wh.md").unwrap(), "hello world\nsecond line\n");
        s.redo().unwrap();
        assert_eq!(s.text("a.wh.md").unwrap(), "howdy world\nthe second line\n");
        s.undo().unwrap();
        assert!(!s.can_undo() && s.can_redo());
    }

    #[test]
    fn rejects_bad_edits_atomically() {
        let mut s = store();
        let r = s.apply(Origin::Text, "x", vec![FileEdit {
            path: "a.wh.md".into(),
            edits: vec![TextEdit::replace(0..5, "a"), TextEdit::replace(3..7, "b")],
        }]);
        assert!(matches!(r, Err(DocError::Overlap { .. })));
        assert_eq!(s.text("a.wh.md").unwrap(), "hello world\nsecond line\n");
    }

    #[test]
    fn merge_write_keeps_concurrent_edits() {
        let mut s = DocStore::new();
        s.open("a.wh.md", "hello world\n\nunchanged\n\nsecond line\n");
        let base = s.text("a.wh.md").unwrap().to_string();
        // the user edits line 1 while the agent rewrites the last line
        s.apply(Origin::Text, "type", vec![FileEdit { path: "a.wh.md".into(), edits: vec![TextEdit::replace(0..5, "HELLO")] }]).unwrap();
        let agent = "hello world\n\nunchanged\n\nsecond line, revised\n";
        let tx = s.merge_write(Origin::Agent, "agent", "a.wh.md", &base, agent).unwrap().unwrap();
        assert_eq!(tx.origin, Origin::Agent);
        assert_eq!(s.text("a.wh.md").unwrap(), "HELLO world\n\nunchanged\n\nsecond line, revised\n");
        // one undo reverts exactly the agent's change
        s.undo();
        assert_eq!(s.text("a.wh.md").unwrap(), "HELLO world\n\nunchanged\n\nsecond line\n");
    }

    #[test]
    fn merge_conflict_is_reported() {
        let mut s = store();
        let base = s.text("a.wh.md").unwrap().to_string();
        s.apply(Origin::Text, "type", vec![FileEdit { path: "a.wh.md".into(), edits: vec![TextEdit::replace(12..18, "2nd")] }]).unwrap();
        let r = s.merge_write(Origin::Agent, "agent", "a.wh.md", &base, "hello world\nother line\n");
        assert!(matches!(r, Err(DocError::Conflict { .. })));
    }

    #[test]
    fn typing_coalesces_into_one_undo_step() {
        let mut s = DocStore::new();
        s.open("a", "def f");
        for (i, c) in "oo".chars().enumerate() {
            let at = 5 + i;
            s.apply_coalescing(Origin::Text, "type", vec![FileEdit { path: "a".into(), edits: vec![TextEdit::insert(at, c.to_string())] }], "block0").unwrap();
        }
        assert_eq!(s.text("a").unwrap(), "def foo");
        assert_eq!(s.history().len(), 1);
        // a graph edit is its own step
        s.apply(Origin::Graph, "g", vec![FileEdit { path: "a".into(), edits: vec![TextEdit::insert(0, "# ")] }]).unwrap();
        s.undo();
        assert_eq!(s.text("a").unwrap(), "def foo");
        s.undo();
        assert_eq!(s.text("a").unwrap(), "def f");
        s.redo();
        assert_eq!(s.text("a").unwrap(), "def foo");
    }

    #[test]
    fn closing_or_reopening_drops_its_history() {
        let mut s = DocStore::new();
        s.open("a", "aaa");
        s.open("b", "bbb");
        let e = |p: &str| vec![FileEdit { path: p.into(), edits: vec![TextEdit::insert(0, "x")] }];
        s.apply(Origin::Text, "a1", e("a")).unwrap();
        s.apply(Origin::Text, "b1", e("b")).unwrap();
        s.apply(Origin::Text, "a2", e("a")).unwrap();
        s.close("a");
        assert_eq!(s.history().len(), 1, "only b's step is left");
        assert_eq!(s.undo().map(|t| t.label.clone()).as_deref(), Some("b1"));
        assert!(s.undo().is_none());
        // reopening replaces the buffer and its history
        s.open("b", "fresh");
        assert!(!s.can_redo() && !s.can_undo());
        s.apply(Origin::Text, "b2", e("b")).unwrap();
        s.open("b", "again");
        assert!(s.undo().is_none(), "no panic, nothing to undo");
    }

    #[test]
    fn diff_edits_are_minimal() {
        let old = "# T\n\nprose stays\n\n```wh\ndef f(x: int) -> int\n```\n";
        let new = "# T\n\nprose stays\n\n```wh\ndef f(x: int) -> str\n```\n";
        let e = diff_edits(old, new);
        assert_eq!(e.len(), 1);
        assert_eq!(&old[e[0].range.clone()], "int");
        assert_eq!(e[0].text, "str");
    }
}
