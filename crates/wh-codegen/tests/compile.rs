//! End-to-end: generate the signup example against reference hole
//! implementations, then build and test the generated crate with cargo.

use std::path::{Path, PathBuf};
use std::process::Command;

use wh_model::{Model, SourceInput};

fn root() -> PathBuf {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../..")).canonicalize().unwrap()
}

fn generate(dir: &Path) {
    let src = root().join("examples/signup/src");
    let inputs: Vec<SourceInput> = ["signup.wh.md", "main.wh.md"]
        .iter()
        .map(|f| SourceInput { path: f.to_string(), text: std::fs::read_to_string(src.join(f)).unwrap() })
        .collect();
    let model = Model::build(&inputs);
    assert!(!model.has_errors());
    let opts = wh_codegen::GenOptions {
        package: "signup".into(),
        crate_name: "signup".into(),
        impl_root: root().join("crates/wh-codegen/tests/fixtures/signup_impl"),
        contracts_in_release: false,
    };
    let g = wh_codegen::generate(&model, &opts);
    wh_codegen::write(&g, dir).unwrap();
}

/// Both tests build packages named `signup` in one target dir; run cargo one at a time.
static CARGO_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn cargo(dir: &Path, args: &[&str]) -> std::process::Output {
    let _guard = CARGO_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    Command::new(env!("CARGO"))
        .args(args)
        .current_dir(dir)
        .env("CARGO_TARGET_DIR", root().join("target/wh-gen-tests"))
        .output()
        .unwrap()
}

#[test]
fn signup_generates_builds_and_passes() {
    let dir = root().join("target/wh-gen-tests/signup");
    generate(&dir);
    let out = cargo(&dir, &["test", "--quiet"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "generated tests failed:\n{stdout}\n{stderr}");
    assert!(stdout.contains("test result: ok"), "{stdout}");
    let out = cargo(&dir, &["run", "--quiet", "--bin", "signup_cli", "--", "Ann@Example.com", "30"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "registered user 1");
    // invalid input: the app reports the error, exits 1, and traces it
    let out = cargo(&dir, &["run", "--quiet", "--bin", "signup_cli", "--", "nope", "30"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("error: bad email: nope"), "{err}");
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            copy_dir(&p, &to.join(e.file_name()));
        } else {
            std::fs::copy(&p, to.join(e.file_name())).unwrap();
        }
    }
}

#[test]
fn contracts_catch_wrong_implementations() {
    let base = root().join("target/wh-gen-tests/broken");
    let impl_root = base.join("impl");
    let _ = std::fs::remove_dir_all(&impl_root);
    copy_dir(&root().join("crates/wh-codegen/tests/fixtures/signup_impl"), &impl_root);
    // violates `ensures result == e.lower()`
    std::fs::write(
        impl_root.join("signup/normalize_email.rs"),
        "use crate::prelude::*;\nuse crate::signup::*;\n\npub fn normalize_email(e: &Email) -> Email {\n    Email::parse(e.to_uppercase()).unwrap()\n}\n",
    )
    .unwrap();
    // violates `raises RegisterError.TooYoung when form.age < 13 and form.guardian is None`
    std::fs::write(
        impl_root.join("signup/check_age.rs"),
        "use crate::prelude::*;\nuse crate::signup::*;\n\npub fn check_age(form: ValidForm) -> Result<ValidForm, RegisterError> {\n    Ok(form)\n}\n",
    )
    .unwrap();
    let src = root().join("examples/signup/src");
    let inputs: Vec<SourceInput> = ["signup.wh.md", "main.wh.md"]
        .iter()
        .map(|f| SourceInput { path: f.to_string(), text: std::fs::read_to_string(src.join(f)).unwrap() })
        .collect();
    let model = Model::build(&inputs);
    let opts = wh_codegen::GenOptions { package: "signup_broken".into(), crate_name: "signup".into(), impl_root, contracts_in_release: false };
    let g = wh_codegen::generate(&model, &opts);
    let dir = base.join("gen");
    wh_codegen::write(&g, &dir).unwrap();
    let out = cargo(&dir, &["test", "--quiet", "--", "--test-threads=1"]);
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(!out.status.success(), "broken implementations passed:\n{text}");
    assert!(text.contains("ensures violated in signup.normalize_email"), "{text}");
    assert!(text.contains("raises violated in signup.check_age"), "{text}");
    // the failing tests map back to their nodes
    let failed: Vec<&str> = text.lines().filter_map(|l| l.strip_prefix("    ")).filter(|l| l.contains("wh_tests::")).collect();
    assert!(failed.iter().any(|t| g.manifest.node_for_test(t) == Some("signup.normalize_email")), "{failed:?}");
}

#[test]
fn unchanged_inputs_skip_generation() {
    let src = root().join("examples/signup/src");
    let read = |f: &str| std::fs::read_to_string(src.join(f)).unwrap();
    let inputs = vec![
        SourceInput { path: "signup.wh.md".into(), text: read("signup.wh.md") },
        SourceInput { path: "main.wh.md".into(), text: read("main.wh.md") },
    ];
    let model = Model::build(&inputs);
    let dir = root().join("target/wh-gen-tests/fingerprint");
    let _ = std::fs::remove_dir_all(&dir);
    let opts = wh_codegen::GenOptions {
        package: "fp".into(),
        crate_name: "fp".into(),
        impl_root: dir.join("impl"),
        contracts_in_release: false,
    };
    let (g1, reused) = wh_codegen::generate_cached(&model, &opts, &dir.join("gen")).unwrap();
    assert!(!reused && !g1.files.is_empty());
    let (g2, reused) = wh_codegen::generate_cached(&model, &opts, &dir.join("gen")).unwrap();
    assert!(reused && g2.files.is_empty(), "second run reuses the crate on disk");
    assert_eq!(g2.manifest.holes.len(), g1.manifest.holes.len());
    // a deleted hole stub is recreated even on reuse
    std::fs::remove_file(&g1.manifest.holes[0].path).unwrap();
    wh_codegen::generate_cached(&model, &opts, &dir.join("gen")).unwrap();
    assert!(g1.manifest.holes[0].path.exists());
    // any source change regenerates
    let mut edited = inputs.clone();
    edited[0].text.push_str("\n```wh\ndef extra(x: int) -> int\n```\n");
    let (g3, reused) = wh_codegen::generate_cached(&Model::build(&edited), &opts, &dir.join("gen")).unwrap();
    assert!(!reused && g3.manifest.holes.iter().any(|h| h.node == "signup.extra"));
}

fn patterns_model() -> Model {
    let src = root().join("crates/wh-codegen/tests/fixtures/patterns/src");
    let inputs: Vec<SourceInput> = ["shop.wh.md", "stock.wh.md", "flows.wh.md", "mail.wh.md"]
        .iter()
        .map(|f| SourceInput { path: f.to_string(), text: std::fs::read_to_string(src.join(f)).unwrap() })
        .collect();
    let model = Model::build(&inputs);
    let errs: Vec<String> = model
        .diagnostics
        .iter()
        .filter(|d| d.diag.severity <= wh_syntax::Severity::Warning)
        .map(|d| d.diag.message.clone())
        .collect();
    assert!(errs.is_empty(), "{errs:#?}");
    model
}

fn patterns_opts(package: &str) -> wh_codegen::GenOptions {
    wh_codegen::GenOptions {
        package: package.into(),
        crate_name: "shop".into(),
        impl_root: root().join("crates/wh-codegen/tests/fixtures/patterns_impl"),
        contracts_in_release: false,
    }
}

/// What machines and the pattern catalog contribute to the generated source.
#[test]
fn patterns_generate_expected_code() {
    let g = wh_codegen::generate(&patterns_model(), &patterns_opts("shop_snapshot"));
    let code = &g.files["src/modules/shop.rs"];
    for needle in [
        // machine: state enum, table, total step, a tracing event per transition
        "pub enum Door {\n    Open,\n    Closed,\n    Locked(String),\n}",
        "pub const STATES: &'static [&'static str] = &[\"Open\", \"Closed\", \"Locked\"];",
        "crate::wh_rt::Transition { id: \"shop.Door#Locked.Unlock[0]\", from: \"Locked\", event: \"Unlock\", to: \"Closed\", guard: Some(\"c == code\"), emit: Some(\"Unlocked\") },",
        "pub fn step(state: Door, event: DoorEvent) -> (Door, Option<DoorEffect>) {",
        "(Door::Locked(code), DoorEvent::Unlock(c)) if (c.clone() == code.clone()) => {",
        "tracing::debug!(wh.node = \"shop.Door#Closed.Lock[0]\", from = \"Closed\", event = \"Lock\", to = \"Locked\", \"transition\");",
        "(state, event) => {",
        // Builder
        "pub struct OrderBuilder {",
        "pub fn builder() -> OrderBuilder {",
        "None => return Err(InvalidValue::new(\"shop.Order\", \"field `item` is required\", String::new())),",
        // Memoize
        "static WH_MEMO: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<(u32,), u64>>>",
        // Deserialize goes through a shadow struct and wh_check
        "#[serde(try_from = \"WhDeSpan\")]",
        "impl TryFrom<WhDeSpan> for Span {",
        // a dependent strategy: `left` is computed from `target`
        "v.left = wh_x;",
    ] {
        assert!(code.contains(needle), "missing {needle:?} in generated code");
    }
    // requires become property preconditions
    assert!(code.contains("{ let wh_req1_n = ((n) as u32); proptest::prop_assume!((wh_req1_n <= 90u32)); }"), "fib's requires is assumed");
    // sensitive parameters and types are redacted
    assert!(code.contains("password = ?crate::wh_rt::Redacted"), "sensitive parameter is traced redacted");
    assert!(code.contains("impl std::fmt::Debug for Token {"), "sensitive type has a redacting Debug");
    assert!(code.contains("Err(InvalidValue::new(\"shop.Token\", Self::RULE, \"<redacted>\".to_string()))"));
    let flows = &g.files["src/modules/flows.rs"];
    for needle in [
        "let mut acc = 0i64;\n",
        "acc = add_line(acc, x);",
        "out.extend(expand(x))",
        "Err(e) if attempt < 3 => {",
        "std::thread::sleep(std::time::Duration::from_millis(1) * 2u32.pow(attempt - 1));",
        // sync parallel runs on scoped threads, so shared capabilities must be Sync
        "pub fn pay_both(cap_gateway: &(impl Gateway + Sync), a: i64, b: i64)",
        "std::thread::scope(|wh_scope| {",
    ] {
        assert!(flows.contains(needle), "missing {needle:?} in flows");
    }
    assert!(g.files["src/modules/mail.rs"].contains("crate::wh_rt::join(async {"), "async parallel joins futures");
    let stock = &g.files["src/modules/stock.rs"];
    assert!(stock.contains("Err(crate::shop::StoreError::Missing { .. })"), "contracts are emitted with qualified paths");
    for (test, node) in [
        ("wh__shop__Door__totality", "shop.Door"),
        ("wh__shop__Door__reachability", "shop.Door"),
        ("wh__shop__area__transform_total", "shop.area"),
        ("wh__shop__MemoryItems__repository_roundtrip", "shop.MemoryItems"),
        ("wh__shop__CappedItems__repository_roundtrip", "shop.CappedItems"),
        ("wh__shop__MemoryItems__contract__count", "shop.MemoryItems"),
        ("wh__shop__CappedItems__contract__rename", "shop.CappedItems"),
        // an adapter in another module gets the capability's tests too
        ("wh__stock__StockItems__contract__rename", "stock.StockItems"),
        ("wh__stock__StockItems__repository_roundtrip", "stock.StockItems"),
    ] {
        assert_eq!(g.manifest.tests.get(test).map(String::as_str), Some(node), "{test}");
    }
    // machines are fully generated: no hole
    assert!(!g.manifest.holes.iter().any(|h| h.node == "shop.Door"));
}

#[test]
fn patterns_build_and_pass() {
    let dir = root().join("target/wh-gen-tests/shop");
    let g = wh_codegen::generate(&patterns_model(), &patterns_opts("shop"));
    wh_codegen::write(&g, &dir).unwrap();
    let out = cargo(&dir, &["test"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "generated tests failed:\n{stdout}\n{stderr}");
    for t in g.manifest.tests.keys() {
        assert!(stdout.contains(&format!("{t} ... ok")), "{t} did not pass:\n{stdout}");
    }
    assert!(!stderr.contains("warning:"), "generated code has warnings:\n{stderr}");
    // the async app: block_on around an async entry flow
    let out = cargo(&dir, &["run", "--quiet", "--bin", "mailer", "--", "a@x.io"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let out = cargo(&dir, &["run", "--quiet", "--bin", "mailer", "--", "nope"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("error: rejected: nope"), "{err}");
}

/// Generate a fixture project (all `.wh.md` files of `src`) into `target/wh-gen-tests/<name>`.
fn gen_fixture(name: &str, src: &Path, impl_root: &Path) -> (PathBuf, wh_codegen::Generated) {
    let mut inputs = Vec::new();
    for dir in [src] {
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).collect();
        files.sort();
        for f in files.into_iter().filter(|f| f.to_string_lossy().ends_with(".wh.md")) {
            inputs.push(SourceInput { path: f.file_name().unwrap().to_string_lossy().into(), text: std::fs::read_to_string(&f).unwrap() });
        }
    }
    let model = Model::build(&inputs);
    let errs: Vec<String> = model.diagnostics.iter().filter(|d| d.diag.severity <= wh_syntax::Severity::Warning).map(|d| d.diag.message.clone()).collect();
    assert!(errs.is_empty(), "{errs:#?}");
    let opts = wh_codegen::GenOptions { package: name.into(), crate_name: name.into(), impl_root: impl_root.to_path_buf(), contracts_in_release: false };
    let g = wh_codegen::generate(&model, &opts);
    let dir = root().join("target/wh-gen-tests").join(name);
    wh_codegen::write(&g, &dir).unwrap();
    (dir, g)
}

fn fixtures() -> PathBuf {
    root().join("crates/wh-codegen/tests/fixtures")
}

fn http(addr: &str, request: &str) -> String {
    use std::io::{Read, Write};
    let mut s = std::net::TcpStream::connect(addr).unwrap();
    s.write_all(request.as_bytes()).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    out
}

#[test]
fn server_target_builds_serves_and_stops() {
    let (dir, g) = gen_fixture("store", &fixtures().join("server/src"), &fixtures().join("server_impl"));
    assert_eq!(g.manifest.targets["store.store_api"].written(), "server(axum)");
    let code = &g.files["src/modules/store.rs"];
    for needle in [
        "pub struct StoreApiServer<WhC0: Catalog + Send + Sync + 'static> {",
        ".route(\"/products/{sku}\", axum::routing::get(wh_store_api_route_1::<WhC0>).delete(wh_store_api_route_2::<WhC0>))",
        "store::StoreError::NotFound { .. } => (404, \"NotFound\"),",
        "store::StoreError::Exists { .. } => (409, \"Exists\"),",
        "store::ApiError::Store(inner) => wh_store_api_status_store_StoreError(inner),",
        "tracing::info_span!(\"wh.request\", wh.node = \"store.store_api\"",
        "pub async fn wh_store_api_shutdown() {",
        // wire types get serde, and invariants are checked when a request is read
        "#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]\n#[serde(try_from = \"WhDeProduct\")]\npub struct Product {",
    ] {
        assert!(code.contains(needle.replace("store::", "").as_str()), "missing {needle:?}");
    }
    assert!(g.files["Cargo.toml"].contains("axum = \"0.8\""));
    let out = cargo(&dir, &["test"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    for t in ["wh__store__store_api__route__POST__products", "wh__store__store_api__route__GET__products__sku", "wh__store__store_api__route__DELETE__products__sku", "wh__store__store_api__route__GET__search"] {
        assert!(stdout.contains(&format!("{t} ... ok")), "{t}:\n{stdout}");
    }
    // the real server: listen on a free port, answer, stop gracefully on SIGTERM
    let built = cargo(&dir, &["build", "--bin", "store_api"]);
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let exe = root().join("target/wh-gen-tests/debug/store_api");
    let mut child = Command::new(&exe)
        .env("WH_ADDR", "127.0.0.1:0")
        .env("WH_LOG", "off")
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = std::io::BufReader::new(child.stderr.take().unwrap());
    let mut line = String::new();
    std::io::BufRead::read_line(&mut stderr, &mut line).unwrap();
    let addr = line.trim().strip_prefix("listening on http://").unwrap_or_else(|| panic!("{line}")).to_string();
    let body = r#"{"sku":"ABC","name":"pen","price":3}"#;
    let res = http(&addr, &format!("POST /products HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()));
    assert!(res.starts_with("HTTP/1.1 200"), "{res}");
    assert!(res.ends_with(body), "{res}");
    let res = http(&addr, "GET /products/XYZ HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    assert!(res.starts_with("HTTP/1.1 404") && res.contains(r#""kind":"NotFound""#), "{res}");
    let res = http(&addr, "GET /search?prefix=A&limit=3 HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n");
    assert!(res.ends_with(&format!("[{body}]")), "{res}");
    let killed = Command::new("kill").args(["-TERM", &child.id().to_string()]).status().unwrap();
    assert!(killed.success());
    let status = child.wait().unwrap();
    assert!(status.success(), "the server stops gracefully on SIGTERM: {status}");
}

#[test]
fn web_target_exports_and_checks() {
    let (dir, g) = gen_fixture("calc", &fixtures().join("web/src"), &fixtures().join("web_impl"));
    assert_eq!(g.manifest.targets["calc.calc_web"].rust_target(), Some("wasm32-unknown-unknown"));
    let cargo_toml = &g.files["Cargo.toml"];
    assert!(cargo_toml.contains("crate-type = [\"cdylib\", \"rlib\"]") && cargo_toml.contains("wasm-bindgen = \"0.2\""), "{cargo_toml}");
    assert!(g.manifest.bins.is_empty(), "a web app has no binary");
    assert!(g.files["web/index.html"].contains("import init, * as wh from \"./pkg/calc.js\";"));
    let code = &g.files["src/modules/calc.rs"];
    assert!(code.contains("#[wasm_bindgen::prelude::wasm_bindgen(js_name = note_twice)]"));
    assert!(code.contains("pub fn wh_export_note_twice(input: &str) -> Result<String, String> {"));
    // exports are tested on the host through their JSON entry points
    let out = cargo(&dir, &["test"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    for t in ["divide", "shout", "remember", "note_twice"] {
        assert!(stdout.contains(&format!("wh__calc__calc_web__export__{t} ... ok")), "{t}:\n{stdout}");
    }
    let profile = &g.manifest.targets["calc.calc_web"];
    let target = profile.rust_target().unwrap();
    if !wh_targets::rust_target_installed(target) {
        eprintln!("skipping `cargo check --target {target}`: the target is not installed (`rustup target add {target}`)");
        return;
    }
    let out = cargo(&dir, &["check", "--lib", "--target", target]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    match wh_targets::wasm_bindgen_cli() {
        Some(v) => eprintln!("found {v}; `wasm-bindgen --target web --out-dir web/pkg` makes the page runnable"),
        None => eprintln!("wasm-bindgen CLI not installed: skipping the JS bindings"),
    }
}

#[test]
fn signup_server_example_serves_signup() {
    let ex = root().join("examples");
    let mut inputs = Vec::new();
    for f in ["signup_server/src/server.wh.md", "signup/src/signup.wh.md", "signup/src/main.wh.md"] {
        let path = f.rsplit('/').next().unwrap().to_string();
        inputs.push(SourceInput { path, text: std::fs::read_to_string(ex.join(f)).unwrap() });
    }
    let model = Model::build(&inputs);
    assert!(!model.has_errors());
    let opts = wh_codegen::GenOptions {
        package: "signup_server".into(),
        crate_name: "signup_server".into(),
        impl_root: ex.join("signup/impl"),
        contracts_in_release: false,
    };
    let g = wh_codegen::generate(&model, &opts);
    let dir = root().join("target/wh-gen-tests/signup_server");
    wh_codegen::write(&g, &dir).unwrap();
    // both apps of the project: the CLI and the server
    assert_eq!(g.manifest.bins.values().cloned().collect::<Vec<_>>(), ["signup_cli", "signup_api"]);
    let out = cargo(&dir, &["test"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("wh__server__signup_api__route__POST__signup ... ok"), "{stdout}");
}
