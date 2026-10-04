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
//! Avellaneda–Stoikov [`vike_mount::SpreadMaker`] built from this profile's own maker fields
//! (`qty`/`half_spread`/`tick_size`/`resolution_ts_ms`) via [`vike_mount::build_maker`] — the same
//! function [`vike_mount::build_paper_maker_core`] has always called. The A-S maker is now ONE
//! mountable strategy rather than the hardcoded one; it is still the DEFAULT one.
//!
//! ## Scope — the mount config, PAPER or LIVE
//! This profile lowers into the [`vike_mount::MakerMountConfig`] that BOTH the paper mount
//! ([`vike_mount::build_paper_maker_core`], the default) and the opt-in LIVE `build_node` mount
//! ([`vike_mount::build_live_maker_core`], under `VIKE_TRADEHUB_LIVE=1`) build from, and — for a
//! `[strategy]` profile — into that config's strategy-free [`vike_mount::MountSpec`] projection. The
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
use vike_mount::{MakerMountConfig, MountSpec, SpreadMaker};
use vike_strategy::{Capability, ParamKeys};

/// The registry names that mean "the Avellaneda–Stoikov maker this profile already describes".
///
/// ⚠ These two do NOT resolve through `vike_strategy::strategy_by_name` on this daemon, and that is
/// the whole point. The registry arm is `SpreadMaker::from_params(params)` — a reader over
/// `[strategy.params]` and NOTHING else — so naming the maker would have mounted a MATERIALLY
/// DIFFERENT maker from the one the same profile mounts with no `[strategy]` table: `qty = 1`,
/// `tick_size = 0`, and the `[0,1]` Bernoulli wall clamp instead of the venue-selected `AsParams`
/// that [`DaemonProfile::to_mount_config`] picks. On a hyperliquid mount that is the exact
/// configuration `crates/vike-tradehub/src/config/mount.rs`'s `to_mount_config` comment records as
/// posting ZERO orders before `vike_mount::MakerMountConfig::crypto` existed — a daemon that starts, logs
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
    /// `vike_mount::build_maker` — the same call the absent-`[strategy]` default path makes.
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

/// The venues this daemon can mount LIVE: the ones `crates/vike-tradehub/src/tradehub_cli.rs`'s
/// `live_mount` has a market-feed arm for — the dispatch lives in
/// `crates/vike-tradehub/src/feeds.rs`'s `venue_feed_plan`, which `live_mount` runs once per
/// `[[mounts]]` row.
/// It is a VENUE list, not a `(venue, symbol)` allow-list — the symbol question is a separate,
/// DERIVED check (see [`DaemonProfile::validate_for_live`]), because "which venues did somebody wire
/// a feed for" and "which symbol does that venue's engine accept" are different facts with different
/// authorities, and conflating them is what made this gate a hand-copied pair table.
///
/// Extending it is a deliberate two-place edit: a row HERE and the matching `venue_feed_plan` arm.
/// `crates/vike-tradehub/tests/daemon/live_wired_venues_pin.rs` is what makes that true — it scans
/// `crates/vike-tradehub/src/feeds.rs`'s `venue_feed_plan` `match` for its actual venue arms and
/// fails on ANY difference, in either direction, under this build's features.
///
/// ⚠ It exists because this doc used to cite `live_wired_venues_are_all_mounted_by_build_node` for
/// that claim, and that test checks something else entirely: `crate::wired_markets::WIRED_MARKETS`, which
/// CONTAINS binance, bybit, okx and seven more venues `venue_feed_plan` has no arm for. Adding `"binance"`
/// to this list was MEASURED green across the whole vike-tradehub suite. The previous hardcoded
/// `(venue, symbol)` pair form forced a match-arm edit that the completeness test caught; a one-line
/// const does not, which is the "declaration-pinning tests don't gate" failure mode this repo has
/// already been bitten by three times. `venue_feed_plan`'s own `v => return Err(…)` catch-all still makes
/// a widened row a loud startup failure rather than a silent live mount — but "loud at 3am" is not
/// the gate this comment promised.
///
/// ## ⚠ fxcm is DELIBERATELY absent, and it is the one venue that can never join
///
/// `crate::wired_markets::WIRED_MARKETS` grew an fxcm row and `build_node` an fxcm `make_engine` arm (both
/// behind the `fxcm` feature), so an fxcm-linked daemon now mounts a real FXCM execution engine and
/// reconcile client. That does NOT make it mountable as a PROFILE venue, because this list is about
/// the other half: which venues `venue_feed_plan` can build a market FEED for. `vike-fxcm` has no
/// market-data seam of any kind — `LiveDataCaps::NONE` in its caps row, `NoPump` in
/// `vike_bridge_core::pump_spec` — so there is no arm to write, and
/// `crates/vike-tradehub/tests/daemon/live_wired_venues_pin.rs` (this list == `venue_feed_plan`'s
/// actual feed arms) would go red on a row added here alone. A profile naming `venue = "fxcm"` is
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
/// when omitted, inherits the [`MakerMountConfig::outcome_token`] recommended default — so a minimal
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
    /// `tau_hold` (see [`MakerMountConfig::outcome_token`]).
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
    /// Refused when the live gate is OFF (`tradehub_cli.rs`'s paper arm) — the paper daemon mounts no
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
    /// (`vike_mount::account_symbols_for`) — which is how a second account reaches a second
    /// instrument at all, since `crate::wired_markets::WIRED_MARKETS` pins one symbol per venue for the DEFAULT
    /// account.
    ///
    /// ⚠ **An account this box will not ARM is a hard startup failure naming venue, account and
    /// mount** (`crates/vike-mount/src/node.rs`'s `refuse_unarmed_mount_accounts`), not a fallback to the default
    /// account. It is the one refusal in this daemon that does not degrade, because the degraded
    /// state is "trading, on a book the author did not choose".
    #[serde(default)]
    pub account: Option<vike_model::accounts::account_keys::AccountLabel>,
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
    /// `tradehub.toml` written before this key existed must keep parsing (it is still what
    /// `vike-cli config mirror --daemon` imports) — the live daemon's own profile was one until
    /// decision 0086 moved it into rows, and making it a required TOML key would have stopped that
    /// box starting. But
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
    pub account: Option<vike_model::accounts::account_keys::AccountLabel>,
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
/// * on a PAPER core `crates/vike-mount/src/run.rs` derives ENGINE ORDER from first-appearance venue
///   order, and the first mount naming a symbol supplies that book's fee and slippage scalars;
/// * on a LIVE core row order decides **nothing at all** — `crates/vike-mount/src/node.rs`'s
///   `build_node` builds its engine list from the wired markets
///   (`crate::wired_markets::WIRED_MARKETS`, held in their `engine_rank` order);
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
/// and sent by the PRIMARY venue's execution client. `crate::server::refusal::venue_refusal` calls this, and
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
    /// ([`vike_model::accounts::account_keys::AccountLabel::text`] is
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
        match spec.account.as_ref().and_then(vike_model::accounts::account_keys::AccountLabel::text)
        {
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

    /// Snapshot-summary cadence as a [`Duration`].
    pub fn summary_interval(&self) -> Duration {
        Duration::from_millis(self.daemon.summary_ms)
    }

    /// Bounded-shutdown deadline as a [`Duration`].
    pub fn shutdown_deadline(&self) -> Duration {
        Duration::from_millis(self.daemon.shutdown_deadline_ms)
    }
}

// `DaemonProfile`'s one `impl` is split by concern into the child modules below; this file keeps
// the types, the constants, the load/accessor/identity methods and the free helpers they share.
mod mount;
mod validate;

/// Resolve the OPERATOR risk budget to arm on the PAPER mount's `RiskGate` from an optional loaded
/// [`vike_core::RunProfile`] (RunProfile wiring, Settings STEP 2 PR 1, Task 2). This is the ONE
/// function `tradehub_cli.rs` calls before mounting — see its doc for why only `[risk]` is consumed from
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
