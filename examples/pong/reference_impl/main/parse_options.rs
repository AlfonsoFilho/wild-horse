use crate::prelude::*;
use crate::main::*;

const USAGE: &str = "usage: pong [--to N]   (N from 1 to 21, default 5)";

pub fn parse_options(args: &[String]) -> Result<Options, PongError> {
    match args.get(1..).unwrap_or_default() {
        [] => Ok(Options { target: 5 }),
        [flag, n] if flag == "--to" => match n.parse::<u8>() {
            Ok(target @ 1..=21) => Ok(Options { target }),
            _ => Err(PongError::Usage(format!("not a target score: {n}\n{USAGE}"))),
        },
        _ => Err(PongError::Usage(USAGE.to_string())),
    }
}
