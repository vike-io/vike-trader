//! [`Flags`] — operator TOGGLES, each with an OWNER and a REVIEW DATE. Env overriding them is
//! *correct*.
//!
//! A flag is a thing an operator turns on for a run: enable reconciliation, mount live execution,
//! open a control surface, start a recorder. Unlike a ceiling, "settable from the environment" is
//! the right property here — that is how a systemd unit, a container and a one-off shell all
//! express intent, and the blast radius of a wrong value is a feature that is on or off, not a
//! risk limit that is wrong.
//!
//! **Every flag defaults to `false`.** That is not tidiness: each one below gates either a live
//! order path, a remote write surface, a settlement poller or extra I/O, and the safe reading of
//! an absent configuration is "do the smaller thing". Two fields need that sentence read carefully
//! and say so in their own docs — [`Flags::poly_redeem_halt`] (a kill SWITCH: `false` means "not
//! halted", and it is safe only because the poller it halts is itself off by default) and
//! [`Flags::preflight_skip`] / [`Flags::allow_withdraw_keys`] / [`Flags::reconcile_off`] /
//! [`Flags::venue_catalog_off`] (safety OVERRIDES, where `false` is the guarded state).
//!
//! ⚠ [`Flags::reconcile_off`] and [`Flags::venue_catalog_off`] are the `*_off` pair, and their
//! shape is the one worth reading before copying: each exists because a DEFAULT-ON behaviour still
//! has to be refusable, and this type's `false`-is-off invariant is machine-checked
//! (`tests/flag_registry.rs`'s `every_flag_defaults_off`). A default-on toggle cannot be a field
//! here; its REFUSAL can, and is.
//! `docs/decisions/0043-reconcile-is-on-by-default-under-quarantine.md` carries the first verdict
//! and `docs/decisions/0066-the-venue-catalog-is-on-by-default-and-the-switch-is-its-refusal.md`
//! the second — which copies this shape rather than inventing a parallel one.
//!
//! ## The owner and the review date
//!
//! The design (`docs/superpowers/specs/2026-08-04-settings-unification-design.md`, Phase 4) says
//! `VIKE_RECONCILE`/`POLY_EXEC`/`VIKE_TRADEHUB_CONTROL` are *permanent flags dressed as temporary
//! ones* — they must graduate to [`crate::Config`] or be deleted. Nothing forces that to happen
//! while a flag has no name attached to it, so **[`FLAG_REGISTRY`] carries a [`FlagMeta`] row per
//! field: an owner, an ISO review date, and the [`Disposition`] the review is expected to reach.**
//! A flag with no owner is a flag nobody will ever delete.
//!
//! `crates/vike-config/tests/flag_registry.rs` is the machine check. It fails when a field has no
//! row, when a row has no field, when an owner is blank, when a date is malformed — and, because
//! it drives every row through the real env and file layers, when a row's env variable is wired to
//! the WRONG field. So an added flag without an owner and a review date fails CI; it does not rely
//! on someone noticing in review.
//!
//! ⚠ The gate deliberately does **not** fail once a review date has passed. That would turn the
//! calendar into a merge blocker on unrelated PRs, and a settings crate must not be able to stop a
//! trading fix from shipping. Making dates bite is a one-line change in that test (`review` parsed
//! and compared against today) and a deliberate future decision, not an oversight.
//!
//! ## A typo is an error
//!
//! Read as the EXACT string `"1"`, *anything else is silently false* — `VIKE_RECONCILE=true` is
//! a run that reconciles nothing and looks completely normal.
//! This crate's flag parser accepts `"1"` and `"0"` and rejects everything else by name and
//! value. Same idiom, no fuzzy truthiness, but the operator is told.
//!
//! A set `POLY_REDEEM_HALT` — empty included, because its reader halted on PRESENCE — refuses
//! startup instead of being parsed (decision 0095, D4: nothing starts the settlement pollers, so
//! their values are parameters).
//!
//! ## Where the env map comes from is the CALLER's business — and it matters
//!
//! [`fn@crate::load`] takes the env map as a parameter; a binary passes `std::env::vars()`, the
//! workspace `.env` map, or a merge (see the crate doc's "I/O ownership"). Two facts a caller has
//! to get right:
//!
//! - **No Polymarket flag has an environment layer.** Decision 0095 made `POLY_EXEC` and
//!   `POLY_RECONCILE` MAP-ONLY: the daemon folds `flags.poly_exec` / `flags.poly_reconcile` into
//!   the credential map, a set variable refuses startup instead of being read, and
//!   [`FlagMeta::reads_env`] is `false` for both rows. `HYPERLIQUID_HIP3`
//!   and `VIKE_ALLOW_WITHDRAW_KEYS` went the same way under that decision: the daemon folds
//!   `flags.hyperliquid_hip3` / `flags.allow_withdraw_keys` into that map, a set variable refuses
//!   startup, and `reads_env` is `false` for both. The five settlement-poller flags
//!   (`poly_heartbeat`, `poly_auto_redeem`, `poly_redeem_halt`, `pm_resolve`, `hl_outcome`) lost
//!   their environment layer too, and nothing folds or reads their rows — no composition root
//!   starts the pollers ([`crate::CONSUMPTION`] says so per row). `VIKE_RECONCILE` deliberately
//!   reads the real process env and NOT the credentials map
//!   (`vike_tradehub::reconcile_config`'s module doc is the authority).
//! - **A `.env` value may carry a trailing comment.** This crate does no `first_token`
//!   normalization, so a caller handing it a RAW `.env` map would turn `VIKE_RECONCILE=1 # on` into
//!   a [`ConfigError::Env`] rather than an arm. Normalizing on the way in is the caller's job, and is
//!   the safer half of the trade — the value is rejected loudly, never read as false.
//!
//! ## What is deliberately NOT a field here
//!
//! Five things in the tree are boolean-ish and are still not [`Flags`] fields. Each is named so the
//! omission is a decision on the record rather than a gap:
//!
//! 1. **The per-venue mark-stream switch** — `venue.<venue>.mark_streams`, one `venue_setting` row
//!    per venue declared in `vike_model::venues::venue_fields` (decision 0095). It is keyed by venue, and
//!    each venue carries its own charter default (ON for the four with a documented mark wire, OFF
//!    for aster), so neither a fixed struct field nor this type's `false`-is-off invariant fits
//!    it. Its retired variables `VIKE_MARK_STREAMS` and `VIKE_MARK_STREAMS_ASTER` refuse startup.
//! 2. **The Polymarket egress settings** — `venue.polymarket.proxy_enabled` / `ws_proxy_enabled` /
//!    `socks_proxy` (their `POLY_PROXY_ENABLED` / `POLY_WS_PROXY_ENABLED` / `POLY_SOCKS_PROXY`
//!    variables refuse startup, decision 0095) — parse a FUZZY
//!    grammar (`false`/`0`/`no`/`off`, `1`/`true`/`yes`/`on`) and `proxy_enabled` defaults ON.
//!    Narrowing them to `"1"`/`"0"` would silently drop the Dublin egress proxy on a deployment
//!    spelling it `true`, and Polymarket is geo-blocked without it. They are egress DEPLOYMENT
//!    settings — venue rows (`vike_model::venues::venue_fields`), read by the composition root and
//!    declared to the bridge — not [`Flags`] fields and not operator toggles.
//! 3. **`VIKE_SWEEP_SEQUENTIAL`** is the on/off sibling of [`crate::Preferences::sweep_threads`]:
//!    backtest execution TUNING, which the taxonomy puts in preferences, not a live-path toggle.
//! 4. **The presence-based GUI/QA harness** — `VIKE_SHOT`, `VIKE_APPMAX`, `VIKE_TOOLS`, `VIKE_MAX`,
//!    `VIKE_MIN`, `VIKE_TRADE_SEED`, `VIKE_STUDIO_AUTORUN` — drives screenshot capture and dev
//!    convenience, never what a trading process does. (`VIKE_SHOT` is not even a boolean: its value
//!    is the output PNG path.)
//! 5. **Test-only self-skip gates** — `VIKE_CAPTURE_FIXTURES`, `POLY_*_SMOKE`, `PMXT_SMOKE`,
//!    `EOD_SMOKE`, `ASTER_SOAK_ALLOW_MAINNET` — exist only under `tests/` and gate `#[ignore]`d
//!    live smokes. `vike_ops::settings::Layer::TestOnly` already classifies them.
//!
//! ## Which of these are actually CONSUMED — and how to find out
//!
//! The answer is machine-recorded rather than described here: **[`crate::CONSUMPTION`]
//! carries one row per flag** naming the file and the read that consumes it, or a written admission
//! that nothing does, and `crates/vike-config/tests/settings_are_consumed.rs` opens each claimed
//! consumer and looks.
//!
//! No count is written down, because it will be wrong by the next flag that moves —
//! `vike-cli config show`'s `READ` column prints the answer per key. What is worth stating is the
//! RULE the rows follow: a flag whose read still lives in a venue adapter or a recorder stays
//! `Consumer::Not`, because moving it means threading the value from a composition root through
//! `vike_mount::NodeConfig` / `vike_mount::make_engine` into that adapter, and a flag living in two
//! places that DISAGREE would be worse than the state this program is fixing. The env read and the
//! file layer flip together, per flag.
//!
//! ⚠ **`Consumer::Not` is not one state but three.** A `Not` row carries a `vike_config::Reader`
//! saying whether the ENVIRONMENT spelling still works: for a row awaiting a thread (`Reader::Live`
//! — none is left today) it does, while for [`Flags::hl_outcome`], [`Flags::pm_resolve`],
//! [`Flags::poly_auto_redeem`], [`Flags::poly_heartbeat`], [`Flags::poly_redeem_halt`] and
//! [`Flags::record_chains`] **neither spelling configures anything** — the code they are for sits
//! inside a poller or a recorder constructor no composition root ever builds. (The first five's
//! variables are retired by decision 0095 and refuse startup; `VIKE_RECORD_CHAINS` is still looked
//! up by that unbuilt constructor.) Their fields say so individually, and [`crate::CONSUMPTION`] is
//! the gated statement; what re-arms one is a root that MOUNTS the feature, which is a decision
//! rather than a threading task.
//!
//! ⚠ `record_dvol` is not a field: [`DEAD_FLAG_KEYS`] is its tombstone and carries the argument.
//!
//! ## The rows
//!
//! ```text
//! # one `flags.<field>` settings row per field below, all optional, all default false.
//! vike-cli config set flags.reconcile true
//! vike-cli config set flags.poly_exec true
//! vike-cli config set flags.poly_reconcile true
//! vike-cli config set flags.record_properties true
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

use crate::error::ConfigError;
use crate::layers::{CliOverride, CliOverrides, EnvOverride, get, parse_flag};

mod registry;
mod retired;

pub use self::registry::{Disposition, FLAG_REGISTRY, FlagMeta, flag_meta};
pub use self::retired::{DEAD_FLAG_KEYS, REMOVED_FLAG_KEYS, dead_flag_env_ignored};
use self::retired::{dead_flag_key, removed_flag_key};

// -------------------------------------------------------------------------------------------
// Environment variable names
//
// Every literal below already carries a row in `vike_ops::settings::SETTINGS`. That gate harvests
// env-shaped string literals from the whole `crates/` tree and fails on an undeclared one, so a
// name invented here would break CI. See `crate::preferences`'
// module doc for the same constraint met from the other side.
// -------------------------------------------------------------------------------------------

/// `VIKE_RECONCILE` — the live reconciliation engine's master gate.
pub const RECONCILE_ENV: &str = "VIKE_RECONCILE";
/// `VIKE_RECONCILE_OFF` — ⚠ SAFETY OVERRIDE: refuse the default-on live reconcile.
pub const RECONCILE_OFF_ENV: &str = "VIKE_RECONCILE_OFF";

/// The warning [`fn@crate::load`] raises when a deployment WROTE a refusal that S2 no longer honours:
/// `reconcile = false` in the settings database (section `flags`), `VIKE_RECONCILE=0` in the
/// environment, or `--reconcile false` on the command line. `origin` is where it was written, in
/// the operator's own vocabulary.
///
/// It exists because the two states are indistinguishable downstream. [`Flags::reconcile`] resolves
/// to a `bool`, so `vike_tradehub::reconcile_config::reconcile_gate` cannot tell "nobody said anything"
/// from "somebody said no" — and after S2 those two now MEAN different things and get the same
/// answer. Ignoring a written refusal in silence is the failure mode this crate exists to remove:
/// a key the operator can see in their own file, doing nothing, with the daemon behaving as though
/// they had never written it.
///
/// A WARNING rather than a hard refusal, deliberately, and the direction is the argument: refusing
/// to start would take a live daemon down over a stale line in a file, while the behaviour it is
/// warning about — reconcile ON under `quarantine` — folds nothing (`docs/decisions/0013-degrade-vs-refuse.md`
/// is the standing verdict on that trade). It is returned as DATA on [`crate::Settings::warnings`]
/// for the same reason every other loader resolution is: this crate carries no `tracing`
/// dependency, and the binaries emit it once a subscriber exists.
#[must_use]
pub fn reconcile_refusal_ignored(origin: &str) -> String {
    let remedy = crate::SettingsSection::Flags.write_remedy("reconcile_off");
    format!(
        "{origin} no longer turns reconciliation OFF. Since 2026-09-06 a mount that arms at least \
         one LIVE venue account reconciles by default (under `quarantine`, so it folds nothing) — \
         `reconcile`/`VIKE_RECONCILE` is now a FORCE-ON and an explicit `false` reads the same as \
         unset. The switch you want is {}, or \
         VIKE_RECONCILE_OFF=1. See docs/ops/reconcile-on-restart.md",
        remedy.inline_write("true")
    )
}
/// `VIKE_RECONCILE_GENERATE_MISSING` — adopt a venue order with no local match.
pub const RECONCILE_GENERATE_MISSING_ENV: &str = "VIKE_RECONCILE_GENERATE_MISSING";
/// `VIKE_RECONCILE_BALANCE` — diff venue BALANCE as a first-class reconcile dimension.
pub const RECONCILE_BALANCE_ENV: &str = "VIKE_RECONCILE_BALANCE";
/// `VIKE_OCO_CANCEL_SIBLING_ON_DEAD_EXIT` — cancel the surviving OCO sibling on a dead exit leg.
pub const OCO_CANCEL_SIBLING_ON_DEAD_EXIT_ENV: &str = "VIKE_OCO_CANCEL_SIBLING_ON_DEAD_EXIT";

/// `VIKE_TRADEHUB_LIVE` — the headless daemon's REAL-MONEY master gate.
pub const TRADEHUB_LIVE_ENV: &str = "VIKE_TRADEHUB_LIVE";
/// `VIKE_TRADEHUB_CONTROL` — open the headless daemon's authenticated CONTROL scope.
pub const TRADEHUB_CONTROL_ENV: &str = "VIKE_TRADEHUB_CONTROL";
/// `VIKE_TRADEHUB_ALLOW_PUBLIC_BIND` — let the node server bind a NON-LOOPBACK address.
pub const TRADEHUB_ALLOW_PUBLIC_BIND_ENV: &str = "VIKE_TRADEHUB_ALLOW_PUBLIC_BIND";
/// `VIKE_TELEGRAM_CONTROL` — enable the Telegram control channel (on top of the Cargo feature).
pub const TELEGRAM_CONTROL_ENV: &str = "VIKE_TELEGRAM_CONTROL";
/// `VIKE_CANCEL_ORDERS_ON_SHUTDOWN` — cancel every resting order during the daemon's teardown.
pub const CANCEL_ORDERS_ON_SHUTDOWN_ENV: &str = "VIKE_CANCEL_ORDERS_ON_SHUTDOWN";

/// the name the daemon folds `flags.poly_exec` into the credential map under; no environment
/// variable of this name is read (decision 0095) — a set one refuses startup.
pub const POLY_EXEC_ENV: &str = "POLY_EXEC";
/// the name the daemon folds `flags.poly_reconcile` into the credential map under; no environment
/// variable of this name is read (decision 0095) — a set one refuses startup.
pub const POLY_RECONCILE_ENV: &str = "POLY_RECONCILE";
/// The retired variable of `flags.poly_heartbeat` (the dead-man heartbeat poller); no environment
/// variable of this name is read (decision 0095) — a set one refuses startup.
pub const POLY_HEARTBEAT_ENV: &str = "POLY_HEARTBEAT";
/// The retired variable of `flags.poly_auto_redeem` (the CTF auto-redeem poller); no environment
/// variable of this name is read (decision 0095) — a set one refuses startup.
pub const POLY_AUTO_REDEEM_ENV: &str = "POLY_AUTO_REDEEM";
/// The retired variable of `flags.poly_redeem_halt` (the auto-redeem KILL SWITCH, which halted on
/// PRESENCE); no environment variable of this name is read (decision 0095) — a set one refuses
/// startup, an EMPTY one included.
pub const POLY_REDEEM_HALT_ENV: &str = "POLY_REDEEM_HALT";

/// The retired variable of `flags.pm_resolve` (the Polymarket market-resolution poller); no
/// environment variable of this name is read (decision 0095) — a set one refuses startup.
pub const PM_RESOLVE_ENV: &str = "VIKE_PM_RESOLVE";
/// The retired variable of `flags.hl_outcome` (the Hyperliquid outcome-token settlement lane); no
/// environment variable of this name is read (decision 0095) — a set one refuses startup.
pub const HL_OUTCOME_ENV: &str = "VIKE_HL_OUTCOME";

/// The name the daemon folds `flags.hyperliquid_hip3` into the credential map under (the
/// hyperliquid mount reads it there); no environment variable of this name is read (decision
/// 0095) — a set one refuses startup.
pub const HYPERLIQUID_HIP3_ENV: &str = "HYPERLIQUID_HIP3";

/// `VIKE_RECORD_PROPERTIES` — record each venue's observed `SymbolProperties` grid over time.
pub const RECORD_PROPERTIES_ENV: &str = "VIKE_RECORD_PROPERTIES";
/// `VIKE_RECORD_CHAINS` — record option-chain snapshots.
pub const RECORD_CHAINS_ENV: &str = "VIKE_RECORD_CHAINS";
/// `VIKE_RECORD_DVOL` — record Deribit's DVOL index.
pub const RECORD_DVOL_ENV: &str = "VIKE_RECORD_DVOL";

/// `VIKE_DATAHUB_VENUE_CATALOG_OFF` — ⚠ SAFETY OVERRIDE: refuse the default-on venue-catalog lane.
///
/// It names the daemon that owns the lane the way [`VENUE_CATALOG_ENV`] did, so the two grep
/// together and an operator who finds one finds the other.
pub const VENUE_CATALOG_OFF_ENV: &str = "VIKE_DATAHUB_VENUE_CATALOG_OFF";

/// `VIKE_DATAHUB_VENUE_CATALOG` — the venue-catalog lane's FORMER arming, kept only so
/// [`venue_catalog_refusal_ignored`] can look it up and warn.
///
/// ⚠ It configures NOTHING after `docs/decisions/0066`. Its grammar was *the exact string `"1"` is
/// on, everything else is off*, so the two halves of that grammar break in OPPOSITE directions on
/// the flip and only one of them needs telling — see [`venue_catalog_refusal_ignored`].
pub const VENUE_CATALOG_ENV: &str = "VIKE_DATAHUB_VENUE_CATALOG";

/// The warning [`fn@crate::load`] raises when a deployment exported the OLD venue-catalog variable
/// with a value that used to mean OFF. `origin` is where they wrote it, in their own vocabulary.
///
/// ⚠ **The asymmetry is the whole of this function, and it is the INVERSE of
/// [`reconcile_refusal_ignored`]'s.** That one warns on a written `false` of a key that still
/// exists. Here the key is gone and its grammar was *exact `"1"` = on*, so:
///
/// | the operator wrote | meant before | means after | warned |
/// |---|---|---|---|
/// | `=1` | catalog on | catalog on | **no** — the belief the value expresses is still true |
/// | any other non-empty value, `0` included | catalog OFF | catalog ON | **yes** |
/// | set but empty | off | on | **no** — `crate::layers::get` reads a blank as unset, which is the rule `RemovedSetting` already follows |
///
/// Getting that backwards would put a warning in front of every correctly-configured legacy box
/// while saying nothing to the only operator who is wrong.
///
/// ⚠ A residual, declared rather than argued away: an operator who keeps `=1` also believes that
/// UNSETTING it would turn the catalog off, and that belief IS false. It is the identical residual
/// the reconcile flip accepted, and the surface that teaches the new switch is `vike-cli config
/// show`, which names the key, its origin and its reader.
///
/// A WARNING rather than a startup refusal ([`crate::REMOVED_ENV`]), for the three reasons
/// `docs/decisions/0066`'s decision 4 gives: the meaning MOVED rather than the value being deleted,
/// a catalog listing is a CAPABILITY (`docs/decisions/0013-degrade-vs-refuse.md`), and
/// `refuse_removed_env` is not called by the one root that owns the lane. Returned as DATA on
/// [`crate::Settings::warnings`] because this crate carries no `tracing` dependency.
#[must_use]
pub fn venue_catalog_refusal_ignored(origin: &str) -> String {
    let remedy = crate::SettingsSection::Flags.write_remedy("venue_catalog_off");
    format!(
        "{origin} no longer turns the venue catalog OFF — it turns nothing on or off at all. \
         Since 2026-09-16 a datahub serves the venue-catalog verb by DEFAULT, because without an \
         instrument list you cannot pick a symbol. The switch you want is {}, or \
         {VENUE_CATALOG_OFF_ENV}=1. See \
         docs/decisions/0066-the-venue-catalog-is-on-by-default-and-the-switch-is-its-refusal.md",
        remedy.inline_write("true")
    )
}

/// `VIKE_DATAHUB_LIVE` — arm the data daemon's live market-data plane.
pub const DATAHUB_LIVE_ENV: &str = "VIKE_DATAHUB_LIVE";
/// `VIKE_DATAHUB_CHART_SEED` — arm the data daemon's chart-gap seed lane.
pub const DATAHUB_CHART_SEED_ENV: &str = "VIKE_DATAHUB_CHART_SEED";
/// `VIKE_DATAHUB_ALLOW_PUBLIC_BIND` — let the data and compute daemons bind a NON-LOOPBACK address.
pub const DATAHUB_ALLOW_PUBLIC_BIND_ENV: &str = "VIKE_DATAHUB_ALLOW_PUBLIC_BIND";

/// The name the daemon folds `flags.allow_withdraw_keys` into the credential map under (the binance
/// withdraw gate reads it there); no environment variable of this name is read (decision 0095) — a
/// set one refuses startup.
pub const ALLOW_WITHDRAW_KEYS_ENV: &str = "VIKE_ALLOW_WITHDRAW_KEYS";
/// `VIKE_PREFLIGHT_SKIP` — skip the startup preflight entirely.
pub const PREFLIGHT_SKIP_ENV: &str = "VIKE_PREFLIGHT_SKIP";

/// Operator toggles. Every field defaults to `false` — see the module doc.
///
/// Each field's doc names what it enables, the variable it mirrors, and its owner + review date.
/// [`FLAG_REGISTRY`] is the MACHINE-READABLE twin of those last two and the authority when they
/// disagree; `tests/flag_registry.rs` keeps the set of fields and the set of rows identical.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct Flags {
    // --- reconciliation ---------------------------------------------------------------------
    /// Run the live reconciliation engine (diff venue reports against local state, resolve per
    /// policy). The rest of the `VIKE_RECONCILE_*` family is CONFIG, not flags — cadences,
    /// lookbacks and the policy name — and lives in [`crate::Config`]'s `reconcile_*` rows since
    /// decision 0111's phase P3 (each still beaten by its variable).
    ///
    /// ⚠ **This is not the whole gate, and it is not the DEFAULT answer.** A
    /// mount that arms at least one LIVE venue account reconciles whether or not this is set —
    /// `vike_tradehub::reconcile_config::reconcile_gate` is the one decision both live roots make, and
    /// `docs/decisions/0043-reconcile-is-on-by-default-under-quarantine.md` is the verdict. What
    /// this field still MEANS is "on even where that default would not turn it on": the default
    /// keys off `vike_mount::armed_live_venues`, a hand-written per-venue probe, and a venue arm
    /// merged without its row would report a live mount as paper. [`Flags::reconcile_off`] is the
    /// refusal.
    ///
    /// ⚠ **An explicitly-written `false` is IGNORED, and the loader says so out loud.** Because the
    /// resolved value is a `bool`, a `flags.reconcile` row set to `false` (or
    /// `VIKE_RECONCILE=0` in a unit file) is indistinguishable from unset at
    /// `reconcile_gate` — so a deployment that wrote it meaning "do not reconcile" now reconciles.
    /// [`reconcile_refusal_ignored`] is the one-line warning `crate::load` raises for that,
    /// naming the origin and pointing at [`Flags::reconcile_off`]; the precedent is
    /// `vike_mount::venue_arming_migration`, which exists because a default change that silently
    /// overrides a written setting hands the operator positive confirmation of something false.
    ///
    /// Was `VIKE_RECONCILE`. Owner `@AlexSokhanych` · review 2026-11-01 · GRADUATE.
    pub reconcile: bool,

    /// Adopt a venue order with no local match (`DivergenceKind::UnknownOrder`) by synthesizing a
    /// decorative accept plus one cumulative fill, instead of leaving it un-resolvable.
    ///
    /// Was `VIKE_RECONCILE_GENERATE_MISSING`. Owner `@AlexSokhanych` · review 2027-02-01 ·
    /// GRADUATE.
    pub reconcile_generate_missing: bool,

    /// Promote venue BALANCE to a first-class diffed dimension of each reconcile pass.
    ///
    /// Was `VIKE_RECONCILE_BALANCE`. Owner `@AlexSokhanych` · review 2027-02-01 · GRADUATE.
    pub reconcile_balance: bool,

    /// Cancel the surviving OCO sibling when a bracket exit leg dies unfilled, rather than
    /// leaving it resting.
    ///
    /// Was `VIKE_OCO_CANCEL_SIBLING_ON_DEAD_EXIT`. Owner `@AlexSokhanych` · review 2026-11-01 ·
    /// RETIRE — a bracket that half-survives its own exit is a bug, not a preference.
    pub oco_cancel_sibling_on_dead_exit: bool,

    // --- the headless daemon ----------------------------------------------------------------
    /// Run the headless daemon against REAL venues with real credentials, instead of the paper
    /// exchange. ⚠ The single largest blast radius on this list.
    ///
    /// Was `VIKE_TRADEHUB_LIVE`. Owner `@AlexSokhanych` · review 2026-11-01 · KEEP — a real-money
    /// arm must stay a deliberate per-run decision, never a file that drifts into a deployment.
    pub tradehub_live: bool,

    /// Open the headless daemon's authenticated CONTROL scope — a remote order-origination path.
    ///
    /// Was `VIKE_TRADEHUB_CONTROL`. Owner `@AlexSokhanych` · review 2026-11-01 · GRADUATE.
    pub tradehub_control: bool,

    /// Let the node server bind a NON-LOOPBACK address. ⚠ A safety OVERRIDE, like
    /// [`Flags::allow_withdraw_keys`]: `false` is the GUARDED state, and the daemon REFUSES such a
    /// bind (logging the SSH-tunnel alternative) rather than opening the surface.
    ///
    /// It exists because that surface's handshake is PLAINTEXT and authenticates the CONNECTION,
    /// not each frame, so its confidentiality and integrity come entirely from being unreachable —
    /// an SSH tunnel, exactly as `vike-datahub` is reached. Nothing enforced that: the address is
    /// only checked for a `:`, and `0.0.0.0:7879` is simply what one types for a server.
    ///
    /// A separate flag rather than an inference from the address, because the mistake it catches
    /// IS typing an address — no address can be its own consent. It is not a ceiling, so the
    /// environment layer is right here: setting it cannot enlarge an order, only widen
    /// reachability, and the daemon then `warn!`s about that on every start.
    ///
    /// Owner `@AlexSokhanych` · review 2026-11-01 · KEEP — reachability belongs to the deployment
    /// and stays a deliberate per-run opt-in.
    pub tradehub_allow_public_bind: bool,

    /// Enable the Telegram control channel. ⚠ Sits UNDER a Cargo feature as well — a default build
    /// does not compile the module at all — and under [`Flags::tradehub_control`]: the live gate
    /// is the AND of both variables, which this type deliberately does not encode. Composition is
    /// the consumer's call; this field only says what the operator asked for.
    ///
    /// Was `VIKE_TELEGRAM_CONTROL`. Owner `@AlexSokhanych` · review 2026-11-01 · KEEP.
    pub telegram_control: bool,

    /// Cancel every resting order during the daemon's shutdown teardown, instead of leaving the
    /// book live at the venue.
    ///
    /// OFF (the default) is what has always happened, and stays byte-identical: teardown detaches
    /// and exits, and **your quotes stay resting at the venue with nothing left running to manage
    /// them**. That really is what an operator restarting for a deploy usually wants, which is why
    /// the safer-SOUNDING value is not the default — flipping it would silently change the stop
    /// behavior of every deployment that already exists.
    ///
    /// ⚠ It cancels; it does not FLATTEN. Positions survive either way. And it can only run on a
    /// path that reaches teardown at all — under `deploy/vike-tradehub.service` SIGTERM runs no
    /// Rust code, so `systemctl stop` bypasses this flag entirely. `docs/ops/kill-switches.md`
    /// carries the operator-facing statement of both limits.
    ///
    /// Was `VIKE_CANCEL_ORDERS_ON_SHUTDOWN`. Owner `@AlexSokhanych` · review 2026-11-01 · KEEP —
    /// "what happens to my book when I stop" is a standing operator preference, not a migration.
    pub cancel_orders_on_shutdown: bool,

    // --- Polymarket -------------------------------------------------------------------------
    /// Mount Polymarket's live `ExecutionClient`. ⚠ This also starts the user-WS fill pump —
    /// on that venue every post-acceptance terminal arrives only on the user channel, so the two
    /// are deliberately inseparable.
    ///
    /// Was `POLY_EXEC` — retired by decision 0095; the row is the only source. Owner
    /// `@AlexSokhanych` · review 2026-11-01 · GRADUATE.
    pub poly_exec: bool,

    /// Mount Polymarket's `ReconClient`. ⚠ Meaningful only alongside [`Flags::poly_exec`]:
    /// reconciling a PAPER engine against live venue state makes the two sides different accounts,
    /// and under the `hybrid` policy `PositionDrift` auto-applies — folding the LIVE account's
    /// position into the PAPER engine's books at the venue's avg price. Run recon-without-exec
    /// under `VIKE_RECONCILE_POLICY=quarantine`.
    ///
    /// Was `POLY_RECONCILE` — retired by decision 0095; the row is the only source. Owner
    /// `@AlexSokhanych` · review 2026-11-01 · GRADUATE.
    pub poly_reconcile: bool,

    /// Run the venue's dead-man heartbeat poller. Polymarket cancels ALL open orders if no valid
    /// beat lands within ~10 s, so a live maker that does not beat is a maker that gets flattened.
    ///
    /// ⚠ **UNMOUNTED: setting this changes nothing.** The poller is PARKED
    /// (`crates/bridges/polymarket/src/lib.rs` says so), no composition root spawns it, and the value
    /// it would take is `HeartbeatPoller::spawn`'s `enabled` parameter. [`crate::CONSUMPTION`]'s row
    /// is the gated authority for the claim.
    ///
    /// Was `POLY_HEARTBEAT` — retired by decision 0095; a set one refuses startup. Owner
    /// `@AlexSokhanych` · review 2026-11-01 · RETIRE — a dead-man switch that a live maker cannot
    /// run without is not an option; the CADENCE is the config.
    pub poly_heartbeat: bool,

    /// Run the CTF auto-redeem poller: redeem resolved winning binary AND neg-risk positions
    /// through the gasless relayer.
    ///
    /// ⚠ **UNMOUNTED: setting this changes nothing.** Nothing outside tests constructs
    /// `AutoRedeemPoller`, and that is a DECISION rather than a gap — spawning it is new unattended
    /// on-chain money movement (`crates/vike-ops/tests/docs/kill_switch_gate.rs`'s RETIRED row). The
    /// value it would take is `AutoRedeemPoller::spawn`'s `enabled` parameter.
    /// [`crate::CONSUMPTION`]'s row is the gated authority.
    ///
    /// Was `POLY_AUTO_REDEEM` — retired by decision 0095; a set one refuses startup. Owner
    /// `@AlexSokhanych` · review 2026-11-01 · KEEP — it MOVES REAL MONEY on-chain and stays an
    /// explicit opt-in.
    pub poly_auto_redeem: bool,

    /// ⚠ KILL SWITCH, and the ONE inverted field here: `true` HALTS the auto-redeem poller's
    /// ticks. `false` therefore means "not halted" — safe only because the thing it halts,
    /// [`Flags::poly_auto_redeem`], is itself off by default.
    ///
    /// ⚠ Decision 0095 retired the variable; a set one — EMPTY included, because it halted on
    /// PRESENCE — refuses startup, and the row is the only spelling left.
    ///
    /// ⚠ **UNMOUNTED, in the sense that there is nothing running to halt**: the poller this
    /// switches off is never spawned, so the row does nothing today. That is a reason to KEEP the
    /// field, not to delete it — deleting a guard while its subject survives leaves the guard
    /// behind the thing it guards. The value it would take is `AutoRedeemPoller::spawn`'s `halted`
    /// parameter, beside the switch's second trip condition, a halt FILE on disk, that this type
    /// does not model. [`crate::CONSUMPTION`]'s row carries the argument.
    ///
    /// Was `POLY_REDEEM_HALT`. Owner `@AlexSokhanych` · review 2027-02-01 · KEEP.
    pub poly_redeem_halt: bool,

    // --- settlement pollers -----------------------------------------------------------------
    /// Run the Polymarket market-resolution poller, which writes resolved payouts into the core.
    ///
    /// ⚠ **UNMOUNTED: setting this changes nothing.** No composition root constructs
    /// `ResolvePoller`; `crates/bridges/polymarket/src/exec_plane/settlement/mod.rs`'s module doc
    /// states it for the whole cluster. The value it would take is `ResolvePoller::spawn`'s
    /// `enabled` parameter. [`crate::CONSUMPTION`]'s row is the gated authority.
    ///
    /// Was `VIKE_PM_RESOLVE` — retired by decision 0095; a set one refuses startup. Owner
    /// `@AlexSokhanych` · review 2027-02-01 · KEEP.
    pub pm_resolve: bool,

    /// Run the Hyperliquid outcome-token (prediction market) settlement lane.
    ///
    /// ⚠ **UNMOUNTED: setting this changes nothing.** Nothing outside tests constructs
    /// `OutcomePoller`; the value it would take is `OutcomePoller::spawn`'s `enabled` parameter.
    /// [`crate::CONSUMPTION`]'s row is the gated authority.
    ///
    /// Was `VIKE_HL_OUTCOME` — retired by decision 0095; a set one refuses startup. Owner
    /// `@AlexSokhanych` · review 2027-02-01 · KEEP.
    pub hl_outcome: bool,

    // --- venue execution / feed opt-ins -----------------------------------------------------
    /// Enumerate Hyperliquid HIP-3 builder-dex markets into symbology. The datahub's venue catalog
    /// reads this row too, so the picker's universe and the mount's symbology agree.
    ///
    /// Was `HYPERLIQUID_HIP3` — its variable is retired (decision 0095); the row is the only
    /// source. Owner `@AlexSokhanych` · review 2027-02-01 · GRADUATE — which market universe a
    /// venue exposes is deployment config, not a run-time toggle.
    pub hyperliquid_hip3: bool,

    // --- recorders --------------------------------------------------------------------------
    /// Record each venue's observed `SymbolProperties` grid into the store's point-in-time
    /// `kind=properties` series at instrument-fetch time.
    ///
    /// Was `VIKE_RECORD_PROPERTIES`. Owner `@AlexSokhanych` · review 2027-02-01 · GRADUATE.
    pub record_properties: bool,

    /// Record option-chain snapshots into the store.
    ///
    /// ⚠ **UNMOUNTED: setting this changes nothing, and neither does `VIKE_RECORD_CHAINS`.** Both
    /// of `vike_data::ChainRecorder`'s impure constructors (`from_env`, `open_from_env`) are dead
    /// doors with no call site in the tree: `crates/vike-desktop/src/main.rs` binds its recorder to
    /// `None`. ⚠ **The mechanism is the STORE, not the fetch.** The desktop still calls
    /// `vike_app_core::tools::spawn_tool_fetchers` and still polls chains every 30 s, but it links
    /// vike-data with DEFAULT features (the trait-only `HistStore` seam) and `open_from_env` is a
    /// `hist-datafusion` constructor, so there is no engine to open. Re-arming
    /// this is a root that can open a STORE beside the fetch it already has. The recorder and its
    /// `kind=chain` series are intact. [`crate::CONSUMPTION`]'s row is the gated authority.
    ///
    /// Was `VIKE_RECORD_CHAINS`. Owner `@AlexSokhanych` · review 2027-02-01 · GRADUATE — it has a
    /// `VIKE_RECORD_CHAINS_CADENCE_MS` sibling that is already config; the pair should move
    /// together.
    pub record_chains: bool,

    // ⚠ `record_dvol` is DELETED — see [`DEAD_FLAG_KEYS`] for the tombstone and the argument.

    // --- safety overrides -------------------------------------------------------------------
    /// ⚠ SAFETY OVERRIDE. Permit arming a live venue whose API key is WITHDRAW-CAPABLE, which the
    /// key-permission check otherwise refuses. `false` is the guarded state.
    ///
    /// Was `VIKE_ALLOW_WITHDRAW_KEYS` — its variable is retired (decision 0095); the row is the
    /// only source. Owner `@AlexSokhanych` · review 2027-02-01 · KEEP — an escape hatch for a real
    /// key that cannot be re-scoped, and it must stay explicit and loud.
    pub allow_withdraw_keys: bool,

    /// ⚠ SAFETY OVERRIDE. Skip the startup preflight entirely (an empty, skipped report and no
    /// probes). `false` is the guarded state.
    ///
    /// Was `VIKE_PREFLIGHT_SKIP`. Owner `@AlexSokhanych` · review 2027-02-01 · KEEP — the hatch
    /// for a venue whose probe is down while trading must continue.
    pub preflight_skip: bool,

    /// ⚠ SAFETY OVERRIDE, and the one whose guarded state is the LOUD side. Refuse the S2
    /// default: a mount that arms a live venue account will NOT fetch that venue's own orders and
    /// positions, at startup or ever. `false` (the default) is the guarded state — the daemon asks
    /// the venue and HOLDS every divergence for you; `true` is the operator saying "do not talk to
    /// my accounts", and a mount that takes it can orphan resting orders across a restart with
    /// nothing in the log to say so.
    ///
    /// It exists because a default-on behaviour still has to be refusable and this type's fields
    /// cannot default to `true` (see the module doc). The three legitimate reasons to set it: a
    /// venue whose read endpoints are rate-limited into uselessness, an account under maintenance,
    /// and a demo run on a box whose credential store holds keys it must not touch — though for
    /// that last one the arming ceiling (`policy.venues.<venue> = "paper"`) is the better answer,
    /// because it also refuses the exec session.
    ///
    /// Beats [`Flags::reconcile`] when both are set (`vike_tradehub::reconcile_config::reconcile_gate`
    /// checks the refusal first): a stale `flags.reconcile` row must not overrule the person typing
    /// the override.
    ///
    /// Was never an environment variable before S2. Owner `@AlexSokhanych` · review 2027-02-01 ·
    /// KEEP — a refusal for a default-on safety behaviour is a standing operator hatch, not a
    /// migration.
    pub reconcile_off: bool,

    /// ⚠ SAFETY OVERRIDE, and the SECOND whose guarded state is the loud side. Refuse the
    /// venue-catalog lane on a `vike-datahub`: the `VenueCatalog` verb is still ANSWERED, having
    /// called no venue, and the `venue_catalog` capability is absent from the handshake. `false`
    /// (the default) is the served state.
    ///
    /// It exists because `docs/decisions/0066` made that lane DEFAULT-ON — without an instrument
    /// list you cannot pick a symbol, so a switch that is off by default means the product does
    /// not work out of the box — and this type's fields cannot default to `true` (see the module
    /// doc). The refusal is what carries the operator who wants no catalog at all, which is the
    /// one thing the old arming variable bought.
    ///
    /// ⚠ What it does NOT do is bound the COST. The per-venue token buckets, the memo TTL and the
    /// bridges' own compiled pagers are what protect the order-signing daemon's shared venue
    /// budget, and none of them moves when this flag moves —
    /// `crates/vike-datahub/src/catalog.rs`'s `CATALOG_VENUE_REFILL` is the site. Setting this
    /// makes the feature ABSENT; it does not make a fetch cheaper.
    ///
    /// The legitimate reasons to set it: a box whose venue-API budget is reserved for something
    /// else, a datahub deliberately serving stored history only, and an operator who does not want
    /// an Observe-scope client able to make this box call a venue at all.
    ///
    /// Was `VIKE_DATAHUB_VENUE_CATALOG=1` inverted (see [`venue_catalog_refusal_ignored`]). Owner
    /// `@AlexSokhanych` · review 2027-02-01 · KEEP — like [`Flags::reconcile_off`], a refusal for a
    /// default-on behaviour is a standing operator hatch and should live exactly as long as the
    /// default does.
    pub venue_catalog_off: bool,

    // --- the data and compute daemons -------------------------------------------------------
    // ⚠ These three are rows since `docs/decisions/0111`'s phase P3, and their readers still read
    // the variable FIRST: `vike-datahub` (and, for the bind, `backtest --addr`) consult the row only
    // where the variable is ABSENT, so a unit's `Environment=` line — including the deploy probe's
    // `=0` — still wins. The arm in `apply_env` below is this type's own contract (every flag with
    // a live variable has one; `tests/flag_registry.rs` holds it), and it agrees with the reader for
    // `1` and `0`.
    /// Arm `vike-datahub`'s live market-data plane: this process becomes the single subscriber to
    /// each venue it serves, spending that venue's API budget from this box's IP. Off (the default)
    /// is byte-identical to a build without the plane.
    ///
    /// Was `VIKE_DATAHUB_LIVE`, which still beats the row. Owner `@AlexSokhanych` · review
    /// 2027-02-01 · KEEP — which planes a data server runs is a per-box decision.
    pub datahub_live: bool,

    /// Arm `vike-datahub`'s chart-gap seed lane: an Observe-scope client may have the server fetch
    /// one bounded kline window per series into its store. Needs a build that mounts a collector
    /// table; armed on one that does not, it warns once and arms nothing.
    /// `docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md` puts defaulting it ON in its
    /// reopen list.
    ///
    /// Was `VIKE_DATAHUB_CHART_SEED`, which still beats the row. Owner `@AlexSokhanych` · review
    /// 2027-02-01 · KEEP.
    pub datahub_chart_seed: bool,

    /// ⚠ SAFETY OVERRIDE. Let `vike-datahub` and the compute daemon (`backtest --addr`) bind a
    /// NON-LOOPBACK address. `false` is the guarded state, and even `true` is refused on a server
    /// with no node keys: consent to being REACHABLE is not consent to serving unauthenticated.
    /// One flag for both daemons on purpose — it answers one posture question for one box.
    ///
    /// Was `VIKE_DATAHUB_ALLOW_PUBLIC_BIND`, which still beats the row. Owner `@AlexSokhanych` ·
    /// review 2027-02-01 · KEEP — like [`Flags::tradehub_allow_public_bind`], an address cannot be
    /// its own consent.
    pub datahub_allow_public_bind: bool,
}

/// The ROW shape of [`Flags`] — all-optional, unknown keys rejected by name.
///
/// One `Option<bool>` per [`Flags`] field, same key spelling, so a `flags.<field>` row key IS the
/// field name. `tests/flag_registry.rs` proves that per row rather than trusting the eye.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlagsPatch {
    /// See [`Flags::reconcile`].
    pub reconcile: Option<bool>,
    /// See [`Flags::reconcile_generate_missing`].
    pub reconcile_generate_missing: Option<bool>,
    /// See [`Flags::reconcile_balance`].
    pub reconcile_balance: Option<bool>,
    /// See [`Flags::oco_cancel_sibling_on_dead_exit`].
    pub oco_cancel_sibling_on_dead_exit: Option<bool>,
    /// See [`Flags::tradehub_live`].
    pub tradehub_live: Option<bool>,
    /// See [`Flags::tradehub_control`].
    pub tradehub_control: Option<bool>,
    /// See [`Flags::tradehub_allow_public_bind`].
    pub tradehub_allow_public_bind: Option<bool>,
    /// See [`Flags::telegram_control`].
    pub telegram_control: Option<bool>,
    /// See [`Flags::cancel_orders_on_shutdown`].
    pub cancel_orders_on_shutdown: Option<bool>,
    /// See [`Flags::poly_exec`].
    pub poly_exec: Option<bool>,
    /// See [`Flags::poly_reconcile`].
    pub poly_reconcile: Option<bool>,
    /// **TOMBSTONE** — see [`REMOVED_FLAG_KEYS`]. Not a [`Flags`] field.
    pub poly_presubmit_register: Option<bool>,
    /// See [`Flags::poly_heartbeat`].
    pub poly_heartbeat: Option<bool>,
    /// **TOMBSTONE** — see [`REMOVED_FLAG_KEYS`]. Not a [`Flags`] field.
    pub poly_rate_gate: Option<bool>,
    /// **TOMBSTONE** — see [`REMOVED_FLAG_KEYS`]. Not a [`Flags`] field.
    pub poly_chain_watch: Option<bool>,
    /// **TOMBSTONE** — see [`REMOVED_FLAG_KEYS`]. Not a [`Flags`] field.
    pub poly_chain_proxy: Option<bool>,
    /// See [`Flags::poly_auto_redeem`].
    pub poly_auto_redeem: Option<bool>,
    /// See [`Flags::poly_redeem_halt`].
    pub poly_redeem_halt: Option<bool>,
    /// See [`Flags::pm_resolve`].
    pub pm_resolve: Option<bool>,
    /// See [`Flags::hl_outcome`].
    pub hl_outcome: Option<bool>,
    /// **TOMBSTONE** — see [`REMOVED_FLAG_KEYS`]. Not a [`Flags`] field.
    pub bybit_fast_exec: Option<bool>,
    /// **TOMBSTONE** — see [`REMOVED_FLAG_KEYS`]. Not a [`Flags`] field.
    pub binance_trade_lite_fill: Option<bool>,
    /// See [`Flags::hyperliquid_hip3`].
    pub hyperliquid_hip3: Option<bool>,
    /// See [`Flags::record_properties`].
    pub record_properties: Option<bool>,
    /// See [`Flags::record_chains`].
    pub record_chains: Option<bool>,
    /// **TOMBSTONE** — see [`DEAD_FLAG_KEYS`]. Not a [`Flags`] field.
    pub record_dvol: Option<bool>,
    /// See [`Flags::allow_withdraw_keys`].
    pub allow_withdraw_keys: Option<bool>,
    /// See [`Flags::preflight_skip`].
    pub preflight_skip: Option<bool>,
    /// See [`Flags::reconcile_off`].
    pub reconcile_off: Option<bool>,
    /// See [`Flags::venue_catalog_off`].
    pub venue_catalog_off: Option<bool>,
    /// See [`Flags::datahub_live`].
    pub datahub_live: Option<bool>,
    /// See [`Flags::datahub_chart_seed`].
    pub datahub_chart_seed: Option<bool>,
    /// See [`Flags::datahub_allow_public_bind`].
    pub datahub_allow_public_bind: Option<bool>,
}

impl Flags {
    /// Fold one file's patch in. Nothing to validate — TOML already types a boolean, and
    /// `deny_unknown_fields` on [`FlagsPatch`] catches the misspelled-key case that actually
    /// bites (a flag the operator believes they set).
    ///
    /// ⚠ It CAN fail, and the ways it can are TOMBSTONES, of TWO kinds. Keys deleted from this
    /// type while the variables they mirror were still LIVE are refused and told where the setting
    /// went ([`REMOVED_FLAG_KEYS`] — decision 0095 has since retired every one of those variables,
    /// so the refusal names a row, or says nothing starts the code, rather than a variable to
    /// export); keys deleted with NO live variable behind them are refused and told that neither
    /// spelling ever did anything ([`DEAD_FLAG_KEYS`]). The two messages must not be swapped — the
    /// first one, said about a dead variable, sends an operator to a second spelling that is
    /// equally dead.
    pub(crate) fn apply(&mut self, patch: FlagsPatch, file: &Path) -> Result<(), ConfigError> {
        for (key, written) in [
            ("poly_presubmit_register", patch.poly_presubmit_register),
            ("poly_rate_gate", patch.poly_rate_gate),
            ("poly_chain_watch", patch.poly_chain_watch),
            ("poly_chain_proxy", patch.poly_chain_proxy),
            ("bybit_fast_exec", patch.bybit_fast_exec),
            ("binance_trade_lite_fill", patch.binance_trade_lite_fill),
        ] {
            if let Some(v) = written {
                return Err(removed_flag_key(file, key, v));
            }
        }
        // …and the DEAD family, whose refusal says the opposite thing about the variable.
        for (key, written) in [("record_dvol", patch.record_dvol)] {
            if let Some(v) = written {
                return Err(dead_flag_key(file, key, v));
            }
        }
        if let Some(v) = patch.reconcile {
            self.reconcile = v;
        }
        if let Some(v) = patch.reconcile_generate_missing {
            self.reconcile_generate_missing = v;
        }
        if let Some(v) = patch.reconcile_balance {
            self.reconcile_balance = v;
        }
        if let Some(v) = patch.oco_cancel_sibling_on_dead_exit {
            self.oco_cancel_sibling_on_dead_exit = v;
        }
        if let Some(v) = patch.tradehub_live {
            self.tradehub_live = v;
        }
        if let Some(v) = patch.tradehub_control {
            self.tradehub_control = v;
        }
        if let Some(v) = patch.tradehub_allow_public_bind {
            self.tradehub_allow_public_bind = v;
        }
        if let Some(v) = patch.telegram_control {
            self.telegram_control = v;
        }
        if let Some(v) = patch.cancel_orders_on_shutdown {
            self.cancel_orders_on_shutdown = v;
        }
        if let Some(v) = patch.poly_exec {
            self.poly_exec = v;
        }
        if let Some(v) = patch.poly_reconcile {
            self.poly_reconcile = v;
        }
        if let Some(v) = patch.poly_heartbeat {
            self.poly_heartbeat = v;
        }
        if let Some(v) = patch.poly_auto_redeem {
            self.poly_auto_redeem = v;
        }
        if let Some(v) = patch.poly_redeem_halt {
            self.poly_redeem_halt = v;
        }
        if let Some(v) = patch.pm_resolve {
            self.pm_resolve = v;
        }
        if let Some(v) = patch.hl_outcome {
            self.hl_outcome = v;
        }
        if let Some(v) = patch.hyperliquid_hip3 {
            self.hyperliquid_hip3 = v;
        }
        if let Some(v) = patch.record_properties {
            self.record_properties = v;
        }
        if let Some(v) = patch.record_chains {
            self.record_chains = v;
        }
        if let Some(v) = patch.allow_withdraw_keys {
            self.allow_withdraw_keys = v;
        }
        if let Some(v) = patch.preflight_skip {
            self.preflight_skip = v;
        }
        if let Some(v) = patch.reconcile_off {
            self.reconcile_off = v;
        }
        if let Some(v) = patch.venue_catalog_off {
            self.venue_catalog_off = v;
        }
        if let Some(v) = patch.datahub_live {
            self.datahub_live = v;
        }
        if let Some(v) = patch.datahub_chart_seed {
            self.datahub_chart_seed = v;
        }
        if let Some(v) = patch.datahub_allow_public_bind {
            self.datahub_allow_public_bind = v;
        }
        Ok(())
    }
}

impl EnvOverride for Flags {
    /// Every field with an environment layer goes through `layers::parse_flag` — the exact
    /// `"1"`/`"0"` grammar, with a truthy typo raised as an error rather than read as false. The
    /// flags whose variable decision 0095 retired have no arm here at all
    /// ([`FlagMeta::reads_env`] is `false` for each) — the presence-parsed kill switch among them.
    fn apply_env(&mut self, env: &HashMap<String, String>) -> Result<(), ConfigError> {
        if let Some(v) = get(env, RECONCILE_ENV) {
            self.reconcile = parse_flag(RECONCILE_ENV, v)?;
        }
        if let Some(v) = get(env, RECONCILE_GENERATE_MISSING_ENV) {
            self.reconcile_generate_missing = parse_flag(RECONCILE_GENERATE_MISSING_ENV, v)?;
        }
        if let Some(v) = get(env, RECONCILE_BALANCE_ENV) {
            self.reconcile_balance = parse_flag(RECONCILE_BALANCE_ENV, v)?;
        }
        if let Some(v) = get(env, OCO_CANCEL_SIBLING_ON_DEAD_EXIT_ENV) {
            self.oco_cancel_sibling_on_dead_exit =
                parse_flag(OCO_CANCEL_SIBLING_ON_DEAD_EXIT_ENV, v)?;
        }
        if let Some(v) = get(env, TRADEHUB_LIVE_ENV) {
            self.tradehub_live = parse_flag(TRADEHUB_LIVE_ENV, v)?;
        }
        if let Some(v) = get(env, TRADEHUB_CONTROL_ENV) {
            self.tradehub_control = parse_flag(TRADEHUB_CONTROL_ENV, v)?;
        }
        if let Some(v) = get(env, TRADEHUB_ALLOW_PUBLIC_BIND_ENV) {
            self.tradehub_allow_public_bind = parse_flag(TRADEHUB_ALLOW_PUBLIC_BIND_ENV, v)?;
        }
        if let Some(v) = get(env, TELEGRAM_CONTROL_ENV) {
            self.telegram_control = parse_flag(TELEGRAM_CONTROL_ENV, v)?;
        }
        if let Some(v) = get(env, CANCEL_ORDERS_ON_SHUTDOWN_ENV) {
            self.cancel_orders_on_shutdown = parse_flag(CANCEL_ORDERS_ON_SHUTDOWN_ENV, v)?;
        }
        // POLY_EXEC / POLY_RECONCILE are NOT read (decision 0095): both variables refuse startup
        // (crate::REMOVED_ENV), flags.poly_exec / flags.poly_reconcile are the only source, and
        // FlagMeta::reads_env is false for both rows.
        //
        // The five settlement-poller flags have NO environment layer since decision 0095 (their
        // variables are crate::REMOVED_ENV refusals, FlagMeta::reads_env is false); the rows stay,
        // inert, until a composition root mounts a poller — see crate::CONSUMPTION.
        //
        // HYPERLIQUID_HIP3 / VIKE_ALLOW_WITHDRAW_KEYS are NOT read (decision 0095): both variables
        // refuse startup (crate::REMOVED_ENV), flags.hyperliquid_hip3 / flags.allow_withdraw_keys
        // are the only source, and FlagMeta::reads_env is false for both.
        if let Some(v) = get(env, RECORD_PROPERTIES_ENV) {
            self.record_properties = parse_flag(RECORD_PROPERTIES_ENV, v)?;
        }
        if let Some(v) = get(env, RECORD_CHAINS_ENV) {
            self.record_chains = parse_flag(RECORD_CHAINS_ENV, v)?;
        }
        // ⚠ `RECORD_DVOL_ENV` is NOT folded here — the field it set is deleted
        // ([`DEAD_FLAG_KEYS`]). The variable is still LOOKED UP, once, by `crate::load`, which
        // WARNS that it configures nothing: refusing it at startup would stop a correct daemon
        // dead over a spelling that never changed anything, and dropping it in silence would take
        // away the only notice an operator who exported it will ever get.
        if let Some(v) = get(env, PREFLIGHT_SKIP_ENV) {
            self.preflight_skip = parse_flag(PREFLIGHT_SKIP_ENV, v)?;
        }
        if let Some(v) = get(env, RECONCILE_OFF_ENV) {
            self.reconcile_off = parse_flag(RECONCILE_OFF_ENV, v)?;
        }
        if let Some(v) = get(env, VENUE_CATALOG_OFF_ENV) {
            self.venue_catalog_off = parse_flag(VENUE_CATALOG_OFF_ENV, v)?;
        }
        if let Some(v) = get(env, DATAHUB_LIVE_ENV) {
            self.datahub_live = parse_flag(DATAHUB_LIVE_ENV, v)?;
        }
        if let Some(v) = get(env, DATAHUB_CHART_SEED_ENV) {
            self.datahub_chart_seed = parse_flag(DATAHUB_CHART_SEED_ENV, v)?;
        }
        if let Some(v) = get(env, DATAHUB_ALLOW_PUBLIC_BIND_ENV) {
            self.datahub_allow_public_bind = parse_flag(DATAHUB_ALLOW_PUBLIC_BIND_ENV, v)?;
        }
        Ok(())
    }
}

impl CliOverride for Flags {
    fn apply_cli(&mut self, cli: &CliOverrides) -> Result<(), ConfigError> {
        if let Some(v) = cli.reconcile {
            self.reconcile = v;
        }
        Ok(())
    }
}

#[path = "flags_tests.rs"]
#[cfg(test)]
mod flags_tests;
