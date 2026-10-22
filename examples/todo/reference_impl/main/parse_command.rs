use crate::prelude::*;
use crate::main::*;

const USAGE: &str = "todo add <title> | done <id> | remove <id> | list [--all] | clear-done";

fn usage(why: impl std::fmt::Display) -> AppError {
    AppError::Usage(format!("{why}\n{USAGE}"))
}

fn task_id(rest: &[String]) -> Result<TaskId, AppError> {
    let [n] = rest else { return Err(usage("expected exactly one task number")) };
    n.parse::<u32>().ok().and_then(|n| TaskId::parse(n).ok()).ok_or_else(|| usage(format!("not a task number: {n}")))
}

pub fn parse_command(args: &[String]) -> Result<Command, AppError> {
    let Some((cmd, rest)) = args.get(1..).and_then(|a| a.split_first()) else {
        return Err(usage("missing command"));
    };
    match cmd.as_str() {
        "add" => {
            let words: Vec<&str> = rest.iter().flat_map(|w| w.split_whitespace()).collect();
            let title = Title::parse(words.join(" ")).map_err(|_| usage("a title must be 1 to 80 characters"))?;
            Ok(Command::Add { title })
        }
        "done" => Ok(Command::Done { id: task_id(rest)? }),
        "remove" => Ok(Command::Remove { id: task_id(rest)? }),
        "list" => match rest {
            [] => Ok(Command::List { include_done: false }),
            [flag] if flag == "--all" => Ok(Command::List { include_done: true }),
            _ => Err(usage("list takes only --all")),
        },
        "clear-done" if rest.is_empty() => Ok(Command::ClearDone),
        other => Err(usage(format!("unknown command: {other}"))),
    }
}
