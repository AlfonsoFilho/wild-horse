use crate::prelude::*;
use crate::mail::*;

pub fn first_arg(args: &[String]) -> String {
    args.get(1).cloned().unwrap_or_default()
}
