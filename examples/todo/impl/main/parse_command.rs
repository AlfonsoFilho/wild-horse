// wh:key=576c7deedfff7fc7
use crate::prelude::*;
use crate::main::*;

use tracing::{debug, trace};

const USAGE: &str = "todo <command>, where <command> is one of: add <title>, done <id>, remove <id>, list [--all], clear-done";

fn usage(problem: impl std::fmt::Display) -> AppError {
    AppError::Usage(format!("{problem}\n{USAGE}"))
}

fn parse_title(words: &[String]) -> Result<Title, AppError> {
    let joined = words.join(" ");
    let trimmed = joined.trim();
    if trimmed.is_empty() {
        return Err(usage("add needs a title"));
    }
    Title::parse(trimmed).map_err(|_| usage("a title must fit on one line and be at most 80 characters"))
}

fn parse_id(command: &str, rest: &[String]) -> Result<TaskId, AppError> {
    let [raw] = rest else {
        return Err(usage(format_args!("{command} needs exactly one task number")));
    };
    raw.trim()
        .parse::<u32>()
        .ok()
        .and_then(|n| TaskId::parse(n).ok())
        .ok_or_else(|| usage(format_args!("{raw:?} is not a positive task number")))
}

fn expect_no_args(command: &str, rest: &[String]) -> Result<(), AppError> {
    if rest.is_empty() {
        Ok(())
    } else {
        Err(usage(format_args!("{command} takes no arguments")))
    }
}

pub fn parse_command(args: &[String]) -> Result<Command, AppError> {
    let Some((command, rest)) = args.get(1..).and_then(<[String]>::split_first) else {
        debug!("no sub-command given");
        return Err(usage("missing command"));
    };
    trace!(command = %command, rest = ?rest, "parsing sub-command");
    match command.as_str() {
        "add" => Ok(Command::Add { title: parse_title(rest)? }),
        "done" => Ok(Command::Done { id: parse_id("done", rest)? }),
        "remove" => Ok(Command::Remove { id: parse_id("remove", rest)? }),
        "list" => match rest {
            [] => Ok(Command::List { include_done: false }),
            [flag] if flag == "--all" => Ok(Command::List { include_done: true }),
            _ => Err(usage("list accepts only --all")),
        },
        "clear-done" => {
            expect_no_args("clear-done", rest)?;
            Ok(Command::ClearDone)
        }
        other => Err(usage(format_args!("unknown command {other:?}"))),
    }
}
