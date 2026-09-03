//! The Data Manager's **Venues** sub-tab — *what is live, and what is not*, per venue, with the
//! switch that changes it.
//!
//! # Why it lives in the Data Manager
//!
//! The same argument [`super::stored::save_polymarket_proxy`] makes for the proxy box: this is
//! where an operator comes when a venue is not doing what they expect. "Which venues does this box
//! trade on" had no answer anybody could read anywhere — the startup banner said it once, into a
//! log — and the setting that decides it (`policy.venues.<venue>`, the arming CEILING) was
//! editable only by hand or over the tradehub control wire, which is OFF on the measured
//! deployment.
//!
//! # The four columns and where each one comes from
//!
//! | column | source |
//! |---|---|
//! | Venue | `vike_model::VENUES`, through the row's `&'static str` — plus the ACCOUNT label when the venue has more than one |
//! | Mode | `policy.venues.<venue>`, or `policy.accounts.<venue>.<LABEL>` for a second account — the operator's CEILING, [`vike_config::VenueMode`] |
//! | Credentials | `vike_connections::credential_status_for_account` for **THIS ROW'S account** — the same enumerator the Connections grid calls, tier NAMES and presence only |
//! | Effective | `vike_run::venue_arming`, i.e. the SAME `venue_account_arming` the mount itself selects accounts with |
//!
//! ⚠ **Effective is the column that earns the screen**, and it is why the row type
//! ([`vike_config::VenueArming`]) is produced by the MOUNT rather than re-derived here. The
//! complaint this tab would otherwise generate is *"I set live and it is still paper"*, and a
//! column computed from the ceiling alone would generate it on the very first use: the ceiling is
//! a `min`, so it can refuse an arming and never create one. Every capped row therefore carries a
//! [`vike_config::ArmingBlock`] naming the specific cause — no credentials, `{VENUE}_MAINNET`
//! unset, `POLY_EXEC` unset, the cargo feature this build lacks, the ceiling itself, an account with
//! no line of its own, an account whose symbol another account of the same venue already took.
//!
//! # Credentials: names and presence, never a value
//!
//! The column renders the three tier NAMES with a present/absent dot, exactly as the Connections
//! grid does. Nothing here reads, formats, logs or journals a secret — a UI that renders one is a
//! defect (root `CLAUDE.md`, "Credentials & the live gate"), and the write below carries no
//! credential at all.
//!
//! ⚠ **And per ACCOUNT, since a venue could have two rows.** The cell was keyed on the venue while
//! the rows were keyed on the account, so every labelled row showed the DEFAULT account's dots —
//! a labelled row claiming credentials that belong to somebody else.
//! [`VenueArmingInputs::creds_for`] is the fix, and it deliberately does NOT fall back to the
//! default account's row: a labelled account with no keys of its own reads all-absent, which is
//! what the mount will do with it. The tier names are the only strings that reach the widget tree
//! either way — the label half of a key name never does, and a value never has.
//!
//! # The switch: asymmetric on purpose
//!
//! * **Loosening** (`paper`→`demo`, anything→`live`) reuses the ratified typed-confirm ceremony
//!   VERBATIM — the operator types the row's exact dotted key, and the confirm box is never
//!   pre-filled. The one decision site is [`super::backend_settings::can_save_fields`], shared with
//!   the wire path's `SettingsEditState`; the daemon applies the same rule server-side
//!   (`crates/vike-tradehub/src/server.rs`'s `apply_set_setting`). Arming real money must not be
//!   one click.
//! * **Tightening** (anything→`paper`, `live`→`demo`) is a plain click. An action that can only
//!   REDUCE authority must not be ceremonious — ceremony on the safe direction is how operators
//!   learn to type past it on the dangerous one.
//!
//! ⚠ The gate is enforced in [`apply_arming`], the function that performs the write, not only in
//! the renderer — the same shape the daemon uses, so a future second caller cannot route around it.
//!
//! # The LOCAL write, and why it is new plumbing
//!
//! `vike-app`'s only settings write until now was `vike_tradehub_client::set_setting` over the
//! control wire, and on the measured the CI box box `flags.tradehub_control` is OFF, so that path does
//! not exist there. This tab writes THIS process's own `<project>/settings/policy.toml` through
//! [`vike_config::set_setting`] — comment-preserving, validated through the loader BEFORE a byte
//! lands, atomic, one key.
//!
//! ⚠ The directory is a **parameter** ([`VenueArmingInputs::settings_dir`]), taken from the
//! binary's ONE boot walk (`vike_boot::Booted`, held in `vike-app`'s `SETTINGS_DIR` cell) — never a
//! resolver call from in here. The `_from`-less resolvers are `$VIKE_SETTINGS_DIR`-BLIND, and this
//! surface writing into whatever project the working directory sat above is the exact defect
//! `vike_connections::CredentialHome` was built to close for the credential store. A boot that
//! found no project yields `None` and the switch is DISABLED with that as its reason, rather than
//! guessing a path.
//!
//! # Restart-to-apply, said out loud
//!
//! `vike-tradehub`'s `hot_reload::classify` seals every `SettingsFile::Policy` key into
//! `HotClass::Restart`, and this process is no different: the mount reads the ceiling once, at
//! `make_engine`. So an accepted write renders [`ARM_RESTART_NOTE`]. That is honest rather than a
//! wart — the alternative is an operator flipping a switch, seeing nothing change, and concluding
//! the screen is broken.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use vike_config::{ArmingBlock, SettingsFile, VenueArming, VenueMode};
use vike_connections::VenueCredStatus;
use vike_model::account_keys::AccountLabel;
use vike_model::change_journal::{Actor, Change, ChangeJournal, Outcome};

use crate::split_plane::AppMode;

/// The sub-tab's label in the Data Manager's tab strip.
pub const VENUES_TAB_LABEL: &str = "Venues";

/// The accepted-write banner. Policy keys are `HotClass::Restart` without exception, and this
/// process reads a venue's ceiling once, at mount — so every accepted write here is restart-to-apply.
pub const ARM_RESTART_NOTE: &str =
    "saved to policy.toml — restart vike-app for it to take effect (a venue's ceiling is read once, \
     at mount)";

/// The refusal when the boot walk found no project, so there is nowhere to write.
pub const NO_SETTINGS_DIR: &str =
    "no settings directory: this process found no project above its working directory, so there is \
     no policy.toml to write. Start vike-app from inside the project, or set VIKE_SETTINGS_DIR.";

/// The banner for a fat build that is OBSERVING a remote backend.
pub const OBSERVING_NOTE: &str =
    "observing a remote backend — this file governs THIS process (which mounts no venues while \
     observing). To arm the backend's own venues, use Connections → Backend settings.";

/// The banner for a thin (`--observe`-only) build, which links no venue mount at all.
pub const THIN_BUILD_NOTE: &str =
    "this thin (--observe) build links no venue mount, so the Effective column cannot be computed \
     here — the ceilings below are still this project's policy.toml, and a fat build reading the \
     same file will honour them.";

// -------------------------------------------------------------------------------------------
// Inputs
// -------------------------------------------------------------------------------------------

/// Everything the tab renders, resolved ONCE per frame by the binary — which is the only place
/// that can resolve it: the rows come from `vike-mount` (a `fat`-only dependency) and the
/// directory comes from the boot walk.
///
/// Owned rather than borrowed because the shell builds it inside the `Data` dispatch arm and drops
/// it after the draw; every field is small (one row per ACCOUNT — roster-length on every box with no
/// `[accounts]` table — a roster-length Vec of three bools and a name, one path).
#[derive(Debug, Clone)]
pub struct VenueArmingInputs {
    /// One row per ACCOUNT — `vike_run::venue_arming` in a fat build,
    /// `vike_config::venue_arming::ceilings_only` in a thin one (which enumerates the DEFAULT
    /// account only, because it can enumerate no other). A box with no `[accounts]` table and no
    /// labelled credential key therefore gets exactly one row per roster venue, as it always did.
    pub rows: Vec<VenueArming>,
    /// Tier presence per venue for the **DEFAULT account**, from
    /// `vike_connections::credential_status` — the SAME producer the Connections grid renders.
    /// Names and presence only.
    pub creds: Vec<VenueCredStatus>,
    /// The same grid for each LABELLED account any row names, one entry per distinct label —
    /// `vike_connections::credential_status_for_account`, the account-aware face of the very same
    /// enumerator, which reads `{VENUE}_{TIER}{SUFFIX}__{LABEL}` keys.
    ///
    /// ⚠ **EMPTY on a box with no labelled account**, which is every box with no `[accounts]`
    /// table: the labels are derived from [`Self::rows`], and a single-account box's rows all carry
    /// [`AccountLabel::Default`]. So the extra grids are not merely cheap there, they do not exist,
    /// and [`Self::creds_for`] resolves every row through [`Self::creds`] exactly as it did before
    /// this field was added.
    pub labelled_creds: Vec<(AccountLabel, Vec<VenueCredStatus>)>,
    /// `<project>/settings`, as the binary's ONE boot walk resolved it. `None` ⇒ no project, so the
    /// switch is disabled with [`NO_SETTINGS_DIR`] as its reason.
    pub settings_dir: Option<PathBuf>,
    /// Which of the three compositions this process is — decides the banner and whether the
    /// Effective column is answerable at all.
    pub mode: AppMode,
}

impl VenueArmingInputs {
    /// Build from the two things the shell already holds plus the boot's directory. `vars` is the
    /// binary's fresh credential-map read; it is consumed HERE into `credential_status` and never
    /// stored, so no credential value survives the constructor.
    ///
    /// ⚠ **The LABELS come from `rows`, not from the store.** Enumerating the store's accounts
    /// instead would list an account the mount produced no row for — and the row set is what this
    /// screen renders, so a grid keyed on anything else would be a column with no row to sit in.
    /// A box whose rows are all default-account rows therefore builds exactly one grid, as before.
    #[must_use]
    pub fn new(
        rows: Vec<VenueArming>,
        vars: &HashMap<String, String>,
        settings_dir: Option<PathBuf>,
        mode: AppMode,
    ) -> Self {
        let mut labels: Vec<AccountLabel> =
            rows.iter().filter(|r| !r.is_default_account()).map(|r| r.label.clone()).collect();
        labels.sort();
        labels.dedup();
        let labelled_creds = labels
            .into_iter()
            .map(|l| {
                let grid = vike_connections::credential_status_for_account(vars, &l);
                (l, grid)
            })
            .collect();
        Self {
            rows,
            creds: vike_connections::credential_status(vars),
            labelled_creds,
            settings_dir,
            mode,
        }
    }

    /// Whether this build can compute an Effective tier at all. `false` for a thin build, whose
    /// rows carry [`ArmingBlock::NoMountInThisBuild`].
    #[must_use]
    pub fn effective_is_answerable(&self) -> bool {
        self.mode != AppMode::ObserveOnly
    }

    /// The one line above the table, or `None` for the ordinary local-core case where the table
    /// says everything.
    #[must_use]
    pub fn banner(&self) -> Option<&'static str> {
        match self.mode {
            AppMode::LocalCore => None,
            AppMode::ObserveWithFeeds => Some(OBSERVING_NOTE),
            AppMode::ObserveOnly => Some(THIN_BUILD_NOTE),
        }
    }

    /// This venue's credential row for the **DEFAULT account**, or `None` for a venue
    /// `credential_status` does not carry.
    #[must_use]
    pub fn creds_of(&self, venue: &str) -> Option<&VenueCredStatus> {
        self.creds.iter().find(|c| c.venue == venue)
    }

    /// **This ACCOUNT's credential row** — what the Credentials cell renders, and the reason that
    /// cell stopped lying once a venue had two rows.
    ///
    /// The column used to be [`Self::creds_of`], keyed on the VENUE alone, so every account row of
    /// a venue showed the DEFAULT account's tier dots: a row labelled `bybit account \`ALT\`` was
    /// claiming credentials that belong to somebody else. A labelled row now reads its own grid.
    ///
    /// ⚠ [`AccountLabel::Default`] short-circuits to [`Self::creds_of`], so a single-account box
    /// resolves through the identical `Vec` it always did. A labelled row whose grid is somehow
    /// absent falls back to `None` — all-absent dots — rather than to the default account's row:
    /// borrowing them is the exact defect this function exists to close.
    #[must_use]
    pub fn creds_for(&self, venue: &str, label: &AccountLabel) -> Option<&VenueCredStatus> {
        if label.is_default() {
            return self.creds_of(venue);
        }
        self.labelled_creds
            .iter()
            .find(|(l, _)| l == label)
            .and_then(|(_, grid)| grid.iter().find(|c| c.venue == venue))
    }
}

/// **Re-read the venue ceilings from `policy.toml` on disk**, so the Mode column shows what the
/// FILE says now rather than what this process loaded at boot.
///
/// # Why a re-read at all
///
/// A policy key is `HotClass::Restart`: the running mount keeps its boot-time ceiling. If the Mode
/// column showed that boot value, an operator who flipped a switch would watch the row not change
/// and conclude the screen is broken. So the column shows the FILE, the Effective column shows what
/// the NEXT start will therefore mount, and [`ARM_RESTART_NOTE`] is what reconciles the two.
///
/// # ⚠ Why it lives HERE and not in `vike-app`'s `main.rs`
///
/// `crates/vike-boot/tests/one_owner.rs` fails a composition root that calls `vike_config::load`,
/// and it is right to: the defect it gates is the SETTINGS WALK happening more than once, which on
/// the CI box produced a daemon with no policy and no credentials. This function cannot walk — the
/// directory is a PARAMETER, threaded from the binary's one boot answer — so putting it in the
/// library is not a way around that gate, it is the shape the gate is asking for.
///
/// # ⚠ Why the EMPTY environment map is the COMPLETE input
///
/// Not a shortcut. `vike_config::Policy` implements neither `EnvOverride` nor `CliOverride` (both
/// sealed), so no environment can set or move a venue ceiling, and
/// `crates/vike-config/tests/venues_table.rs`'s
/// `nothing_in_the_environment_can_set_a_venue_ceiling` asserts exactly that against a hostile map.
/// The `[venues]` table's only source is the file, so the file is the only thing this needs to read.
///
/// `None` — no settings directory, or a file the loader refuses (somebody hand-broke it since boot)
/// — means the caller keeps the boot-time ceilings. The write path faces the SAME loader and
/// reports the breakage with its own message, so a refused reload is never a silent swallow.
#[must_use]
pub fn reload_venue_ceilings(settings_dir: Option<&Path>) -> Option<vike_config::VenuePolicy> {
    let dir = settings_dir?;
    vike_config::load(Some(dir), &HashMap::new()).ok().map(|s| s.policy.venues)
}

// -------------------------------------------------------------------------------------------
// The switch: direction, confirm state, and the write
// -------------------------------------------------------------------------------------------

/// Which way a requested change moves a venue's authority — the ONE thing that decides whether the
/// typed confirm is demanded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmDirection {
    /// The target equals the current ceiling: nothing to do.
    Unchanged,
    /// The target is BELOW the current ceiling — the switch can only reduce authority, so it is a
    /// plain click.
    Tighten,
    /// The target is ABOVE the current ceiling — the typed confirm is demanded.
    Loosen,
}

/// Classify a requested change. Uses [`VenueMode`]'s own `Ord` (declared in ascending risk order),
/// so this cannot disagree with `VenueMode::cap` about which direction is dangerous.
#[must_use]
pub fn arm_direction(current: VenueMode, target: VenueMode) -> ArmDirection {
    match target.cmp(&current) {
        std::cmp::Ordering::Equal => ArmDirection::Unchanged,
        std::cmp::Ordering::Less => ArmDirection::Tighten,
        std::cmp::Ordering::Greater => ArmDirection::Loosen,
    }
}

/// The tab's per-window edit flow — at most ONE venue in flight, owned by the UI thread (its
/// confirm buffer is live-edited every frame) and advanced by pure transitions, so the whole
/// click → confirm → saved/failed walk is CI-testable without a window.
///
/// Deliberately the same shape as [`super::backend_settings::SettingsEditState`], the wire path's
/// twin, rather than a reuse of it: that one is keyed on a `WireSettingsRow` (file + free-text
/// value) while this one is keyed on a venue and a [`VenueMode`], and collapsing the two would mean
/// the arming switch could be handed a value the mode enum cannot represent.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ArmEdit {
    /// Nothing in flight.
    #[default]
    Idle,
    /// A LOOSENING is waiting on the typed confirm. `confirm` ALWAYS starts empty.
    Confirming {
        /// The dotted KEY of the row whose ceiling is being raised — `policy.venues.<venue>` for a
        /// venue's default account, `policy.accounts.<venue>.<LABEL>` for a second one.
        ///
        /// ⚠ It was the VENUE id, and it could not stay one: two rows of a venue are two
        /// independently settable keys, so a flow keyed on the venue would put both rows into
        /// confirm at once and write whichever key the renderer happened to build.
        key: String,
        /// How the row names itself in a message — `bybit`, or ``bybit account `ALT```.
        subject: String,
        /// The requested ceiling.
        target: VenueMode,
        /// The confirm buffer — must come to equal `policy.venues.<venue>` before Apply unlocks.
        confirm: String,
    },
    /// The write landed. The row renders [`ARM_RESTART_NOTE`].
    Saved {
        /// The key that was written.
        key: String,
        /// The ceiling now in the file.
        target: VenueMode,
    },
    /// The write was refused (the loader's own message, the confirm contract, a missing settings
    /// directory) — rendered verbatim, and the operator can start again from here.
    Failed {
        /// The key whose write failed.
        key: String,
        /// The refusal, verbatim.
        error: String,
    },
}

/// Begin a LOOSENING: the confirm buffer starts EMPTY (the typed-confirm contract — the operator
/// types the key, the UI never types it for them).
///
/// A tightening never reaches here: it has no ceremony and is applied directly.
#[must_use]
pub fn begin_confirm(key: &str, subject: &str, target: VenueMode) -> ArmEdit {
    ArmEdit::Confirming {
        key: key.to_string(),
        subject: subject.to_string(),
        target,
        confirm: String::new(),
    }
}

/// THE Apply gate for a loosening: the typed confirm must equal the row's exact dotted key.
///
/// It defers to [`super::backend_settings::can_save_fields`] — the SAME one decision site the wire
/// path's Save button consults — rather than re-implementing `confirm == key`, so a change to the
/// ceremony moves both surfaces at once.
#[must_use]
pub fn can_apply(state: &ArmEdit) -> bool {
    match state {
        ArmEdit::Confirming { key, confirm, .. } => {
            super::backend_settings::can_save_fields(SettingsFile::Policy.file_name(), key, confirm)
        }
        _ => false,
    }
}

/// Take the Apply transition: `Confirming` (passing [`can_apply`]) → the `(venue, target, confirm)`
/// the caller hands to [`apply_arming`]. Any other state, or a wrong confirm, yields `None` and
/// leaves the flow untouched — the operator keeps typing.
pub fn take_apply(state: &ArmEdit) -> Option<(String, String, VenueMode, String)> {
    if !can_apply(state) {
        return None;
    }
    let ArmEdit::Confirming { key, subject, target, confirm } = state else {
        unreachable!("can_apply admits only Confirming");
    };
    Some((key.clone(), subject.clone(), *target, confirm.clone()))
}

/// **THE write.** One key, one file, comment-preserving, loader-validated before a byte lands,
/// atomic — and recorded in the change journal.
///
/// # The gate is HERE, not only in the renderer
///
/// `confirm` is checked against the direction exactly as the daemon's `apply_set_setting` checks
/// it: a LOOSENING with no confirm, or with a confirm that is not the exact key, is refused and
/// **nothing is written**. A TIGHTENING needs none. The renderer's Apply button is a convenience
/// on top of this; a second caller cannot route around it.
///
/// # What reaches the ledger
///
/// The key, the old file value, the new one, and `Actor::Gui`. No credential is involved in a
/// policy write at all — this is the first production caller of
/// [`vike_model::change_journal::Change::set_setting`], whose cells are exactly those. A journal
/// failure is logged and does NOT fail the write: the ceiling is already on disk, and reporting the
/// write as refused would be the worse lie.
///
/// Returns the operator-facing note on success ([`ARM_RESTART_NOTE`]) and the refusal verbatim on
/// failure.
///
/// ⚠ The eighth parameter is `subject`, and it is a MESSAGE concern rather than a decision one: the
/// key alone decides everything (which file, which line, whether the confirm matches), while the
/// refusal an operator reads has to name the ACCOUNT rather than a dotted key. Folding the two into
/// one struct would hide the fact that only one of them is load-bearing.
#[allow(clippy::too_many_arguments)]
pub fn apply_arming(
    settings_dir: Option<&Path>,
    key: &str,
    subject: &str,
    current: VenueMode,
    target: VenueMode,
    confirm: Option<&str>,
    journal: Option<&ChangeJournal>,
    now_ms: i64,
) -> Result<&'static str, String> {
    if arm_direction(current, target) == ArmDirection::Loosen {
        match confirm {
            None => {
                return Err(format!(
                    "raising {subject} from `{current}` to `{target}` arms more authority than it \
                     has now, so it needs the typed confirm: type the exact key `{key}`"
                ))
            }
            Some(c)
                if !super::backend_settings::can_save_fields(
                    SettingsFile::Policy.file_name(),
                    key,
                    c,
                ) =>
            {
                return Err(format!(
                    "confirm mismatch: it must equal the exact key `{key}` (got `{c}`) — nothing \
                     was written"
                ))
            }
            Some(_) => {}
        }
    }
    let Some(dir) = settings_dir else {
        return Err(NO_SETTINGS_DIR.to_string());
    };

    match vike_config::set_setting(dir, SettingsFile::Policy, key, target.as_str()) {
        Ok(write) => {
            record(
                journal,
                now_ms,
                Change::set_setting(
                    Outcome::AppliedPendingRestart,
                    Actor::Gui,
                    write.file,
                    &write.key,
                    write.old_value.as_deref(),
                    &write.new_value,
                ),
            );
            tracing::info!(
                kind = "set_setting",
                key = %key,
                mode = target.as_str(),
                "venue arming ceiling written; takes effect on restart"
            );
            Ok(ARM_RESTART_NOTE)
        }
        Err(e) => {
            let error = e.to_string();
            record(
                journal,
                now_ms,
                Change::set_setting(
                    Outcome::Refused,
                    Actor::Gui,
                    SettingsFile::Policy.file_name(),
                    key,
                    None,
                    target.as_str(),
                )
                .with_reason(Some(&error)),
            );
            tracing::error!(key = %key, error = %error, "venue arming write refused");
            Err(error)
        }
    }
}

/// Append one record, swallowing (and logging) a ledger failure. The DISK write already happened —
/// or already did not — and the ledger cannot change that verdict.
fn record(journal: Option<&ChangeJournal>, now_ms: i64, change: Change) {
    if let Some(j) = journal {
        if let Err(e) = j.append(now_ms, &change) {
            tracing::warn!(error = %e, "the venue-arming write is on disk; its ledger record is not");
        }
    }
}

// -------------------------------------------------------------------------------------------
// The render
// -------------------------------------------------------------------------------------------

/// The Credentials cell's text for one venue — tier NAMES with a present/absent dot, exactly the
/// vocabulary the Connections grid uses. `None` (a venue `credential_status` does not carry) reads
/// as all-absent rather than as blank, because blank would be indistinguishable from "not checked".
#[must_use]
pub fn credentials_cell(status: Option<&VenueCredStatus>) -> String {
    let (sim, demo, live) = status.map_or((false, false, false), |s| (s.sim, s.demo, s.live));
    let dot = |on: bool, name: &str| format!("{} {name}", if on { '\u{25CF}' } else { '\u{25CB}' });
    format!("{}  {}  {}", dot(sim, "sim"), dot(demo, "demo"), dot(live, "live"))
}

/// The Effective cell's text: the tier, plus the block's badge in brackets when something is being
/// refused. A thin build renders a dash rather than a tier it cannot know.
#[must_use]
pub fn effective_cell(row: &VenueArming, answerable: bool) -> String {
    if !answerable || row.block == ArmingBlock::NoMountInThisBuild {
        return "—".to_string();
    }
    if row.block.is_clear() {
        row.effective.to_string()
    } else {
        format!("{} ({})", row.effective, row.block.as_str())
    }
}

/// The Venues sub-tab body. `inputs` is `None` only when the shell did not resolve them (it
/// resolves them exactly when this sub-tab is visible), which renders as a loading line rather
/// than as an empty table.
pub fn venues_tab_content(
    ui: &mut egui::Ui,
    inputs: Option<&VenueArmingInputs>,
    state: &mut ArmEdit,
    journal: Option<&ChangeJournal>,
    now_ms: i64,
) {
    let Some(inputs) = inputs else {
        ui.weak("resolving venue arming…");
        return;
    };
    ui.label(
        egui::RichText::new(
            "What each venue is permitted to do, as policy.toml reads RIGHT NOW — so Effective is \
             what the NEXT start will mount, which is what the restart note reconciles. The \
             ceiling can only ever REFUSE; it never arms anything the credentials and flags have \
             not already armed.",
        )
        .size(11.0),
    );
    if let Some(note) = inputs.banner() {
        ui.add_space(2.0);
        ui.weak(note);
    }
    if inputs.settings_dir.is_none() {
        ui.add_space(2.0);
        ui.colored_label(egui::Color32::from_rgb(220, 120, 90), NO_SETTINGS_DIR);
    }
    ui.add_space(6.0);

    let answerable = inputs.effective_is_answerable();
    let writable = inputs.settings_dir.is_some();
    // ⚠ Keyed by the ROW'S dotted KEY, not by the venue: a venue can have two rows now, and they
    // write two different keys. `subject` rides along so a message can still name the account.
    let mut requested: Option<(String, String, VenueMode, VenueMode)> = None;
    let mut applied: Option<(String, String, VenueMode, VenueMode, String)> = None;

    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("venue_arming").show(
        ui,
        |ui| {
            egui::Grid::new("venue_arming_grid").num_columns(5).striped(true).show(ui, |ui| {
                ui.strong("Venue");
                ui.strong("Mode");
                ui.strong("Credentials");
                ui.strong("Effective");
                ui.strong("Switch");
                ui.end_row();

                for row in &inputs.rows {
                    let row_key = row.key();
                    let subject = row.subject();
                    // The Venue cell names the ACCOUNT when there is more than one of them; a
                    // default-account row reads exactly as it always did.
                    ui.label(subject.clone());
                    ui.label(row.ceiling.as_str());
                    // ⚠ The ACCOUNT's credentials, not the venue's: this cell was `creds_of(row.venue)`
                    // until a venue could have two rows, and it then showed the DEFAULT account's
                    // tier dots on every labelled row.
                    ui.label(credentials_cell(inputs.creds_for(row.venue, &row.label)));
                    ui.label(effective_cell(row, answerable)).on_hover_text(row.why());
                    ui.horizontal(|ui| {
                        for target in VenueMode::ALL {
                            // ⚠ A plain `Button`, not a `SelectableLabel`: the label must be the
                            // MODE NAME and nothing else, because that label is the handle the
                            // headless gate (`crates/vike-app-core/tests/venue_arming_screen.rs`)
                            // clicks by. The CURRENT mode is shown by the Mode column and by this
                            // button being inert, never by decorating its text.
                            let on = target == row.ceiling;
                            let resp =
                                ui.add_enabled(writable && !on, egui::Button::new(target.as_str()));
                            if resp.clicked() {
                                requested =
                                    Some((row_key.clone(), subject.clone(), row.ceiling, target));
                            }
                        }
                    });
                    ui.end_row();

                    // The per-row second line: the confirm box for THIS venue, or its outcome.
                    match state {
                        ArmEdit::Confirming { key, target, confirm, .. } if *key == row_key => {
                            let key = key.clone();
                            ui.label("");
                            ui.label("");
                            ui.label(format!("→ {target}"));
                            ui.label(
                                egui::RichText::new(format!("type `{key}` to confirm")).size(11.0),
                            );
                            let allowed = super::backend_settings::can_save_fields(
                                SettingsFile::Policy.file_name(),
                                &key,
                                confirm,
                            );
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::TextEdit::singleline(confirm)
                                        .hint_text(key.as_str())
                                        .desired_width(220.0),
                                );
                                if ui.add_enabled(allowed, egui::Button::new("Arm")).clicked() {
                                    applied = Some((
                                        row_key.clone(),
                                        subject.clone(),
                                        row.ceiling,
                                        *target,
                                        confirm.clone(),
                                    ));
                                }
                                if ui.button("Cancel").clicked() {
                                    requested = Some((
                                        row_key.clone(),
                                        subject.clone(),
                                        row.ceiling,
                                        row.ceiling,
                                    ));
                                }
                            });
                            ui.end_row();
                        }
                        ArmEdit::Saved { key, .. } if *key == row_key => {
                            ui.label("");
                            ui.label("");
                            ui.label("");
                            ui.label(egui::RichText::new(ARM_RESTART_NOTE).size(11.0));
                            ui.label("");
                            ui.end_row();
                        }
                        ArmEdit::Failed { key, error } if *key == row_key => {
                            ui.label("");
                            ui.label("");
                            ui.label("");
                            ui.colored_label(
                                egui::Color32::from_rgb(220, 120, 90),
                                egui::RichText::new(error.as_str()).size(11.0),
                            );
                            ui.label("");
                            ui.end_row();
                        }
                        _ => {}
                    }
                }
            });
        },
    );

    // ── the deferred mutations, after the borrow of `inputs.rows` ends ──────────────────────
    if let Some((key, subject, current, target)) = requested {
        *state = match arm_direction(current, target) {
            // A tightening (or the Cancel above, which requests the CURRENT mode) needs no
            // ceremony; `Unchanged` simply clears the flow.
            ArmDirection::Unchanged => ArmEdit::Idle,
            ArmDirection::Tighten => {
                match apply_arming(
                    inputs.settings_dir.as_deref(),
                    &key,
                    &subject,
                    current,
                    target,
                    None,
                    journal,
                    now_ms,
                ) {
                    Ok(_) => ArmEdit::Saved { key, target },
                    Err(error) => ArmEdit::Failed { key, error },
                }
            }
            ArmDirection::Loosen => begin_confirm(&key, &subject, target),
        };
    } else if let Some((key, subject, current, target, confirm)) = applied {
        *state = match apply_arming(
            inputs.settings_dir.as_deref(),
            &key,
            &subject,
            current,
            target,
            Some(&confirm),
            journal,
            now_ms,
        ) {
            Ok(_) => ArmEdit::Saved { key, target },
            Err(error) => ArmEdit::Failed { key, error },
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(venue: &'static str, ceiling: VenueMode) -> VenueArming {
        VenueArming {
            venue,
            label: vike_model::account_keys::AccountLabel::Default,
            ceiling,
            effective: ceiling,
            block: ArmingBlock::None,
        }
    }

    /// The direction table — the ONE thing that decides whether ceremony is demanded, over all
    /// nine ordered pairs rather than the three interesting ones.
    #[test]
    fn every_raise_is_a_loosening_and_every_drop_is_a_tightening() {
        for current in VenueMode::ALL {
            for target in VenueMode::ALL {
                let dir = arm_direction(current, target);
                match dir {
                    ArmDirection::Unchanged => assert_eq!(current, target),
                    ArmDirection::Tighten => assert!(target < current, "{current} → {target}"),
                    ArmDirection::Loosen => assert!(target > current, "{current} → {target}"),
                }
                // …and the classification agrees with the cap: a tightening is exactly a target
                // the ceiling fold would have chosen anyway.
                if dir == ArmDirection::Tighten {
                    assert_eq!(target.cap(current), target);
                }
            }
        }
        assert_eq!(arm_direction(VenueMode::Paper, VenueMode::Live), ArmDirection::Loosen);
        assert_eq!(arm_direction(VenueMode::Live, VenueMode::Paper), ArmDirection::Tighten);
        assert_eq!(arm_direction(VenueMode::Demo, VenueMode::Demo), ArmDirection::Unchanged);
    }

    /// The confirm buffer is never pre-filled, and only the EXACT key unlocks Apply.
    #[test]
    fn the_confirm_starts_empty_and_only_the_exact_key_unlocks_it() {
        let mut state = begin_confirm("policy.venues.bybit", "bybit", VenueMode::Live);
        match &state {
            ArmEdit::Confirming { key, target, confirm, .. } => {
                assert_eq!(key, "policy.venues.bybit");
                assert_eq!(*target, VenueMode::Live);
                assert!(confirm.is_empty(), "the confirm buffer is NEVER pre-filled");
            }
            other => panic!("{other:?}"),
        }
        assert!(!can_apply(&state));
        assert_eq!(take_apply(&state), None);

        for near_miss in ["policy.venues", "venues.bybit", "policy.venues.bybit ", "BYBIT"] {
            if let ArmEdit::Confirming { confirm, .. } = &mut state {
                *confirm = near_miss.to_string();
            }
            assert!(!can_apply(&state), "{near_miss:?} must not unlock the switch");
            assert_eq!(take_apply(&state), None, "{near_miss:?}");
        }

        if let ArmEdit::Confirming { confirm, .. } = &mut state {
            *confirm = "policy.venues.bybit".to_string();
        }
        assert!(can_apply(&state));
        assert_eq!(
            take_apply(&state),
            Some((
                "policy.venues.bybit".to_string(),
                "bybit".to_string(),
                VenueMode::Live,
                "policy.venues.bybit".to_string()
            ))
        );
        // …and `take_apply` does NOT advance the flow: the caller owns the transition, so a
        // refused write can leave the operator exactly where they were.
        assert!(matches!(state, ArmEdit::Confirming { .. }));
    }

    /// Nothing but `Confirming` can apply.
    #[test]
    fn no_other_state_can_apply() {
        for state in [
            ArmEdit::Idle,
            ArmEdit::Saved { key: "policy.venues.bybit".into(), target: VenueMode::Live },
            ArmEdit::Failed { key: "policy.venues.bybit".into(), error: "nope".into() },
        ] {
            assert!(!can_apply(&state), "{state:?}");
            assert_eq!(take_apply(&state), None, "{state:?}");
        }
    }

    /// The Credentials cell renders tier NAMES and a presence dot — never a value, and never a
    /// credential KEY name either.
    #[test]
    fn the_credentials_cell_shows_names_and_presence_only() {
        let status =
            VenueCredStatus { venue: "binance".into(), sim: false, demo: true, live: true };
        let cell = credentials_cell(Some(&status));
        assert!(cell.contains("\u{25CB} sim"), "{cell}");
        assert!(cell.contains("\u{25CF} demo"), "{cell}");
        assert!(cell.contains("\u{25CF} live"), "{cell}");
        assert!(!cell.contains("API_KEY"), "no credential key name reaches the cell: {cell}");

        // An absent row reads as all-absent, never as blank.
        let none = credentials_cell(None);
        assert!(none.contains("\u{25CB} sim") && none.contains("\u{25CB} live"), "{none}");
    }

    /// The Effective cell names the tier, and the badge appears exactly when something is refusing.
    #[test]
    fn the_effective_cell_shows_the_tier_and_names_the_block() {
        let clear = row("bybit", VenueMode::Demo);
        assert_eq!(effective_cell(&clear, true), "demo");

        let capped = VenueArming {
            venue: "bybit",
            label: vike_model::account_keys::AccountLabel::Default,
            ceiling: VenueMode::Live,
            effective: VenueMode::Demo,
            block: ArmingBlock::MainnetSwitchUnset,
        };
        let cell = effective_cell(&capped, true);
        assert!(cell.starts_with("demo"), "{cell}");
        assert!(cell.contains(ArmingBlock::MainnetSwitchUnset.as_str()), "{cell}");

        // A build that cannot answer renders a dash rather than a tier it does not know.
        let thin = VenueArming { block: ArmingBlock::NoMountInThisBuild, ..clear.clone() };
        assert_eq!(effective_cell(&thin, false), "—");
        assert_eq!(effective_cell(&thin, true), "—", "the row's own block wins either way");
    }

    /// The banner says which composition this is, and only the local-core case is silent.
    #[test]
    fn each_composition_says_what_it_can_and_cannot_answer() {
        let build = |mode| VenueArmingInputs {
            rows: vec![row("bybit", VenueMode::Paper)],
            creds: Vec::new(),
            labelled_creds: Vec::new(),
            settings_dir: Some(PathBuf::from("/nowhere")),
            mode,
        };
        assert_eq!(build(AppMode::LocalCore).banner(), None);
        assert!(build(AppMode::LocalCore).effective_is_answerable());

        assert_eq!(build(AppMode::ObserveWithFeeds).banner(), Some(OBSERVING_NOTE));
        assert!(
            build(AppMode::ObserveWithFeeds).effective_is_answerable(),
            "a fat observer still LINKS the mount, so the column is computable"
        );
        // …and it points at the surface that edits the REMOTE node, rather than implying this file
        // governs it.
        assert!(OBSERVING_NOTE.contains("Backend settings"));

        assert_eq!(build(AppMode::ObserveOnly).banner(), Some(THIN_BUILD_NOTE));
        assert!(!build(AppMode::ObserveOnly).effective_is_answerable());
    }
}
