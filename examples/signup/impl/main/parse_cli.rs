// wh:key=646b9568b2f04482 prose=af1349b9
use crate::prelude::*;
use crate::main::*;
use tracing::debug;

const USAGE: &str = "<program> <email> <age>";

pub fn parse_cli(args: &[String]) -> Result<SignupForm, AppError> {
    let [_program, email, age] = args else {
        debug!(argc = args.len(), "wrong number of arguments");
        return Err(AppError::Usage(format!(
            "expected exactly 2 arguments, got {}; usage: {USAGE}",
            args.len().saturating_sub(1)
        )));
    };

    let age: i64 = age.trim().parse().map_err(|_| {
        debug!(age_len = age.len(), "age is not an integer");
        AppError::Usage(format!("age must be an integer; usage: {USAGE}"))
    })?;

    debug!(email_len = email.len(), age, "parsed cli arguments");
    Ok(SignupForm {
        email: email.clone(),
        age,
        guardian: None,
    })
}
