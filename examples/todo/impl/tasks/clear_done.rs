// wh:key=d46d250fdb9c9604 prose=19b649f1
use crate::prelude::*;
use crate::tasks::*;

pub fn clear_done(todos: TodoList) -> Change {
    let mut todos = todos;
    let before = todos.tasks.len();
    // Removing tasks keeps ids unique and below next_id, so the invariants hold.
    todos.tasks.retain(|t| t.status == Status::Open);
    let cleared = before - todos.tasks.len();
    debug!(cleared, remaining = todos.tasks.len(), "cleared done tasks");
    Change {
        todos,
        message: format!("cleared {cleared} done task(s)"),
    }
}
