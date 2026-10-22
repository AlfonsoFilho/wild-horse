use crate::prelude::*;
use crate::store::*;

pub fn search(cap_catalog: &impl Catalog, prefix: &str, limit: u8) -> Vec<Product> {
    cap_catalog.all().into_iter().filter(|p| p.sku.starts_with(prefix)).take(limit as usize).collect()
}
