use crate::prelude::*;
use crate::store::*;

/// Internal state of `MemoryCatalog`, created with `Default` by `MemoryCatalog::new`.
#[derive(Default)]
pub struct State {
    products: std::sync::Mutex<Vec<Product>>,
}

pub fn put(this: &MemoryCatalog, p: Product) -> Result<(), StoreError> {
    let mut ps = this.state.products.lock().unwrap();
    if ps.iter().any(|x| x.sku == p.sku) {
        return Err(StoreError::Exists(p.sku.to_string()));
    }
    ps.push(p);
    Ok(())
}

pub fn get(this: &MemoryCatalog, sku: &Sku) -> Result<Product, StoreError> {
    let ps = this.state.products.lock().unwrap();
    ps.iter().find(|x| &x.sku == sku).cloned().ok_or_else(|| StoreError::NotFound(sku.to_string()))
}

pub fn remove(this: &MemoryCatalog, sku: &Sku) -> Result<(), StoreError> {
    let mut ps = this.state.products.lock().unwrap();
    let before = ps.len();
    ps.retain(|x| &x.sku != sku);
    if ps.len() == before { Err(StoreError::NotFound(sku.to_string())) } else { Ok(()) }
}

pub fn all(this: &MemoryCatalog) -> Vec<Product> {
    this.state.products.lock().unwrap().clone()
}
