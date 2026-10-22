use crate::prelude::*;
use crate::tasks::*;

pub fn commit(cap_store: &impl Store, change: Change) -> Result<String, StoreError> {
    cap_store.save(&change.todos)?;
    Ok(change.message)
}
