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
//! Avellaneda–Stoikov [`vike_run::SpreadMaker`] built from this profile's own maker fields
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
/// ⚠ `docs/ops/fxcm-forexconnect.md` used to describe the remaining wiring step as "a row in
/// `WIRED_MARKETS`, a `build_node` arm, and the matching entry in this allow-list". The first two
/// are right; the third is not, and following it would have demanded a feed arm that cannot exist.
/// The engine reached that way is reachable by ROUTING (a `vike_core::MountLeg::at` naming fxcm
/// from a mount on a venue that does have prices), not by being the profile's own venue.
pub const LIVE_WIRED_VENUES: &[&str] = &[
    // ⚠ CREDENTIALED MARKET DATA (split-plane I9): alpaca's feed is a real `DataClient` but it
    // AUTHENTICATES (the OAuth2 SANDBOX client-credentials trio) — there is no keyless price
    // stream to fall back to, so `venue_feed_plan`'s alpaca arm REFUSES the live mount outright
    // when the trio is absent rather than mounting a feed-less core that quotes into the void.
    // Exec stays SANDBOX-tier-pinned in `crates/bridges/alpaca/src/mount.rs`'s `AlpacaVenueMount`
    // (no `ALPACA_MAINNET` flag; a LIVE-tier flip is a deliberate code change, not a config flip).
    "alpaca",
    // ⚠ REAL MONEY IN PRACTICE: aster's exec tier is credential-resolved MAINNET-FIRST
    // (`AsterVenueMount` in `crates/bridges/aster/src/mount.rs` prefers `ASTER_LIVE_*`, and only
    // the LIVE tier is configured in practice — a testnet exists, the constraint is credentials;
    // root CLAUDE.md + `crates/bridges/aster/CLAUDE.md`). This row is CAPABILITY only: absent
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
    // `crates/bridges/ig/src/mount.rs`'s `IgVenueMount` (it loads `Environment::Demo` only;
    // `ig_rest_base` has a live-gateway arm with NO caller in the workspace, so `IG_LIVE_*` in the
    // store configures nothing and the venue silently stays paper — `crates/bridges/ig/CLAUDE.md`
    // records that trap) — and the feed
    // resolves that same tier, so this venue is demo exec over demo prices end to end.
    "ig",
    // ⚠ CREDENTIALED MARKET DATA on BOTH lanes, and NO market-data WS at all (split-plane I9):
    // oanda's feed is `market_feed::Feeds`, a `pump_spec` `OwnPump` — quotes ride a chunked-HTTP
    // `/pricing/stream` line stream and bars POLL the candles REST endpoint, both Bearer-authed,
    // so absent credentials refuse the live mount (the alpaca/ctrader argument). Exec arms only
    // the PRACTICE tier: `crates/bridges/oanda/src/mount.rs`'s `OandaVenueMount` goes through
    // `crates/bridges/oanda/src/config.rs`'s `mountable_tier_for_account`, which also REFUSES to
    // arm a store holding any live-named key (the fxTrade tier is implemented but has no caller,
    // so a live flip is a deliberate code change). The feed resolves that same practice tier
    // (`crates/vike-tradehub/src/venue_plan.rs`'s `oanda_plan`) but WITHOUT that refusal, so on a
    // store holding a live-named key the feed still plans while the mount stays paper. Otherwise
    // this venue is practice exec over practice prices end to end.
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
    /// **WHAT PRODUCT this mount trades** — see [`MountCfg::asset_class`], which this is the
    /// single-mount spelling of. `Option` here, `NOT NULL` in the row; the asymmetry is the
    /// migration, and it is argued at that field.
    #[serde(default)]
    pub asset_class: Option<String>,
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
    /// **WHAT PRODUCT this mount trades** — the stored word of a `vike_model::AssetClass`
    /// (`"CryptoSpot"`, `"CryptoPerp"`, `"Option"`, …). Phase 5 of
    /// `docs/decisions/0061-an-instrument-names-its-kind.md`, ORDERED by the owner: *"we need to add
    /// type if it is spot or perp or option or anything else bcz it will help us in future"*.
    ///
    /// ⚠ **`Option` HERE and NOT NULL in the ROW, and the asymmetry is the migration.** A
    /// `tradehub.toml` written before this key existed must keep parsing — the live daemon's own
    /// profile is one, and making it a required TOML key would stop that box starting. But
    /// `vike_secrets::profile_store::MountRow` requires the class, so
    /// [`crate::profile_rows::daemon_profile_to_rows`] REFUSES to lower a mount that does not name
    /// one. The cost of making it mandatory is therefore exactly what 0061 priced: **one value in
    /// one file**, paid by whoever migrates that profile into rows, and nothing before then.
    ///
    /// A `String` rather than a typed enum so the TOML spelling and the COLUMN carry one word;
    /// [`crate::profile_rows::parse_mount_asset_class`] is where it becomes an `AssetClass`, and an
    /// unknown word is refused there by name.
    #[serde(default)]
    pub asset_class: Option<String>,
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
    /// **THE EXPLICIT PRIMARY** — `primary = true` on at most ONE row.
    ///
    /// This is the field 0057's *tradehub.toml* verdict says must exist before mounts become rows:
    /// *"the first mount row silently means three different things and means a fourth thing not at
    /// all"*, and **SQL rows have no inherent order**, so a table cannot reproduce "the first one"
    /// without either an ordinal or a declaration.
    ///
    /// It is deliberately NOT lowered onto [`DaemonProfile`] by [`DaemonProfile::mount_rows`].
    /// Primacy is a fact about a row's place in the SET, and a lowered row is a single-mount profile
    /// that no longer has a set to be first in; carrying it down would have made
    /// `DaemonProfile::validate` answer a question that only `validate_multi` can.
    /// [`DaemonProfile::primary_mount`] reads it off `mounts` directly, which is the one resolution.
    ///
    /// ⚠ **Absent on every row is the historical answer and stays byte-identical**: the FIRST row
    /// is the primary, exactly as `resolved[0]` made it before this key existed
    /// ([`PrimaryMount::ImplicitFirst`]). Nothing that has ever shipped sets this, so nothing moves.
    #[serde(default)]
    pub primary: Option<bool>,
}

/// **WHICH mount is the daemon's singular identity, and how that was decided.**
///
/// # What "primary" actually is today, measured rather than assumed
///
/// `crates/vike-tradehub/src/tradehub_cli.rs` took `resolved[0]` and its own comment named the three
/// jobs that index does: *"the daemon's historical singular identity (summary token, mode line, seed
/// policy)"*. 0057 adds the half that comment does not, and it is the reason this type exists:
///
/// * on a PAPER core `crates/vike-run/src/lib.rs` derives ENGINE ORDER from first-appearance venue
///   order, and the first mount naming a symbol supplies that book's fee and slippage scalars;
/// * on a LIVE core row order decides **nothing at all** — `crates/vike-run/src/node.rs`'s
///   `build_node` builds its engine list straight-line from `WIRED_MARKETS`;
/// * so the same index means three things in one binary and nothing in the other, and **no document
///   declared any of them**. That is what made PR #1866's defect reachable from the other side: the
///   order path's silent fall-through to engine zero was a fall-through to THIS, undeclared.
///
/// # Why an enum and not a `usize`
///
/// The migration to rows must not change the answer. [`Self::ImplicitFirst`] is today's rule kept
/// verbatim for every profile that declares nothing — which is every profile that has ever shipped —
/// and [`Self::Declared`] is the new capability. A bare index could not tell a caller which of the
/// two it was holding, and a startup line that cannot say *implicit* is the positive confirmation of
/// something false this workspace deletes settings keys over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimaryMount {
    /// A `[[mounts]]` row set `primary = true`. The index is that row's, which need NOT be `0` —
    /// that is the entire point of the key.
    Declared(usize),
    /// No row declared one, so index `0` is the primary: the first `[[mounts]]` row, or — for the
    /// historical single-mount spelling — the profile itself. **Byte-identical to `resolved[0]`.**
    ImplicitFirst(usize),
}

impl PrimaryMount {
    /// The index into [`DaemonProfile::mount_rows`]'s output.
    #[must_use]
    pub fn index(self) -> usize {
        match self {
            PrimaryMount::Declared(i) | PrimaryMount::ImplicitFirst(i) => i,
        }
    }

    /// The word a startup line prints, so an operator can tell a declaration from an accident.
    #[must_use]
    pub fn word(self) -> &'static str {
        match self {
            PrimaryMount::Declared(_) => "declared",
            PrimaryMount::ImplicitFirst(_) => "implicit (the first row; no `primary = true` set)",
        }
    }
}

/// **A node that cannot honour a name must not choose one** — PR #1866's refusal sentence, stated
/// ONCE and shared by the order path and the mount path.
///
/// #1866 fixed exactly this defect on the ORDER path: `CoreThread::route_of` resolved an unknown
/// venue to `.unwrap_or(0)`, so a command naming a venue the process runs no engine for was signed
/// and sent by the PRIMARY venue's execution client. `crate::server::venue_refusal` calls this, and
/// so does the profile path, because a mount naming an engine-less venue is the same mistake one
/// layer earlier — and 0057's Phase 0 names it as such: *"This is the mount-side twin of the defect
/// the order path had fixed."*
///
/// `engines` EMPTY means the roster is not known here and refuses nothing — #1866's own reading (an
/// empty engine roster means the core has not published yet, not that it runs none).
#[must_use]
pub fn no_engine_refusal(venue: &str, engines: &[String], subject: &str) -> Option<String> {
    if engines.is_empty() || engines.iter().any(|e| e == venue) {
        return None;
    }
    let mut known: Vec<&str> = engines.iter().map(String::as_str).collect();
    known.sort_unstable();
    known.dedup();
    Some(format!(
        "this node runs no engine for venue `{venue}` — it runs: {}. {subject} was REFUSED rather \
         than applied to this node's primary engine: an order names the book it is for, and a node \
         that cannot honour the name must not choose one",
        known.join(", ")
    ))
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
                asset_class: m.asset_class.clone(),
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

    /// **THE ONE PRIMARY RESOLUTION.** Every consumer asks this instead of indexing `[0]`, so the
    /// daemon's singular identity is answered in one place and can be REPORTED rather than assumed.
    ///
    /// * a `[[mounts]]` row with `primary = true` ⇒ [`PrimaryMount::Declared`] at that row's index;
    /// * otherwise ⇒ [`PrimaryMount::ImplicitFirst(0)`](PrimaryMount::ImplicitFirst), which is
    ///   `resolved[0]` — the historical answer, byte for byte, for the single-mount spelling and for
    ///   every `[[mounts]]` profile that declares nothing.
    ///
    /// Two declared primaries are impossible here: [`Self::validate_multi`] refuses the profile at
    /// LOAD, naming both rows, so this function has no tie to break and needs no rule for one.
    #[must_use]
    pub fn primary_mount(&self) -> PrimaryMount {
        match self.mounts.iter().position(|m| m.primary == Some(true)) {
            Some(i) => PrimaryMount::Declared(i),
            None => PrimaryMount::ImplicitFirst(0),
        }
    }

    /// Refuse this profile's PRIMARY mount when the node runs no engine for its venue, **in the
    /// vocabulary PR #1866 established** ([`no_engine_refusal`]).
    ///
    /// The primary is the mount whose venue becomes the daemon's identity and — before #1866 — the
    /// engine an unroutable order silently reached. A profile that declares a primary on a venue
    /// this node cannot run is therefore the same fault the order path now refuses, arriving one
    /// layer earlier, and it is refused with the same words rather than a second vocabulary.
    ///
    /// `engines` empty ⇒ `Ok(())`. See [`no_engine_refusal`] for why.
    ///
    /// # Errors
    ///
    /// The refusal sentence, prefixed with which mount it is about.
    pub fn refuse_unrunnable_primary(&self, engines: &[String]) -> Result<(), String> {
        let primary = self.primary_mount();
        let rows = self.mount_rows();
        let Some(row) = rows.get(primary.index()) else { return Ok(()) };
        match no_engine_refusal(
            row.venue(),
            engines,
            &format!("The {} primary mount (mounts[{}])", primary.word(), primary.index()),
        ) {
            Some(msg) => Err(msg),
            None => Ok(()),
        }
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
                );
            }
            (None, None) => {
                return Err("a mount symbol is required: set `symbol` (or, on polymarket, \
                            `token_id`)"
                    .to_string());
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
                        .to_string());
                }
                (None, None) => {
                    return Err("a `[strategy]` table must say WHAT to mount: set \
                                `name = \"<registry strategy>\"` or `rhai = \"<path to .rhai \
                                script>\"` (or drop the table for the default A-S maker) — a \
                                table with neither selects nothing for its `[strategy.params]` to \
                                configure"
                        .to_string());
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
            ("asset_class", self.asset_class.is_some()),
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
        // ⚠ TWO DECLARED PRIMARIES IS A LOAD REFUSAL, naming both rows — the file-side twin of the
        // store's `mount_one_primary_per_profile` partial unique index, so the two spellings cannot
        // disagree about how many primaries a profile may have. There is no defensible tie-break: a
        // primary is the daemon's singular identity (summary token, mode line, seed policy), and
        // picking the earlier of two declarations would silently reinstate exactly the
        // position-decides-it accident `primary` exists to remove.
        let declared: Vec<usize> = self
            .mounts
            .iter()
            .enumerate()
            .filter(|(_, m)| m.primary == Some(true))
            .map(|(i, _)| i)
            .collect();
        if declared.len() > 1 {
            return Err(format!(
                "mounts[{}] and mounts[{}] both set `primary = true` — at most one mount may be \
                 the primary, because the primary IS the daemon's singular identity (its summary \
                 token, its mode line and its seed policy all read that one row). Drop the key \
                 from every row but one; dropping it from all of them keeps the historical answer, \
                 which is the FIRST row",
                declared[0], declared[1]
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
                ));
            }
            Capability::SimulatorOnly(why) => {
                return Err(format!(
                    "strategy {name:?} is simulator-only ({why}) — it backtests, but this daemon \
                         does not link the simulator and could not mount it"
                ));
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
                        .filter(|(_, v)| v.is_live())
                        .map(|(n, _)| *n)
                        .collect::<Vec<_>>()
                        .join(", "),
                    if vike_user_strategies::USER_STRATEGIES.is_empty() {
                        String::new()
                    } else {
                        format!("; user: {}", vike_user_strategies::USER_STRATEGIES.join(", "))
                    },
                ));
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
/// mount shape; a `RunProfile` has no venue/execution-target sections any more — `[event_source]`
/// and `[broker]` were DELETED for being read by nothing — so ONLY
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

#[path = "config_tests.rs"]
#[cfg(test)]
mod config_tests;
