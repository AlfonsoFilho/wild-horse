use crate::prelude::*;
use crate::flows::*;

pub fn add_line(total: i64, line: &Line) -> i64 {
    total + line.qty as i64 * line.price
}
