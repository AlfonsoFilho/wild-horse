use crate::prelude::*;
use crate::tasks::*;

use std::path::PathBuf;

/// Internal state of `JsonFileStore`, created with `Default` by `JsonFileStore::new`.
#[derive(Default)]
pub struct State {}

fn path(this: &JsonFileStore) -> PathBuf {
    if this.path.is_empty() { PathBuf::from("todo.json") } else { PathBuf::from(&this.path) }
}

pub fn load(this: &JsonFileStore) -> Result<TodoList, StoreError> {
    let path = path(this);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(new_list()),
        Err(e) => return Err(StoreError::Io(format!("{}: {e}", path.display()))),
    };
    let todos: TodoList =
        serde_json::from_str(&text).map_err(|e| StoreError::Corrupt(format!("{}: {e}", path.display())))?;
    for t in &todos.tasks {
        if !TaskId::check(t.id.as_inner()) || !Title::check(t.title.as_inner()) {
            return Err(StoreError::Corrupt(format!("{}: invalid task {t:?}", path.display())));
        }
    }
    if !TaskId::check(todos.next_id.as_inner()) {
        return Err(StoreError::Corrupt(format!("{}: invalid next_id", path.display())));
    }
    todos.wh_check().map_err(|e| StoreError::Corrupt(format!("{}: {e}", path.display())))?;
    Ok(todos)
}

pub fn save(this: &JsonFileStore, todos: &TodoList) -> Result<(), StoreError> {
    let path = path(this);
    let mut tmp = path.clone().into_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let text = serde_json::to_string_pretty(todos).map_err(|e| StoreError::Io(e.to_string()))?;
    std::fs::write(&tmp, text + "\n").map_err(|e| StoreError::Io(format!("{}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, &path).map_err(|e| StoreError::Io(format!("{}: {e}", path.display())))?;
    Ok(())
}
