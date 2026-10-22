# wh-targets

The target profiles of spec §11, as plain data: `native(cli)`, `server(axum)`, `web(wasm)`.

## Why it is a crate of its own

Three layers need the same description of a target:
- the checker (`wh-model`): adapter availability, and per-target rules such as no threads on `web`;
- the code generator (`wh-codegen`): dependencies, `crate-type`, the entry point;
- the build and run tools (`wh-build`, `wh-project`, `wh-ide`): cargo arguments and the runner.

`wh-model` cannot depend on `wh-codegen`, and the build tools should not need the code generator to learn how to run an app. A leaf crate that depends only on `serde` lets all of them share one definition without a dependency cycle.

It does not duplicate anything. The other `Target` enums in the workspace are unrelated:
- `wh-perf::spec::Target`: optimize targets such as `speed` and `latency`
- `wh-mcp::Target`: MCP edit targets
- `wh-ide::markdown::Target`: link targets in rendered prose

## API

- `TargetProfile::parse(kind, flavor)` and `native_cli()`
- `label()` / `written()`
- `rust_target()`, `entry()`, `has_binary()`, `lib_crate_types()`
- `dependencies()` / `dev_dependencies()`
- `cargo_build_args(release)`, `cargo_test_args()`
- `runner()`: `Exec`, `Serve { addr_env, default_addr }` or `WasmBindgen { out_dir }`
- `supports_threads()`, `supports_async()`
- `adapter_available(labels, in_memory)`
- Tool detection only (nothing is ever installed): `installed_rust_targets()`, `rust_target_installed(t)`, `wasm_bindgen_cli()`, `wasm_bindgen_command(wasm, out)`

`wh_codegen::Manifest.targets` records each app's profile, so tools can pick the runner without re-checking the model.

## Not wired yet (owners of those crates)

- `wh-project::run_app` runs every binary as `Runner::Exec`. A server app should be started, its `listening on http://<addr>` line shown, and the app stopped with SIGTERM.
- `wh-build` should build web apps with `cargo_build_args(..)` (`--lib --target wasm32-unknown-unknown`), then run `wasm_bindgen_command` only when `wasm_bindgen_cli()` finds the CLI.
