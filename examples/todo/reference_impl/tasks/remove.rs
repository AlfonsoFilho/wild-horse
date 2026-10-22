use crate::prelude::*;
use crate::tasks::*;

pub fn remove(mut todos: TodoList, id: TaskId) -> Result<Change, TodoError> {
    let pos = todos.tasks.iter().position(|t| t.id == id).ok_or(TodoError::NotFound(id))?;
    let task = todos.tasks.remove(pos);
    Ok(Change { todos, message: format!("removed #{id}: {}", task.title) })
}
