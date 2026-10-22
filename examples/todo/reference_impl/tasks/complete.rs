use crate::prelude::*;
use crate::tasks::*;

pub fn complete(mut todos: TodoList, id: TaskId) -> Result<Change, TodoError> {
    let task = todos.tasks.iter_mut().find(|t| t.id == id).ok_or(TodoError::NotFound(id))?;
    task.status = Status::Done;
    let message = format!("done #{id}: {}", task.title);
    Ok(Change { todos, message })
}
