use crate::prelude::*;
use crate::signup::*;

pub fn dedupe_emails(emails: &[Email]) -> Vec<Email> {
    let mut seen = std::collections::HashSet::with_capacity(emails.len());
    emails.iter().filter(|e| seen.insert(*e)).cloned().collect()
}
