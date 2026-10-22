// wh:key=de9424eebe7af8d0 prose=e7deb0bc
use crate::prelude::*;
use crate::signup::*;

pub fn normalize_email(e: &Email) -> Email {
    let lowered = e.as_inner().to_lowercase();
    trace!(len = lowered.len(), "normalized email");
    // Lowercasing never introduces `@` or whitespace, so the refinement still holds.
    Email::parse(lowered).expect("lowercased email keeps the Email shape")
}
