// wh:key=4f82d0bec13e370c prose=b0a209f9
use crate::prelude::*;
use crate::signup::*;

use std::sync::{Mutex, MutexGuard};

use tracing::{debug, trace};

/// Internal state of `MemoryDb`, created with `Default` by `MemoryDb::new`.
#[derive(Default)]
pub struct State {
    users: Mutex<Vec<User>>,
}

/// Locks the user table; a poisoned lock means the store is unusable.
fn lock(this: &MemoryDb) -> Result<MutexGuard<'_, Vec<User>>, DbError> {
    this.state.users.lock().map_err(|_| {
        debug!("user table lock is poisoned");
        DbError::Unavailable
    })
}

pub fn insert(this: &MemoryDb, form: ValidForm) -> Result<u64, DbError> {
    let mut users = lock(this)?;
    if users.iter().any(|u| u.email == form.email) {
        debug!(count = users.len(), "insert rejected: email already taken");
        return Err(DbError::Duplicate(form.email));
    }
    // Ids start at 1 and increase by one; users are never removed.
    let id = u64::try_from(users.len()).map_err(|_| DbError::Unavailable)? + 1;
    users.push(User {
        id,
        email: form.email,
        age: form.age,
        guardian: form.guardian,
    });
    trace!(id, count = users.len(), "user inserted");
    Ok(id)
}

pub fn find_by_email(this: &MemoryDb, email: &Email) -> Result<Option<User>, DbError> {
    let users = lock(this)?;
    let found = users.iter().find(|u| u.email == *email).cloned();
    trace!(found = found.as_ref().map(|u| u.id), "lookup by email");
    Ok(found)
}
