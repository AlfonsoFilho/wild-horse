use crate::prelude::*;
use crate::mail::*;

pub async fn notify(cap_mailer: &impl Mailer, to: &str, name: &str) -> Result<String, MailError> {
    cap_mailer.send(to, &format!("hello {name}")).await?;
    Ok(format!("sent to {to}"))
}
