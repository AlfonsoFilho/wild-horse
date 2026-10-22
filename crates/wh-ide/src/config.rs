//! What the IDE keeps outside the document model: the `[author]` section of
//! `wildhorse.toml`, the recent-projects list in the user config dir, the
//! dock layout sidecar and the skeleton for new files (new projects come
//! from `wh_project::scaffold`).

use std::path::{Path, PathBuf};

/// `[author] apply = "review"`: the author agent's edits wait for Accept /
/// Reject in the Agent panel instead of applying live.
pub fn author_review(project_dir: &Path) -> bool {
    let Ok(t) = std::fs::read_to_string(project_dir.join("wildhorse.toml")) else { return false };
    let Ok(v) = t.parse::<toml::Table>() else { return false };
    v.get("author").and_then(|a| a.get("apply")).and_then(|a| a.as_str()) == Some("review")
}

/// `[synthesis] budget_usd`: the agent spending limit per synthesis run, if any.
pub fn synthesis_budget(project_dir: &Path) -> Option<f64> {
    let t = std::fs::read_to_string(project_dir.join("wildhorse.toml")).ok()?;
    let v = t.parse::<toml::Table>().ok()?;
    let b = v.get("synthesis")?.get("budget_usd")?;
    b.as_float().or_else(|| b.as_integer().map(|i| i as f64))
}

pub fn is_project(dir: &Path) -> bool {
    dir.join("wildhorse.toml").is_file()
}

fn config_dir() -> Option<PathBuf> {
    match std::env::var_os("WH_IDE_CONFIG_DIR") {
        Some(d) => Some(PathBuf::from(d)),
        None => Some(dirs::config_dir()?.join("wild-horse")),
    }
}

const MAX_RECENT: usize = 12;

/// Recently opened projects, most recent first (only those that still exist).
pub fn recent_projects() -> Vec<PathBuf> {
    let Some(f) = config_dir().map(|d| d.join("recent.json")) else { return vec![] };
    let list: Vec<PathBuf> = std::fs::read_to_string(f).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    list.into_iter().filter(|p| is_project(p)).collect()
}

pub fn add_recent(dir: &Path) {
    let Some(cfg) = config_dir() else { return };
    let mut list = recent_projects();
    list.retain(|p| p != dir);
    list.insert(0, dir.to_path_buf());
    list.truncate(MAX_RECENT);
    let _ = std::fs::create_dir_all(&cfg);
    if let Ok(json) = serde_json::to_string_pretty(&list) {
        let _ = std::fs::write(cfg.join("recent.json"), json + "\n");
    }
}

pub fn clear_recent() {
    if let Some(cfg) = config_dir() {
        let _ = std::fs::remove_file(cfg.join("recent.json"));
    }
}

/// `.wh/dock.json`: the dock layout (gpui-kit `DockAreaState`), next to `layout.json`.
pub fn dock_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".wh").join("dock.json")
}

/// A snake_case identifier made from a file or folder name.
pub fn snake_ident(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            if c.is_ascii_uppercase() && !out.is_empty() && !out.ends_with('_') {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('_') {
            out.push('_');
        }
    }
    let out = out.trim_end_matches('_').to_string();
    match out.chars().next() {
        None => "main".into(),
        Some(c) if c.is_ascii_digit() => format!("m_{out}"),
        Some(_) => out,
    }
}

fn title_case(s: &str) -> String {
    s.split('_').filter(|w| !w.is_empty()).map(|w| w[..1].to_uppercase() + &w[1..]).collect::<Vec<_>>().join(" ")
}

/// A new `.wh.md` module: front matter, a heading and a prose stub.
pub fn new_file_text(module: &str) -> String {
    format!("---\nmodule: {module}\n---\n# {}\n\nTODO: explain what this module is for.\n", title_case(module))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaffolded_project_opens_clean() {
        let dir = std::env::temp_dir().join(format!("wh-ide-new-{}", std::process::id())).join("My-App");
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
        let name = snake_ident(&dir.file_name().unwrap().to_string_lossy());
        wh_project::scaffold::new_project(&dir, &name).unwrap();
        let p = crate::project::Project::open(&dir).unwrap();
        assert_eq!(p.name, "my_app");
        assert_eq!(p.files, ["src/main.wh.md"]);
        assert!(p.model.diagnostics.iter().all(|d| d.diag.severity != wh_syntax::Severity::Error));
        assert!(!author_review(&dir));
        let t = std::fs::read_to_string(dir.join("wildhorse.toml")).unwrap() + "\n[author]\napply = \"review\"\n";
        std::fs::write(dir.join("wildhorse.toml"), t).unwrap();
        assert!(author_review(&dir));
        assert!(wh_project::Project::load(&dir).is_ok(), "wh-project reads the [author] section fine");
        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    #[test]
    fn idents_and_templates() {
        assert_eq!(snake_ident("My-App"), "my_app");
        assert_eq!(snake_ident("orderBook v2"), "order_book_v2");
        assert_eq!(snake_ident("3d"), "m_3d");
        assert_eq!(snake_ident("--"), "main");
        let t = new_file_text("billing_rules");
        assert!(t.starts_with("---\nmodule: billing_rules\n---\n# Billing Rules\n"));
        let m = wh_model::Model::build(&[wh_model::SourceInput { path: "src/billing_rules.wh.md".into(), text: t }]);
        assert!(m.diagnostics.iter().all(|d| d.diag.severity != wh_syntax::Severity::Error));
    }

    #[test]
    fn recent_list_round_trip() {
        let cfg = std::env::temp_dir().join(format!("wh-ide-cfg-{}", std::process::id()));
        let proj = cfg.join("p");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("wildhorse.toml"), "[project]\nname = \"p\"\n").unwrap();
        // SAFETY: tests touching this variable run in this one test only
        unsafe { std::env::set_var("WH_IDE_CONFIG_DIR", &cfg) };
        clear_recent();
        add_recent(&proj);
        add_recent(&cfg.join("gone"));
        add_recent(&proj);
        assert_eq!(recent_projects(), [proj.clone()]);
        std::fs::remove_dir_all(&cfg).unwrap();
    }
}
