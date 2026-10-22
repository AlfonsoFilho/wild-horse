//! `wh-mcp`: the `wh` MCP server over stdio.
//!
//! ```text
//! wh-mcp [--project <dir>]     file mode: edit the project's sources on disk
//! wh-mcp --bridge <socket>     bridge mode: forward every tool call to the IDE
//! ```

fn main() -> std::process::ExitCode {
    wh_mcp::cli_main(std::env::args().skip(1))
}
