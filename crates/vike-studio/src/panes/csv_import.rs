//! The Data pane's IMPORT CSV form (decision 0272): a user's file from this PC into a NEW `user-*`
//! series of the datahub's store, on the observe key. Pick the file (type its path, or drop it on the
//! pane), say its series and how it is written, PREVIEW how the datahub reads its first lines, map its
//! columns from the header that preview returned, and IMPORT it.
//!
//! The form computes nothing of the file: the datahub decodes and stores, and every line shown here is
//! its answer (decision 0078 as amended 2026-11-10). The form only builds the request, holding it to
//! the wire's own validator for the MESSAGE before anything is sent. `StudioState` runs the upload
//! (`crate::backend::csv_upload`) and hands the form its progress and its answer.

use std::path::PathBuf;

use vike_data::csv_bars::CsvBarsFormatWords;
use vike_datahub_client::archive::{ImportTarget, validate_import_target};
use vike_datahub_client::upload::CsvUploadDone;
use vike_model::time::{epoch_ms_to_utc_date, epoch_ms_to_utc_timestamp};
use vike_ui_theme::components::Status;
use vike_ui_theme::components::button::ActionButton;
use vike_ui_theme::components::input::{self, Field};
use vike_ui_theme::metrics::space;

/// The field delimiters the decoder reads, as its own words.
const DELIMITERS: [&str; 3] = [",", ";", "tab"];
/// The decimal marks.
const DECIMALS: [&str; 2] = [".", ","];
/// The thousands separators: none, or one of the decoder's words.
const THOUSANDS: [&str; 4] = ["none", "space", ".", ","];
/// The six fields a column mapping names, in the decoder's order.
const FIELDS: [&str; 6] = ["time", "open", "high", "low", "close", "volume"];
/// A column pick that names no column: the field is not in the file (`open`/`high`/`low` all three,
/// or `volume`).
const NO_COLUMN: &str = "—";

/// What the form asks the shell to send, the frame Preview or Import is pressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsvImportAsk {
    pub path: PathBuf,
    pub target: ImportTarget,
    /// The head of the file as a dry run (Preview); `false` sends the whole file and stores it.
    pub preview: bool,
}

/// The form's state. Its fields are the user's words, kept as typed.
#[derive(Debug, Clone, PartialEq)]
pub struct CsvImportForm {
    /// Whether the form is shown under the pane's header.
    pub open: bool,
    pub path: String,
    pub label: String,
    /// Index into [`Self::venues`]: 0 is `user`, data that is no venue's.
    pub venue_index: usize,
    pub symbol: String,
    pub interval: String,
    pub delimiter_index: usize,
    pub decimal_index: usize,
    pub thousands_index: usize,
    pub time: String,
    pub tz: String,
    /// Name the columns from the file's header rather than by the decoder's usual names.
    pub map_columns: bool,
    /// Per field of [`FIELDS`], an index into [`NO_COLUMN`] followed by the header's cells.
    pub picks: [usize; 6],
    /// The file's header cells, as the datahub's last answer split them.
    pub header: Vec<String>,
    ask: Option<CsvImportAsk>,
    /// `Some(preview)` while an upload runs.
    running: Option<bool>,
    progress: Option<(u64, u64)>,
    /// The last answer, and whether it was a preview's.
    answer: Option<(bool, Result<CsvUploadDone, String>)>,
    /// The request's own refusal, said before anything is sent.
    local: Option<String>,
}

impl Default for CsvImportForm {
    fn default() -> Self {
        Self {
            open: false,
            path: String::new(),
            label: String::new(),
            venue_index: 0,
            symbol: String::new(),
            interval: "1d".to_string(),
            delimiter_index: 0,
            decimal_index: 0,
            thousands_index: 0,
            time: String::new(),
            tz: String::new(),
            map_columns: false,
            picks: [0; 6],
            header: Vec::new(),
            ask: None,
            running: None,
            progress: None,
            answer: None,
            local: None,
        }
    }
}

impl CsvImportForm {
    /// The venues a series may be stored under: `user` first, then the roster.
    pub fn venues() -> Vec<&'static str> {
        std::iter::once(vike_data::store::store_kind::USER_VENUE)
            .chain(vike_model::VENUES.iter().copied())
            .collect()
    }

    /// The column mapping in the decoder's words, or `None`: found by the header's usual names.
    pub fn columns(&self) -> Option<String> {
        if !self.map_columns {
            return None;
        }
        let pairs: Vec<String> = FIELDS
            .iter()
            .zip(self.picks)
            .filter_map(|(field, pick)| {
                let name = self.header.get(pick.checked_sub(1)?)?;
                Some(format!("{field}={name}"))
            })
            .collect();
        Some(pairs.join(","))
    }

    /// The request's series and form, held to the wire's own validator.
    pub fn target(&self) -> Result<ImportTarget, String> {
        let word = |s: &str| (!s.trim().is_empty()).then(|| s.trim().to_string());
        let target = ImportTarget {
            venue: Self::venues()[self.venue_index.min(Self::venues().len() - 1)].to_string(),
            symbol: self.symbol.trim().to_string(),
            interval: self.interval.trim().to_string(),
            label: self.label.trim().to_string(),
            file_format: CsvBarsFormatWords {
                delimiter: (self.delimiter_index > 0)
                    .then(|| DELIMITERS[self.delimiter_index].into()),
                decimal: (self.decimal_index > 0).then(|| DECIMALS[self.decimal_index].into()),
                thousands: (self.thousands_index > 0)
                    .then(|| THOUSANDS[self.thousands_index].into()),
                time: word(&self.time),
                tz: word(&self.tz),
            },
            columns: self.columns(),
        };
        validate_import_target(&target)?;
        Ok(target)
    }

    /// The request the form holds once Preview or Import was pressed; taken once.
    pub fn take_ask(&mut self) -> Option<CsvImportAsk> {
        self.ask.take()
    }

    /// An upload of the form's file started.
    pub fn started(&mut self, preview: bool) {
        self.running = Some(preview);
        self.progress = None;
    }

    /// The datahub acknowledged `sent` of the `total` bytes being sent.
    pub fn progress(&mut self, sent: u64, total: u64) {
        self.progress = Some((sent, total));
    }

    /// The upload ended: its answer. A header it carries becomes the column choices.
    pub fn finish(&mut self, preview: bool, answer: Result<CsvUploadDone, String>) {
        self.running = None;
        self.progress = None;
        if let Ok(done) = &answer
            && !done.header.is_empty()
            && done.header != self.header
        {
            self.header = done.header.clone();
            self.picks = [0; 6];
        }
        self.answer = Some((preview, answer));
    }

    /// Whether an upload is running.
    pub fn running(&self) -> bool {
        self.running.is_some()
    }

    /// Draw the toggle and, open, the form. `datahub` is the address an upload goes to, `None` when
    /// the Studio reads no datahub.
    pub fn ui(&mut self, ui: &mut egui::Ui, datahub: Option<&str>) {
        let dropped = ui.ctx().input(|i| i.raw.dropped_files.first().map(|f| f.path().to_owned()));
        if let Some(path) = dropped {
            self.path = path.display().to_string();
            self.open = true;
        }
        if ui
            .add(ActionButton::secondary("Import CSV…").selected(self.open))
            .on_hover_text("Send a CSV file from this PC into a new lane of the datahub's store.")
            .clicked()
        {
            self.open = !self.open;
        }
        if !self.open {
            return;
        }
        ui.add_space(space::XS);
        // The form scrolls within the pane's height, never past its foot: the Data pane is not inside
        // the tools panel's shared scroll. The stored catalog below it keeps whatever height is left.
        egui::ScrollArea::vertical()
            .id_salt("studio-csv-import-scroll")
            .max_height(ui.available_height())
            .auto_shrink([false, true])
            .show(ui, |ui| self.form_ui(ui, datahub));
    }

    /// The form's rows, its buttons and the datahub's last answer. Every row is a narrow label column
    /// and a field filling the rest of the pane's width, so nothing lays out wider than the pane: a
    /// wider child once cost the tools panel its width, and the central panel painted over its left
    /// edge.
    fn form_ui(&mut self, ui: &mut egui::Ui, datahub: Option<&str>) {
        let label_w = label_column(ui);
        let venues = Self::venues();
        text_row(ui, label_w, "File", &mut self.path, "a path, or drop the file here");
        text_row(ui, label_w, "Label", &mut self.label, "lane user-<label>: a-z 0-9 - _");
        combo_row(ui, label_w, "Venue", "csv-venue", &mut self.venue_index, &venues);
        text_row(ui, label_w, "Symbol", &mut self.symbol, "USDT-USD");
        text_row(ui, label_w, "Interval", &mut self.interval, "1d, 1h, 1m …");
        combo_row(ui, label_w, "Delimiter", "csv-delim", &mut self.delimiter_index, &DELIMITERS);
        combo_row(ui, label_w, "Decimal", "csv-decimal", &mut self.decimal_index, &DECIMALS);
        combo_row(ui, label_w, "Thousands", "csv-thousands", &mut self.thousands_index, &THOUSANDS);
        text_row(ui, label_w, "Time", &mut self.time, "epoch, or DD.MM.YYYY HH:mm");
        text_row(ui, label_w, "Zone", &mut self.tz, "UTC, +03:00, Europe/Berlin");
        self.columns_ui(ui, label_w);
        ui.add_space(space::SM);
        self.buttons_ui(ui, datahub);
        self.answer_ui(ui);
    }

    /// The column mapping, offered once a preview returned the file's header.
    fn columns_ui(&mut self, ui: &mut egui::Ui, label_w: f32) {
        if self.header.is_empty() {
            wrapped(ui, egui::RichText::new("Preview to read the file's columns.").weak());
            return;
        }
        ui.checkbox(&mut self.map_columns, "Name the columns");
        if !self.map_columns {
            return;
        }
        let choices: Vec<&str> =
            std::iter::once(NO_COLUMN).chain(self.header.iter().map(String::as_str)).collect();
        for (field, pick) in FIELDS.iter().zip(self.picks.iter_mut()) {
            combo_row(ui, label_w, field, ("csv-col", *field), pick, &choices);
        }
        wrapped(
            ui,
            egui::RichText::new(
                "From the file's header: time and close are needed; open, high and low all three \
                 or none (one value per row); no volume is 0.",
            )
            .weak(),
        );
    }

    fn buttons_ui(&mut self, ui: &mut egui::Ui, datahub: Option<&str>) {
        let why = if datahub.is_none() {
            Some("Studio reads no datahub, so there is nowhere to send the file.")
        } else if self.running.is_some() {
            Some("An upload is running.")
        } else if self.path.trim().is_empty() {
            Some("Pick the file first: type its path, or drop it on this pane.")
        } else {
            None
        };
        let mut pressed = None;
        ui.horizontal(|ui| {
            let mut preview = ActionButton::secondary("Preview");
            let mut import = ActionButton::primary("Import");
            if let Some(why) = why {
                preview = preview.disabled_because(why);
                import = import.disabled_because(why);
            }
            if ui
                .add(preview)
                .on_hover_text("Read the file's first lines; store nothing.")
                .clicked()
            {
                pressed = Some(true);
            }
            if ui.add(import).on_hover_text("Send the whole file and store it.").clicked() {
                pressed = Some(false);
            }
            if self.running.is_some() {
                ui.spinner();
                if let Some((sent, total)) = self.progress {
                    ui.label(format!("{} of {}", mib(sent), mib(total)));
                }
            }
        });
        if let Some(preview) = pressed {
            match self.target() {
                Ok(target) => {
                    self.local = None;
                    self.ask = Some(CsvImportAsk {
                        path: PathBuf::from(self.path.trim()),
                        target,
                        preview,
                    });
                }
                Err(why) => self.local = Some(why),
            }
        }
        if let Some(why) = &self.local {
            wrapped(ui, egui::RichText::new(why).color(Status::Error.color()));
        }
    }

    /// The datahub's last answer: the rows as it read them, what it stored, or why it refused.
    fn answer_ui(&self, ui: &mut egui::Ui) {
        let Some((preview, answer)) = &self.answer else { return };
        ui.add_space(space::SM);
        let error = |ui: &mut egui::Ui, text: String| {
            wrapped(ui, egui::RichText::new(text).color(Status::Error.color()));
        };
        let done = match answer {
            Ok(done) => done,
            Err(why) => return error(ui, why.clone()),
        };
        if let Some(why) = &done.refused {
            return error(ui, format!("The datahub refused the file: {why}"));
        }
        let span = match (done.first_day, done.last_day) {
            (Some(a), Some(b)) => {
                format!(", {} .. {}", epoch_ms_to_utc_date(a), epoch_ms_to_utc_date(b))
            }
            _ => String::new(),
        };
        let lane = format!("user-{}", self.label.trim());
        let line = if *preview {
            format!("Read {} bars of the file's first lines{span}. Nothing was stored.", done.bars)
        } else {
            format!(
                "Stored {} days ({} bars) into {lane}{span}.",
                done.stored_days, done.stored_bars
            )
        };
        let tone = if *preview { Status::Info.color() } else { Status::Ok.color() };
        wrapped(ui, egui::RichText::new(line).color(tone));
        for d in &done.not_stored {
            if let vike_datahub_client::archive::ArchiveDayResultWire::Refused(r) = &d.result {
                let day = epoch_ms_to_utc_date(d.day);
                wrapped(ui, egui::RichText::new(format!("{day} not stored: {}", r.detail)).weak());
            }
        }
        if done.preview.is_empty() {
            return;
        }
        // Six columns are wider than the pane: they scroll sideways inside it, never past its edge.
        egui::ScrollArea::horizontal()
            .id_salt("studio-csv-preview-scroll")
            .max_width(ui.available_width())
            .auto_shrink([false, true])
            .show(ui, |ui| {
                egui::Grid::new("studio-csv-preview").num_columns(6).striped(true).show(ui, |ui| {
                    for head in ["time (UTC)", "open", "high", "low", "close", "volume"] {
                        ui.label(egui::RichText::new(head).weak());
                    }
                    ui.end_row();
                    for r in &done.preview {
                        ui.label(epoch_ms_to_utc_timestamp(r.timestamp));
                        for v in [&r.open, &r.high, &r.low, &r.close, &r.volume] {
                            ui.label(v.as_str());
                        }
                        ui.end_row();
                    }
                });
            });
    }
}

/// Every row label the form draws: the label column is as wide as the widest of them.
const ROW_LABELS: [&str; 16] = [
    "File",
    "Label",
    "Venue",
    "Symbol",
    "Interval",
    "Delimiter",
    "Decimal",
    "Thousands",
    "Time",
    "Zone",
    "time",
    "open",
    "high",
    "low",
    "close",
    "volume",
];

/// The label column's width: the widest row label in the body font, so every field starts at one x.
fn label_column(ui: &egui::Ui) -> f32 {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let color = ui.visuals().text_color();
    ROW_LABELS
        .iter()
        .map(|l| ui.painter().layout_no_wrap((*l).to_string(), font.clone(), color).size().x)
        .fold(0.0, f32::max)
}

/// One row: `name` in the label column, and `field` given the rest of the row's width.
fn row(
    ui: &mut egui::Ui,
    label_w: f32,
    name: &str,
    field: impl FnOnce(&mut egui::Ui, f32) -> egui::Response,
) {
    ui.horizontal(|ui| {
        let size = egui::vec2(label_w, ui.spacing().interact_size.y);
        let label = ui
            .allocate_ui_with_layout(size, egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.set_min_width(label_w);
                ui.add(egui::Label::new(egui::RichText::new(name).weak()).truncate())
            })
            .inner
            .id;
        let width = ui.available_width();
        field(ui, width).labelled_by(label);
    });
}

/// A row whose field is a text input as wide as the row leaves.
fn text_row(ui: &mut egui::Ui, label_w: f32, name: &str, value: &mut String, hint: &str) {
    row(ui, label_w, name, |ui, width| {
        ui.scope(|ui| {
            ui.spacing_mut().text_edit_width = width;
            input::text(ui, value, Field { hint, ..Field::default() })
        })
        .inner
    });
}

/// A row whose field is a combo box over `choices`, its value an index, as wide as the row leaves; a
/// long choice is cut rather than widening the pane.
fn combo_row(
    ui: &mut egui::Ui,
    label_w: f32,
    name: &str,
    id: impl std::hash::Hash + std::fmt::Debug,
    index: &mut usize,
    choices: &[&str],
) {
    row(ui, label_w, name, |ui, width| {
        egui::ComboBox::from_id_salt(id).width(width).truncate().show_index(
            ui,
            index,
            choices.len(),
            |i| choices.get(i).copied().unwrap_or_default().to_string(),
        )
    });
}

/// A sentence wrapped to the pane's width.
fn wrapped(ui: &mut egui::Ui, text: egui::RichText) {
    ui.add(egui::Label::new(text).wrap());
}

/// `bytes` in MiB, one decimal: the progress label.
fn mib(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled() -> CsvImportForm {
        CsvImportForm {
            path: "C:/data/rates.csv".into(),
            label: "coinbase".into(),
            symbol: "USDT-USD".into(),
            ..CsvImportForm::default()
        }
    }

    #[test]
    fn the_form_builds_a_user_venue_target_held_to_the_wires_rules() {
        let t = filled().target().unwrap();
        assert_eq!(
            (t.venue.as_str(), t.interval.as_str(), t.label.as_str()),
            ("user", "1d", "coinbase")
        );
        assert!(t.file_format.is_plain() && t.columns.is_none());
        let regional =
            CsvImportForm { delimiter_index: 1, decimal_index: 1, thousands_index: 1, ..filled() };
        let words = regional.target().unwrap().file_format;
        assert_eq!(
            (words.delimiter.as_deref(), words.decimal.as_deref(), words.thousands.as_deref()),
            (Some(";"), Some(","), Some("space"))
        );
        let bad = CsvImportForm { label: "My File".into(), ..filled() };
        assert!(bad.target().unwrap_err().contains("label"));
        let binance = CsvImportForm {
            venue_index: 1 + vike_model::VENUES.iter().position(|v| *v == "binance").unwrap(),
            ..filled()
        };
        assert_eq!(binance.target().unwrap().venue, "binance");
    }

    #[test]
    fn the_columns_are_named_from_the_header_the_datahub_returned() {
        let mut form = filled();
        let read = CsvUploadDone {
            header: vec!["timestamp_ms".into(), "rate".into()],
            refused: Some("line 1: header: no time column".into()),
            ..CsvUploadDone::default()
        };
        form.finish(true, Ok(read));
        assert_eq!(form.header, ["timestamp_ms", "rate"]);
        assert_eq!(form.columns(), None, "not mapped until the user says so");
        form.map_columns = true;
        form.picks = [1, 0, 0, 0, 2, 0];
        assert_eq!(form.columns().as_deref(), Some("time=timestamp_ms,close=rate"));
        assert_eq!(form.target().unwrap().columns.as_deref(), Some("time=timestamp_ms,close=rate"));
        // open without high and low is the decoder's refusal, said before anything is sent.
        form.picks = [1, 2, 0, 0, 2, 0];
        assert!(form.target().unwrap_err().contains("open"));
    }

    #[test]
    fn an_upload_runs_reports_progress_and_ends_with_its_answer() {
        let mut form = filled();
        form.started(false);
        assert!(form.running());
        form.progress(4, 8);
        assert_eq!(form.progress, Some((4, 8)));
        form.finish(false, Err("this datahub takes no CSV upload".into()));
        assert!(!form.running() && form.progress.is_none());
        assert!(matches!(&form.answer, Some((false, Err(_)))));
        assert_eq!(form.take_ask(), None);
    }
}
