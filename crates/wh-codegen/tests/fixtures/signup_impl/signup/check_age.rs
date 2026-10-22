use crate::prelude::*;
use crate::signup::*;

pub fn check_age(form: ValidForm) -> Result<ValidForm, RegisterError> {
    if *form.age < 13 && form.guardian.is_none() {
        return Err(RegisterError::TooYoung);
    }
    Ok(form)
}
