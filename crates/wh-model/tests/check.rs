use std::path::Path;

use wh_model::{Model, NodeKind, SourceInput};
use wh_syntax::Severity;

fn root() -> &'static Path {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
}

pub fn signup() -> Model {
    let dir = root().join("examples/signup/src");
    let inputs: Vec<SourceInput> = ["signup.wh.md", "main.wh.md"]
        .iter()
        .map(|f| SourceInput { path: f.to_string(), text: std::fs::read_to_string(dir.join(f)).unwrap() })
        .collect();
    Model::build(&inputs)
}

fn render(m: &Model, min: Severity) -> Vec<String> {
    m.diagnostics
        .iter()
        .filter(|d| d.diag.severity <= min)
        .map(|d| d.diag.render(&m.files[d.file].path, &m.files[d.file].text))
        .collect()
}

#[test]
fn signup_is_clean() {
    let m = signup();
    let errs = render(&m, Severity::Warning);
    assert!(errs.is_empty(), "{}", errs.join("\n"));
}

#[test]
fn signup_flow_and_caps() {
    let m = signup();
    let NodeKind::Flow(f) = &m.nodes["signup.register"].kind else { panic!() };
    let labels: Vec<&str> = f.steps.iter().map(|s| s.label.as_str()).collect();
    assert_eq!(labels, ["validate", "normalize_form", "check_age", "save_user"]);
    assert_eq!(f.sig.uses, ["signup.Db"]);
    assert_eq!(f.result, Some(3));
    let NodeKind::App(app) = &m.nodes["main.signup_cli"].kind else { panic!() };
    let entry = app.entry.as_ref().unwrap();
    assert_eq!(entry.sig.uses, ["main.Console", "signup.Db"]);
}

#[test]
fn spec_keys_ignore_prose_and_formatting() {
    let m = signup();
    let k1 = wh_model::hash::spec_key(&m, "signup.normalize_email", "");
    let dir = root().join("examples/signup/src");
    let text = std::fs::read_to_string(dir.join("signup.wh.md")).unwrap();
    let edited = text
        .replace("Emails are compared case-insensitively", "Emails compare case-insensitively")
        .replace("\"\"\"Canonical form used for uniqueness checks.\"\"\"", "\"\"\"Canonical.\"\"\"  # comment");
    let m2 = Model::build(&[
        SourceInput { path: "signup.wh.md".into(), text: edited.clone() },
        SourceInput { path: "main.wh.md".into(), text: std::fs::read_to_string(dir.join("main.wh.md")).unwrap() },
    ]);
    assert_eq!(k1, wh_model::hash::spec_key(&m2, "signup.normalize_email", ""));
    let m3 = Model::build(&[SourceInput {
        path: "signup.wh.md".into(),
        text: edited.replace("ensures result == e.lower()", "ensures result == e.upper()"),
    }]);
    assert_ne!(k1, wh_model::hash::spec_key(&m3, "signup.normalize_email", ""));
    // changing a type the signature uses changes the key too
    let m4 = Model::build(&[SourceInput { path: "signup.wh.md".into(), text: edited.replace("[^@\\s]+$", "[^@]+$") }]);
    assert_ne!(k1, wh_model::hash::spec_key(&m4, "signup.normalize_email", ""));
}

fn errors(src: &str) -> Vec<String> {
    let m = Model::build(&[SourceInput { path: "t.wh.md".into(), text: format!("```wh\n{src}\n```\n") }]);
    m.diagnostics.iter().filter(|d| d.diag.severity == Severity::Error).map(|d| d.diag.message.clone()).collect()
}

fn assert_error(src: &str, needle: &str) {
    let e = errors(src);
    assert!(e.iter().any(|m| m.contains(needle)), "expected an error containing {needle:?}, got {e:#?}");
}

#[test]
fn rule_names_resolve() {
    assert_error("def f(x: Missing) -> int", "unknown type `Missing`");
    assert_error("def f(x: int) -> int:\n    ensures result == y", "unknown name `y`");
    assert_error("def f(x: int) -> int\ndef f(x: int) -> int", "defined more than once");
    assert_error("struct bad_name:\n    a: int", "PascalCase");
    assert_error("def BadName(x: int) -> int", "snake_case");
}

#[test]
fn rule_types() {
    assert_error("def f(x: int) -> int:\n    ensures result", "must be a `bool`");
    assert_error("def f(x: list[int, int]) -> int", "takes 1 type argument");
    assert_error("struct S:\n    a: S", "contains itself");
    assert_error("struct S:\n    a: int\n    a: int", "duplicate field");
}

#[test]
fn rule_purity() {
    let src = "capability Db:\n    def get() -> int\n\ndef load() -> int uses Db\n\ndef f(x: int) -> int:\n    ensures result == load()";
    assert_error(src, "must be pure");
    assert_error("capability Db:\n    def get() -> int\n\ndef f() -> int uses Db:\n    pure", "cannot use capabilities");
}

#[test]
fn rule_flows() {
    let base = "def a(x: int) -> str\ndef b(x: int) -> int\n";
    assert_error(&format!("{base}flow f(x: int) -> int:\n    x\n    |> a\n    |> b"), "piped value is `str`");
    assert_error(&format!("{base}flow f(x: int) -> str:\n    x\n    |> b"), "returns `str`");
    let errs = "@error\nenum E1:\n    A\n@error\nenum E2:\n    B\ndef a(x: int) -> int raises E1\n";
    assert_error(&format!("{errs}flow f(x: int) -> int raises E2:\n    x |> a"), "does not convert into `E2`");
    assert_error(&format!("{errs}flow f(x: int) -> int:\n    x |> a"), "does not declare `raises`");
    let caps = "capability Db:\n    def get() -> int\ncapability Log:\n    def put() -> int\ndef a(x: int) -> int uses Db\n";
    assert_error(&format!("{caps}flow f(x: int) -> int uses Log:\n    x |> a"), "does not declare in `uses`");
    let en = "enum Ev:\n    A\n    B(int)\ndef h(x: int) -> unit\n";
    assert_error(&format!("{en}flow f(e: Ev) -> unit:\n    match e:\n        case A: return"), "does not handle: B");
}

#[test]
fn rule_apps_and_tests() {
    let base = "capability Db:\n    def get() -> int\nadapter Mem implements Db:\n    pattern InMemory\ndef a(x: int) -> int uses Db\nflow f(x: int) -> int:\n    x |> a\n";
    assert_error(&format!("{base}app x:\n    target native(cli)\n    entry main(args: list[str]) -> unit:\n        1 |> f"), "add `provide Db");
    assert_error(&format!("{base}test \"t\":\n    expect f(1) == 2"), "add `given Db");
    assert!(errors(&format!("{base}test \"t\":\n    given Db = Mem()\n    expect f(1) == 2")).is_empty());
}

#[test]
fn rule_literate() {
    let m = Model::build(&[SourceInput { path: "t.wh.md".into(), text: "See [[nope]].\n\n```wh\ndef f(x: int) -> int\n```\n".into() }]);
    let msgs: Vec<_> = m.diagnostics.iter().map(|d| (d.diag.severity, d.diag.message.clone())).collect();
    assert!(msgs.iter().any(|(s, m)| *s == Severity::Warning && m.contains("unknown definition `nope`")), "{msgs:?}");
    let m = Model::build(&[SourceInput { path: "t.wh.md".into(), text: "```wh\ndef f(x: int) -> int\n```\n".into() }]);
    assert!(m.diagnostics.iter().any(|d| d.diag.severity == Severity::Lint));
}

const DOOR: &str = r#"enum DoorEvent:
    Open
    Close
    Lock(str)
    Unlock(str)

enum DoorEffect:
    Unlocked
    Alarm

machine Door(event: DoorEvent) -> DoorEffect:
    Open           on Close                  -> Closed
    Closed         on Open                   -> Open
    Closed         on Lock(code)             -> Locked(code)
    Locked(code)   on Unlock(c) if c == code -> Closed      emit Unlocked
    Locked(code)   on Unlock(c)              -> Locked(code) emit Alarm
    ignore Open on Open
    ignore Open on Lock
    ignore Open on Unlock
    ignore Closed on Close
    ignore Closed on Unlock
    ignore Locked on Open
    ignore Locked on Close
    ignore Locked on Lock
    initial Closed"#;

fn model_of(src: &str) -> Model {
    Model::build(&[SourceInput { path: "t.wh.md".into(), text: format!("```wh\n{src}\n```\n") }])
}

#[test]
fn machine_states_are_inferred() {
    assert!(errors(DOOR).is_empty(), "{:#?}", errors(DOOR));
    let m = model_of(DOOR);
    let NodeKind::Machine(info) = &m.nodes["t.Door"].kind else { panic!() };
    let states: Vec<(&str, Vec<String>)> =
        info.states.variants.iter().map(|v| (v.name.as_str(), v.items.iter().map(|i| i.ty.to_string()).collect())).collect();
    assert_eq!(states, [("Open", vec![]), ("Closed", vec![]), ("Locked", vec!["str".to_string()])]);
    assert_eq!(info.initial.as_deref(), Some("Closed"));
    assert_eq!(info.transitions[3].id, "t.Door#Locked.Unlock[0]");
    assert_eq!(info.transitions[4].id, "t.Door#Locked.Unlock[1]");
    assert_eq!(info.paths["Locked"], [2]);
    assert_eq!(info.paths["Open"], [1]);
    assert!(info.transitions[3].has_guard && info.transitions[3].has_emit);
    // machine states are usable in specs: `Door.Locked(...)`, `is`, `Door.step`
    let spec = format!(
        "{DOOR}\n\ndef f(d: Door) -> bool:\n    example Door.step(Door.Closed, DoorEvent.Lock(\"1\")) == (Door.Locked(\"1\"), None)\n    example Door.initial() is Closed"
    );
    assert!(errors(&spec).is_empty(), "{:#?}", errors(&spec));
}

#[test]
fn machine_rules() {
    let ev = "enum Ev:\n    Go\n    Stop\n";
    // unhandled pairs
    assert_error(&format!("{ev}machine M(e: Ev):\n    A on Go -> B\n    B on Go -> A\n    initial A"), "state `A` does not handle event: Stop");
    // `_` and ignore make it total
    assert!(errors(&format!("{ev}machine M(e: Ev):\n    A on Go -> B\n    B on _ -> A\n    ignore A on Stop\n    initial A")).is_empty());
    // unknown events and states
    assert_error(&format!("{ev}machine M(e: Ev):\n    A on Jump -> A\n    ignore A on _\n    initial A"), "`Jump` is not an event of `Ev`");
    assert_error(&format!("{ev}machine M(e: Ev):\n    A on _ -> A\n    ignore Q on Go\n    initial A"), "unknown state `Q`");
    // unreachable states
    assert_error(&format!("{ev}machine M(e: Ev):\n    A on _ -> A\n    B on _ -> A\n    initial A"), "state `B` is unreachable");
    // initial is required
    assert_error(&format!("{ev}machine M(e: Ev):\n    A on _ -> A"), "needs an `initial` state");
    // payload types must be inferable, guards boolean, effects declared
    assert_error(&format!("{ev}machine M(e: Ev):\n    A on _ -> B(x)\n    B(x) on _ -> A\n    initial A"), "unknown name `x`");
    assert_error(&format!("{ev}machine M(e: Ev):\n    A on Go if 1 -> A\n    A on Stop -> A\n    initial A"), "must be a `bool`");
    assert_error(&format!("{ev}machine M(e: Ev):\n    A on _ -> A emit Go\n    initial A"), "emits nothing");
    assert_error("machine M(e: int):\n    A on _ -> A\n    initial A", "must be an enum");
    assert_error(&format!("{ev}machine M(e: Ev):\n    A on _ -> A\n    initial A\n    trace loud"), "unknown trace level");
}

#[test]
fn literals_satisfy_refinements() {
    let base = "type Email = str where matches(self, r\"^[^@\\s]+@[^@\\s]+$\")\ntype Age = u8 where self <= 150\ntype Code = str where len(self) == 4 and self.isdigit()\n";
    // examples
    assert_error(&format!("{base}def f(e: Email) -> Email:\n    example f(\"nope\") == \"nope\""), "\"nope\" does not satisfy `Email`");
    assert!(errors(&format!("{base}def f(e: Email) -> Email:\n    example f(\"a@b.io\") == \"a@b.io\"")).is_empty());
    assert_error(&format!("{base}def g(a: Age) -> int:\n    example g(151) == 1"), "151 does not satisfy `Age`: `self <= 150`");
    assert_error(&format!("{base}def g(a: Age) -> int:\n    example g(300) == 1"), "`300` does not fit in `u8`");
    assert_error(&format!("{base}def h(c: Code) -> int:\n    example h(\"12a4\") == 1"), "does not satisfy `Code`");
    assert!(errors(&format!("{base}def h(c: Code) -> int:\n    example h(\"1234\") == 1")).is_empty());
    // constructors, struct fields, defaults (fields and parameters), lists, options
    assert_error(&format!("{base}def g(a: Age) -> int:\n    example g(Age(200)) == 1"), "200 does not satisfy `Age`");
    assert_error(&format!("{base}struct P:\n    age: Age = 200"), "200 does not satisfy `Age`");
    assert_error(&format!("{base}def k(a: Age = 151) -> int"), "151 does not satisfy `Age`");
    assert_error(&format!("{base}struct P:\n    age: Age | None = None\n    tags: list[Code] = []\n\ndef p(x: P) -> int:\n    example p(P(age=7, tags=[\"1234\", \"x\"])) == 1"), "\"x\" does not satisfy `Code`");
    // scenario tests
    assert_error(&format!("{base}def g(a: Age) -> int\ntest \"t\":\n    expect g(999) == 1"), "does not fit in `u8`");
    // undecidable predicates are left to run time
    assert!(errors(&format!("{base}def big(x: int) -> bool\ntype B = int where big(self)\ndef q(b: B) -> int:\n    example q(5) == 1")).is_empty());
    // invalid regular expressions are reported at check time
    assert_error("type R = str where matches(self, r\"(\")", "invalid regular expression");
}

#[test]
fn alias_names_are_kept() {
    let m = signup();
    let NodeKind::Struct(user) = &m.nodes["signup.User"].kind else { panic!() };
    let id = user.fields.iter().find(|f| f.name == "id").unwrap();
    assert_eq!((id.written.as_str(), id.alias.as_deref()), ("UserId", Some("signup.UserId")));
    let guardian = user.fields.iter().find(|f| f.name == "guardian").unwrap();
    assert_eq!((guardian.written.as_str(), guardian.alias.as_deref()), ("UserId | None", Some("signup.UserId")));
    assert_eq!(m.key_owner_of_alias("signup.UserId").map(|n| n.id.as_str()), Some("signup.User"));
    assert_eq!(m.reference_target(guardian.alias.as_deref(), &guardian.ty).map(|n| n.id.as_str()), Some("signup.User"));
    // a plain u64 field is not a reference just because UserId expands to u64
    let src = "alias UserId = u64\nstruct User:\n    key id: UserId\nstruct Counter:\n    hits: u64\n    owner: UserId";
    let m = model_of(src);
    let NodeKind::Struct(c) = &m.nodes["t.Counter"].kind else { panic!() };
    assert!(m.reference_target(c.fields[0].alias.as_deref(), &c.fields[0].ty).is_none());
    assert_eq!(m.reference_target(c.fields[1].alias.as_deref(), &c.fields[1].ty).map(|n| n.name.as_str()), Some("User"));
    // parameters keep their written types too
    let NodeKind::Capability(db) = &signup().nodes["signup.Db"].kind else { panic!() };
    let find = db.methods.iter().find(|m| m.name == "find_by_email").unwrap();
    assert_eq!(find.sig.params[0].written, "Email");
}

#[test]
fn pattern_shape_rules() {
    assert_error("@pattern(Builder)\nenum E:\n    A", "pattern `Builder` applies to struct, not to a enum");
    assert_error("enum E:\n    A\nmachine M(e: E):\n    S on _ -> S\n    initial S\n@pattern(StateMachine)\nstruct X:\n    a: int", "applies to machine");
    assert_error("def f(x: float) -> float:\n    pattern Memoize", "needs `Hash` inputs");
    assert_error("def f(mut x: list[int]) -> int:\n    pattern Memoize", "mutates its argument");
    assert!(errors("def f(x: int, y: str) -> int:\n    pattern Memoize").is_empty());
    let repo = "struct Item:\n    key id: u64\n    name: str\n";
    assert!(errors(&format!("{repo}@pattern(Repository[Item])\ncapability Items:\n    def add(own x: Item) -> unit\n    def get(id: u64) -> Item | None")).is_empty());
    assert!(errors(&format!("{repo}@pattern(Repository)\ncapability Items:\n    def insert(own x: Item) -> u64\n    def find(id: u64) -> Item | None")).is_empty());
    assert_error(&format!("{repo}@pattern(Repository)\ncapability Items:\n    def add(own x: Item) -> unit\n    def get(name: str) -> Item | None"), "pattern `Repository` needs");
    let m = model_of(&format!("{repo}@pattern(Repository)\ncapability Items:\n    def add(own x: Item) -> unit\n    def get(id: u64) -> Item | None"));
    let shape = m.repository_shape("t.Items", None).unwrap();
    assert_eq!((shape.insert.as_str(), shape.find.as_str(), shape.key_field.as_deref()), ("add", "get", Some("id")));
    // capability contracts are checked, and adapters that cannot be built for contract tests are flagged
    assert_error("capability C:\n    def f(x: int) -> int:\n        ensures result == y", "unknown name `y`");
    assert_error("capability C:\n    def f(x: int) -> int:\n        example f(1) == 1", "capability methods take");
    let m = model_of("capability C:\n    def f(x: int) -> int:\n        ensures result >= 0\n@pattern(Adapter)\nadapter A implements C:\n    path: str");
    assert!(m.diagnostics.iter().any(|d| d.diag.message.contains("gets no contract tests")), "{:?}", m.diagnostics);
}

#[test]
fn flow_combinators() {
    let base = "struct Line:\n    qty: int\n@error\nenum E:\n    Bad\ndef add(total: int, l: Line) -> int\ndef split(l: Line) -> list[str]\ndef one(l: Line) -> str\ndef fetch(n: int) -> str raises E\ndef calm(n: int) -> str\n";
    let ok = format!(
        "{base}flow t(ls: list[Line]) -> int:\n    ls |> fold(0, add)\nflow u(ls: list[Line]) -> list[str]:\n    ls |> flat_map(split)\nflow r(n: int) -> str raises E:\n    n |> retry(fetch, times=3, backoff=5ms)\nflow p(ls: list[Line]) -> tuple[int, list[str]]:\n    a, b = parallel(t(ls), u(ls))\n    return (a, b)"
    );
    assert!(errors(&ok).is_empty(), "{:#?}", errors(&ok));
    let m = model_of(&ok);
    let NodeKind::Flow(p) = &m.nodes["t.p"].kind else { panic!() };
    let labels: Vec<&str> = p.steps.iter().map(|s| s.label.as_str()).collect();
    assert_eq!(labels, ["t", "u", "parallel"]);
    assert_eq!(p.steps[2].output.to_string(), "tuple[int, list[str]]");
    let NodeKind::Flow(r) = &m.nodes["t.r"].kind else { panic!() };
    assert_eq!((r.steps[0].label.as_str(), r.steps[0].raises.as_ref().map(|t| t.to_string())), ("retry:fetch", Some("E".into())));
    // typing errors
    assert_error(&format!("{base}flow f(ls: list[Line]) -> int:\n    ls |> fold(add)"), "`fold` takes an initial value");
    assert_error(&format!("{base}flow f(ls: list[Line]) -> int:\n    ls |> fold(\"x\", add)"), "initial value of `fold` should be `int`");
    assert_error(&format!("{base}flow f(ls: list[Line]) -> list[str]:\n    ls |> flat_map(one)"), "`flat_map` needs a function returning a list");
    assert_error(&format!("{base}flow f(n: int) -> str:\n    n |> retry(calm, times=2)"), "never fails");
    assert_error(&format!("{base}flow f(n: int) -> str raises E:\n    n |> retry(fetch)"), "needs the number of attempts");
    assert_error(&format!("{base}flow f(n: int) -> str raises E:\n    n |> retry(fetch, times=0)"), "at least 1");
    assert_error(&format!("{base}flow f(n: int) -> str raises E:\n    n |> retry(fetch, times=2, backoff=3)"), "`backoff` is a duration");
    assert_error(&format!("{base}flow f(n: int) -> str:\n    n |> retry(fetch, times=2)"), "does not declare `raises`");
    assert_error(&format!("{base}flow f(n: int) -> tuple[str, str]:\n    a, b = parallel(calm(n), n)\n    return (a, b)"), "each branch of `parallel`");
}

#[test]
fn requires_on_literal_calls() {
    let base = "def fib(n: u32) -> u64:\n    requires n <= 90\n";
    assert_error(&format!("{base}    example fib(91) == 1"), "this call violates `requires n <= 90` of `fib`");
    assert!(errors(&format!("{base}    example fib(90) > 1")).is_empty());
    // named and default arguments; undecidable arguments are left alone
    let base = "def take(xs: list[int], n: int = 5) -> list[int]:\n    requires n <= len(xs)\n";
    assert_error(&format!("{base}    example take([1, 2]) == [1, 2]"), "violates `requires n <= len(xs)`");
    assert_error(&format!("{base}    example take(n=3, xs=[1]) == [1]"), "violates");
    assert!(errors(&format!("{base}    example take([1, 2, 3, 4, 5, 6]) == [1, 2, 3, 4, 5]")).is_empty());
    assert!(errors(&format!("{base}    property p(xs: list[int]):\n        len(take(xs, 1)) <= 1")).is_empty());
    // scenario tests too
    assert_error("def fib(n: u32) -> u64:\n    requires n <= 90\ntest \"t\":\n    expect fib(100) == 1", "violates");
}

#[test]
fn sensitive_params_and_types() {
    let m = model_of("@sensitive\ntype Token = str\n\ndef login(user: str, sensitive password: str, own sensitive t: Token) -> bool");
    assert!(m.nodes["t.Token"].is_sensitive);
    let NodeKind::Def(d) = &m.nodes["t.login"].kind else { panic!() };
    let flags: Vec<bool> = d.sig.params.iter().map(|p| p.is_sensitive).collect();
    assert_eq!(flags, [false, true, true]);
    assert!(m.type_is_sensitive(&d.sig.params[2].ty));
    assert_error("@sensitive\nalias Key = str", "`@sensitive` applies to refined types, structs and enums");
    // `sensitive` is still a usable parameter name
    assert!(errors("def f(sensitive: bool) -> bool").is_empty());
}

#[test]
fn target_profiles() {
    let base = "struct Form:\n    name: str\n@error\nenum E:\n    Missing\n    Taken\ncapability Db:\n    def put(f: Form) -> int raises E\nadapter MemoryDb implements Db:\n    pattern InMemory\nadapter PgDb implements Db:\n    url: str = \"\"\n@target(web)\nadapter BrowserDb implements Db:\n    note \"IndexedDB\"\ndef save(f: Form) -> int raises E uses Db\ndef get(id: int, verbose: bool) -> Form raises E uses Db\ndef calc(x: int) -> int\ndef streamed(f: (int) -> int) -> int\n";
    let server = format!(
        "{base}app api:\n    target server(axum)\n    listen \"127.0.0.1:8080\"\n    provide Db = PgDb()\n    route POST \"/forms\" -> save\n    route GET \"/forms/{{id}}\" -> get\n    status E.Missing = 404\n    status E = 409"
    );
    assert!(errors(&server).is_empty(), "{:#?}", errors(&server));
    let m = model_of(&server);
    let NodeKind::App(a) = &m.nodes["t.api"].kind else { panic!() };
    assert_eq!(a.profile().written(), "server(axum)");
    assert_eq!(a.listen.as_deref(), Some("127.0.0.1:8080"));
    let srcs: Vec<(&str, wh_model::RouteSource)> = a.routes[1].params.iter().map(|(n, s)| (n.as_str(), s.clone())).collect();
    assert_eq!(srcs, [("id", wh_model::RouteSource::Path), ("verbose", wh_model::RouteSource::Query)]);
    assert_eq!(a.statuses.iter().map(|s| (s.variant.clone(), s.code)).collect::<Vec<_>>(), [(Some("Missing".into()), 404), (None, 409)]);
    // route rules
    let app = |body: &str| format!("{base}app api:\n    target server(axum)\n    provide Db = MemoryDb()\n{body}");
    assert_error(&app("    route FETCH \"/x\" -> save"), "unknown HTTP method `FETCH`");
    assert_error(&app("    route POST \"x\" -> save"), "starts with `/`");
    assert_error(&app("    route POST \"/x\" -> save\n    route POST \"/x\" -> save"), "declared twice");
    assert_error(&app("    route GET \"/x/{nope}\" -> calc"), "path segment `{nope}` is not a parameter");
    assert_error(&app("    route GET \"/x\" -> get\n    route POST \"/y\" -> Form"), "not a flow or def");
    assert_error(&app("    route GET \"/x\" -> save"), "cannot be read from the query string");
    assert_error(&app("    route POST \"/x\" -> streamed"), "cannot be sent as JSON");
    assert_error(&app("    route POST \"/x\" -> save\n    status E.Gone = 410"), "has no variant `Gone`");
    assert_error(&app("    route POST \"/x\" -> save\n    status E = 99"), "from 100 to 599");
    assert_error(&app("    route POST \"/x\" -> save\n    listen \"nowhere\""), "address string");
    assert_error(&app("    entry main(args: list[str]) -> unit:\n        args |> calc"), "has `route`s, not an `entry`");
    assert_error(&app(""), "needs at least one `route`");
    assert_error(&format!("{base}app api:\n    target server(axum)\n    route POST \"/x\" -> save"), "add `provide Db");
    // web rules: adapters must run in the browser, no threads
    let web = |provide: &str, body: &str| format!("{base}app w:\n    target web(wasm)\n    provide Db = {provide}()\n{body}");
    assert!(errors(&web("MemoryDb", "    export save, calc")).is_empty(), "{:#?}", errors(&web("MemoryDb", "    export save, calc")));
    assert!(errors(&web("BrowserDb", "    export save")).is_empty());
    assert_error(&web("PgDb", "    export save"), "adapter `PgDb` is not available on web(wasm)");
    assert_error(&web("MemoryDb", "    route POST \"/x\" -> save"), "`route` needs `target server(axum)`");
    assert_error(&web("MemoryDb", ""), "needs at least one `export`");
    let threads = format!("{base}def a(x: int) -> int\ndef b(x: int) -> int\nflow both(x: int) -> tuple[int, int]:\n    p, q = parallel(a(x), b(x))\n    return (p, q)\napp w:\n    target web(wasm)\n    export both");
    assert_error(&threads, "runs `parallel` on threads, which needs threads; web(wasm) has none");
    // `@target` restricts adapters natively too
    assert_error(&format!("{base}app cli:\n    target native(cli)\n    provide Db = BrowserDb()\n    entry main(args: list[str]) -> unit:\n        args |> calc"), "not available on native(cli)");
    // targets
    assert_error(&format!("{base}app x:\n    target embedded(arm)\n    entry main(args: list[str]) -> unit:\n        args |> calc"), "unknown target `embedded(arm)`");
    assert_error(&format!("{base}app x:\n    target native(cli)\n    target web(wasm)\n    entry main(args: list[str]) -> unit:\n        args |> calc"), "one target");
    assert_error(&format!("{base}app w:\n    target web(wasm)\n    provide Db = MemoryDb()\n    export calc\napp cli:\n    target native(cli)\n    entry main(args: list[str]) -> unit:\n        args |> calc"), "must be the only app");
    assert_error("@target(moon)\nadapter A implements B:\n    note \"x\"", "`@target` takes native, server, web");
}
