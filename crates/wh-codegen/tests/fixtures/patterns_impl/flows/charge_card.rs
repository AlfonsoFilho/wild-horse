use crate::prelude::*;
use crate::flows::*;

pub fn charge_card(cap_gateway: &impl Gateway, amount: i64) -> Result<String, PayError> {
    cap_gateway.charge(amount)
}
