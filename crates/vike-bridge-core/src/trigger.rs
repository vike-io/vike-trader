//! The ONE cross-venue trigger-price-source authority (the [`crate::tif`] pattern applied to
//! `OrderRequest.trigger_by`): this table ENCODES what each venue adapter does with a requested
//! trigger source on its CONDITIONAL/trigger-order path, and [`deny_unsupported_trigger_by`] is
//! the shared submit-side gate for the inexpressible rows.
//!
//! `None` (no requested source) is NOT in this table by design: every adapter then produces
//! EXACTLY its pre-`trigger_by` wire bytes (binance: no `workingType` → venue default
//! `CONTRACT_PRICE`; bybit: the historical `triggerBy:"LastPrice"` hardcode; okx: no
//! `slTriggerPxType` → venue default `last`; hyperliquid: no field exists — MARK by venue law).
//! The table answers only "what happens when a source IS requested":
//!
//! - binance perp (the `"binance-perp"` lane sub-key, mirroring [`crate::tif`]'s lane split):
//!   fapi `workingType` expresses `CONTRACT_PRICE` (last) and `MARK_PRICE` only — Index is a
//!   LOUD deny. The spot lane (`"binance"`) has no trigger-source axis at all (spot stops
//!   evaluate on last trades, the only series spot has): Last matches that law, Mark/Index deny.
//! - bybit: V5 `triggerBy` expresses all three (LastPrice/MarkPrice/IndexPrice).
//! - okx: the algo-order `slTriggerPxType` expresses all three (`last`/`mark`/`index`).
//! - hyperliquid: NO trigger-source field exists and triggers evaluate against MARK by venue
//!   law — Mark is accepted as [`TriggerByOutcome::VenueLaw`] (nothing to emit), Last/Index deny.
//! - aster: shares binance's family builder (`family::order_map::build_perp_order_params`
//!   CONSUMES this row generically, keyed by the caller-supplied `venue` — no per-venue code
//!   change was needed to flip it), and unlike the TIF row (still `Ignored{GTC}` — no demo
//!   account to prove a TIF flip by smoke), this row IS mapped: Aster's own fapi-fork API
//!   documents `workingType` (MARK_PRICE/CONTRACT_PRICE) as a supported perp order param
//!   (`docs/superpowers/specs/2026-07-16-aster-dex-bridge-design.md` §"Orders & capabilities",
//!   sourced from Aster's API docs during the bridge build — not assumed from binance alone),
//!   and Aster's fapi is a byte-identical fork of binance's fapi order grammar. Same law as
//!   binance-perp: Last/Mark map onto `CONTRACT_PRICE`/`MARK_PRICE`, Index has no such
//!   workingType and is [`TriggerByOutcome::Unsupported`] — gated at submit
//!   (`deny_unsupported_trigger_by`, wired into `AsterPerpRest::submit_order`/`submit_batch`
//!   exactly as binance-perp's).
//! - okx: the row above maps all three sources, but ONLY for the stop-loss leg
//!   (`submit_stop_algo`/`build_stop_algo_params`, which alone emits `slTriggerPxType`). OKX has
//!   no separate take-profit routing at all today — `submit_order` diverts only
//!   `order_type.eq_ignore_ascii_case("stop")` to the algo endpoint; a `"take_profit"` request
//!   falls through to the plain order path (`build_order_params`, which doesn't recognize
//!   `"take_profit"` either — a pre-existing, tracked gap, not this table's concern:
//!   `docs/superpowers/specs/2026-07-18-order-emulator-design.md` §0.2 item 5, "no
//!   take-profit-market"). So a requested `trigger_by` on a would-be OKX take-profit has nowhere
//!   to ride the wire AND nothing here denies it (the row maps all three) — this is a real gap,
//!   but it's downstream of "no TP order type" rather than "TP drops the trigger source"; closing
//!   it means building OKX take-profit routing first, out of this table's scope.
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
/// exactly as in [`crate::tif::venue_tif`], because binance's spot and perp lanes genuinely
/// diverge (spot has no trigger-source axis; fapi has `workingType`).
#[must_use]
pub fn venue_trigger_by(venue: &str, source: TriggerBy) -> TriggerByOutcome {
    use TriggerBy::{Index, Last, Mark};
    match venue {
        // family/order_map.rs (`build_perp_order_params`, stop arm) consumes this row: fapi
        // `workingType` — CONTRACT_PRICE is the venue default (last), MARK_PRICE native; there
        // is no index workingType, so Index is denied at submit.
        "binance-perp" => match source {
            Last => TriggerByOutcome::Mapped("CONTRACT_PRICE"),
            Mark => TriggerByOutcome::Mapped("MARK_PRICE"),
            Index => TriggerByOutcome::Unsupported,
        },
        // spot has no trigger-source axis: stops (STOP_LOSS*) evaluate on last trades — the only
        // price series spot has. Last matches that law; Mark/Index are inexpressible.
        "binance" => match source {
            Last => TriggerByOutcome::VenueLaw,
            Mark | Index => TriggerByOutcome::Unsupported,
        },
        // perp.rs::build_order_params (stop arm) consumes this row — replaces the historical
        // `triggerBy:"LastPrice"` hardcode; a None request still emits LastPrice byte-identically.
        "bybit" => match source {
            Last => TriggerByOutcome::Mapped("LastPrice"),
            Mark => TriggerByOutcome::Mapped("MarkPrice"),
            Index => TriggerByOutcome::Mapped("IndexPrice"),
        },
        // perp.rs::submit_stop_algo consumes this row: the algo-order `slTriggerPxType`
        // (venue default when absent = last).
        "okx" => match source {
            Last => TriggerByOutcome::Mapped("last"),
            Mark => TriggerByOutcome::Mapped("mark"),
            Index => TriggerByOutcome::Mapped("index"),
        },
        // exec.rs::build_order_wire consumes this row: HL trigger orders carry no source field
        // and evaluate against MARK by venue law — Mark matches, Last/Index are inexpressible.
        "hyperliquid" => match source {
            Mark => TriggerByOutcome::VenueLaw,
            Last | Index => TriggerByOutcome::Unsupported,
        },
        // family/order_map.rs (`build_perp_order_params`, stop arm) consumes this row exactly as
        // binance-perp does — aster's fapi is a byte-identical fork of binance's fapi and its own
        // API docs list `workingType` (MARK_PRICE/CONTRACT_PRICE) as supported (module doc has the
        // citation): Last/Mark map, Index has no fapi index workingType and is denied at submit.
        "aster" => match source {
            Last => TriggerByOutcome::Mapped("CONTRACT_PRICE"),
            Mark => TriggerByOutcome::Mapped("MARK_PRICE"),
            Index => TriggerByOutcome::Unsupported,
        },
        // every other venue either has no trigger-order path or doesn't model a source axis yet —
        // Ignored fallthrough; a venue growing one MUST add its row here.
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
/// proceeds). The [`crate::tif::deny_unsupported_tif`] twin.
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
            // hyperliquid: MARK by venue law, no field — the venue the None-default divergence
            // was discovered on (last=99, mark=101, SL=100 fires only here)
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
