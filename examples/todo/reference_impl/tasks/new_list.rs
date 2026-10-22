use crate::prelude::*;
use crate::tasks::*;

pub fn new_list() -> TodoList {
    TodoList { tasks: Vec::new(), next_id: TaskId::parse(1).expect("1 is a valid task id") }
}
