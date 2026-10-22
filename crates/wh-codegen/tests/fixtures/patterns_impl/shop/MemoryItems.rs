use crate::prelude::*;
use crate::shop::*;

#[derive(Default)]
pub struct State {
    items: std::sync::Mutex<Vec<Item>>,
}

pub fn add(this: &MemoryItems, item: Item) -> Result<(), StoreError> {
    this.state.items.lock().unwrap().push(item);
    Ok(())
}

pub fn get(this: &MemoryItems, id: u64) -> Result<Option<Item>, StoreError> {
    Ok(this.state.items.lock().unwrap().iter().find(|i| i.id == id).cloned())
}

pub fn count(this: &MemoryItems) -> i64 {
    this.state.items.lock().unwrap().len() as i64
}

pub fn rename(this: &MemoryItems, id: u64, name: &Name) -> Result<(), StoreError> {
    let mut items = this.state.items.lock().unwrap();
    let item = items.iter_mut().find(|i| i.id == id).ok_or(StoreError::Missing)?;
    item.name = name.clone();
    Ok(())
}
