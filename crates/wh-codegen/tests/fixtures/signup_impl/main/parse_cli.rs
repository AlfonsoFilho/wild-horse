use crate::prelude::*;
use crate::main::*;

pub fn parse_cli(args: &[String]) -> Result<SignupForm, AppError> {
    let [_, email, age] = args else {
        return Err(AppError::Usage("usage: signup_cli <email> <age>".into()));
    };
    let age = age.parse().map_err(|_| AppError::Usage(format!("not an age: {age}")))?;
    Ok(SignupForm { email: email.clone(), age, guardian: None })
}
