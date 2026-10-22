// wh:key=9f5ca04d0e1907ab prose=69e008ac
use crate::prelude::*;
use crate::tasks::*;

use std::fmt::Write as _;

pub fn show(todos: &TodoList, include_done: bool) -> String {
    let mut out = String::new();
    for task in todos
        .tasks
        .iter()
        .filter(|t| include_done || !matches!(t.status, Status::Done))
    {
        if !out.is_empty() {
            out.push('\n');
        }
        let mark = match task.status {
            Status::Open => ' ',
            Status::Done => 'x',
        };
        // Writing to a String cannot fail.
        let _ = write!(out, "[{mark}] #{} {}", task.id, task.title);
    }
    trace!(include_done, total = todos.tasks.len(), "rendered task list");
    if out.is_empty() {
        debug!("no tasks to show");
        return "nothing to do".to_owned();
    }
    out
}
