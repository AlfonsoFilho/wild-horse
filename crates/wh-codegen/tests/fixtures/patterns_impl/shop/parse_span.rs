use crate::prelude::*;
use crate::shop::*;

pub fn parse_span(text: &str) -> Result<Span, ParseError> {
    serde_json::from_str(text).map_err(|e| ParseError::Bad(e.to_string()))
}
