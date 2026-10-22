use crate::prelude::*;
use crate::shop::*;

pub fn is_locked(d: &Door) -> bool {
    matches!(d, Door::Locked(_))
}
