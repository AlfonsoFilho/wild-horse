// wh:key=67b07d0bbbcbec59 prose=0b9e02a4
use crate::prelude::*;
use crate::tasks::*;

use tracing::debug;

pub fn remove(todos: TodoList, id: TaskId) -> Result<Change, TodoError> {
    let mut todos = todos;
    let Some(index) = todos.tasks.iter().position(|t| t.id == id) else {
        debug!(%id, "task to remove not found");
        return Err(TodoError::NotFound(id));
    };
    // `remove` (not `swap_remove`) keeps the remaining tasks in their order;
    // `next_id` is left untouched so the number is never reused.
    let task = todos.tasks.remove(index);
    debug!(%id, remaining = todos.tasks.len(), "task removed");
    let message = format!("removed #{}: {}", task.id, task.title);
    Ok(Change { todos, message })
}
