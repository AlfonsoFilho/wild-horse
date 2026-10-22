// wh:key=6398b79fc0877650 prose=af1349b9
use crate::prelude::*;
use crate::signup::*;

pub fn normalize_form(form: ValidForm) -> ValidForm {
    let email = normalize_email(&form.email);
    trace!(has_guardian = form.guardian.is_some(), "normalized form email");
    ValidForm { email, ..form }
}
