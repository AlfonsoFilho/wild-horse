// wh:key=2e47c4810f9a154d
use crate::prelude::*;
use crate::tasks::*;

use std::sync::{Mutex, MutexGuard};
use tracing::trace;

/// Internal state of `MemoryStore`, created with `Default` by `MemoryStore::new`.
pub struct State {
    todos: Mutex<TodoList>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            todos: Mutex::new(new_list()),
        }
    }
}

impl State {
    /// Locks the list, recovering from poisoning: the list is only ever
    /// replaced wholesale, so it is always in a consistent state.
    fn lock(&self) -> MutexGuard<'_, TodoList> {
        self.todos.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

pub fn load(this: &MemoryStore) -> Result<TodoList, StoreError> {
    let todos = this.state.lock().clone();
    trace!(tasks = todos.tasks.len(), "loaded todo list from memory");
    Ok(todos)
}

pub fn save(this: &MemoryStore, todos: &TodoList) -> Result<(), StoreError> {
    this.state.lock().clone_from(todos);
    trace!(tasks = todos.tasks.len(), "saved todo list to memory");
    Ok(())
}
