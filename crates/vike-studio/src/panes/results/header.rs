//! The lines above the results' tabs: the RUN LINE (what is on its way and for how long, with Cancel; else how
//! long each kind's last answer took), the CODE LINE (which code produced the results on screen, and whether
//! the editor still shows it) and the SUMMARY STRIP (the run's key numbers, as its summary carries them).
//!
//! # Why the code line exists (the owner's desktop test of 2026-11-20)
//!
//! A run executed the server's file `probe/qa_studio_probe_20261120.rs`, a stop-and-reverse, while the editor
//! showed an unsaved long-only Rhai template: the Trades tab's shorts read as an engine bug. What a Run sends
//! depends on the strategy source (`crate::studio::StudioState::current_entry`): the editor's Rhai text, a file
//! of the server's folder, or a Rust file the server built. So the shell records the code at the click
//! ([`StudioResultsOrigin::send`]) and the pane names it beside the results, with a notice when the editor no
//! longer shows it ([`StudioResultsCode::stale`]).
//!
//! # The run line's times are the CLIENT's
//!
//! From the click to the answer in hand, the network included: the summary carries no compute time. Cancel is
//! `crate::studio::StudioState::cancel`, which ABANDONS the answer (the server finishes the work); no request
//! reports progress, so the line shows a spinner and the seconds waited, not a share done.

use std::time::{Duration, Instant};

use vike_analytics::INVALID_LABEL;
use vike_backtesthub_client::summary::RunSummaryWire;
use vike_model::params::ParamValue;
use vike_ui_theme::components::{ThemeStatus, section};
use vike_ui_theme::icons;
use vike_ui_theme::metrics::space;

use super::{duration_words, metric_words, row_cell};

/// Which code a Run, a param scan or a walk-forward was sent with, as the shell read it at the click.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum StudioResultsCode {
    /// The editor's Rhai text as it was sent; `file` is the server file it was opened from or saved as (empty
    /// for none), and `saved` whether the text was that file's when it was sent.
    Buffer { text: String, file: String, saved: bool },
    /// A strategy file of the server's folder, run as the server holds it: the editor's text was not sent.
    ServerFile(String),
    /// A Rust strategy file of the server's folder, built from this editor text.
    RustFile { file: String, text: String },
}

impl StudioResultsCode {
    /// The words the code line names it with.
    pub(crate) fn words(&self) -> String {
        match self {
            Self::Buffer { file, .. } if file.is_empty() => {
                "the editor's Rhai text, saved to no file".to_string()
            }
            Self::Buffer { file, saved: true, .. } => {
                format!("the editor's Rhai text, as saved in {file}")
            }
            Self::Buffer { file, .. } => {
                format!("the editor's Rhai text, with edits not saved to {file}")
            }
            Self::ServerFile(file) => format!("the server's file {file}, as saved there"),
            Self::RustFile { file, .. } => {
                format!("the Rust file {file}, built from the editor's text")
            }
        }
    }

    /// The server file the code is, or the editor text was opened from or saved as; empty for a text
    /// saved to none.
    pub(crate) fn file(&self) -> &str {
        match self {
            Self::Buffer { file, .. } | Self::RustFile { file, .. } => file,
            Self::ServerFile(file) => file,
        }
    }

    /// Why the editor does not show the code these results came from, or `None` when it does; `editor` is the
    /// editor's text now. A server file's run never sent the editor's text, so it always says so.
    pub(crate) fn stale(&self, editor: &str) -> Option<String> {
        match self {
            Self::Buffer { text, .. } | Self::RustFile { text, .. } if text != editor => Some(
                "The editor has changed since this run: these results are from an older version of \
                 the code. Run again to see the current one."
                    .to_string(),
            ),
            Self::ServerFile(file) => Some(format!(
                "The editor's text did not run: these results are the server's file {file}."
            )),
            _ => None,
        }
    }
}

/// One kind of request's origin (a Run's, a param scan's, a walk-forward's): the code and the moment the one on
/// its way was sent, and once it answered, how long it took and what code it was. One per kind, because the
/// pane can show a run's results while a scan is on its way.
#[derive(Debug, Default)]
pub(crate) struct StudioResultsOrigin {
    sent: Option<(Instant, StudioResultsCode)>,
    answered: Option<(Duration, StudioResultsCode)>,
    /// The parameter values the request on its way, then the one answered, carried ([`Self::send_with_values`]).
    sent_values: Vec<(String, ParamValue)>,
    values: Vec<(String, ParamValue)>,
}

impl StudioResultsOrigin {
    /// A request of this kind was sent with `code`, now.
    pub(crate) fn send(&mut self, code: StudioResultsCode) {
        self.send_with_values(code, Vec::new());
    }

    /// [`Self::send`], the request carrying the parameter values `values`: what a fixed walk-forward's
    /// windows ran with, which its answer does not name.
    pub(crate) fn send_with_values(
        &mut self,
        code: StudioResultsCode,
        values: Vec<(String, ParamValue)>,
    ) {
        self.sent = Some((Instant::now(), code));
        self.sent_values = values;
    }

    /// The request on its way answered (a result or a failure): it took from its send to now.
    pub(crate) fn answer(&mut self) {
        if let Some((at, code)) = self.sent.take() {
            self.answered = Some((at.elapsed(), code));
            self.values = std::mem::take(&mut self.sent_values);
        }
    }

    /// The parameter values the last answered request carried; empty when it named none.
    pub(crate) fn values(&self) -> &[(String, ParamValue)] {
        &self.values
    }

    /// How long the request sent last has waited; read only while one is on its way.
    pub(crate) fn waited(&self) -> Option<Duration> {
        self.sent.as_ref().map(|(at, _)| at.elapsed())
    }

    /// How long the last answer took, and the code it was sent with.
    pub(crate) fn answered(&self) -> Option<&(Duration, StudioResultsCode)> {
        self.answered.as_ref()
    }
}

/// A duration as the run line says it, `"1.4 seconds"`.
fn seconds(d: Duration) -> String {
    format!("{:.1} seconds", d.as_secs_f64())
}

/// The run line's words over `kinds`, each `(name, on its way, origin)`: "<name> running for 3.0 seconds" for
/// one on its way, else "<name> took 1.4 seconds" for one that answered; `None` when no kind has either.
pub(crate) fn run_line_words(kinds: &[(&str, bool, &StudioResultsOrigin)]) -> Option<String> {
    let parts: Vec<String> = kinds
        .iter()
        .filter_map(|&(name, busy, origin)| {
            if busy {
                origin.waited().map(|d| format!("{name} running for {}", seconds(d)))
            } else {
                origin.answered().map(|(d, _)| format!("{name} took {}", seconds(*d)))
            }
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

/// The run line: a spinner and Cancel while any kind is on its way, then [`run_line_words`], then each kind of
/// `saved` (its name, and the saved run its last answer was read back from, empty for an answer just computed)
/// as "answered from saved run <id>" (decision 0078's amendment of 2026-11-20). Answers whether Cancel was clicked.
pub(crate) fn run_line_ui(
    ui: &mut egui::Ui,
    kinds: &[(&str, bool, &StudioResultsOrigin)],
    saved: &[(&str, &str)],
) -> bool {
    let Some(words) = run_line_words(kinds) else { return false };
    let busy = kinds.iter().any(|&(_, busy, _)| busy);
    let mut cancel = false;
    // Wrapped: in a narrow pane the line ran past its edge (and widened the pane's Ui under it).
    ui.horizontal_wrapped(|ui| {
        if busy {
            ui.spinner();
        }
        ui.label(egui::RichText::new(words).weak());
        if busy {
            cancel = ui
                .button((icons::CANCEL, "Cancel"))
                .on_hover_text(
                    "Abandon what is running: the server finishes it, the answer is dropped",
                )
                .clicked();
        }
        for &(name, id) in saved.iter().filter(|(_, id)| !id.is_empty()) {
            ui.label(egui::RichText::new(format!("{name} answered from saved run {id}")).weak())
                .on_hover_text("An identical run; tick Recompute to run it again");
        }
    });
    cancel
}

/// The code line: which code produced the results on screen ([`StudioResultsCode::words`]), and the notice when
/// the editor (`editor`, its text now) does not show it.
pub(crate) fn code_line_ui(ui: &mut egui::Ui, code: &StudioResultsCode, editor: &str) {
    ui.label(egui::RichText::new(format!("Code: {}", code.words())).weak());
    if let Some(stale) = code.stale(editor) {
        let words = egui::RichText::new(stale).color(ThemeStatus::Warning.color());
        ui.label(icons::WARNING.before(ui.style(), words));
    }
}

/// The summary strip's catalog rows, in strip order.
const STRIP_IDS: [&str; 6] =
    ["total_return", "sharpe", "max_drawdown", "n_trades", "total_fees", "final_equity"];

/// The summary strip's cells, `(words, value)`: each of [`STRIP_IDS`] the summary carries, as the Performance tab
/// shows it, then a tick run's curve step (`TradingResults::returns_interval_ms`). A run whose equity reached
/// zero keeps its counts and money (its summary keeps only those rows) and reads INVALID for each other one.
pub(crate) fn strip_cells(summary: &RunSummaryWire) -> Vec<(String, String)> {
    let invalid = summary.invalid.is_some();
    let mut cells: Vec<(String, String)> = STRIP_IDS
        .iter()
        .filter_map(|id| {
            row_cell(summary, id)
                .or_else(|| invalid.then(|| (metric_words(id), INVALID_LABEL.to_string())))
        })
        .collect();
    if let Some(step) = summary.results.as_ref().and_then(|r| r.returns_interval_ms) {
        cells.push(("Curve step".to_string(), duration_words(step)));
    }
    cells
}

/// The summary strip, always above the tabs ([`strip_cells`]): a run whose equity reached zero says so above
/// it, in its INVALID words. The cells wrap in a narrow pane rather than widen it.
pub(super) fn strip_ui(ui: &mut egui::Ui, summary: &RunSummaryWire) {
    if let Some(why) = &summary.invalid {
        let words = format!("{INVALID_LABEL}: {why}");
        ui.label(egui::RichText::new(words).strong().color(ThemeStatus::Warning.color()));
    }
    let cells = strip_cells(summary);
    if cells.is_empty() {
        return;
    }
    section::context_bar(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            for (words, value) in &cells {
                section::meta_cell(ui, words, value, true);
                ui.add_space(space::LG);
            }
        })
    });
}
