// wh:key=64dba4ffb24dafe0
use crate::prelude::*;
use crate::tasks::*;

use tracing::trace;

pub fn new_list() -> TodoList {
    trace!("creating an empty todo list");
    TodoList {
        tasks: Vec::new(),
        next_id: TaskId::parse(1).expect("1 satisfies TaskId"),
    }
}
