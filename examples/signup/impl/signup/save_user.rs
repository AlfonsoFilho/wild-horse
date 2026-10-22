// wh:key=9ebbad1f793b2eed prose=af1349b9
use crate::prelude::*;
use crate::signup::*;

pub fn save_user(cap_db: &impl Db, form: ValidForm) -> Result<u64, DbError> {
    tracing::debug!("delegating user insert to Db");
    let id = cap_db.insert(form)?;
    tracing::trace!(user_id = id, "user inserted");
    Ok(id)
}
