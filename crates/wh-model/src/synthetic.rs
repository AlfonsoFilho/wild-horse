//! Synthetic projects for benchmarks and incremental-checking tests: `n`
//! modules of `groups` definition groups each (a refined type, a struct, an
//! error enum, a def with contracts and specs, and a flow), every module
//! importing from the previous one.

use crate::model::SourceInput;

/// Definitions per group (type, struct, error, def, flow).
pub const DEFS_PER_GROUP: usize = 5;

/// The source of one group of definitions of module `m`.
pub fn group(m: usize, k: usize, variant: usize) -> String {
    let prev = if m > 0 { format!("\n    prev: m{}.Rec{k} | None = None", m - 1) } else { String::new() };
    let bound = 20 + variant;
    format!(
        r#"## Group {k}

A code of group {k}.

```wh
type Code{k} = str where len(self) >= 1 and len(self) <= {bound}
```

A record of group {k}.

```wh
struct Rec{k}:
    key id: u64
    name: Code{k}
    score: int{prev}
    invariant self.score >= 0
```

What can go wrong in group {k}.

```wh
@error
enum Err{k}:
    Bad(str)
    Missing
```

Making a record of group {k}.

```wh
def make{k}(name: Code{k}, score: int) -> Rec{k} raises Err{k}:
    requires score <= 1000
    ensures result.score == score
    raises Err{k}.Bad when score < 0
    example make{k}("ab", 3).score == 3
    example make{k}("ab", -1) raises Err{k}
    property keeps_name(n: Code{k}):
        make{k}(n, 1).name == n
```

The pipeline of group {k}.

```wh
flow pipe{k}(name: Code{k}) -> Rec{k} raises Err{k}:
    name |> make{k}(1)
```

"#
    )
}

/// One module's file.
pub fn module(m: usize, groups: usize, variant: usize) -> SourceInput {
    let mut text = format!("---\nmodule: m{m}\n---\n# Module {m}\n\nGenerated for benchmarks.\n\n");
    if m > 0 {
        let names: Vec<String> = (0..groups).map(|k| format!("Rec{k}")).collect();
        text.push_str(&format!("```wh\nfrom m{} import {}\n```\n\n", m - 1, names.join(", ")));
    }
    for k in 0..groups {
        text.push_str(&group(m, k, if k == 0 { variant } else { 0 }));
    }
    SourceInput { path: format!("src/m{m}.wh.md"), text }
}

/// A project of `modules` modules with `groups` groups each
/// (`modules * groups * DEFS_PER_GROUP` definitions).
pub fn project(modules: usize, groups: usize) -> Vec<SourceInput> {
    (0..modules).map(|m| module(m, groups, 0)).collect()
}
