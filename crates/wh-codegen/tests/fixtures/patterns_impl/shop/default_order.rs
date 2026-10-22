use crate::prelude::*;
use crate::shop::*;

pub fn default_order(item: &Sku) -> Order {
    Order::builder().id(1).item(item.clone()).build().unwrap()
}
