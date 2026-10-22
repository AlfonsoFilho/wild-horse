use crate::prelude::*;
use crate::tasks::*;

pub fn add_task(mut todos: TodoList, title: &Title) -> Change {
    let id = todos.next_id;
    todos.next_id = TaskId::parse(*id + 1).expect("ids only grow");
    todos.tasks.push(Task { id, title: title.clone(), status: Status::Open });
    Change { todos, message: format!("added #{id}: {title}") }
}
