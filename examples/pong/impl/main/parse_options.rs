// wh:key=554cf837ca688349 prose=4a1f232e
use crate::prelude::*;
use crate::main::*;
use tracing::{debug, trace};

const DEFAULT_TARGET: u8 = 5;

pub fn parse_options(args: &[String]) -> Result<Options, PongError> {
    let mut target = DEFAULT_TARGET;
    let mut rest = args.iter().skip(1);
    while let Some(arg) = rest.next() {
        trace!(%arg, "parsing argument");
        match arg.as_str() {
            "--to" => {
                let value = rest
                    .next()
                    .ok_or_else(|| PongError::Usage("--to requires a value".to_owned()))?;
                target = parse_target(value)?;
            }
            other => {
                return Err(PongError::Usage(format!("unknown argument `{other}`")));
            }
        }
    }
    debug!(target, "parsed options");
    Ok(Options { target })
}

fn parse_target(value: &str) -> Result<u8, PongError> {
    match value.parse::<u8>() {
        Ok(0) => Err(PongError::Usage("--to must be at least 1".to_owned())),
        Ok(n) => Ok(n),
        Err(_) => Err(PongError::Usage(format!(
            "--to expects a number from 1 to 255, got `{value}`"
        ))),
    }
}
