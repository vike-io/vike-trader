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
//! | Effective | a dash today — `vike_config::venue_arming::ceilings_only` claims no tier (see [`VenueArmingInputs::rows`]); until 2026-09-09 it was `vike_mount::venue_arming` (reached through `vike-run`'s wrapper, which merged into `vike-mount` under decision 0098), the SAME `venue_account_arming` the mount itself selects accounts with |
//!
//! ⚠ **Effective is the column that earns the screen**, and it is why the row type
//! ([`vike_config::VenueArming`]) is produced by the MOUNT rather than re-derived here — in a
//! process that links one. The desktop has linked none since the `fat` build was deleted on
//! 2026-09-09 (#1727), so its rows are ceilings only and the column renders a dash. The
//! complaint this tab would otherwise generate is *"I set live and it is still paper"*, and a
//! column computed from the ceiling alone would generate it on the very first use: the ceiling is
//! a `min`, so it can refuse an arming and never create one. Every capped row therefore carries a
//! [`vike_config::ArmingBlock`] naming the specific cause — no LIVE-tier credentials (decision 0095:
//! for a CEX venue or hyperliquid this is what a `live` ceiling with only DEMO-tier keys now means),
//! `POLY_EXEC` unset, the cargo feature this build lacks, the ceiling itself, an account with
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
//! # The switch: ONE CLICK, either direction (`docs/decisions/0086` point 7)
//!
//! ⚠ **This used to read "asymmetric on purpose": a LOOSENING (`paper`→`demo`, anything→`live`)
//! demanded the ratified typed-confirm ceremony — retype the row's exact dotted key — while a
//! TIGHTENING was a plain click. The ceremony is DELETED, in EITHER direction.** The owner's
//! ruling: *"confirmation over confirmation … a nightmare"*, and 0086 names this exact screen as
//! the resolution: *"this also ends the conflict with
//! [0055](0055-every-setting-is-editable-from-the-ui.md), which calls arming switches ordinary
//! controls."* What guards a mistake now is [`vike_config::write_setting_row`]'s own loader-backed
//! bounds check (run twice) and the row's own `Saved`/`Failed` line an operator reads immediately
//! after clicking — never a ceremony that can be satisfied by pressing return.
//!
//! The confirm ceremony this deleted had its own state machine here — `ArmEdit::Confirming`,
//! `begin_confirm`, `can_apply` and `take_apply` — named by an earlier design spec
//! (`docs/superpowers/specs/2026-09-13-settings-store-schema-design.md`); none of the four exist in
//! this file any more, the click applies directly.
//!
//! # The LOCAL write
//!
//! The shell's only settings write before this tab was `vike_tradehub_client::set_setting` over the
//! control wire, and on the measured the CI box box `flags.tradehub_control` is OFF, so that path does
//! not exist there. This tab writes ONE `venue_arming` ROW in THIS process's own
//! `<project>/settings/db/vike.db`, through [`vike_config::write_setting_row`] — validated through
//! the loader BEFORE anything commits, inside one transaction, with a wait budget this surface
//! chooses itself ([`VENUE_ARM_LOCK_BUDGET`], which is where the argument for it lives).
//!
//! ⚠ The directory is a **parameter** ([`VenueArmingInputs::settings_dir`]), taken from the
//! binary's ONE boot walk (`vike_boot::Booted`, held in `vike-desktop`'s `SETTINGS_DIR` cell) —
//! never a resolver call from in here. The `_from`-less resolvers are `$VIKE_SETTINGS_DIR`-BLIND,
//! and this surface writing into whatever project the working directory sat above is the exact
//! defect `vike_connections::CredentialHome` was built to close for the credential store. A boot
//! that found no project yields `None` and the switch is DISABLED with that as its reason, rather
//! than guessing a path.
//!
//! # Restart-to-apply, said out loud
//!
//! Nothing in this process watches the settings database for a LOCAL write, and the mount reads the
//! ceiling once, at `make_engine`. So an accepted write renders [`ARM_RESTART_NOTE`]. That is
//! honest rather than a wart — the alternative is an operator flipping a switch, seeing nothing
//! change, and concluding the screen is broken.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use vike_config::{ArmingBlock, VenueArming, VenueMode};
use vike_connections::VenueCredStatus;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::change_journal::{Actor, Change, ChangeJournal, Outcome};
use vike_ui_theme::components::state::{self, Load};
use vike_ui_theme::components::{Status, Tokens};
use vike_ui_theme::metrics::space;
use vike_ui_theme::type_scale::TextRole;

use crate::backend::split_plane::AppMode;

/// The accepted-write banner. Nothing in this process watches the settings database for a LOCAL
/// write, and this process reads a venue's ceiling once, at mount — so every accepted write here is
/// restart-to-apply.
pub const ARM_RESTART_NOTE: &str = "saved to the settings database — restart vike-app for it to take effect (a venue's ceiling is \
     read once, at mount)";

/// **The budget this surface gives the settings database's write lock: none.**
///
/// ⚠ [`apply_arming`] is called from the DEFERRED-MUTATIONS block of [`venues_tab_content`], which
/// runs inside an egui frame — so whatever this value is, the UI thread spends it. The shared CLI
/// writer's default is ~3 s, sized (correctly) for a process that is the whole thing and has no
/// supervisor above it; spent HERE it is a frozen window with no repaint, no spinner and no cancel,
/// which an operator reads as "the app hung" rather than as "another writer holds the database".
/// A frame owes 16 ms and cannot lend one of them to another process's critical section.
///
/// So: ONE attempt and no wait at all. The contention this gives up on is not exotic — it is
/// exactly the GUI-Save-versus-daemon-control-write pair the writer's lock exists for — and giving
/// up on it costs the operator one click, because a refusal here lands in the [`ArmEdit::Failed`]
/// row the surface already renders and already invites them to start again from.
///
/// The alternative shape — retry on the NEXT frame — was refused: it needs a new `ArmEdit` state to
/// carry the pending write, and it would spin a render loop against a lock a daemon may hold for as
/// long as it likes, with nothing on screen saying so. A visible refusal the operator can act on is
/// smaller and more honest than an invisible retry.
pub const VENUE_ARM_LOCK_BUDGET: std::time::Duration = std::time::Duration::ZERO;

/// What the row says when the write found the settings database locked by another writer.
///
/// ⚠ It REPLACES the writer's own text rather than rendering it verbatim, which is the one place
/// this surface does that. That message is written for a process with a retry loop above it, and
/// there is none here. The DISPOSITION is identical and is what matters: nothing was written, and
/// trying again is the whole remedy.
pub const ARM_BUSY_NOTE: &str = "not saved: another writer holds this settings database right now (this box's \
     vike-tradehub control channel, or a `vike-cli config set`). NOTHING was written and nothing \
     is half-written — press Apply again.";

/// The refusal when the boot walk found no project, so there is nowhere to write.
pub const NO_SETTINGS_DIR: &str = "no settings directory: this process found no project above its working directory, so there is \
     no settings database to write. Start vike-app from inside the project, or set \
     VIKE_SETTINGS_DIR.";

/// The banner for [`AppMode::ObserveWithFeeds`] — a `fat` build OBSERVING a remote backend. No
/// launch has composed that mode since the `fat` build was deleted on 2026-09-09 (#1727).
pub const OBSERVING_NOTE: &str = "observing a remote backend — these ceilings govern THIS process (which mounts no venues while \
     observing). To arm the backend's own venues, use Connections → Backend settings.";

/// The banner for [`AppMode::ObserveOnly`], which links no venue mount at all — every desktop
/// launch since 2026-09-09, when the `fat` build was deleted and "thin" stopped naming one build of
/// two.
pub const THIN_BUILD_NOTE: &str = "this thin (--observe) build links no venue mount, so the Effective column cannot be computed \
     here — the ceilings below are still this project's policy.venues rows, and a fat build \
     reading the same rows will honour them.";

// -------------------------------------------------------------------------------------------
// Inputs
// -------------------------------------------------------------------------------------------

/// Everything the tab renders, resolved ONCE per frame by the binary, which holds the boot walk the
/// directory comes from — `crates/vike-desktop/src/main.rs`'s `venue_arming_inputs`. Its rows are
/// `vike_config::venue_arming::ceilings_only`; they came from `vike-mount` while the `fat` build
/// linked one, until 2026-09-09.
///
/// Owned rather than borrowed because the shell builds it inside the `Data` dispatch arm and drops
/// it after the draw; every field is small (one row per ACCOUNT — roster-length on every box with no
/// `[accounts]` table — a roster-length Vec of three bools and a name, one path).
#[derive(Debug, Clone)]
pub struct VenueArmingInputs {
    /// One row per ACCOUNT the producer can see. The desktop's producer is
    /// `vike_config::venue_arming::ceilings_only`, which enumerates the DEFAULT account only,
    /// because it can enumerate no other, and marks every row [`ArmingBlock::NoMountInThisBuild`].
    /// (`vike_mount::venue_arming`, one row per account the mount would select, was the producer in
    /// the `fat` build, deleted 2026-09-09; `vike-run`'s wrapper of it merged into `vike-mount`
    /// under decision 0098.) A box with no `[accounts]` table and no labelled
    /// credential key therefore gets exactly one row per roster venue, as it always did.
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
    /// Which of the two compositions this process is — decides the banner and whether the
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

    /// Whether this build can compute an Effective tier at all. `false` for
    /// [`AppMode::ObserveOnly`] — every desktop launch today — whose rows carry
    /// [`ArmingBlock::NoMountInThisBuild`].
    #[must_use]
    pub fn effective_is_answerable(&self) -> bool {
        self.mode != AppMode::ObserveOnly
    }

    /// The one line above the table. ⚠ It was an `Option`, `None` for the local-core mode where the
    /// table said everything — a mode no launch composed after the `fat` build was deleted
    /// (2026-09-09), and whose variant was deleted in 2026-10. Every mode left has a line.
    #[must_use]
    pub fn banner(&self) -> &'static str {
        match self.mode {
            AppMode::ObserveWithFeeds => OBSERVING_NOTE,
            AppMode::ObserveOnly => THIN_BUILD_NOTE,
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

/// **Re-read the venue ceilings from the `policy.venues` rows on disk**, so the Mode column shows
/// what the ROWS say now rather than what this process loaded at boot.
///
/// # Why a re-read at all
///
/// A policy key is `HotClass::Restart`: the running mount keeps its boot-time ceiling. If the Mode
/// column showed that boot value, an operator who flipped a switch would watch the row not change
/// and conclude the screen is broken. So the column shows the ROWS, the Effective column shows what
/// the NEXT start will therefore mount, and [`ARM_RESTART_NOTE`] is what reconciles the two.
///
/// # ⚠ Why it lives HERE and not in `vike-desktop`'s `main.rs`
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
/// The `policy.venues` rows' only source is the settings database, so that is the only thing this
/// needs to read.
///
/// `None` — no settings directory, or a load that returns `Err` — means the caller keeps the
/// boot-time ceilings. The write path faces the SAME loader and reports the breakage with its own
/// message, so a refused reload is never a silent swallow.
///
/// ⚠ **THROUGH THE SETTINGS SOURCE, and this reload was STORE-BLIND until the crossing landed.**
/// The paragraph above is still true about the ENVIRONMENT and is no longer the whole story: since
/// `docs/decisions/0057`'s crossing, a box that had run `vike-cli config adopt` (and, since
/// `docs/decisions/0086`, every box) resolves `policy.venues` from the settings DATABASE and opens
/// no `policy.toml` for resolution at all. A store-blind `vike_config::load` on such a box answers
/// `Policy::default()` — **every roster venue `paper` with `is_declared()` false — and this
/// function feeds the ARMING TAB**, so it would have painted a full unmount every frame that tab
/// was visible, on a box whose arming was correct.
///
/// It is a DISPLAY path, which is why the defect would have been survivable; it is the display path
/// for ARMING, which is why it would have been believed. `crates/vike-boot/tests/one_owner.rs`'s
/// exemption for this call argues that the DIRECTORY is a parameter here, which stays true and says
/// nothing about the LAYER SET — and the layer set is what stopped being complete.
#[must_use]
pub fn reload_venue_ceilings(settings_dir: Option<&Path>) -> Option<vike_config::VenuePolicy> {
    let dir = settings_dir?;
    let read = vike_secrets::read_settings_in(dir);
    let mut refusal = String::new();
    let source = vike_config::StoreLayer::of(Some(&read), &mut refusal);
    vike_config::load_with_source(
        Some(dir),
        source,
        &HashMap::new(),
        &vike_config::CliOverrides::default(),
    )
    .ok()
    .map(|s| s.policy.venues)
}

// -------------------------------------------------------------------------------------------
// The switch: direction, and the write
// -------------------------------------------------------------------------------------------

/// Which way a requested change moves a venue's authority.
///
/// ⚠ It used to be the ONE thing that decided whether the typed confirm was demanded (a LOOSENING
/// was). That ceremony is deleted in both directions (`docs/decisions/0086` point 7), so today the
/// only distinction that DECIDES anything is `Unchanged` versus a change; the direction is kept
/// because it is what a row reports and what its tests pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmDirection {
    /// The target equals the current ceiling: nothing to do.
    Unchanged,
    /// The target is BELOW the current ceiling — the switch reduces authority. One click.
    Tighten,
    /// The target is ABOVE the current ceiling — the switch widens authority. One click too.
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
    /// The write landed. The row renders [`ARM_RESTART_NOTE`].
    Saved {
        /// The key that was written.
        key: String,
        /// The ceiling now in the row.
        target: VenueMode,
    },
    /// The write was refused (the loader's own message, no roster to refine, a missing settings
    /// directory) — rendered verbatim, and the operator can start again from here.
    Failed {
        /// The key whose write failed.
        key: String,
        /// The refusal, verbatim.
        error: String,
    },
}

/// **THE write.** One row, loader-validated before anything commits, inside one transaction — and
/// recorded in the change journal.
///
/// ⚠ **This used to gate a LOOSENING behind a retyped-key confirm, exactly as the daemon's
/// `apply_set_setting` did — and that ceremony is DELETED for every direction**
/// (`docs/decisions/0086` point 7: *"confirmation over confirmation … a nightmare"*, and the record
/// names this exact screen: *"this also ends the conflict with
/// [0055](0055-every-setting-is-editable-from-the-ui.md), which calls arming switches ordinary
/// controls"*). Both directions are now a direct write; the renderer's buttons are a convenience on
/// top of this, and there is no ceremony for a second caller to route around.
///
/// # What reaches the ledger
///
/// The key, the row's own previous value, the new one, and `Actor::Gui`. No credential is involved
/// in a policy write at all — this is the first production caller of
/// [`vike_model::change_journal::Change::set_setting`], whose cells are exactly those. A journal
/// failure is logged and does NOT fail the write: the ceiling is already in the database, and
/// reporting the write as refused would be the worse lie.
///
/// Returns the operator-facing note on success ([`ARM_RESTART_NOTE`]) and the refusal verbatim on
/// failure.
///
/// ⚠ `subject` is a MESSAGE concern rather than a decision one: the key alone decides everything
/// (which row, which section), while the refusal an operator reads has to name the ACCOUNT rather
/// than a dotted key.
pub fn apply_arming(
    settings_dir: Option<&Path>,
    key: &str,
    subject: &str,
    target: VenueMode,
    journal: Option<&ChangeJournal>,
    now_ms: i64,
) -> Result<&'static str, String> {
    let _ = subject; // carried for message shape parity with the writer's own doc; kept as an
    // explicit parameter rather than folded away, so a future refusal message can name the account
    // without a signature change.
    let Some(dir) = settings_dir else {
        return Err(NO_SETTINGS_DIR.to_string());
    };

    // ⚠ [`VENUE_ARM_LOCK_BUDGET`] carries the whole argument for why this call is on the RENDER
    // thread and may not wait.
    match vike_config::write_setting_row(dir, key, target.as_str(), VENUE_ARM_LOCK_BUDGET) {
        Ok(report) => {
            let section = key.split('.').next().unwrap_or(key);
            record(
                journal,
                now_ms,
                Change::set_setting(
                    Outcome::AppliedPendingRestart,
                    Actor::Gui,
                    section,
                    &report.key,
                    report.old_value.as_deref(),
                    &report.new_value,
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
            // The LEDGER keeps the writer's own words (a `Busy` row should say the retry is the
            // remedy), while the ROW gets the one an operator with a mouse can act on. They are the
            // same disposition; only the audience differs.
            let error = e.to_string();
            let shown = match &e {
                vike_config::RowPlanError::Refused(vike_secrets::RowWriteError::Busy) => {
                    ARM_BUSY_NOTE.to_string()
                }
                _ => error.clone(),
            };
            // ⚠ `old: None` is "no previous value was established", NOT "the key was unset in the
            // store": this writer refused, so it never reached the point of reading one, and
            // `target` is the ceiling that did NOT land. `vike_model::change_journal::
            // SettingTarget`'s own doc is the authority for how a reader is meant to take a
            // non-applied row's cells — outcome first.
            let section = key.split('.').next().unwrap_or(key);
            record(
                journal,
                now_ms,
                Change::set_setting(
                    Outcome::Refused,
                    Actor::Gui,
                    section,
                    key,
                    None,
                    target.as_str(),
                )
                .with_reason(Some(&error)),
            );
            tracing::error!(key = %key, error = %error, "venue arming write refused");
            Err(shown)
        }
    }
}

/// Append one record, swallowing (and logging) a ledger failure. The DISK write already happened —
/// or already did not — and the ledger cannot change that verdict.
fn record(journal: Option<&ChangeJournal>, now_ms: i64, change: Change) {
    if let Some(j) = journal
        && let Err(e) = j.append(now_ms, &change)
    {
        tracing::warn!(error = %e, "the venue-arming write is on disk; its ledger record is not");
    }
}

// -------------------------------------------------------------------------------------------
// The render
// -------------------------------------------------------------------------------------------

/// The Credentials cell's text for one venue — tier NAMES with a present/absent dot, exactly the
/// vocabulary the Connections grid uses. `None` (a venue `credential_status` does not carry) reads
/// as all-absent rather than as blank, because blank would be indistinguishable from "not checked".
///
/// # ⚠ `sim` here is the CREDENTIAL KEY TOKEN and it deliberately did NOT become `paper`
///
/// Ruling 7 of `docs/superpowers/specs/2026-09-22-the-settings-store-plane-design.md` renamed the
/// ACCOUNT TIER `sim` to `paper`, and §4.4 names exactly three sites — `vike_secrets::
/// ACCOUNT_TIERS`, the store's `CHECK`, and the key-name parser. **This cell is none of them.** It
/// renders `VenueCredStatus`, whose three bools say WHICH CREDENTIAL KEY SETS ARE PRESENT, and
/// those keys are spelled `{VENUE}_SIM_*` — `vike_model::credential_keys::CREDENTIAL_TIERS`, which
/// the same ruling deliberately leaves alone because it is what an operator TYPED. Relabelling
/// this cell `paper` would put a word on screen that appears in no key the operator must write,
/// on the one surface they ACT from, so it would create the mismatch the rename exists to remove
/// rather than close it. `crates/vike-app-core/tests/venue_arming_screen.rs` asserts on this
/// rendering and changes WITH the label, which is to say: not now.
///
/// The Effective cell below is the other half and answers in the ARMING vocabulary
/// (`vike_config::VenueMode`), which `paper` has always been. The two cells were never the same
/// vocabulary and the rename does not make them one.
#[must_use]
pub fn credentials_cell(status: Option<&VenueCredStatus>) -> String {
    let (sim, demo, live) = status.map_or((false, false, false), |s| (s.sim, s.demo, s.live));
    let dot = |on: bool, name: &str| format!("{} {name}", if on { '\u{25CF}' } else { '\u{25CB}' });
    format!("{}  {}  {}", dot(sim, "sim"), dot(demo, "demo"), dot(live, "live"))
}

/// The Effective cell's text: the tier, plus the block's badge in brackets when something is being
/// refused. A mount-less build — the desktop, since 2026-09-09 — renders a dash rather than a tier
/// it cannot know.
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
        state::view(ui, Load::Loading("resolving venue arming…"));
        return;
    };
    let t = Tokens::of(ui.ctx());
    ui.label(
        egui::RichText::new(
            "What each venue is permitted to do, as the policy.venues rows read RIGHT NOW — so \
             Effective is what the NEXT start will mount, which is what the restart note \
             reconciles. The ceiling can only ever REFUSE; it never arms anything the credentials \
             and flags have not already armed.",
        )
        .font(t.font(TextRole::Body)),
    );
    ui.add_space(space::XS);
    ui.weak(inputs.banner());
    if inputs.settings_dir.is_none() {
        ui.add_space(space::XS);
        ui.colored_label(Status::Error.color(), NO_SETTINGS_DIR);
    }
    ui.add_space(space::MD);

    let answerable = inputs.effective_is_answerable();
    let writable = inputs.settings_dir.is_some();
    // ⚠ Keyed by the ROW'S dotted KEY, not by the venue: a venue can have two rows now, and they
    // write two different keys. `subject` rides along so a message can still name the account.
    // ⚠ **One click, either direction** (`docs/decisions/0086` point 7) — there is no more
    // Confirming step to defer into, so a clicked button writes immediately.
    let mut requested: Option<(String, String, VenueMode, VenueMode)> = None;

    // ⚠ `auto_shrink`'s height axis is `true` — this screen is EMBEDDED on Data Manager's
    // Credentials destination (`data.rs`'s `DataDest::Credentials` arm) beside `connections_ui`,
    // under one OUTER scroll area that owns the overflow for both. A `false` here (this grid's
    // only other caller, `tests/venue_arming_screen.rs`, asserts nothing about scroll behaviour)
    // made this ScrollArea claim the Credentials destination's entire fixed-height body — `ui`'s
    // `available_height()` inside an unbounded outer-ScrollArea content area, not the viewport —
    // leaving `connections_ui`, drawn after it with no scroll area of its own, laid out below the
    // visible region with no way to reach it. Shrinking to the grid's own content height instead
    // lets the outer scroll area carry both pieces in sequence. See `data.rs`'s `DataDest::Credentials`
    // arm for the other half of this fix.
    egui::ScrollArea::vertical().auto_shrink([false, true]).id_salt("venue_arming").show(
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

                    // The per-row second line: this row's last outcome, if any.
                    match state {
                        ArmEdit::Saved { key, .. } if *key == row_key => {
                            ui.label("");
                            ui.label("");
                            ui.label("");
                            ui.label(
                                egui::RichText::new(ARM_RESTART_NOTE).font(t.font(TextRole::Body)),
                            );
                            ui.label("");
                            ui.end_row();
                        }
                        ArmEdit::Failed { key, error } if *key == row_key => {
                            ui.label("");
                            ui.label("");
                            ui.label("");
                            ui.colored_label(
                                Status::Error.color(),
                                egui::RichText::new(error.as_str()).font(t.font(TextRole::Body)),
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

    // ── the deferred mutation, after the borrow of `inputs.rows` ends ───────────────────────
    // One click, either direction (`docs/decisions/0086` point 7) — a button is disabled for the
    // CURRENT mode already, so `Unchanged` here is a defensive no-op rather than a reachable path.
    if let Some((key, subject, current, target)) = requested {
        *state = match arm_direction(current, target) {
            ArmDirection::Unchanged => ArmEdit::Idle,
            ArmDirection::Tighten | ArmDirection::Loosen => {
                match apply_arming(
                    inputs.settings_dir.as_deref(),
                    &key,
                    &subject,
                    target,
                    journal,
                    now_ms,
                ) {
                    Ok(_) => ArmEdit::Saved { key, target },
                    Err(error) => ArmEdit::Failed { key, error },
                }
            }
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(venue: &'static str, ceiling: VenueMode) -> VenueArming {
        VenueArming {
            venue,
            label: vike_model::accounts::account_keys::AccountLabel::Default,
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

    /// **A write lands with no ceremony, in either direction** (`docs/decisions/0086` point 7) —
    /// the end-to-end proof, driven against a real planted database.
    #[test]
    fn a_write_lands_directly_with_no_confirm_ceremony() {
        let d = tempfile::tempdir().unwrap();
        vike_secrets::plant_settings_rows(
            d.path(),
            &vike_secrets::StoredSettings {
                settings: vec![],
                arming: vec![vike_secrets::ArmingRow {
                    venue: "bybit".to_string(),
                    label: None,
                    mode: "paper".to_string(),
                    max_exposure: None,
                }],
                ..Default::default()
            },
        )
        .expect("a fresh store plants");

        // A LOOSENING (paper -> live) needs no confirm parameter at all any more.
        let note =
            apply_arming(Some(d.path()), "policy.venues.bybit", "bybit", VenueMode::Live, None, 0)
                .expect("a loosening lands directly");
        assert_eq!(note, ARM_RESTART_NOTE);

        // …and a TIGHTENING right back, same call shape.
        apply_arming(Some(d.path()), "policy.venues.bybit", "bybit", VenueMode::Paper, None, 0)
            .expect("a tightening lands directly");
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
            label: vike_model::accounts::account_keys::AccountLabel::Default,
            ceiling: VenueMode::Live,
            // Decision 0095: a `live` ceiling with no LIVE-tier bybit credentials is PAPER now,
            // never demo — `LiveCredentialsAbsent` is reachable in exactly this shape.
            effective: VenueMode::Paper,
            block: ArmingBlock::LiveCredentialsAbsent,
        };
        let cell = effective_cell(&capped, true);
        assert!(cell.starts_with("paper"), "{cell}");
        assert!(cell.contains(ArmingBlock::LiveCredentialsAbsent.as_str()), "{cell}");

        // A build that cannot answer renders a dash rather than a tier it does not know.
        let thin = VenueArming { block: ArmingBlock::NoMountInThisBuild, ..clear.clone() };
        assert_eq!(effective_cell(&thin, false), "—");
        assert_eq!(effective_cell(&thin, true), "—", "the row's own block wins either way");
    }

    /// The banner says which composition this is. (The local-core case was the silent one; its
    /// variant was deleted in 2026-10.)
    #[test]
    fn each_composition_says_what_it_can_and_cannot_answer() {
        let build = |mode| VenueArmingInputs {
            rows: vec![row("bybit", VenueMode::Paper)],
            creds: Vec::new(),
            labelled_creds: Vec::new(),
            settings_dir: Some(PathBuf::from("/nowhere")),
            mode,
        };
        assert_eq!(build(AppMode::ObserveWithFeeds).banner(), OBSERVING_NOTE);
        assert!(
            build(AppMode::ObserveWithFeeds).effective_is_answerable(),
            "a fat observer still LINKS the mount, so the column is computable"
        );
        // …and it points at the surface that edits the REMOTE node, rather than implying this file
        // governs it.
        assert!(OBSERVING_NOTE.contains("Backend settings"));

        assert_eq!(build(AppMode::ObserveOnly).banner(), THIN_BUILD_NOTE);
        assert!(!build(AppMode::ObserveOnly).effective_is_answerable());
    }
}
