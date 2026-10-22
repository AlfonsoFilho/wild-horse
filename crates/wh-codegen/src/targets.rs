//! Code for the non-native target profiles (spec §11): `server(axum)` apps
//! (state, router, handlers, error statuses, tracing middleware, graceful
//! shutdown, `main`, route tests) and `web(wasm)` apps (JSON exports with
//! `wasm-bindgen` wrappers, export tests, `web/index.html`).

use std::collections::BTreeSet;
use std::fmt::Write as _;

use wh_model::{AppInfo, Model, Node, NodeKind, ParamInfo, RouteSource, SigInfo, Ty};
use wh_syntax::ast::{ItemKind, ParamMode};
use wh_targets::{TargetKind, TargetProfile};

use crate::expr::{ExprGen, Scope};
use crate::items::{ModuleGen, esc};
use crate::rust::{Names, cap_var, rust_keyword_safe, test_prefix};
use crate::tests::TestGen;

/// Every app with its target profile.
pub fn apps(model: &Model) -> Vec<(&Node, &AppInfo, TargetProfile)> {
    model
        .nodes
        .values()
        .filter_map(|n| match &n.kind {
            NodeKind::App(a) => Some((n, a, a.profile())),
            _ => None,
        })
        .collect()
}

/// Types whose values cross a route or an export as JSON (they get serde
/// derives), with everything they contain.
pub fn wire_types(model: &Model) -> BTreeSet<String> {
    let mut stack: Vec<Ty> = Vec::new();
    for (_, a, _) in apps(model) {
        let targets = a.routes.iter().map(|r| &r.target).chain(&a.exports);
        for id in targets {
            if let Some(sig) = model.nodes.get(id).and_then(|n| n.sig()) {
                stack.extend(sig.params.iter().map(|p| p.ty.clone()));
                stack.push(sig.ret.clone());
            }
        }
    }
    let mut out = BTreeSet::new();
    while let Some(t) = stack.pop() {
        let mut named = Vec::new();
        t.walk_named(&mut |n| named.push(n.to_string()));
        for id in named {
            if !out.insert(id.clone()) {
                continue;
            }
            match model.nodes.get(&id).map(|n| &n.kind) {
                Some(NodeKind::Struct(s)) => stack.extend(s.fields.iter().map(|f| f.ty.clone())),
                Some(NodeKind::Refined { base, .. }) => stack.push(base.clone()),
                Some(k @ (NodeKind::Enum(_) | NodeKind::Machine(_))) => {
                    stack.extend(k.enum_like().unwrap().variants.iter().flat_map(|v| v.items.iter().map(|i| i.ty.clone())))
                }
                _ => {}
            }
        }
    }
    out
}

pub fn is_wire_type(model: &Model, id: &str) -> bool {
    // the set is small and cheap to compute; recomputing keeps codegen stateless
    model.nodes.values().any(|n| matches!(&n.kind, NodeKind::App(a) if !a.routes.is_empty() || !a.exports.is_empty()))
        && wire_types(model).contains(id)
}

fn pascal(name: &str) -> String {
    name.split('_').filter(|s| !s.is_empty()).map(|s| s[..1].to_uppercase() + &s[1..]).collect()
}

/// `register(&st.cap_db, &form)` for a flow/def called with owned values
/// named after its parameters; `state` holds the capabilities.
fn call_code(names: &Names, eg: &ExprGen, id: &str, sig: &SigInfo, state: &str) -> String {
    let mut args: Vec<String> = sig.uses.iter().map(|c| format!("&{state}.{}", cap_var(c))).collect();
    for p in &sig.params {
        args.push(eg.pass(rust_keyword_safe(&p.name), p));
    }
    let call = format!("{}({})", names.path(id), args.join(", "));
    if sig.is_async { format!("crate::wh_rt::block_on({call})") } else { call }
}

/// `#[derive] pub struct Name { … }` for a group of parameters.
fn params_struct(names: &Names, name: &str, params: &[&ParamInfo]) -> String {
    let fields: Vec<String> = params.iter().map(|p| format!("    pub {}: {},", rust_keyword_safe(&p.name), names.ty(&p.ty))).collect();
    format!("#[doc(hidden)]\n#[derive(serde::Serialize, serde::Deserialize)]\npub struct {name} {{\n{}\n}}\n\n", fields.join("\n"))
}

/// `E::V { .. }`-style patterns of an error enum's variants, binding the
/// single item of `from` variants as `inner`.
fn status_arms(model: &Model, names: &Names, app: &AppInfo, err: &str, fns: &BTreeSet<String>, prefix: &str) -> String {
    let Some(NodeKind::Enum(e)) = model.nodes.get(err).map(|n| &n.kind) else { return "        _ => (500, \"\"),\n".into() };
    let path = names.path(err);
    let whole = app.statuses.iter().find(|s| s.error.named_id() == Some(err) && s.variant.is_none()).map(|s| s.code);
    let mut arms = String::new();
    for v in &e.variants {
        let explicit = app.statuses.iter().find(|s| s.error.named_id() == Some(err) && s.variant.as_deref() == Some(&v.name)).map(|s| s.code);
        let inner = match v.items.as_slice() {
            [one] if one.is_from => one.ty.named_id().filter(|t| fns.contains(*t)).map(|t| (t.to_string(), one.name.clone())),
            _ => None,
        };
        let kind = esc(&v.name);
        let arm = match (explicit.or(whole), inner) {
            (Some(code), _) => format!("{path}::{} {{ .. }} => ({code}, {kind}),", v.name),
            (None, Some((t, field))) => {
                let pat = match field {
                    Some(f) => format!("{path}::{} {{ {}: inner }}", v.name, rust_keyword_safe(&f)),
                    None => format!("{path}::{}(inner)", v.name),
                };
                format!("{pat} => {prefix}_status_{}(inner),", t.replace('.', "_"))
            }
            (None, None) => format!("{path}::{} {{ .. }} => (500, {kind}),", v.name),
        };
        let _ = writeln!(arms, "        {arm}");
    }
    arms
}

/// Error types a server app reports: those its routes raise, and the error
/// enums wrapped by their `from` variants.
fn error_types(model: &Model, app: &AppInfo) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut stack: Vec<String> = app
        .routes
        .iter()
        .filter_map(|r| model.nodes.get(&r.target)?.sig()?.raises.as_ref()?.named_id().map(String::from))
        .collect();
    while let Some(id) = stack.pop() {
        let Some(NodeKind::Enum(e)) = model.nodes.get(&id).map(|n| &n.kind) else { continue };
        if !out.insert(id.clone()) {
            continue;
        }
        for v in &e.variants {
            if let [one] = v.items.as_slice() {
                if one.is_from {
                    stack.extend(one.ty.named_id().map(String::from));
                }
            }
        }
    }
    out
}

impl<'m> ModuleGen<'m> {
    fn provide_exprs(&self, n: &Node) -> Vec<String> {
        let ItemKind::App(ast_app) = &n.item.kind else { return vec![] };
        let eg = self.eg();
        ast_app.provides.iter().map(|p| eg.emit(&p.value, &Scope::default())).collect()
    }

    /// A `server(axum)` app: its state (one field per provided capability,
    /// generic so tests can substitute adapters), router, handlers, error
    /// statuses, tracing middleware and shutdown signal.
    pub(crate) fn server_app(&mut self, n: &Node, a: &AppInfo) {
        let names = self.names();
        let eg = self.eg();
        let app = rust_keyword_safe(&n.name);
        let st = format!("{}Server", pascal(&n.name));
        let gens: Vec<String> = (0..a.provides.len()).map(|i| format!("WhC{i}")).collect();
        let bounds: Vec<String> = a
            .provides
            .iter()
            .zip(&gens)
            .map(|(p, g)| format!("{g}: {} + Send + Sync + 'static", names.path(&p.capability)))
            .collect();
        let (decl, uses) = if gens.is_empty() {
            (String::new(), String::new())
        } else {
            (format!("<{}>", bounds.join(", ")), format!("<{}>", gens.join(", ")))
        };
        let mut out = String::new();
        let fields: Vec<String> = a.provides.iter().zip(&gens).map(|(p, g)| format!("    pub {}: {g},", cap_var(&p.capability))).collect();
        let _ = writeln!(
            out,
            "/// State of the `{}` server: the capabilities its routes use.\npub struct {st}{decl} {{\n{}\n}}\n",
            n.name,
            fields.join("\n")
        );
        let concrete: Vec<String> = a.provides.iter().filter_map(|p| p.adapter.as_ref().map(|ad| names.path(ad))).collect();
        let inits: Vec<String> =
            a.provides.iter().zip(self.provide_exprs(n)).map(|(p, e)| format!("{}: {e}", cap_var(&p.capability))).collect();
        let concrete = if concrete.is_empty() { String::new() } else { format!("<{}>", concrete.join(", ")) };
        let _ = writeln!(
            out,
            "impl {st}{concrete} {{\n    /// The adapters the app `provide`s.\n    pub fn new() -> Self {{\n        Self {{ {} }}\n    }}\n}}\n",
            inits.join(", ")
        );
        // error statuses
        let errs = error_types(self.model, a);
        let prefix = format!("wh_{}", n.name);
        for e in &errs {
            let ident = e.replace('.', "_");
            let ty = names.path(e);
            let arms = status_arms(self.model, &names, a, e, &errs, &prefix);
            let msg = if self.model.nodes[e].is_error_type { "format!(\"{e}\")" } else { "format!(\"{e:?}\")" };
            let _ = writeln!(
                out,
                "/// HTTP status and variant name of a `{}`: its `status` rule, else that of the error it wraps, else 500.\npub fn {prefix}_status_{ident}(e: &{ty}) -> (u16, &'static str) {{\n    #[allow(unreachable_patterns)]\n    match e {{\n{arms}    }}\n}}\n\nfn {prefix}_error_{ident}(e: &{ty}) -> axum::response::Response {{\n    use axum::response::IntoResponse;\n    let (code, kind) = {prefix}_status_{ident}(e);\n    tracing::debug!(error = ?e, status = code, \"request failed\");\n    let status = axum::http::StatusCode::from_u16(code).unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR);\n    (status, axum::Json(serde_json::json!({{ \"error\": {msg}, \"kind\": kind }}))).into_response()\n}}\n",
                self.model.nodes[e].name
            );
        }
        // handlers
        let mut by_path: Vec<(String, Vec<String>)> = Vec::new();
        for (i, r) in a.routes.iter().enumerate() {
            let Some(sig) = self.model.nodes.get(&r.target).and_then(|t| t.sig()).cloned() else { continue };
            let of = |src: RouteSource| -> Vec<&ParamInfo> {
                sig.params.iter().zip(&r.params).filter(|(_, (_, s))| *s == src).map(|(p, _)| p).collect()
            };
            let (path_ps, query_ps, body_ps) = (of(RouteSource::Path), of(RouteSource::Query), of(RouteSource::Body));
            let mut extract = vec![format!("axum::extract::State(wh_state): axum::extract::State<std::sync::Arc<{st}{}>>", uses_or(&gens))];
            let mut binds = String::new();
            if !path_ps.is_empty() {
                out.push_str(&params_struct(&names, &format!("{st}Path{i}"), &path_ps));
                extract.push(format!("axum::extract::Path(wh_path): axum::extract::Path<{st}Path{i}>"));
                for p in &path_ps {
                    let _ = writeln!(binds, "        let {0} = wh_path.{0};", rust_keyword_safe(&p.name));
                }
            }
            if !query_ps.is_empty() {
                out.push_str(&params_struct(&names, &format!("{st}Query{i}"), &query_ps));
                extract.push(format!("axum::extract::Query(wh_query): axum::extract::Query<{st}Query{i}>"));
                for p in &query_ps {
                    let _ = writeln!(binds, "        let {0} = wh_query.{0};", rust_keyword_safe(&p.name));
                }
            }
            match body_ps.as_slice() {
                [] => {}
                [one] => {
                    extract.push(format!("axum::Json(wh_body): axum::Json<{}>", names.ty(&one.ty)));
                    let _ = writeln!(binds, "        let {} = wh_body;", rust_keyword_safe(&one.name));
                }
                many => {
                    out.push_str(&params_struct(&names, &format!("{st}Body{i}"), many));
                    extract.push(format!("axum::Json(wh_body): axum::Json<{st}Body{i}>"));
                    for p in many {
                        let _ = writeln!(binds, "        let {0} = wh_body.{0};", rust_keyword_safe(&p.name));
                    }
                }
            }
            for p in sig.params.iter().filter(|p| p.mode == ParamMode::Mut) {
                let _ = writeln!(binds, "        let mut {0} = {0};", rust_keyword_safe(&p.name));
            }
            let call = call_code(&names, &eg, &r.target, &sig, "wh_state");
            let success = if sig.ret == Ty::Unit {
                "axum::http::StatusCode::NO_CONTENT.into_response()".to_string()
            } else {
                "(axum::http::StatusCode::OK, axum::Json(v)).into_response()".to_string()
            };
            let outcome = match sig.raises.as_ref().and_then(|e| e.named_id()) {
                Some(e) if errs.contains(e) => {
                    format!("match wh_out {{\n            Ok(v) => {success},\n            Err(e) => {prefix}_error_{}(&e),\n        }}", e.replace('.', "_"))
                }
                Some(_) => format!(
                    "match wh_out {{\n            Ok(v) => {success},\n            Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, axum::Json(serde_json::json!({{ \"error\": format!(\"{{e:?}}\") }}))).into_response(),\n        }}"
                ),
                None => format!("{{\n            let v = wh_out;\n            {success}\n        }}"),
            };
            let handler = format!("wh_{}_route_{i}", n.name);
            let _ = writeln!(
                out,
                "/// `{} {}` → `{}`\nasync fn {handler}{decl}({}) -> axum::response::Response {{\n    use axum::response::IntoResponse;\n    // flows are synchronous (async ones are driven by `block_on`): run them off the async workers\n    let wh_span = tracing::Span::current();\n    let wh_result = tokio::task::spawn_blocking(move || {{\n        let _wh_in = wh_span.enter();\n{binds}        {call}\n    }})\n    .await;\n    match wh_result {{\n        Ok(wh_out) => {outcome},\n        Err(e) => {{\n            tracing::error!(error = %e, \"route failed\");\n            (axum::http::StatusCode::INTERNAL_SERVER_ERROR, axum::Json(serde_json::json!({{ \"error\": \"internal error\" }}))).into_response()\n        }}\n    }}\n}}\n",
                r.method,
                r.path,
                r.target,
                extract.join(", ")
            );
            let method = r.method.to_lowercase();
            let turbofish = if gens.is_empty() { String::new() } else { format!("::{uses}") };
            let h = format!("{method}({handler}{turbofish})");
            match by_path.iter_mut().find(|(p, _)| p == &r.path) {
                Some((_, hs)) => hs.push(h),
                None => by_path.push((r.path.clone(), vec![h])),
            }
        }
        let mut router = String::from("    axum::Router::new()\n");
        for (path, hs) in &by_path {
            let chain = hs.iter().skip(1).fold(format!("axum::routing::{}", hs[0]), |acc, h| format!("{acc}.{h}"));
            let _ = writeln!(router, "        .route({}, {chain})", esc(path));
        }
        let _ = writeln!(
            out,
            "/// The `{name}` app's routes, with request tracing.\npub fn {app}_router{decl}(state: std::sync::Arc<{st}{u}>) -> axum::Router {{\n{router}        .with_state(state)\n        .layer(axum::middleware::from_fn(wh_{name}_trace))\n}}\n\n/// One `wh.request` span per request, carrying the app's node id and the response status.\nasync fn wh_{name}_trace(req: axum::extract::Request, next: axum::middleware::Next) -> axum::response::Response {{\n    use tracing::Instrument;\n    let span = tracing::info_span!(\"wh.request\", wh.node = {id}, method = %req.method(), path = %req.uri().path(), status = tracing::field::Empty);\n    let res = next.run(req).instrument(span.clone()).await;\n    span.record(\"status\", res.status().as_u16());\n    res\n}}\n\n/// Resolves on Ctrl-C or SIGTERM: the server then stops accepting connections and finishes the open ones.\npub async fn wh_{name}_shutdown() {{\n    let ctrl_c = async {{\n        let _ = tokio::signal::ctrl_c().await;\n    }};\n    #[cfg(unix)]\n    let terminate = async {{\n        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {{\n            s.recv().await;\n        }}\n    }};\n    #[cfg(not(unix))]\n    let terminate = std::future::pending::<()>();\n    tokio::select! {{\n        _ = ctrl_c => {{}}\n        _ = terminate => {{}}\n    }}\n    tracing::info!(wh.node = {id}, \"shutting down\");\n}}\n",
            name = n.name,
            u = uses_or(&gens),
            id = esc(&n.id),
        );
        self.out.push_str(&out);
    }

    /// A `web(wasm)` app: its state (thread-local: wasm is single-threaded),
    /// a JSON entry point per export and its `#[wasm_bindgen]` wrapper.
    pub(crate) fn web_app(&mut self, n: &Node, a: &AppInfo) {
        let names = self.names();
        let eg = self.eg();
        let st = format!("{}Web", pascal(&n.name));
        let mut out = String::new();
        let fields: Vec<String> = a
            .provides
            .iter()
            .filter_map(|p| Some(format!("    pub {}: {},", cap_var(&p.capability), names.path(p.adapter.as_ref()?))))
            .collect();
        let inits: Vec<String> =
            a.provides.iter().zip(self.provide_exprs(n)).map(|(p, e)| format!("{}: {e}", cap_var(&p.capability))).collect();
        let state_var = format!("WH_{}_STATE", n.name.to_uppercase());
        let _ = writeln!(
            out,
            "/// State of the `{}` web app: the adapters it `provide`s.\npub struct {st} {{\n{}\n}}\n\nimpl {st} {{\n    pub fn new() -> Self {{\n        Self {{ {} }}\n    }}\n}}\n\nthread_local! {{\n    static {state_var}: {st} = {st}::new();\n}}\n",
            n.name,
            fields.join("\n"),
            inits.join(", ")
        );
        for (i, id) in a.exports.iter().enumerate() {
            let node = &self.model.nodes[id];
            let Some(sig) = node.sig().cloned() else { continue };
            let fname = rust_keyword_safe(&node.name);
            let ps: Vec<&ParamInfo> = sig.params.iter().collect();
            let (input, parse) = match ps.as_slice() {
                [] => (String::new(), String::new()),
                [one] => (
                    "input: &str".to_string(),
                    format!(
                        "    let {}: {} = serde_json::from_str(input).map_err(|e| format!(\"invalid input: {{e}}\"))?;\n",
                        rust_keyword_safe(&one.name),
                        names.ty(&one.ty)
                    ),
                ),
                many => {
                    out.push_str(&params_struct(&names, &format!("{st}Args{i}"), many));
                    let mut p = format!("    let wh_args: {st}Args{i} = serde_json::from_str(input).map_err(|e| format!(\"invalid input: {{e}}\"))?;\n");
                    for x in many {
                        let _ = writeln!(p, "    let {0} = wh_args.{0};", rust_keyword_safe(&x.name));
                    }
                    ("input: &str".to_string(), p)
                }
            };
            let mut muts = String::new();
            for p in sig.params.iter().filter(|p| p.mode == ParamMode::Mut) {
                let _ = writeln!(muts, "    let mut {0} = {0};", rust_keyword_safe(&p.name));
            }
            let call = call_code(&names, &eg, id, &sig, "wh_state");
            let shown = |e: &Ty| if self.model.nodes.get(e.named_id().unwrap_or("")).is_some_and(|n| n.is_error_type) { "{e}" } else { "{e:?}" };
            let outcome = match &sig.raises {
                Some(e) => format!(
                    "    match wh_out {{\n        Ok(v) => serde_json::to_string(&v).map_err(|e| e.to_string()),\n        Err(e) => Err(format!(\"{}\")),\n    }}",
                    shown(e)
                ),
                None => "    serde_json::to_string(&wh_out).map_err(|e| e.to_string())".to_string(),
            };
            let arg = if input.is_empty() { "" } else { "input" };
            let _ = writeln!(
                out,
                "/// `{id}` for JavaScript: JSON in (the argument, or an object of the arguments), JSON out.\npub fn wh_export_{fname}({input}) -> Result<String, String> {{\n{parse}{muts}    let wh_out = {state_var}.with(|wh_state| {call});\n{outcome}\n}}\n\n#[wasm_bindgen::prelude::wasm_bindgen(js_name = {fname})]\npub fn wh_wasm_{fname}({input}) -> Result<String, wasm_bindgen::JsError> {{\n    wh_export_{fname}({arg}).map_err(|e| wasm_bindgen::JsError::new(&e))\n}}\n"
            );
        }
        self.out.push_str(&out);
    }
}

fn uses_or(gens: &[String]) -> String {
    if gens.is_empty() { String::new() } else { format!("<{}>", gens.join(", ")) }
}

/// The server binary: build the state from `provide`, listen on `$WH_ADDR`
/// (default: `listen`, else the profile's address), serve until shutdown.
pub fn server_main(n: &Node, a: &AppInfo, lib: &str, level: &str, format: &str) -> String {
    let module = crate::rust::module_ident(&n.module);
    let wh_targets::Runner::Serve { addr_env, default_addr } = a.profile().runner() else { return String::new() };
    let addr = a.listen.clone().unwrap_or_else(|| default_addr.to_string());
    let st = format!("{}Server", pascal(&n.name));
    format!(
        r#"//! Generated by Wild Horse from app `{id}` (target server(axum)). Do not edit.
use {lib}::{module}::*;

#[tokio::main]
async fn main() -> std::process::ExitCode {{
    {lib}::wh_rt::init_tracing({level:?}, {format:?});
    let state = std::sync::Arc::new({st}::new());
    let addr = std::env::var({addr_env:?}).unwrap_or_else(|_| {addr:?}.to_string());
    let listener = match tokio::net::TcpListener::bind(&addr).await {{
        Ok(l) => l,
        Err(e) => {{
            eprintln!("error: cannot listen on {{addr}}: {{e}}");
            return std::process::ExitCode::FAILURE;
        }}
    }};
    let local = listener.local_addr().map(|a| a.to_string()).unwrap_or(addr);
    tracing::info!(wh.node = {id:?}, addr = %local, "listening");
    eprintln!("listening on http://{{local}}");
    match axum::serve(listener, {app}_router(state)).with_graceful_shutdown(wh_{name}_shutdown()).await {{
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {{
            eprintln!("error: {{e}}");
            std::process::ExitCode::FAILURE
        }}
    }}
}}
"#,
        id = n.id,
        app = rust_keyword_safe(&n.name),
        name = n.name,
    )
}

/// `web/index.html`: loads the bindings `wasm-bindgen --target web` writes
/// to `web/pkg` and exposes the exports as `window.wh`.
pub fn web_index(n: &Node, a: &AppInfo, lib: &str) -> String {
    let exports: Vec<String> = a.exports.iter().map(|e| format!("\"{}\"", e.rsplit('.').next().unwrap_or(e))).collect();
    format!(
        "<!doctype html>\n<!-- Generated by Wild Horse from app `{}`. Build: cargo build --lib --release --target wasm32-unknown-unknown, then wasm-bindgen --target web --out-dir web/pkg <target>/wasm32-unknown-unknown/release/{lib}.wasm -->\n<meta charset=\"utf-8\">\n<title>{}</title>\n<script type=\"module\">\n  import init, * as wh from \"./pkg/{lib}.js\";\n  await init();\n  window.wh = wh;\n  console.log(\"exports (JSON in, JSON out):\", [{}]);\n</script>\n",
        n.id,
        n.name,
        exports.join(", ")
    )
}

impl<'m> TestGen<'m> {
    /// An adapter for tests: an in-memory one of the capability when it can
    /// be built, else the one the app provides.
    fn test_adapter(&self, cap: &str, provided: String) -> String {
        self.model
            .adapters_of(cap)
            .into_iter()
            .filter(|ad| ad.patterns.iter().any(|p| p == "InMemory"))
            .find_map(|ad| self.adapter_ctor(ad))
            .unwrap_or(provided)
    }

    /// `let x: T = <sample>;` for each parameter, from deterministic samples.
    fn samples(&mut self, sig: &SigInfo) -> Option<(String, Vec<String>)> {
        let mut code = String::new();
        let mut strats = Vec::new();
        for (k, p) in sig.params.iter().enumerate() {
            if !self.generable(&p.ty) {
                return None;
            }
            self.small = true;
            let s = self.strategy(&p.ty);
            self.small = false;
            strats.push(format!("let wh_strat_{k} = {s};"));
            let mutable = if p.mode == ParamMode::Mut { "mut " } else { "" };
            let _ = writeln!(
                code,
                "            let {mutable}{}: {} = proptest::strategy::ValueTree::current(&proptest::strategy::Strategy::new_tree(&wh_strat_{k}, &mut wh_runner).unwrap());",
                rust_keyword_safe(&p.name),
                self.names().ty(&p.ty)
            );
        }
        Some((code, strats))
    }

    fn oracle_call(&self, id: &str, sig: &SigInfo, state: &str) -> String {
        let mut args: Vec<String> = sig.uses.iter().map(|c| format!("&{state}.{}", cap_var(c))).collect();
        args.extend(sig.params.iter().map(|p| self.pass_owned(&rust_keyword_safe(&p.name), p)));
        let call = format!("{}({})", self.names().path(id), args.join(", "));
        if sig.is_async { format!("crate::wh_rt::block_on({call})") } else { call }
    }

    /// Server apps: every route is requested (through the router, no network)
    /// with sample inputs and must answer what the flow itself returns on
    /// the same (in-memory) adapters: its JSON result, or its error's status.
    pub(crate) fn server_tests(&mut self, body: &mut String, n: &Node, a: &AppInfo) {
        let ItemKind::App(ast_app) = &n.item.kind else { return };
        let eg = self.eg();
        let st = format!("{}Server", pascal(&n.name));
        let inits: Vec<String> = a
            .provides
            .iter()
            .zip(&ast_app.provides)
            .map(|(p, ap)| format!("{}: {}", cap_var(&p.capability), self.test_adapter(&p.capability, eg.emit(&ap.value, &Scope::default()))))
            .collect();
        let state = format!("{st} {{ {} }}", inits.join(", "));
        let errs = error_types(self.model, a);
        for (i, r) in a.routes.iter().enumerate() {
            let Some(sig) = self.model.nodes.get(&r.target).and_then(|t| t.sig()).cloned() else { continue };
            let Some((sample, strats)) = self.samples(&sig) else { continue };
            let names = self.names();
            let mut uri = format!("{:?}.to_string()", r.path);
            let mut skip = String::new();
            for (p, (_, src)) in sig.params.iter().zip(&r.params) {
                if *src == RouteSource::Path {
                    let v = rust_keyword_safe(&p.name);
                    let _ = writeln!(skip, "            let wh_seg_{v} = wh_seg(serde_json::to_value(&{v}).unwrap());\n            if wh_seg_{v}.is_empty() {{ continue; }}");
                    uri = format!("{uri}.replace(\"{{{}}}\", &wh_seg_{v})", p.name);
                }
            }
            let group = |src: RouteSource| -> Vec<&ParamInfo> {
                sig.params.iter().zip(&r.params).filter(|(_, (_, s))| *s == src).map(|(p, _)| p).collect()
            };
            let query = group(RouteSource::Query);
            if !query.is_empty() {
                let fs: Vec<String> = query.iter().map(|p| format!("{0}: {0}.clone()", rust_keyword_safe(&p.name))).collect();
                uri = format!("format!(\"{{}}?{{}}\", {uri}, serde_urlencoded::to_string(&{st}Query{i} {{ {} }}).unwrap())", fs.join(", "));
            }
            let bodies = group(RouteSource::Body);
            let req_body = match bodies.as_slice() {
                [] => "axum::body::Body::empty()".to_string(),
                [one] => format!("axum::body::Body::from(serde_json::to_vec(&{}).unwrap())", rust_keyword_safe(&one.name)),
                many => {
                    let fs: Vec<String> = many.iter().map(|p| format!("{0}: {0}.clone()", rust_keyword_safe(&p.name))).collect();
                    format!("axum::body::Body::from(serde_json::to_vec(&{st}Body{i} {{ {} }}).unwrap())", fs.join(", "))
                }
            };
            let ok = if sig.ret == Ty::Unit {
                "assert_eq!(wh_status, 204, \"{}\", String::from_utf8_lossy(&wh_bytes));".to_string()
            } else {
                "assert_eq!(wh_status, 200, \"{}\", String::from_utf8_lossy(&wh_bytes));\n                assert_eq!(serde_json::from_slice::<serde_json::Value>(&wh_bytes).unwrap(), serde_json::to_value(&v).unwrap());".to_string()
            };
            let compare = match sig.raises.as_ref().and_then(|e| e.named_id()) {
                Some(e) => {
                    let status = if errs.contains(e) { format!("wh_{}_status_{}(&e).0", n.name, e.replace('.', "_")) } else { "500".into() };
                    format!("match wh_expected {{\n                Ok(v) => {{\n                {ok}\n                }}\n                Err(e) => assert_eq!(wh_status, {status}, \"{{e:?}}\"),\n            }}")
                }
                None => format!("{{\n                let v = wh_expected;\n                {ok}\n            }}"),
            };
            let bad = if bodies.is_empty() {
                String::new()
            } else {
                format!(
                    "        // a malformed body is the client's error\n        let req = axum::http::Request::builder().method({m:?}).uri({uri0:?}).header(\"content-type\", \"application/json\").body(axum::body::Body::from(\"{{\")).unwrap();\n        let res = wh_rt.block_on(wh_router.clone().oneshot(req)).unwrap();\n        assert!(res.status().is_client_error(), \"{{}}\", res.status());\n",
                    m = r.method,
                    uri0 = r.path
                )
            };
            let _ = names;
            let slug: String = format!("{}_{}", r.method, r.path).chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
            let name = format!("{}__route__{}", test_prefix(&n.id), slug.trim_end_matches('_'));
            let oracle = self.oracle_call(&r.target, &sig, "wh_oracle");
            let _ = writeln!(
                body,
                "    #[test]\n    fn {name}() {{\n        use tower::ServiceExt as _;\n        crate::wh_rt::init_test_tracing();\n        let wh_rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();\n        let mut wh_runner = proptest::test_runner::TestRunner::deterministic();\n        let wh_seg = |v: serde_json::Value| {{\n            let s = match v {{ serde_json::Value::String(s) => s, other => other.to_string() }};\n            s.bytes().map(|b| if b.is_ascii_alphanumeric() || b\"-_.~\".contains(&b) {{ (b as char).to_string() }} else {{ format!(\"%{{b:02X}}\") }}).collect::<String>()\n        }};\n        let _ = &wh_seg;\n        {}\n        // the oracle: the flow itself, on its own copy of the test adapters\n        let wh_oracle = {state};\n        let wh_router = {app}_router(std::sync::Arc::new({state}));\n        for _ in 0..16 {{\n{sample}{skip}            let wh_expected = {oracle};\n            let req = axum::http::Request::builder().method({m:?}).uri({uri}).header(\"content-type\", \"application/json\").body({req_body}).unwrap();\n            let res = wh_rt.block_on(wh_router.clone().oneshot(req)).unwrap();\n            let wh_status = res.status().as_u16();\n            let wh_bytes = wh_rt.block_on(axum::body::to_bytes(res.into_body(), usize::MAX)).unwrap();\n            {compare}\n        }}\n{bad}    }}\n",
                strats.join("\n        "),
                app = rust_keyword_safe(&n.name),
                m = r.method,
            );
            self.tests.push((name, n.id.clone()));
        }
    }

    /// Web apps: every export answers with sample inputs (as JSON) what the
    /// flow or def returns on its own copy of the adapters.
    pub(crate) fn web_tests(&mut self, body: &mut String, n: &Node, a: &AppInfo) {
        let st = format!("{}Web", pascal(&n.name));
        for (i, id) in a.exports.iter().enumerate() {
            let Some(sig) = self.model.nodes.get(id).and_then(|t| t.sig()).cloned() else { continue };
            let Some((sample, strats)) = self.samples(&sig) else { continue };
            let fname = rust_keyword_safe(&self.model.nodes[id].name);
            let input = match sig.params.as_slice() {
                [] => String::new(),
                [one] => format!("&serde_json::to_string(&{}).unwrap()", rust_keyword_safe(&one.name)),
                many => {
                    let fs: Vec<String> = many.iter().map(|p| format!("{0}: {0}.clone()", rust_keyword_safe(&p.name))).collect();
                    format!("&serde_json::to_string(&{st}Args{i} {{ {} }}).unwrap()", fs.join(", "))
                }
            };
            let shown = if sig.raises.as_ref().and_then(|e| self.model.nodes.get(e.named_id()?)).is_some_and(|n| n.is_error_type) { "{e}" } else { "{e:?}" };
            let compare = if sig.raises.is_some() {
                format!("match wh_expected {{\n                Ok(v) => assert_eq!(serde_json::from_str::<serde_json::Value>(&wh_got.unwrap()).unwrap(), serde_json::to_value(&v).unwrap()),\n                Err(e) => assert_eq!(wh_got.unwrap_err(), format!(\"{shown}\")),\n            }}")
            } else {
                "assert_eq!(serde_json::from_str::<serde_json::Value>(&wh_got.unwrap()).unwrap(), serde_json::to_value(&wh_expected).unwrap());".to_string()
            };
            let bad = if input.is_empty() { String::new() } else { format!("        assert!(wh_export_{fname}(\"{{\").is_err(), \"malformed JSON is rejected\");\n") };
            let name = format!("{}__export__{fname}", test_prefix(&n.id));
            let oracle = self.oracle_call(id, &sig, "wh_oracle");
            let _ = writeln!(
                body,
                "    #[test]\n    fn {name}() {{\n        crate::wh_rt::init_test_tracing();\n        let mut wh_runner = proptest::test_runner::TestRunner::deterministic();\n        {}\n        let wh_oracle = {st}::new();\n        for _ in 0..16 {{\n{sample}            let wh_expected = {oracle};\n            let wh_got = wh_export_{fname}({input});\n            {compare}\n        }}\n{bad}    }}\n",
                strats.join("\n        ")
            );
            self.tests.push((name, n.id.clone()));
        }
    }
}

/// `TargetKind` of every app, for building the crate.
pub fn profiles(model: &Model) -> Vec<TargetProfile> {
    let mut out: Vec<TargetProfile> = apps(model).into_iter().map(|(_, _, p)| p).collect();
    out.dedup();
    out
}

pub fn has_kind(model: &Model, k: TargetKind) -> bool {
    profiles(model).iter().any(|p| p.kind == k)
}
