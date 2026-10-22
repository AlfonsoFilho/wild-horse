use crate::prelude::*;
use crate::calc::*;

pub fn divide(r: &Ratio) -> Result<i64, CalcError> {
    if r.den == 0 { Err(CalcError::DivByZero) } else { Ok(r.num.div_euclid(r.den)) }
}
