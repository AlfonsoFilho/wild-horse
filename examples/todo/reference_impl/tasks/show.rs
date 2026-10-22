use crate::prelude::*;
use crate::tasks::*;

pub fn show(todos: &TodoList, include_done: bool) -> String {
    let lines: Vec<String> = todos
        .tasks
        .iter()
        .filter(|t| include_done || t.status == Status::Open)
        .map(|t| {
            let mark = if t.status == Status::Done { "x" } else { " " };
            format!("[{mark}] #{} {}", t.id, t.title)
        })
        .collect();
    if lines.is_empty() { "nothing to do".to_string() } else { lines.join("\n") }
}
