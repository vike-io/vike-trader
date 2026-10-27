//! The best-effort disk half: load, save, append, the two bounds and the JSON helpers.

use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::learning::{Learning, LearningStore};
use super::trial::{Trial, TrialLedger};
use super::{MAX_CURVE_ROWS, MAX_LEARNING_TEXT, MAX_LEARNINGS, MAX_TRIALS};

/// Truncate to at most `max_bytes` BYTES at a char boundary — never splitting a UTF-8 sequence.
/// (A byte cap, not a char cap: the file size is what the bound exists to control.)
pub fn truncate_text(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Enforce both bounds on a ledger, in place: keep the newest [`MAX_TRIALS`] rows, and keep an
/// `oos_equity_curve` only on the newest [`MAX_CURVE_ROWS`] ACCEPTED rows (every other row's curve
/// is emptied — its scalars, including the load-bearing `sr_per_obs`, are untouched).
pub fn prune(ledger: &mut TrialLedger) {
    let n = ledger.trials.len();
    if n > MAX_TRIALS {
        ledger.trials.drain(..n - MAX_TRIALS);
    }
    let mut curves_kept = 0usize;
    for t in ledger.trials.iter_mut().rev() {
        if t.accepted && curves_kept < MAX_CURVE_ROWS {
            curves_kept += 1;
        } else {
            t.oos_equity_curve = Vec::new();
        }
    }
}

/// Read + parse the trial ledger at a CALLER-SUPPLIED path. Missing ⇒ `Default`; corrupt ⇒
/// `Default` plus one warning. Never an error — see the module doc.
pub fn load_trials(path: &Path) -> TrialLedger {
    load_json(path, "ai trial ledger")
}

/// Read + parse the learnings store at a CALLER-SUPPLIED path — the [`load_trials`] twin.
pub fn load_learnings(path: &Path) -> LearningStore {
    load_json(path, "ai learnings store")
}

/// Write the trial ledger to a CALLER-SUPPLIED path. A failure is one warning and nothing else.
pub fn save_trials(ledger: &TrialLedger, path: &Path) {
    save_json(ledger, path, "ai trial ledger");
}

/// Write the learnings store to a CALLER-SUPPLIED path — the [`save_trials`] twin.
pub fn save_learnings(store: &LearningStore, path: &Path) {
    save_json(store, path, "ai learnings store");
}

/// Load → append → [`prune`] → save, the one write path the authoring loop uses. Best-effort at
/// every step: a corrupt existing file is replaced by a fresh ledger holding just this trial
/// rather than aborting the append.
pub fn append_trial(path: &Path, trial: Trial) {
    let mut ledger = load_trials(path);
    ledger.trials.push(trial);
    prune(&mut ledger);
    save_trials(&ledger, path);
}

/// Load → truncate the text → append → cap → save. The [`append_trial`] twin for the
/// `record_learning` tool: `text` is capped at [`MAX_LEARNING_TEXT`] bytes and the file at
/// [`MAX_LEARNINGS`] rows (oldest dropped first).
pub fn append_learning(path: &Path, mut learning: Learning) {
    learning.text = truncate_text(&learning.text, MAX_LEARNING_TEXT);
    let mut store = load_learnings(path);
    store.learnings.push(learning);
    let n = store.learnings.len();
    if n > MAX_LEARNINGS {
        store.learnings.drain(..n - MAX_LEARNINGS);
    }
    save_learnings(&store, path);
}

fn load_json<T: DeserializeOwned + Default>(path: &Path, what: &str) -> T {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return T::default(); // absent (or unreadable) ⇒ "nothing recorded yet", the OFF state
    };
    match serde_json::from_str::<T>(&raw) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("{what} at {} is unparseable, starting empty: {e}", path.display());
            T::default()
        }
    }
}

fn save_json<T: Serialize>(value: &T, path: &Path, what: &str) {
    match serde_json::to_string_pretty(value) {
        Err(e) => tracing::warn!("{what} could not be serialized, not written: {e}"),
        Ok(json) => {
            if let Err(e) = std::fs::write(path, json) {
                tracing::warn!("{what} could not be written to {}: {e}", path.display());
            }
        }
    }
}
