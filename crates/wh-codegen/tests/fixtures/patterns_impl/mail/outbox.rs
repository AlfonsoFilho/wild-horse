use crate::prelude::*;
use crate::mail::*;

pub fn outbox(cap_mailer: &impl Mailer) -> Vec<String> {
    cap_mailer.sent()
}
