use crate::prelude::*;
use crate::mail::*;

/// Internal state of `MemoryMailer`, created with `Default` by `MemoryMailer::new`.
#[derive(Default)]
pub struct State {
    sent: std::sync::Mutex<Vec<String>>,
}

pub async fn send(this: &MemoryMailer, to: &str, body: &str) -> Result<(), MailError> {
    if !to.contains('@') {
        return Err(MailError::Rejected(to.to_string()));
    }
    debug!(to, body, "sending");
    this.state.sent.lock().unwrap().push(to.to_string());
    Ok(())
}

pub fn sent(this: &MemoryMailer) -> Vec<String> {
    this.state.sent.lock().unwrap().clone()
}
