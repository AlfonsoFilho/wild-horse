use crate::prelude::*;
use crate::shop::*;
use crate::stock::*;

#[derive(Default)]
pub struct State {
    items: std::sync::Mutex<Vec<Item>>,
}

pub fn add(this: &StockItems, item: Item) -> Result<(), StoreError> {
    this.state.items.lock().unwrap().push(item);
    Ok(())
}

pub fn get(this: &StockItems, id: u64) -> Result<Option<Item>, StoreError> {
    Ok(this.state.items.lock().unwrap().iter().find(|i| i.id == id).cloned())
}

pub fn count(this: &StockItems) -> i64 {
    this.state.items.lock().unwrap().len() as i64
}

pub fn rename(this: &StockItems, id: u64, name: &Name) -> Result<(), StoreError> {
    let mut items = this.state.items.lock().unwrap();
    let item = items.iter_mut().find(|i| i.id == id).ok_or(StoreError::Missing)?;
    item.name = name.clone();
    Ok(())
}
