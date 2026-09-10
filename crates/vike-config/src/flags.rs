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
//! an absent configuration is "do the smaller thing". It also keeps this crate byte-compatible
//! with today, where every one of them is opt-in. Two fields need that sentence read carefully and
//! say so in their own docs — [`Flags::poly_redeem_halt`] (a kill SWITCH: `false` means "not
//! halted", and it is safe only because the poller it halts is itself off by default) and
//! [`Flags::preflight_skip`] / [`Flags::allow_withdraw_keys`] / [`Flags::reconcile_off`] (safety
//! OVERRIDES, where `false` is the guarded state).
//!
//! ⚠ [`Flags::reconcile_off`] is the newest of those and the one whose shape is worth reading
//! before copying: it exists because a DEFAULT-ON behaviour still has to be refusable, and this
//! type's `false`-is-off invariant is machine-checked (`tests/flag_registry.rs`'s
//! `every_flag_defaults_off`). A default-on toggle cannot be a field here; its REFUSAL can, and is.
//! `docs/decisions/0043-reconcile-is-on-by-default-under-quarantine.md` carries the verdict.
//!
//! ## The owner and the review date — Phase 4's whole point
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
//! ## One deliberate behaviour change: a typo is an error
//!
//! Today these are read as the EXACT string `"1"` and *anything else is silently false* —
//! `VIKE_RECONCILE=true` is a run that reconciles nothing and looks completely normal.
//! This crate's flag parser accepts `"1"` and `"0"` and rejects everything else by name and
//! value. Same idiom, no fuzzy truthiness, but the operator is told.
//!
//! **The one exception is [`Flags::poly_redeem_halt`]**, whose live read is
//! `env::var_os(..).is_some()` — PRESENCE with any value, empty included, halts redemption. It is
//! parsed here by presence for that reason: narrowing a kill switch to `"1"` would silently stop
//! `POLY_REDEEM_HALT=` and `POLY_REDEEM_HALT=0` from halting anything, and a kill switch that
//! fails to fire is the worst regression on this list. There is no typo hazard to close for it —
//! every value already means the same thing.
//!
//! ## Where the env map comes from is the CALLER's business — and it matters
//!
//! [`crate::load`] takes the env map as a parameter; a binary passes `std::env::vars()`, the
//! workspace `.env` map, or a merge (see the crate doc's "I/O ownership"). Two facts a Phase-6
//! caller has to get right, both true of the code as it stands today:
//!
//! - **Some of these are read from the process env only, some from both.** The Polymarket family
//!   and `{VENUE}_MAINNET` read process env FIRST and the workspace `.env` second; `VIKE_RECONCILE`
//!   deliberately reads the real process env and NOT the credentials map
//!   (`vike_ops::reconcile_config`'s module doc is the authority). Merging the two maps for this
//!   type reproduces the "both" behaviour with process-env precedence; passing only one does not.
//! - **A `.env` value may carry a trailing comment.** The Polymarket sites normalize with
//!   `first_token` before comparing, so `POLY_EXEC=1 # arm it` arms today. This crate does no such
//!   normalization, so a caller handing it a RAW `.env` map would turn that line into a
//!   [`ConfigError::Env`] rather than an arm. Normalizing on the way in is the caller's job, and
//!   is the safer half of the trade — the value is rejected loudly, never read as false.
//!
//! ## What is deliberately NOT a field here
//!
//! Six things in the tree are boolean-ish and are still not [`Flags`] fields. Each is named so the
//! omission is a decision on the record rather than a gap:
//!
//! 1. **The two COMPUTED per-venue families.** `{VENUE}_MAINNET` (`vike_bridge_core::mainnet`) and
//!    `VIKE_MARK_STREAMS_{VENUE}` (`vike_bridge_core::mark_stream`) are keyed by venue, not by a
//!    fixed name. A struct field per venue would be a second copy of a roster this workspace
//!    already keeps in exactly one place — `vike_model::VENUES`, with `mainnet_switch_for`'s
//!    exhaustiveness gate over it — and "a bound is imported, never restated" is the rule the crate
//!    doc opens with. The shape they want is a `BTreeMap<venue, bool>` field validated against
//!    `VENUES` at load, which is a different type (`Flags` is `Copy` today) and a different
//!    argument; it is proposed, not smuggled in here.
//! 2. **`VIKE_MARK_STREAMS`** (the master of that family) is **default ON** and disabled only by
//!    the exact `"0"`. Representing it needs either a `true` default — breaking the invariant that
//!    makes an absent `flags.toml` the safe reading — or an inverted `mark_streams_disabled` field,
//!    which renames an operator-facing concept to suit a struct. It travels with its per-venue
//!    sibling.
//! 3. **`POLY_PROXY_ENABLED` / `POLY_WS_PROXY_ENABLED` / `POLY_SOCKS_PROXY`** parse a FUZZY
//!    grammar (`false`/`0`/`no`/`off`, `1`/`true`/`yes`/`on`) and `POLY_PROXY_ENABLED` defaults ON.
//!    Narrowing them to `"1"`/`"0"` would silently drop the Dublin egress proxy on a deployment
//!    spelling it `POLY_PROXY_ENABLED=true`, and Polymarket is geo-blocked without it. They are
//!    egress DEPLOYMENT settings — [`crate::Config`]'s side of the taxonomy — not operator toggles.
//! 4. **`VIKE_SWEEP_SEQUENTIAL`** is the on/off sibling of [`crate::Preferences::sweep_threads`]:
//!    backtest execution TUNING, which the taxonomy puts in preferences, not a live-path toggle.
//! 5. **The presence-based GUI/QA harness** — `VIKE_SHOT`, `VIKE_APPMAX`, `VIKE_TOOLS`, `VIKE_MAX`,
//!    `VIKE_MIN`, `VIKE_DOM_TESTORDER`, `VIKE_STUDIO_AUTORUN` — drives screenshot capture and dev
//!    convenience, never what a trading process does. (`VIKE_SHOT` is not even a boolean: its value
//!    is the output PNG path.)
//! 6. **Test-only self-skip gates** — `VIKE_CAPTURE_FIXTURES`, `POLY_*_SMOKE`, `PMXT_SMOKE`,
//!    `EOD_SMOKE`, `ASTER_SOAK_ALLOW_MAINNET` — exist only under `tests/` and gate `#[ignore]`d
//!    live smokes. `vike_ops::settings::Layer::TestOnly` already classifies them.
//!
//! ## Which of these are actually CONSUMED — and how to find out
//!
//! Phase 4 rewired nothing: every variable below was still read by its existing owner, and this
//! type was a parallel, currently-dead declaration of the same set. That is no longer uniformly
//! true, and the difference is machine-recorded rather than described here: **[`crate::CONSUMPTION`]
//! carries one row per flag** naming the file and the read that consumes it, or a written admission
//! that nothing does, and `crates/vike-config/tests/settings_are_consumed.rs` opens each claimed
//! consumer and looks.
//!
//! No count is written down, because it will be wrong by the next flag that moves —
//! `vike-cli config show`'s `READ` column prints the answer per key. What is worth stating is the
//! RULE the rows follow: a flag whose read still lives in a venue adapter or a recorder stays
//! `Consumer::Not`, because moving it means threading the value from a composition root through
//! `vike_run::NodeConfig` / `vike_mount::make_engine` into that adapter, and a flag living in two
//! places that DISAGREE would be worse than the state this program is fixing. The env read and the
//! file layer flip together, per flag.
//!
//! ## The file
//!
//! ```toml
//! # <project>/settings/flags.toml — one key per field below, all optional, all default false.
//! reconcile = true
//! poly_exec = true
//! poly_reconcile = true
//! record_properties = true
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::error::ConfigError;
use crate::layers::{CliOverride, CliOverrides, EnvOverride, get, parse_flag};

// -------------------------------------------------------------------------------------------
// Environment variable names
//
// Every literal below already carries a row in `vike_ops::settings::SETTINGS`. That gate harvests
// env-shaped string literals from the whole `crates/` tree and fails on an undeclared one, so a
// name invented here would break CI in a crate this phase must not touch. See `crate::preferences`'
// module doc for the same constraint met from the other side.
// -------------------------------------------------------------------------------------------

/// `VIKE_RECONCILE` — the live reconciliation engine's master gate.
pub const RECONCILE_ENV: &str = "VIKE_RECONCILE";
/// `VIKE_RECONCILE_OFF` — ⚠ SAFETY OVERRIDE: refuse the default-on live reconcile.
pub const RECONCILE_OFF_ENV: &str = "VIKE_RECONCILE_OFF";

/// The warning [`crate::load`] raises when a deployment WROTE a refusal that S2 no longer honours:
/// `reconcile = false` in `flags.toml`, `VIKE_RECONCILE=0` in the environment, or `--reconcile
/// false` on the command line. `origin` is where it was written, in the operator's own vocabulary.
///
/// It exists because the two states are indistinguishable downstream. [`Flags::reconcile`] resolves
/// to a `bool`, so `vike_ops::reconcile_config::reconcile_gate` cannot tell "nobody said anything"
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
    format!(
        "{origin} no longer turns reconciliation OFF. Since 2026-09-06 a mount that arms at least \
         one LIVE venue account reconciles by default (under `quarantine`, so it folds nothing) — \
         `reconcile`/`VIKE_RECONCILE` is now a FORCE-ON and an explicit `false` reads the same as \
         unset. The switch you want is `reconcile_off = true` in <project>/settings/flags.toml, or \
         VIKE_RECONCILE_OFF=1. See docs/ops/reconcile-on-restart.md"
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
/// `VIKE_TRADEHUB_RECORD` — tee the daemon's live feeds into the store.
pub const TRADEHUB_RECORD_ENV: &str = "VIKE_TRADEHUB_RECORD";
/// `VIKE_CANCEL_ORDERS_ON_SHUTDOWN` — cancel every resting order during the daemon's teardown.
pub const CANCEL_ORDERS_ON_SHUTDOWN_ENV: &str = "VIKE_CANCEL_ORDERS_ON_SHUTDOWN";

/// `POLY_EXEC` — mount Polymarket's live `ExecutionClient` (and, inseparably, its user-WS fill pump).
pub const POLY_EXEC_ENV: &str = "POLY_EXEC";
/// `POLY_RECONCILE` — mount Polymarket's `ReconClient`.
pub const POLY_RECONCILE_ENV: &str = "POLY_RECONCILE";
/// `POLY_PRESUBMIT_REGISTER` — pre-register the derived order id before each submit.
pub const POLY_PRESUBMIT_REGISTER_ENV: &str = "POLY_PRESUBMIT_REGISTER";
/// `POLY_HEARTBEAT` — run the venue's dead-man heartbeat poller.
pub const POLY_HEARTBEAT_ENV: &str = "POLY_HEARTBEAT";
/// `POLY_RATE_GATE` — turn the observe-only local submit rate mirror into a real gate.
pub const POLY_RATE_GATE_ENV: &str = "POLY_RATE_GATE";
/// `POLY_CHAIN_WATCH` — run the on-chain settlement watcher.
pub const POLY_CHAIN_WATCH_ENV: &str = "POLY_CHAIN_WATCH";
/// `POLY_CHAIN_PROXY` — route the Polygon RPC through the venue's SOCKS tunnel.
pub const POLY_CHAIN_PROXY_ENV: &str = "POLY_CHAIN_PROXY";
/// `POLY_AUTO_REDEEM` — run the CTF auto-redeem poller.
pub const POLY_AUTO_REDEEM_ENV: &str = "POLY_AUTO_REDEEM";
/// `POLY_REDEEM_HALT` — the auto-redeem KILL SWITCH. ⚠ Parsed by PRESENCE, see [`Flags`].
pub const POLY_REDEEM_HALT_ENV: &str = "POLY_REDEEM_HALT";

/// `VIKE_PM_RESOLVE` — run the Polymarket market-resolution settlement poller.
pub const PM_RESOLVE_ENV: &str = "VIKE_PM_RESOLVE";
/// `VIKE_HL_OUTCOME` — run the Hyperliquid outcome-token settlement lane.
pub const HL_OUTCOME_ENV: &str = "VIKE_HL_OUTCOME";

/// `VIKE_BYBIT_FAST_EXEC` — subscribe Bybit's `execution.fast` private topic.
pub const BYBIT_FAST_EXEC_ENV: &str = "VIKE_BYBIT_FAST_EXEC";
/// `VIKE_BINANCE_TRADE_LITE_FILL` — emit an early bare fill from each `TRADE_LITE` frame.
pub const BINANCE_TRADE_LITE_FILL_ENV: &str = "VIKE_BINANCE_TRADE_LITE_FILL";
/// `HYPERLIQUID_HIP3` — enumerate HIP-3 builder-dex markets into symbology.
pub const HYPERLIQUID_HIP3_ENV: &str = "HYPERLIQUID_HIP3";

/// `VIKE_RECORD_PROPERTIES` — record each venue's observed `SymbolProperties` grid over time.
pub const RECORD_PROPERTIES_ENV: &str = "VIKE_RECORD_PROPERTIES";
/// `VIKE_RECORD_CHAINS` — record option-chain snapshots.
pub const RECORD_CHAINS_ENV: &str = "VIKE_RECORD_CHAINS";
/// `VIKE_RECORD_DVOL` — record Deribit's DVOL index.
pub const RECORD_DVOL_ENV: &str = "VIKE_RECORD_DVOL";

/// `VIKE_ALLOW_WITHDRAW_KEYS` — permit arming a live venue with a withdraw-capable API key.
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
    /// lookbacks and the policy name — and belongs in [`crate::Config`] when it moves.
    ///
    /// ⚠ **Since S2 this is no longer the whole gate, and it is no longer the DEFAULT answer.** A
    /// mount that arms at least one LIVE venue account reconciles whether or not this is set —
    /// `vike_ops::reconcile_config::reconcile_gate` is the one decision both live roots make, and
    /// `docs/decisions/0043-reconcile-is-on-by-default-under-quarantine.md` is the verdict. What
    /// this field still MEANS is "on even where that default would not turn it on": the default
    /// keys off `vike_run::armed_live_venues`, a hand-written per-venue probe, and a venue arm
    /// merged without its row would report a live mount as paper. [`Flags::reconcile_off`] is the
    /// refusal.
    ///
    /// ⚠ **An explicitly-written `false` is IGNORED, and the loader says so out loud.** Because the
    /// resolved value is a `bool`, `reconcile = false` in a deployed `flags.toml` (or
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

    /// Tee the daemon's live feeds into the history store through a `RecorderSink`.
    ///
    /// Was `VIKE_TRADEHUB_RECORD`. Owner `@AlexSokhanych` · review 2027-02-01 · GRADUATE — what
    /// to record is a recorder PROFILE, not a boolean.
    pub tradehub_record: bool,

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
    /// Was `POLY_EXEC`. Owner `@AlexSokhanych` · review 2026-11-01 · GRADUATE.
    pub poly_exec: bool,

    /// Mount Polymarket's `ReconClient`. ⚠ Meaningful only alongside [`Flags::poly_exec`]:
    /// reconciling a PAPER engine against live venue state makes the two sides different accounts,
    /// and under the `hybrid` policy `PositionDrift` auto-applies — folding the LIVE account's
    /// position into the PAPER engine's books at the venue's avg price. Run recon-without-exec
    /// under `VIKE_RECONCILE_POLICY=quarantine`. (⚠ was "diffs every local order as an orphan and,
    /// under the `hybrid` policy, auto-cancels it every pass" — the orphan half is true, the
    /// auto-cancel half is not: see `crates/vike-exec/tests/recon/recon_policy_pin.rs`.)
    ///
    /// Was `POLY_RECONCILE`. Owner `@AlexSokhanych` · review 2026-11-01 · GRADUATE.
    pub poly_reconcile: bool,

    /// Pre-register the derived `(coid, order id)` pair just before each submit, so a fill that
    /// beats the ack needs no staging to be attributed.
    ///
    /// Was `POLY_PRESUBMIT_REGISTER`. Owner `@AlexSokhanych` · review 2026-11-01 · RETIRE — since
    /// the unconditional staging landed this is an OPTIMISATION, not the safety net it once was,
    /// and an optimisation either wins for everyone or goes.
    pub poly_presubmit_register: bool,

    /// Run the venue's dead-man heartbeat poller. Polymarket cancels ALL open orders if no valid
    /// beat lands within ~10 s, so a live maker that does not beat is a maker that gets flattened.
    ///
    /// Was `POLY_HEARTBEAT`. Owner `@AlexSokhanych` · review 2026-11-01 · RETIRE — a dead-man
    /// switch that a live maker cannot run without is not an option; the CADENCE is the config.
    pub poly_heartbeat: bool,

    /// Turn the local submit rate mirror from OBSERVE-ONLY into a real gate that refuses an
    /// over-budget submit (as an `OrderRejected`, never a silent drop).
    ///
    /// Was `POLY_RATE_GATE`. Owner `@AlexSokhanych` · review 2026-11-01 · RETIRE — the observe
    /// phase exists to prove the mirror, and a proven mirror should enforce for everyone.
    pub poly_rate_gate: bool,

    /// Run the on-chain (Polygon) settlement watcher, whose verdict outranks the data-api's
    /// `redeemable` flag. ⚠ That flag is NOT a winner flag — every position of a resolved
    /// condition reports `redeemable: true`, losers included.
    ///
    /// Was `POLY_CHAIN_WATCH`. Owner `@AlexSokhanych` · review 2026-11-01 · RETIRE — settlement
    /// truth is not optional once it is proven live.
    pub poly_chain_watch: bool,

    /// Route the Polygon RPC through the venue's SOCKS tunnel, like every other Polymarket lane.
    ///
    /// Was `POLY_CHAIN_PROXY`. Owner `@AlexSokhanych` · review 2027-02-01 · GRADUATE — egress
    /// routing is deployment [`crate::Config`], and its `POLY_*_PROXY_*` siblings already are.
    pub poly_chain_proxy: bool,

    /// Run the CTF auto-redeem poller: redeem resolved winning binary AND neg-risk positions
    /// through the gasless relayer.
    ///
    /// Was `POLY_AUTO_REDEEM`. Owner `@AlexSokhanych` · review 2026-11-01 · KEEP — it MOVES REAL
    /// MONEY on-chain and stays an explicit opt-in.
    pub poly_auto_redeem: bool,

    /// ⚠ KILL SWITCH, and the ONE inverted field here: `true` HALTS the auto-redeem poller's
    /// ticks. `false` therefore means "not halted" — safe only because the thing it halts,
    /// [`Flags::poly_auto_redeem`], is itself off by default.
    ///
    /// ⚠ Parsed by PRESENCE from the environment, not by `"1"`, matching today's
    /// `env::var_os(..).is_some()` read: `POLY_REDEEM_HALT=`, `=0` and `=anything` all halt, and
    /// no env value can UN-halt it. The file layer is an ordinary boolean.
    ///
    /// Was `POLY_REDEEM_HALT`. Owner `@AlexSokhanych` · review 2027-02-01 · KEEP.
    pub poly_redeem_halt: bool,

    // --- settlement pollers -----------------------------------------------------------------
    /// Run the Polymarket market-resolution poller, which writes resolved payouts into the core.
    ///
    /// Was `VIKE_PM_RESOLVE`. Owner `@AlexSokhanych` · review 2027-02-01 · KEEP.
    pub pm_resolve: bool,

    /// Run the Hyperliquid outcome-token (prediction market) settlement lane.
    ///
    /// Was `VIKE_HL_OUTCOME`. Owner `@AlexSokhanych` · review 2027-02-01 · KEEP.
    pub hl_outcome: bool,

    // --- venue execution / feed opt-ins -----------------------------------------------------
    /// Subscribe Bybit's low-latency `execution.fast` private topic ALONGSIDE `execution` (it
    /// augments the slow twin, never replaces it — the slow frame carries execFee and the
    /// terminal wrap the FSM folds).
    ///
    /// Was `VIKE_BYBIT_FAST_EXEC`. Owner `@AlexSokhanych` · review 2026-11-01 · RETIRE — a
    /// latency A/B either wins and becomes the path, or it goes.
    pub bybit_fast_exec: bool,

    /// Emit an early bare fill from each Binance `TRADE_LITE` frame, ahead of the full order
    /// update.
    ///
    /// Was `VIKE_BINANCE_TRADE_LITE_FILL`. Owner `@AlexSokhanych` · review 2026-11-01 · RETIRE —
    /// same argument as [`Flags::bybit_fast_exec`], same decision owed.
    pub binance_trade_lite_fill: bool,

    /// Enumerate Hyperliquid HIP-3 builder-dex markets into symbology.
    ///
    /// Was `HYPERLIQUID_HIP3`. Owner `@AlexSokhanych` · review 2027-02-01 · GRADUATE — which
    /// market universe a venue exposes is deployment config, not a run-time toggle.
    pub hyperliquid_hip3: bool,

    // --- recorders --------------------------------------------------------------------------
    /// Record each venue's observed `SymbolProperties` grid into the store's point-in-time
    /// `kind=properties` series at instrument-fetch time.
    ///
    /// Was `VIKE_RECORD_PROPERTIES`. Owner `@AlexSokhanych` · review 2027-02-01 · GRADUATE.
    pub record_properties: bool,

    /// Record option-chain snapshots into the store.
    ///
    /// Was `VIKE_RECORD_CHAINS`. Owner `@AlexSokhanych` · review 2027-02-01 · GRADUATE — it has a
    /// `VIKE_RECORD_CHAINS_CADENCE_MS` sibling that is already config; the pair should move
    /// together.
    pub record_chains: bool,

    /// Record Deribit's DVOL index into the store.
    ///
    /// ⚠ **UNWIRED, again** — this doc said WIRED, and that claim died with the desktop shell's
    /// local market-data plane. It gated a whole FEED (the public keyless
    /// `deribit_volatility_index.*` subscription plus its `DvolRecorder`) from the GUI's `main.rs`,
    /// and the GUI now opens no venue socket at all, so nothing in the tree branches on this value.
    /// `vike_deribit::{DvolRecorder::from_flag, spawn_deribit_dvol_feed}` survive and still take
    /// the resolved flag as a PARAMETER, so the feature is unmounted rather than removed and
    /// re-wiring it is a composition root's job, not this crate's. This crate's `apply_env` remains
    /// the one reader of `VIKE_RECORD_DVOL` in the tree — the promotion that briefly wired the flag
    /// also deleted the recorder's own library env read, and that half was not undone.
    /// `crate::CONSUMPTION`'s row carries the full argument and is the gated statement of it.
    ///
    /// Was `VIKE_RECORD_DVOL`. Owner `@AlexSokhanych` · review 2027-02-01 · GRADUATE — the
    /// `VIKE_RECORD_DVOL_CADENCE_MS` sibling is still a library env read (the same shape as
    /// [`Flags::record_chains`]'s), so the pair has not finished moving.
    pub record_dvol: bool,

    // --- safety overrides -------------------------------------------------------------------
    /// ⚠ SAFETY OVERRIDE. Permit arming a live venue whose API key is WITHDRAW-CAPABLE, which the
    /// key-permission check otherwise refuses. `false` is the guarded state.
    ///
    /// Was `VIKE_ALLOW_WITHDRAW_KEYS`. Owner `@AlexSokhanych` · review 2027-02-01 · KEEP — an
    /// escape hatch for a real key that cannot be re-scoped, and it must stay explicit and loud.
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
    /// Beats [`Flags::reconcile`] when both are set (`vike_ops::reconcile_config::reconcile_gate`
    /// checks the refusal first): a stale `flags.toml` line must not overrule the person typing
    /// the override.
    ///
    /// Was never an environment variable before S2. Owner `@AlexSokhanych` · review 2027-02-01 ·
    /// KEEP — a refusal for a default-on safety behaviour is a standing operator hatch, not a
    /// migration.
    pub reconcile_off: bool,
}

/// The FILE shape of [`Flags`] — all-optional, unknown keys rejected by name.
///
/// One `Option<bool>` per [`Flags`] field, same key spelling, so a `flags.toml` key IS the field
/// name. `tests/flag_registry.rs` proves that per row rather than trusting the eye.
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
    /// See [`Flags::tradehub_record`].
    pub tradehub_record: Option<bool>,
    /// See [`Flags::cancel_orders_on_shutdown`].
    pub cancel_orders_on_shutdown: Option<bool>,
    /// See [`Flags::poly_exec`].
    pub poly_exec: Option<bool>,
    /// See [`Flags::poly_reconcile`].
    pub poly_reconcile: Option<bool>,
    /// See [`Flags::poly_presubmit_register`].
    pub poly_presubmit_register: Option<bool>,
    /// See [`Flags::poly_heartbeat`].
    pub poly_heartbeat: Option<bool>,
    /// See [`Flags::poly_rate_gate`].
    pub poly_rate_gate: Option<bool>,
    /// See [`Flags::poly_chain_watch`].
    pub poly_chain_watch: Option<bool>,
    /// See [`Flags::poly_chain_proxy`].
    pub poly_chain_proxy: Option<bool>,
    /// See [`Flags::poly_auto_redeem`].
    pub poly_auto_redeem: Option<bool>,
    /// See [`Flags::poly_redeem_halt`].
    pub poly_redeem_halt: Option<bool>,
    /// See [`Flags::pm_resolve`].
    pub pm_resolve: Option<bool>,
    /// See [`Flags::hl_outcome`].
    pub hl_outcome: Option<bool>,
    /// See [`Flags::bybit_fast_exec`].
    pub bybit_fast_exec: Option<bool>,
    /// See [`Flags::binance_trade_lite_fill`].
    pub binance_trade_lite_fill: Option<bool>,
    /// See [`Flags::hyperliquid_hip3`].
    pub hyperliquid_hip3: Option<bool>,
    /// See [`Flags::record_properties`].
    pub record_properties: Option<bool>,
    /// See [`Flags::record_chains`].
    pub record_chains: Option<bool>,
    /// See [`Flags::record_dvol`].
    pub record_dvol: Option<bool>,
    /// See [`Flags::allow_withdraw_keys`].
    pub allow_withdraw_keys: Option<bool>,
    /// See [`Flags::preflight_skip`].
    pub preflight_skip: Option<bool>,
    /// See [`Flags::reconcile_off`].
    pub reconcile_off: Option<bool>,
}

impl Flags {
    /// Fold one file's patch in. Nothing to validate — TOML already types a boolean, and
    /// `deny_unknown_fields` on [`FlagsPatch`] catches the misspelled-key case that actually
    /// bites (a flag the operator believes they set).
    pub(crate) fn apply(&mut self, patch: FlagsPatch) {
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
        if let Some(v) = patch.tradehub_record {
            self.tradehub_record = v;
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
        if let Some(v) = patch.poly_presubmit_register {
            self.poly_presubmit_register = v;
        }
        if let Some(v) = patch.poly_heartbeat {
            self.poly_heartbeat = v;
        }
        if let Some(v) = patch.poly_rate_gate {
            self.poly_rate_gate = v;
        }
        if let Some(v) = patch.poly_chain_watch {
            self.poly_chain_watch = v;
        }
        if let Some(v) = patch.poly_chain_proxy {
            self.poly_chain_proxy = v;
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
        if let Some(v) = patch.bybit_fast_exec {
            self.bybit_fast_exec = v;
        }
        if let Some(v) = patch.binance_trade_lite_fill {
            self.binance_trade_lite_fill = v;
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
        if let Some(v) = patch.record_dvol {
            self.record_dvol = v;
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
    }
}

impl EnvOverride for Flags {
    /// Every field but one goes through `layers::parse_flag` — the exact `"1"`/`"0"` grammar,
    /// with a truthy typo raised as an error rather than read as false.
    ///
    /// ⚠ [`Flags::poly_redeem_halt`] is the exception and is armed by the variable being PRESENT
    /// at all, empty value included, mirroring today's `env::var_os(..).is_some()` kill-switch
    /// read. Narrowing it to `"1"` would make `POLY_REDEEM_HALT=0` stop halting — see the module
    /// doc.
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
        if let Some(v) = get(env, TRADEHUB_RECORD_ENV) {
            self.tradehub_record = parse_flag(TRADEHUB_RECORD_ENV, v)?;
        }
        if let Some(v) = get(env, CANCEL_ORDERS_ON_SHUTDOWN_ENV) {
            self.cancel_orders_on_shutdown = parse_flag(CANCEL_ORDERS_ON_SHUTDOWN_ENV, v)?;
        }
        if let Some(v) = get(env, POLY_EXEC_ENV) {
            self.poly_exec = parse_flag(POLY_EXEC_ENV, v)?;
        }
        if let Some(v) = get(env, POLY_RECONCILE_ENV) {
            self.poly_reconcile = parse_flag(POLY_RECONCILE_ENV, v)?;
        }
        if let Some(v) = get(env, POLY_PRESUBMIT_REGISTER_ENV) {
            self.poly_presubmit_register = parse_flag(POLY_PRESUBMIT_REGISTER_ENV, v)?;
        }
        if let Some(v) = get(env, POLY_HEARTBEAT_ENV) {
            self.poly_heartbeat = parse_flag(POLY_HEARTBEAT_ENV, v)?;
        }
        if let Some(v) = get(env, POLY_RATE_GATE_ENV) {
            self.poly_rate_gate = parse_flag(POLY_RATE_GATE_ENV, v)?;
        }
        if let Some(v) = get(env, POLY_CHAIN_WATCH_ENV) {
            self.poly_chain_watch = parse_flag(POLY_CHAIN_WATCH_ENV, v)?;
        }
        if let Some(v) = get(env, POLY_CHAIN_PROXY_ENV) {
            self.poly_chain_proxy = parse_flag(POLY_CHAIN_PROXY_ENV, v)?;
        }
        if let Some(v) = get(env, POLY_AUTO_REDEEM_ENV) {
            self.poly_auto_redeem = parse_flag(POLY_AUTO_REDEEM_ENV, v)?;
        }
        // ⚠ PRESENCE, not `parse_flag` — the kill-switch exception. `contains_key` rather than
        // `get` on purpose: `get` treats an empty value as unset, and an exported-but-empty
        // `POLY_REDEEM_HALT=` halts today.
        if env.contains_key(POLY_REDEEM_HALT_ENV) {
            self.poly_redeem_halt = true;
        }
        if let Some(v) = get(env, PM_RESOLVE_ENV) {
            self.pm_resolve = parse_flag(PM_RESOLVE_ENV, v)?;
        }
        if let Some(v) = get(env, HL_OUTCOME_ENV) {
            self.hl_outcome = parse_flag(HL_OUTCOME_ENV, v)?;
        }
        if let Some(v) = get(env, BYBIT_FAST_EXEC_ENV) {
            self.bybit_fast_exec = parse_flag(BYBIT_FAST_EXEC_ENV, v)?;
        }
        if let Some(v) = get(env, BINANCE_TRADE_LITE_FILL_ENV) {
            self.binance_trade_lite_fill = parse_flag(BINANCE_TRADE_LITE_FILL_ENV, v)?;
        }
        if let Some(v) = get(env, HYPERLIQUID_HIP3_ENV) {
            self.hyperliquid_hip3 = parse_flag(HYPERLIQUID_HIP3_ENV, v)?;
        }
        if let Some(v) = get(env, RECORD_PROPERTIES_ENV) {
            self.record_properties = parse_flag(RECORD_PROPERTIES_ENV, v)?;
        }
        if let Some(v) = get(env, RECORD_CHAINS_ENV) {
            self.record_chains = parse_flag(RECORD_CHAINS_ENV, v)?;
        }
        if let Some(v) = get(env, RECORD_DVOL_ENV) {
            self.record_dvol = parse_flag(RECORD_DVOL_ENV, v)?;
        }
        if let Some(v) = get(env, ALLOW_WITHDRAW_KEYS_ENV) {
            self.allow_withdraw_keys = parse_flag(ALLOW_WITHDRAW_KEYS_ENV, v)?;
        }
        if let Some(v) = get(env, PREFLIGHT_SKIP_ENV) {
            self.preflight_skip = parse_flag(PREFLIGHT_SKIP_ENV, v)?;
        }
        if let Some(v) = get(env, RECONCILE_OFF_ENV) {
            self.reconcile_off = parse_flag(RECONCILE_OFF_ENV, v)?;
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

// -------------------------------------------------------------------------------------------
// Stewardship: owner + review date + expected disposition, one row per field
// -------------------------------------------------------------------------------------------

/// What a flag's review is expected to CONCLUDE. The date forces the conversation; this records
/// which conversation it is, so the review is not re-derived from scratch every time.
///
/// Deliberately three coarse verdicts rather than a free-text note: a verdict a reader can act on
/// beats a paragraph they have to interpret, and "keep" being explicit is what stops the list from
/// reading as a backlog of unfinished work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Disposition {
    /// Stays a flag. This is genuinely a per-run operator decision — a real-money arm, a kill
    /// switch, a safety override — and a file that quietly carries it would be worse, not better.
    Keep,
    /// Should GRADUATE out of [`Flags`] into [`crate::Config`] (or [`crate::Policy`]): it is not
    /// really a per-run decision, it is how this deployment is set up, and it usually already has
    /// non-boolean siblings (`*_CADENCE_MS`, `*_INTERVAL_MS`, a policy name) sitting in config.
    Graduate,
    /// Should be DELETED, with the behaviour it gates becoming unconditional (or dropped). The
    /// flag exists because something was unproven; once it is proven, an option nobody should turn
    /// off is a branch nobody tests.
    Retire,
}

/// One flag's stewardship record — the machine-readable twin of the owner/review line in each
/// [`Flags`] field's doc comment, and the AUTHORITY when the two disagree.
///
/// Kept as a `&'static [FlagMeta]` rather than attributes on the fields for the reason the
/// `vike_model::VENUES` roster and `vike_ops::settings::SETTINGS` are tables: a table can be
/// iterated by a test, and a doc comment cannot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FlagMeta {
    /// The [`Flags`] field name — identical to the `flags.toml` key, which
    /// `tests/flag_registry.rs` proves rather than assumes.
    pub field: &'static str,
    /// The environment variable that overrides it.
    pub env: &'static str,
    /// WHO decides this flag's fate. Never blank — that is the one thing the gate exists to
    /// enforce, because a flag with no owner is a flag nobody will ever delete.
    ///
    /// Every row names the same handle today, because the repository has one maintainer and
    /// inventing team names would be fiction. The field exists so ownership can diverge later
    /// without a schema change — which is the whole reason it is a column and not a comment.
    pub owner: &'static str,
    /// ISO-8601 `YYYY-MM-DD`: when this flag's [`Disposition`] is due to be revisited.
    ///
    /// ⚠ A passed date does NOT fail the gate — see the module doc for why the calendar must not
    /// become a merge blocker on unrelated PRs.
    pub review: &'static str,
    /// What that review is expected to conclude.
    pub disposition: Disposition,
}

/// Every flag's owner and review date, one row per [`Flags`] field.
///
/// `crates/vike-config/tests/flag_registry.rs` gates this table BOTH ways — a field with no row
/// and a row with no field both fail — and additionally drives each row through the real env and
/// file layers, so a row wired to the wrong field fails too. Adding a `Flags` field without adding
/// a row here therefore fails CI; it does not depend on a reviewer noticing.
///
/// The dates cluster on two dates on purpose: `2026-11-01` for anything touching a live order
/// path, a remote write surface or an unproven-and-therefore-flagged behaviour, and `2027-02-01`
/// for the diagnostics, recorders and long-lived escape hatches. A per-flag date invented to look
/// precise would be noise.
pub const FLAG_REGISTRY: &[FlagMeta] = &[
    // --- reconciliation ---------------------------------------------------------------------
    FlagMeta {
        field: "reconcile",
        env: RECONCILE_ENV,
        owner: "@AlexSokhanych",
        review: "2026-11-01",
        // Named by the design as a permanent flag dressed as a temporary one. The rest of the
        // `VIKE_RECONCILE_*` family (interval, lookback, policy) is already config-shaped.
        disposition: Disposition::Graduate,
    },
    FlagMeta {
        field: "reconcile_generate_missing",
        env: RECONCILE_GENERATE_MISSING_ENV,
        owner: "@AlexSokhanych",
        review: "2027-02-01",
        // A reconcile POLICY choice, and `VIKE_RECONCILE_POLICY` is already a config string.
        disposition: Disposition::Graduate,
    },
    FlagMeta {
        field: "reconcile_balance",
        env: RECONCILE_BALANCE_ENV,
        owner: "@AlexSokhanych",
        review: "2027-02-01",
        // Ships with `*_TOL_ABS`/`*_TOL_REL` siblings that are plainly config; the trio moves
        // together or not at all.
        disposition: Disposition::Graduate,
    },
    FlagMeta {
        field: "oco_cancel_sibling_on_dead_exit",
        env: OCO_CANCEL_SIBLING_ON_DEAD_EXIT_ENV,
        owner: "@AlexSokhanych",
        review: "2026-11-01",
        disposition: Disposition::Retire,
    },
    // --- the headless daemon ----------------------------------------------------------------
    FlagMeta {
        field: "tradehub_live",
        env: TRADEHUB_LIVE_ENV,
        owner: "@AlexSokhanych",
        review: "2026-11-01",
        disposition: Disposition::Keep,
    },
    FlagMeta {
        field: "tradehub_control",
        env: TRADEHUB_CONTROL_ENV,
        owner: "@AlexSokhanych",
        review: "2026-11-01",
        // Also named by the design. It already has `*_KEY` and `*_RATE` config siblings; the
        // listen ADDRESS being present is the honest gate, not a separate boolean.
        disposition: Disposition::Graduate,
    },
    FlagMeta {
        field: "tradehub_allow_public_bind",
        env: TRADEHUB_ALLOW_PUBLIC_BIND_ENV,
        owner: "@AlexSokhanych",
        review: "2026-11-01",
        // KEEP. Unlike its siblings this is a safety OVERRIDE, not a feature switch: it has no
        // config-shaped answer to graduate INTO (the address it guards is already
        // `config.tradehub_addr`, and letting that address imply its own consent is exactly the
        // failure being closed), and retiring it would mean either refusing every LAN deployment
        // or silently publishing a plaintext order surface again.
        disposition: Disposition::Keep,
    },
    FlagMeta {
        field: "telegram_control",
        env: TELEGRAM_CONTROL_ENV,
        owner: "@AlexSokhanych",
        review: "2026-11-01",
        disposition: Disposition::Keep,
    },
    FlagMeta {
        field: "tradehub_record",
        env: TRADEHUB_RECORD_ENV,
        owner: "@AlexSokhanych",
        review: "2027-02-01",
        disposition: Disposition::Graduate,
    },
    FlagMeta {
        field: "cancel_orders_on_shutdown",
        env: CANCEL_ORDERS_ON_SHUTDOWN_ENV,
        owner: "@AlexSokhanych",
        review: "2026-11-01",
        // A standing operator preference about what a stop does to the book — there is no end
        // state in which this stops being a choice, so it is not on its way anywhere.
        disposition: Disposition::Keep,
    },
    // --- Polymarket -------------------------------------------------------------------------
    FlagMeta {
        field: "poly_exec",
        env: POLY_EXEC_ENV,
        owner: "@AlexSokhanych",
        review: "2026-11-01",
        // The third flag the design names by hand.
        disposition: Disposition::Graduate,
    },
    FlagMeta {
        field: "poly_reconcile",
        env: POLY_RECONCILE_ENV,
        owner: "@AlexSokhanych",
        review: "2026-11-01",
        disposition: Disposition::Graduate,
    },
    FlagMeta {
        field: "poly_presubmit_register",
        env: POLY_PRESUBMIT_REGISTER_ENV,
        owner: "@AlexSokhanych",
        review: "2026-11-01",
        disposition: Disposition::Retire,
    },
    FlagMeta {
        field: "poly_heartbeat",
        env: POLY_HEARTBEAT_ENV,
        owner: "@AlexSokhanych",
        review: "2026-11-01",
        disposition: Disposition::Retire,
    },
    FlagMeta {
        field: "poly_rate_gate",
        env: POLY_RATE_GATE_ENV,
        owner: "@AlexSokhanych",
        review: "2026-11-01",
        disposition: Disposition::Retire,
    },
    FlagMeta {
        field: "poly_chain_watch",
        env: POLY_CHAIN_WATCH_ENV,
        owner: "@AlexSokhanych",
        review: "2026-11-01",
        disposition: Disposition::Retire,
    },
    FlagMeta {
        field: "poly_chain_proxy",
        env: POLY_CHAIN_PROXY_ENV,
        owner: "@AlexSokhanych",
        review: "2027-02-01",
        disposition: Disposition::Graduate,
    },
    FlagMeta {
        field: "poly_auto_redeem",
        env: POLY_AUTO_REDEEM_ENV,
        owner: "@AlexSokhanych",
        review: "2026-11-01",
        disposition: Disposition::Keep,
    },
    FlagMeta {
        field: "poly_redeem_halt",
        env: POLY_REDEEM_HALT_ENV,
        owner: "@AlexSokhanych",
        review: "2027-02-01",
        disposition: Disposition::Keep,
    },
    // --- settlement pollers -----------------------------------------------------------------
    FlagMeta {
        field: "pm_resolve",
        env: PM_RESOLVE_ENV,
        owner: "@AlexSokhanych",
        review: "2027-02-01",
        disposition: Disposition::Keep,
    },
    FlagMeta {
        field: "hl_outcome",
        env: HL_OUTCOME_ENV,
        owner: "@AlexSokhanych",
        review: "2027-02-01",
        disposition: Disposition::Keep,
    },
    // --- venue execution / feed opt-ins -----------------------------------------------------
    FlagMeta {
        field: "bybit_fast_exec",
        env: BYBIT_FAST_EXEC_ENV,
        owner: "@AlexSokhanych",
        review: "2026-11-01",
        disposition: Disposition::Retire,
    },
    FlagMeta {
        field: "binance_trade_lite_fill",
        env: BINANCE_TRADE_LITE_FILL_ENV,
        owner: "@AlexSokhanych",
        review: "2026-11-01",
        disposition: Disposition::Retire,
    },
    FlagMeta {
        field: "hyperliquid_hip3",
        env: HYPERLIQUID_HIP3_ENV,
        owner: "@AlexSokhanych",
        review: "2027-02-01",
        disposition: Disposition::Graduate,
    },
    // --- recorders --------------------------------------------------------------------------
    FlagMeta {
        field: "record_properties",
        env: RECORD_PROPERTIES_ENV,
        owner: "@AlexSokhanych",
        review: "2027-02-01",
        disposition: Disposition::Graduate,
    },
    FlagMeta {
        field: "record_chains",
        env: RECORD_CHAINS_ENV,
        owner: "@AlexSokhanych",
        review: "2027-02-01",
        disposition: Disposition::Graduate,
    },
    FlagMeta {
        field: "record_dvol",
        env: RECORD_DVOL_ENV,
        owner: "@AlexSokhanych",
        review: "2027-02-01",
        disposition: Disposition::Graduate,
    },
    // --- safety overrides -------------------------------------------------------------------
    FlagMeta {
        field: "allow_withdraw_keys",
        env: ALLOW_WITHDRAW_KEYS_ENV,
        owner: "@AlexSokhanych",
        review: "2027-02-01",
        disposition: Disposition::Keep,
    },
    FlagMeta {
        field: "preflight_skip",
        env: PREFLIGHT_SKIP_ENV,
        owner: "@AlexSokhanych",
        review: "2027-02-01",
        disposition: Disposition::Keep,
    },
    FlagMeta {
        field: "reconcile_off",
        env: RECONCILE_OFF_ENV,
        owner: "@AlexSokhanych",
        review: "2027-02-01",
        // KEEP, not GRADUATE, and for the reason the safety-override siblings above give: this is
        // an escape hatch from a DEFAULT-ON safety behaviour, not a piece of configuration on its
        // way to `Config`. It should stay a flag for exactly as long as the default exists.
        disposition: Disposition::Keep,
    },
];

/// The row for one [`Flags`] field, by field name. `None` for an unknown name.
///
/// Exists so a `config show`-style dump can print "who owns this and when is it reviewed" next to
/// an effective value, which is the operator-facing point of the whole table.
#[must_use]
pub fn flag_meta(field: &str) -> Option<&'static FlagMeta> {
    FLAG_REGISTRY.iter().find(|m| m.field == field)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_flag_defaults_off() {
        // Serialized rather than field-by-field: this cannot go stale when a field is added,
        // which a hand-written list of `assert!(!f.x)` silently does.
        let table = toml::Table::try_from(Flags::default()).unwrap();
        assert_eq!(table.len(), FLAG_REGISTRY.len());
        for (name, value) in &table {
            assert_eq!(value.as_bool(), Some(false), "{name} must default to false");
        }
    }

    #[test]
    fn env_turns_a_flag_on_and_off() {
        let mut f = Flags::default();
        f.apply_env(&HashMap::from([(RECONCILE_ENV.to_string(), "1".to_string())])).unwrap();
        assert!(f.reconcile);
        f.apply_env(&HashMap::from([(RECONCILE_ENV.to_string(), "0".to_string())])).unwrap();
        assert!(!f.reconcile);
    }

    #[test]
    fn a_file_true_can_be_turned_off_by_env() {
        let mut f = Flags::default();
        f.apply(FlagsPatch { poly_exec: Some(true), ..Default::default() });
        assert!(f.poly_exec);
        f.apply_env(&HashMap::from([(POLY_EXEC_ENV.to_string(), "0".to_string())])).unwrap();
        assert!(!f.poly_exec);
    }

    #[test]
    fn a_truthy_typo_is_an_error_not_a_silent_false() {
        let env = HashMap::from([(RECONCILE_ENV.to_string(), "true".to_string())]);
        let err = Flags::default().apply_env(&env).unwrap_err();
        assert!(err.to_string().starts_with("VIKE_RECONCILE=true: "), "{err}");
    }

    /// The kill-switch exception, pinned in every spelling that halts today. Under `parse_flag`
    /// three of these four would stop halting (`"0"` and `""` would read false, `"anything"` would
    /// be an error) — which is why this is a test and not a comment.
    #[test]
    fn the_redeem_kill_switch_is_armed_by_presence_not_by_a_value() {
        for value in ["1", "0", "", "anything"] {
            let env = HashMap::from([(POLY_REDEEM_HALT_ENV.to_string(), value.to_string())]);
            let mut f = Flags::default();
            f.apply_env(&env).unwrap();
            assert!(f.poly_redeem_halt, "POLY_REDEEM_HALT={value:?} must halt");
        }
        // Absent is the only way it stays un-halted, and no env value can UN-halt it.
        let mut f = Flags { poly_redeem_halt: true, ..Default::default() };
        f.apply_env(&HashMap::from([(POLY_REDEEM_HALT_ENV.to_string(), "0".to_string())])).unwrap();
        assert!(f.poly_redeem_halt, "no env value un-halts the kill switch");
    }

    #[test]
    fn flag_meta_resolves_a_field_and_rejects_an_unknown_one() {
        assert_eq!(flag_meta("reconcile").map(|m| m.env), Some(RECONCILE_ENV));
        assert_eq!(flag_meta("no_such_flag"), None);
    }
}
