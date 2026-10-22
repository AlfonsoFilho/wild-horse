use crate::prelude::*;
use crate::tasks::*;

pub fn clear_done(mut todos: TodoList) -> Change {
    let before = todos.tasks.len();
    todos.tasks.retain(|t| t.status == Status::Open);
    let cleared = before - todos.tasks.len();
    Change { todos, message: format!("cleared {cleared} done task(s)") }
}
