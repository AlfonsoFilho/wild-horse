use crate::prelude::*;
use crate::store::*;

pub fn add_product(cap_catalog: &impl Catalog, p: Product) -> Result<Product, ApiError> {
    if p.price == 0 {
        return Err(ApiError::BadPrice);
    }
    cap_catalog.put(p.clone())?;
    Ok(p)
}
