// wh:key=b64e181994af1033 prose=1a95d431
use crate::prelude::*;
use crate::tasks::*;
use tracing::debug;

pub fn add_task(todos: TodoList, title: &Title) -> Change {
    let TodoList { mut tasks, next_id } = todos;
    let id = next_id;
    let next_id = id
        .into_inner()
        .checked_add(1)
        .and_then(|n| TaskId::parse(n).ok())
        .expect("task id space exhausted");

    debug!(%id, %title, "adding task");
    tasks.push(Task {
        id,
        title: title.clone(),
        status: Status::Open,
    });

    Change {
        todos: TodoList { tasks, next_id },
        message: format!("added #{id}: {title}"),
    }
}
