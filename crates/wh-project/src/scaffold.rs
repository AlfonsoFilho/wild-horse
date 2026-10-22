//! `wh new`: a minimal, literate starter project.

use std::path::Path;

use anyhow::{Result, bail};

pub const GITIGNORE: &str = "# Wild Horse: everything under .wh/ is derived, except the graph layout\n.wh/*\n!.wh/layout.json\n";

fn wildhorse_toml(name: &str) -> String {
    format!(
        r#"[project]
name = "{name}"
sources = ["src"]

[agent.compiler]
command = "npx"
args = ["-y", "@agentclientprotocol/claude-agent-acp"]

[agent.author]
command = "npx"
args = ["-y", "@agentclientprotocol/claude-agent-acp"]

[synthesis]
mode = "auto"          # auto | on_demand
debounce_ms = 3000
concurrency = 4
max_rounds = 4
"#
    )
}

fn main_wh_md(name: &str) -> String {
    format!(
        r#"---
module: main
---
# {name}

Describe here what the program is for. Prose in this document is part of the
program: it explains each definition to readers and to the agents that
implement and revise it.

## Output

Everything the program prints goes through a capability, so the pure parts
stay pure and tests can replace the console.

```wh
capability Console:
    def print_line(line: str) -> unit

adapter StdConsole implements Console:
    """Writes each line to standard output."""
```

## Greeting

Whom to greet: the first command-line argument, or the whole world.

```wh
def pick_name(args: list[str]) -> str:
    ensures len(result) > 0
    example pick_name(["prog"]) == "world"
    example pick_name(["prog", "Ada"]) == "Ada"
```

The greeting itself.

```wh
def greet(name: str) -> str:
    requires len(name) > 0
    ensures result.startswith("Hello, ")
    example greet("Ada") == "Hello, Ada!"
    pattern Transform
```

Printing it is the only effect.

```wh
def say(line: str) -> unit uses Console
```

## The application

```wh
app {name}:
    target native(cli)
    log level="warn" format="compact"
    provide Console = StdConsole()
    entry main(args: list[str]) -> unit:
        args |> pick_name |> greet |> say
```
"#
    )
}

/// Create a new project in `dir` (which must not exist or be empty).
pub fn new_project(dir: &Path, name: &str) -> Result<()> {
    if !name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        || !name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        bail!("`{name}` is not a valid project name: use snake_case (letters, digits, `_`)");
    }
    if dir.exists() && std::fs::read_dir(dir)?.next().is_some() {
        bail!("{} is not empty", dir.display());
    }
    std::fs::create_dir_all(dir.join("src"))?;
    std::fs::write(dir.join("wildhorse.toml"), wildhorse_toml(name))?;
    std::fs::write(dir.join("src/main.wh.md"), main_wh_md(name))?;
    std::fs::write(dir.join(".gitignore"), GITIGNORE)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn new_project_checks_clean() {
        let dir = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/wh-project-tests/new"));
        let _ = std::fs::remove_dir_all(dir);
        super::new_project(dir, "hello").unwrap();
        let p = crate::Project::load(dir).unwrap();
        let m = p.model().unwrap();
        let problems: Vec<String> = m
            .diagnostics
            .iter()
            .map(|d| d.diag.render(&m.files[d.file].path, &m.files[d.file].text))
            .collect();
        assert!(problems.is_empty(), "{}", problems.join("\n"));
        assert_eq!(m.nodes.values().filter(|n| n.is_hole()).count(), 3);
        assert!(super::new_project(dir, "hello").is_err(), "refuses a non-empty folder");
        assert!(super::new_project(&dir.join("x"), "Bad-Name").is_err());
    }
}
