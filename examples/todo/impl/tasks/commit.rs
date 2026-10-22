// wh:key=a736646212852661
use crate::prelude::*;
use crate::tasks::*;

pub fn commit(cap_store: &impl Store, change: Change) -> Result<String, StoreError> {
    let Change { todos, message } = change;
    tracing::debug!(tasks = todos.tasks.len(), "saving todo list");
    cap_store.save(&todos)?;
    Ok(message)
}
