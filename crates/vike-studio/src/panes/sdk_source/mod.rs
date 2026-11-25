//! **The SDK source view** (decision 0296, verdict 3: "Studio gets two read verbs that serve an SDK's
//! source"): the strategy SDK the open strategy builds against on the backtest server, read-only, as
//! NinjaTrader ships its indicators' source. Two surfaces, one state:
//!
//! - [`StudioSdkSourcePane::tree_ui`], the Strategy tool pane's "SDK source" section: which SDK the editor's
//!   strategy resolves to (its header's `[strategy] sdk` pin, else the newest usable SDK, and `sdk-dev`
//!   on a dev checkout: `vike_backtesthub_client::sdk_source::WireSdkListing::resolve`), any other SDK
//!   the server holds to pick instead, and the SDK's crates and files as a tree, a folder read when it
//!   is first opened (one SDK is about 1,300 files, too many for the tunnel at once);
//! - [`StudioSdkSourcePane::file_ui`], the central panel's SDK surface: one file in the code editor
//!   the Studio already has (`egui_code_editor`, Rust colouring for a `.rs` file), which cannot edit it.
//!
//! It reads over the wire only (`crate::backend::backtesthub::sdk_source`, Read dials): the desktop links no
//! server crate. The shell holds the receivers and hands each answer here (`StudioState::poll`).
//!
//! **No "go to definition".** A name in the strategy file would need an index of every item the SDK's
//! crates define (the module paths, `pub use`-free as the crates are), built on the server or by reading
//! all 1,300 files over the tunnel: nothing that exists answers it cheaply, so it is not here.

use std::collections::BTreeMap;

use egui_code_editor::{CodeEditor, ColorTheme, Syntax};
use vike_backtesthub_client::sdk_source::{
    SDK_CRATES_FOLDER, WireSdk, WireSdkEntry, WireSdkFile, WireSdkListing,
};
use vike_ui_theme::components::ThemeStatus;
use vike_ui_theme::metrics::space;
use vike_ui_theme::value::studio;

/// What the pane asks the shell for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StudioSdkSourceAction {
    /// List the server's SDKs again.
    Refresh,
    /// Show this listed SDK's tree instead.
    Show(String),
    /// Read this folder of the shown SDK: it is open and has not been read.
    ReadFolder(String),
    /// Read this file of the shown SDK into the code view.
    OpenFile(String),
}

/// The SDK source view's state: the server's SDKs, the one shown, its folders read so far and the file
/// in the code view.
#[derive(Debug, Default)]
pub(crate) struct StudioSdkSourcePane {
    /// The server's SDKs, or why they could not be listed; `None` until asked.
    listing: Option<Result<WireSdkListing, String>>,
    /// The SDK the tree shows, a listed name: the open strategy's until the user picks another.
    shown: Option<String>,
    /// The user picked [`Self::shown`] themselves, so a new listing keeps it rather than resolving again.
    picked: bool,
    /// Every folder of [`Self::shown`] read so far, by its path: its entries, or why it could not be read.
    folders: BTreeMap<String, Result<Vec<WireSdkEntry>, String>>,
    /// The file in the code view (its SDK and path inside), or why it could not be read.
    file: Option<Result<WireSdkFile, String>>,
}

/// The `[strategy] sdk` pin of a strategy file's text, through the one header parser
/// (`vike_strategy::file::StrategyFileHeader`); `None` when it pins none or its header does not parse
/// (a Rhai buffer, a file being written).
pub(crate) fn strategy_pin(source: &str) -> Option<u32> {
    vike_strategy::file::StrategyFileHeader::parse(source).ok()?.strategy.sdk
}

/// One line saying which SDK a strategy pinning `pin` builds against on the server `listing` describes.
pub(crate) fn resolution_line(listing: &WireSdkListing, pin: Option<u32>) -> String {
    match (listing.resolve(pin), pin) {
        (Some(sdk), _) if sdk.number.is_none() => format!(
            "The server is a dev checkout: every strategy file builds against its live tree, {}.",
            sdk.name
        ),
        (Some(sdk), Some(pin)) => format!("Your strategy pins SDK {pin}: {}.", sdk.name),
        (Some(sdk), None) => {
            format!(
                "Your strategy pins no SDK: it builds against the newest usable one, {}.",
                sdk.name
            )
        }
        (None, Some(pin)) => {
            format!(
                "Your strategy pins SDK {pin}, which this server does not hold: it cannot build."
            )
        }
        (None, None) => {
            "This server holds no usable SDK: no strategy file builds there.".to_string()
        }
    }
}

/// An SDK's facts in one line: its commit, interface version, and whether a file builds against it.
fn sdk_line(sdk: &WireSdk) -> String {
    let commit = if sdk.commit.is_empty() { "unknown commit" } else { sdk.commit.as_str() };
    let abi = sdk.abi_version.map_or_else(|| "?".to_string(), |v| v.to_string());
    let builds = if sdk.usable {
        "builds here".to_string()
    } else {
        format!("does not build here: {}", sdk.why_unusable)
    };
    format!("{} · {commit} · interface {abi} · {builds}", sdk.name)
}

/// A listed SDK as the toolbar's SDK pick shows it: its number, and when a file does not build against
/// it on the server, why. `sdk-dev` (a dev checkout, where every file builds against the live tree
/// whatever it asks) is said so; only a usable numbered SDK can be picked.
pub(crate) fn sdk_pick_label(sdk: &WireSdk) -> String {
    match (sdk.number, sdk.usable) {
        (None, _) => {
            format!("{}: a dev checkout builds every file against its live tree", sdk.name)
        }
        (Some(number), true) => format!("SDK {number}"),
        (Some(number), false) => format!("SDK {number}: does not build here: {}", sdk.why_unusable),
    }
}

/// The toolbar's SDK pick, closed: WHICH SDK a Run builds against and why, by its folder name
/// (`sdk-N`, `sdk-dev` on a dev checkout) once the server's list is in hand, else the name a pin names.
/// `pick` is the toolbar's pick (`None`: as the file says); `pin` the file's own pin while the editor
/// holds the file (`Some(None)`: it pins none), `None` when the file's header is not here to read.
pub(crate) fn sdk_pick_text(
    pick: Option<u32>,
    pin: Option<Option<u32>>,
    listing: Option<&WireSdkListing>,
) -> String {
    let named =
        |wanted: Option<u32>| listing.and_then(|l| l.resolve(wanted)).map(|s| s.name.clone());
    let by_number = |n: u32| named(Some(n)).unwrap_or_else(|| format!("sdk-{n}"));
    match (pick, pin) {
        (Some(n), _) => format!("SDK: {} (picked)", by_number(n)),
        (None, Some(Some(n))) => format!("SDK: {} (from the file)", by_number(n)),
        (None, Some(None)) => match named(None) {
            Some(name) => format!("SDK: {name} (the newest)"),
            None => "SDK: the newest on the server".to_string(),
        },
        (None, None) => "SDK: as the file says".to_string(),
    }
}

impl StudioSdkSourcePane {
    /// Whether the server's SDKs have been asked for (the shell lists them the first time the section is
    /// opened).
    pub(crate) fn listed(&self) -> bool {
        self.listing.is_some()
    }

    /// The server's SDKs, or why they could not be listed; `None` until asked. The toolbar's SDK pick
    /// offers the same list (`crate::studio::shell`'s `toolbar_sdk_pick`).
    pub(crate) fn listing(&self) -> Option<&Result<WireSdkListing, String>> {
        self.listing.as_ref()
    }

    /// Fold the server's SDK list in; the tree shows the SDK the strategy pinning `pin` resolves to,
    /// unless the user picked one the new list still holds.
    pub(crate) fn apply_listing(
        &mut self,
        answer: Result<WireSdkListing, String>,
        pin: Option<u32>,
    ) {
        let shown = self.shown.as_ref();
        self.picked = self.picked
            && answer.as_ref().is_ok_and(|l| l.sdks.iter().any(|s| Some(&s.name) == shown));
        self.listing = Some(answer);
        self.follow(pin);
    }

    /// Keep the tree on the SDK the strategy pinning `pin` resolves to, while the user has picked none:
    /// an edited pin moves it (the shell calls this every frame the section is drawn).
    pub(crate) fn follow(&mut self, pin: Option<u32>) {
        if self.picked {
            return;
        }
        let resolved = match &self.listing {
            Some(Ok(listed)) => listed.resolve(pin).map(|s| s.name.clone()),
            _ => None,
        };
        self.show(resolved, false);
    }

    /// Show `sdk`'s tree (`picked`: by the user's choice), forgetting the folders of another.
    pub(crate) fn show(&mut self, sdk: Option<String>, picked: bool) {
        if sdk != self.shown {
            self.folders.clear();
        }
        self.shown = sdk;
        self.picked = picked;
    }

    /// Fold one folder read in, by the path asked; an answer for another SDK than the one shown (the
    /// user switched meanwhile) is dropped.
    pub(crate) fn apply_folder(
        &mut self,
        sdk: &str,
        path: String,
        answer: Result<WireSdkFile, String>,
    ) {
        if self.shown.as_deref() == Some(sdk) {
            self.folders.insert(path, answer.map(|f| f.entries));
        }
    }

    /// Fold a file read in: it is what the code view shows.
    pub(crate) fn apply_file(&mut self, answer: Result<WireSdkFile, String>) {
        self.file = Some(answer);
    }

    /// The SDK the tree shows.
    pub(crate) fn shown(&self) -> Option<&str> {
        self.shown.as_deref()
    }

    /// The Strategy pane's "SDK source" section body: the resolution, the SDK picker and the tree. `pin`
    /// is the editor's strategy pin, `listing` that a list is on its way, `reading` that a folder is.
    pub(crate) fn tree_ui(
        &self,
        ui: &mut egui::Ui,
        pin: Option<u32>,
        listing: bool,
        reading: bool,
    ) -> Option<StudioSdkSourceAction> {
        let mut action = None;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("The SDK your strategy builds against").weak());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button("Refresh")
                    .on_hover_text("List the backtest server's strategy SDKs again")
                    .clicked()
                {
                    action = Some(StudioSdkSourceAction::Refresh);
                }
                if listing {
                    ui.spinner();
                }
            });
        });
        let listed = match &self.listing {
            None => {
                ui.label(egui::RichText::new("Listing the server's SDKs…").weak());
                return action;
            }
            Some(Err(why)) => {
                ui.add(
                    egui::Label::new(egui::RichText::new(why).color(ThemeStatus::Error.color()))
                        .wrap(),
                );
                return action;
            }
            Some(Ok(listed)) => listed,
        };
        ui.add(egui::Label::new(resolution_line(listed, pin)).wrap());
        let mut picked = None;
        if !listed.sdks.is_empty() {
            let current = self.shown.clone().unwrap_or_default();
            egui::ComboBox::from_id_salt("studio-sdk-picker")
                .selected_text(current.as_str())
                .show_ui(ui, |ui| {
                    for sdk in &listed.sdks {
                        let label = if sdk.usable {
                            sdk.name.clone()
                        } else {
                            format!("{} (does not build)", sdk.name)
                        };
                        if ui.selectable_label(sdk.name == current, label).clicked() {
                            picked = Some(sdk.name.clone());
                        }
                    }
                })
                .response
                .on_hover_text("Read another SDK of the server");
        }
        if let Some(sdk) = listed.sdks.iter().find(|s| Some(&s.name) == self.shown.as_ref()) {
            let words = egui::RichText::new(sdk_line(sdk)).weak();
            ui.add(egui::Label::new(words).wrap());
        }
        if let Some(name) = picked {
            return Some(StudioSdkSourceAction::Show(name));
        }
        if self.shown.is_some() {
            ui.add_space(space::XS);
            let open = self.open_file_path();
            self.folder_ui(ui, SDK_CRATES_FOLDER, open.as_deref(), reading, &mut action);
        }
        action
    }

    /// The path of the file in the code view, when it is of the SDK shown.
    fn open_file_path(&self) -> Option<String> {
        match &self.file {
            Some(Ok(f)) if Some(&f.sdk) == self.shown.as_ref() => Some(f.path.clone()),
            _ => None,
        }
    }

    /// One folder of the tree: its entries, a sub-folder a collapsible row read when first opened, a
    /// file a row that opens it in the code view. An unread folder asks to be read (one at a time:
    /// none while `reading`).
    fn folder_ui(
        &self,
        ui: &mut egui::Ui,
        path: &str,
        open: Option<&str>,
        reading: bool,
        action: &mut Option<StudioSdkSourceAction>,
    ) {
        match self.folders.get(path) {
            None => {
                if !reading && action.is_none() {
                    *action = Some(StudioSdkSourceAction::ReadFolder(path.to_string()));
                }
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(egui::RichText::new("reading…").weak());
                });
            }
            Some(Err(why)) => {
                ui.add(
                    egui::Label::new(egui::RichText::new(why).color(ThemeStatus::Error.color()))
                        .wrap(),
                );
            }
            Some(Ok(entries)) => {
                for entry in entries {
                    let child = format!("{path}/{}", entry.name);
                    if entry.folder {
                        egui::CollapsingHeader::new(entry.name.as_str())
                            .id_salt(("studio-sdk-folder", self.shown.as_deref(), child.as_str()))
                            .show(ui, |ui| self.folder_ui(ui, &child, open, reading, action));
                    } else if ui
                        .selectable_label(open == Some(child.as_str()), entry.name.as_str())
                        .on_hover_text(child.as_str())
                        .clicked()
                    {
                        *action = Some(StudioSdkSourceAction::OpenFile(child));
                    }
                }
            }
        }
    }

    /// The central panel's SDK surface: the file asked for, read-only, or why it could not be read.
    /// `reading` says a file is on its way. Returns `true` when "Back to results" was clicked.
    pub(crate) fn file_ui(&self, ui: &mut egui::Ui, reading: bool) -> bool {
        let mut back = false;
        ui.horizontal(|ui| {
            let title = match &self.file {
                Some(Ok(f)) => format!("SDK source · {} · {}", f.sdk, f.path),
                _ => "SDK source".to_string(),
            };
            ui.label(egui::RichText::new(title).strong());
            ui.label(egui::RichText::new("read-only").weak());
            if reading {
                ui.spinner();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                back = ui.button("Back to results").clicked();
            });
        });
        ui.add_space(space::XS);
        match &self.file {
            Some(Ok(file)) => {
                let syntax =
                    if file.path.ends_with(".rs") { Syntax::rust() } else { Syntax::simple("#") };
                // `&str` is a TextBuffer egui will not edit: the code view selects and copies, never
                // changes the SDK's text.
                let mut text: &str = file.text.as_str();
                CodeEditor::default()
                    .id_source("vike-studio-sdk-file")
                    .with_rows(crate::panes::editor::rows_for_height(
                        ui.available_height(),
                        studio::EDITOR_FONT_SIZE,
                    ))
                    .with_fontsize(studio::EDITOR_FONT_SIZE)
                    .with_theme(&ColorTheme::GRUVBOX.into())
                    .with_numlines(true)
                    .show(ui, &mut text, &syntax);
            }
            Some(Err(why)) => {
                ui.add(
                    egui::Label::new(egui::RichText::new(why).color(ThemeStatus::Error.color()))
                        .wrap(),
                );
            }
            None => {
                ui.label(
                    egui::RichText::new("Pick a file in the Strategy tool's SDK source tree.")
                        .weak(),
                );
            }
        }
        back
    }
}

#[cfg(test)]
mod tests;
