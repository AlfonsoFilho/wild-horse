//! File watching: changes under the project directory arrive as paths
//! relative to it, drained by the app state on its poll timer.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};

use notify::{RecursiveMode, Watcher as _};

pub struct Watcher {
    _inner: notify::RecommendedWatcher,
    rx: Receiver<PathBuf>,
    dir: PathBuf,
}

impl Watcher {
    pub fn new(dir: &Path) -> notify::Result<Watcher> {
        let (tx, rx) = channel();
        let mut inner = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(e) = res {
                if e.kind.is_access() {
                    return;
                }
                for p in e.paths {
                    let _ = tx.send(p);
                }
            }
        })?;
        inner.watch(dir, RecursiveMode::Recursive)?;
        Ok(Watcher { _inner: inner, rx, dir: dir.to_path_buf() })
    }

    /// Changed paths since the last call, relative to the project directory,
    /// without build output and IDE sidecars.
    pub fn drain(&self) -> BTreeSet<String> {
        self.rx.try_iter().filter_map(|p| relative(&self.dir, &p)).filter(|r| !ignored(r)).collect()
    }
}

fn relative(dir: &Path, p: &Path) -> Option<String> {
    let rel = match p.strip_prefix(dir) {
        Ok(r) => r.to_path_buf(),
        // FSEvents may report another spelling of the same path (/private/var vs /var)
        Err(_) => {
            let parent = p.parent()?.canonicalize().ok()?;
            parent.strip_prefix(dir).ok()?.join(p.file_name()?)
        }
    };
    Some(rel.to_string_lossy().replace('\\', "/"))
}

fn ignored(rel: &str) -> bool {
    rel.starts_with(".wh/") || rel.starts_with("target/") || rel.starts_with(".git/") || rel.contains("/.") || rel.starts_with('.')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn reports_relative_source_changes() {
        let dir = std::env::temp_dir().join(format!("wh-ide-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join(".wh")).unwrap();
        let dir = dir.canonicalize().unwrap();
        let w = Watcher::new(&dir).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        std::fs::write(dir.join("src/a.wh.md"), "x").unwrap();
        std::fs::write(dir.join(".wh/layout.json"), "{}").unwrap();
        let start = Instant::now();
        let mut seen = BTreeSet::new();
        while start.elapsed() < Duration::from_secs(10) && !seen.contains("src/a.wh.md") {
            seen.extend(w.drain());
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(seen.contains("src/a.wh.md"), "{seen:?}");
        assert!(!seen.iter().any(|p| p.starts_with(".wh")), "{seen:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
