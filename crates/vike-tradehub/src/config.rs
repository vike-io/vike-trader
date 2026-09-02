//! `config` — the `vike-tradehub` daemon PROFILE (headless two-layer plan, Layer 2, PR-9).
//!
//! ONE reviewable TOML file replaces a pile of env vars (LEAN "environments", steal S8): it names
//! the mount (venue / `symbol` / interval), an optional `[strategy]` table naming WHICH strategy to
//! run, and the daemon settings (snapshot-summary cadence, bounded-shutdown deadline).
//!
//! ## The `[strategy]` table — the same vocabulary a backtest profile uses
//! `[strategy] name = "…"` + `[strategy.params]` is deliberately the SAME shape
//! `vike_backtest::harness::BacktestProfile` has always had, resolved through the SAME registry
//! (`vike_strategy::strategy_by_name`, which is generic over the broker precisely so the simulator
//! and this daemon share it). So the profile that was backtested is the profile that trades.
//!
//! `[strategy] rhai = "…"` (mutually exclusive with `name`) instead mounts a Rhai SCRIPT —
//! `vike_script::RhaiStrategy` at the same `LiveBroker`, the script named EXPLICITLY by path in
//! this reviewable file, its compiled source's sha256 logged at INFO as the audit trail. The
//! reversal that made scripts live-mountable, its rails, and the Python/JS class refusal are
//! `docs/decisions/0024-rhai-strategies-live.md`'s; the mechanics are
//! [`DaemonProfile::resolve_script`]'s.
//!
//! **ABSENT `[strategy]` is the historical behaviour, byte-identically**: the daemon mounts the
//! Avellaneda–Stoikov [`vike_mm::SpreadMaker`] built from this profile's own maker fields
//! (`qty`/`half_spread`/`tick_size`/`resolution_ts_ms`) via [`vike_run::build_maker`] — the same
//! function [`vike_run::build_paper_maker_core`] has always called. The A-S maker is now ONE
//! mountable strategy rather than the hardcoded one; it is still the DEFAULT one.
//!
//! ## Scope — the mount config, PAPER or LIVE
//! This profile lowers into the [`vike_run::MakerMountConfig`] that BOTH the paper mount
//! ([`vike_run::build_paper_maker_core`], the default) and the opt-in LIVE `build_node` mount
//! ([`vike_run::build_live_maker_core`], under `VIKE_TRADEHUB_LIVE=1`) build from, and — for a
//! `[strategy]` profile — into that config's strategy-free [`vike_run::MountSpec`] projection. The
//! daemon's LIVE path additionally calls [`DaemonProfile::validate_for_live`] to reject a venue it
//! has not wired for live and a symbol its engine would silently drop. The auth'd network control
//! server is PR-10/11/12. Existing env gates the mount already honors (`VIKE_JOURNAL_DIR` /
//! `VIKE_PIN_CORES` / …, read INSIDE the builders) keep their meaning — this profile deliberately
//! does not re-parse them.

use std::time::Duration;

use serde::Deserialize;
use vike_core::{ProfileError, RunProfile};
use vike_exec::RiskLimits;
use vike_model::SpreadModel;
use vike_run::{MakerMountConfig, MountSpec, SpreadMaker};
use vike_strategy::{Capability, ParamKeys};

/// The registry names that mean "the Avellaneda–Stoikov maker this profile already describes".
///
/// ⚠ These two do NOT resolve through `vike_strategy::strategy_by_name` on this daemon, and that is
/// the whole point. The registry arm is `SpreadMaker::from_params(params)` — a reader over
/// `[strategy.params]` and NOTHING else — so naming the maker would have mounted a MATERIALLY
/// DIFFERENT maker from the one the same profile mounts with no `[strategy]` table: `qty = 1`,
/// `tick_size = 0`, and the `[0,1]` Bernoulli wall clamp instead of the venue-selected `AsParams`
/// that [`DaemonProfile::to_mount_config`] picks. On a hyperliquid mount that is the exact
/// configuration `main.rs`'s module doc records as having posted ZERO orders before
/// `vike_run::MakerMountConfig::crypto` existed — a daemon that starts, logs
/// `strategy = spread_maker`, and never quotes. On polymarket it silently drops `resolution_ts_ms`,
/// the anchor every settlement feature keys off.
///
/// So [`DaemonProfile::mounted_maker`] is the ONE construction site for both spellings, and
/// `DaemonProfile::validate` REFUSES a `[strategy.params]` table under either name rather than
/// dropping it. The two spellings cannot differ, by construction; an attempt to make them differ is
/// a startup error. `the_two_spellings_of_the_as_maker_are_one_construction` and
/// `the_maker_names_refuse_a_params_table` are the two halves of that gate, and
/// `every_not_enumerated_registry_row_is_refused_here` is what stops a future
/// `ParamKeys::NotEnumerated` row appearing with no daemon-side rule behind it.
pub const AS_MAKER_NAMES: &[&str] = &["spread_maker", "gueant_maker"];

/// What [`DaemonProfile::resolve_mount`] resolved, and — the point — WHICH construction produced it.
///
/// Both variants are the same thing to the mount builders (a `Strategy<LiveBroker> + Send`), so the
/// daemon boxes them and forgets the difference one line later. The difference is stated HERE
/// because it was invisible where it mattered: for the two [`AS_MAKER_NAMES`], the registry arm and
/// this profile's own lowering produce materially different makers, and an opaque box from both
/// paths meant nothing downstream — and no test — could tell which one was mounted.
///
/// The maker is boxed too, not for indirection but because it is a far larger value than a `Box`
/// (`clippy::large_enum_variant`), and a lopsided enum here would be paid on every resolve.
pub enum MountedStrategy {
    /// The Avellaneda–Stoikov maker, built from THIS profile's own maker fields through
    /// `vike_run::build_maker` — the same call the absent-`[strategy]` default path makes.
    AsMaker(Box<SpreadMaker>),
    /// Anything else named by `name`, resolved through the shared `vike_strategy` registry at
    /// [`vike_core::LiveBroker`] — the same table a backtest profile resolves through.
    Registered(Box<dyn vike_model::Strategy<vike_core::LiveBroker> + Send>),
    /// A Rhai script named by `rhai = "<path>"`, compiled by [`DaemonProfile::resolve_script`]
    /// at the SAME [`vike_core::LiveBroker`] (`docs/decisions/0024-rhai-strategies-live.md`).
    ///
    /// Carries the audit pair alongside the box — `path` as the profile spelled it and `sha256`
    /// of the source that was ACTUALLY read and compiled — so the "which code traded" claim the
    /// resolve logs at INFO is also assertable by a test, without a log subscriber.
    Script {
        /// The compiled script, boxed exactly as a registry strategy is.
        strategy: Box<dyn vike_model::Strategy<vike_core::LiveBroker> + Send>,
        /// The script path as the profile named it.
        path: String,
        /// Lowercase-hex sha256 of the compiled source ([`script_sha256`]).
        sha256: String,
    },
}

/// The venues this daemon can mount LIVE: the ones `main.rs`'s `live_mount` has a market-feed arm
/// for (since split-plane I10 the dispatch lives in its `venue_feed_plan` helper, run once per
/// `[[mounts]]` row — same file, same arms).
/// It is a VENUE list, not a `(venue, symbol)` allow-list — the symbol question is a separate,
/// DERIVED check (see [`DaemonProfile::validate_for_live`]), because "which venues did somebody wire
/// a feed for" and "which symbol does that venue's engine accept" are different facts with different
/// authorities, and conflating them is what made this gate a hand-copied pair table.
///
/// Extending it is a deliberate two-place edit: a row HERE and the matching `live_mount` arm.
/// `crates/vike-tradehub/tests/daemon/live_wired_venues_pin.rs` is what makes that true — it scans
/// `main.rs`'s `live_mount` for its actual venue arms and fails on ANY difference, in either
/// direction, under this build's features.
///
/// ⚠ It exists because this doc used to cite `live_wired_venues_are_all_mounted_by_build_node` for
/// that claim, and that test checks something else entirely: `vike_run::WIRED_MARKETS`, which
/// CONTAINS binance, bybit, okx and seven more venues `live_mount` has no arm for. Adding `"binance"`
/// to this list was MEASURED green across the whole vike-tradehub suite. The previous hardcoded
/// `(venue, symbol)` pair form forced a match-arm edit that the completeness test caught; a one-line
/// const does not, which is the "declaration-pinning tests don't gate" failure mode this repo has
/// already been bitten by three times. `live_mount`'s own `v => return Err(…)` catch-all still makes
/// a widened row a loud startup failure rather than a silent live mount — but "loud at 3am" is not
/// the gate this comment promised.
///
/// ## ⚠ fxcm is DELIBERATELY absent, and it is the one venue that can never join
///
/// `vike_run::WIRED_MARKETS` grew an fxcm row and `build_node` an fxcm `make_engine` arm (both
/// behind the `fxcm` feature), so an fxcm-linked daemon now mounts a real FXCM execution engine and
/// reconcile client. That does NOT make it mountable as a PROFILE venue, because this list is about
/// the other half: which venues `live_mount` can build a market FEED for. `vike-fxcm` has no
/// market-data seam of any kind — `LiveDataCaps::NONE` in its caps row, `NoPump` in
/// `vike_bridge_core::pump_spec` — so there is no arm to write, and
/// `crates/vike-tradehub/tests/daemon/live_wired_venues_pin.rs` (this list == `live_mount`'s actual
/// feed arms) would go red on a row added here alone. A profile naming `venue = "fxcm"` is
/// therefore refused at step (1) of [`DaemonProfile::validate_for_live`], which is correct: a mount
/// with no prices is a strategy that never gets a tick, and arming it would be arming nothing.
///
/// ⚠ `docs/ops/fxcm-forexconnect-the CI box.md` used to describe the remaining wiring step as "a row in
/// `WIRED_MARKETS`, a `build_node` arm, and the matching entry in this allow-list". The first two
/// are right; the third is not, and following it would have demanded a feed arm that cannot exist.
/// The engine reached that way is reachable by ROUTING (a `vike_core::MountLeg::at` naming fxcm
/// from a mount on a venue that does have prices), not by being the profile's own venue.
pub const LIVE_WIRED_VENUES: &[&str] = &[
    // ⚠ CREDENTIALED MARKET DATA (split-plane I9): alpaca's feed is a real `DataClient` but it
    // AUTHENTICATES (the OAuth2 SANDBOX client-credentials trio) — there is no keyless price
    // stream to fall back to, so `venue_feed_plan`'s alpaca arm REFUSES the live mount outright
    // when the trio is absent rather than mounting a feed-less core that quotes into the void.
    // Exec stays SANDBOX-tier-pinned in `vike_mount::make_engine` (no `ALPACA_MAINNET` flag; a
    // LIVE-tier flip is a deliberate code change, not a config flip).
    "alpaca",
    // ⚠ REAL MONEY IN PRACTICE: aster's exec tier is credential-resolved MAINNET-FIRST
    // (`vike_mount::make_engine`'s `("aster", _)` arm prefers `ASTER_LIVE_*`, and only the LIVE
    // tier is configured in practice — a testnet exists, the constraint is credentials; root
    // CLAUDE.md + `crates/bridges/aster/CLAUDE.md`). This row is CAPABILITY only: absent
    // credentials still mount paper, exactly like every other venue.
    "aster",
    "binance",
    "bybit",
    // ⚠ CREDENTIALED MARKET DATA + a SYNCHRONOUS data handshake (split-plane I9): ctrader's feed
    // is `CtraderData` over its own dedicated protobuf socket, DEMO-tier credentials required
    // (`CtraderConfig::from_vars(Demo, …)` — absent creds refuse the live mount, same argument as
    // alpaca above), and `conn::connect_and_auth` connects AT MOUNT — a data connect failure
    // refuses daemon startup, unlike the exec side's session-long demote-to-paper.
    "ctrader",
    // ⚠ KEYLESS MARKET DATA on a venue whose two halves point at DIFFERENT NETWORKS (split-plane
    // I9): deribit's feed is `market_feed::Feeds`, four `DataClient` verbs off the venue's PUBLIC
    // MAINNET host with no credentials at all — so absent creds mount paper here exactly like the
    // CEX venues, never a startup refusal. Exec is the other half: `vike_mount::make_engine`
    // resolves the DEMO key tier (`cex_mainnet_enabled` has no deribit arm, so the flag venues'
    // LIVE branch is unreachable) and every authed socket the bridge opens is hardcoded TESTNET
    // (`crates/bridges/deribit/CLAUDE.md`'s two-networks section) — there is no path to deribit
    // MAINNET exec in this tree at all. So a live deribit mount is testnet exec over MAINNET
    // prices, the CEX arms' shape reached structurally rather than by choice.
    "deribit",
    "hyperliquid",
    // ⚠ CREDENTIALED MARKET DATA over Lightstreamer TLCP, and TWO VERBS ONLY (split-plane I9):
    // ig's feed is `market_feed::Feeds`, which serves `subscribe_quotes` + `subscribe_bars` and
    // refuses trades/book/depth through the declared caps row — IG is a DEALER venue publishing
    // its own two-sided price, so a public tape and an L2 ladder do not exist to be wired. Every
    // subscription logs in with the exec side's own `IgConfig`, so absent credentials refuse the
    // live mount (the alpaca/ctrader/oanda argument). Exec is DEMO-gateway-pinned in
    // `vike_mount::make_engine` (`load_ig_config_from(Demo, …)`; `ig_rest_base` has a live-gateway
    // arm with NO caller in the workspace, so `IG_LIVE_*` in the store configures nothing and the
    // venue silently stays paper — `crates/bridges/ig/CLAUDE.md` records that trap) — and the feed
    // resolves that same tier, so this venue is demo exec over demo prices end to end.
    "ig",
    // ⚠ CREDENTIALED MARKET DATA on BOTH lanes, and NO market-data WS at all (split-plane I9):
    // oanda's feed is `market_feed::Feeds`, a `pump_spec` `OwnPump` — quotes ride a chunked-HTTP
    // `/pricing/stream` line stream and bars POLL the candles REST endpoint, both Bearer-authed,
    // so absent credentials refuse the live mount (the alpaca/ctrader argument). Exec is
    // PRACTICE-tier-pinned in `vike_mount::make_engine` (`load_oanda_config_from(Demo, …)`, no
    // `OANDA_MAINNET` flag; the fxTrade tier is implemented but has no caller, so a live flip is
    // a deliberate code change) — and the feed resolves that same tier, so this venue is practice
    // exec over practice prices end to end.
    "oanda",
    "okx",
    #[cfg(feature = "polymarket")]
    "polymarket",
];

/// The venues a profile may declare `data_only = true` for — the CREDENTIALED-MARKET-DATA subset
/// of [`LIVE_WIRED_VENUES`] (split-plane I9's alpaca/ctrader/oanda/ig rows above, i.e. exactly the
/// [`crate::VenuePlan`] variants that CARRY a resolved config). The declaration exists for
/// these venues alone because only here does the workspace live gate ("absent credentials ⇒
/// paper") fail to provide a data-only mount: their feed AUTHENTICATES with the SAME credentials
/// `vike_mount::make_engine`'s exec arm reads, so a store that lets the feed mount also arms real
/// demo exec. `data_only = true` is the declared exception — `live_mount` WITHHOLDS that venue's
/// credentials from the exec mount (the feed keeps them; its plan resolved first), so exec rides
/// the ordinary absent-credentials paper fallback while the credentialed feed streams.
///
/// A KEYLESS-data venue is refused the declaration on purpose, not spared the implementation: its
/// data-only mount already exists — withhold the venue's credentials from the store — and a
/// declaration there could only re-state the live gate while making the arming disclosure
/// (`cex_arming`'s "add {keys}" remedy) advise arming a mount the profile just said not to arm.
pub const DATA_ONLY_VENUES: &[&str] = &["alpaca", "ctrader", "ig", "oanda"];

/// What `name`'s params reader accepts, as `key (type)` pairs — the "here is what you CAN set" tail
/// of both params refusals.
///
/// The TYPE is on it because both refusals are now about the same table and an operator hitting the
/// second one (`size = "2"`) needs the type more than the name. Empty for a name with no enumerated
/// key set, which on this daemon is only the maker aliases — and those never reach here, because
/// they are refused a params table outright one branch up.
fn readable_keys(name: &str) -> String {
    match vike_strategy::param_keys(name) {
        Some(ParamKeys::Declared(k)) => k
            .iter()
            .map(|(key, ty)| format!("{key} ({})", ty.expected()))
            .collect::<Vec<_>>()
            .join(", "),
        _ => String::new(),
    }
}

/// Strategy selection: a registry name OR a Rhai script path, plus arbitrary TOML params the
/// strategy constructor interprets itself. The `name` half is DELIBERATELY the same shape as
/// `vike_backtest::harness::StrategyCfg`, so a profile that was backtested can be traded by
/// copying the table across.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyCfg {
    /// A [`vike_strategy::PORTABLE_STRATEGIES`] name. Validated at profile LOAD, not at mount:
    /// a typo, a simulator-only strategy and a strategy that cannot trade live all fail here with
    /// their own message rather than mounting something that never trades.
    ///
    /// Exactly one of `name` / [`Self::rhai`] must be set — the same mutual-exclusion shape as
    /// `symbol`/`token_id`, refused by [`DaemonProfile::validate`] with its own message.
    #[serde(default)]
    pub name: Option<String>,
    /// Path to a Rhai SCRIPT to mount as the strategy (`vike_script::RhaiStrategy`, at the SAME
    /// `vike_core::LiveBroker` every registry strategy mounts at) — the daemon spelling of the
    /// backtest registry's inline-`src` `rhai` arm, per `docs/decisions/0024-rhai-strategies-live.md`.
    ///
    /// The script is named EXPLICITLY here, in the reviewable profile — never auto-discovered from
    /// a folder. Absolute, or relative to the daemon's working directory (every shipped unit runs
    /// `WorkingDirectory=<project>`, so a relative path is project-relative in the deployed shape).
    /// The file is read at RESOLVE time, not at load: [`DaemonProfile::validate`] is pure over the
    /// TOML text (the docs-profiles CI gate parses profiles on runners that have no script tree),
    /// so a missing/unreadable file or a compile error fails at [`DaemonProfile::resolve_mount`] —
    /// still before any core spawns. At resolve the mounted source's sha256 + path are logged at
    /// INFO (the audit trail for WHICH code traded — see [`MountedStrategy::Script`]).
    #[serde(default)]
    pub rhai: Option<String>,
    /// The strategy's own knobs. Free-form on the wire (each registry strategy reads its own), and
    /// **not** free-form in effect: `DaemonProfile::validate_strategy` refuses any key the named
    /// strategy does not read, because a params reader ignores what it does not recognise and the
    /// knob then mounts at its compiled default — on this daemon, a live order at a size nobody
    /// typed. It refuses a declared key carrying an unusable VALUE for the same reason, and a key
    /// that names a market this mount does not trade (a params `symbol`/`venue` the live core
    /// overrides) for a sharper one. Under an [`AS_MAKER_NAMES`] name the whole table is refused;
    /// the maker takes its configuration from the profile's own top-level maker fields.
    ///
    /// Under [`Self::rhai`] every key is a `param(name, default)` OVERRIDE baked into the script
    /// scope at compile — the same knobs a backtest `[sweep]` grids over. The same
    /// no-silently-ignored-key posture applies, in the script's own vocabulary:
    /// [`DaemonProfile::validate`] refuses a non-numeric value (`param` takes an `f64`, so a quoted
    /// number configures nothing), and [`DaemonProfile::resolve_mount`] refuses a key the script's
    /// own top level never passes to `param(…)` (`vike_script::discover_params`).
    #[serde(default = "default_params")]
    pub params: toml::Value,
}

fn default_params() -> toml::Value {
    toml::Value::Table(Default::default())
}

/// Lowercase-hex sha256 of a Rhai script's SOURCE TEXT — the pure half of the mount audit line
/// (`DaemonProfile::resolve_script` logs it at INFO beside the path). The hash is of the bytes
/// that were actually read and compiled, so the journal can answer "which code traded" even after
/// the file at that path has been edited. `sha2` here is the workspace's ONE SHA-256 (the same
/// crate the venue signers link), not a new hash dependency.
pub fn script_sha256(source: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(source.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// A Rhai profile's `[strategy.params]` as the `param(name, default)` override map
/// [`vike_script::RhaiStrategy::compile_with_params`] bakes into the script scope — every NUMERIC
/// key (TOML float or integer, the workspace's lenient numeric convention). The same collection
/// the backtest registry's `rhai` arm builds, minus its reserved `src` key, which does not exist
/// in this spelling (the script arrives by PATH). Non-numeric values never reach here:
/// `DaemonProfile::validate_script` refused them at load.
fn script_overrides(params: &toml::Value) -> indexmap::IndexMap<String, f64> {
    params
        .as_table()
        .map(|t| {
            t.iter()
                .filter_map(|(k, v)| {
                    v.as_float()
                        .or_else(|| v.as_integer().map(|i| i as f64))
                        .map(|n| (k.clone(), n))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The whole daemon profile: the mount fields (flattened at the top level), the optional
/// `[strategy]` table, and a `[daemon]` table. Every mount field except the SYMBOL is optional and,
/// when omitted, inherits the [`MakerMountConfig::polymarket`] recommended default — so a minimal
/// profile is just a symbol. `deny_unknown_fields` turns a typo'd key into a parse error instead of
/// a silent no-op.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonProfile {
    /// venue tag (default `"polymarket"`) — also the paper client's + engine's venue. An `Option`
    /// (resolved through [`Self::venue`], the accessor) rather than a serde-defaulted `String`, so
    /// [`Self::validate`] can tell an explicit `venue = "…"` from an omitted one — the difference
    /// between refusing and silently ignoring a top-level venue beside a `[[mounts]]` array.
    #[serde(default)]
    pub venue: Option<String>,
    /// The engine/mount SYMBOL the strategy trades. The GENERAL spelling, and the one to use for a
    /// non-Polymarket venue: a hyperliquid mount names `"BTC"`, not a `token_id`.
    ///
    /// Exactly one of `symbol` / [`Self::token_id`] must be set — see [`Self::mount_symbol`].
    #[serde(default)]
    pub symbol: Option<String>,
    /// The Polymarket spelling of [`Self::symbol`]: an outcome CLOB `token_id`. Kept because it is
    /// what every shipped profile says and this daemon runs a live paper node off one right now —
    /// renaming the key would have been a silent parse failure at the next restart
    /// (`deny_unknown_fields` makes an unknown key fatal). Both spellings mean the same field.
    #[serde(default)]
    pub token_id: Option<String>,
    /// WHICH strategy to mount. Absent ⇒ the Avellaneda–Stoikov maker built from this profile's own
    /// maker fields, byte-identical to every daemon that shipped before this table existed.
    #[serde(default)]
    pub strategy: Option<StrategyCfg>,
    /// A-S time-to-resolution horizon anchor (epoch-ms). `None` ⇒ A-S falls back to its constant
    /// `tau_hold` (see [`MakerMountConfig::polymarket`]).
    #[serde(default)]
    pub resolution_ts_ms: Option<i64>,
    /// mounted bar-series interval (default `"1m"`).
    #[serde(default)]
    pub interval: Option<String>,
    /// synth-bar window in ms (default `60_000`); must match `interval`.
    #[serde(default)]
    pub interval_ms: Option<i64>,
    /// base quote size per side, shares (default `20.0`).
    #[serde(default)]
    pub qty: Option<f64>,
    /// fallback fixed half-spread seed (default `0.01`; unused while A-S prices).
    #[serde(default)]
    pub half_spread: Option<f64>,
    /// venue price grid the A-S L1 lane snaps onto (default `0.01`).
    #[serde(default)]
    pub tick_size: Option<f64>,
    /// paper account seed equity (default `1_000.0`).
    #[serde(default)]
    pub seed_cash: Option<f64>,
    /// The DATA-PLANE-ONLY declaration (the data-only credential seam): `true` tells `live_mount`
    /// to WITHHOLD this venue's credentials from the exec mount, so exec stays on the ordinary
    /// absent-credentials paper fallback while the venue's CREDENTIALED market feed still
    /// authenticates (its plan resolves the credentials BEFORE the withhold). Valid only for the
    /// [`DATA_ONLY_VENUES`] subset — a keyless-data venue is refused at load, because its
    /// data-only mount is "withhold the credentials from the store" and a declaration there could
    /// only mislead the arming disclosure. Absent/`false` (the default) is byte-identical to a
    /// profile without the key: credentials present ⇒ exec arms, the workspace live gate.
    /// Refused when the live gate is OFF (`main.rs`'s paper arm) — the paper daemon mounts no
    /// venue feed, so the key would configure nothing while reading as real.
    #[serde(default)]
    pub data_only: Option<bool>,
    /// **WHICH ACCOUNT of [`Self::venue`] this mount trades on** — a `policy.accounts.<venue>.<LABEL>`
    /// label, e.g. `account = "ALT"`. Absent (every profile that has ever shipped) is the venue's
    /// DEFAULT account: the same engine, the same route key, the same `LIVE-<route_key>.lock`
    /// filename, byte for byte.
    ///
    /// Set it and this mount's orders AND its `Broker` reads go to that account's own engine, and
    /// [`Self::mount_symbol`] becomes the symbol that account is armed on
    /// (`vike_run::account_symbols_for`) — which is how a second account reaches a second
    /// instrument at all, since `vike_run::WIRED_MARKETS` pins one symbol per venue for the DEFAULT
    /// account.
    ///
    /// ⚠ **An account this box will not ARM is a hard startup failure naming venue, account and
    /// mount** (`vike_run`'s `refuse_unarmed_mount_accounts`), not a fallback to the default
    /// account. It is the one refusal in this daemon that does not degrade, because the degraded
    /// state is "trading, on a book the author did not choose".
    #[serde(default)]
    pub account: Option<vike_model::account_keys::AccountLabel>,
    /// daemon runtime settings (summary cadence + shutdown deadline).
    #[serde(default)]
    pub daemon: DaemonSettings,
    /// The MULTI-mount spelling (split-plane I10, Pattern A: strategies sharing an account share a
    /// PROCESS): `[[mounts]]` — an ARRAY of mount blocks, each carrying the SAME fields the
    /// single-mount spelling puts at the top level (venue / `symbol`-or-`token_id` / interval /
    /// the maker knobs / an optional `[mounts.strategy]` table). Empty (the default) ⇒ the
    /// historical single-mount profile, byte-identically.
    ///
    /// The two spellings are MUTUALLY EXCLUSIVE — a profile that sets any top-level mount field
    /// beside a `[[mounts]]` array is refused at load ([`Self::validate`]), because a top-level
    /// knob beside `[[mounts]]` would configure NOTHING while reading as real (the
    /// declared-but-unread failure this repo's settings rule exists for). Each row validates
    /// through the SAME per-mount refusals a single-mount profile passes ([`Self::mount_rows`]
    /// lowers each row to a single-mount profile and validates it), each failure naming its row.
    #[serde(default)]
    pub mounts: Vec<MountCfg>,
}

/// One `[[mounts]]` row — the single-mount profile's mount fields, verbatim, MINUS the `[daemon]`
/// table (daemon-wide, not per-mount) and minus a nested `mounts` (rows do not recurse).
///
/// `venue` is an `Option` here where [`DaemonProfile`] defaults it silently, so
/// [`DaemonProfile::mount_rows`] can apply the same `"polymarket"` default per row; every other
/// field is the exact `Option` the top level has. Field docs live on [`DaemonProfile`] — one
/// authority, and these ARE those fields.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MountCfg {
    #[serde(default)]
    pub venue: Option<String>,
    #[serde(default)]
    pub symbol: Option<String>,
    #[serde(default)]
    pub token_id: Option<String>,
    #[serde(default)]
    pub strategy: Option<StrategyCfg>,
    #[serde(default)]
    pub resolution_ts_ms: Option<i64>,
    #[serde(default)]
    pub interval: Option<String>,
    #[serde(default)]
    pub interval_ms: Option<i64>,
    #[serde(default)]
    pub qty: Option<f64>,
    #[serde(default)]
    pub half_spread: Option<f64>,
    #[serde(default)]
    pub tick_size: Option<f64>,
    #[serde(default)]
    pub seed_cash: Option<f64>,
    #[serde(default)]
    pub data_only: Option<bool>,
    /// WHICH ACCOUNT of this row's venue it trades on — see [`DaemonProfile::account`], which this
    /// lowers into verbatim.
    #[serde(default)]
    pub account: Option<vike_model::account_keys::AccountLabel>,
}

/// Daemon runtime settings — the two knobs that are the daemon's own, not the mount's.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonSettings {
    /// snapshot-summary cadence in ms (default `5_000`): how often a one-line JSON summary of the
    /// live snapshot is printed to STDOUT.
    #[serde(default = "default_summary_ms")]
    pub summary_ms: u64,
    /// bounded-shutdown deadline in ms (default `5_000`): the whole `run_with_deadline` teardown is
    /// hard-capped at this, so a wedged `shutdown_and_join` can never hang exit.
    #[serde(default = "default_shutdown_deadline_ms")]
    pub shutdown_deadline_ms: u64,
}

/// The historical default venue an omitted `venue` key resolves to ([`DaemonProfile::venue`]).
const DEFAULT_VENUE: &str = "polymarket";
fn default_summary_ms() -> u64 {
    5_000
}
fn default_shutdown_deadline_ms() -> u64 {
    5_000
}

impl Default for DaemonSettings {
    fn default() -> Self {
        DaemonSettings {
            summary_ms: default_summary_ms(),
            shutdown_deadline_ms: default_shutdown_deadline_ms(),
        }
    }
}

impl DaemonProfile {
    /// Parse a profile from TOML text, then validate it.
    pub fn from_toml_str(s: &str) -> Result<Self, String> {
        let profile: DaemonProfile =
            toml::from_str(s).map_err(|e| format!("parse profile TOML: {e}"))?;
        profile.validate()?;
        Ok(profile)
    }

    /// Read + parse + validate a profile file.
    pub fn load(path: &str) -> Result<Self, String> {
        let text =
            std::fs::read_to_string(path).map_err(|e| format!("read profile {path}: {e}"))?;
        Self::from_toml_str(&text)
    }

    /// The mount SYMBOL, from whichever spelling the profile used. Panics never: [`Self::validate`]
    /// has already rejected a profile that set neither or both, and every construction path goes
    /// through it.
    pub fn mount_symbol(&self) -> &str {
        self.symbol.as_deref().or(self.token_id.as_deref()).unwrap_or_default()
    }

    /// The mount VENUE, with the historical `"polymarket"` default applied — the one resolution
    /// site for the field's `Option` (see the field doc for why it is one).
    pub fn venue(&self) -> &str {
        self.venue.as_deref().unwrap_or(DEFAULT_VENUE)
    }

    /// The EFFECTIVE data-only verdict for this mount row (see the [`Self::data_only`] field doc):
    /// absent and explicit `false` are the same answer — exec follows the ordinary credential
    /// gate — so a profile that spells the default is byte-identical to one that omits it.
    pub fn data_only_effective(&self) -> bool {
        self.data_only.unwrap_or(false)
    }

    /// This profile as N single-mount rows: itself for the historical single-mount spelling, else
    /// one lowered [`DaemonProfile`] per `[[mounts]]` row (same `[daemon]` settings, `mounts`
    /// emptied — rows do not recurse). Every consumer of a mount — validation, the mount-config /
    /// mount-spec lowerings, strategy resolve, `effective_params`, `validate_for_live` — runs on a
    /// ROW, so the multi-mount path reuses the single-mount machinery verbatim and the two
    /// spellings cannot diverge on what one mount means.
    pub fn mount_rows(&self) -> Vec<DaemonProfile> {
        if self.mounts.is_empty() {
            return vec![self.clone()];
        }
        self.mounts
            .iter()
            .map(|m| DaemonProfile {
                venue: m.venue.clone(),
                symbol: m.symbol.clone(),
                token_id: m.token_id.clone(),
                strategy: m.strategy.clone(),
                resolution_ts_ms: m.resolution_ts_ms,
                interval: m.interval.clone(),
                interval_ms: m.interval_ms,
                qty: m.qty,
                half_spread: m.half_spread,
                tick_size: m.tick_size,
                seed_cash: m.seed_cash,
                data_only: m.data_only,
                account: m.account.clone(),
                daemon: self.daemon.clone(),
                mounts: Vec::new(),
            })
            .collect()
    }

    /// The DERIVED per-mount controller id a `[[mounts]]` row mounts under:
    /// `{venue}__{symbol}__{interval}__{strategy-identity}` — the runtime's legacy
    /// `{venue}__{symbol}__{interval}` triple (`vike_core::strategy_state`'s `mount_id_with`
    /// fallback) extended by WHAT is mounted, so two different strategies on one series get two
    /// identities (their own durable-state sidecar, their own journal attribution key) instead of
    /// the `assemble_core` duplicate-id panic.
    ///
    /// The strategy identity is [`Self::strategy_name`] for a registry/maker mount; a Rhai row
    /// appends the script FILENAME STEM (sanitized to `[A-Za-z0-9_-]` — the id is a state-sidecar
    /// FILENAME component) so two different scripts on one series stay distinct. Two rows deriving
    /// the SAME id are refused at load ([`Self::validate`]), naming both rows.
    ///
    /// Single-mount profiles deliberately do NOT use this: their `controller_id` stays `None`, so
    /// the runtime keeps the legacy triple derivation and an existing deployment's state sidecar /
    /// journal attribution keys are byte-identical.
    ///
    /// # ⚠ THE ACCOUNT IS PART OF THE IDENTITY, and leaving it out REFUSED THE HEADLINE SPREAD
    ///
    /// A labelled row appends `__{LABEL}`. Without it the id above forgets [`Self::account`]
    /// entirely, and two rows differing ONLY by account derive the SAME id — so the duplicate-id
    /// refusal in [`Self::validate`] rejected, at load, exactly the configuration the `account`
    /// field exists to enable: long BTC on the default account, short BTC on a labelled one, one
    /// venue, one symbol, one interval, one strategy. Its message even told the operator to
    /// "make one row distinct — a different interval, symbol or strategy", i.e. to stop running a
    /// spread. The refusal was not wrong; the IDENTITY was, and this is the half that was missing.
    ///
    /// It is a correctness fix as well as an unblocking one. Two accounts are two BOOKS — separate
    /// positions, separate fills, separate reconcile — so two such rows must never share one
    /// durable-state sidecar or one journal attribution key. Had the duplicate check been relaxed
    /// instead, that sharing is precisely what would have happened.
    ///
    /// **The DEFAULT account renders the id unchanged, byte for byte**
    /// ([`vike_model::account_keys::AccountLabel::text`] is
    /// `None` for it), so no deployment's sidecar or attribution key moves. Nothing has ever
    /// shipped a labelled row — the field is new — so the appended half moves nothing either.
    ///
    /// ⚠ A residual, declared rather than fixed: a SINGLE-mount profile that names an account keeps
    /// `controller_id = None` and therefore the legacy account-blind triple, so switching one from
    /// the default account to a labelled one INHERITS the default account's sidecar. Giving it an
    /// account-aware id would move every shipped single-mount deployment's state, which is the
    /// byte-identity this whole derivation is built around; `[[mounts]]` is the multi-account
    /// spelling, and it is the one that carries the account into the id.
    pub fn derived_controller_id(&self) -> String {
        let spec = self.to_mount_spec();
        let base = format!(
            "{}__{}__{}__{}",
            spec.venue,
            spec.symbol,
            spec.interval,
            self.mount_identity()
        );
        match spec.account.as_ref().and_then(vike_model::account_keys::AccountLabel::text) {
            None => base,
            Some(label) => format!("{base}__{label}"),
        }
    }

    /// The strategy half of [`Self::derived_controller_id`] — the registry name, or
    /// `rhai-<sanitized file stem>` for a script row.
    fn mount_identity(&self) -> String {
        let rhai = self.strategy.as_ref().and_then(|s| s.rhai.as_deref());
        match rhai {
            None => self.strategy_name().to_string(),
            Some(path) => {
                let stem = std::path::Path::new(path)
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let sanitized: String = stem
                    .chars()
                    .map(
                        |c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '-' },
                    )
                    .collect();
                format!("rhai-{sanitized}")
            }
        }
    }

    /// Everything a mount cannot recover from, checked at LOAD:
    ///
    /// 1. exactly one of `symbol` / `token_id`, non-empty — the mount routes and fills nothing
    ///    without a symbol, and two disagreeing spellings have no defensible winner;
    /// 2. `[strategy]`, if present, names exactly ONE thing to mount (`name` XOR `rhai` — the same
    ///    mutual-exclusion shape as (1), and for the same reason: two disagreeing selections have
    ///    no defensible winner, and a table with neither selects nothing for its params to
    ///    configure);
    /// 3. `[strategy].name`, if set, is a strategy this daemon can actually MOUNT AND TRADE;
    /// 4. `[strategy.params]`, if present, contains only keys that strategy actually READS, each
    ///    carrying a value its reader can TAKE and — for the keys that name a market — naming THIS
    ///    mount's own `(venue, symbol)`; see `DaemonProfile::validate_strategy` for why each of the
    ///    three is a live hazard rather than a tidiness question. Under `rhai` the same posture in
    ///    the script vocabulary: every value must be a NUMBER (see [`Self::validate_script`]).
    ///
    /// (3) is the honest half. `vike_strategy::capability` distinguishes four answers, and three of
    /// them are failures with DIFFERENT causes: a typo, a simulator-only strategy (it exists, but
    /// only inside `vike-backtest` — this daemon deliberately does not link the simulator), and a
    /// strategy that RESOLVES but whose input never arrives live (a funding reader with no live
    /// funding series; a two-leg strategy with no second mounted leg). That third class is the one
    /// worth failing loudly for: it mounts cleanly, logs nothing unusual and simply never submits —
    /// the live-vs-backtest divergence shape an operator discovers days later.
    fn validate(&self) -> Result<(), String> {
        if !self.mounts.is_empty() {
            return self.validate_multi();
        }
        match (self.symbol.as_deref(), self.token_id.as_deref()) {
            (Some(_), Some(_)) => {
                return Err(
                    "set `symbol` OR `token_id`, not both (they name the same field)".to_string()
                )
            }
            (None, None) => {
                return Err("a mount symbol is required: set `symbol` (or, on polymarket, \
                            `token_id`)"
                    .to_string())
            }
            _ => {}
        }
        if self.mount_symbol().trim().is_empty() {
            return Err("the mount symbol must not be empty".to_string());
        }
        // The data-only declaration is meaningful ONLY where the venue's market data is
        // credentialed (see `DATA_ONLY_VENUES`' doc): on a keyless-data venue the data-only mount
        // already exists — withhold the venue's credentials from the store — and a declaration
        // here could only re-state the live gate while misleading the arming disclosure.
        if self.data_only_effective() && !DATA_ONLY_VENUES.contains(&self.venue()) {
            return Err(format!(
                "`data_only = true` is only meaningful for a venue whose MARKET DATA is \
                 credentialed ({}), where the same credentials would otherwise arm exec; {} has a \
                 keyless data plane, so its data-only mount is simply \"no {} credentials in the \
                 store\" (absent credentials ARE the live gate). Drop the key",
                DATA_ONLY_VENUES.join(", "),
                self.venue(),
                self.venue().to_uppercase()
            ));
        }
        if let Some(s) = &self.strategy {
            match (s.name.as_deref(), s.rhai.as_deref()) {
                (Some(_), Some(_)) => {
                    return Err("set `[strategy] name` OR `[strategy] rhai`, not both: they each \
                                select the whole strategy, and two disagreeing selections have no \
                                defensible winner"
                        .to_string())
                }
                (None, None) => {
                    return Err("a `[strategy]` table must say WHAT to mount: set \
                                `name = \"<registry strategy>\"` or `rhai = \"<path to .rhai \
                                script>\"` (or drop the table for the default A-S maker) — a \
                                table with neither selects nothing for its `[strategy.params]` to \
                                configure"
                        .to_string())
                }
                (Some(name), None) => self.validate_strategy(name, s)?,
                (None, Some(path)) => Self::validate_script(path, s)?,
            }
        }
        Ok(())
    }

    /// The `[[mounts]]` half of [`Self::validate`] (split-plane I10). THREE refusals, each a way
    /// the profile would otherwise be silently wrong:
    ///
    /// 1. **Both spellings set.** Any top-level mount field beside a `[[mounts]]` array would
    ///    configure NOTHING while reading as real — the declared-but-unread failure. Refused
    ///    naming every offending key.
    /// 2. **A row that fails the single-mount refusals.** Each row lowers to a single-mount
    ///    profile ([`Self::mount_rows`]) and runs the ENTIRE existing `validate` — symbol
    ///    mutual-exclusion, the strategy-capability gate, all four params refusals — so a
    ///    `[[mounts]]` row cannot mount anything a single-mount profile could not. The error names
    ///    the row (`mounts[i]`).
    /// 3. **Two rows deriving ONE mount id** ([`Self::derived_controller_id`]). The runtime
    ///    PANICS on a duplicate controller id (`assemble_core` — a shared id silently shares
    ///    durable state), so the duplicate is refused HERE, at load, naming both rows, never at
    ///    the panic.
    fn validate_multi(&self) -> Result<(), String> {
        let set_besides: Vec<&str> = [
            ("venue", self.venue.is_some()),
            ("symbol", self.symbol.is_some()),
            ("token_id", self.token_id.is_some()),
            ("strategy", self.strategy.is_some()),
            ("resolution_ts_ms", self.resolution_ts_ms.is_some()),
            ("interval", self.interval.is_some()),
            ("interval_ms", self.interval_ms.is_some()),
            ("qty", self.qty.is_some()),
            ("half_spread", self.half_spread.is_some()),
            ("tick_size", self.tick_size.is_some()),
            ("seed_cash", self.seed_cash.is_some()),
            ("data_only", self.data_only.is_some()),
        ]
        .into_iter()
        .filter_map(|(k, set)| set.then_some(k))
        .collect();
        if !set_besides.is_empty() {
            return Err(format!(
                "this profile sets BOTH spellings: `[[mounts]]` AND top-level mount field(s) {}. \
                 With a `[[mounts]]` array every mount field lives inside its own row — a \
                 top-level knob beside the array would configure nothing while reading as real. \
                 Move the field(s) into a `[[mounts]]` row, or drop the array",
                set_besides.join(", ")
            ));
        }
        let rows = self.mount_rows();
        for (i, row) in rows.iter().enumerate() {
            row.validate().map_err(|e| {
                format!(
                    "mounts[{i}] (venue={:?}, symbol={:?}): {e}",
                    row.venue(),
                    row.mount_symbol()
                )
            })?;
        }
        let ids: Vec<String> = rows.iter().map(DaemonProfile::derived_controller_id).collect();
        for j in 1..ids.len() {
            if let Some(i) = (0..j).find(|&i| ids[i] == ids[j]) {
                return Err(format!(
                    "mounts[{i}] and mounts[{j}] derive the SAME mount id {:?} — venue, symbol, \
                     interval, strategy AND account all equal, so the two rows would share one \
                     durable-state sidecar and one journal attribution key (the runtime refuses \
                     that with a panic; this refusal is the load-time version that can name the \
                     rows). Make one row distinct — a different interval, symbol, strategy or \
                     `account` — or delete the duplicate. ⚠ Two rows on one venue and one symbol \
                     that differ by `account` are an ordinary SPREAD and are NOT this error: the \
                     account is part of the mount id, so they derive different ids",
                    ids[j]
                ));
            }
        }
        // Rows sharing a VENUE must agree on the data-only verdict: the withhold is per venue
        // account (one exec engine, one credential set), so "row A trades live while row B is
        // data-only" is not a state `live_mount` can construct — refused here, naming both rows,
        // rather than silently resolved in favour of whichever row is read first.
        for j in 1..rows.len() {
            if let Some(i) = (0..j).find(|&i| {
                rows[i].venue() == rows[j].venue()
                    && rows[i].data_only_effective() != rows[j].data_only_effective()
            }) {
                return Err(format!(
                    "mounts[{i}] and mounts[{j}] share venue {:?} but DISAGREE on `data_only` — \
                     the declaration withholds that venue's credentials from the ONE exec engine \
                     both rows share, so the two rows cannot have different answers. Set the same \
                     value on both (or drop the key from both)",
                    rows[j].venue()
                ));
            }
        }
        Ok(())
    }

    /// The `[strategy]` half of [`Self::validate`]: WHICH strategy, then WHAT it was handed.
    ///
    /// ⚠ **Four refusals over one table, and each one is the case the previous one passes.** A key
    /// no reader reads ([`vike_strategy::unknown_params`]); a key whose VALUE no reader can take
    /// ([`vike_strategy::mistyped_params`]); a key that is read, well-typed, and OVERRIDDEN BY THE
    /// MOUNT ([`vike_strategy::misrouted_params`]); and a whole TABLE that is spelled, typed and
    /// routed right and still describes no order at all ([`vike_strategy::unarmable_params`]). The
    /// first three end in the same place — the knob runs at something the profile does not state —
    /// and the third of them is the worst, because the knobs it covers name WHICH INSTRUMENT and
    /// WHICH VENUE real orders go to. MEASURED on the CI box before it existed: a `buy_hold` profile with
    /// `symbol = "MOUNTED_SYMBOL"` and `[strategy.params] symbol = "A_COMPLETELY_DIFFERENT_SYMBOL"`
    /// loaded, announced `symbol=A_COMPLETELY_DIFFERENT_SYMBOL`, and filled on `MOUNTED_SYMBOL`.
    ///
    /// The second half exists because a `from_params` reader CANNOT FAIL — every one in this
    /// workspace ignores a key it does not recognise, so `qtyy = 0.005` is not an error, it is `qty`
    /// at the strategy's compiled default. `deny_unknown_fields` on [`DaemonProfile`] makes a
    /// top-level typo fatal and stops at the `[strategy.params]` boundary, because the field is a
    /// free-form `toml::Value`. That asymmetry was safe while this table only fed the backtest
    /// simulator (a wrong number is a wrong chart); it is not safe now that the same table is the
    /// SIZE INPUT TO REAL ORDERS, where the compiled default may be hundreds of times the intended
    /// clip and the only thing behind it is the OPTIONAL `policy.max_notional_per_order`.
    ///
    /// ⚠ Deliberately the SAME strictness on the paper mount as on the live one. A paper rehearsal
    /// exists to predict the live mount, so a rehearsal that silently ran different parameters would
    /// conceal exactly what it is for — and two strictness levels over one table is how this daemon
    /// got two spellings of the A-S maker in the first place.
    ///
    /// ⚠ **An INERT KEY is a fifth member of the family, and it is deliberately NOT a refusal.** A
    /// key can pass all four checks above and still be read by nobody, because another key in the
    /// same table sent the strategy down a branch that never looks at it (`anchor_price` with
    /// `anchor` unset; `tick` outside a `bounded01` market). A refusal would be wrong for a reason
    /// a misroute does not share: a misrouted symbol is wrong under every configuration and
    /// unrecoverable at runtime, while such a key is armed from the SAME table — and one shipped
    /// caller supplies half of an inert pair as a matter of course
    /// (`crates/vike-strategy/src/trailing_scalper.rs`'s module doc: the batch tool passes
    /// `market_open_ms`/`market_close_ms` per run while the delay/cutoff knobs stay off by default),
    /// so a refusal would reject that tool's own documented default. Nothing annotates it either —
    /// see [`Self::effective_params`]. What keeps that class from growing is
    /// `crates/vike-strategy/tests/param_gates.rs`, which drives every strategy and fails when a
    /// declared key changes nothing.
    ///
    /// ⚠ **The fourth refusal is NOT that rule wearing a coat, and the difference is exactly the
    /// one above.** It refuses a table whose LADDER is empty, never a key whose value looks wrong:
    /// no missing line arms it (every key is present and no value of any other key rescues it), it
    /// rejects no shipped profile shape, and its consequence is the class this daemon most needs to
    /// fail loudly for — a mount that starts clean, announces a full configuration line and then
    /// never submits, discovered days later. `vike_strategy::unarmable_params`' own doc carries the
    /// argument, including why the tempting wider rule ("refuse a mount that would place no
    /// orders") is NOT safe: a strategy waiting on a market condition places none either, and no
    /// load-time check can tell the two apart without simulating a market.
    fn validate_strategy(&self, name: &str, s: &StrategyCfg) -> Result<(), String> {
        // The ONE registry name that is not mounted BY NAME here — and no longer a refusal of the
        // script path itself. Scripts ARE live-mountable on this daemon
        // (`docs/decisions/0024-rhai-strategies-live.md`), through the `rhai = "<path>"` spelling;
        // the `name = "rhai"` arm stays the BACKTEST's, because it takes the script as an inline
        // `src` param and lives in `vike-backtest`'s registry (`vike-script` declares the same
        // layer rank as `vike-strategy`, so the shared registry cannot name it). Redirect rather
        // than letting `capability` call it simulator-only, which stopped being the whole truth
        // the day the reversal landed.
        if name == "rhai" {
            return Err("scripts mount here by PATH, not by registry name: set `[strategy] \
                        rhai = \"<path to .rhai script>\"` (the `name = \"rhai\"` spelling is the \
                        backtest registry's inline-`src` arm, which this daemon does not link — \
                        see docs/decisions/0024-rhai-strategies-live.md)"
                .to_string());
        }
        match vike_strategy::capability(name) {
            Capability::Live => {}
            Capability::NotLive(why) => {
                return Err(format!(
                    "strategy {name:?} resolves but cannot trade on this daemon: {why}"
                ))
            }
            Capability::SimulatorOnly(why) => {
                return Err(format!(
                    "strategy {name:?} is simulator-only ({why}) — it backtests, but this daemon \
                         does not link the simulator and could not mount it"
                ))
            }
            // Not a built-in: the USER registry (compiled from user_data/strategies/rust — empty
            // in any checkout without one) is consulted LAST, so a user folder can never shadow a
            // built-in name. Live mounting keeps the LIVE_CAPABLE default-deny posture: a user
            // strategy trades here only if its own folder manifest opted in.
            Capability::Unknown if vike_user_strategies::USER_STRATEGIES.contains(&name) => {
                if !vike_user_strategies::USER_LIVE_CAPABLE.contains(&name) {
                    return Err(format!(
                        "user strategy {:?} resolves but cannot trade on this daemon: its \
                         folder's strategy.toml declares no `live = true` (the user-tier \
                         LIVE_CAPABLE opt-in)",
                        name
                    ));
                }
            }
            Capability::Unknown => {
                return Err(format!(
                    "unknown strategy {:?} (mountable here: {}{})",
                    name,
                    vike_strategy::LIVE_CAPABLE
                        .iter()
                        .filter(|(_, w)| w.is_none())
                        .map(|(n, _)| *n)
                        .collect::<Vec<_>>()
                        .join(", "),
                    if vike_user_strategies::USER_STRATEGIES.is_empty() {
                        String::new()
                    } else {
                        format!("; user: {}", vike_user_strategies::USER_STRATEGIES.join(", "))
                    },
                ))
            }
        }
        // WHAT it was handed. `[strategy.params]` is a free-form table by design (the registry is
        // shared with the backtest, whose strategies each read their own knobs), so the profile's
        // serde layer cannot judge it — but the registry can.
        let Some(table) = s.params.as_table() else {
            return Err(format!(
                "`[strategy.params]` must be a TABLE of keys, got {}",
                s.params.type_str()
            ));
        };
        if AS_MAKER_NAMES.contains(&name) {
            if !table.is_empty() {
                let keys: Vec<&str> = table.keys().map(String::as_str).collect();
                return Err(format!(
                    "strategy {:?} takes no `[strategy.params]` on this daemon (got: {}). It is \
                     built from the profile's OWN maker fields — `qty` / `half_spread` / \
                     `tick_size` / `resolution_ts_ms` at the top level — through the same \
                     `vike_run::build_maker` call a profile with no `[strategy]` table makes, so \
                     the two spellings cannot differ. A-S knob tuning (`gamma`, `kappa_*`, \
                     `price_domain`, …) has NO profile surface here yet: these keys would have been \
                     silently dropped, along with the venue-selected price domain, which on a \
                     $-scale venue means a maker that never quotes.",
                    name,
                    keys.join(", ")
                ));
            }
        } else {
            let unknown = vike_strategy::unknown_params(name, &s.params);
            if !unknown.is_empty() {
                return Err(format!(
                    "strategy {:?} does not read these `[strategy.params]` keys: {}. A params \
                     reader IGNORES what it does not recognise, so each of them would mount at the \
                     strategy's COMPILED DEFAULT — a size knob among them is a live order at a size \
                     nobody typed. It reads: {}.",
                    name,
                    unknown.join(", "),
                    readable_keys(name)
                ));
            }
            // ...and the same hazard one level down: a key it DOES read, carrying a value of a type
            // it cannot take. `and_then(Value::as_…)` yields `None` for a wrong type exactly as it
            // does for an absent key, so the knob lands on the compiled default either way — but
            // this time the operator's own profile states a number and the daemon signs orders at
            // another. Quoting a number (`size = "2"`) is the ordinary way to get there, and it is
            // invisible to `deny_unknown_fields`, to `unknown_params`, and to the reader itself.
            let mistyped = vike_strategy::mistyped_params(name, &s.params);
            if !mistyped.is_empty() {
                return Err(format!(
                    "strategy {:?} was handed `[strategy.params]` values of the wrong TYPE: {}. A \
                     params reader takes a value only when its TOML type matches, and IGNORES it \
                     otherwise — exactly as it ignores an unknown key — so each of these would \
                     mount at the strategy's COMPILED DEFAULT while the profile says otherwise. It \
                     reads: {}.",
                    name,
                    mistyped.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("; "),
                    readable_keys(name)
                ));
            }
            // ...and the third case, which the two above BOTH pass: a key that is read, whose value
            // is well-typed, and which the MOUNT then overrides. See the message for the mechanism.
            let misrouted =
                vike_strategy::misrouted_params(name, &s.params, self.venue(), self.mount_symbol());
            if !misrouted.is_empty() {
                return Err(format!(
                    "strategy {:?} was handed `[strategy.params]` keys naming a market this mount \
                     does not trade: {}. A live mount routes EVERY order to its OWN (venue, \
                     symbol) — `crates/vike-core/src/runtime/strategy_drive.rs`'s \
                     `resolve_intent_symbol` returns the mount's symbol and `resolve_intent_venue` \
                     its venue, unconditionally, because no mount declares extra legs today \
                     (`vike_run::MountSpec`'s `legs` is empty on every spec and \
                     `build_paper_strategy_core_with` asserts it). So each of these configures \
                     NOTHING while the mount line echoes it as real — the operator reads one \
                     instrument and the orders hit another. Set it to this mount's own \
                     `venue = {:?}` / `symbol = {:?}`, or drop the key.",
                    name,
                    misrouted.iter().map(|m| m.to_string()).collect::<Vec<_>>().join("; "),
                    self.venue(),
                    self.mount_symbol(),
                ));
            }
            // ...and the fourth, which all three above pass: every key is spelled right, typed right
            // and routed right, and the ORDER SET they describe between them is empty. The other
            // three are "this knob runs at something you did not state"; this one is "nothing runs
            // at all", and it is the quietest failure of the four — a clean mount line and then
            // silence.
            if let Some(why) = vike_strategy::unarmable_params(name, &s.params) {
                return Err(format!(
                    "{why}. Both `arm`s in `crates/vike-strategy/src/grid_dca.rs` build their WHOLE \
                     order set from the params plus one anchor, once, on the first bar or tick — so \
                     an empty ladder is not a state the market moves this mount out of; it is a \
                     daemon that starts clean, echoes a full configuration line and then never \
                     submits. The rung builders (`legs_at` and `entries_at`) admit nothing when the \
                     ladder is degenerate (`rungs`, `size` or `step` at zero or below), when a \
                     `bounded01` market's `step` does not fit inside `(tick, 1 - tick)` — the \
                     compiled `step = 1.0` spans that whole domain — or when every rung prices at \
                     or below zero, which the compiled `anchor_price = 0` does to every long ladder \
                     the moment `anchor = \"fixed\"` selects it. Give the ladder a rung it can \
                     rest, or drop the `[strategy]` table."
                ));
            }
        }
        Ok(())
    }

    /// The `rhai = "<path>"` half of [`Self::validate`] — the PURE checks only, because this runs
    /// at profile LOAD and load is filesystem-free by contract (the docs-profiles CI gate parses
    /// every shipped profile on runners that have no script tree). Reading, hashing and compiling
    /// the script — and refusing an override key the script never asks for, which needs the script
    /// TEXT — happen at [`Self::resolve_script`], still before any core spawns.
    ///
    /// What IS refusable here is the value-type half of the daemon's no-silently-ignored-key
    /// posture, in the script vocabulary: every `[strategy.params]` value under `rhai` is a
    /// `param(name, default)` override, `param` yields an `f64`, and the override map is built by
    /// a NUMERIC filter (the same lenient reader convention the backtest's `rhai` arm uses) — so a
    /// quoted number (`size = "2"`) would be silently DROPPED and the script would run its own
    /// compiled default while the profile states otherwise. That is `mistyped_params`' exact
    /// hazard, refused here with the same posture.
    fn validate_script(path: &str, s: &StrategyCfg) -> Result<(), String> {
        if path.trim().is_empty() {
            return Err("`[strategy] rhai` must name a script file (absolute, or relative to the \
                        daemon's working directory)"
                .to_string());
        }
        let Some(table) = s.params.as_table() else {
            return Err(format!(
                "`[strategy.params]` must be a TABLE of keys, got {}",
                s.params.type_str()
            ));
        };
        let non_numeric: Vec<String> = table
            .iter()
            .filter(|(_, v)| v.as_float().is_none() && v.as_integer().is_none())
            .map(|(k, v)| format!("{k} ({})", v.type_str()))
            .collect();
        if !non_numeric.is_empty() {
            return Err(format!(
                "a Rhai `[strategy.params]` value must be a NUMBER, got: {}. Every key here is a \
                 `param(name, default)` override baked into the script at compile; `param` takes \
                 an f64, so any other TOML type would be silently dropped and the script would run \
                 its own compiled default while this profile states otherwise.",
                non_numeric.join(", ")
            ));
        }
        Ok(())
    }

    /// The A-S [`SpreadMaker`] this profile mounts, or `None` when it names a different strategy.
    ///
    /// **This is the ONE construction site for the maker, under BOTH spellings** — absent
    /// `[strategy]` and `[strategy] name = "spread_maker"` (or `"gueant_maker"`) reach the identical
    /// `vike_run::build_maker(cfg)` call over the identical `cfg`, so they cannot produce different
    /// makers. See [`AS_MAKER_NAMES`] for what the registry arm would have produced instead, and why
    /// it was a live hazard rather than a cosmetic difference.
    ///
    /// `gueant_maker` is that same maker with the GLFT closed form selected — the registry's own
    /// definition of the alias (`spread_maker` forced to [`SpreadModel::Gueant`]), applied to the
    /// profile's config rather than to a default one.
    pub fn mounted_maker(&self, cfg: &MakerMountConfig) -> Option<SpreadMaker> {
        let name = self.maker_name()?;
        let maker = vike_run::build_maker(cfg);
        Some(if name == "gueant_maker" {
            maker.with_spread_model(SpreadModel::Gueant)
        } else {
            maker
        })
    }

    /// WHICH maker name this profile means, or `None` when it names a different strategy. The ONE
    /// routing decision behind [`Self::mounted_maker`], [`Self::resolve_mount`] and
    /// [`Self::effective_params`], so the three cannot disagree about what is mounted.
    fn maker_name(&self) -> Option<&str> {
        match &self.strategy {
            None => Some("spread_maker"),
            Some(s) => match s.name.as_deref() {
                Some(n) if AS_MAKER_NAMES.contains(&n) => Some(n),
                // Any other registry name, or a `rhai = "<path>"` script — not the maker.
                _ => None,
            },
        }
    }

    /// Resolve this profile's strategy, **saying WHICH construction produced it**.
    ///
    /// The variant is the property under test. `[strategy] name = "spread_maker"` reaching
    /// [`MountedStrategy::Registered`] is exactly the defect this shape exists to make impossible:
    /// that arm reads `[strategy.params]` alone, so it would mount `qty = 1`, `tick_size = 0` and
    /// the `[0,1]` wall clamp while the profile said otherwise. Returning an opaque box from both
    /// paths is what let the difference hide — nothing downstream could tell the two apart, and
    /// neither could a test.
    ///
    /// [`Self::validate`] already rejected every name the registry arm can fail on, so an `Err` here
    /// means the two disagreed — worth surfacing rather than unwrapping.
    pub fn resolve_mount(&self, cfg: &MakerMountConfig) -> Result<MountedStrategy, String> {
        if let Some(maker) = self.mounted_maker(cfg) {
            return Ok(MountedStrategy::AsMaker(Box::new(maker)));
        }
        let s =
            self.strategy.as_ref().expect("mounted_maker returns Some for an absent [strategy]");
        if let Some(path) = s.rhai.as_deref() {
            return Self::resolve_script(path, s);
        }
        let name = s
            .name
            .as_deref()
            .expect("validate refused a [strategy] table with neither `name` nor `rhai`");
        match vike_strategy::strategy_by_name::<vike_core::LiveBroker>(name, &s.params) {
            Ok(boxed) => Ok(MountedStrategy::Registered(boxed)),
            // Not a built-in: the USER registry, tried last (same order as `validate_strategy`,
            // which already gated live-capability — an unknown name surviving to here still errors
            // with the built-in wording, per the "the two disagreed" contract above).
            Err(vike_strategy::RegistryError::Unknown(_)) => {
                match vike_user_strategies::user_strategy_by_name::<vike_core::LiveBroker>(
                    name, &s.params,
                ) {
                    Some(boxed) => Ok(MountedStrategy::Registered(boxed)),
                    None => {
                        Err(vike_strategy::RegistryError::Unknown(name.to_string()).to_string())
                    }
                }
            }
            Err(e) => Err(e.to_string()),
        }
    }

    /// The `rhai = "<path>"` arm of [`Self::resolve_mount`]: read the script, hash it, refuse an
    /// override the script never asks for, compile it at [`vike_core::LiveBroker`] — the SAME
    /// broker every registry strategy mounts at — and say so at INFO.
    ///
    /// Two of the four rails `docs/decisions/0024-rhai-strategies-live.md` names live HERE; the
    /// other two need no code in this crate at all (the mandatory live risk budget is
    /// `vike_mount`'s pre-connect `require_live_risk_budget`, strategy-agnostic by construction,
    /// and the HALT/panic safe-state is `vike-core`'s per-dispatch `catch_unwind` — a script
    /// strategy rides both unchanged because it enters the core as an opaque
    /// `Box<dyn Strategy<LiveBroker>>` like every other strategy):
    ///
    /// * **The audit line.** The INFO log below carries the script PATH and the sha256 of the
    ///   source that was ACTUALLY compiled — the journal answer to "which code traded", which a
    ///   path alone cannot give (the file can be edited between restarts). [`script_sha256`] is
    ///   the pure half; [`MountedStrategy::Script`] carries both values so a test asserts them
    ///   without a log subscriber.
    /// * **The engine surface.** [`vike_script::RhaiStrategy::compile_with_params`] builds its own
    ///   resource-limited engine through `vike-script`'s ONE `build_engine` — the same construction
    ///   the backtest's `rhai` arm uses, verbatim. This daemon registers NO host function of its
    ///   own, so what a script may call live is byte-identical to what it may call in a backtest.
    ///
    /// The unknown-override refusal is the daemon's no-silently-ignored-key posture in the script
    /// vocabulary, and it is deliberately keyed on `vike_script::discover_params` — the script's
    /// own one-time TOP-LEVEL run. The declared residual: a `param()` call made only INSIDE a hook
    /// body is invisible to that run, so its override key would be refused here with the message
    /// below. That is the right side of the trade: binding params at top level
    /// (`const SIZE = param("size", 1.0);`) is the documented idiom (`vike-script`'s `param`
    /// registration says so — the one-time run is what BAKES the value in), and refusing loudly
    /// with the fix in the message beats silently accepting a key that may configure nothing.
    fn resolve_script(path: &str, s: &StrategyCfg) -> Result<MountedStrategy, String> {
        let source =
            std::fs::read_to_string(path).map_err(|e| format!("read rhai script {path}: {e}"))?;
        let sha256 = script_sha256(&source);
        let declared = vike_script::discover_params(&source)
            .map_err(|e| format!("rhai script {path} failed to compile: {e}"))?;
        let overrides = script_overrides(&s.params);
        let unknown: Vec<&str> = overrides
            .keys()
            .filter(|k| !declared.iter().any(|(name, _)| name == *k))
            .map(String::as_str)
            .collect();
        if !unknown.is_empty() {
            return Err(format!(
                "rhai script {path} never asks for these `[strategy.params]` keys: {}. An \
                 override applies only where the script itself calls `param(name, default)` at \
                 top level, so each of these would configure NOTHING while the profile states a \
                 number. The script's own knobs: {}. (A `param()` call made only inside a hook \
                 body is invisible to this check — bind it at top level, `const X = \
                 param(\"x\", …);`, which is also what bakes the value in.)",
                unknown.join(", "),
                if declared.is_empty() {
                    "none — it declares no param() call at top level".to_string()
                } else {
                    declared
                        .iter()
                        .map(|(name, default)| format!("{name} (default {default})"))
                        .collect::<Vec<_>>()
                        .join(", ")
                },
            ));
        }
        let strategy = vike_script::RhaiStrategy::<vike_core::LiveBroker>::compile_with_params(
            &source, overrides,
        )
        .map_err(|e| format!("rhai script {path} failed to compile: {e}"))?;
        // The audit trail: WHICH code trades. INFO, once, at resolve — which `main` runs
        // immediately before either mount builder, so this line sits directly above the mount
        // announcement in the journal, paper and live alike.
        tracing::info!(
            script = %path,
            sha256 = %sha256,
            "mounting a Rhai script strategy — this hash is the source that was compiled"
        );
        Ok(MountedStrategy::Script { strategy: Box::new(strategy), path: path.to_string(), sha256 })
    }

    /// [`Self::resolve_mount`] boxed for the two mount builders — what `main` calls. Boxing is the
    /// only thing this adds; the routing, and the fact that it is OBSERVABLE, live one frame up.
    pub fn resolve_strategy(
        &self,
        cfg: &MakerMountConfig,
    ) -> Result<Box<dyn vike_model::Strategy<vike_core::LiveBroker> + Send>, String> {
        Ok(match self.resolve_mount(cfg)? {
            MountedStrategy::AsMaker(maker) => maker,
            MountedStrategy::Registered(strategy) => strategy,
            MountedStrategy::Script { strategy, .. } => strategy,
        })
    }

    /// The name of the strategy this profile mounts — the registry name, `"rhai"` for a
    /// `rhai = "<path>"` script (the PATH is in the resolve's own audit line, not here), or
    /// `"spread_maker"` for the absent-`[strategy]` default (which mounts exactly that strategy).
    pub fn strategy_name(&self) -> &str {
        match &self.strategy {
            None => "spread_maker",
            // A validated `[strategy]` table carries `name` XOR `rhai`, so `name`-absent IS the
            // script arm; every construction path goes through `validate` (the `mount_symbol`
            // contract).
            Some(s) => s.name.as_deref().unwrap_or("rhai"),
        }
    }

    /// The EFFECTIVE parameters of the mounted strategy, as one log-safe line.
    ///
    /// ⚠ Not decoration. Until this existed the daemon logged `strategy = <name>` and NOTHING about
    /// what it was configured with, so an operator could not tell — then or afterwards, from the
    /// journal — which numbers were actually mounted. That is half of why a silently-dropped params
    /// key was so hard to see: the other half (`DaemonProfile::validate_strategy`) makes it impossible, and
    /// this makes it diagnosable.
    ///
    /// **Both paths report what was RESOLVED, never what was typed.** For the A-S maker that is the
    /// knobs the mount actually built (which the profile may not state — the venue-selected price
    /// domain and variance/horizon modes come from [`Self::to_mount_config`]); for a registry
    /// strategy it is `vike_strategy::resolved_params`, which re-runs the SAME pure `from_params`
    /// the mount used and reports every knob it landed on.
    ///
    /// ⚠ It echoed the RAW `[strategy.params]` table for a whole round, and that was worse than
    /// logging nothing: `size = "2"` printed `size="2"` while the strategy ran `size = 1`, so the
    /// one diagnostic added to make the mount visible affirmatively misreported it. This repo's own
    /// settings-consumption rule is the authority on why — a declared-but-unread key "hands the
    /// operator positive confirmation of something false", and `Policy::max_total_exposure` was
    /// deleted for exactly that. `DaemonProfile::validate_strategy` now refuses that particular
    /// input, but a type check only ever covers the inputs it rejects: a reader may still CLAMP
    /// (`read_rungs` floors `rungs = -5` at `0`), fall back on an unrecognised string
    /// (`side = "shrot"` mounts LONG), or supply a default the profile never mentions (a
    /// controller harness's
    /// `venue = "sim"`). Echoing the resolution is what makes ALL of them visible, including the
    /// ones nobody has thought of yet.
    ///
    /// ⚠ **Read this line for what it is: it says what each knob RESOLVED TO, and it does not claim
    /// any knob is in force.** Whether a resolved value is CONSUMED can depend on the other knobs
    /// and on the market — `anchor_price` on a grid whose `anchor` is unset resolves to exactly the
    /// number the operator typed and is then read by nobody
    /// (`crates/vike-strategy/src/grid_dca.rs`'s `anchor_at` looks at it only in the
    /// `AnchorMode::Fixed` arm). ⚠ The most extreme case of that shape no longer reaches this line:
    /// a `grid`/`dca_accumulate` whose ladder rests nothing read none of its own knobs at all, and
    /// `vike_strategy::unarmable_params` refuses that table at load rather than letting it mount and
    /// echo a configuration nothing runs. Two rounds annotated the subset of that a predicate over the params
    /// table can reach, and `vike_strategy::PARAM_GATES`' own doc records why each round's
    /// annotation turned out to be a fresh false claim and why the whole marking was deleted rather
    /// than repaired again. Said once, here, is the whole of it.
    ///
    /// ## ⚠ The maker arm reports the knobs that DECIDE THE POSTED WIDTH, which it once did not
    ///
    /// It formatted nine fields and not one of the four that set the quoted half-spread —
    /// `min_half_spread_ticks`, `max_half_spread_ticks`, `kappa_default`, `tau_hold_ms` — so an
    /// operator reading the startup line could see `gamma` and the price domain while the two numbers
    /// that actually bound `δ` were invisible. It also printed `half_spread`, which is DEAD on this
    /// daemon: A-S always prices here, so that field is only the fixed-spread seed
    /// `SpreadMaker::new` takes and nothing consumes it (`MakerMountConfig::crypto`'s own comment says
    /// "Unused while A-S prices"). Logging a dead knob beside the live ones is the declared-but-unread
    /// failure this method's doc opens with, so it is GONE rather than annotated.
    ///
    /// `round_trip_fee_rate` is here because it is the one knob whose `None` an operator must be able
    /// to see: `None` means NO break-even floor is armed — "nobody could name this venue's maker fee
    /// as a fraction of price" — and it is NOT the same statement as `Some(0.0)`, a measured zero-fee
    /// venue. Read it beside `max_half_spread_ticks`: when `½ · rate · mid` exceeds
    /// `max_half_spread_ticks · tick_size` the maker posts NOTHING, by design
    /// (`vike_mm::avellaneda::bounded_half_spread`), and these are the numbers that say so.
    pub fn effective_params(&self, cfg: &MakerMountConfig) -> String {
        if self.mounted_maker(cfg).is_some() {
            let p = &cfg.as_params;
            return format!(
                "qty={} tick_size={} min_half_spread_ticks={} max_half_spread_ticks={} \
                 round_trip_fee_rate={:?} kappa_default={} tau_hold_ms={} price_domain={:?} \
                 variance_mode={:?} horizon_mode={:?} spread_model={:?} gamma={} \
                 resolution_ts={:?}",
                cfg.qty,
                cfg.tick_size,
                p.min_half_spread_ticks,
                p.max_half_spread_ticks,
                p.round_trip_fee_rate,
                p.kappa_default,
                p.tau_hold_ms,
                p.price_domain,
                p.variance_mode,
                p.horizon_mode,
                if self.strategy_name() == "gueant_maker" {
                    SpreadModel::Gueant
                } else {
                    p.spread_model
                },
                p.gamma,
                p.resolution_ts,
            );
        }
        let Some(s) = self.strategy.as_ref() else {
            // Unreachable: an absent `[strategy]` IS the maker, handled above.
            return "(none)".to_string();
        };
        // A Rhai script: report the path plus the numeric OVERRIDES the compile bakes in — a true
        // claim, because `resolve_script` refuses any override key the script's own top level
        // never passes to `param(…)`, so every key printed here WAS asked for and DID land. Knobs
        // the profile does not override run at the script's own `param` defaults, which live in
        // the source the path names — and the resolve's sha256 audit line pins WHICH source.
        if let Some(path) = s.rhai.as_deref() {
            let overrides = script_overrides(&s.params);
            return if overrides.is_empty() {
                format!("script={path} (no overrides — every param() runs its script default)")
            } else {
                let knobs: Vec<String> =
                    overrides.iter().map(|(k, v)| format!("{k}={v}")).collect();
                format!("script={path} {}", knobs.join(" "))
            };
        }
        let name = s.name.as_deref().unwrap_or_default();
        match vike_strategy::resolved_params(name, &s.params) {
            Some(rows) => {
                rows.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ")
            }
            // A USER strategy (compiled from user_data): its reader is the operator's own code,
            // not instrumented by `resolved_params` — say so, and do NOT echo the raw table (the
            // misreport this method exists to have stopped applies with extra force to a reader
            // nobody in-tree has audited).
            None if vike_user_strategies::USER_STRATEGIES.contains(&name) => format!(
                "(user strategy {name:?} — its own reader is not instrumented, so nothing here \
                 can state what it resolved)"
            ),
            // A name that declines to enumerate its knobs. On this daemon that is only the two
            // maker aliases, which took the branch above — so say SO rather than falling back to
            // echoing the raw table, which is the misreport this method exists to have stopped.
            None => format!(
                "(unreported — {name:?} enumerates no knobs, so nothing here can state what it \
                 resolved)"
            ),
        }
    }

    /// The strategy-free mount projection both generic builders take — this profile's
    /// [`MakerMountConfig`] lowering, minus the A-S knobs. One derivation, so a `[strategy]` mount
    /// and the default A-S mount can never disagree about venue/symbol/interval/seed_cash or the
    /// paper fee model.
    pub fn to_mount_spec(&self) -> MountSpec {
        let mut spec = self.to_mount_config().mount_spec();
        // …plus the ACCOUNT, which `MakerMountConfig` has no field for and should not grow one:
        // that type is the A-S maker's own knobs, and WHICH ACCOUNT a mount trades on is a fact
        // about the mount rather than about the maker (`vike_run::MountSpec::account` is where it
        // belongs, and every other strategy this daemon can mount reaches it through the same
        // spec). Set here rather than threaded through `to_mount_config` so a maker built for a
        // paper rehearsal is byte-identical.
        spec.account = self.account.clone();
        spec
    }

    /// Safety gate #5: may this profile be armed LIVE? Called from `main` ONLY when the live gate is
    /// on; the paper path never consults it (a paper profile can name any venue/symbol).
    ///
    /// THREE questions, in order, each with its own authority — it used to be one hardcoded table of
    /// `(venue, symbol)` PAIRS, which conflated them and duplicated `build_node`'s own symbol
    /// literals:
    ///
    /// 1. **Is the VENUE live-wired in this build?** [`LIVE_WIRED_VENUES`] — the venues `live_mount`
    ///    has a market-feed arm for. This is the gate that stops a silent widening, and it is the
    ///    one an operator can reason about ("did somebody wire polymarket?").
    /// 2. **Would that venue's engine ACCEPT this symbol?** Derived from [`vike_run::WIRED_MARKETS`],
    ///    the table `build_node`'s own `make_engine` calls read their venue+symbol from. ⚠ This is
    ///    not pedantry: `vike_mount::make_engine` sets no `extra_symbols`, so
    ///    [`vike_exec::ExecutionEngine::accepts_symbol`] is a plain equality test and a foreign
    ///    symbol's orders and fills are SILENTLY DROPPED — the strategy quotes, the daemon logs
    ///    nothing, and nothing ever trades. A row whose symbol is EMPTY is ACCOUNT-WIDE (polymarket:
    ///    exec/fills/reconcile key off the wallet, tokens resolve per order), so any symbol passes.
    /// 3. **Venue-specific SHAPE.** Polymarket outcome ids are dynamic ERC-1155 ids (a long DECIMAL
    ///    string), not a fixed symbol, so its account-wide row cannot answer (2) — gate on shape
    ///    (≥20 ASCII digits) instead. This is what keeps the shipped paper default (`token_id =
    ///    "TOK"`) un-armable.
    ///
    /// Extending the daemon to another venue remains a deliberate edit HERE (one
    /// [`LIVE_WIRED_VENUES`] row) **and** the matching `live_mount` arm — never a silent widening.
    /// Three gates, each over a different pair: `live_wired_venues_are_all_mounted_by_build_node`
    /// (this list ⊆ `vike_run::WIRED_MARKETS`, i.e. an ENGINE exists) and
    /// `every_unwired_venue_is_still_refused` (its converse over that table), plus
    /// `crates/vike-tradehub/tests/daemon/live_wired_venues_pin.rs` (this list == `live_mount`'s actual
    /// FEED arms) — which is the one that catches a row added here alone, and the one that did not
    /// exist until it was measured missing.
    pub fn validate_for_live(&self) -> Result<(), String> {
        // A `[[mounts]]` profile arms live only when EVERY row does — the same per-row refusals,
        // each failure naming its row (split-plane I10).
        if !self.mounts.is_empty() {
            for (i, row) in self.mount_rows().iter().enumerate() {
                row.validate_for_live().map_err(|e| {
                    format!(
                        "mounts[{i}] (venue={:?}, symbol={:?}): {e}",
                        row.venue(),
                        row.mount_symbol()
                    )
                })?;
            }
            return Ok(());
        }
        let venue = self.venue();
        let symbol = self.mount_symbol();
        if !LIVE_WIRED_VENUES.contains(&venue) {
            return Err(format!(
                "venue {venue:?} is not live-wired in this build; the daemon can mount: {}",
                LIVE_WIRED_VENUES.join(", ")
            ));
        }
        // (2) routing. `build_node` is what actually mounts the engines, so its table answers.
        let Some(&(_, wired_symbol)) = vike_run::WIRED_MARKETS.iter().find(|(v, _)| *v == venue)
        else {
            return Err(format!(
                "venue {venue:?} is live-wired for a feed but `build_node` mounts no engine for it \
                 — orders would have nowhere to go"
            ));
        };
        // ⚠ THE DEFAULT ACCOUNT ONLY. `build_node` mounts a venue's DEFAULT engine on the
        // `WIRED_MARKETS` symbol and sets no `extra_symbols`, so an account-less row naming
        // anything else is silently dropped — that refusal is unchanged, and it is why this daemon
        // can still reach exactly one instrument per venue on the default account.
        //
        // A row naming an ACCOUNT is a different question with a different answer:
        // `vike_run::account_symbols_for` mounts that account's engine on THIS ROW'S OWN symbol, so
        // `accepts_symbol` answers about the symbol the row named and there is nothing to refuse.
        // That is how a second instrument is reached, and refusing it here would have made the
        // account field unusable for the one thing it is for.
        let default_account = self.account.as_ref().is_none_or(|l| l.is_default());
        if default_account && !wired_symbol.is_empty() && wired_symbol != symbol {
            return Err(format!(
                "{venue} is mounted on {wired_symbol:?} by build_node, but this profile names \
                 {symbol:?}: that engine accepts exactly its mounted symbol \
                 (`ExecutionEngine::accepts_symbol` — no `extra_symbols` are wired), so every order \
                 and fill would be SILENTLY DROPPED. A row naming a second `account` is exempt — \
                 that account's engine is mounted on the row's own symbol"
            ));
        }
        // (3) venue SHAPE. Feature-free on purpose (profile shape only): the REAL capability gate
        // for polymarket is the `polymarket` cargo feature, without which `live_mount` hard-errors.
        if venue == "polymarket"
            && !(symbol.len() >= 20 && symbol.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err(format!(
                "polymarket needs an outcome token id (a ≥20-digit decimal ERC-1155 id), got \
                 {symbol:?}"
            ));
        }
        // (4) the DRAWDOWN LATCH needs a capital base. `live_mount` hardcodes
        // `CoreConfig::max_drawdown = Some(0.25)`, and `CoreThread::sweep_drawdown_latch` measures
        // that 25% as a fraction of `Σ seed_cash + own PnL` — configured capital, deliberately NOT
        // the venue's wallet, which on a shared account is not this daemon's money. So a
        // `seed_cash = 0` here leaves the live daemon's one automatic liquidate-only trip with no
        // denominator and it can never arm. Refused rather than warned: an operator reading a
        // profile that says nothing about drawdown believes the compiled-in 25% is protecting them.
        // (`seed_cash` is unset in most profiles, and its default is a positive 1000 — so this
        // fires only on an explicit zero/negative/NaN.)
        if let Some(seed) = self.seed_cash {
            // ⚠ Spelled `!is_finite() || <= 0.0` and NOT `<= 0.0`, which is what
            // `clippy::neg_cmp_op_on_partial_ord` suggests for the equivalent `!(seed > 0.0)`. Every
            // comparison against NaN is false, so `NaN <= 0.0` is FALSE and a NaN capital base would
            // sail through the guard that exists to stop exactly that. The lint is right that
            // `!(a > b)` is a smell on a partial order; the cure is to say which non-orderable values
            // are meant, not to drop them.
            //
            // This also refuses an INFINITE base, which `!(seed > 0.0)` accepted — deliberately, and
            // it is the same hazard: an infinite denominator makes the drop-fraction 0.0 forever, so
            // the latch can never arm. Nothing legitimate sets it.
            if !seed.is_finite() || seed <= 0.0 {
                return Err(format!(
                    "`seed_cash = {seed}` disarms the live daemon's 25% drawdown latch: it \
                     measures the drop as a fraction of configured capital plus own PnL, so a \
                     non-positive base leaves nothing to measure against. Omit it (default 1000) \
                     or set the capital this mount is meant to risk"
                ));
            }
        }
        Ok(())
    }

    /// Lower into the [`vike_run::MakerMountConfig`] the paper mount is built from: start from the
    /// recommended defaults for this venue's PRICE DOMAIN, then override ONLY the fields this profile
    /// set. Feature-free — no A-S internals are exposed here (operator A-S tuning is a follow-up);
    /// the recommended `AsParams` (with `resolution_ts`) applies.
    ///
    /// The domain choice is `[0,1]` for polymarket and `$`-scale for every other venue — see the
    /// comment on the branch, which is the one thing in this function that decides whether the
    /// mounted maker quotes at all.
    pub fn to_mount_config(&self) -> MakerMountConfig {
        // ⚠ POLYMARKET IS THE EXCEPTION, NOT THE RULE — and the test in that direction is what this
        // condition asserts. `::polymarket` tunes Avellaneda–Stoikov for `[0,1]` OUTCOME-TOKEN prices
        // (a Bernoulli variance cap and a `[tick, 1−tick]` wall clamp); `::crypto` tunes it for an
        // UNBOUNDED `$`-priced asset (Unbounded price domain + RawLocal variance + ConstantTau horizon
        // + a min-half-spread floor). Those are the only two price domains this daemon mounts, and a
        // `$`-scale asset priced through the `[0,1]` one does not quote AT ALL — every quote is wall-
        // clamped away from a $65k mid, so the maker mounts cleanly, logs a healthy feed, and posts
        // ZERO orders forever.
        //
        // ⚠ It USED to read `if self.venue == "hyperliquid"`, with polymarket as the catch-all. That
        // was correct only while hyperliquid was the one live-wired `$`-scale venue: binance, bybit
        // and okx are all `$`-scale, all now live-wired, and every one of them would have fallen into
        // the `[0,1]` arm and silently never quoted. Keying on the ONE venue that genuinely is a
        // `[0,1]` market makes the next `$`-scale venue correct by default instead of silently mute —
        // the failure mode this repo calls a silent do-nothing, and the direction the wrong default
        // must point.
        let mut cfg = if self.venue() == "polymarket" {
            let mut c = MakerMountConfig::polymarket(self.mount_symbol(), self.resolution_ts_ms);
            c.venue = self.venue().to_string();
            c
        } else {
            MakerMountConfig::crypto(
                self.venue(),
                self.mount_symbol(),
                self.tick_size.unwrap_or(1.0),
                self.qty.unwrap_or(0.005),
            )
        };
        if let Some(interval) = &self.interval {
            cfg.interval = interval.clone();
        }
        if let Some(ms) = self.interval_ms {
            cfg.interval_ms = ms;
        }
        if let Some(qty) = self.qty {
            cfg.qty = qty;
        }
        if let Some(half_spread) = self.half_spread {
            cfg.half_spread = half_spread;
        }
        if let Some(tick_size) = self.tick_size {
            cfg.tick_size = tick_size;
        }
        if let Some(seed_cash) = self.seed_cash {
            cfg.seed_cash = seed_cash;
        }
        cfg
    }

    /// Snapshot-summary cadence as a [`Duration`].
    pub fn summary_interval(&self) -> Duration {
        Duration::from_millis(self.daemon.summary_ms)
    }

    /// Bounded-shutdown deadline as a [`Duration`].
    pub fn shutdown_deadline(&self) -> Duration {
        Duration::from_millis(self.daemon.shutdown_deadline_ms)
    }
}

/// Resolve the OPERATOR risk budget to arm on the PAPER mount's `RiskGate` from an optional loaded
/// [`vike_core::RunProfile`] (RunProfile wiring, Settings STEP 2 PR 1, Task 2). This is the ONE
/// function `main.rs` calls before mounting — see its doc for why only `[risk]` is consumed from
/// the resolved profile (the daemon's own [`DaemonProfile`] already owns the venue/token_id/A-S
/// mount shape; a `RunProfile`'s `event_source`/`broker` sections are ignored here on purpose, ONLY
/// the risk budget is layered on top).
///
/// - `profile: None` (no `--profile` / `VIKE_RUN_PROFILE`) ⇒ `Ok(RiskLimits::new())` —
///   BYTE-IDENTICAL to the pre-Task-2 daemon's hardcoded value. This is the merge-safety property:
///   an operator who never opts into a profile sees no behavior change whatsoever.
/// - `profile: Some(p)` ⇒ `p.apply_risk(RiskLimits::new())`, which routes through
///   [`vike_core::RunProfile::apply_risk`] — the `GridSource` this call effectively uses is
///   whatever [`vike_core::RunProfile::grid_source`] derives from `p.mode`, NOT a value this
///   function picks itself. In practice that is [`vike_core::GridSource::NoGridFetched`] for a
///   `backtest`/`paper` profile (the PAPER mount this daemon builds never fetches a real venue
///   instrument grid, so the profile's `[risk]` table may also supply the instrument-grid fields
///   itself — `tick_size`/`lot_size`/`min_qty`/`min_notional` — alongside the operator-budget ones,
///   since nothing else on this mount ever will), and `VenueFetched` for a `mode = "live"` profile
///   — which `RunProfile::validate`'s unconditional live-mode check has already guaranteed carries
///   no instrument-grid fields to reject in the first place, so this is `Ok` either way for every
///   profile that passed `validate`. The `Result` return is kept for honesty (and so a caller need
///   not special-case an infallible-looking signature) rather than unwrapped here.
pub fn resolve_paper_risk_limits(profile: Option<&RunProfile>) -> Result<RiskLimits, ProfileError> {
    match profile {
        None => Ok(RiskLimits::new()),
        Some(p) => p.apply_risk(RiskLimits::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_profile_uses_polymarket_defaults() {
        let p = DaemonProfile::from_toml_str("token_id = \"TOK\"").expect("minimal profile parses");
        assert_eq!(p.venue(), "polymarket");
        assert_eq!(p.mount_symbol(), "TOK");
        assert!(p.strategy.is_none(), "no [strategy] table ⇒ the historical A-S maker");
        assert_eq!(p.resolution_ts_ms, None);
        assert_eq!(p.daemon.summary_ms, 5_000);
        assert_eq!(p.daemon.shutdown_deadline_ms, 5_000);

        // The MakerMountConfig::polymarket recommended defaults flow through untouched.
        let cfg = p.to_mount_config();
        assert_eq!(cfg.venue, "polymarket");
        assert_eq!(cfg.token_id, "TOK");
        assert_eq!(cfg.interval, "1m");
        assert_eq!(cfg.interval_ms, 60_000);
        assert_eq!(cfg.qty.to_bits(), 20.0_f64.to_bits());
        assert_eq!(cfg.tick_size.to_bits(), 0.01_f64.to_bits());
        assert_eq!(cfg.seed_cash.to_bits(), 1_000.0_f64.to_bits());
        assert_eq!(cfg.as_params.resolution_ts, None);
    }

    #[test]
    fn overrides_apply_to_the_mount_config() {
        let toml = r#"
venue = "polymarket"
token_id = "OUTCOME"
resolution_ts_ms = 1793491200000
interval = "5m"
interval_ms = 300000
qty = 50.0
half_spread = 0.02
tick_size = 0.01
seed_cash = 250.0

[daemon]
summary_ms = 2000
shutdown_deadline_ms = 3000
"#;
        let p = DaemonProfile::from_toml_str(toml).expect("full profile parses");
        assert_eq!(p.daemon.summary_ms, 2_000);
        assert_eq!(p.summary_interval(), Duration::from_millis(2_000));
        assert_eq!(p.shutdown_deadline(), Duration::from_millis(3_000));

        let cfg = p.to_mount_config();
        assert_eq!(cfg.token_id, "OUTCOME");
        assert_eq!(cfg.as_params.resolution_ts, Some(1_793_491_200_000));
        assert_eq!(cfg.interval, "5m");
        assert_eq!(cfg.interval_ms, 300_000);
        assert_eq!(cfg.qty.to_bits(), 50.0_f64.to_bits());
        assert_eq!(cfg.half_spread.to_bits(), 0.02_f64.to_bits());
        assert_eq!(cfg.seed_cash.to_bits(), 250.0_f64.to_bits());
    }

    #[test]
    fn empty_symbol_is_rejected_under_either_spelling() {
        for toml in ["token_id = \"\"", "symbol = \"\""] {
            let err = DaemonProfile::from_toml_str(toml).unwrap_err();
            assert!(err.contains("symbol"), "error must name the symbol: {err}");
        }
    }

    #[test]
    fn a_profile_with_no_symbol_at_all_is_rejected() {
        let err = DaemonProfile::from_toml_str("venue = \"polymarket\"").unwrap_err();
        assert!(err.contains("symbol"), "names what is missing: {err}");
    }

    #[test]
    fn setting_both_symbol_spellings_is_rejected() {
        // Two spellings of one field with different values has no defensible winner, and silently
        // picking one is how a live mount ends up on the instrument nobody typed.
        let err = DaemonProfile::from_toml_str("symbol = \"BTC\"\ntoken_id = \"TOK\"").unwrap_err();
        assert!(err.contains("not both"), "names the conflict: {err}");
    }

    /// BACK-COMPAT, the property the CI box's running paper daemon depends on: the SHIPPED profile shape
    /// — `token_id` with no `symbol` key and no `[strategy]` table — still parses and still lowers
    /// to the identical `MakerMountConfig`. Both example profiles in this repo are that shape.
    #[test]
    fn the_shipped_token_id_profile_shape_is_unchanged() {
        let shipped = r#"
venue = "polymarket"
token_id = "71321045679252212594626385532706912750332728571942532289631379312455583992563"
interval = "1m"
interval_ms = 60000
qty = 20.0
half_spread = 0.01
tick_size = 0.01
seed_cash = 1000.0

[daemon]
summary_ms = 5000
shutdown_deadline_ms = 5000
"#;
        let p = DaemonProfile::from_toml_str(shipped).expect("the shipped profile shape parses");
        assert!(p.strategy.is_none(), "no [strategy] ⇒ the A-S maker, as before");
        let cfg = p.to_mount_config();
        assert_eq!(cfg.venue, "polymarket");
        assert_eq!(
            cfg.token_id,
            "71321045679252212594626385532706912750332728571942532289631379312455583992563"
        );
        assert_eq!(cfg.qty.to_bits(), 20.0_f64.to_bits());
        assert_eq!(cfg.seed_cash.to_bits(), 1_000.0_f64.to_bits());
        // ...and it is still live-armable IN A BUILD THAT CAN MOUNT IT, which is the half a
        // venue-based gate could have broken.
        //
        // ⚠ The feature condition is a deliberate TIGHTENING, not a regression. `validate_for_live`
        // used to be feature-free and accepted a polymarket profile even in a build with no
        // polymarket `live_mount` arm; the daemon then hard-errored a few frames later, INSIDE the
        // mount. Refusing it here means the same outcome (a loud startup failure, never a silent
        // paper fallback) reported before anything is built, by the gate whose job it is.
        assert_eq!(
            p.validate_for_live().is_ok(),
            cfg!(feature = "polymarket"),
            "the shipped live profile shape stays armable wherever it can actually be mounted"
        );
    }

    /// The tightening above, stated as its own claim so it cannot be read as an accident: in a build
    /// that CANNOT mount polymarket, the refusal names the venue rather than the token shape.
    #[cfg(not(feature = "polymarket"))]
    #[test]
    fn a_default_build_refuses_polymarket_by_venue_not_by_token_shape() {
        let p = DaemonProfile::from_toml_str(
            "venue = \"polymarket\"\ntoken_id = \"71321045679252212594626385532706912750332728571942532289631379312455583992563\"",
        )
        .expect("parses");
        let err = p.validate_for_live().unwrap_err();
        assert!(err.contains("not live-wired in this build"), "names the real reason: {err}");
    }

    #[test]
    fn symbol_is_the_general_spelling_of_token_id() {
        let by_symbol =
            DaemonProfile::from_toml_str("venue = \"hyperliquid\"\nsymbol = \"BTC\"").unwrap();
        let by_token =
            DaemonProfile::from_toml_str("venue = \"hyperliquid\"\ntoken_id = \"BTC\"").unwrap();
        assert_eq!(by_symbol.mount_symbol(), by_token.mount_symbol());
        assert_eq!(by_symbol.to_mount_config().token_id, by_token.to_mount_config().token_id);
    }

    #[test]
    fn unknown_field_is_rejected() {
        // deny_unknown_fields catches a typo'd key rather than silently ignoring it.
        let err = DaemonProfile::from_toml_str("token_id = \"TOK\"\nbogus = 1").unwrap_err();
        assert!(!err.is_empty());
    }

    #[test]
    fn partial_daemon_table_keeps_the_other_default() {
        // Only one of the two daemon knobs set — the other must keep its default.
        let p = DaemonProfile::from_toml_str("token_id = \"TOK\"\n[daemon]\nsummary_ms = 1000")
            .expect("partial daemon table parses");
        assert_eq!(p.daemon.summary_ms, 1_000);
        assert_eq!(p.daemon.shutdown_deadline_ms, 5_000);
    }

    // ---------------------------------------------------------------------------------------------
    // The data-plane-only declaration (`data_only` — the data-only credential seam).
    // ---------------------------------------------------------------------------------------------

    /// The declaration parses on every eligible venue, lowers per `[[mounts]]` row, and the
    /// effective accessor reads absent and explicit `false` as the SAME (default) answer — the
    /// property `live_mount`'s withhold decision keys on.
    #[test]
    fn data_only_parses_on_eligible_venues_and_defaults_off() {
        for venue in DATA_ONLY_VENUES {
            let p = DaemonProfile::from_toml_str(&format!(
                "venue = \"{venue}\"\nsymbol = \"X\"\ndata_only = true"
            ))
            .unwrap_or_else(|e| panic!("{venue}: the declaration must parse: {e}"));
            assert!(p.data_only_effective(), "{venue}: an explicit true must read true");
        }
        let absent = DaemonProfile::from_toml_str("venue = \"oanda\"\nsymbol = \"X\"").unwrap();
        assert!(!absent.data_only_effective(), "absent is the default: exec follows credentials");
        let explicit_false =
            DaemonProfile::from_toml_str("venue = \"oanda\"\nsymbol = \"X\"\ndata_only = false")
                .unwrap();
        assert!(!explicit_false.data_only_effective(), "explicit false IS the default");
    }

    /// A keyless-data venue is REFUSED the declaration at load, naming the eligible set and the
    /// venue's own data-only path (withhold the credentials) — see `DATA_ONLY_VENUES`' doc for why
    /// this is a refusal rather than a widening.
    #[test]
    fn data_only_is_refused_on_a_keyless_data_venue_naming_the_eligible_set() {
        for venue in ["binance", "bybit", "okx", "deribit", "hyperliquid", "aster"] {
            let err = DaemonProfile::from_toml_str(&format!(
                "venue = \"{venue}\"\nsymbol = \"X\"\ndata_only = true"
            ))
            .unwrap_err();
            for needle in ["data_only", "keyless", "oanda"] {
                assert!(err.contains(needle), "{venue}: the refusal must carry {needle}: {err}");
            }
        }
    }

    /// Two `[[mounts]]` rows on ONE venue disagreeing on the declaration are refused at load,
    /// naming both rows — the withhold is per venue account, so no per-row split exists to grant.
    /// Rows AGREEING (or on different venues) pass.
    #[test]
    fn data_only_rows_sharing_a_venue_must_agree() {
        let disagree = "\
            [[mounts]]\nvenue = \"oanda\"\nsymbol = \"EURUSD\"\ndata_only = true\n\
            [[mounts]]\nvenue = \"oanda\"\nsymbol = \"EURUSD\"\ninterval = \"5m\"\n";
        let err = DaemonProfile::from_toml_str(disagree).unwrap_err();
        for needle in ["mounts[0]", "mounts[1]", "data_only"] {
            assert!(err.contains(needle), "the refusal must carry {needle}: {err}");
        }
        let agree = "\
            [[mounts]]\nvenue = \"oanda\"\nsymbol = \"EURUSD\"\ndata_only = true\n\
            [[mounts]]\nvenue = \"oanda\"\nsymbol = \"EURUSD\"\ninterval = \"5m\"\ndata_only = \
                     true\n";
        DaemonProfile::from_toml_str(agree).expect("agreeing rows are one venue-wide declaration");
    }

    /// A top-level `data_only` beside a `[[mounts]]` array joins the both-spellings refusal — the
    /// key would configure nothing while reading as real, exactly like every other top-level
    /// mount field there.
    #[test]
    fn data_only_joins_the_both_spellings_refusal() {
        let err = DaemonProfile::from_toml_str(
            "data_only = true\n[[mounts]]\nvenue = \"oanda\"\nsymbol = \"EURUSD\"\n",
        )
        .unwrap_err();
        assert!(err.contains("data_only"), "the refusal must name the offending key: {err}");
        assert!(err.contains("[[mounts]]"), "…and the spelling conflict: {err}");
    }

    /// Every eligible venue is live-wired — the declaration can only name venues `live_mount` has
    /// a feed arm for, in both feature builds (the subset relation, not a hand copy of either
    /// list).
    #[test]
    fn data_only_venues_are_a_subset_of_the_live_wired_set() {
        for venue in DATA_ONLY_VENUES {
            assert!(
                LIVE_WIRED_VENUES.contains(venue),
                "{venue} is declared data-only-eligible but is not live-wired at all"
            );
        }
    }

    // ---------------------------------------------------------------------------------------------
    // Audit F5 — the wired-set SYNC gate (the CLAUDE.md capability-map STEP-1 pattern).
    //
    // The gate SURVIVED the allow-list becoming a venue question; it did not get deleted with it.
    // What changed is which half is hand-written: the VENUE list is (one row per wired feed arm),
    // the SYMBOL is derived from `build_node`'s own table — so the daemon can no longer disagree
    // with the node about which symbol an engine accepts, which is what the old hardcoded "BTC"
    // literal made possible.
    // ---------------------------------------------------------------------------------------------

    /// Build a profile directly (no TOML round-trip) so table-driven probing can name any
    /// `(venue, symbol)` pair. Every non-identity field stays at the parse-time default.
    fn profile_for(venue: &str, symbol: &str) -> DaemonProfile {
        DaemonProfile {
            venue: Some(venue.to_string()),
            symbol: Some(symbol.to_string()),
            token_id: None,
            strategy: None,
            resolution_ts_ms: None,
            interval: None,
            interval_ms: None,
            qty: None,
            half_spread: None,
            tick_size: None,
            seed_cash: None,
            data_only: None,
            account: None,
            daemon: DaemonSettings::default(),
            mounts: Vec::new(),
        }
    }

    /// Direction 1 — every venue the daemon will arm LIVE is a venue `build_node` actually mounts an
    /// engine for, pinned against [`vike_run::WIRED_MARKETS`]. Without this a `LIVE_WIRED_VENUES`
    /// row could name a venue with a feed but no engine: the strategy would quote and every order
    /// would go nowhere.
    #[test]
    fn live_wired_venues_are_all_mounted_by_build_node() {
        for venue in LIVE_WIRED_VENUES {
            assert!(
                vike_run::WIRED_MARKETS.iter().any(|(v, _)| v == venue),
                "{venue} is on the daemon's live venue list but build_node mounts no engine for it \
                 — orders would have nowhere to go: {:?}",
                vike_run::WIRED_MARKETS
            );
        }
    }

    /// Direction 2 — COMPLETENESS over the node table: every venue `build_node` mounts but this
    /// daemon has NOT wired a feed for must still be REFUSED. This is the anti-silent-widening half:
    /// a venue quietly added to `LIVE_WIRED_VENUES` without a `live_mount` arm surfaces here as an
    /// unexpectedly-accepted row.
    #[test]
    fn every_unwired_venue_is_still_refused() {
        for &(venue, symbol) in vike_run::WIRED_MARKETS {
            if LIVE_WIRED_VENUES.contains(&venue) {
                continue; // the wired venues, asserted by their own tests below
            }
            let p = profile_for(venue, if symbol.is_empty() { "X" } else { symbol });
            assert!(
                p.validate_for_live().is_err(),
                "({venue}, {symbol}) is in WIRED_MARKETS but has no daemon feed arm — it must stay \
                 refused; if it was deliberately live-wired, extend LIVE_WIRED_VENUES AND live_mount"
            );
        }
    }

    /// The SYMBOL half, now DERIVED rather than hardcoded. `build_node` mounts hyperliquid on one
    /// symbol and `make_engine` wires no `extra_symbols`, so any other symbol on that venue would be
    /// dropped at `ExecutionEngine::accepts_symbol` with no error anywhere — the silent-no-trade
    /// failure this check exists to convert into a startup error.
    #[test]
    fn a_foreign_symbol_on_a_wired_venue_is_refused_by_the_node_table() {
        let &(_, hl_symbol) = vike_run::WIRED_MARKETS
            .iter()
            .find(|(v, _)| *v == "hyperliquid")
            .expect("build_node mounts hyperliquid");
        assert!(profile_for("hyperliquid", hl_symbol).validate_for_live().is_ok());

        let foreign = format!("{hl_symbol}-NOT-THE-MOUNTED-ONE");
        let err = profile_for("hyperliquid", &foreign).validate_for_live().unwrap_err();
        assert!(err.contains("SILENTLY DROPPED"), "names the real failure mode: {err}");
        assert!(err.contains(hl_symbol), "names the symbol build_node actually mounts: {err}");
    }

    /// ⚠ `live_mount` hardcodes `CoreConfig::max_drawdown = Some(0.25)`, and
    /// `CoreThread::sweep_drawdown_latch` measures that 25% against `Σ seed_cash + own PnL` — NOT
    /// against the venue wallet, which on a shared account is not this daemon's money. So a
    /// `seed_cash = 0` here leaves the live daemon's one automatic liquidate-only trip with no
    /// denominator, and an operator reading a profile that says nothing about drawdown believes the
    /// compiled-in 25% is protecting them. Refused, not warned.
    #[test]
    fn a_non_positive_seed_cash_is_refused_for_live_because_it_disarms_the_drawdown_latch() {
        let &(_, hl_symbol) = vike_run::WIRED_MARKETS
            .iter()
            .find(|(v, _)| *v == "hyperliquid")
            .expect("build_node mounts hyperliquid");
        // ⚠ `nan` and `inf` are load-bearing rows, not padding. TOML spells both, and both defeat a
        // FRACTIONAL threshold in ways `seed <= 0.0` does not catch: every comparison against NaN is
        // false, so a naive `<= 0.0` waves NaN through, and an infinite base makes the drop-fraction
        // 0.0 forever. Neither was covered before — the guard's own comment claimed the NaN case
        // while nothing asserted it.
        for bad in ["0.0", "-1.0", "nan", "inf", "-inf"] {
            let p = DaemonProfile::from_toml_str(&format!(
                "venue = \"hyperliquid\"\ntoken_id = \"{hl_symbol}\"\nseed_cash = {bad}"
            ))
            .expect("it PARSES — this is a semantic refusal, not a schema one");
            let err = p.validate_for_live().unwrap_err();
            assert!(err.contains("drawdown latch"), "names what it disarms: {err}");
        }
        // OMITTED is fine (the default is a positive 1000), and so is an explicit positive value —
        // otherwise this guard would be a silent tightening of every profile in the wild.
        assert!(profile_for("hyperliquid", hl_symbol).validate_for_live().is_ok());
        let ok = DaemonProfile::from_toml_str(&format!(
            "venue = \"hyperliquid\"\ntoken_id = \"{hl_symbol}\"\nseed_cash = 250.0"
        ))
        .expect("parses");
        assert!(ok.validate_for_live().is_ok(), "a positive base arms the latch");
    }

    #[test]
    fn validate_for_live_accepts_the_wired_pairs() {
        // hyperliquid on build_node's own HL market.
        let hl = DaemonProfile::from_toml_str("venue = \"hyperliquid\"\ntoken_id = \"BTC\"")
            .expect("hyperliquid/BTC profile parses");
        assert!(hl.validate_for_live().is_ok(), "hyperliquid/BTC must be live-wireable");

        // polymarket/<real outcome token id> — a long decimal ERC-1155 id (the maker's native
        // domain). Its build_node row is ACCOUNT-WIDE (empty symbol), so the SHAPE gate is what
        // answers instead. ⚠ Only under the `polymarket` feature: a default build has no feed arm.
        let poly = DaemonProfile::from_toml_str(
            "venue = \"polymarket\"\ntoken_id = \"71321045679252212594626385532706912750332728571942532289631379312455583992563\"",
        )
        .expect("polymarket/token profile parses");
        assert_eq!(
            poly.validate_for_live().is_ok(),
            cfg!(feature = "polymarket"),
            "polymarket is live-wireable exactly when its feed arm is compiled in"
        );

        // The three CEX venues, each on the symbol `build_node` actually mounts it on. DERIVED from
        // `WIRED_MARKETS` rather than restated, so these rows cannot drift from the engine table.
        for venue in ["binance", "bybit", "okx"] {
            let &(_, symbol) = vike_run::WIRED_MARKETS
                .iter()
                .find(|(v, _)| *v == venue)
                .unwrap_or_else(|| panic!("build_node mounts {venue}"));
            let p = profile_for(venue, symbol);
            assert!(
                p.validate_for_live().is_ok(),
                "{venue}/{symbol} must be live-wireable — it has a `live_mount` feed arm"
            );
            // ...and the refusal is still SYMBOL-scoped, not venue-scoped: a live-wired venue on a
            // foreign symbol stays refused, because `accepts_symbol` is a plain equality test.
            let err =
                profile_for(venue, &format!("{symbol}-NOPE")).validate_for_live().unwrap_err();
            assert!(err.contains("SILENTLY DROPPED"), "{venue} names the real failure mode: {err}");
        }

        // ⚠ HAVING AN ENGINE IS NOT SUFFICIENT — the property that makes [`LIVE_WIRED_VENUES`] a
        // gate rather than a description: the venue check runs FIRST, before the `WIRED_MARKETS`
        // routing lookup, so a venue `build_node` mounts an engine for is still refused by NAME
        // when `live_mount` wires it no feed.
        //
        // This used to be spelled with deribit as the standing example, and split-plane I9 wired
        // deribit's feed — so the exemplar is DERIVED now. Both populations are checked because
        // either one can be empty as the two tables converge, and an `is_empty()` loop is the
        // vacuous-gate shape this repo has been bitten by: engine-but-no-feed venues prove the
        // ordering directly, and roster venues with NEITHER prove the same refusal branch survives
        // the day that first set empties out. The floor below is what stops both going silent.
        let engine_but_no_feed: Vec<&str> = vike_run::WIRED_MARKETS
            .iter()
            .map(|&(v, _)| v)
            .filter(|v| !LIVE_WIRED_VENUES.contains(v))
            .collect();
        let neither: Vec<&str> = vike_model::VENUES
            .iter()
            .copied()
            .filter(|v| {
                !LIVE_WIRED_VENUES.contains(v)
                    && !vike_run::WIRED_MARKETS.iter().any(|(w, _)| w == v)
            })
            .collect();
        assert!(
            !engine_but_no_feed.is_empty() || !neither.is_empty(),
            "every roster venue is both engine-wired and feed-wired — the venue gate has no \
             witness left in this build, so re-derive this assertion rather than deleting it"
        );
        for venue in engine_but_no_feed.iter().chain(neither.iter()) {
            let err = profile_for(venue, "ANY-SYMBOL").validate_for_live().unwrap_err();
            assert!(
                err.contains("not live-wired"),
                "{venue} has no `live_mount` feed arm and must be refused at the VENUE gate, \
                 before the symbol is ever looked at: {err}"
            );
        }

        // The DEFAULT (polymarket) paper profile with a PLACEHOLDER token is NOT live-wireable: the
        // shape gate (≥20 ASCII digits) refuses "TOK", so the paper default can't be accidentally
        // armed. (Under a default build the venue gate refuses it first — either way it is refused.)
        let default_paper = DaemonProfile::from_toml_str("token_id = \"TOK\"").expect("parses");
        assert!(
            default_paper.validate_for_live().is_err(),
            "the placeholder paper default must not be live-wireable"
        );
        // A polymarket profile with a too-short / non-numeric token is also rejected (shape gate).
        let bad_token =
            DaemonProfile::from_toml_str("venue = \"polymarket\"\ntoken_id = \"12345\"")
                .expect("parses");
        assert!(bad_token.validate_for_live().is_err(), "a non-token-shaped id must be rejected");
    }

    /// **Every live-wired venue except polymarket must lower to the UNBOUNDED price domain.**
    ///
    /// This is a silent-do-nothing gate, not a style check. `MakerMountConfig::polymarket` tunes
    /// Avellaneda–Stoikov for `[0,1]` outcome-token prices, and its `PriceDomain::UnitInterval` wall
    /// clamps every quote into `[tick, 1-tick]`. Lower a `$`-scale venue through it and the maker
    /// mounts cleanly, subscribes a healthy feed, and then posts NOTHING — forever, with no error,
    /// because every quote it computes is clamped away from a $65k mid.
    ///
    /// `to_mount_config` used to key on `venue == "hyperliquid"` with polymarket as the CATCH-ALL,
    /// which was correct only while hyperliquid was the single live-wired `$`-scale venue. Wiring
    /// binance/bybit/okx made three more, all of which would have landed in the `[0,1]` arm. Driving
    /// this off `LIVE_WIRED_VENUES` rather than a literal list means the NEXT venue added is covered
    /// the moment its row lands — a venue cannot be wired and silently muted by the same PR.
    #[test]
    fn every_live_wired_dollar_scale_venue_gets_the_unbounded_price_domain() {
        use vike_model::PriceDomain;

        let mut checked = 0;
        for venue in LIVE_WIRED_VENUES {
            let &(_, wired_symbol) = vike_run::WIRED_MARKETS
                .iter()
                .find(|(v, _)| v == venue)
                .unwrap_or_else(|| panic!("{venue} is live-wired but build_node mounts no engine"));
            // Polymarket's row is ACCOUNT-WIDE (empty symbol) and it IS the `[0,1]` market — the one
            // venue that must keep the bounded domain. Assert that, then skip the $-scale check.
            if *venue == "polymarket" {
                let p = profile_for(
                    venue,
                    "71321045679252212594626385532706912750332728571942532289631379312455583992563",
                );
                assert_eq!(
                    p.to_mount_config().as_params.price_domain,
                    PriceDomain::UnitInterval,
                    "polymarket is the [0,1] market and must NOT be moved to the $-scale domain"
                );
                checked += 1;
                continue;
            }
            let cfg = profile_for(venue, wired_symbol).to_mount_config();
            assert_eq!(
                cfg.as_params.price_domain,
                PriceDomain::Unbounded,
                "{venue} is a $-scale venue: a UnitInterval domain clamps every quote away from its \
                 mid, so the maker would mount, look healthy, and post ZERO orders forever"
            );
            assert_eq!(cfg.venue, *venue, "{venue}'s mount config must carry its own venue string");
            checked += 1;
        }
        assert_eq!(
            checked,
            LIVE_WIRED_VENUES.len(),
            "every live-wired venue must be classified, not skipped"
        );
    }

    // ---------------------------------------------------------------------------------------------
    // The `[strategy]` table.
    // ---------------------------------------------------------------------------------------------

    fn strategy_profile(name: &str) -> Result<DaemonProfile, String> {
        DaemonProfile::from_toml_str(&format!(
            "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"{name}\"\n"
        ))
    }

    /// The headline: a strategy OTHER than the A-S maker resolves and lowers to a mountable box.
    #[test]
    fn a_registered_strategy_resolves_to_a_mountable_box() {
        let p = strategy_profile("grid").expect("a grid profile parses");
        let cfg = p.to_mount_config();
        assert!(p.resolve_strategy(&cfg).is_ok(), "grid must resolve into the mount box");
        // ...and so does every other name the registry says is live-capable, from ONE table.
        for (name, why) in vike_strategy::LIVE_CAPABLE {
            if why.is_some() {
                continue;
            }
            let p = strategy_profile(name).unwrap_or_else(|e| panic!("{name} profile: {e}"));
            assert!(p.resolve_strategy(&p.to_mount_config()).is_ok(), "{name} must resolve");
        }
    }

    /// The A-S maker is now ONE registered strategy rather than the hardcoded one — reachable by
    /// name, exactly like the others.
    #[test]
    fn the_as_maker_is_reachable_by_name() {
        let p = strategy_profile("spread_maker").expect("parses");
        assert!(p.resolve_strategy(&p.to_mount_config()).is_ok());
    }

    // ---------------------------------------------------------------------------------------------
    // The two spellings of the A-S maker (round-2 review, blocker 1).
    //
    // `[strategy] name = "spread_maker"` used to resolve through the REGISTRY arm
    // (`SpreadMaker::from_params`), which reads `[strategy.params]` and NOTHING else — so on this
    // daemon it mounted `qty = 1`, `tick_size = 0` and the `[0,1]` wall clamp instead of the
    // venue-selected `AsParams`, while the very same profile with NO `[strategy]` table mounted the
    // configured maker. MEASURED on a hyperliquid profile: registry `qty=1 tick=0` /
    // `PriceDomain::UnitInterval` vs default `qty=0.005 tick=1.0` / `PriceDomain::Unbounded` — the
    // configuration that posts ZERO orders on a $64k asset, under a startup log saying
    // `strategy = spread_maker`.
    // ---------------------------------------------------------------------------------------------

    /// The headline property: naming the maker and not naming it are ONE construction, so they
    /// cannot produce different makers. Compared on `SpreadMakerParams` — the maker's whole
    /// observable knob surface, including the A-S bag — not on a hand-picked field or two.
    #[test]
    fn the_two_spellings_of_the_as_maker_are_one_construction() {
        for venue_toml in [
            "venue = \"hyperliquid\"\nsymbol = \"BTC\"\ntick_size = 1.0\nqty = 0.005\n",
            "venue = \"polymarket\"\ntoken_id = \"TOK\"\nqty = 20.0\nresolution_ts_ms = 1793491200000\n",
        ] {
            let default_path = DaemonProfile::from_toml_str(venue_toml).expect("parses");
            let named = DaemonProfile::from_toml_str(&format!(
                "{venue_toml}[strategy]\nname = \"spread_maker\"\n"
            ))
            .expect("parses");

            // ⚠ Through `resolve_mount`, the function `main` actually calls (via
            // `resolve_strategy`) — NOT through the helper. Asserting on the helper alone would be
            // circular: the defect was that the named spelling took the OTHER route.
            let maker_of = |p: &DaemonProfile| match p
                .resolve_mount(&p.to_mount_config())
                .expect("resolves")
            {
                MountedStrategy::AsMaker(m) => m.params(),
                MountedStrategy::Registered(_) | MountedStrategy::Script { .. } => panic!(
                    "the A-S maker came from the REGISTRY arm, which reads `[strategy.params]` \
                     alone — it would mount qty=1 / tick_size=0 / the [0,1] wall clamp instead of \
                     this profile's maker fields"
                ),
            };
            let a = maker_of(&default_path);
            let b = maker_of(&named);
            assert_eq!(
                a, b,
                "`[strategy] name = \"spread_maker\"` must mount the SAME maker as no [strategy] \
                 table at all, for {venue_toml:?}"
            );

            // ...and it is the PROFILE's maker, not a defaults-only one: the qty the profile states
            // is the qty that mounts. (The registry arm's `SpreadMaker::from_params` on an empty
            // params table yields `qty = 1.0`, which is what made this a live hazard.)
            let cfg = named.to_mount_config();
            assert_eq!(b.qty.to_bits(), cfg.qty.to_bits(), "the profile's qty is the mounted qty");
            assert_eq!(
                b.avellaneda_stoikov.expect("A-S is on").price_domain,
                cfg.as_params.price_domain,
                "the VENUE-selected price domain is the mounted one — the [0,1] wall clamp on a \
                 $-scale asset is what posted zero orders"
            );
        }
    }

    /// `gueant_maker` is the same maker with the GLFT closed form selected — the registry's own
    /// definition of the alias, applied to the PROFILE's config rather than to a default one.
    #[test]
    fn the_gueant_alias_is_the_same_maker_with_the_glft_model() {
        let base = "venue = \"hyperliquid\"\nsymbol = \"BTC\"\ntick_size = 1.0\nqty = 0.005\n";
        let plain = DaemonProfile::from_toml_str(base).expect("parses");
        let gueant =
            DaemonProfile::from_toml_str(&format!("{base}[strategy]\nname = \"gueant_maker\"\n"))
                .expect("parses");
        let a = plain.mounted_maker(&plain.to_mount_config()).expect("maker").params();
        let g = gueant.mounted_maker(&gueant.to_mount_config()).expect("maker").params();
        let (a_as, g_as) = (a.avellaneda_stoikov.expect("A-S"), g.avellaneda_stoikov.expect("A-S"));
        assert_eq!(g_as.spread_model, SpreadModel::Gueant, "the alias selects GLFT");
        assert_ne!(a_as.spread_model, g_as.spread_model, "…and that is the ONLY difference:");
        assert_eq!(
            g.qty.to_bits(),
            a.qty.to_bits(),
            "…the profile's own maker fields still reach it"
        );
        assert_eq!(SpreadModel::Gueant, g_as.spread_model);
        assert_eq!(
            vike_model::AsParams { spread_model: a_as.spread_model, ..g_as },
            a_as,
            "gueant_maker differs from the default mount in the spread model and nothing else"
        );
    }

    /// A strategy the registry resolves normally is NOT diverted through the maker path — the
    /// routing above must be exactly the two maker names, never a catch-all.
    #[test]
    fn a_non_maker_strategy_is_not_diverted_through_the_maker_path() {
        let p = strategy_profile("grid").expect("parses");
        assert!(
            matches!(
                p.resolve_mount(&p.to_mount_config()).expect("resolves"),
                MountedStrategy::Registered(_)
            ),
            "`grid` must resolve through the registry, not as the A-S maker"
        );
        assert!(p.resolve_strategy(&p.to_mount_config()).is_ok());
    }

    // ---------------------------------------------------------------------------------------------
    // `[strategy.params]` strictness (round-2 review, blocker 2).
    // ---------------------------------------------------------------------------------------------

    /// The maker names take NO params here, because there is nowhere for them to go: the maker is
    /// built from the profile's own fields, so a `[strategy.params]` table would be silently
    /// dropped — and a dropped `qty` is a live order at the compiled default.
    #[test]
    fn the_maker_names_refuse_a_params_table() {
        for name in AS_MAKER_NAMES {
            let err = DaemonProfile::from_toml_str(&format!(
                "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"{name}\"\n\n\
                 [strategy.params]\nqty = 0.005\ngamma = 0.3\n"
            ))
            .unwrap_err();
            assert!(err.contains("takes no `[strategy.params]`"), "{name}: {err}");
            assert!(err.contains("gamma"), "{name} names the offending keys: {err}");
            assert!(err.contains("tick_size"), "{name} names where the knobs DO live: {err}");
            // An EMPTY table is fine — it configures nothing and asks for nothing.
            assert!(DaemonProfile::from_toml_str(&format!(
                "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"{name}\"\n\n[strategy.params]\n"
            ))
            .is_ok());
        }
    }

    /// A params key the strategy does not read is REFUSED at load, not ignored. `deny_unknown_fields`
    /// stops at the `[strategy.params]` boundary (the field is a free-form `toml::Value`), so
    /// without this a mistyped size knob mounts at the compiled default with only the OPTIONAL
    /// `policy.max_notional_per_order` behind it.
    #[test]
    fn an_unread_params_key_is_refused_with_the_readable_set() {
        let err = DaemonProfile::from_toml_str(
            "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"grid\"\n\n\
             [strategy.params]\nstep = 0.5\nsizee = 2.0\n",
        )
        .unwrap_err();
        assert!(err.contains("sizee"), "names the typo: {err}");
        assert!(err.contains("COMPILED DEFAULT"), "names the consequence: {err}");
        assert!(err.contains("band"), "names what it CAN read: {err}");
        // ...and the same table with the key spelled right is accepted.
        assert!(DaemonProfile::from_toml_str(
            "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"grid\"\n\n\
             [strategy.params]\nstep = 0.5\nsize = 2.0\n",
        )
        .is_ok());
    }

    /// The strictness is the SAME on a paper profile as on a live one, and stated as its own claim:
    /// a rehearsal that ran different parameters would conceal precisely what it exists to show.
    #[test]
    fn the_params_gate_does_not_depend_on_the_live_gate() {
        // The default (polymarket paper) venue, a placeholder token — a profile `validate_for_live`
        // refuses outright — is refused by the PARAMS rule at load all the same.
        let err = DaemonProfile::from_toml_str(
            "token_id = \"TOK\"\n[strategy]\nname = \"buy_hold\"\n\n[strategy.params]\nsizee = 1.0\n",
        )
        .unwrap_err();
        assert!(err.contains("sizee"), "the paper path is just as strict: {err}");
        // ...and so is the TYPE half, on the same paper profile.
        let err = DaemonProfile::from_toml_str(
            "token_id = \"TOK\"\n[strategy]\nname = \"buy_hold\"\n\n[strategy.params]\nsize = \"1\"\n",
        )
        .unwrap_err();
        assert!(err.contains("wrong TYPE"), "the paper path is just as strict: {err}");
    }

    /// A key the strategy DOES read, at a type its reader cannot take, is refused at load — naming
    /// the key, what it got and what the reader wants. These four inputs are the review's own
    /// examples, verbatim; each of them used to mount the compiled default with the profile stating
    /// otherwise, which is the "a live order at a size nobody typed" consequence the key check was
    /// added for, reached through the single most ordinary TOML slip there is.
    #[test]
    fn a_mistyped_params_value_is_refused_naming_both_types() {
        let profile = |name: &str, params: &str| {
            DaemonProfile::from_toml_str(&format!(
                "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"{name}\"\n\n\
                 [strategy.params]\n{params}\n"
            ))
        };
        for (name, params, key, got) in [
            ("grid", "size  = \"2\"", "size", "string"),
            ("grid", "rungs = 4.0", "rungs", "float"),
            ("grid", "band  = true", "band", "boolean"),
            ("buy_hold", "size  = \"3\"", "size", "string"),
        ] {
            let err = profile(name, params).unwrap_err();
            assert!(err.contains("wrong TYPE"), "{params}: {err}");
            assert!(err.contains(&format!("`{key}`")), "names the key: {err}");
            assert!(err.contains(&format!("got {got}")), "names what it got: {err}");
            assert!(err.contains("COMPILED DEFAULT"), "names the consequence: {err}");
        }
        // The expected type is named too, and it is the one the reader really wants — `rungs` is
        // `Value::as_integer`, so the message must say integer and not merely "a number".
        let err = profile("grid", "rungs = 4.0").unwrap_err();
        assert!(err.contains("wants an integer"), "{err}");
        let err = profile("grid", "size  = \"2\"").unwrap_err();
        assert!(err.contains("wants a number (integer or float)"), "{err}");

        // ...and every spelling the reader ACTUALLY accepts still loads. A rule refusing `size = 2`
        // where `as_f64` happily takes it would break working profiles, which is the failure mode
        // the type table is read off the source to avoid.
        for params in ["size = 2", "size = 2.0", "rungs = 4", "band = 3", "band = 3.5"] {
            assert!(profile("grid", params).is_ok(), "`{params}` must still load");
        }
    }

    /// A params key that names a market this mount does not trade is refused at load, NAMING BOTH.
    ///
    /// ⚠ The reviewer's own probe, verbatim, is the first row: `symbol = "MOUNTED_SYMBOL"` at the
    /// top level and `[strategy.params] symbol = "A_COMPLETELY_DIFFERENT_SYMBOL"`. MEASURED on the CI box
    /// before this refusal existed, that profile LOADED, announced
    /// `size=3 symbol=A_COMPLETELY_DIFFERENT_SYMBOL`, and filled on `MOUNTED_SYMBOL` — a startup
    /// line naming one instrument while the orders hit another.
    ///
    /// The venue half is the same defect on the same rule: a `momentum` mount's `venue`/`venues`
    /// are read by `ControllerHarness` and then discarded by the core's `resolve_intent_venue`.
    ///
    /// MUTATION: delete the `misrouted_params` block from [`DaemonProfile::validate_strategy`] and
    /// every row below goes red — the profiles all parse, all type-check, and all mount.
    #[test]
    fn a_params_key_naming_another_market_is_refused_naming_both() {
        // (venue, symbol, strategy, params, the two names the message must carry)
        for (venue, symbol, name, params, named, mounted) in [
            (
                "polymarket",
                "MOUNTED_SYMBOL",
                "buy_hold",
                "size = 3\nsymbol = \"A_COMPLETELY_DIFFERENT_SYMBOL\"",
                "A_COMPLETELY_DIFFERENT_SYMBOL",
                "MOUNTED_SYMBOL",
            ),
            ("hyperliquid", "BTC", "grid", "symbol = \"ETH\"", "ETH", "BTC"),
            ("hyperliquid", "BTC", "dca_accumulate", "symbol = \"ETH\"", "ETH", "BTC"),
            // The VENUE half — `momentum` has no `symbol` key at all, so this row also proves the
            // rule is not "symbol only".
            ("hyperliquid", "BTC", "momentum", "venue = \"binance\"", "binance", "hyperliquid"),
            // ...and one ROW of the routing table, which is a `(symbol, venue)` pair.
            (
                "hyperliquid",
                "BTC",
                "momentum",
                "venues = { ETH = \"binance\" }",
                "binance",
                "hyperliquid",
            ),
        ] {
            let err = DaemonProfile::from_toml_str(&format!(
                "venue = \"{venue}\"\nsymbol = \"{symbol}\"\n[strategy]\nname = \"{name}\"\n\n\
                 [strategy.params]\n{params}\n"
            ))
            .unwrap_err();
            assert!(err.contains(named), "{name}/{params}: names what the profile said: {err}");
            assert!(err.contains(mounted), "{name}/{params}: names what is mounted: {err}");
            assert!(
                err.contains("configures NOTHING"),
                "{name}/{params}: names the consequence: {err}"
            );
        }

        // ...and the AGREEING spellings still load, or the rule would be refusing correct profiles.
        // `symbol` restating the mount is a no-op; an ABSENT one is the working default; an EMPTY
        // one is a mount that cannot trade, which `resolved_params` reports rather than refuses.
        for params in ["symbol = \"BTC\"", "size = 1.0", "symbol = \"\""] {
            assert!(
                DaemonProfile::from_toml_str(&format!(
                    "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"buy_hold\"\n\n\
                     [strategy.params]\n{params}\n"
                ))
                .is_ok(),
                "`{params}` names this mount's own market and must still load"
            );
        }
        for params in ["venue = \"hyperliquid\"", "qty = 1.0", "venues = { BTC = \"hyperliquid\" }"]
        {
            assert!(
                DaemonProfile::from_toml_str(&format!(
                    "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"momentum\"\n\n\
                     [strategy.params]\n{params}\n"
                ))
                .is_ok(),
                "`{params}` names this mount's own market and must still load"
            );
        }
    }

    /// A params table that describes NO order at all is refused at load — carrying the resolution,
    /// so the operator can see which knob left the ladder empty.
    ///
    /// ⚠ Both dead configurations were MEASURED, as zero broker calls over a scripted market,
    /// before this refusal existed (`crates/vike-strategy/tests/param_gates.rs`'s `DEAD` ledger).
    /// They are the quietest failure this daemon has: the profile parses, every key is spelled,
    /// typed and routed right, the mount line prints a full configuration — and nothing is ever
    /// submitted, on any market, at any price.
    ///
    /// MUTATION: delete the `unarmable_params` block from [`DaemonProfile::validate_strategy`] and
    /// every row below goes red — the profiles all parse, all type-check, all route and all mount.
    #[test]
    fn a_ladder_that_can_never_rest_a_rung_is_refused() {
        let profile = |name: &str, params: &str| {
            DaemonProfile::from_toml_str(&format!(
                "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"{name}\"\n\n\
                 [strategy.params]\n{params}\n"
            ))
        };
        // (strategy, params, the resolution the message must show)
        for (name, params, shown) in [
            // A FIXED anchor with the price left at its compiled `0`: every long rung prices at or
            // below zero and is skipped, and the anchor is stamped anyway so it never re-arms.
            ("dca_accumulate", "anchor = \"fixed\"", "anchor_price=0"),
            // A 0..1 grid at the compiled `step = 1.0`: one rung spacing spans the whole domain.
            ("grid", "bounded01 = true", "step=1"),
            // ...and the degenerate ladder, the same defect through the other guard. `rungs = -5`
            // is here because `read_rungs` CLAMPS it to zero — the input the echo test used to
            // demonstrate that clamp with, now refused before there is a mount line to read.
            ("grid", "rungs = 0", "rungs=0"),
            ("grid", "rungs = -5", "rungs=0"),
            ("dca_accumulate", "size = 0.0", "size=0"),
        ] {
            let err = profile(name, params).unwrap_err();
            assert!(err.contains("NO rung"), "{name}/{params}: names what is wrong: {err}");
            assert!(err.contains("never place an order"), "{name}/{params}: {err}");
            assert!(err.contains(shown), "{name}/{params}: carries the resolution: {err}");
            assert!(
                !err.contains("wrong TYPE") && !err.contains("does not read"),
                "{name}/{params}: this is the case the other three PASS: {err}"
            );
        }

        // ...and the near-misses must still load, or this is a rule about suspicious VALUES rather
        // than about an empty ladder — the over-refusal `unarmable_params`' doc argues against.
        for (name, params) in [
            // A SHORT ladder anchored at zero steps AWAY from it and rests real rungs.
            ("dca_accumulate", "anchor = \"fixed\"\nside = \"short\"\nstep = 0.05"),
            // A bounded grid whose step FITS inside the walls.
            ("grid", "bounded01 = true\nstep = 0.05"),
            // A fixed anchor with a real price on it.
            ("dca_accumulate", "anchor = \"fixed\"\nanchor_price = 40.0"),
            // ...and the ordinary tables, which is what makes every refusal above a contrast.
            ("grid", "rungs = 4\nstep = 0.5"),
            ("dca_accumulate", "rungs = 4\nstep = 0.5"),
        ] {
            assert!(profile(name, params).is_ok(), "`{name}` / `{params}` must still load");
        }
        // The empty table — every knob at its compiled default — is armable for both names, so the
        // refusal can never be reached by simply naming one of them.
        for name in ["grid", "dca_accumulate"] {
            assert!(profile(name, "").is_ok(), "{name}'s own defaults must mount");
        }
    }

    /// The cross-crate link that keeps the route rule from being SKIPPED on a future name — the twin
    /// of [`every_not_enumerated_registry_row_is_refused_here`], one table over.
    ///
    /// [`vike_strategy::misrouted_params`] judges only [`vike_strategy::ParamRoutes::SingleLeg`]
    /// names: a
    /// `MultiLeg` row's keys name LEGS (so "must equal the mount" is false about them) and a
    /// `NotEnumerated` row's key set is unknown. Both abstentions are correct TODAY only because no
    /// such name is `Capability::Live` except the two maker aliases, whose params table this daemon
    /// refuses outright. Flip `funding_carry` or `pairs_zscore` to live without first giving the
    /// mount real legs and the route check would silently stop applying to it — a strategy naming a
    /// leg the mount cannot route, mounting clean. This fails first instead.
    #[test]
    fn every_live_name_this_daemon_mounts_is_route_checked_or_refused_outright() {
        let mut checked = 0;
        for name in vike_strategy::PORTABLE_STRATEGIES {
            if !matches!(vike_strategy::capability(name), vike_strategy::Capability::Live) {
                continue; // refused by NAME at `validate_strategy`'s first gate
            }
            match vike_strategy::param_routes(name) {
                Some(vike_strategy::ParamRoutes::SingleLeg(_)) => checked += 1,
                _ => assert!(
                    AS_MAKER_NAMES.contains(name),
                    "`{name}` is LIVE-mountable here and its PARAM_ROUTES row is not SingleLeg, so \
                     `misrouted_params` abstains on it — yet this daemon does not refuse its \
                     params table either. Give the mount real legs (`vike_run::MountSpec::legs` + \
                     `MultiPaperExecutionClient`) before flipping the LIVE_CAPABLE row"
                ),
            }
        }
        assert!(checked > 0, "no live name is route-checked — this gate is vacuous");
    }

    /// The `token_id` spelling of the mount symbol is the SAME field, so the route rule must read it
    /// through [`DaemonProfile::mount_symbol`] and not off `symbol` alone.
    ///
    /// Not a restatement: this daemon's shipped profiles all say `token_id`, so a rule that compared
    /// against `self.symbol` would be `None` there and — depending on which way it fell — either
    /// refuse every Polymarket profile or check nothing on the ones that actually run.
    #[test]
    fn the_route_rule_reads_the_mount_symbol_under_either_spelling() {
        let err = DaemonProfile::from_toml_str(
            "token_id = \"TOK\"\n[strategy]\nname = \"buy_hold\"\n\n[strategy.params]\n\
             symbol = \"OTHER\"\n",
        )
        .unwrap_err();
        assert!(err.contains("TOK") && err.contains("OTHER"), "names both: {err}");
        assert!(
            DaemonProfile::from_toml_str(
                "token_id = \"TOK\"\n[strategy]\nname = \"buy_hold\"\n\n[strategy.params]\n\
                 symbol = \"TOK\"\n",
            )
            .is_ok(),
            "the `token_id` spelling must satisfy the rule when it agrees"
        );
    }

    /// The knob that is refused is the knob that would have been WRONG — proven end to end rather
    /// than trusted: the same table with the value spelled right mounts the value the operator
    /// typed, and the refused one would have mounted the compiled default.
    #[test]
    fn the_refused_value_is_the_one_that_would_have_silently_defaulted() {
        let g = DaemonProfile::from_toml_str(
            "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"grid\"\n\n\
             [strategy.params]\nsize = 2.0\n",
        )
        .expect("the correctly-typed profile loads");
        assert!(g.effective_params(&g.to_mount_config()).contains("size=2"), "mounts what it says");
        // The mistyped twin resolves to the compiled default — which is why it is refused at load
        // rather than mounted and logged.
        let quoted: toml::Value = toml::from_str("size = \"2\"").unwrap();
        let resolved = vike_strategy::resolved_params("grid", &quoted).expect("grid enumerates");
        assert_eq!(
            resolved.iter().find(|(k, _)| *k == "size").map(|(_, v)| v.as_str()),
            Some("1"),
            "`size = \"2\"` really does read as the compiled default"
        );
    }

    /// The cross-crate link that keeps [`AS_MAKER_NAMES`] honest: a registry row that declines to
    /// enumerate its keys ([`ParamKeys::NotEnumerated`]) is one `unknown_params` cannot check, so
    /// this daemon owes it a stricter rule of its own — and the only such rule is the maker refusal.
    /// A future `NotEnumerated` row therefore fails HERE rather than mounting unchecked params.
    #[test]
    fn every_not_enumerated_registry_row_is_refused_here() {
        for (name, keys) in vike_strategy::PARAM_KEYS {
            if matches!(keys, ParamKeys::NotEnumerated(_)) {
                assert!(
                    AS_MAKER_NAMES.contains(name),
                    "`{name}` declines to enumerate its params keys, so `unknown_params` reports \
                     nothing about it — but this daemon has no rule of its own for it either, so \
                     any key would mount unchecked. Either enumerate the row, or give this daemon a \
                     rule (the maker names are refused a params table outright)."
                );
            }
        }
        // ...and the converse: every maker name really is a NotEnumerated row, so the refusal is
        // covering a real gap rather than being an unexplained special case.
        for name in AS_MAKER_NAMES {
            assert!(
                matches!(vike_strategy::param_keys(name), Some(ParamKeys::NotEnumerated(_))),
                "`{name}` is refused a params table here but the registry enumerates its keys — \
                 one of the two is now wrong"
            );
        }
    }

    /// The echo (blocker 2's second half): what actually mounted must be readable. Logging the NAME
    /// alone left an operator unable to tell which numbers were running.
    #[test]
    fn the_effective_params_line_reports_what_mounted() {
        // The maker reports the RESOLVED knobs, including the venue-selected domain the profile
        // never states — the one that decides whether it quotes at all.
        let hl = DaemonProfile::from_toml_str(
            "venue = \"hyperliquid\"\nsymbol = \"BTC\"\ntick_size = 1.0\nqty = 0.005",
        )
        .expect("parses");
        let line = hl.effective_params(&hl.to_mount_config());
        assert!(line.contains("qty=0.005"), "{line}");
        assert!(line.contains("price_domain=Unbounded"), "{line}");
        assert_eq!(hl.strategy_name(), "spread_maker", "the default mount names itself");

        // Every other strategy reports the RESOLVED knobs the same way — the whole set, not the
        // subset the profile happened to mention, because a knob nobody typed is still a knob the
        // strategy is running. Each row is `key=<what the reader landed on>` and nothing else: this
        // line answers "what did the mount resolve", not "what is in force" — see
        // `effective_params`' own doc.
        let g = DaemonProfile::from_toml_str(
            "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"grid\"\n\n\
             [strategy.params]\nstep = 0.5\n",
        )
        .expect("parses");
        assert_eq!(
            g.effective_params(&g.to_mount_config()),
            "anchor=first anchor_price=0 step=0.5 rungs=3 size=1 band=10 bounded01=false \
             tick=0.001 symbol=(from the feed)"
        );
        assert_eq!(g.strategy_name(), "grid");

        // An empty table is not "nothing configured" — it is every knob at its compiled default,
        // and the line now SAYS what those are.
        let b = DaemonProfile::from_toml_str(
            "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"buy_hold\"\n",
        )
        .expect("parses");
        assert_eq!(b.effective_params(&b.to_mount_config()), "size=1 symbol=(from the feed)");
    }

    /// The maker line must carry the knobs that DECIDE THE POSTED WIDTH — and must not carry the
    /// dead one. It formatted nine fields and none of these four, so an operator could read `gamma`
    /// off the startup line while the two numbers that actually bound `δ` were invisible; meanwhile
    /// it printed `half_spread`, which A-S never consumes on this daemon.
    #[test]
    fn the_maker_line_reports_the_knobs_that_set_the_posted_width() {
        let hl = DaemonProfile::from_toml_str(
            "venue = \"hyperliquid\"\nsymbol = \"BTC\"\ntick_size = 1.0\nqty = 0.005",
        )
        .expect("parses");
        let line = hl.effective_params(&hl.to_mount_config());
        for knob in [
            "min_half_spread_ticks=2",
            "max_half_spread_ticks=60",
            "kappa_default=50",
            "tau_hold_ms=3600000",
        ] {
            assert!(line.contains(knob), "the width knob `{knob}` is missing from: {line}");
        }
        assert!(
            !line.contains("half_spread="),
            "the DEAD fixed-spread seed must not be logged beside the live knobs: {line}"
        );
        // The break-even fee floor is reported as the `Option` it is: a `Some` on a venue whose fee
        // shape has a flat rate (hyperliquid, 1.5 bps maker ⇒ a 3 bps round trip)...
        assert!(line.contains("round_trip_fee_rate=Some(0.0003"), "{line}");
        // ...and a NONE that an operator can SEE on one that has not (polymarket's p(1−p) curve),
        // because "no bar is armed" and "the fee is zero" must never read the same.
        let pm = DaemonProfile::from_toml_str("venue = \"polymarket\"\nsymbol = \"TOK\"")
            .expect("parses");
        let pm_line = pm.effective_params(&pm.to_mount_config());
        assert!(pm_line.contains("round_trip_fee_rate=None"), "{pm_line}");
    }

    /// The defect the round-2 repair introduced, pinned so it cannot return: the echo reported the
    /// RAW table, so a value the reader coerced printed as what was TYPED. A diagnostic that
    /// affirmatively misstates the mount is worse than no diagnostic — this repo deleted
    /// `Policy::max_total_exposure` over the same principle.
    ///
    /// Every case below is type-CORRECT input, so [`vike_strategy::mistyped_params`] passes it and
    /// only the echo can tell the truth about it.
    #[test]
    fn the_echo_reports_the_resolution_and_not_the_input() {
        let line = |name: &str, params: &str| {
            let p = DaemonProfile::from_toml_str(&format!(
                "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nname = \"{name}\"\n\n\
                 [strategy.params]\n{params}\n"
            ))
            .unwrap_or_else(|e| panic!("`{params}` must LOAD (it is well-typed): {e}"));
            p.effective_params(&p.to_mount_config())
        };
        // ⚠ The CLAMP row (`rungs = -5`, which `read_rungs` floors to zero) is no longer HERE: a
        // grid that rests nothing is refused at load by `vike_strategy::unarmable_params`, so there
        // is no mount line to inspect. The clamp is still pinned, in the refusal MESSAGE, by
        // `a_ladder_that_can_never_rest_a_rung_is_refused` below — the resolution the operator
        // needs to see is carried either way, which is the property this test is really about.
        //
        // An unrecognised STRING silently falling back — the anchor price is then never used.
        let l = line("grid", "anchor = \"fixd\"\nanchor_price = 42.0");
        assert!(l.contains("anchor=first"), "{l}");
        // ...and the same shape on a direction knob, where the fallback is a SIDE.
        let l = line("dca_accumulate", "side = \"shrot\"");
        assert!(l.contains("side=long"), "{l}");
        // A default the profile never mentions at all: the controller harness's venue tag.
        let l = line("momentum", "qty = 2.0");
        assert!(
            l.contains("venue=sim"),
            "a live mount under the tag `sim`, and now it says so: {l}"
        );
        assert!(l.contains("tp=(unarmed)"), "an un-armed barrier leg says so: {l}");
    }

    /// ...and ABSENT `[strategy]` still means the A-S maker, which is the back-compat property.
    #[test]
    fn no_strategy_table_still_resolves_the_as_maker() {
        let p = DaemonProfile::from_toml_str("token_id = \"TOK\"").expect("parses");
        assert!(p.strategy.is_none());
        assert!(p.resolve_strategy(&p.to_mount_config()).is_ok());
    }

    /// The three rejection classes, each with its OWN message. This is the honest-gate test: a
    /// strategy that would mount and never trade must fail at profile LOAD, not at 3am.
    #[test]
    fn unmountable_strategies_are_rejected_at_load_with_their_reason() {
        // (a) a typo.
        let err = strategy_profile("grud").unwrap_err();
        assert!(err.contains("unknown strategy"), "typo message: {err}");
        assert!(err.contains("grid"), "names what it could have meant: {err}");

        // (b) simulator-only: it backtests, but this daemon does not link the simulator.
        let err = strategy_profile("rotation_top_k").unwrap_err();
        assert!(err.contains("simulator-only"), "sim-only message: {err}");

        // (c) resolves, but its input never arrives live — the SILENT NO-OP class.
        let err = strategy_profile("funding_capture").unwrap_err();
        assert!(err.contains("cannot trade"), "not-live message: {err}");
        assert!(err.contains("Bar::funding"), "names the missing input: {err}");

        let err = strategy_profile("pairs_zscore").unwrap_err();
        assert!(err.contains("TWO-LEG"), "names why a two-leg mount cannot route: {err}");
    }

    /// Every `NotLive` row in the shared table is refused here — table-driven, so a future row
    /// cannot be added to the registry and silently stay mountable by this daemon.
    #[test]
    fn every_not_live_registry_row_is_refused_by_the_profile() {
        for (name, why) in vike_strategy::LIVE_CAPABLE {
            if why.is_none() {
                continue;
            }
            assert!(
                strategy_profile(name).is_err(),
                "{name} is declared not-live-capable but the profile accepted it"
            );
        }
    }

    /// The mount SPEC a `[strategy]` profile lowers to is the SAME projection the A-S path uses —
    /// one derivation, so the two can never disagree about venue/symbol/interval/seed_cash or the
    /// paper fee model. And its `legs` stay EMPTY: a multi-leg paper rehearsal would book both legs
    /// under one symbol (`build_paper_strategy_core_with`'s tripwire), so no profile may declare one
    /// until `MultiPaperExecutionClient` is wired.
    #[test]
    fn the_mount_spec_matches_the_maker_lowering_and_declares_no_legs() {
        let p = strategy_profile("grid").expect("parses");
        let cfg = p.to_mount_config();
        let spec = p.to_mount_spec();
        assert_eq!(spec.venue, cfg.venue);
        assert_eq!(spec.symbol, cfg.token_id);
        assert_eq!(spec.interval, cfg.interval);
        assert_eq!(spec.interval_ms, cfg.interval_ms);
        assert_eq!(spec.seed_cash.to_bits(), cfg.seed_cash.to_bits());
        assert_eq!(spec.maker_fee.to_bits(), cfg.maker_fee.to_bits());
        assert_eq!(spec.taker_fee.to_bits(), cfg.taker_fee.to_bits());
        assert!(spec.legs.is_empty(), "no profile may declare a mount leg yet");
    }

    #[test]
    fn hyperliquid_lowers_to_the_crypto_dollar_scale_mount() {
        // A hyperliquid profile lowers into `MakerMountConfig::crypto` (the $-scale A-S domain) so the
        // maker can quote a $64k asset; a polymarket profile keeps the [0,1] domain. The crypto mount's
        // signature here is its min-half-spread FLOOR (>0) — absent (0.0) on the [0,1] default.
        let hl = DaemonProfile::from_toml_str(
            "venue = \"hyperliquid\"\ntoken_id = \"BTC\"\ntick_size = 1.0\nqty = 0.005",
        )
        .expect("parses");
        let cfg = hl.to_mount_config();
        assert_eq!(cfg.venue, "hyperliquid");
        assert!(
            cfg.as_params.min_half_spread_ticks > 0.0,
            "hyperliquid must lower to the crypto $-scale mount (min-half-spread floor set)"
        );

        // polymarket (the default venue) keeps the [0,1] domain — no crypto floor.
        let poly = DaemonProfile::from_toml_str("token_id = \"TOK\"").expect("parses");
        assert_eq!(
            poly.to_mount_config().as_params.min_half_spread_ticks,
            0.0,
            "polymarket keeps the [0,1] default (no crypto floor)"
        );
    }

    // ---------------------------------------------------------------------------------------------
    // The `rhai = "<path>"` strategy spelling (docs/decisions/0024-rhai-strategies-live.md).
    // ---------------------------------------------------------------------------------------------

    /// A profile-shaped rhai TOML over a hyperliquid mount — the same base `strategy_profile` uses.
    fn rhai_profile_toml(rhai_line: &str, params: &str) -> String {
        format!("venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\n{rhai_line}\n{params}")
    }

    /// A script file this test owns, under a pid-keyed temp dir (the `own_sentinel` idiom) — the
    /// unit tests here never touch a checkout path.
    /// ⚠ Returns the `TempDir` ALONGSIDE the path, and the caller must bind it — dropping it
    /// deletes the script the path points at.
    ///
    /// This used to be `temp_dir().join(format!("…-{}", process::id()))`, which satisfies
    /// `crates/vike-ops/tests/temp_path_gate.rs` (the name is not fixed) and was still wrong twice
    /// over. MEASURED on the CI box, 2026-08-25:
    ///
    /// * **1,725 of these directories were sitting in `/tmp`**, dating back to 2026-08-18 — 1,082
    ///   owned by `the CI user` and 643 by `the operator`. Nothing ever deleted one, so every CI run and
    ///   every lane run leaked a directory permanently.
    /// * **A PID is REUSED.** When one collides with a directory the OTHER user created, the
    ///   `create_dir_all` succeeds (it already exists) and the `fs::write` fails with
    ///   PermissionDenied. That is a live, intermittent CI flake, and it is exactly the failure
    ///   `temp_path_gate`'s own message describes — *"whichever creates that directory first owns
    ///   it and every later run under the other user fails"* — arriving through the very idiom
    ///   that gate suggests as the remedy. PID-uniquification prevents collision WITHIN a run; it
    ///   does not prevent collision ACROSS users over time, and it leaks either way.
    ///
    /// `tempfile::TempDir` fixes both halves at once: unique by construction, and self-deleting.
    fn own_script(name: &str, source: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("temp script dir");
        let path = dir.path().join(format!("{name}.rhai"));
        std::fs::write(&path, source).expect("write script");
        (dir, path)
    }

    /// TOML-safe spelling of a path (backslashes escaped — this suite runs on the Windows dev box
    /// as well as the Linux runners).
    fn toml_path(p: &std::path::Path) -> String {
        p.display().to_string().replace('\\', "\\\\")
    }

    #[test]
    fn a_rhai_profile_parses_and_validates() {
        let p = DaemonProfile::from_toml_str(&rhai_profile_toml(
            "rhai = \"strategies/thing.rhai\"",
            "[strategy.params]\nsize = 2.0\n",
        ))
        .expect("a rhai profile parses and validates without touching the filesystem");
        assert_eq!(p.strategy_name(), "rhai");
        // The maker path must NOT claim it: a script is not the A-S maker.
        assert!(p.mounted_maker(&p.to_mount_config()).is_none());
    }

    #[test]
    fn a_strategy_table_with_both_name_and_rhai_is_refused() {
        let err = DaemonProfile::from_toml_str(&rhai_profile_toml(
            "name = \"grid\"\nrhai = \"thing.rhai\"",
            "",
        ))
        .unwrap_err();
        assert!(err.contains("not both"), "names the conflict: {err}");
    }

    #[test]
    fn a_strategy_table_with_neither_name_nor_rhai_is_refused() {
        // With params AND without: a table that selects nothing must fail either way, with a
        // message naming both spellings.
        for params in ["", "[strategy.params]\nsize = 2.0\n"] {
            let err = DaemonProfile::from_toml_str(&rhai_profile_toml("", params)).unwrap_err();
            assert!(err.contains("`name"), "names the name spelling: {err}");
            assert!(err.contains("`rhai"), "names the script spelling: {err}");
        }
    }

    #[test]
    fn an_empty_rhai_path_is_refused() {
        let err = DaemonProfile::from_toml_str(&rhai_profile_toml("rhai = \"\"", "")).unwrap_err();
        assert!(err.contains("script file"), "names what is missing: {err}");
    }

    /// The script arm keeps the daemon's no-silently-ignored-key posture at the VALUE level: a
    /// non-numeric override can never apply (`param` takes an f64), so it is refused at LOAD.
    #[test]
    fn a_non_numeric_rhai_param_is_refused_at_load() {
        let err = DaemonProfile::from_toml_str(&rhai_profile_toml(
            "rhai = \"thing.rhai\"",
            "[strategy.params]\nsize = \"2\"\n",
        ))
        .unwrap_err();
        assert!(err.contains("size"), "names the offending key: {err}");
        assert!(err.contains("NUMBER"), "states the requirement: {err}");
    }

    /// `name = "rhai"` is redirected to the path spelling rather than refused as simulator-only —
    /// the message an operator acts on after the 0024 reversal.
    #[test]
    fn name_rhai_is_redirected_to_the_path_spelling() {
        let err = strategy_profile("rhai").unwrap_err();
        assert!(err.contains("rhai = "), "points at the path spelling: {err}");
        assert!(err.contains("0024"), "cites the decision record: {err}");
    }

    /// The resolve reads the file, and a missing one fails NAMING THE PATH — before any core
    /// spawns, same as every other resolve failure.
    #[test]
    fn a_missing_script_file_is_a_resolve_error_naming_the_path() {
        let p = DaemonProfile::from_toml_str(&rhai_profile_toml(
            "rhai = \"no-such-dir/no-such-script.rhai\"",
            "",
        ))
        .expect("validates — the file is read at resolve, not at load");
        let err = match p.resolve_mount(&p.to_mount_config()) {
            Err(e) => e,
            Ok(_) => panic!("a missing script file must fail the resolve"),
        };
        assert!(err.contains("no-such-script.rhai"), "names the path: {err}");
    }

    /// The resolve refuses an override the script's own top level never asks for — the script-arm
    /// twin of `an_unread_params_key_is_refused_with_the_readable_set`, keyed on
    /// `vike_script::discover_params`.
    #[test]
    fn a_rhai_override_the_script_never_asks_for_is_refused_at_resolve() {
        let (_tmp, path) = own_script(
            "declares-size",
            "const SIZE = param(\"size\", 1.0);\nfn on_bar() { if position() == 0.0 { \
             buy(SIZE); } }\n",
        );
        let p = DaemonProfile::from_toml_str(&rhai_profile_toml(
            &format!("rhai = \"{}\"", toml_path(&path)),
            "[strategy.params]\nsizee = 2.0\n",
        ))
        .expect("validates — key names are checked at resolve, against the script text");
        let err = match p.resolve_mount(&p.to_mount_config()) {
            Err(e) => e,
            Ok(_) => panic!("an unasked-for override must fail the resolve"),
        };
        assert!(err.contains("sizee"), "names the offender: {err}");
        assert!(err.contains("size (default 1)"), "names the script's own knobs: {err}");
    }

    /// The happy path: a script resolves into the SAME `MountedStrategy` seam a registry name
    /// does, and the `Script` variant carries the audit pair — the path as the profile spelled it
    /// and the sha256 of the source that was actually read — so the INFO audit line's claim is
    /// assertable without a log subscriber.
    #[test]
    fn a_rhai_profile_resolves_to_a_script_mount_carrying_the_audit_hash() {
        let source = "const SIZE = param(\"size\", 1.0);\nfn on_bar() { if position() == 0.0 { \
                      buy(SIZE); } }\n";
        let (_tmp, path) = own_script("resolves", source);
        let p = DaemonProfile::from_toml_str(&rhai_profile_toml(
            &format!("rhai = \"{}\"", toml_path(&path)),
            "[strategy.params]\nsize = 3.0\n",
        ))
        .expect("validates");
        match p.resolve_mount(&p.to_mount_config()).expect("the script compiles and mounts") {
            MountedStrategy::Script { path: got_path, sha256, .. } => {
                assert_eq!(got_path, path.display().to_string());
                assert_eq!(sha256, script_sha256(source), "the hash is of the source read");
            }
            MountedStrategy::AsMaker(_) | MountedStrategy::Registered(_) => {
                panic!("a rhai profile must resolve through the Script arm")
            }
        }
        // ...and the boxing wrapper `main` calls accepts it like any other strategy.
        assert!(p.resolve_strategy(&p.to_mount_config()).is_ok());
    }

    /// [`script_sha256`] against the NIST SHA-256 test vector for "abc" — the pure half of the
    /// audit line, pinned to a value computed outside this codebase.
    #[test]
    fn script_sha256_matches_the_known_vector() {
        assert_eq!(
            script_sha256("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    /// The effective-params line reports the script path plus the overrides that DID land (the
    /// resolve refuses any other kind), never the raw table.
    #[test]
    fn the_rhai_effective_params_line_reports_path_and_overrides() {
        let p = DaemonProfile::from_toml_str(&rhai_profile_toml(
            "rhai = \"thing.rhai\"",
            "[strategy.params]\nsize = 3.0\n",
        ))
        .expect("validates");
        let line = p.effective_params(&p.to_mount_config());
        assert!(line.contains("script=thing.rhai"), "names the script: {line}");
        assert!(line.contains("size=3"), "names the override: {line}");

        let bare = DaemonProfile::from_toml_str(&rhai_profile_toml("rhai = \"thing.rhai\"", ""))
            .expect("validates");
        let line = bare.effective_params(&bare.to_mount_config());
        assert!(
            line.contains("no overrides"),
            "an override-free mount says so rather than claiming knobs: {line}"
        );
    }

    /// `validate_for_live` is strategy-agnostic and a rhai profile rides it unchanged: the wired
    /// hyperliquid/BTC pair passes, a foreign symbol on the same venue is refused — the same
    /// verdicts a named-strategy profile gets.
    #[test]
    fn a_rhai_profile_gets_the_same_live_verdicts_as_a_named_one() {
        let ok = DaemonProfile::from_toml_str(&rhai_profile_toml("rhai = \"thing.rhai\"", ""))
            .expect("validates");
        assert!(ok.validate_for_live().is_ok(), "hyperliquid/BTC is a live-wired pair");

        let foreign = DaemonProfile::from_toml_str(
            "venue = \"hyperliquid\"\nsymbol = \"NOPE\"\n[strategy]\nrhai = \"thing.rhai\"\n",
        )
        .expect("validates");
        assert!(foreign.validate_for_live().is_err(), "a foreign symbol is still refused");
    }

    // ---------------------------------------------------------------------------------------------
    // resolve_paper_risk_limits — the RunProfile risk-budget resolver (Task 2).
    // ---------------------------------------------------------------------------------------------

    /// MERGE-SAFETY PROPERTY: absent a profile, the resolved limits must be byte-identical to
    /// `RiskLimits::new()` — the value the daemon has hardcoded since before this wiring existed.
    #[test]
    fn no_profile_is_byte_identical_to_the_pre_profile_default() {
        let got = resolve_paper_risk_limits(None).expect("no profile is never an error");
        assert_eq!(got, RiskLimits::new(), "no profile must not change mounted behavior at all");
    }

    #[test]
    fn profile_operator_budget_fields_are_armed() {
        let toml = r#"
mode = "paper"
[event_source]
kind = "live_venue"
venue = "polymarket"
symbol = "TOK"
[broker]
kind = "paper"
seed_cash = 1000.0
[risk]
max_notional_per_order      = 100.0
max_total_exposure          = 500.0
max_orders_per_window       = 3
window_ms                   = 2000
max_leverage                = 4.0
required_free_bp_pct        = 0.1
block_reduce_only_overshoot = true
"#;
        let profile = RunProfile::from_toml_str(toml).expect("profile parses and validates");
        let got = resolve_paper_risk_limits(Some(&profile)).expect("NoGridFetched never errors");
        assert_eq!(got.max_notional_per_order, Some(100.0));
        assert_eq!(got.max_total_exposure, Some(500.0));
        assert_eq!(got.max_orders_per_window, Some(3));
        assert_eq!(got.window_ms, 2000);
        assert_eq!(got.max_leverage, Some(4.0));
        // DERIVED from `max_leverage` (issue #822): the TOML has no `im_requirement` key.
        assert_eq!(got.im_requirement, Some(0.25));
        assert_eq!(got.required_free_bp_pct, 0.1);
        assert!(got.block_reduce_only_overshoot);
        // No instrument fields were set in the profile -> stay None (NoGridFetched takes the
        // profile's own instrument fields verbatim; an unset field is None, not inherited from
        // anywhere else — there is no "anywhere else" on a paper mount).
        assert_eq!(got.tick_size, None);
        assert_eq!(got.lot_size, None);
        assert_eq!(got.min_qty, None);
        assert_eq!(got.min_notional, None);
    }

    #[test]
    fn profile_may_also_supply_instrument_fields_under_no_grid_fetched() {
        // The PAPER mount's one legitimate use of `apply_to`'s NoGridFetched exception: a
        // `mode = "paper"` profile's own instrument-grid fields take effect with no opt-in of any
        // kind, since `RunProfile::grid_source` derives `NoGridFetched` from `mode = "paper"` and
        // nothing else ever fetches a grid for a paper mount.
        let toml = r#"
mode = "paper"
[event_source]
kind = "live_venue"
venue = "polymarket"
symbol = "TOK"
[broker]
kind = "paper"
seed_cash = 1000.0
[risk]
tick_size    = 0.01
lot_size     = 1.0
min_qty      = 1.0
min_notional = 1.0
"#;
        let profile = RunProfile::from_toml_str(toml).expect("profile parses and validates");
        let got = resolve_paper_risk_limits(Some(&profile)).expect("NoGridFetched never errors");
        assert_eq!(got.tick_size, Some(0.01));
        assert_eq!(got.lot_size, Some(1.0));
        assert_eq!(got.min_qty, Some(1.0));
        assert_eq!(got.min_notional, Some(1.0));
    }
}
