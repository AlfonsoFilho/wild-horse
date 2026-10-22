use crate::prelude::*;
use crate::signup::*;

pub fn save_user(cap_db: &impl Db, form: ValidForm) -> Result<u64, DbError> {
    cap_db.insert(form)
}
