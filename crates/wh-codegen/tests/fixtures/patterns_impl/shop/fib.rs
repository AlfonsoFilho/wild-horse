use crate::prelude::*;
use crate::shop::*;

/// Deliberately exponential without the cache.
pub fn fib(n: u32) -> u64 {
    if n < 2 { n as u64 } else { crate::shop::fib(n - 1) + crate::shop::fib(n - 2) }
}
