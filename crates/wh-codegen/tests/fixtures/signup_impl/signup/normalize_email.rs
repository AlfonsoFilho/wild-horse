use crate::prelude::*;
use crate::signup::*;

pub fn normalize_email(e: &Email) -> Email {
    Email::parse(e.to_lowercase()).expect("lowercasing keeps an email valid")
}
