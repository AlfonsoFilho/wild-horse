use crate::prelude::*;
use crate::store::*;

pub fn find_product(cap_catalog: &impl Catalog, sku: &Sku) -> Result<Product, StoreError> {
    cap_catalog.get(sku)
}
