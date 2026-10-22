use crate::prelude::*;
use crate::flows::*;

pub fn expand(line: &Line) -> Vec<String> {
    vec![line.sku.clone(); line.qty as usize]
}
