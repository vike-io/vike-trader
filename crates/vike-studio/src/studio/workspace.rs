//! Workspace persistence: a small `StudioWorkspace` snapshot of restorable UI state (which
//! right-hand tab is open, the editor/tools collapsed flags, the template dropdown selection,
//! the editor's last source, and the strategy source that says which language that buffer is —
//! with the strategy-file pane selection it needs to mean the same thing after a restart).
//!
//! **Where it lives: `<project>/settings/state/studio_workspace.json`**, not beside the hist store:
//! which tab is open describes THIS USER's screen, not the data in that store. The old
//! `<store_root>/studio_workspace.json` is read by nothing (decision 0117).
//!
//! [`state_dir`] resolves it: the directory the composition root's boot resolved, declared once
//! through [`declare_workspace_state_dir`] (the desktop hands it `vike_boot::Booted::state_dir`,
//! the only answer that honours `$VIKE_SETTINGS_DIR`), otherwise `<project>/settings/state` found
//! by walking up from the working directory ([`project_state_dir`]) for a process that declares
//! nothing — a test or an example. There is no second location and no environment rung:
//! `VIKE_STATE_ROOT` refuses startup (decision 0111).
//!
//! `load_workspace`/`save_workspace` are pure, mirroring `saved::{load_strategies,
//! save_strategies}`: a missing file yields `StudioWorkspace::default()`, and a file that is not read
//! (an older build's, or a corrupt one) is an `Err` whose hint the Studio shows while it opens on
//! the defaults and writes nothing over the file. With no resolvable state directory there is
//! nothing to read and nothing is written ([`write_workspace_file`]).
//!
//! **The writes go through [`StudioWorkspaceSaver`]**: debounced ([`WORKSPACE_SAVE_DEBOUNCE`]) and
//! off the paint thread, with a blocking [`StudioWorkspaceSaver::flush`] for the Studio window's
//! close and the app's exit. A failed write is REPORTED, never swallowed: the Studio shows its
//! sentence and the flush returns it to the desktop, which logs it (this crate links no logger).
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::panes::param_scan::{StudioParamRow, StudioTestConditions};
use crate::panes::saved::{OLD_FILE_WORD, RETIRED_RUST_TAG, StudioStrategySource, old_word_in};
use crate::studio::StudioTab;

/// The workspace snapshot's basename, stated once for the read and the write.
pub(crate) const WORKSPACE_FILE: &str = "studio_workspace.json";

/// `<project>/settings/state` exactly as the composition root's boot resolved it, set once by
/// [`declare_workspace_state_dir`]. `None` inside the cell is a legitimate answer (the root booted
/// and found no project); an UNSET cell means no root declared, which is a test or an example.
static DECLARED_STATE_DIR: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();

/// **Declare the state directory the composition root already resolved** — call it with
/// `vike_boot::Booted::state_dir`, once, before the Studio is constructed.
///
/// A declaration rather than a constructor parameter for the reason
/// `vike_app_core::ui::window::workspace::declare_project_state_dir` gives for the desktop's own
/// workspace family: the answer is a property of the PROCESS (its one boot walk), and this library
/// may not resolve it from the environment itself.
///
/// # Errors
/// A second declaration naming a DIFFERENT directory is refused and changes nothing — one process
/// has one workspace directory. Repeating the declaration in force is not a fault.
pub fn declare_workspace_state_dir(state_dir: Option<PathBuf>) -> Result<(), String> {
    let declared = DECLARED_STATE_DIR.get_or_init(|| state_dir.clone());
    if declared.as_deref() == state_dir.as_deref() {
        return Ok(());
    }
    let label = |d: Option<&Path>| {
        d.map_or_else(|| "<no project>".to_string(), |p| p.display().to_string())
    };
    Err(format!(
        "the Studio's workspace directory was already declared as {}; the second declaration ({}) \
         is IGNORED — one process has one workspace directory",
        label(declared.as_deref()),
        label(state_dir.as_deref())
    ))
}

/// `<project>/settings/state` — found by walking UP from the working directory for the project
/// marker, for a process whose root declared nothing. `None` when no project sits above it.
fn project_state_dir() -> Option<PathBuf> {
    std::env::current_dir().ok().and_then(|cwd| vike_paths::state_path::project_state_dir(&cwd))
}

/// The state directory the snapshot lives in: the root's declaration, else the walk. There is no
/// third.
fn state_dir() -> Option<PathBuf> {
    match DECLARED_STATE_DIR.get() {
        Some(declared) => declared.clone(),
        None => project_state_dir(),
    }
}

/// Where to READ the workspace snapshot from: `<state_dir>/studio_workspace.json`. `None` when no
/// state directory resolves — there is no file to read then, and the caller restores the default.
pub(crate) fn workspace_read_path() -> Option<PathBuf> {
    Some(state_dir()?.join(WORKSPACE_FILE))
}

/// Where to WRITE it: `<state_dir>/studio_workspace.json`, the directory created lazily.
///
/// `Err` when the state directory cannot be resolved or created, or when the state path is a
/// SYMLINK, which `vike_paths::state_path::write_path` refuses rather than truncating the
/// link's target. [`write_workspace_file`] turns it into the sentence the Studio reports.
pub(crate) fn workspace_write_path() -> std::io::Result<PathBuf> {
    vike_paths::state_path::write_path(state_dir().as_deref(), WORKSPACE_FILE)
}

/// **The real file's writer**, which [`StudioWorkspaceSaver`] runs off the paint thread: `Ok` with
/// nothing written when no state directory resolves (no project above this process, so nothing was
/// read either), else [`save_workspace`] at [`workspace_write_path`], every failure a sentence
/// naming the file.
pub(crate) fn write_workspace_file(ws: &StudioWorkspace) -> Result<(), String> {
    if state_dir().is_none() {
        return Ok(());
    }
    let not_saved = |e: std::io::Error| format!("the Studio workspace was not saved: {e}");
    let path = workspace_write_path().map_err(not_saved)?;
    save_workspace(&path, ws)
        .map_err(|e| format!("the Studio workspace was not saved to {}: {e}", path.display()))
}

/// How long the snapshot must hold still before it is written: typing writes once per pause,
/// never once per keystroke.
pub(crate) const WORKSPACE_SAVE_DEBOUNCE: Duration = Duration::from_millis(1500);

/// How often a frame looks for a write's answer while one is in flight.
const WORKSPACE_SAVE_POLL: Duration = Duration::from_millis(50);

/// **The snapshot's writer: debounced, and off the paint thread.**
///
/// The Studio used to replace the file ([`save_workspace`], which fsyncs) on the paint thread at
/// every drift, so each editor keystroke stalled a frame on a slow disk. Now a change is written
/// once the snapshot has held still for [`WORKSPACE_SAVE_DEBOUNCE`], on a thread of its own, one
/// write at a time (so two writes never race for the file); [`Self::flush`] writes whatever is
/// still pending and waits for it. The bytes are [`save_workspace`]'s, unchanged.
///
/// ⚠ **An edit is never dropped**: `written` moves only when a write SUCCEEDS, so a failed one
/// stays pending (tried again after another quiet period, and by [`Self::flush`]) and its sentence
/// is held in [`Self::error`] until a write succeeds.
pub(crate) struct StudioWorkspaceSaver {
    /// What the file holds: the last snapshot written, or the one the session opened on.
    written: StudioWorkspace,
    /// The newest snapshot seen, and since when: the quiet period runs from there.
    seen: (StudioWorkspace, Instant),
    /// The write in flight: the snapshot it writes, and where its answer arrives.
    in_flight: Option<(StudioWorkspace, Receiver<Result<(), String>>)>,
    /// Why the last write failed, until one succeeds.
    pub(crate) error: Option<String>,
    /// The writer: [`write_workspace_file`] in the Studio, a stand-in in a test.
    write: fn(&StudioWorkspace) -> Result<(), String>,
}

impl StudioWorkspaceSaver {
    /// A saver whose file holds `written` (what the session opened on), writing through `write`.
    pub(crate) fn new(
        written: StudioWorkspace,
        write: fn(&StudioWorkspace) -> Result<(), String>,
    ) -> Self {
        Self {
            seen: (written.clone(), Instant::now()),
            written,
            in_flight: None,
            error: None,
            write,
        }
    }

    /// One frame at `now` with the state `current`: fold in a finished write, note a change, and
    /// start the write once `current` has held still for [`WORKSPACE_SAVE_DEBOUNCE`]. Returns how
    /// soon the caller needs another frame for it (`None`: nothing pending), since egui draws none
    /// unprompted.
    pub(crate) fn tick(&mut self, current: StudioWorkspace, now: Instant) -> Option<Duration> {
        let answer = self.in_flight.as_ref().and_then(|(_, rx)| match rx.try_recv() {
            Ok(answer) => Some(answer),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(WRITER_GONE.to_string())),
        });
        if let Some(answer) = answer
            && let Some((ws, _)) = self.in_flight.take()
        {
            self.settle(ws, answer, now);
        }
        if current != self.seen.0 {
            self.seen = (current, now);
        }
        if self.in_flight.is_some() {
            return Some(WORKSPACE_SAVE_POLL);
        }
        if self.seen.0 == self.written {
            // The file holds what is on screen: an earlier failure is no longer owed anything.
            self.error = None;
            return None;
        }
        let quiet = now.saturating_duration_since(self.seen.1);
        if quiet < WORKSPACE_SAVE_DEBOUNCE {
            return Some(WORKSPACE_SAVE_DEBOUNCE - quiet);
        }
        let ws = self.seen.0.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let write = self.write;
        let job = ws.clone();
        let spawned = std::thread::Builder::new()
            .name("studio-ws-save".into())
            .spawn(move || tx.send(write(&job)));
        match spawned {
            Ok(_) => self.in_flight = Some((ws, rx)),
            Err(e) => {
                let why = format!("the Studio workspace was not saved: {e}");
                self.settle(ws, Err(why), now);
            }
        }
        Some(WORKSPACE_SAVE_POLL)
    }

    /// **Write what is still pending NOW, and wait for it** — the Studio window's close and the
    /// app's exit, the moments a pending edit would otherwise be lost. A write in flight is waited
    /// for first. `Err` is the sentence of a write that failed, for the caller to log.
    pub(crate) fn flush(&mut self, current: StudioWorkspace) -> Result<(), String> {
        let now = Instant::now();
        if let Some((ws, rx)) = self.in_flight.take() {
            let answer = rx.recv().unwrap_or_else(|_| Err(WRITER_GONE.to_string()));
            self.settle(ws, answer, now);
        }
        self.seen = (current.clone(), now);
        if current == self.written {
            self.error = None;
            return Ok(());
        }
        let answer = (self.write)(&current);
        self.settle(current, answer.clone(), now);
        answer
    }

    /// Fold in a write's answer: a success is what the file holds now; a failure keeps the edit
    /// pending, holds its sentence and restarts the quiet period, so it is tried again after it.
    fn settle(&mut self, ws: StudioWorkspace, answer: Result<(), String>, now: Instant) {
        match answer {
            Ok(()) => {
                self.written = ws;
                self.error = None;
            }
            Err(why) => {
                self.error = Some(why);
                self.seen.1 = now;
            }
        }
    }
}

/// A writer thread that ended without answering (it panicked): the write is not known to have
/// landed, so it counts as failed and stays pending.
const WRITER_GONE: &str =
    "the Studio workspace was not saved: its writer stopped without answering";

/// A snapshot of the Studio's restorable UI state — everything `StudioState::new_with_qa` needs to
/// reopen where the user left off.
///
/// An unknown key refuses the file ([`load_workspace`]): every file an older build wrote says
/// `template_idx` (one of the original five keys), so it is never read with an empty field. A key
/// marked `#[serde(default)]` may be absent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct StudioWorkspace {
    pub(crate) right_tab: StudioTab,
    pub(crate) editor_collapsed: bool,
    pub(crate) tools_collapsed: bool,
    /// The template dropdown's selection, an index into `vike_rhai::TEMPLATES`.
    pub(crate) template_index: usize,
    /// The editor buffer's source as of the last persist. Empty (the `Default` value) is
    /// treated by the caller as "nothing to restore" — keep the default starter script rather
    /// than blanking the editor.
    pub(crate) editor_source: String,
    /// Which language [`Self::editor_source`] is — the Strategy pane's source selection. Absent, it
    /// reads as [`StudioStrategySource::Rhai`].
    ///
    /// ⚠ It rides WITH the buffer because the buffer alone is not a restorable fact: persisted
    /// without it, a Strategy file author's Rust source came back in Rhai mode.
    #[serde(default)]
    pub(crate) strategy_source: StudioStrategySource,
    /// The strategy-file pane's selection, by the file's PATH under the server's folder. Empty:
    /// nothing is selected until the list is fetched and a file picked.
    #[serde(default)]
    pub(crate) picked_file: String,
    /// The strategy-file pane's `(key, value-text)` param rows, verbatim.
    #[serde(default)]
    pub(crate) file_params: Vec<(String, String)>,
    /// The path the Rust buffer is built as. Its last-built SHA is deliberately NOT here: a sha
    /// with no recorded source is a sha the staleness guard cannot check — `StudioState`'s
    /// constructor says why a restored Rust session starts unbuilt instead.
    #[serde(default)]
    pub(crate) rust_file: String,
    /// The editor panel's width, where its splitter was left. Absent, the theme's default.
    #[serde(default)]
    pub(crate) editor_width: Option<f32>,
    /// The Param scan pane's strategy-parameter rows (each one's value, scan tick and range), by
    /// strategy: `StudioState::param_key`.
    #[serde(default)]
    pub(crate) param_rows: std::collections::BTreeMap<String, Vec<StudioParamRow>>,
    /// The test conditions a Run, a scan and a walk-forward share.
    #[serde(default)]
    pub(crate) test_conditions: StudioTestConditions,
    /// Whether [`Self::editor_source`] is Rust where [`Self::strategy_source`] does not say: Strategy
    /// files shows the buffer it found, so a restart there must not judge Rust as Rhai.
    #[serde(default)]
    pub(crate) buffer_is_rust: bool,
    /// The other language's buffer, parked while this one is on screen (`StudioState::switch_source`).
    #[serde(default)]
    pub(crate) parked_source: Option<String>,
}

/// The words only a `studio_workspace.json` an older build wrote holds — keys (`template_idx`, the
/// two `native_*` keys, the retired slippage switch's `slippage`: a run's slippage is a strategy
/// param since the owner's ruling of 2026-11-18) and values (the old strategy-source words, the
/// slippage switch's variant names) — spelled with `concat!` where a sweep could rewrite them, as
/// `RETIRED_RUST_TAG` is.
const OLD_WORKSPACE_WORDS: [&str; 10] = [
    concat!("template", "_idx"),
    concat!("native", "_strategy"),
    concat!("native", "_params"),
    OLD_FILE_WORD,
    RETIRED_RUST_TAG,
    "Off",
    "Percent",
    "Ticks",
    concat!("Market", "Impact"),
    "slippage",
];

/// Load the workspace snapshot from `path`. A missing file is `StudioWorkspace::default()`.
///
/// ⚠ **A file that is not read is an `Err` with its one-line reason**, and the caller opens on
/// the default WITHOUT writing over it (`StudioState::new_with_qa`): the snapshot holds the editor
/// buffer, which for a Rust author may be the only copy. A file an older build wrote (it names one
/// of `OLD_WORKSPACE_WORDS`) is the expected case, refused and never converted, and its hint says
/// so.
pub(crate) fn load_workspace(path: &Path) -> Result<StudioWorkspace, String> {
    let Ok(text) = std::fs::read_to_string(path) else { return Ok(StudioWorkspace::default()) };
    serde_json::from_str(&text).map_err(|e| match old_word_in(&e, &OLD_WORKSPACE_WORDS) {
        Some(word) => format!(
            "{} was written by an older Vike Trader (it says `{word}`) and is not read, so this \
             session starts on the defaults and writes nothing over it: copy your editor buffer \
             out of it if you need it, then move it away.",
            path.display()
        ),
        None => format!(
            "{} was not read ({e}), so this session starts on the defaults and writes nothing \
             over it: fix or move the file to keep your layout and editor buffer.",
            path.display()
        ),
    })
}

/// Write the workspace snapshot to `path` as pretty JSON, replacing any existing file whole through
/// `vike_utility::atomic_file`'s `write_atomic`: a crash mid-save leaves the previous snapshot (and
/// its editor buffer), never a truncated file.
pub(crate) fn save_workspace(path: &Path, ws: &StudioWorkspace) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(ws).unwrap_or_else(|_| "{}".to_string());
    vike_utility::atomic_file::write_atomic(path, text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A snapshot whose editor buffer is `text`: one keystroke's worth of state.
    fn typed(text: &str) -> StudioWorkspace {
        StudioWorkspace { editor_source: text.to_string(), ..Default::default() }
    }

    /// **Fifty keystrokes are ONE write**, started only once the buffer has held still for
    /// [`WORKSPACE_SAVE_DEBOUNCE`], made OFF the calling thread, and holding the last keystroke.
    #[test]
    fn many_keystrokes_are_one_write_after_the_quiet_period() {
        static WRITES: std::sync::Mutex<Vec<(String, Option<String>)>> =
            std::sync::Mutex::new(Vec::new());
        fn record(ws: &StudioWorkspace) -> Result<(), String> {
            let thread = std::thread::current().name().map(str::to_string);
            WRITES.lock().unwrap().push((ws.editor_source.clone(), thread));
            Ok(())
        }
        let mut saver = StudioWorkspaceSaver::new(StudioWorkspace::default(), record);
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut text = String::new();
        for key in 0..50 {
            text.push('x');
            // A keystroke every 100 ms: each restarts the quiet period.
            let wait = saver.tick(typed(&text), at(key * 100));
            assert_eq!(wait, Some(WORKSPACE_SAVE_DEBOUNCE), "keystroke {key}");
        }
        let last = at(49 * 100);
        let almost = last + WORKSPACE_SAVE_DEBOUNCE - Duration::from_millis(1);
        assert_eq!(saver.tick(typed(&text), almost), Some(Duration::from_millis(1)));
        assert!(WRITES.lock().unwrap().is_empty(), "nothing is written inside the quiet period");

        let quiet = last + WORKSPACE_SAVE_DEBOUNCE;
        assert_eq!(saver.tick(typed(&text), quiet), Some(WORKSPACE_SAVE_POLL), "the write starts");
        // `flush` waits for the write in flight, and finds nothing else owed.
        assert_eq!(saver.flush(typed(&text)), Ok(()));
        let writes = WRITES.lock().unwrap().clone();
        assert_eq!(writes, vec![(text.clone(), Some("studio-ws-save".to_string()))]);
        assert_eq!(saver.tick(typed(&text), quiet + WORKSPACE_SAVE_DEBOUNCE), None, "all written");
    }

    /// **The exit save flushes**: an edit still inside its quiet period when the window closes or
    /// the app exits is written by `flush`, after any write in flight, and only once.
    #[test]
    fn the_exit_flush_writes_what_is_pending_after_the_write_in_flight() {
        static WRITES: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        fn record(ws: &StudioWorkspace) -> Result<(), String> {
            WRITES.lock().unwrap().push(ws.editor_source.clone());
            Ok(())
        }
        let mut saver = StudioWorkspaceSaver::new(StudioWorkspace::default(), record);
        let t0 = Instant::now();
        saver.tick(typed("a"), t0);
        saver.tick(typed("a"), t0 + WORKSPACE_SAVE_DEBOUNCE); // "a" is in flight
        saver.tick(typed("ab"), t0 + WORKSPACE_SAVE_DEBOUNCE); // "ab" waits out its quiet period

        assert_eq!(saver.flush(typed("ab")), Ok(()));
        assert_eq!(*WRITES.lock().unwrap(), vec!["a".to_string(), "ab".to_string()]);
        assert_eq!(saver.flush(typed("ab")), Ok(()), "nothing is owed twice");
        assert_eq!(saver.tick(typed("ab"), Instant::now()), None);
        assert_eq!(WRITES.lock().unwrap().len(), 2);
    }

    /// **A failed write is REPORTED, never swallowed**, and the edit stays pending: the flush
    /// returns the failure, the saver holds it for the Studio to show, and the next frame still owes
    /// the write.
    #[test]
    fn a_failed_write_is_reported_and_the_edit_stays_pending() {
        static TRIES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        fn refuse(_: &StudioWorkspace) -> Result<(), String> {
            TRIES.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err("disk full".to_string())
        }
        let mut saver = StudioWorkspaceSaver::new(StudioWorkspace::default(), refuse);
        let t0 = Instant::now();
        saver.tick(typed("edit"), t0);
        saver.tick(typed("edit"), t0 + WORKSPACE_SAVE_DEBOUNCE); // the write starts, and fails

        assert_eq!(saver.flush(typed("edit")), Err("disk full".to_string()));
        assert_eq!(saver.error.as_deref(), Some("disk full"), "held for the Studio to show");
        let tries = TRIES.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(tries, 2, "the write in flight, then the flush's own");
        assert!(saver.tick(typed("edit"), Instant::now()).is_some(), "still owed");
    }

    #[test]
    fn default_workspace_has_sane_defaults() {
        let ws = StudioWorkspace::default();
        assert!(ws.editor_source.is_empty());
        assert_eq!(ws.right_tab, StudioTab::Sweep);
        assert!(!ws.editor_collapsed);
        assert!(!ws.tools_collapsed);
        assert_eq!(ws.template_index, 0);
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("studio_workspace.json");
        let ws = StudioWorkspace {
            right_tab: StudioTab::Saved,
            editor_collapsed: true,
            tools_collapsed: false,
            template_index: 3,
            editor_source: "pub fn build() {}".to_string(),
            strategy_source: StudioStrategySource::Rust,
            picked_file: "trend/buy_hold.rs".to_string(),
            file_params: vec![("size".to_string(), "2".to_string())],
            rust_file: "my/strat.rs".to_string(),
            editor_width: Some(612.0),
            param_rows: [(
                "trend/buy_hold.rs".to_string(),
                vec![StudioParamRow {
                    value: "7".to_string(),
                    ..StudioParamRow::scanned("fast", "5..30 step 5")
                }],
            )]
            .into(),
            test_conditions: StudioTestConditions {
                capital: "25000".to_string(),
                costs: vec![StudioParamRow {
                    name: "commission".to_string(),
                    value: "0.1".to_string(),
                    ..StudioParamRow::default()
                }],
                funding: true,
            },
            buffer_is_rust: true,
            parked_source: Some("fn on_bar() {}".to_string()),
        };

        save_workspace(&path, &ws).unwrap();
        let loaded = load_workspace(&path).expect("the file reads");

        assert_eq!(loaded, ws);
    }

    /// THE OWNER'S RULING OF 2026-11-17: a `studio_workspace.json` an older build wrote — the
    /// original five-key shape, the 11-17 shape (`native_*`, `"model": "MarketImpact"`), and one
    /// whose strategy source is the retired Rust tag — is REFUSED with a one-line hint naming the
    /// file and what to do; nothing in it is read, and the bytes stay as they were.
    #[test]
    fn an_old_studio_workspace_is_refused_and_left_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(WORKSPACE_FILE);
        let five_keys = r#"{
  "right_tab": "Saved",
  "editor_collapsed": true,
  "tools_collapsed": false,
  "template_idx": 2,
  "editor_source": "fn on_bar(strategy) {}"
}"#;
        let eleven_seventeen = r#"{
  "right_tab": "Strategy",
  "editor_collapsed": false,
  "tools_collapsed": false,
  "template_idx": 0,
  "editor_source": "const FAST = 5;",
  "strategy_source": "rust",
  "native_strategy": "",
  "native_params": [],
  "rust_file": "",
  "slippage": { "model": "MarketImpact", "value": 0.0, "tick": 0.0 }
}"#;
        let retired = format!(
            r#"{{ "right_tab": "Strategy", "editor_collapsed": false, "tools_collapsed": false,
  "editor_source": "pub fn build() {{}}", "strategy_source": "{RETIRED_RUST_TAG}" }}"#
        );
        for (old, word) in [
            (five_keys.to_string(), "template_idx"),
            (eleven_seventeen.to_string(), "template_idx"),
            (eleven_seventeen.replace("  \"template_idx\": 0,\n", ""), "native_strategy"),
            (retired, RETIRED_RUST_TAG),
        ] {
            std::fs::write(&path, &old).unwrap();
            let hint = load_workspace(&path).expect_err("an old file must be refused");
            assert_eq!(
                hint,
                format!(
                    "{} was written by an older Vike Trader (it says `{word}`) and is not read, so \
                     this session starts on the defaults and writes nothing over it: copy your \
                     editor buffer out of it if you need it, then move it away.",
                    path.display()
                )
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), old, "the file is untouched");
        }
    }

    /// A file written while the Strategy pane had its slippage switch (retired by the owner's ruling of
    /// 2026-11-18: a run's slippage is a strategy param) is an older build's file: REFUSED with the
    /// hint naming `slippage`, never read with the key dropped, and its bytes stay as they were. The
    /// same file without the key reads.
    #[test]
    fn a_file_with_the_retired_slippage_switch_is_an_older_builds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(WORKSPACE_FILE);
        let with = |slippage: &str| {
            format!(
                r#"{{ "right_tab": "Saved", "editor_collapsed": false, "tools_collapsed": false,
  "template_index": 1, "editor_source": "fn on_bar() {{}}", "strategy_source": "file",
  "picked_file": "buy_hold", "file_params": [["size", "2"]], "rust_file": "p.rs"{slippage} }}"#
            )
        };
        std::fs::write(&path, with("")).unwrap();
        let loaded = load_workspace(&path).expect("a file without the switch reads");
        assert_eq!((loaded.template_index, loaded.picked_file.as_str()), (1, "buy_hold"));
        let old = with(r#", "slippage": { "model": "percent", "value": 0.0005 }"#);
        std::fs::write(&path, &old).unwrap();
        let hint = load_workspace(&path).expect_err("the retired switch is an older build's key");
        assert!(hint.contains("it says `slippage`"), "{hint}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), old, "the file is untouched");
    }

    #[test]
    fn load_of_a_missing_path_is_default_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does_not_exist.json");
        assert_eq!(load_workspace(&path), Ok(StudioWorkspace::default()));
    }

    #[test]
    fn load_of_a_corrupt_file_is_refused_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("garbage.json");
        std::fs::write(&path, "{ not: valid json ]]]").unwrap();
        assert!(load_workspace(&path).is_err());
    }

    /// The snapshot lives in the project's `settings/state`, found from a nested working directory,
    /// and a save there is what the next read loads.
    ///
    /// Pure by construction: `project_state_dir` walks up for the project marker, so a throwaway
    /// project laid out under a temp dir answers without touching the real repo, the process CWD,
    /// or any environment variable.
    #[test]
    fn the_snapshot_lives_in_the_project_settings_state_dir() {
        assert_eq!(WORKSPACE_FILE, "studio_workspace.json");
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("proj");
        std::fs::create_dir_all(project.join("crates/vike-studio")).unwrap();
        std::fs::write(project.join("Cargo.toml"), "[workspace]\n").unwrap();

        // The resolver this module consults, and where it points…
        let proj_state = vike_paths::state_path::project_state_dir(&project).unwrap();
        assert_eq!(proj_state, project.join("settings").join("state"));
        // …found from a nested working directory, not just the project root.
        assert_eq!(
            vike_paths::state_path::project_state_dir(&project.join("crates/vike-studio")),
            Some(proj_state.clone())
        );

        // The write, spelled exactly as `workspace_write_path` composes it, creates the directory
        // lazily, and the read path `workspace_read_path` composes then loads it.
        let written =
            vike_paths::state_path::write_path(Some(&proj_state), WORKSPACE_FILE).unwrap();
        assert_eq!(written, proj_state.join(WORKSPACE_FILE));
        save_workspace(&written, &StudioWorkspace { template_index: 8, ..Default::default() })
            .unwrap();
        assert_eq!(load_workspace(&proj_state.join(WORKSPACE_FILE)).unwrap().template_index, 8);
    }

    /// A state directory that cannot be created (here: a plain FILE sits where it should be) is an
    /// `Err` the caller swallows: the save is skipped, never a panic and never a second location.
    #[test]
    fn an_unusable_state_dir_is_an_error_not_a_second_location() {
        let root = tempfile::tempdir().unwrap();
        let blocker = root.path().join("blocker");
        std::fs::write(&blocker, "not a directory").unwrap();

        let blocked = blocker.join("state");
        assert!(
            vike_paths::state_path::write_path(Some(blocked.as_path()), WORKSPACE_FILE).is_err()
        );
    }

    #[test]
    fn save_overwrites_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("studio_workspace.json");
        save_workspace(&path, &StudioWorkspace { template_index: 1, ..Default::default() })
            .unwrap();
        save_workspace(&path, &StudioWorkspace { template_index: 2, ..Default::default() })
            .unwrap();
        assert_eq!(
            load_workspace(&path),
            Ok(StudioWorkspace { template_index: 2, ..Default::default() })
        );
    }

    /// A save never truncates and rewrites the previous snapshot in place (it writes a sibling and
    /// renames it over), so a save interrupted at any point before that rename leaves it whole.
    /// Measured through a handle opened on the previous file before the save: an in-place rewrite
    /// (`std::fs::write`, the old body) changes what it reads; a replace leaves it the old bytes.
    #[cfg(unix)]
    #[test]
    fn an_interrupted_save_leaves_the_previous_snapshot_intact() {
        use std::io::Read;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("studio_workspace.json");
        save_workspace(&path, &StudioWorkspace { template_index: 1, ..Default::default() })
            .unwrap();
        let old = std::fs::read_to_string(&path).unwrap();
        let mut previous = std::fs::File::open(&path).unwrap();
        save_workspace(&path, &StudioWorkspace { template_index: 2, ..Default::default() })
            .unwrap();
        let mut kept = String::new();
        previous.read_to_string(&mut kept).unwrap();
        assert_eq!(kept, old, "the previous file was rewritten in place");
        assert_eq!(
            load_workspace(&path),
            Ok(StudioWorkspace { template_index: 2, ..Default::default() })
        );
    }
}
