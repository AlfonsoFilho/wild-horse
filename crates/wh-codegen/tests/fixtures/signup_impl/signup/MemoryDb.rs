use crate::prelude::*;
use crate::signup::*;

/// Internal state of `MemoryDb`, created with `Default` by `MemoryDb::new`.
#[derive(Default)]
pub struct State {
    users: std::sync::Mutex<Vec<User>>,
}

pub fn insert(this: &MemoryDb, form: ValidForm) -> Result<u64, DbError> {
    let mut users = this.state.users.lock().map_err(|_| DbError::Unavailable)?;
    if users.iter().any(|u| u.email == form.email) {
        return Err(DbError::Duplicate(form.email));
    }
    let id = users.len() as u64 + 1;
    users.push(User { id, email: form.email, age: form.age, guardian: form.guardian });
    Ok(id)
}

pub fn find_by_email(this: &MemoryDb, email: &Email) -> Result<Option<User>, DbError> {
    let users = this.state.users.lock().map_err(|_| DbError::Unavailable)?;
    Ok(users.iter().find(|u| &u.email == email).cloned())
}
