//! `.wh/layout.json`: view-only state (node positions per graph, viewport),
//! keyed by node id so the literate document stays clean.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::layout::Point;

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Viewport {
    pub x: f32,
    pub y: f32,
    pub zoom: f32,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct GraphLayout {
    #[serde(default)]
    pub positions: BTreeMap<String, Point>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewport: Option<Viewport>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LayoutStore {
    #[serde(default = "version")]
    pub version: u32,
    /// Graph name (`flow`, `data`) → layout.
    #[serde(default)]
    pub graphs: BTreeMap<String, GraphLayout>,
}

fn version() -> u32 {
    1
}

impl LayoutStore {
    pub fn path(project_dir: &Path) -> PathBuf {
        project_dir.join(".wh").join("layout.json")
    }

    /// Load the project's layout; a missing or unreadable file is an empty layout.
    pub fn load(project_dir: &Path) -> LayoutStore {
        std::fs::read_to_string(Self::path(project_dir))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(LayoutStore { version: 1, graphs: BTreeMap::new() })
    }

    pub fn save(&self, project_dir: &Path) -> io::Result<()> {
        let p = Self::path(project_dir);
        std::fs::create_dir_all(p.parent().unwrap())?;
        let json = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        std::fs::write(p, json + "\n")
    }

    pub fn positions(&self, graph: &str) -> BTreeMap<String, Point> {
        self.graphs.get(graph).map(|g| g.positions.clone()).unwrap_or_default()
    }

    pub fn set(&mut self, graph: &str, id: &str, p: Point) {
        self.graphs.entry(graph.to_string()).or_default().positions.insert(id.to_string(), p);
    }

    pub fn viewport(&self, graph: &str) -> Option<Viewport> {
        self.graphs.get(graph).and_then(|g| g.viewport)
    }

    pub fn set_viewport(&mut self, graph: &str, v: Viewport) {
        self.graphs.entry(graph.to_string()).or_default().viewport = Some(v);
    }

    /// Re-key a renamed node (and its step/param ids `old#…`) in every graph.
    pub fn rekey(&mut self, old: &str, new: &str) {
        for g in self.graphs.values_mut() {
            let keys: Vec<String> =
                g.positions.keys().filter(|k| *k == old || k.starts_with(&format!("{old}#"))).cloned().collect();
            for k in keys {
                let p = g.positions.remove(&k).unwrap();
                g.positions.insert(format!("{new}{}", &k[old.len()..]), p);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_rekey() {
        let dir = std::env::temp_dir().join(format!("wh-graph-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(LayoutStore::load(&dir).graphs.len(), 0);
        let mut s = LayoutStore::load(&dir);
        s.set("flow", "signup.register", Point::new(10.0, 20.0));
        s.set("flow", "signup.register#validate[0]", Point::new(30.0, 40.0));
        s.set("data", "signup.User", Point::new(1.0, 2.0));
        s.set_viewport("flow", Viewport { x: 1.0, y: 2.0, zoom: 1.5 });
        s.save(&dir).unwrap();
        let mut t = LayoutStore::load(&dir);
        assert_eq!(s, t);
        t.rekey("signup.register", "signup.sign_up");
        let f = t.positions("flow");
        assert_eq!(f.get("signup.sign_up"), Some(&Point::new(10.0, 20.0)));
        assert_eq!(f.get("signup.sign_up#validate[0]"), Some(&Point::new(30.0, 40.0)));
        assert!(!f.contains_key("signup.register"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
