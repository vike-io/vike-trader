//! **A settings FILE on an ADOPTED box is a stale draft** — how that is detected, and why it is a
//! warning rather than a refusal.
//!
//! Once `vike-cli config adopt` has sealed a store, [`crate::load_with_source`] does not open the
//! four settings files for resolution at all. A file that is still on disk therefore cannot arm
//! anything, cannot widen anything and cannot change one value — it can only mislead a human who
//! edits it and believes something happened.
//!
//! # ⚠ Why this is a WARNING and not a boot refusal
//!
//! A refusal was available and is refused, on the remedy it teaches. The obvious reaction to
//! *"`policy.toml` disagrees with the rows and I will not start"* is `rm policy.toml` — and on an
//! adopted box deletion removes NOTHING: the row keeps deciding, the daemon boots clean, and the
//! operator now believes they took a cap off that is still on. That lesson would be taught at 06:40
//! under the unattended-upgrade restart window with a page already firing, which is the worst
//! possible moment to teach somebody a false model of where their ceilings live. An inert hand edit
//! would have been converted into a live outage on two daemons and the GUI, and its cure would be a
//! silent ceiling change.
//!
//! So drift warns — at the boot ([`boot_warnings`], on `Settings::warnings`), in `vike-cli config
//! check` at `Level::Warn`, and per-key in `vike-cli config show`'s ORIGIN column. The conditions
//! that genuinely want a daemon STOPPED are the other two: a store that cannot be read at all, and
//! a seal whose integrity check fails ([`crate::mirror::adoption_integrity`]). Those are `Fail`.
//!
//! # ⚠ The comparison is over RESOLVED VALUES, never row bytes
//!
//! Comparing `rows_from_files` against the stored rows would report a difference for a key stated
//! at exactly its default — a real row, rendering a real file line, changing no value. So both
//! sides are resolved into a [`Settings`] and the two are compared:
//!
//! * `file_image` = `Settings::default()` ⊕ the four files, with NO environment and NO CLI layer;
//! * `row_image` = `Settings::default()` ⊕ the store's rows.
//!
//! The env and CLI layers are excluded deliberately and symmetrically: they sit ABOVE both sources,
//! so including them could only ever hide a disagreement by overriding both halves with the same
//! value.
//!
//! `warnings` is NOT compared — the two loads legitimately produce different ones (a `reconcile =
//! false` refusal names its own origin, and one side names a file while the other names the store).
//! The four typed sections are, which is exactly the tuple
//! `crates/vike-config/tests/mirror.rs`'s `values()` already compares.
//!
//! ⚠ **`VenuePolicy::is_declared` is compared for FREE, and it is the axis that matters most.**
//! `Policy` derives `PartialEq` and `VenuePolicy`'s `declared` and `accounts` are ordinary fields
//! despite being `#[serde(skip)]` — so the derived comparison sees them, where a leaf walk over the
//! serialized form could not. That axis is the one that silently unmounts a box: `VenuePolicy`'s
//! default is every roster venue at `Paper`, so a file that never stated an arming and a file that
//! stated every venue as `paper` produce byte-identical MAPS and opposite states of knowledge.
//!
//! # ⚠ A settings file that will not PARSE is drift, not a refusal
//!
//! On an adopted box a broken `policy.toml` is inert. Refusing to boot over a file whose contents
//! reach no value would be the loudest possible way to be wrong. [`compare_sources`] therefore reports a
//! parse failure as a DIFFERENCE with its message, and never as an error return.

use std::collections::HashMap;
use std::path::Path;

use vike_secrets::StoredSettings;

use crate::layers::CliOverrides;
use crate::load::{Settings, settings_files};
use crate::source::StoreLayer;

/// What the two resolutions disagree about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drift {
    /// The settings files that exist on disk, by name, in `SECTION_FILES` order. On an adopted box
    /// every one of these is INERT.
    pub files_present: Vec<&'static str>,
    /// The dotted keys whose resolved values differ, each with the file's answer and the store's.
    /// Empty when the two resolutions are identical — which is the state `vike-cli config adopt`
    /// refuses to seal without.
    pub keys: Vec<DriftedKey>,
    /// A settings file that could not be READ or PARSED during the comparison. Not an error: on an
    /// adopted box such a file is inert, and a boot refusal over it would be the wrong lesson at
    /// the worst time. See this module's doc.
    pub unreadable: Vec<String>,
}

impl Drift {
    /// Do the two resolutions agree, with nothing unreadable? The condition `vike-cli config adopt`
    /// requires before it writes the seal.
    #[must_use]
    pub fn is_identical(&self) -> bool {
        self.keys.is_empty() && self.unreadable.is_empty()
    }
}

/// One key the two resolutions disagree about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriftedKey {
    /// The dotted key as an operator names it — `policy.max_notional_per_order`. Spelled from
    /// [`crate::provenance::setting_keys`]' leaf walk, so the message speaks `config show`'s
    /// vocabulary rather than a second one.
    pub key: String,
    /// What the FILES resolve it to, rendered.
    pub file: String,
    /// What the STORE's rows resolve it to, rendered.
    pub store: String,
}

/// **Resolve this box twice — files alone, rows alone — and report every disagreement.**
///
/// The engine behind `vike-cli config compare`, `vike-cli config adopt`'s precondition, `vike-cli
/// config check`'s drift finding and [`boot_warnings`]. ONE implementation, so a boot warning and
/// the command an operator runs to investigate it can never answer differently.
///
/// Neither half applies the environment or the CLI layer — see this module's doc.
#[must_use]
pub fn compare_sources(settings_dir: &Path, rows: &StoredSettings) -> Drift {
    let empty = HashMap::new();
    let cli = CliOverrides::default();

    let mut unreadable = Vec::new();
    let mut files_present = Vec::new();
    for path in settings_files(settings_dir) {
        if path.is_file()
            && let Some(name) = crate::mirror::SECTION_FILES
                .iter()
                .map(|(_, f)| *f)
                .find(|f| path.file_name().is_some_and(|n| n == *f))
        {
            files_present.push(name);
        }
    }

    // The FILE image. A parse failure is reported, never returned: see this module's doc.
    let file_image = match crate::load_with_source(
        Some(settings_dir),
        StoreLayer::NotConsulted(FILES_HALF),
        &empty,
        &cli,
    ) {
        Ok(s) => s,
        Err(e) => {
            unreadable.push(format!("the settings files do not resolve: {e}"));
            return Drift { files_present, keys: Vec::new(), unreadable };
        }
    };

    // The ROW image. `StoreLayer::Rows { adopted: None }` rather than the adopted arm: this is a
    // COMPARISON and must not inherit the adopted path's integrity refusal — `config compare` is
    // one of the commands an operator reaches for precisely when that refusal has fired.
    let row_image =
        match crate::load_with_source(None, StoreLayer::Rows { rows, adopted: None }, &empty, &cli)
        {
            Ok(s) => s,
            Err(e) => {
                unreadable.push(format!("the settings rows do not resolve: {e}"));
                return Drift { files_present, keys: Vec::new(), unreadable };
            }
        };

    Drift { files_present, keys: differing_keys(&file_image, &row_image), unreadable }
}

/// What [`compare_sources`]'s file half declares to [`crate::load_with_source`].
const FILES_HALF: &str = "`vike_config::drift::compare` resolves the FILE half deliberately store-blind: it is one side \
     of a comparison, not a resolution this process runs on";

/// The dotted keys whose resolved values differ between two [`Settings`].
///
/// Derived from [`crate::provenance::setting_keys`]' leaf walk so the vocabulary is `config show`'s
/// — plus the two axes that walk structurally cannot see, each named explicitly below.
fn differing_keys(files: &Settings, store: &Settings) -> Vec<DriftedKey> {
    let mut out = Vec::new();
    for spec in crate::provenance::setting_keys() {
        let a = crate::provenance::effective_value(files, &spec);
        let b = crate::provenance::effective_value(store, &spec);
        if a != b {
            out.push(DriftedKey {
                key: spec.key.clone(),
                file: a.unwrap_or_else(|| UNSET.to_string()),
                store: b.unwrap_or_else(|| UNSET.to_string()),
            });
        }
    }

    // ⚠ **`is_declared` is `#[serde(skip)]`, so the leaf walk above cannot see it** — and it is the
    // one axis whose disagreement silently unmounts a box. `VenuePolicy::default()` is every roster
    // venue at `Paper`, so a file that stated `[venues]` with everything at `paper` and a file that
    // stated nothing produce the identical MAP; only this flag separates them, and only it decides
    // whether `vike_mount::venue_arming_migration`'s paste-ready banner fires.
    if files.policy.venues.is_declared() != store.policy.venues.is_declared() {
        out.push(DriftedKey {
            key: "policy.venues (stated at all)".to_string(),
            file: files.policy.venues.is_declared().to_string(),
            store: store.policy.venues.is_declared().to_string(),
        });
    }

    // ...and the per-ACCOUNT ceilings, which ride `policy.accounts` as one leaf and are compared
    // here by their resolved map so a disagreeing LABEL is named rather than folded into one row.
    let (fa, sa) =
        (files.policy.venues.accounts_by_venue(), store.policy.venues.accounts_by_venue());
    if fa != sa {
        out.push(DriftedKey {
            key: "policy.accounts".to_string(),
            file: format!("{fa:?}"),
            store: format!("{sa:?}"),
        });
    }

    out
}

/// **The boot's drift warnings** — what [`crate::load_with_source`] pushes onto
/// `Settings::warnings` on an adopted box that still carries settings files.
///
/// Empty when no file is present, which is the state the deletion PR leaves behind and the one this
/// whole module stops mattering in.
///
/// ⚠ It re-resolves both halves, so it costs a second parse of four small TOML files at every boot
/// of an adopted box. That is the price of the only thing on the box that can tell an operator
/// their edit did nothing, and it is paid exactly once per process.
#[must_use]
pub fn boot_warnings(settings_dir: &Path, resolved: &Settings) -> Vec<String> {
    let files: Vec<String> = settings_files(settings_dir)
        .iter()
        .filter(|p| p.is_file())
        .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(str::to_string))
        .collect();

    if files.is_empty() {
        return Vec::new();
    }

    let mut out = vec![format!(
        "this box resolves its settings from the settings DATABASE (`vike-cli config adopt` sealed \
         it), so {} on disk {} INERT — editing one changes nothing. `vike-cli config mirror` files \
         the file's values into the rows; `vike-cli config set` writes both.",
        files.join(", "),
        if files.len() == 1 { "is" } else { "are" }
    )];

    // The per-key half needs the rows, and this function is called from inside the load that
    // already has them — so it takes the RESOLVED settings and compares against a files-only
    // resolution, which is the same pair `compare` builds from the other end.
    let empty = HashMap::new();
    let cli = CliOverrides::default();
    match crate::load_with_source(
        Some(settings_dir),
        StoreLayer::NotConsulted(FILES_HALF),
        &empty,
        &cli,
    ) {
        Ok(file_image) => {
            let keys = differing_keys(&file_image, resolved);
            if !keys.is_empty() {
                let named: Vec<String> = keys
                    .iter()
                    .map(|k| format!("{} (file {}, in force {})", k.key, k.file, k.store))
                    .collect();
                out.push(format!(
                    "...and {} of them DISAGREE with what this box is running on: {}. Run \
                     `vike-cli config compare`.",
                    named.len(),
                    named.join("; ")
                ));
            }
        }
        Err(e) => out.push(format!(
            "...and one of them does not even parse ({e}). On this box that file is inert, so this \
             is a warning and not a refusal — but it is also not the file anything is reading."
        )),
    }
    out
}

/// How a key nothing set is RENDERED in a drift report.
///
/// It is a word rather than an empty cell because the two sides of this comparison are two whole
/// sources, and *this source does not set the key* is the commonest and most consequential
/// disagreement there is — a blank would read as a rendering accident.
const UNSET: &str = "(unset)";
