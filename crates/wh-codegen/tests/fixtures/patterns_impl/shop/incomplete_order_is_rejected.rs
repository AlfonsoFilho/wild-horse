use crate::prelude::*;
use crate::shop::*;

pub fn incomplete_order_is_rejected() -> bool {
    Order::builder().id(1).build().is_err_and(|e| e.rule == "field `item` is required")
}
