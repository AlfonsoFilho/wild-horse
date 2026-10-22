use crate::prelude::*;
use crate::signup::*;

pub fn normalize_form(form: ValidForm) -> ValidForm {
    ValidForm { email: normalize_email(&form.email), ..form }
}
