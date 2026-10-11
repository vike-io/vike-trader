//! The ONE cross-venue trigger-price-source authority (the [`vike_model::venues::venue_tif`] pattern
//! applied to `OrderRequest.trigger_by`): this table ENCODES what each venue adapter does with a
//! requested trigger source on its CONDITIONAL/trigger-order path, and
//! [`deny_unsupported_trigger_by`] is the shared submit-side gate for the inexpressible rows.
//!
//! `None` (no requested source) is NOT in this table by design: every adapter then emits its
//! default wire bytes (binance: no `workingType` → venue default `CONTRACT_PRICE`; bybit:
//! `triggerBy:"LastPrice"`; okx: no `slTriggerPxType` → venue default `last`; hyperliquid: no field
//! exists — MARK by venue law). The table answers only "what happens when a source IS requested":
//!
//! - binance perp (the `"binance-perp"` lane sub-key, mirroring [`vike_model::venues::venue_tif`]'s
//!   lane split): fapi `workingType` expresses `CONTRACT_PRICE` (last) and `MARK_PRICE` only — Index
//!   is a LOUD deny. The spot lane (`"binance"`) has no trigger-source axis (spot stops evaluate on
//!   last trades): Last matches that law, Mark/Index deny.
//! - bybit: V5 `triggerBy` expresses all three (LastPrice/MarkPrice/IndexPrice).
//! - okx: the algo-order `slTriggerPxType` expresses all three (`last`/`mark`/`index`), but ONLY on
//!   the stop-loss leg (`submit_stop_algo`/`build_stop_algo_params`). ⚠ OKX has no take-profit
//!   routing: `submit_order` diverts only `"stop"` to the algo endpoint, so a requested `trigger_by`
//!   on a would-be OKX take-profit has nowhere to ride the wire AND nothing here denies it. That gap
//!   closes with OKX take-profit routing, outside this table
//!   (`docs/superpowers/specs/2026-07-18-order-emulator-design.md` §0.2 item 5).
//! - hyperliquid: NO trigger-source field exists and triggers evaluate against MARK by venue
//!   law — Mark is accepted as [`TriggerByOutcome::VenueLaw`] (nothing to emit), Last/Index deny.
//! - aster: shares binance's family builder (`family::order_map::build_perp_order_params` consumes
//!   this row keyed by the caller-supplied `venue`) and binance-perp's law: its fapi fork documents
//!   `workingType` (MARK_PRICE/CONTRACT_PRICE) as a supported perp order param
//!   (`docs/superpowers/specs/2026-07-16-aster-dex-bridge-design.md` §"Orders & capabilities"), so
//!   Last/Mark map and Index is [`TriggerByOutcome::Unsupported`], gated at submit
//!   (`AsterPerpRest::submit_order`/`submit_batch`) exactly as binance-perp's. Its TIF row is a
//!   separate question (`venue_tif` still reads `Ignored{GTC}` for aster).
//! - every other venue (and unknown ids): [`TriggerByOutcome::Ignored`] fallthrough — the
//!   adapter never reads `trigger_by`. A venue that grows a trigger-order path MUST add its row.

use vike_model::events::{Event, OrderRejected};
use vike_model::{OrderRequest, TriggerBy};

/// What one venue adapter does with a REQUESTED `OrderRequest.trigger_by` on its trigger-order
/// path (`None` never consults this table — see the module doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerByOutcome {
    /// The requested source goes on the wire 1:1 as this venue string.
    Mapped(&'static str),
    /// The venue triggers on exactly this source BY LAW and exposes no field to say so — the
    /// request matches reality, nothing is emitted, submit proceeds.
    VenueLaw,
    /// The adapter never reads `trigger_by`; the wire is byte-identical regardless (every venue
    /// without a modeled trigger-source axis).
    Ignored,
    /// The venue cannot express this source on its wire, and the adapter REFUSES the order at
    /// submit — a terminal `OrderRejected` (via [`deny_unsupported_trigger_by`], pushed after
    /// the synchronous `OrderSubmitted` per the emitter split) instead of a silent substitution
    /// of a different trigger series (the roster-gate philosophy).
    Unsupported,
}

impl TriggerByOutcome {
    /// The wire string a MAPPING venue emits for the requested source; `None` when nothing
    /// reaches the wire (`VenueLaw` / `Ignored` / `Unsupported`).
    #[must_use]
    pub fn wire(self) -> Option<&'static str> {
        match self {
            TriggerByOutcome::Mapped(w) => Some(w),
            TriggerByOutcome::VenueLaw
            | TriggerByOutcome::Ignored
            | TriggerByOutcome::Unsupported => None,
        }
    }
}

/// Per-venue trigger-source reality, one row per `(venue, source)`. `venue` is the canonical
/// lowercase venue id — plus the `"binance-perp"` LANE sub-key (`vike_binance::perp::TIF_LANE`),
/// exactly as in [`vike_model::venues::venue_tif::venue_tif`], because binance's spot and perp lanes
/// genuinely diverge (spot has no trigger-source axis; fapi has `workingType`).
#[must_use]
pub fn venue_trigger_by(venue: &str, source: TriggerBy) -> TriggerByOutcome {
    use TriggerBy::{Index, Last, Mark};
    match venue {
        // Consumer: binance family/order_map.rs `build_perp_order_params` (stop arm).
        "binance-perp" => match source {
            Last => TriggerByOutcome::Mapped("CONTRACT_PRICE"),
            Mark => TriggerByOutcome::Mapped("MARK_PRICE"),
            Index => TriggerByOutcome::Unsupported,
        },
        // Spot stops (STOP_LOSS*) evaluate on last trades, the only series spot has.
        "binance" => match source {
            Last => TriggerByOutcome::VenueLaw,
            Mark | Index => TriggerByOutcome::Unsupported,
        },
        // Consumer: bybit perp.rs `build_order_params` (stop arm); a None request emits LastPrice.
        "bybit" => match source {
            Last => TriggerByOutcome::Mapped("LastPrice"),
            Mark => TriggerByOutcome::Mapped("MarkPrice"),
            Index => TriggerByOutcome::Mapped("IndexPrice"),
        },
        // Consumer: okx perp.rs's stop-algo path (`slTriggerPxType`; venue default when absent =
        // last).
        "okx" => match source {
            Last => TriggerByOutcome::Mapped("last"),
            Mark => TriggerByOutcome::Mapped("mark"),
            Index => TriggerByOutcome::Mapped("index"),
        },
        // Consumer: hyperliquid exec.rs `build_order_wire`; MARK by venue law, no field.
        "hyperliquid" => match source {
            Mark => TriggerByOutcome::VenueLaw,
            Last | Index => TriggerByOutcome::Unsupported,
        },
        // Consumer: the same binance family builder; binance-perp's law (module doc has the
        // evidence).
        "aster" => match source {
            Last => TriggerByOutcome::Mapped("CONTRACT_PRICE"),
            Mark => TriggerByOutcome::Mapped("MARK_PRICE"),
            Index => TriggerByOutcome::Unsupported,
        },
        // A venue growing a trigger-order path MUST add its row here.
        _ => TriggerByOutcome::Ignored,
    }
}

/// Is this request a TRIGGER order — the only shape where a trigger-source axis exists? Matches
/// the venue adapters' own routing: an explicit `stop`/`take_profit` order type, or any request
/// carrying a `trigger_price` (hyperliquid's trigger predicate).
fn is_trigger_order(request: &OrderRequest) -> bool {
    request.order_type.eq_ignore_ascii_case("stop")
        || request.order_type.eq_ignore_ascii_case("take_profit")
        || request.trigger_price.is_some()
}

/// The submit-side trigger-source gate: `Some(OrderRejected)` — terminal, per the emitter split
/// (the adapter pushes it after the synchronous `OrderSubmitted` and returns WITHOUT touching the
/// wire) — when a TRIGGER order requests a source whose row is [`TriggerByOutcome::Unsupported`].
/// `None` requests, non-trigger orders, and every other row pass through (`None` = submit
/// proceeds). The [`vike_model::venues::venue_tif::deny_unsupported_tif`] twin.
#[must_use]
pub fn deny_unsupported_trigger_by(venue: &str, request: &OrderRequest) -> Option<Event> {
    let source = request.trigger_by?;
    if !is_trigger_order(request)
        || venue_trigger_by(venue, source) != TriggerByOutcome::Unsupported
    {
        return None;
    }
    Some(Event::OrderRejected(OrderRejected {
        client_order_id: request.client_order_id.clone(),
        reason: format!(
            "trigger_by {source:?} is not supported on {venue} — order refused, never silently \
             coerced to a different trigger price series"
        )
        .into(),
        ts: request.ts,
    }))
}

#[cfg(test)]
mod tests {
    use super::TriggerByOutcome::{Ignored, Mapped, Unsupported, VenueLaw};
    use super::{TriggerByOutcome, deny_unsupported_trigger_by, venue_trigger_by};
    use vike_model::TriggerBy::{self, Index, Last, Mark};

    const ALL_SOURCES: [TriggerBy; 3] = [Last, Mark, Index];

    /// The CURRENT matrix, verbatim — any drift in a venue row is loud here.
    #[test]
    fn current_trigger_by_matrix_is_pinned() {
        #[rustfmt::skip]
        let rows: &[(&str, TriggerBy, TriggerByOutcome)] = &[
            // binance PERP lane: fapi workingType — no index series
            ("binance-perp", Last, Mapped("CONTRACT_PRICE")),
            ("binance-perp", Mark, Mapped("MARK_PRICE")),
            ("binance-perp", Index, Unsupported),
            // binance SPOT lane: no source axis; last IS the spot law
            ("binance", Last, VenueLaw),
            ("binance", Mark, Unsupported),
            ("binance", Index, Unsupported),
            // bybit: V5 triggerBy expresses all three
            ("bybit", Last, Mapped("LastPrice")),
            ("bybit", Mark, Mapped("MarkPrice")),
            ("bybit", Index, Mapped("IndexPrice")),
            // okx: algo slTriggerPxType expresses all three
            ("okx", Last, Mapped("last")),
            ("okx", Mark, Mapped("mark")),
            ("okx", Index, Mapped("index")),
            // hyperliquid: MARK by venue law, no field
            ("hyperliquid", Last, Unsupported),
            ("hyperliquid", Mark, VenueLaw),
            ("hyperliquid", Index, Unsupported),
            // aster: shares binance-perp's fapi fork and law — same Mapped/Mapped/Unsupported
            // trio, evidenced by aster's own API docs (see the module doc)
            ("aster", Last, Mapped("CONTRACT_PRICE")),
            ("aster", Mark, Mapped("MARK_PRICE")),
            ("aster", Index, Unsupported),
        ];
        for (venue, source, want) in rows {
            assert_eq!(venue_trigger_by(venue, *source), *want, "{venue}/{source:?}");
        }
        // completeness: every table venue is covered for all three sources
        for v in ["binance-perp", "binance", "bybit", "okx", "hyperliquid", "aster"] {
            assert_eq!(rows.iter().filter(|(rv, _, _)| rv == &v).count(), ALL_SOURCES.len(), "{v}");
        }
    }

    /// Venues without a modeled trigger-source axis (and unknown ids) fall through to `Ignored`.
    #[test]
    fn unmodeled_and_unknown_venues_are_ignored() {
        for v in ["deribit", "oanda", "polymarket", "ibkr", "no-such-venue"] {
            for s in ALL_SOURCES {
                assert_eq!(venue_trigger_by(v, s), Ignored, "{v}/{s:?}");
            }
        }
    }

    /// `wire()` — Some for Mapped only (the request source reaches the wire), None otherwise.
    #[test]
    fn wire_projection() {
        assert_eq!(Mapped("MarkPrice").wire(), Some("MarkPrice"));
        assert_eq!(VenueLaw.wire(), None);
        assert_eq!(Ignored.wire(), None);
        assert_eq!(Unsupported.wire(), None);
    }

    fn req(venue: &str, order_type: &str, tb: Option<TriggerBy>) -> vike_model::OrderRequest {
        vike_model::OrderRequest {
            client_order_id: "c-deny".to_string(),
            venue: venue.to_string(),
            symbol: "X".to_string(),
            side: -1,
            qty: 1.0,
            order_type: order_type.to_string(),
            trigger_price: if matches!(order_type, "stop" | "take_profit") {
                Some(95.0)
            } else {
                None
            },
            trigger_by: tb,
            ..Default::default()
        }
    }

    /// The deny gate fires ONLY on (trigger-order, Some(source), Unsupported-row) triples.
    #[test]
    fn deny_gate_fires_only_on_unsupported_trigger_rows() {
        // None NEVER denies anywhere — the byte-identical default
        for v in ["binance", "binance-perp", "bybit", "okx", "hyperliquid", "aster", "nope"] {
            assert!(deny_unsupported_trigger_by(v, &req(v, "stop", None)).is_none(), "{v}/None");
        }
        // supported/ignored rows pass
        assert!(
            deny_unsupported_trigger_by("binance-perp", &req("binance", "stop", Some(Mark)))
                .is_none()
        );
        assert!(deny_unsupported_trigger_by("bybit", &req("bybit", "stop", Some(Index))).is_none());
        assert!(deny_unsupported_trigger_by("okx", &req("okx", "stop", Some(Mark))).is_none());
        assert!(
            deny_unsupported_trigger_by("hyperliquid", &req("hyperliquid", "stop", Some(Mark)))
                .is_none(),
            "Mark IS hyperliquid's venue law"
        );
        assert!(
            deny_unsupported_trigger_by("aster", &req("aster", "stop", Some(Mark))).is_none(),
            "Mark maps onto MARK_PRICE, same law as binance-perp"
        );
        // unsupported rows DENY, terminally, naming the source and the venue
        for (v, s) in [
            ("binance-perp", Index),
            ("binance", Mark),
            ("binance", Index),
            ("hyperliquid", Last),
            ("hyperliquid", Index),
            ("aster", Index),
        ] {
            let ev = deny_unsupported_trigger_by(v, &req(v, "stop", Some(s)))
                .unwrap_or_else(|| panic!("{v}/{s:?} must deny"));
            match ev {
                vike_model::events::Event::OrderRejected(r) => {
                    assert_eq!(r.client_order_id, "c-deny");
                    assert!(r.reason.contains(&format!("{s:?}")), "{}", r.reason);
                    assert!(r.reason.contains(v), "{}", r.reason);
                }
                other => panic!("expected OrderRejected, got {other:?}"),
            }
        }
        // take_profit and bare-trigger_price shapes are trigger orders too
        assert!(
            deny_unsupported_trigger_by(
                "hyperliquid",
                &req("hyperliquid", "take_profit", Some(Last))
            )
            .is_some()
        );
        let mut bare = req("hyperliquid", "limit", Some(Last));
        bare.trigger_price = Some(95.0); // HL routes any trigger_price to its trigger path
        assert!(deny_unsupported_trigger_by("hyperliquid", &bare).is_some());
        // non-trigger orders never deny — the field is meaningless there
        assert!(
            deny_unsupported_trigger_by("hyperliquid", &req("hyperliquid", "limit", Some(Last)))
                .is_none()
        );
        assert!(
            deny_unsupported_trigger_by("binance", &req("binance", "market", Some(Mark))).is_none()
        );
    }
}
