//! Tracks GitHub PRs opened by Claude Code sessions, attributed per Warp pane.
//!
//! A per-account `PostToolUse` hook (`~/.warp-claude/record-pr.sh`) appends each
//! PR URL created via `gh pr create` to `~/.warp-claude-prs/<WARP_TERMINAL_SESSION_UUID>`
//! (one URL per line). This model watches that directory and exposes, per pane
//! session uuid, the list of PRs that session opened — which the vertical tabs
//! use to show a PR badge per session, correctly attributed even when a session
//! works across multiple repositories.
//!
//! Keyed on the stable per-pane uuid, so a tab keeps its PRs across restart and a
//! forked tab (new pane, new uuid) starts empty. Entries are never cleared or
//! re-validated — a PR stays for the life of the tab even after it merges.

use std::collections::HashMap;
use std::path::PathBuf;

use warpui::{Entity, ModelContext, SingletonEntity};

#[cfg(not(target_family = "wasm"))]
use std::time::Duration;
#[cfg(not(target_family = "wasm"))]
use notify_debouncer_full::notify::{RecursiveMode, WatchFilter};
#[cfg(not(target_family = "wasm"))]
use warpui::ModelHandle;
#[cfg(not(target_family = "wasm"))]
use watcher::{BulkFilesystemWatcher, BulkFilesystemWatcherEvent};

#[cfg(not(target_family = "wasm"))]
const DEBOUNCE_MILLI_SECS: u64 = 500;

fn prs_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|home| home.join(".warp-claude-prs"))
}

pub(crate) struct ClaudePrAttributionModel {
    /// `hex(session_uuid)` -> PR URLs in recorded order.
    prs: HashMap<String, Vec<String>>,
    #[cfg(not(target_family = "wasm"))]
    _watcher: Option<ModelHandle<BulkFilesystemWatcher>>,
}

impl ClaudePrAttributionModel {
    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn new(ctx: &mut ModelContext<Self>) -> Self {
        let mut model = Self {
            prs: HashMap::new(),
            _watcher: None,
        };
        let Some(dir) = prs_dir() else {
            return model;
        };
        let _ = std::fs::create_dir_all(&dir);
        model.reload();

        let watcher = ctx.add_model(|ctx| {
            BulkFilesystemWatcher::new(Duration::from_millis(DEBOUNCE_MILLI_SECS), ctx)
        });
        ctx.subscribe_to_model(&watcher, Self::handle_fs_event);
        let registration = watcher.update(ctx, |watcher, _ctx| {
            watcher.register_path(&dir, WatchFilter::accept_all(), RecursiveMode::NonRecursive)
        });
        ctx.spawn(registration, |_, result, _ctx| {
            if let Err(err) = result {
                log::warn!("Failed to watch ~/.warp-claude-prs: {err:?}");
            }
        });
        model._watcher = Some(watcher);
        model
    }

    #[cfg(target_family = "wasm")]
    pub(crate) fn new(_ctx: &mut ModelContext<Self>) -> Self {
        Self {
            prs: HashMap::new(),
        }
    }

    /// Test-only constructor that registers no filesystem watcher, so tests that
    /// build a `Workspace` (which subscribes to this singleton) don't spawn a
    /// real watcher or touch `~/.warp-claude-prs`.
    #[cfg(test)]
    pub(crate) fn new_for_testing(_ctx: &mut ModelContext<Self>) -> Self {
        Self {
            prs: HashMap::new(),
            _watcher: None,
        }
    }

    #[cfg(not(target_family = "wasm"))]
    fn reload(&mut self) {
        self.prs.clear();
        let Some(dir) = prs_dir() else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(uuid) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Ok(contents) = std::fs::read_to_string(&path) else {
                continue;
            };
            // Each line is `<url>` (older files may have a trailing `\t<title>`,
            // which we ignore — only the URL field matters).
            let urls: Vec<String> = contents
                .lines()
                .map(|line| line.split('\t').next().unwrap_or(line).trim())
                .filter(|url| !url.is_empty())
                .map(str::to_owned)
                .collect();
            if !urls.is_empty() {
                self.prs.insert(uuid.to_owned(), urls);
            }
        }
    }

    #[cfg(not(target_family = "wasm"))]
    fn handle_fs_event(
        &mut self,
        _: ModelHandle<BulkFilesystemWatcher>,
        _event: &BulkFilesystemWatcherEvent,
        ctx: &mut ModelContext<Self>,
    ) {
        self.reload();
        ctx.notify();
        // Notify observers (the workspace re-renders so PR chips update live).
        ctx.emit(());
    }

    /// PR URLs recorded for the given pane session uuid (raw bytes), in the order
    /// they were opened. Empty when the session opened no PRs.
    pub(crate) fn prs_for(&self, session_uuid: &[u8]) -> Vec<String> {
        self.prs
            .get(&hex::encode(session_uuid))
            .cloned()
            .unwrap_or_default()
    }

    /// Like [`Self::prs_for`] but keyed by the already-hex-encoded session uuid.
    pub(crate) fn prs_for_hex(&self, session_uuid_hex: &str) -> Vec<String> {
        self.prs.get(session_uuid_hex).cloned().unwrap_or_default()
    }
}

/// Appends `url` to `~/.warp-claude-prs/<uuid_hex>` unless already present
/// (deduped by the first tab-separated field, tolerating legacy `\t<title>`
/// lines). Creates the directory/file as needed; the watcher reloads so chips
/// update live. Used by the "Assign PRs" modal.
#[cfg(not(target_family = "wasm"))]
pub(crate) fn add_recorded_pr(uuid_hex: &str, url: &str) {
    use std::io::Write as _;
    let Some(dir) = prs_dir() else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(uuid_hex);
    let already = std::fs::read_to_string(&path)
        .map(|contents| {
            contents
                .lines()
                .any(|line| line.split('\t').next().unwrap_or(line).trim() == url)
        })
        .unwrap_or(false);
    if already {
        return;
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(file, "{url}");
    }
}

/// Removes `url` from `~/.warp-claude-prs/<uuid_hex>`, deleting the file when it
/// becomes empty. The watcher reloads so the chip disappears.
#[cfg(not(target_family = "wasm"))]
pub(crate) fn remove_recorded_pr(uuid_hex: &str, url: &str) {
    let Some(dir) = prs_dir() else {
        return;
    };
    let path = dir.join(uuid_hex);
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return;
    };
    let remaining: Vec<&str> = contents
        .lines()
        .filter(|line| line.split('\t').next().unwrap_or(line).trim() != url)
        .collect();
    if remaining.is_empty() {
        let _ = std::fs::remove_file(&path);
    } else {
        let mut out = remaining.join("\n");
        out.push('\n');
        let _ = std::fs::write(&path, out);
    }
}

#[cfg(target_family = "wasm")]
pub(crate) fn add_recorded_pr(_uuid_hex: &str, _url: &str) {}

#[cfg(target_family = "wasm")]
pub(crate) fn remove_recorded_pr(_uuid_hex: &str, _url: &str) {}

impl Entity for ClaudePrAttributionModel {
    type Event = ();
}

impl SingletonEntity for ClaudePrAttributionModel {}
