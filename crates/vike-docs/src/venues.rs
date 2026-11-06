//! `venues.json` — one record per roster venue, rendered from the per-venue capability tables.
//! - `wiring` — `vike_venues::venue_wiring::wiring_for`'s `Wiring::as_str`: how the trading
//!   daemon wires the venue's exec and reconcile sides (`crates/vike-venues/src/venue_wiring.rs`).
//! - `caps` — `vike_venues::caps_for` (`crates/vike-venues/src/venue_caps.rs`): what the ADAPTER
//!   wires today.
//! - `margin` — `vike_venues::venue_margin_support`: what the EXCHANGE offers (vendor-doc
//!   transcription — that module's evidence-class note travels with the data, not with this
//!   renderer).
//! - `fees` — `vike_venues::fee_schedule_for`, the DEFAULT registry: polymarket renders `free`; the
//!   V2 probability curve is the `fee_schedule_for_with_pm_curve` OPT-IN and is deliberately not
//!   exported, for the same default-compatibility reason that registry keeps `Free`.
//! - `amend_semantics` — `vike_venues::amend_semantics`: what an amend's QUANTITY means on this
//!   venue. ⚠ A second table on the axis of `caps.supports_modify`, keyed differently (`caps_for`
//!   resolves a per-BACKEND row), so a record can carry a pair that reads as a contradiction.
//! - `amend_semantics_note` — `vike_venues::venue_amend`'s `CAPS_DISAGREEMENTS` through
//!   `vike_venues::amend_caps_note`: that pair's explanation, or `null` when the two agree (every
//!   roster venue but ibkr today). Gated in both directions by
//!   `crates/vike-docs/tests/docs_data_gate/venues.rs`'s
//!   `an_incoherent_amend_pair_is_explained_and_a_coherent_one_is_not`.
//! - `attribution` — `vike_venues::attribution::attribution_for`.
//! - `tif` — `vike_venues::venue_tif::venue_tif`, one entry per [`vike_model::TimeInForce`]
//!   (every `TimeInForce::ALL` member, for every venue; the key SET is the contract, its order is
//!   not — see the crate doc's "Output contract"). ⚠ Keyed by ROSTER id only: binance's entry is
//!   its SPOT lane, and the `"binance-perp"` LANE sub-key (see `venue_tif`'s own doc) is
//!   deliberately not exported — a lane sub-key is not a roster venue. The `"binance-perp"` fee
//!   lane is skipped on the same grounds.

use serde_json::{Value, json};
use vike_model::{FeeSchedule, MarginMode, SwitchMechanism, TimeInForce, VENUES};
use vike_venues::attribution::{AttributionMechanic, attribution_for};
use vike_venues::venue_tif::{TifOutcome, venue_tif};
use vike_venues::venue_wiring::wiring_for;
use vike_venues::{
    AmendSemantics, TriggerType, VenueCaps, amend_caps_note, amend_semantics, caps_for,
    fee_schedule_for, venue_caps::LiveDataCaps, venue_margin_support,
    venue_margin_support::VenueMarginSupport,
};

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
        TimeInForce::PostOnly => "post_only",
    }
}

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
        "reduce_only_kinds": caps.reduce_only_kinds,
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
/// On a roster venue with no `VENUE_WIRING` row — deliberately: exporting a guessed class would be the
/// silent-wrong-answer failure the playbook exists to remove, and `vike-venues`'s
/// `the_wiring_table_is_roster_complete` keeps this arm unreachable from a green tree.
#[must_use]
pub fn venue_record(venue: &str) -> Value {
    let wiring = wiring_for(venue).unwrap_or_else(|| {
        panic!(
            "roster venue {venue} has no VENUE_WIRING row — add one \
             (crates/vike-venues/src/venue_wiring.rs) before exporting"
        )
    });
    let tif: Value = TimeInForce::ALL
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
        "amend_semantics_note": amend_caps_note(venue),
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
