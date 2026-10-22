use crate::prelude::*;
use crate::store::*;

pub fn delete_product(cap_catalog: &impl Catalog, sku: &Sku) -> Result<(), StoreError> {
    cap_catalog.remove(sku)
}
