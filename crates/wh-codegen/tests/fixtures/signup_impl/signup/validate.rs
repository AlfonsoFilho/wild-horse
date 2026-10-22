use crate::prelude::*;
use crate::signup::*;

pub fn validate(form: &SignupForm) -> Result<ValidForm, ValidationError> {
    if form.email.to_lowercase().ends_with("@example.org") {
        return Err(ValidationError::DisposableEmail("example.org".into()));
    }
    let email = Email::parse(form.email.clone()).map_err(|_| ValidationError::BadEmail(form.email.clone()))?;
    if !(0..=150).contains(&form.age) {
        return Err(ValidationError::BadAge(form.age));
    }
    let age = Age::parse(form.age as u8).map_err(|_| ValidationError::BadAge(form.age))?;
    Ok(ValidForm { email, age, guardian: form.guardian })
}
