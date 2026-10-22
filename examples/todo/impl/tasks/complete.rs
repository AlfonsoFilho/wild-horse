// wh:key=1473abd99d63b847 prose=3abf1a2d
use crate::prelude::*;
use crate::tasks::*;

pub fn complete(todos: TodoList, id: TaskId) -> Result<Change, TodoError> {
    let mut todos = todos;
    let Some(task) = todos.tasks.iter_mut().find(|t| t.id == id) else {
        tracing::debug!(%id, "task not found");
        return Err(TodoError::NotFound(id));
    };
    if task.status == Status::Done {
        tracing::trace!(%id, "task already done");
    }
    task.status = Status::Done;
    let message = format!("done #{}: {}", task.id, task.title);
    Ok(Change { todos, message })
}
