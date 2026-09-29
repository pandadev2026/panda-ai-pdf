// Plan C for the "print to Stirling PDF" workflow (see
// windows/virtual-printer/README.md for why Plan B — a Print Support App v4
// virtual printer — is blocked). Instead of registering a printer, this
// watches the folder where the user saves output from Windows' built-in
// "Microsoft Print to PDF" printer and auto-opens new PDFs that land there.
//
// Deliberate limitation (accepted): this only runs while the app process is
// alive. There's no background service, so a PDF printed while Stirling PDF
// isn't running just sits in the folder until next launch, at which point
// the startup catch-up scan picks it up.

#[cfg(target_os = "windows")]
mod windows_impl {
    use crate::commands::window::{forward_files_to_window, target_window_label, MAIN_WINDOW_LABEL};
    use crate::utils::{add_log, app_data_dir};
    use notify::{EventKind, RecursiveMode, Watcher};
    use serde::{Deserialize, Serialize};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use tauri::{AppHandle, Manager};

    // Where we ask the user to point "Microsoft Print to PDF"'s save dialog.
    const INBOX_SUBDIR: [&str; 2] = ["Stirling PDF", "Print Inbox"];
    const STATE_FILE_NAME: &str = "print-inbox-state.json";

    // How long a file's size is allowed to keep changing before we give up on
    // it (Microsoft Print to PDF writes the whole file in one go, so a real
    // print job stabilizes in well under this).
    const STABLE_POLL_INTERVAL: Duration = Duration::from_millis(250);
    const STABLE_POLL_ATTEMPTS: u32 = 20; // ~5s worst case

    #[derive(Serialize, Deserialize, Default)]
    struct State {
        // Only files newer than this (mtime, unix seconds) get opened. Persisted
        // so a restart doesn't re-open everything already handled, and so the
        // startup catch-up scan knows what it missed while the app was closed.
        last_processed_unix_secs: u64,
    }

    fn print_inbox_dir(app: &AppHandle) -> Option<PathBuf> {
        let mut dir = app.path().document_dir().ok()?;
        for part in INBOX_SUBDIR {
            dir.push(part);
        }
        Some(dir)
    }

    fn state_file_path() -> PathBuf {
        app_data_dir().join(STATE_FILE_NAME)
    }

    fn load_state(path: &Path) -> Option<State> {
        let data = fs::read_to_string(path).ok()?;
        serde_json::from_str(&data).ok()
    }

    fn save_state(path: &Path, state: &State) {
        let Ok(json) = serde_json::to_string(state) else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Err(e) = fs::write(path, json) {
            add_log(format!(
                "⚠️ Print Inbox: failed to persist state to {}: {}",
                path.display(),
                e
            ));
        }
    }

    fn now_unix_secs() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    fn mtime_unix_secs(path: &Path) -> Option<u64> {
        fs::metadata(path)
            .ok()?
            .modified()
            .ok()?
            .duration_since(UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs())
    }

    fn is_pdf(path: &Path) -> bool {
        path.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
    }

    // Poll the file's size until it stops changing, so we don't open a PDF
    // mid-write. Returns false if the file vanished or never settled.
    fn wait_until_stable(path: &Path) -> bool {
        let mut last_len = match fs::metadata(path) {
            Ok(m) => m.len(),
            Err(_) => return false,
        };
        for _ in 0..STABLE_POLL_ATTEMPTS {
            thread::sleep(STABLE_POLL_INTERVAL);
            let len = match fs::metadata(path) {
                Ok(m) => m.len(),
                Err(_) => return false,
            };
            if len == last_len && len > 0 {
                return true;
            }
            last_len = len;
        }
        false
    }

    fn open_pdf(app: &AppHandle, path: &Path) {
        let Some(path_str) = path.to_str() else {
            add_log(format!(
                "⚠️ Print Inbox: skipping non-UTF8 path {}",
                path.display()
            ));
            return;
        };
        add_log(format!("🖨️ Print Inbox: opening {}", path_str));
        let label = target_window_label(app).unwrap_or_else(|| MAIN_WINDOW_LABEL.to_string());
        forward_files_to_window(app, &label, vec![path_str.to_string()]);
    }

    // Startup catch-up: open anything dropped in the folder while the app
    // wasn't running, oldest first so they open in print order.
    fn scan_missed(app: &AppHandle, dir: &Path, last_processed: &mut u64, state_path: &Path) {
        let entries = match fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) => {
                add_log(format!(
                    "⚠️ Print Inbox: failed to read {}: {}",
                    dir.display(),
                    e
                ));
                return;
            }
        };

        let mut missed: Vec<(PathBuf, u64)> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| is_pdf(p))
            .filter_map(|p| mtime_unix_secs(&p).map(|m| (p, m)))
            .filter(|(_, mtime)| *mtime > *last_processed)
            .collect();
        missed.sort_by_key(|(_, mtime)| *mtime);

        if !missed.is_empty() {
            add_log(format!(
                "🖨️ Print Inbox: catching up on {} file(s) from while the app was closed",
                missed.len()
            ));
        }

        for (path, mtime) in missed {
            if !wait_until_stable(&path) {
                continue;
            }
            open_pdf(app, &path);
            if mtime > *last_processed {
                *last_processed = mtime;
                save_state(state_path, &State { last_processed_unix_secs: *last_processed });
            }
        }
    }

    fn handle_created(app: &AppHandle, path: &Path, last_processed: &mut u64, state_path: &Path) {
        if !is_pdf(path) {
            return;
        }
        if !wait_until_stable(path) {
            add_log(format!(
                "⚠️ Print Inbox: {} never stabilized, skipping",
                path.display()
            ));
            return;
        }
        let Some(mtime) = mtime_unix_secs(path) else {
            return;
        };
        // Already handled (e.g. a duplicate Create event) - skip.
        if mtime <= *last_processed {
            return;
        }
        open_pdf(app, path);
        *last_processed = mtime;
        save_state(state_path, &State { last_processed_unix_secs: mtime });
    }

    pub fn start(app: AppHandle) {
        thread::spawn(move || {
            let Some(dir) = print_inbox_dir(&app) else {
                add_log(
                    "⚠️ Print Inbox: could not resolve the Documents folder, watcher disabled"
                        .to_string(),
                );
                return;
            };
            if let Err(e) = fs::create_dir_all(&dir) {
                add_log(format!(
                    "⚠️ Print Inbox: failed to create {}: {}",
                    dir.display(),
                    e
                ));
                return;
            }

            let state_path = state_file_path();
            let mut last_processed = match load_state(&state_path) {
                Some(state) => state.last_processed_unix_secs,
                None => {
                    // First run of this feature: don't bulk-open whatever
                    // already happens to be sitting in the folder - just start
                    // tracking from now.
                    let now = now_unix_secs();
                    save_state(&state_path, &State { last_processed_unix_secs: now });
                    now
                }
            };

            scan_missed(&app, &dir, &mut last_processed, &state_path);

            let (tx, rx) = std::sync::mpsc::channel();
            let mut watcher = match notify::recommended_watcher(tx) {
                Ok(w) => w,
                Err(e) => {
                    add_log(format!("⚠️ Print Inbox: failed to create watcher: {}", e));
                    return;
                }
            };
            if let Err(e) = watcher.watch(&dir, RecursiveMode::NonRecursive) {
                add_log(format!(
                    "⚠️ Print Inbox: failed to watch {}: {}",
                    dir.display(),
                    e
                ));
                return;
            }
            add_log(format!("🖨️ Print Inbox: watching {}", dir.display()));

            // React only to Create - Microsoft Print to PDF creates the
            // destination file once when the save starts, so this fires
            // exactly once per print job. Reacting to Modify too would just
            // mean handling the same file multiple times as it's written.
            for res in rx {
                let Ok(event) = res else { continue };
                if !matches!(event.kind, EventKind::Create(_)) {
                    continue;
                }
                for path in &event.paths {
                    handle_created(&app, path, &mut last_processed, &state_path);
                }
            }
            // The loop only exits if every Sender is dropped, which happens
            // when `watcher` itself is dropped - i.e. never, since it's held
            // in this stack frame for the life of the thread.
        });
    }
}

#[cfg(target_os = "windows")]
pub use windows_impl::start as start_print_inbox_watcher;

// Microsoft Print to PDF (and this whole plan) is Windows-only.
#[cfg(not(target_os = "windows"))]
pub fn start_print_inbox_watcher(_app: tauri::AppHandle) {}
