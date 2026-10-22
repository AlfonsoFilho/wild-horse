// wh:key=11db6d1350fcad8c prose=669353ef
use crate::prelude::*;
use crate::signup::*;

use tracing::{debug, trace};

/// Age below which a guardian is required.
const MIN_UNSUPERVISED_AGE: u8 = 13;

pub fn check_age(form: ValidForm) -> Result<ValidForm, RegisterError> {
    let age = *form.age;
    if age < MIN_UNSUPERVISED_AGE && form.guardian.is_none() {
        debug!(age, "minor without guardian");
        return Err(RegisterError::TooYoung);
    }
    trace!(age, has_guardian = form.guardian.is_some(), "age accepted");
    Ok(form)
}
