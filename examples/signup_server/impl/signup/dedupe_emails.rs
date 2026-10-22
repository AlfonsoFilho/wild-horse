// wh:key=522831f7d09cc6bd prose=69fe1421
use crate::prelude::*;
use crate::signup::*;

use std::collections::HashSet;

use tracing::debug;

pub fn dedupe_emails(emails: &[Email]) -> Vec<Email> {
    let mut seen: HashSet<&Email> = HashSet::with_capacity(emails.len());
    let result: Vec<Email> = emails
        .iter()
        .filter(|e| seen.insert(*e))
        .cloned()
        .collect();
    debug!(
        input = emails.len(),
        output = result.len(),
        dropped = emails.len() - result.len(),
        "deduplicated emails"
    );
    result
}
