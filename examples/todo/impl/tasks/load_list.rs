// wh:key=c4bce6665eedf07e
use crate::prelude::*;
use crate::tasks::*;

pub fn load_list(cap_store: &impl Store) -> Result<TodoList, StoreError> {
    tracing::trace!("loading todo list from store");
    cap_store.load()
}
