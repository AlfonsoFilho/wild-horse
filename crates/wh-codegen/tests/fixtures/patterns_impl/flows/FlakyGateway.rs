use crate::prelude::*;
use crate::flows::*;

/// Internal state of `FlakyGateway`, created with `Default` by `FlakyGateway::new`.
#[derive(Default)]
pub struct State {
    calls: std::sync::atomic::AtomicI64,
}

pub fn charge(this: &FlakyGateway, amount: i64) -> Result<String, PayError> {
    let n = this.state.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    if n < this.failures {
        return Err(PayError::Timeout);
    }
    Ok(format!("paid {amount}"))
}
