use crate::prelude::*;
use crate::tasks::*;

/// Internal state of `MemoryStore`, created with `Default` by `MemoryStore::new`.
pub struct State {
    todos: std::sync::Mutex<TodoList>,
}

impl Default for State {
    fn default() -> Self {
        Self { todos: std::sync::Mutex::new(new_list()) }
    }
}

pub fn load(this: &MemoryStore) -> Result<TodoList, StoreError> {
    let todos = this.state.todos.lock().map_err(|_| StoreError::Io("store lock poisoned".into()))?;
    Ok(todos.clone())
}

pub fn save(this: &MemoryStore, todos: &TodoList) -> Result<(), StoreError> {
    let mut stored = this.state.todos.lock().map_err(|_| StoreError::Io("store lock poisoned".into()))?;
    *stored = todos.clone();
    Ok(())
}
