// wh:key=eea63f74cd7bf761
use crate::prelude::*;
use crate::tasks::*;

use std::fs;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use tracing::{debug, trace};

/// Internal state of `JsonFileStore`, created with `Default` by `JsonFileStore::new`.
#[derive(Default)]
pub struct State {}

const DEFAULT_PATH: &str = "todo.json";

/// The file the store reads and writes: an empty `path` means `todo.json`.
fn file_path(this: &JsonFileStore) -> &Path {
    if this.path.is_empty() {
        Path::new(DEFAULT_PATH)
    } else {
        Path::new(&this.path)
    }
}

/// `<path>.tmp`, next to the target so the rename stays on one filesystem.
fn tmp_path(path: &Path) -> PathBuf {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    PathBuf::from(tmp)
}

fn io_error(action: &str, path: &Path, e: &io::Error) -> StoreError {
    StoreError::Io(format!("cannot {action} {}: {e}", path.display()))
}

pub fn load(this: &JsonFileStore) -> Result<TodoList, StoreError> {
    let path = file_path(this);
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            debug!(path = %path.display(), "store file missing, starting with an empty list");
            return Ok(new_list());
        }
        Err(e) => return Err(io_error("read", path, &e)),
    };
    trace!(path = %path.display(), bytes = text.len(), "read store file");

    let todos: TodoList = serde_json::from_str(&text)
        .map_err(|e| StoreError::Corrupt(format!("{}: {e}", path.display())))?;
    todos.wh_check().map_err(|e| {
        StoreError::Corrupt(format!("{}: violates `{}`", path.display(), e.rule))
    })?;
    debug!(path = %path.display(), tasks = todos.tasks.len(), "loaded list");
    Ok(todos)
}

pub fn save(this: &JsonFileStore, todos: &TodoList) -> Result<(), StoreError> {
    let path = file_path(this);
    let tmp = tmp_path(path);

    let write_tmp = || -> io::Result<()> {
        let mut out = BufWriter::new(fs::File::create(&tmp)?);
        serde_json::to_writer_pretty(&mut out, todos).map_err(io::Error::other)?;
        out.write_all(b"\n")?;
        out.into_inner().map_err(io::IntoInnerError::into_error)?.sync_all()
    };
    if let Err(e) = write_tmp() {
        // Best effort: don't leave a stray partial temp file behind.
        let _ = fs::remove_file(&tmp);
        return Err(io_error("write", &tmp, &e));
    }
    trace!(tmp = %tmp.display(), "wrote temporary file");

    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        io_error("replace", path, &e)
    })?;
    debug!(path = %path.display(), tasks = todos.tasks.len(), "saved list");
    Ok(())
}
