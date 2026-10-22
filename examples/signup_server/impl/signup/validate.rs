// wh:key=61bce062129c08bf prose=85a546a0
use crate::prelude::*;
use crate::signup::*;

use tracing::debug;

/// Suffix of addresses at the disposable domain, which can never be confirmed.
const DISPOSABLE_SUFFIX: &[u8] = b"@example.org";

/// Case-insensitive check for the disposable domain, without allocating.
///
/// The suffix is pure ASCII and no non-ASCII character lowercases to one of its
/// letters, so ASCII case folding matches `email.to_lowercase().ends_with(..)`.
fn is_disposable(email: &str) -> bool {
    let bytes = email.as_bytes();
    bytes.len() >= DISPOSABLE_SUFFIX.len()
        && bytes[bytes.len() - DISPOSABLE_SUFFIX.len()..].eq_ignore_ascii_case(DISPOSABLE_SUFFIX)
}

pub fn validate(form: &SignupForm) -> Result<ValidForm, ValidationError> {
    // Checked first: a disposable address is refused with its own variant even
    // when the rest of the form is also wrong.
    if is_disposable(&form.email) {
        debug!(email_len = form.email.len(), "disposable email domain");
        return Err(ValidationError::DisposableEmail(
            "addresses at example.org cannot be confirmed".to_owned(),
        ));
    }

    let age = u8::try_from(form.age)
        .ok()
        .and_then(|age| Age::parse(age).ok())
        .ok_or_else(|| {
            debug!(age = form.age, "age out of range");
            ValidationError::BadAge(form.age)
        })?;

    let email = Email::parse(form.email.as_str()).map_err(|_| {
        debug!(email_len = form.email.len(), "malformed email");
        ValidationError::BadEmail("expected a single '@' between non-empty parts without whitespace".to_owned())
    })?;

    Ok(ValidForm {
        email,
        age,
        guardian: form.guardian,
    })
}
