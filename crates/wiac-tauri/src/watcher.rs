//! Cross-platform file-system watcher driving the frontend's "source
//! changed — reload?" flow. One global instance per app holds the active
//! watch set; `set_paths` replaces it atomically. Modify events are
//! debounced (CAD apps emit a burst on save: tmp file → rename → attr
//! touch) and forwarded to JS as `source-file-changed` Tauri events with
//! a `{ path }` payload.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use notify::event::ModifyKind;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Runtime};

const DEBOUNCE: Duration = Duration::from_millis(200);
pub const SOURCE_CHANGED_EVENT: &str = "source-file-changed";

#[derive(Serialize, Clone, Debug)]
pub struct SourceFileChanged {
    pub path: String,
}

trait EventSink: Send + Sync + 'static {
    fn emit_changed(&self, path: &Path);
}

struct AppHandleSink<R: Runtime>(AppHandle<R>);

impl<R: Runtime> EventSink for AppHandleSink<R> {
    fn emit_changed(&self, path: &Path) {
        let _ = self.0.emit(
            SOURCE_CHANGED_EVENT,
            SourceFileChanged {
                path: path.to_string_lossy().into_owned(),
            },
        );
    }
}

pub struct ProjectWatcher {
    watcher: Option<RecommendedWatcher>,
    watched: Vec<PathBuf>,
    last_emit: Arc<Mutex<HashMap<PathBuf, Instant>>>,
    sink: Arc<dyn EventSink>,
}

impl std::fmt::Debug for ProjectWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProjectWatcher")
            .field("watched", &self.watched)
            .field("active", &self.watcher.is_some())
            .finish()
    }
}

impl ProjectWatcher {
    pub fn new<R: Runtime>(app: AppHandle<R>) -> Self {
        Self::with_sink(Arc::new(AppHandleSink(app)))
    }

    fn with_sink(sink: Arc<dyn EventSink>) -> Self {
        Self {
            watcher: None,
            watched: Vec::new(),
            last_emit: Arc::new(Mutex::new(HashMap::new())),
            sink,
        }
    }

    #[allow(dead_code)]
    pub fn watched(&self) -> &[PathBuf] {
        &self.watched
    }

    /// Atomically replace the watch set. Old watches are dropped together
    /// with the underlying `RecommendedWatcher` so we never leave stale
    /// inotify slots behind. `paths` is deduped; non-existent paths are
    /// silently skipped (we only watch real files — the directory itself
    /// is the source of events on Linux, so the parent has to exist).
    pub fn set_paths(&mut self, paths: Vec<PathBuf>) -> Result<(), String> {
        self.watcher = None;
        self.watched.clear();
        self.last_emit
            .lock()
            .map_err(|e| format!("watcher mutex poisoned: {e}"))?
            .clear();

        let mut deduped: Vec<PathBuf> = Vec::new();
        for p in paths {
            if !deduped.iter().any(|q| q == &p) {
                deduped.push(p);
            }
        }
        if deduped.is_empty() {
            return Ok(());
        }

        let last_emit = Arc::clone(&self.last_emit);
        let sink = Arc::clone(&self.sink);
        let watch_targets: Vec<PathBuf> = deduped.clone();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(event) = res else { return };
            if !is_relevant(&event.kind) {
                return;
            }
            for evt_path in &event.paths {
                let canonical = canonicalize_or_self(evt_path);
                let matched = watch_targets
                    .iter()
                    .find(|t| paths_equal(t, &canonical) || paths_equal(t, evt_path));
                let Some(target) = matched else { continue };
                let now = Instant::now();
                let mut guard = match last_emit.lock() {
                    Ok(g) => g,
                    Err(_) => continue,
                };
                let recent = guard
                    .get(target)
                    .is_some_and(|prev| now.duration_since(*prev) < DEBOUNCE);
                if recent {
                    continue;
                }
                guard.insert(target.clone(), now);
                drop(guard);
                sink.emit_changed(target);
            }
        })
        .map_err(|e| format!("watcher init: {e}"))?;

        for p in &deduped {
            let watch_root: &Path = if p.is_file() {
                p.parent().unwrap_or(p.as_path())
            } else {
                p.as_path()
            };
            if let Err(e) = watcher.watch(watch_root, RecursiveMode::NonRecursive) {
                return Err(format!("watch {}: {}", watch_root.display(), e));
            }
        }

        self.watcher = Some(watcher);
        self.watched = deduped;
        Ok(())
    }

    pub fn unwatch_all(&mut self) -> Result<(), String> {
        self.set_paths(Vec::new())
    }
}

fn is_relevant(kind: &EventKind) -> bool {
    matches!(
        kind,
        EventKind::Modify(ModifyKind::Data(_))
            | EventKind::Modify(ModifyKind::Name(_))
            | EventKind::Modify(ModifyKind::Any)
            | EventKind::Modify(ModifyKind::Other)
            | EventKind::Create(_)
    )
}

fn canonicalize_or_self(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn paths_equal(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::sync::mpsc;
    use tempfile::tempdir;

    struct TestSink {
        tx: Mutex<mpsc::Sender<PathBuf>>,
    }

    impl EventSink for TestSink {
        fn emit_changed(&self, path: &Path) {
            let _ = self
                .tx
                .lock()
                .expect("test sink mutex")
                .send(path.to_path_buf());
        }
    }

    fn channel_watcher() -> (ProjectWatcher, mpsc::Receiver<PathBuf>) {
        let (tx, rx) = mpsc::channel();
        let sink = Arc::new(TestSink { tx: Mutex::new(tx) });
        (ProjectWatcher::with_sink(sink), rx)
    }

    fn touch(path: &Path, content: &str) {
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .expect("open temp file for write");
        f.write_all(content.as_bytes()).expect("write");
        f.sync_all().ok();
    }

    fn drain_for(rx: &mpsc::Receiver<PathBuf>, dur: Duration) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let deadline = Instant::now() + dur;
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(p) => out.push(p),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        out
    }

    #[test]
    fn single_path_emits_event() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("src.dxf");
        touch(&path, "0\nSECTION\n");

        let (mut watcher, rx) = channel_watcher();
        watcher
            .set_paths(vec![path.clone()])
            .expect("set_paths ok");

        std::thread::sleep(Duration::from_millis(100));

        touch(&path, "0\nSECTION\nMODIFIED\n");

        let received = rx
            .recv_timeout(Duration::from_millis(2000))
            .expect("event within 2s");
        assert!(
            paths_equal(&received, &path),
            "expected {} got {}",
            path.display(),
            received.display(),
        );
    }

    #[test]
    fn debounce_collapses_rapid_writes() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("src.svg");
        touch(&path, "<svg/>");

        let (mut watcher, rx) = channel_watcher();
        watcher
            .set_paths(vec![path.clone()])
            .expect("set_paths ok");

        std::thread::sleep(Duration::from_millis(100));

        for i in 0..5 {
            touch(&path, &format!("<svg data-rev=\"{i}\"/>"));
            std::thread::sleep(Duration::from_millis(20));
        }

        let events = drain_for(&rx, Duration::from_millis(600));
        let matching = events.iter().filter(|p| paths_equal(p, &path)).count();
        assert!(
            matching >= 1 && matching <= 2,
            "debounced 5 rapid writes; got {matching} events for path",
        );
    }

    #[test]
    fn set_paths_replaces_old_watches() {
        let dir = tempdir().expect("tempdir");
        let a = dir.path().join("a.dxf");
        let b = dir.path().join("b.dxf");
        touch(&a, "old");
        touch(&b, "old");

        let (mut watcher, rx) = channel_watcher();
        watcher.set_paths(vec![a.clone()]).expect("watch a");
        std::thread::sleep(Duration::from_millis(100));

        watcher.set_paths(vec![b.clone()]).expect("re-watch b");
        std::thread::sleep(Duration::from_millis(100));

        touch(&a, "modified");
        let events = drain_for(&rx, Duration::from_millis(600));
        let leaked = events.iter().filter(|p| paths_equal(p, &a)).count();
        assert_eq!(
            leaked, 0,
            "events for a leaked after re-watch (got {events:?})",
        );
    }

    #[test]
    fn unwatch_all_clears_set() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("a.dxf");
        touch(&path, "x");

        let (mut watcher, rx) = channel_watcher();
        watcher.set_paths(vec![path.clone()]).expect("set");
        watcher.unwatch_all().expect("unwatch");
        assert!(watcher.watched().is_empty());

        std::thread::sleep(Duration::from_millis(50));
        touch(&path, "y");
        let events = drain_for(&rx, Duration::from_millis(400));
        let leaked = events.iter().filter(|p| paths_equal(p, &path)).count();
        assert_eq!(leaked, 0, "unwatched path still emitting");
    }
}
