//! `docs-data` — the machine-readable export of the CI-gated per-venue capability tables.
//!
//! The website/docs consume the JSON files attached to every GitHub Release as `docs-data` assets
//! (`.github/workflows/release.yml`'s "Render the docs-data assets" step, and the
//! `src/bin/docs_data.rs` bin that writes them). This module renders FIVE of them: `venues.json` —
//! one record per [`vike_model::VENUES`] entry, in roster order, rendered from the same registries
//! the runtime consults; `stats.json` (roster sizes, the latency-gate budget, generator identity);
//! `indicators.json` — every built-in indicator and pair indicator, by [`vike_indicators::Category`];
//! `templates.json` — the strategy registry's rosters with each portable strategy's parameter
//! surface and live-mount verdict; and `rosters.json` — the five remaining CI-gated rosters
//! ([`EVENTS`], the order kinds, the asset classes, the hist store's `kind=` layouts, and the
//! reconcile divergence kinds with each policy's COMPUTED verdict). The sixth asset, `bins.json`,
//! is NOT rendered here: it is derived from `cargo metadata`, so it belongs to `xtask`
//! (`xtask/src/docs_bins.rs`), the one crate that already reads that document — a renderer over
//! compile-time tables cannot see a manifest.
//!
//! Everything here is a RENDERER over tables that already exist; this module declares no capability
//! fact of its own, with TWO exceptions, each pinned to an authority it cannot reach at run time:
//! [`WIRING`], the recon/exec wiring class, which is table-encoded nowhere else, and [`EVENTS`],
//! whose wire tags are serde attributes. `WIRING` follows the playbook rules for a per-venue table (root
//! `CLAUDE.md`, "Per-venue capability maps"): a NAMED row per roster venue, a roster-completeness
//! test (`crates/vike-ops/tests/docs_data_gate.rs`'s `wiring_map_is_roster_complete`), and a
//! `just new-venue` marker so a scaffolded venue lands with a conservative placeholder row rather
//! than silently missing.
//!
//! What each output field is rendered FROM — the authorities, by symbol:
//! - `caps` — `vike_model::caps_for` (`crates/vike-model/src/venue_caps.rs`): what the ADAPTER
//!   wires today.
//! - `margin` — `vike_model::venue_margin_support`: what the EXCHANGE offers (vendor-doc
//!   transcription — that module's evidence-class note travels with the data, not with this
//!   renderer).
//! - `fees` — `vike_model::fee_schedule_for`, the DEFAULT registry: polymarket renders `free`; the
//!   V2 probability curve is the `fee_schedule_for_with_pm_curve` OPT-IN and is deliberately not
//!   exported, for the same default-compatibility reason that registry keeps `Free`.
//! - `amend_semantics` — `vike_model::amend_semantics`.
//! - `attribution` — `vike_model::attribution::attribution_for`.
//! - `tif` — `vike_bridge_core::tif::venue_tif`, one entry per [`vike_model::TimeInForce`].
//!   ⚠ Keyed by ROSTER id only: binance's entry is its SPOT lane, and the `"binance-perp"` LANE
//!   sub-key (see `venue_tif`'s own doc) is deliberately not exported — a lane sub-key is not a
//!   roster venue. The `"binance-perp"` fee lane is skipped on the same grounds.
//! - `indicators.json` — [`vike_indicators::registry`] and [`vike_indicators::pair_registry`], the
//!   built-in rosters (`IndicatorMeta::user` rows never sit in `registry()`, so nothing user-loaded
//!   is exported). Every [`vike_indicators::Category`] variant is rendered, EMPTY ones included —
//!   `User` is the class user-loaded indicators land in and no built-in row occupies it — because
//!   a category that vanished for having no rows is the silent omission the gate's category-set
//!   assertion exists to refuse.
//! - `templates.json` — `vike_strategy::registry`'s `PORTABLE_STRATEGIES` / `SIMULATOR_ONLY` /
//!   `SCRIPT_ONLY` rosters, each portable name's `PARAM_KEYS` row (rendered as a tagged object,
//!   because `ParamKeys::NotEnumerated` carries a REASON a plain key list could not) and its
//!   `LIVE_CAPABLE` verdict (`None` = mountable on the live core today; `Some(reason)` = resolves
//!   but would not trade). That registry's own tests hold the three portable tables exhaustive
//!   against each other; the renderer inherits that and PANICS on a missing row rather than
//!   guessing, the same rule as [`venue_record`]'s wiring lookup.
//! - `rosters.json` — five rosters, four of them read straight off their authority:
//!   `vike_model::venue_caps`'s `ORDER_KINDS`/`TRIGGER_KINDS`, `vike_catalog::AssetClass` (with
//!   each class's Symbol-picker tab, through `AssetClass::tab`), `vike_data::store_kind::
//!   STORE_KINDS` verbatim, and `vike_exec::recon`'s `DivergenceKind`. The divergence roster's
//!   per-policy verdicts are **COMPUTED** through `vike_exec::recon::mode_applies` rather than
//!   restated — see [`divergence_record`], which carries why. The fifth, [`EVENTS`], is the one
//!   DECLARED table here besides [`WIRING`]: a wire tag is a serde ATTRIBUTE no runtime renderer
//!   can read, so it is declared and pinned against the enum's source by the gate.
//!
//! [`P99_BUDGET_NS`] duplicates the constant of the same name in
//! `crates/vike-core/tests/runtime_latency.rs` — a TEST-file const this module cannot import. The
//! duplication is pinned, not trusted: `docs_data_gate.rs`'s
//! `latency_budget_matches_the_runtime_latency_gate` parses that source file and fails when the
//! two drift. What is exported is the BUDGET (the gate's ceiling), never a measurement.
//!
//! # Output contract
//!
//! Enum-ish values render kebab-case; canonical order-kind strings (`"stop_limit"`) stay verbatim
//! — they are domain vocabulary ([`vike_model::venue_caps::ORDER_KINDS`]), not renderer inventions.
//! A `max_batch` of `usize::MAX` ("the adapter imposes no cap of its own") renders `null`. The
//! venues ARRAY is in roster order and one build's render is fully deterministic (pinned by the
//! gate's `rendered_files_are_the_five_assets_and_deterministic`). ⚠ JSON object KEY order is NOT
//! part of the contract: it is an artifact of serde_json's map flavor, and the workspace's
//! DataFusion consumers (arrow-json / datafusion-physical-plan) flip `preserve_order` ON through
//! resolver-2 feature unification whenever they share a build with this crate — so a roster-lane
//! build renders insertion-order keys while a standalone `cargo run -p vike-ops` (the release
//! step's shape) renders alphabetical. Consumers read objects as unordered maps, and the gate
//! asserts key SETS, never order. An unclassified roster venue PANICS
//! the renderer rather than exporting a guess — deny loudly, never silently substitute — and the
//! completeness test makes that panic unreachable from a green tree.

use serde_json::{Value, json};
use vike_bridge_core::tif::{TifOutcome, venue_tif};
use vike_catalog::AssetClass;
use vike_data::store_kind::{Partition, STORE_KINDS, StoreKind};
use vike_exec::recon::{DivergenceKind, DivergenceOrigin, POLICY_NAMES, ReconPolicy, mode_applies};
use vike_indicators::{
    Category, IndicatorMeta, PairMeta, ParamSpec, pair_registry, registry as indicator_registry,
};
use vike_model::attribution::{AttributionMechanic, attribution_for};
use vike_model::venue_caps::{LiveDataCaps, ORDER_KINDS, TRIGGER_KINDS, TriggerType, VenueCaps};
use vike_model::{
    AmendSemantics, FeeSchedule, MarginMode, SwitchMechanism, TimeInForce, VENUES,
    VenueMarginSupport, amend_semantics, caps_for, fee_schedule_for, venue_margin_support,
};
use vike_strategy::{
    LIVE_CAPABLE, PARAM_KEYS, PORTABLE_STRATEGIES, ParamKeys, ParamType, SCRIPT_ONLY,
    SIMULATOR_ONLY,
};

/// Version stamp for the OUTPUT SHAPE of every file this module renders, carried in `stats.json`.
/// Bump it when a field is renamed/removed or its meaning changes; adding a field — or a whole
/// file, as `indicators.json` and `templates.json` were — is compatible and does not bump it.
pub const SCHEMA_VERSION: u32 = 1;

/// The core-hop tail budget the latency gate enforces, in nanoseconds — the CEILING, not a
/// measurement. Duplicated from `crates/vike-core/tests/runtime_latency.rs`'s `P99_BUDGET_NS`
/// (a `#[cfg(test)]`-world const this crate cannot import); `docs_data_gate.rs`'s
/// `latency_budget_matches_the_runtime_latency_gate` pins the two equal.
pub const P99_BUDGET_NS: u64 = 10_000;

/// What `stats.json` reports as `generated_from` when the caller names no commit: a build that
/// is not a release render.
///
/// The stamp itself is a RUNTIME ARGUMENT ([`stats_value`]'s parameter, fed by the `docs_data`
/// bin's optional second argv slot through [`generated_from`]) and NOT a compile-time
/// `option_env!("GITHUB_SHA")` bake, because that bake published a SHA it could not vouch for on
/// either of the release workflow's two triggers:
///
/// * Under `workflow_dispatch` — `.github/workflows/release.yml`'s re-release hatch, and the ONLY
///   way to re-run a failed tag run, since a tag-push run executes the workflow file at the tag —
///   `GITHUB_SHA` is the SHA of the ref the run was dispatched ON (main's head), while the checkout
///   step puts the TAG on disk (`ref: ${{ inputs.tag || github.ref }}`; the workflow's separate
///   tag-resolution step exists precisely because the two disagree). The render would describe the
///   tag while the stamp named main.
/// * On the tag-push path the bake is not even reliably that run's own value: the shared CI setup
///   action installs sccache as `RUSTC_WRAPPER`, and sccache's cache key does not hash
///   `GITHUB_SHA`, so a cached rustc result carries whichever run first compiled this constant.
///
/// The workflow passes `git rev-parse HEAD` instead — the commit actually checked out, identical
/// under both triggers, and immune to the cache hole because nothing about it is compiled in.
///
/// ⚠ The value arrives as a PARAMETER rather than as an environment read inside this library, and
/// that is the workspace rule rather than a preference — libraries take configuration as
/// parameters and only binaries read the process environment
/// (root `CLAUDE.md`, "Settings & configuration").
/// A library-layer `env::var` here would need a `LIBRARY_PIN` row in
/// `crates/vike-ops/tests/settings_registry.rs` — the ratchet the `option_env!` spelling was
/// dodging in the first place — and argv reaches the same place with no global state at all.
pub const DEFAULT_GENERATED_FROM: &str = "dev";

/// Resolve the `generated_from` stamp from the `docs_data` bin's optional `GENERATED_FROM`
/// argument: absent means [`DEFAULT_GENERATED_FROM`], present means exactly what was passed.
///
/// A present-but-BLANK argument is an `Err` rather than a silent fall back to the default, and the
/// distinction is the whole reason this is a function instead of an `unwrap_or`. The release
/// workflow spells the argument as a command substitution over `git rev-parse HEAD`; a
/// substitution that fails yields the EMPTY STRING and the surrounding command still runs, so a
/// silent default would publish a release-rendered `stats.json` claiming a dev build — the same
/// class of confident-wrong stamp this argument replaced. Blank is a usage error the bin reports
/// with a non-zero exit; the workflow step then fails loudly instead of shipping a lie.
///
/// # Errors
/// The argument is present and contains only whitespace.
pub fn generated_from(arg: Option<&str>) -> Result<&str, &'static str> {
    match arg {
        None => Ok(DEFAULT_GENERATED_FROM),
        Some(v) if v.trim().is_empty() => {
            Err("GENERATED_FROM is blank — pass the rendering commit (the release workflow passes \
                 `git rev-parse HEAD`) or omit the argument entirely")
        }
        Some(v) => Ok(v),
    }
}

/// The `venues.json` asset name — one place, shared by the bin and the gate test.
pub const VENUES_JSON: &str = "venues.json";
/// The `stats.json` asset name.
pub const STATS_JSON: &str = "stats.json";
/// The `indicators.json` asset name.
pub const INDICATORS_JSON: &str = "indicators.json";
/// The `templates.json` asset name.
pub const TEMPLATES_JSON: &str = "templates.json";

/// How a venue's EXEC and RECONCILE sides are wired into a default (feature-complete) live mount —
/// the one docs-facing axis no capability table encodes, because it is a property of
/// `crates/vike-mount/src/lib.rs`'s `make_engine` arms rather than of any adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wiring {
    /// An ungated `make_engine` arm mounts a real `ExecutionClient` from credentials AND the venue
    /// is in the default reconcile set (root `CLAUDE.md`, "Reconciliation engine").
    LiveExecRecon,
    /// The live arm compiles only under a vike-mount cargo feature (`ibkr` / `polymarket` /
    /// `fxcm`); a default build has no arm at all and falls through to paper. Reconcile, where
    /// wired, sits behind the same feature (plus each venue's own inner gates).
    FeatureGated,
    /// Exec runs through an owned child process (the dukascopy JForex Java sidecar over JSON-lines
    /// stdio) and no `make_engine` arm wires its `recon_client` factory — a `ReconClient` existing
    /// is not the same as being in the live reconcile set.
    SidecarExecNoRecon,
    /// Only the paper fallback mounts it — no live exec arm anywhere. No roster venue today; the
    /// variant exists so the class is expressible (and it is the scaffold's conservative
    /// placeholder for a venue whose bridge just landed).
    PaperOnly,
}

impl Wiring {
    /// The kebab-case string `venues.json` carries.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Wiring::LiveExecRecon => "live-exec-recon",
            Wiring::FeatureGated => "feature-gated",
            Wiring::SidecarExecNoRecon => "sidecar-exec-no-recon",
            Wiring::PaperOnly => "paper-only",
        }
    }
}

/// The declared wiring class per roster venue. One NAMED row per [`VENUES`] entry — the
/// completeness test (`docs_data_gate.rs`) fails until a new venue's row exists, and the marker
/// below hands the scaffold a conservative placeholder to render.
///
/// The ten `LiveExecRecon` rows are the default reconcile set the root `CLAUDE.md` names; the
/// three `FeatureGated` rows are the venues whose `make_engine` arm is `#[cfg(feature = …)]`-gated
/// in `crates/vike-mount/src/lib.rs`; dukascopy's row is argued on [`Wiring::SidecarExecNoRecon`]
/// itself.
///
/// `#[rustfmt::skip]`: a `just new-venue` marker at the TAIL of a bracketed literal is re-indented
/// by rustfmt once a row ending in a trailing `//` comment is generated above it, which defeats
/// `--remove`. Gated by `crates/vike-ops/tests/new_venue_gate.rs`'s
/// `a_trailing_comment_marker_is_rustfmt_skipped_unless_a_recognised_sibling_follows`.
#[rustfmt::skip]
pub const WIRING: &[(&str, Wiring)] = &[
    ("binance", Wiring::LiveExecRecon),
    ("bybit", Wiring::LiveExecRecon),
    ("okx", Wiring::LiveExecRecon),
    ("deribit", Wiring::LiveExecRecon),
    ("oanda", Wiring::LiveExecRecon),
    ("ig", Wiring::LiveExecRecon),
    ("fxcm", Wiring::FeatureGated),
    ("dukascopy", Wiring::SidecarExecNoRecon),
    ("polymarket", Wiring::FeatureGated),
    ("ibkr", Wiring::FeatureGated),
    ("ctrader", Wiring::LiveExecRecon),
    ("alpaca", Wiring::LiveExecRecon),
    ("aster", Wiring::LiveExecRecon),
    ("hyperliquid", Wiring::LiveExecRecon),
    // vike:new-venue:row ("{venue}", Wiring::PaperOnly), // TODO(new-venue: {venue}): classify the recon/exec wiring — PaperOnly is the scaffold's conservative placeholder, not a verdict
];

/// The declared [`Wiring`] for a canonical venue string; `None` for anything not in [`WIRING`].
/// `None` for a ROSTER venue is a gate failure (`wiring_map_is_roster_complete`), never a state
/// the renderer tolerates — see [`venue_record`].
#[must_use]
pub fn wiring_for(venue: &str) -> Option<Wiring> {
    WIRING.iter().find(|(v, _)| *v == venue).map(|&(_, w)| w)
}

/// Lowercase canonical name for a [`TimeInForce`] — the KEY vocabulary of the `tif` object and the
/// element vocabulary of `supported_tifs`/`accepted_tifs` (canonical names, never venue wire
/// strings — those live inside each `tif` entry's `wire` field).
const fn tif_name(tif: TimeInForce) -> &'static str {
    match tif {
        TimeInForce::Gtc => "gtc",
        TimeInForce::Ioc => "ioc",
        TimeInForce::Fok => "fok",
        TimeInForce::Gtd => "gtd",
        TimeInForce::Day => "day",
    }
}

/// All five TIFs, in the declaration order of [`TimeInForce`] — the `tif` object covers exactly
/// this set for every venue (key ORDER is outside the contract; see the module doc).
const ALL_TIFS: [TimeInForce; 5] =
    [TimeInForce::Gtc, TimeInForce::Ioc, TimeInForce::Fok, TimeInForce::Gtd, TimeInForce::Day];

const fn margin_mode_name(mode: MarginMode) -> &'static str {
    match mode {
        MarginMode::Cash => "cash",
        MarginMode::Cross => "cross",
        MarginMode::Isolated => "isolated",
    }
}

const fn switch_mechanism_name(mechanism: SwitchMechanism) -> &'static str {
    match mechanism {
        SwitchMechanism::PerOrderField => "per-order-field",
        SwitchMechanism::PerSymbolEndpoint => "per-symbol-endpoint",
        SwitchMechanism::AtLeverageSet => "at-leverage-set",
        SwitchMechanism::AccountLevel => "account-level",
        SwitchMechanism::NotApplicable => "not-applicable",
    }
}

const fn trigger_type_name(trigger: TriggerType) -> &'static str {
    match trigger {
        TriggerType::StopLoss => "stop-loss",
        TriggerType::TakeProfit => "take-profit",
    }
}

const fn amend_semantics_name(amend: AmendSemantics) -> &'static str {
    match amend {
        AmendSemantics::InPlaceTotal => "in-place-total",
        AmendSemantics::CancelReplace => "cancel-replace",
        AmendSemantics::InPlaceRemaining => "in-place-remaining",
        AmendSemantics::Unsupported => "unsupported",
        AmendSemantics::Unknown => "unknown",
    }
}

/// [`FeeSchedule`] as a tagged object: `kind` names the shape, the shape's own fields ride beside
/// it. Exhaustive on purpose — a new variant is a compile error here, never a silent omission.
fn fee_value(fees: FeeSchedule) -> Value {
    match fees {
        FeeSchedule::PercentMakerTaker { maker_bps, taker_bps } => {
            json!({ "kind": "percent-maker-taker", "maker_bps": maker_bps, "taker_bps": taker_bps })
        }
        FeeSchedule::PerShareWithFloor { per_share, min, max_pct } => {
            json!({ "kind": "per-share-with-floor", "per_share": per_share, "min": min, "max_pct": max_pct })
        }
        FeeSchedule::PercentOfUnderlying { bps, premium_cap_pct } => {
            json!({ "kind": "percent-of-underlying", "bps": bps, "premium_cap_pct": premium_cap_pct })
        }
        FeeSchedule::ProbabilityScaled { taker_rate, maker_rate, maker_rebate_share } => {
            json!({
                "kind": "probability-scaled",
                "taker_rate": taker_rate,
                "maker_rate": maker_rate,
                "maker_rebate_share": maker_rebate_share,
            })
        }
        FeeSchedule::Free => json!({ "kind": "free" }),
    }
}

/// [`AttributionMechanic`] as a tagged object, same convention as [`fee_value`].
fn attribution_value(mechanic: AttributionMechanic) -> Value {
    match mechanic {
        AttributionMechanic::CoidPrefix { max_total_len } => {
            json!({ "kind": "coid-prefix", "max_total_len": max_total_len })
        }
        AttributionMechanic::Header { name } => json!({ "kind": "header", "name": name }),
        AttributionMechanic::OrderTag { field, max_len } => {
            json!({ "kind": "order-tag", "field": field, "max_len": max_len })
        }
        AttributionMechanic::SignedBuilder { needs_onchain_approval } => {
            json!({ "kind": "signed-builder", "needs_onchain_approval": needs_onchain_approval })
        }
        AttributionMechanic::None => json!({ "kind": "none" }),
    }
}

/// One [`TifOutcome`] as a tagged object. `Coerced`'s `from` is the entry's own key, so only the
/// substituted `to` and the wire string are carried.
fn tif_outcome_value(outcome: TifOutcome) -> Value {
    match outcome {
        TifOutcome::Mapped(wire) => json!({ "outcome": "mapped", "wire": wire }),
        TifOutcome::Coerced { from: _, to, wire } => {
            json!({ "outcome": "coerced", "to": tif_name(to), "wire": wire })
        }
        TifOutcome::Ignored { wire } => json!({ "outcome": "ignored", "wire": wire }),
        TifOutcome::NotEmitted => json!({ "outcome": "not-emitted" }),
        TifOutcome::Unsupported => json!({ "outcome": "unsupported" }),
    }
}

fn live_data_value(live: LiveDataCaps) -> Value {
    json!({
        "bars": live.bars,
        "quotes": live.quotes,
        "trades": live.trades,
        "book": live.book,
        "depth": live.depth,
    })
}

/// `usize::MAX` means "the adapter imposes no cap of its own" ([`VenueCaps::max_batch`]'s doc) and
/// renders `null` — a 2^64-magnitude integer would silently lose precision in every JS consumer.
fn max_batch_value(max_batch: usize) -> Value {
    if max_batch == usize::MAX { Value::Null } else { Value::from(max_batch) }
}

fn caps_value(caps: VenueCaps) -> Value {
    json!({
        "supports_modify": caps.supports_modify,
        "supports_native_batch": caps.supports_native_batch,
        "supports_reduce_only": caps.supports_reduce_only,
        "supports_combo": caps.supports_combo,
        "supports_post_only": caps.supports_post_only,
        "supported_tifs": caps.supported_tifs.iter().copied().map(tif_name).collect::<Vec<_>>(),
        "accepted_tifs": caps.accepted_tifs.iter().copied().map(tif_name).collect::<Vec<_>>(),
        "supported_order_kinds": caps.supported_order_kinds,
        "trigger_types":
            caps.trigger_types.iter().copied().map(trigger_type_name).collect::<Vec<_>>(),
        "margin_modes": caps.margin_modes.iter().copied().map(margin_mode_name).collect::<Vec<_>>(),
        "default_margin_mode": margin_mode_name(caps.default_margin_mode),
        "max_batch": max_batch_value(caps.max_batch),
        "live_data": live_data_value(caps.live_data),
        "backfill_bars": caps.backfill_bars,
        "backfill_ticks": caps.backfill_ticks,
    })
}

fn margin_value(margin: VenueMarginSupport) -> Value {
    json!({
        "offered_modes":
            margin.offered_modes.iter().copied().map(margin_mode_name).collect::<Vec<_>>(),
        "switch_mechanism": switch_mechanism_name(margin.switch_mechanism),
        "isolated_wallet_adjustable": margin.isolated_wallet_adjustable,
    })
}

/// One venue's complete `venues.json` record.
///
/// # Panics
/// On a roster venue with no [`WIRING`] row — deliberately: exporting a guessed class would be the
/// silent-wrong-answer failure the playbook exists to remove, and `wiring_map_is_roster_complete`
/// keeps this arm unreachable from a green tree.
#[must_use]
pub fn venue_record(venue: &str) -> Value {
    let wiring = wiring_for(venue).unwrap_or_else(|| {
        panic!("roster venue {venue} has no WIRING row — add one (docs_data.rs) before exporting")
    });
    let tif: Value = ALL_TIFS
        .iter()
        .map(|&t| (tif_name(t).to_string(), tif_outcome_value(venue_tif(venue, t))))
        .collect::<serde_json::Map<String, Value>>()
        .into();
    json!({
        "id": venue,
        "wiring": wiring.as_str(),
        "caps": caps_value(caps_for(venue)),
        "margin": margin_value(venue_margin_support(venue)),
        "fees": fee_value(fee_schedule_for(venue)),
        "amend_semantics": amend_semantics_name(amend_semantics(venue)),
        "attribution": attribution_value(attribution_for(venue)),
        "tif": tif,
    })
}

/// The whole `venues.json` document: a top-level ARRAY, one [`venue_record`] per [`VENUES`] entry,
/// in roster order.
#[must_use]
pub fn venues_value() -> Value {
    Value::Array(VENUES.iter().copied().map(venue_record).collect())
}

/// The whole `stats.json` document. `generated_from` is the caller's — the commit this render
/// describes, resolved by [`generated_from`] from the bin's argv; see [`DEFAULT_GENERATED_FROM`]
/// for why it is a parameter and not baked in.
#[must_use]
pub fn stats_value(generated_from: &str) -> Value {
    json!({
        "schema_version": SCHEMA_VERSION,
        "venue_count": VENUES.len(),
        "indicator_count": indicator_registry().len(),
        "pair_indicator_count": pair_registry().len(),
        "portable_strategy_count": PORTABLE_STRATEGIES.len(),
        "simulator_only_strategy_count": SIMULATOR_ONLY.len(),
        "script_only_strategy_count": SCRIPT_ONLY.len(),
        "event_count": EVENTS.len(),
        "store_kind_count": STORE_KINDS.len(),
        "order_kind_count": ORDER_KINDS.len(),
        "asset_class_count": ALL_ASSET_CLASSES.len(),
        "divergence_kind_count": ALL_DIVERGENCE_KINDS.len(),
        "latency_p99_budget_ns": P99_BUDGET_NS,
        "generator_version": env!("CARGO_PKG_VERSION"),
        "generated_from": generated_from,
    })
}

// ── indicators.json ──────────────────────────────────────────────────────────────────────────────

/// Every [`Category`], in declaration order — `indicators.json`'s `categories` array covers exactly
/// this set, EMPTY ones included (see the module doc). A new variant is a compile error in
/// [`category_name`] and a gate failure in `docs_data_gate.rs`'s category-set assertion until it is
/// added here, which is the intended cost.
const ALL_CATEGORIES: [Category; 9] = [
    Category::Overlap,
    Category::Momentum,
    Category::Volatility,
    Category::Volume,
    Category::Statistics,
    Category::Pattern,
    Category::Price,
    Category::Structure,
    Category::User,
];

/// Kebab-case name of a [`Category`] — the `name` of a category record.
const fn category_name(category: &Category) -> &'static str {
    match category {
        Category::Overlap => "overlap",
        Category::Momentum => "momentum",
        Category::Volatility => "volatility",
        Category::Volume => "volume",
        Category::Statistics => "statistics",
        Category::Pattern => "pattern",
        Category::Price => "price",
        Category::Structure => "structure",
        Category::User => "user",
    }
}

/// One [`ParamSpec`]: the parameter's name, its built-in default, and the range the Studio's
/// sweep grid offers for it. All four numbers are the registry's own `f64`s, rendered as JSON
/// numbers.
fn param_spec_value(spec: &ParamSpec) -> Value {
    json!({
        "name": spec.name,
        "default": spec.default,
        "min": spec.min,
        "max": spec.max,
        "step": spec.step,
    })
}

/// One built-in indicator: registry `name` as `id`, `pretty` as `display`, its parameter surface,
/// and `batch_only` — the flag marking the few indicators whose streaming path cannot equal the
/// batch kernel because the batch reads future bars ([`IndicatorMeta::batch_only`]'s doc).
fn indicator_value(meta: &IndicatorMeta) -> Value {
    json!({
        "id": meta.name,
        "display": meta.pretty,
        "params": meta.params.iter().map(param_spec_value).collect::<Vec<_>>(),
        "batch_only": meta.batch_only,
    })
}

/// One pair indicator ([`PairMeta`]) — the same shape minus `batch_only`, which the pair registry
/// does not carry.
fn pair_value(meta: &PairMeta) -> Value {
    json!({
        "id": meta.name,
        "display": meta.pretty,
        "params": meta.params.iter().map(param_spec_value).collect::<Vec<_>>(),
    })
}

/// The whole `indicators.json` document: `categories` (every [`Category`], each with its built-in
/// rows in registry order) and `pairs` (every pair-registry row, in registry order).
#[must_use]
pub fn indicators_value() -> Value {
    let categories: Vec<Value> = ALL_CATEGORIES
        .iter()
        .map(|category| {
            let name = category_name(category);
            let indicators: Vec<Value> = indicator_registry()
                .iter()
                .filter(|meta| category_name(&meta.category) == name)
                .map(indicator_value)
                .collect();
            json!({ "name": name, "indicators": indicators })
        })
        .collect();
    let pairs: Vec<Value> = pair_registry().iter().map(pair_value).collect();
    json!({ "categories": categories, "pairs": pairs })
}

// ── templates.json ───────────────────────────────────────────────────────────────────────────────

/// Kebab-case name of a [`ParamType`] — the accessor class a strategy's `from_params` reads a key
/// through (`vike_strategy::registry::PARAM_KEYS`'s doc).
const fn param_type_name(kind: &ParamType) -> &'static str {
    match kind {
        ParamType::Number => "number",
        ParamType::Integer => "integer",
        ParamType::Str => "string",
        ParamType::Bool => "bool",
        ParamType::Table => "table",
        ParamType::StrOrInteger => "string-or-integer",
    }
}

/// A strategy's [`ParamKeys`] row as a tagged object: `declared` carries the key list,
/// `not-enumerated` carries the registry's stated reason. Tagged rather than a bare array for the
/// same reason as [`fee_value`] — the second variant has a payload a list cannot hold.
fn param_keys_value(keys: &ParamKeys) -> Value {
    match keys {
        ParamKeys::Declared(list) => json!({
            "kind": "declared",
            "keys": list
                .iter()
                .map(|(name, kind)| json!({ "name": name, "kind": param_type_name(kind) }))
                .collect::<Vec<_>>(),
        }),
        ParamKeys::NotEnumerated(reason) => json!({ "kind": "not-enumerated", "reason": reason }),
    }
}

/// A `LIVE_CAPABLE` row: `verdict` is `true` exactly when the registry says the strategy is
/// mountable on the live core today, and `reason` is the registry's stated blocker otherwise —
/// `null` on a `true` verdict, never an empty string, so a consumer cannot mistake one for the
/// other.
fn live_capable_value(blocker: Option<&str>) -> Value {
    match blocker {
        None => json!({ "verdict": true, "reason": Value::Null }),
        Some(reason) => json!({ "verdict": false, "reason": reason }),
    }
}

/// One portable strategy's complete `templates.json` record.
///
/// # Panics
/// On a `PORTABLE_STRATEGIES` name with no `PARAM_KEYS` or `LIVE_CAPABLE` row — deliberately, the
/// same rule as [`venue_record`]: the strategy registry's own tests hold those tables exhaustive,
/// so this arm is unreachable from a green tree and exporting a guess would be the silent-wrong
/// answer the tables exist to remove.
#[must_use]
pub fn template_record(id: &str) -> Value {
    let (_, keys) = PARAM_KEYS
        .iter()
        .find(|(name, _)| *name == id)
        .unwrap_or_else(|| panic!("portable strategy {id} has no PARAM_KEYS row"));
    let (_, blocker) = LIVE_CAPABLE
        .iter()
        .find(|(name, _)| *name == id)
        .unwrap_or_else(|| panic!("portable strategy {id} has no LIVE_CAPABLE row"));
    json!({
        "id": id,
        "params": param_keys_value(keys),
        "live_capable": live_capable_value(*blocker),
    })
}

/// A `(name, reason)` roster row — the shape `SIMULATOR_ONLY` and `SCRIPT_ONLY` share.
fn reasoned_row(row: &(&str, &str)) -> Value {
    let (id, reason) = row;
    json!({ "id": id, "reason": reason })
}

/// The whole `templates.json` document: `portable` (one [`template_record`] per
/// `PORTABLE_STRATEGIES` entry, in roster order), `simulator_only` (one `{ id, reason }` per
/// `SIMULATOR_ONLY` entry — the names a backtest profile may still resolve that a live mount
/// cannot) and `script_only` (the same shape over `SCRIPT_ONLY` — the inline-`src` script arm that
/// lives in `vike-backtest` and sits on no roster), each in its table's order and each carrying the
/// registry's own reason.
#[must_use]
pub fn templates_value() -> Value {
    json!({
        "portable": PORTABLE_STRATEGIES.iter().copied().map(template_record).collect::<Vec<_>>(),
        "simulator_only": SIMULATOR_ONLY.iter().map(reasoned_row).collect::<Vec<_>>(),
        "script_only": SCRIPT_ONLY.iter().map(reasoned_row).collect::<Vec<_>>(),
    })
}

// ── the rendered set ─────────────────────────────────────────────────────────────────────────────

/// Every asset this module renders, as `(file name, pretty JSON + trailing newline)` — `venues.json`
/// and `stats.json` first, in that order, so the two positions the original consumers read are
/// stable; `indicators.json`, `templates.json` and `rosters.json` follow. The ONE rendering the bin
/// writes and the gate test parses — in-process, so the test proves the exact bytes a release
/// attaches. `generated_from` rides straight into `stats.json` through [`stats_value`]; everything
/// else is derived from the compile-time tables, so one tree plus one stamp is one rendering.
#[must_use]
pub fn rendered_files(generated_from: &str) -> [(&'static str, String); 5] {
    [
        (VENUES_JSON, render(&venues_value())),
        (STATS_JSON, render(&stats_value(generated_from))),
        (INDICATORS_JSON, render(&indicators_value())),
        (TEMPLATES_JSON, render(&templates_value())),
        (ROSTERS_JSON, render(&rosters_value())),
    ]
}

fn render(value: &Value) -> String {
    let mut out = serde_json::to_string_pretty(value)
        .expect("a serde_json::Value with string keys always serializes");
    out.push('\n');
    out
}

// ── rosters.json ─────────────────────────────────────────────────────────────────────────────────

/// Every `vike_model::Event` variant, as `(variant, payload type, wire tag)`.
///
/// DECLARED here rather than derived, and that is forced: the wire tag is a `#[serde(rename = …)]`
/// ATTRIBUTE, which no runtime renderer can read, and the enum has no value this module could
/// enumerate. The declaration is therefore PINNED against its authority the way [`P99_BUDGET_NS`]
/// is: `docs_data_gate.rs`'s `event_table_matches_the_event_enum` parses
/// `crates/vike-model/src/events.rs`'s enum body — every variant, its payload type, and the rename
/// where one exists — and fails when this table and that source disagree, so a new variant reddens
/// CI until it is listed.
///
/// Two variants carry a rename because their wire tag predates the Rust spelling: `Fill` ships as
/// `"FillEvent"` and `Funding` as `"FundingEvent"` — fixtures and the exec journal pin those tags,
/// so only the Rust variant was shortened.
pub const EVENTS: &[(&str, &str, &str)] = &[
    ("Fill", "FillEvent", "FillEvent"),
    ("OrderSubmitted", "OrderSubmitted", "OrderSubmitted"),
    ("OrderAccepted", "OrderAccepted", "OrderAccepted"),
    ("OrderRejected", "OrderRejected", "OrderRejected"),
    ("OrderDenied", "OrderDenied", "OrderDenied"),
    ("OrderTriggered", "OrderTriggered", "OrderTriggered"),
    ("OrderPartiallyFilled", "OrderPartiallyFilled", "OrderPartiallyFilled"),
    ("OrderFilled", "OrderFilled", "OrderFilled"),
    ("OrderCanceled", "OrderCanceled", "OrderCanceled"),
    ("OrderExpired", "OrderExpired", "OrderExpired"),
    ("OrderLiquidated", "OrderLiquidated", "OrderLiquidated"),
    ("OrderModified", "OrderModified", "OrderModified"),
    ("PositionOpened", "PositionOpened", "PositionOpened"),
    ("PositionChanged", "PositionChanged", "PositionChanged"),
    ("PositionClosed", "PositionClosed", "PositionClosed"),
    ("AccountState", "AccountState", "AccountState"),
    ("Funding", "FundingEvent", "FundingEvent"),
    ("PositionLiquidated", "PositionLiquidated", "PositionLiquidated"),
    ("OrderCancelRejected", "OrderCancelRejected", "OrderCancelRejected"),
    ("OrderModifyRejected", "OrderModifyRejected", "OrderModifyRejected"),
];

/// The `rosters.json` asset name.
pub const ROSTERS_JSON: &str = "rosters.json";

/// Every [`AssetClass`], in declaration order. A new variant is a compile error in
/// [`asset_class_name`] and a gate failure until it joins this array.
const ALL_ASSET_CLASSES: [AssetClass; 11] = [
    AssetClass::Equity,
    AssetClass::Etf,
    AssetClass::CryptoSpot,
    AssetClass::CryptoPerp,
    AssetClass::CryptoFuture,
    AssetClass::Option,
    AssetClass::Fx,
    AssetClass::Future,
    AssetClass::Index,
    AssetClass::PredictionMarket,
    AssetClass::Cfd,
];

/// Kebab-case id of an [`AssetClass`].
///
/// ⚠ `Index` renders `index-instrument`, NOT `index`: every consumer of this roster keys a page on
/// the id, and `index` is the reserved slug of a directory's own index page — a class rendered as
/// `index` would silently overwrite the tree's landing page. The docs architecture states the same
/// exception from the docs side; this is where it is enforced.
const fn asset_class_name(class: AssetClass) -> &'static str {
    match class {
        AssetClass::Equity => "equity",
        AssetClass::Etf => "etf",
        AssetClass::CryptoSpot => "crypto-spot",
        AssetClass::CryptoPerp => "crypto-perp",
        AssetClass::CryptoFuture => "crypto-future",
        AssetClass::Option => "option",
        AssetClass::Fx => "fx",
        AssetClass::Future => "future",
        AssetClass::Index => "index-instrument",
        AssetClass::PredictionMarket => "prediction-market",
        AssetClass::Cfd => "cfd",
    }
}

/// Every [`DivergenceKind`], in declaration order — the same nine
/// `crates/vike-exec/tests/recon/recon_policy_pin.rs` pins, listed here so this renderer can ask
/// each one the policy question.
const ALL_DIVERGENCE_KINDS: [DivergenceKind; 9] = [
    DivergenceKind::MissingFill,
    DivergenceKind::MissingTerminal,
    DivergenceKind::OrphanLocalOrder,
    DivergenceKind::UnknownOrder,
    DivergenceKind::PositionDrift,
    DivergenceKind::PositionOnlyExternal,
    DivergenceKind::OrphanLocalPosition,
    DivergenceKind::BalanceDrift,
    DivergenceKind::JournalDivergence,
];

/// Kebab-case id of a [`DivergenceKind`] — the slug a per-kind page is keyed on.
const fn divergence_kind_name(kind: DivergenceKind) -> &'static str {
    match kind {
        DivergenceKind::MissingFill => "missing-fill",
        DivergenceKind::MissingTerminal => "missing-terminal",
        DivergenceKind::OrphanLocalOrder => "orphan-local-order",
        DivergenceKind::UnknownOrder => "unknown-order",
        DivergenceKind::PositionDrift => "position-drift",
        DivergenceKind::PositionOnlyExternal => "position-only-external",
        DivergenceKind::OrphanLocalPosition => "orphan-local-position",
        DivergenceKind::BalanceDrift => "balance-drift",
        DivergenceKind::JournalDivergence => "journal-divergence",
    }
}

/// Kebab-case name of a [`DivergenceOrigin`] — where the EVIDENCE for a divergence came from, the
/// axis `external-quarantine` keys on.
const fn divergence_origin_name(origin: DivergenceOrigin) -> &'static str {
    match origin {
        DivergenceOrigin::ReconciliationMaterialized => "reconciliation-materialized",
        DivergenceOrigin::External => "external",
        DivergenceOrigin::Absence => "absence",
    }
}

/// The kinds whose resolution produces real events — the candidate set
/// `vike_ops::reconcile_config::auto_applied_kinds` filters through `mode_applies`. Every other
/// kind folds nothing whatever its policy verdict, which is the distinction
/// [`divergence_record`]'s doc exists to keep visible.
const RESOLVING_KINDS: [DivergenceKind; 5] = [
    DivergenceKind::MissingFill,
    DivergenceKind::PositionDrift,
    DivergenceKind::UnknownOrder,
    DivergenceKind::PositionOnlyExternal,
    DivergenceKind::BalanceDrift,
];

/// One divergence kind's record: its id, its origin class, and what each of the four policies does
/// with it.
///
/// The per-policy answer is **COMPUTED** through `vike_exec::recon::mode_applies`, never restated.
/// That is the whole reason this roster exists: the workspace's own guidance records that the
/// opposite claim about `hybrid` was written down in six places and was false in all six, and the
/// cure it names is asking the function rather than copying a list. `"fold"` means the policy
/// auto-applies the kind; `"hold"` means it is quarantined for an operator.
///
/// ⚠ `fold` is not the same as "does something": `OrphanLocalOrder` and `MissingTerminal` fold
/// under `hybrid` and resolve to EMPTY event lists, so they change nothing and do not even alert.
/// The `resolves_to_events` flag carries that distinction, from the same candidate set
/// (`RESOLVING_KINDS`) `vike_ops::reconcile_config::auto_applied_kinds` filters.
fn divergence_record(kind: DivergenceKind) -> Value {
    let policies: serde_json::Map<String, Value> = POLICY_NAMES
        .iter()
        .map(|&name| {
            let policy = ReconPolicy::from_policy_name(name).unwrap_or_else(|| {
                panic!("POLICY_NAMES lists {name}, which ReconPolicy::from_policy_name refuses")
            });
            let verdict = if mode_applies(&policy, kind) { "fold" } else { "hold" };
            (name.to_string(), Value::from(verdict))
        })
        .collect();
    json!({
        "id": divergence_kind_name(kind),
        "variant": format!("{kind:?}"),
        "origin": divergence_origin_name(kind.origin()),
        "resolves_to_events": RESOLVING_KINDS.contains(&kind),
        "policies": Value::Object(policies),
    })
}

/// How a store kind's series leaf is partitioned, kebab-case.
const fn partition_name(partition: Partition) -> &'static str {
    match partition {
        Partition::Symbol => "symbol",
        Partition::SymbolInterval => "symbol-interval",
    }
}

/// One `kind=` of the hist store, rendered from its [`StoreKind`] row verbatim — the layout
/// contract its producers and consumers must agree on.
fn store_kind_record(k: &StoreKind) -> Value {
    json!({
        "kind": k.kind,
        "row": k.row,
        "codec": k.codec,
        "write_verb": k.write_verb,
        "read_verb": k.read_verb,
        "columns": k
            .columns
            .iter()
            .map(|(name, flavor)| json!({ "name": name, "flavor": flavor }))
            .collect::<Vec<_>>(),
        "schema_meta": k.schema_meta,
        "partition": partition_name(k.partition),
        "grouped": k.grouped,
        "tick_lane": k.tick_lane,
        "identity": k.identity,
        "commit_keys": k
            .commit_keys
            .iter()
            .map(|c| json!({ "producer": c.producer, "template": c.template }))
            .collect::<Vec<_>>(),
        "notes": k.notes,
    })
}

/// The whole `rosters.json` document: the five remaining CI-gated rosters the docs render a page
/// per member from, each from the table the runtime consults.
///
/// `events` is the declared-and-pinned [`EVENTS`] table (see its doc for why a renderer cannot
/// derive it); `order_kinds` is `vike_model::venue_caps`'s `ORDER_KINDS` with its `TRIGGER_KINDS`
/// subset; `asset_classes` is [`AssetClass`] with each class's Symbol-picker tab; `store_kinds` is
/// `vike_data::store_kind::STORE_KINDS` verbatim; `divergence_kinds` is every
/// `vike_exec::recon::DivergenceKind` with its origin and its four COMPUTED policy verdicts.
///
/// ⚠ `commit_keys` inside a store kind is OBSERVED, not exhaustive, and `StoreKind::commit_keys`'s
/// own doc explains why no gate can make it so: a commit key is an ordinary `&str` argument. What
/// IS gated per listed entry is that the producer file exists and spells the template verbatim. A
/// consumer must not render it as "the producers of this kind".
#[must_use]
pub fn rosters_value() -> Value {
    json!({
        "events": EVENTS
            .iter()
            .map(|(variant, payload, wire_tag)| json!({
                "variant": variant,
                "payload": payload,
                "wire_tag": wire_tag,
            }))
            .collect::<Vec<_>>(),
        "order_kinds": {
            "all": ORDER_KINDS,
            "trigger": TRIGGER_KINDS,
        },
        "asset_classes": ALL_ASSET_CLASSES
            .iter()
            .map(|&c| json!({
                "id": asset_class_name(c),
                "variant": format!("{c:?}"),
                "picker_tab": c.tab().label(),
            }))
            .collect::<Vec<_>>(),
        "store_kinds": STORE_KINDS.iter().map(store_kind_record).collect::<Vec<_>>(),
        "divergence_kinds": ALL_DIVERGENCE_KINDS
            .iter()
            .map(|&k| divergence_record(k))
            .collect::<Vec<_>>(),
    })
}
