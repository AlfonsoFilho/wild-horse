use crate::prelude::*;
use crate::tasks::*;

pub fn load_list(cap_store: &impl Store) -> Result<TodoList, StoreError> {
    cap_store.load()
}
