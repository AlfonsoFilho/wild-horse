use crate::prelude::*;
use crate::shop::*;

pub fn big_order_needs_a_note() -> bool {
    let item = Sku::parse("ABC-123").unwrap();
    let big = Order::builder().item(item.clone()).id(2).qty(Qty::parse(500).unwrap());
    big.clone().build().is_err() && big.note("pallet".into()).build().is_ok()
}
