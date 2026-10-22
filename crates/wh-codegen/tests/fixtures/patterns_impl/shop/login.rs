use crate::prelude::*;
use crate::shop::*;

pub fn login(user: &str, password: &str) -> bool {
    user == "ann" && password == "hunter22"
}
