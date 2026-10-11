//! [`Flags`] — operator TOGGLES, each with an OWNER and a REVIEW DATE. Each one is a `flags.<field>`
//! row of the settings database, and nothing else.
//!
//! A flag is a thing an operator turns on for a box: enable reconciliation, mount live execution,
//! open a control surface, start a recorder. The blast radius of a wrong value is a feature that is
//! on or off, not a risk limit that is wrong — which is why a flag may also be set from the command
//! line where a binary offers one — but no flag is read from the process environment
//! (`docs/decisions/0111-no-setting-lives-in-the-environment-or-a-toml-file.md`): each field's
//! former variable ([`FlagMeta::env`]) is a [`crate::REMOVED_ENV`] row that refuses startup and
//! prints the `vike-cli config set flags.<field> …` line that replaces it.
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
//! it drives every row through the real row layer, when a row key is wired to the WRONG field. So
//! an added flag without an owner and a review date fails CI; it does not rely on someone noticing
//! in review.
//!
//! ⚠ The gate deliberately does **not** fail once a review date has passed. That would turn the
//! calendar into a merge blocker on unrelated PRs, and a settings crate must not be able to stop a
//! trading fix from shipping. Making dates bite is a one-line change in that test (`review` parsed
//! and compared against today) and a deliberate future decision, not an oversight.
//!
//! ## A typo is an error
//!
//! A row is a typed TOML boolean, so `true`/`false` are the only values it can hold and an unknown
//! `flags.*` key is refused by name ([`FlagsPatch`]'s `deny_unknown_fields`). The old variables'
//! exact-`"1"` grammar survives only in the refusal: [`crate::REMOVED_ENV`] maps a set variable's
//! `1` to the `true` it meant (`crate::ValueMap::Switch`), so the line it prints writes what the
//! variable did.
//!
//! ## Six flags reach a venue adapter through the credential map
//!
//! A venue adapter several frames below the binary reads its gate out of the map the mount threads
//! (`vike_mount::NodeConfig`'s), not out of [`Flags`]. So the daemon FOLDS six resolved flags into
//! that map under their former variable names — `flags.poly_exec`, `flags.poly_reconcile`,
//! `flags.hyperliquid_hip3`, `flags.record_properties`, `flags.allow_withdraw_keys` and
//! `flags.preflight_skip` (`vike_tradehub`'s `fold_flags_into_vars` is the one fold). The variable
//! of the same name refuses startup, so the map's value is the row's and nothing else. The five
//! settlement-poller flags (`poly_heartbeat`, `poly_auto_redeem`, `poly_redeem_halt`, `pm_resolve`,
//! `hl_outcome`) are neither folded nor read — no composition root starts the pollers
//! ([`crate::CONSUMPTION`] says so per row).
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
//! places that DISAGREE would be worse than the state this program is fixing.
//!
//! ⚠ **`Consumer::Not` says WHY nothing reads a row.** A `Not` row carries a `vike_config::Reader`:
//! for [`Flags::hl_outcome`], [`Flags::pm_resolve`], [`Flags::poly_auto_redeem`],
//! [`Flags::poly_heartbeat`], [`Flags::poly_redeem_halt`] and [`Flags::record_chains`] **the row
//! configures nothing** — the code they are for sits inside a poller or a recorder constructor no
//! composition root ever builds, and each takes its value as a parameter. Their fields say so
//! individually, and [`crate::CONSUMPTION`] is the gated statement; what re-arms one is a root that
//! MOUNTS the feature, which is a decision rather than a threading task.
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
use std::path::Path;

use crate::error::ConfigError;
use crate::layers::{CliOverride, CliOverrides};

mod registry;
mod retired;

pub use self::registry::{Disposition, FLAG_REGISTRY, FlagMeta, flag_meta};
pub use self::retired::{DEAD_FLAG_KEYS, REMOVED_FLAG_KEYS};
use self::retired::{dead_flag_key, removed_flag_key};

// -------------------------------------------------------------------------------------------
// The flags' former variable names
//
// Each is the variable a `flags.*` row replaced: a [`crate::REMOVED_ENV`] row refuses it at
// startup (decision 0111), [`FlagMeta::env`] names it, and six of them are also the key the daemon
// folds the resolved flag into the credential map under (the module doc's "Six flags"). Every
// literal below carries a row in `vike_ops::settings::SETTINGS`; that gate harvests env-shaped
// string literals from the whole `crates/` tree and fails on an undeclared one.
// -------------------------------------------------------------------------------------------

/// `VIKE_RECONCILE` — the retired variable of [`Flags::reconcile`].
pub const RECONCILE_ENV: &str = "VIKE_RECONCILE";
/// `VIKE_RECONCILE_OFF` — the retired variable of [`Flags::reconcile_off`].
pub const RECONCILE_OFF_ENV: &str = "VIKE_RECONCILE_OFF";
/// `VIKE_RECONCILE_RESTORE_OFF` — the retired variable of [`Flags::reconcile_restore_off`]. No
/// release ever read it: the name exists so a box that sets it is refused at startup with the row
/// that is the switch, instead of believing an environment variable turned restore off.
pub const RECONCILE_RESTORE_OFF_ENV: &str = "VIKE_RECONCILE_RESTORE_OFF";

/// The warning [`fn@crate::load`] raises when a deployment WROTE a refusal that S2 no longer honours:
/// `reconcile = false` in the settings database (section `flags`), or `--reconcile false` on the
/// command line. `origin` is where it was written, in the operator's own vocabulary.
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
         `flags.reconcile` is now a FORCE-ON and an explicit `false` reads the same as unset. The \
         switch you want is {}. See docs/ops/reconcile-on-restart.md",
        remedy.inline_write("true")
    )
}
/// `VIKE_RECONCILE_GENERATE_MISSING` — the retired variable of
/// [`Flags::reconcile_generate_missing`].
pub const RECONCILE_GENERATE_MISSING_ENV: &str = "VIKE_RECONCILE_GENERATE_MISSING";
/// `VIKE_RECONCILE_BALANCE` — the retired variable of [`Flags::reconcile_balance`].
pub const RECONCILE_BALANCE_ENV: &str = "VIKE_RECONCILE_BALANCE";
/// `VIKE_OCO_CANCEL_SIBLING_ON_DEAD_EXIT` — the retired variable of
/// [`Flags::oco_cancel_sibling_on_dead_exit`].
pub const OCO_CANCEL_SIBLING_ON_DEAD_EXIT_ENV: &str = "VIKE_OCO_CANCEL_SIBLING_ON_DEAD_EXIT";

/// `VIKE_TRADEHUB_LIVE` — the retired variable of [`Flags::tradehub_live`].
pub const TRADEHUB_LIVE_ENV: &str = "VIKE_TRADEHUB_LIVE";
/// `VIKE_TRADEHUB_CONTROL` — the retired variable of [`Flags::tradehub_control`].
pub const TRADEHUB_CONTROL_ENV: &str = "VIKE_TRADEHUB_CONTROL";
/// `VIKE_TRADEHUB_ALLOW_PUBLIC_BIND` — the retired variable of
/// [`Flags::tradehub_allow_public_bind`].
pub const TRADEHUB_ALLOW_PUBLIC_BIND_ENV: &str = "VIKE_TRADEHUB_ALLOW_PUBLIC_BIND";
/// `VIKE_TELEGRAM_CONTROL` — the retired variable of [`Flags::telegram_control`].
pub const TELEGRAM_CONTROL_ENV: &str = "VIKE_TELEGRAM_CONTROL";
/// `VIKE_CANCEL_ORDERS_ON_SHUTDOWN` — the retired variable of [`Flags::cancel_orders_on_shutdown`].
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

/// The name the daemon folds `flags.record_properties` into the credential map under; no
/// environment variable of this name is read (decision 0111) — a set one refuses startup.
pub const RECORD_PROPERTIES_ENV: &str = "VIKE_RECORD_PROPERTIES";
/// `VIKE_RECORD_CHAINS` — the retired variable of [`Flags::record_chains`].
pub const RECORD_CHAINS_ENV: &str = "VIKE_RECORD_CHAINS";
/// `VIKE_RECORD_DVOL` — the variable of the deleted `record_dvol` flag ([`DEAD_FLAG_KEYS`]); a set
/// one refuses startup.
pub const RECORD_DVOL_ENV: &str = "VIKE_RECORD_DVOL";

/// `VIKE_DATAHUB_VENUE_CATALOG_OFF` — the retired variable of [`Flags::venue_catalog_off`].
pub const VENUE_CATALOG_OFF_ENV: &str = "VIKE_DATAHUB_VENUE_CATALOG_OFF";

/// `VIKE_DATAHUB_LIVE` — the retired variable of [`Flags::datahub_live`].
pub const DATAHUB_LIVE_ENV: &str = "VIKE_DATAHUB_LIVE";
/// `VIKE_DATAHUB_CHART_SEED` — the retired variable of [`Flags::datahub_chart_seed`].
pub const DATAHUB_CHART_SEED_ENV: &str = "VIKE_DATAHUB_CHART_SEED";
/// `VIKE_DATAHUB_ALLOW_PUBLIC_BIND` — the retired variable of [`Flags::datahub_allow_public_bind`].
pub const DATAHUB_ALLOW_PUBLIC_BIND_ENV: &str = "VIKE_DATAHUB_ALLOW_PUBLIC_BIND";

/// The name the daemon folds `flags.allow_withdraw_keys` into the credential map under (the binance
/// withdraw gate reads it there); no environment variable of this name is read (decision 0095) — a
/// set one refuses startup.
pub const ALLOW_WITHDRAW_KEYS_ENV: &str = "VIKE_ALLOW_WITHDRAW_KEYS";
/// The name the daemon folds `flags.preflight_skip` into the credential map under (the mount's
/// startup preflight reads it there); no environment variable of this name is read (decision 0111)
/// — a set one refuses startup.
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
    /// policy). The rest of the reconcile settings are CONFIG, not flags — cadences, lookbacks and
    /// the policy name — and live in [`crate::Config`]'s `reconcile_*` rows.
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
    /// resolved value is a `bool`, a `flags.reconcile` row set to `false` is indistinguishable from
    /// unset at `reconcile_gate` — so a deployment that wrote it meaning "do not reconcile" now
    /// reconciles.
    /// [`reconcile_refusal_ignored`] is the one-line warning `crate::load` raises for that,
    /// naming the origin and pointing at [`Flags::reconcile_off`], because a default change that
    /// silently overrides a written setting hands the operator positive confirmation of something
    /// false.
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
    /// IS typing an address — no address can be its own consent. Setting it cannot enlarge an
    /// order, only widen reachability, and the daemon then `warn!`s about that on every start.
    ///
    /// ⚠ **A container consents with an ARGUMENT, not this row.** Docker publishes a port to the
    /// container's `eth0`, so every containerised daemon binds wide; the image's entrypoint passes
    /// `vike-backend trade --allow-public-bind`, which the daemon ORs with this row. A row written
    /// by the entrypoint would persist "public bind OK" in the operator's own database and follow
    /// it to a box that is not a container.
    ///
    /// Was `VIKE_TRADEHUB_ALLOW_PUBLIC_BIND`. Owner `@AlexSokhanych` · review 2026-11-01 · KEEP —
    /// reachability belongs to the deployment and stays a deliberate per-run opt-in.
    pub tradehub_allow_public_bind: bool,

    /// Enable the Telegram control channel. ⚠ Sits UNDER a Cargo feature as well — a default build
    /// does not compile the module at all — and under [`Flags::tradehub_control`]: the live gate
    /// is the AND of both rows, which this type deliberately does not encode. Composition is
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
    /// under `config.reconcile_policy = quarantine`.
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
    /// ⚠ **UNMOUNTED: setting this changes nothing.** `vike_data::ChainRecorder::open`, the one
    /// constructor that takes this flag as its `enabled` parameter, has no call site in the tree:
    /// `crates/vike-desktop/src/main.rs` binds its recorder to `None`. ⚠ **The mechanism is the
    /// STORE, not the fetch.** The desktop still calls `vike_app_core::tools::spawn_tool_fetchers`
    /// and still polls chains every 30 s, but it links vike-data with DEFAULT features (the
    /// trait-only `HistStore` seam) and `open` is a `hist-datafusion` constructor, so there is no
    /// engine to open. Re-arming this is a root that can open a STORE beside the fetch it already
    /// has; the recorder's cadence is then that root's parameter too. The recorder and its
    /// `kind=chain` series are intact. [`crate::CONSUMPTION`]'s row is the gated authority.
    ///
    /// Was `VIKE_RECORD_CHAINS`. Owner `@AlexSokhanych` · review 2027-02-01 · GRADUATE.
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
    /// that last one deactivating the account (`vike-cli secrets account deactivate --id <N>`) is
    /// the better answer, because it also refuses the exec session.
    ///
    /// Beats [`Flags::reconcile`] when both are set (`vike_tradehub::reconcile_config::reconcile_gate`
    /// checks the refusal first): a stale `flags.reconcile` row must not overrule the person typing
    /// the override.
    ///
    /// Was never an environment variable before S2. Owner `@AlexSokhanych` · review 2027-02-01 ·
    /// KEEP — a refusal for a default-on safety behaviour is a standing operator hatch, not a
    /// migration.
    pub reconcile_off: bool,

    /// Refuse the restore of previous-session orders into the engine registry. After a restart the
    /// FIRST reconcile pass puts the orders a previous session left resting back into the registry
    /// (their strategy tags and the venue-order rows), so a mount sees the order it placed before
    /// the restart; `true` is the operator saying "do not put them back": ownership and fills
    /// still restore as before and nothing else (`vike_core::CoreConfig::restore_orders_off`
    /// carries the meaning). `false` (the default) restores.
    ///
    /// It exists for [`Flags::reconcile_off`]'s reason: the restore is a DEFAULT-ON behaviour, so
    /// its refusal is the flag (this type's fields cannot default to `true`). It has an effect only
    /// where a reconcile pass runs, so it is independent of [`Flags::reconcile_off`], which stops
    /// the passes themselves.
    ///
    /// Restart-required (the core is built once). Owner `@AlexSokhanych` · review 2027-02-01 ·
    /// KEEP — a refusal of a default-on behaviour is a standing operator hatch.
    pub reconcile_restore_off: bool,

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
    /// Was `VIKE_DATAHUB_VENUE_CATALOG=1` inverted (that variable refuses startup now). Owner
    /// `@AlexSokhanych` · review 2027-02-01 · KEEP — like [`Flags::reconcile_off`], a refusal for a
    /// default-on behaviour is a standing operator hatch and should live exactly as long as the
    /// default does.
    pub venue_catalog_off: bool,

    // --- the data and compute daemons -------------------------------------------------------
    // The data and compute daemons read these three rows after their boot (decision 0111); each
    // former variable refuses startup.
    /// Arm `vike-datahub`'s live market-data plane: this process becomes the single subscriber to
    /// each venue it serves, spending that venue's API budget from this box's IP. Off (the default)
    /// is byte-identical to a build without the plane.
    ///
    /// Was `VIKE_DATAHUB_LIVE`. Owner `@AlexSokhanych` · review
    /// 2027-02-01 · KEEP — which planes a data server runs is a per-box decision.
    pub datahub_live: bool,

    /// Arm `vike-datahub`'s chart-gap seed lane: an Observe-scope client may have the server fetch
    /// one bounded kline window per series into its store. Needs a build that mounts a collector
    /// table; armed on one that does not, it warns once and arms nothing.
    /// `docs/decisions/0058-a-chart-gap-fetch-is-an-observe-verb.md` puts defaulting it ON in its
    /// reopen list.
    ///
    /// Was `VIKE_DATAHUB_CHART_SEED`. Owner `@AlexSokhanych` · review
    /// 2027-02-01 · KEEP.
    pub datahub_chart_seed: bool,

    /// ⚠ SAFETY OVERRIDE. Let `vike-datahub` and the compute daemon (`backtest --addr`) bind a
    /// NON-LOOPBACK address. `false` is the guarded state, and even `true` is refused on a server
    /// with no node keys: consent to being REACHABLE is not consent to serving unauthenticated.
    /// One flag for both daemons on purpose — it answers one posture question for one box.
    ///
    /// Was `VIKE_DATAHUB_ALLOW_PUBLIC_BIND`. Owner `@AlexSokhanych` ·
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
    /// See [`Flags::reconcile_restore_off`].
    pub reconcile_restore_off: Option<bool>,
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
        if let Some(v) = patch.reconcile_restore_off {
            self.reconcile_restore_off = v;
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

// ⚠ `Flags::apply_env`, the environment layer, is DELETED (decision 0111): every flag is its
// `flags.*` row, and each former variable is a `crate::REMOVED_ENV` refusal.

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
