//! The ONE cross-venue TimeInForce authority: this table ENCODES what each venue adapter does
//! with `OrderRequest.time_in_force`, verbatim. STEP 1 pinned today's reality byte-identically;
//! STEP 2 flips venues one at a time to HONOR the requested TIF, each flip proven by a gated
//! demo smoke — binance, bybit, deribit and okx are flipped (their limit paths consume their
//! rows: GTC/IOC/FOK map 1:1, deribit additionally Day -> `good_til_day`, and okx's TIF rides
//! `ordType` — `ioc`/`fok` ARE ordTypes there, no separate field exists; an inexpressible TIF
//! is a LOUD terminal deny via [`deny_unsupported_tif`], never a silent coercion). Still
//! encoded as-is, unflipped: polymarket coerces `Ioc -> FOK` (upgrades to fill-or-kill: partial
//! fills FORBIDDEN) while hyperliquid coerces `Fok -> Ioc` (downgrades to immediate-or-cancel:
//! partial fills ALLOWED) — the SAME pair, folded in OPPOSITE directions; aster/ig still never
//! read the field (orders silently rest GTC). Binance is the one LANE-SPLIT venue: its perp
//! lane (`"binance-perp"` sub-key) additionally maps native fapi GTD while its spot lane
//! (`"binance"`) keeps the GTD deny — see the [`venue_tif`] doc.
//!
//! [`venue_tif`] describes the venue's RESTING-order path (limit / working order) — the only path
//! where a TIF axis exists at most venues. Known market-order divergences, documented here rather
//! than modeled (none of them read a *request* TIF except oanda):
//!
//! - binance/aster (shared family builder), bybit: NO TIF param on market/stop orders.
//! - ig: the MARKET body carries no `timeInForce` (only working orders do).
//! - hyperliquid: the emulated market order hardcodes `"Ioc"`; trigger orders carry no TIF.
//! - polymarket: one submit path for everything — its row applies to ALL orders.
//! - oanda: MARKET accepts only FOK/IOC, so `Ioc -> "IOC"` and EVERYTHING else coerces to
//!   `"FOK"` — a third coercion shape, live at `crates/bridges/oanda/src/exec.rs::oanda_tif`.
//!
//! Row ownership: every MAPPING venue CONSUMES its row — the venue fn is the table lookup:
//! polymarket (`client.rs::order_type_of`), hyperliquid (`exec.rs::hl_tif`), oanda
//! (`exec.rs::oanda_tif`, working-order arm; its MARKET arm stays local — the genuine venue
//! constraint above), alpaca (`event_mapper.rs::tif_str`), ibkr (`order.rs::tif_of`). No local
//! TIF mappers remain, so a future flip is a one-line row edit here. The Ignored/NotEmitted
//! venues keep their hardcode/omission sites untouched (zero wire risk) with doc-comment
//! references back here plus venue-side pins that a request TIF never changes their wire bytes.

use vike_model::events::{Event, OrderRejected};
use vike_model::{OrderRequest, TimeInForce};

/// What one venue adapter does with `OrderRequest.time_in_force` on its resting-order path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TifOutcome {
    /// The requested TIF goes on the wire 1:1 as this venue string.
    Mapped(&'static str),
    /// The venue adapter SUBSTITUTES a different TIF: the request's `from` is sent as `to`'s
    /// wire string. Every `Coerced` row is a silent semantic change step 2 must resolve.
    Coerced {
        /// The TIF the caller asked for.
        from: TimeInForce,
        /// The TIF actually sent instead.
        to: TimeInForce,
        /// `to`'s venue wire string.
        wire: &'static str,
    },
    /// The adapter never reads `request.time_in_force`; the wire carries this hardcoded value on
    /// the resting path regardless of what was requested (orders silently rest GTC today).
    Ignored {
        /// The hardcoded venue wire string the resting path always sends.
        wire: &'static str,
    },
    /// The adapter emits no TIF field at all — the venue's server-side default rules.
    NotEmitted,
    /// Step-2 flip outcome: the venue cannot express this TIF on its wire at all, and the
    /// adapter REFUSES the order at submit — a terminal `OrderRejected` (via
    /// [`deny_unsupported_tif`], pushed after the synchronous `OrderSubmitted` per the emitter
    /// split) instead of a silent coercion. The roster-gate philosophy: deny loudly, never
    /// quietly substitute a different lifetime.
    Unsupported,
}

impl TifOutcome {
    /// The wire string a MAPPING venue emits for the requested TIF (`Mapped`/`Coerced`); `None`
    /// when the request's TIF never reaches the wire (`Ignored` — the hardcoded default is sent
    /// instead — or `NotEmitted`).
    #[must_use]
    pub fn wire(self) -> Option<&'static str> {
        match self {
            TifOutcome::Mapped(w) | TifOutcome::Coerced { wire: w, .. } => Some(w),
            TifOutcome::Ignored { .. } | TifOutcome::NotEmitted | TifOutcome::Unsupported => None,
        }
    }
}

/// The submit-side TIF gate for a FLIPPED venue: `Some(OrderRejected)` — terminal, per the
/// emitter split (the adapter pushes it after the synchronous `OrderSubmitted` and returns
/// WITHOUT touching the wire) — when the request asks a TIF whose row is
/// [`TifOutcome::Unsupported`] on the path where a TIF axis exists (a resting `limit` order).
/// Market/stop paths carry no request TIF on any flipped venue (the module-doc divergences),
/// so they pass through untouched; every other row returns `None` (submit proceeds).
#[must_use]
pub fn deny_unsupported_tif(venue: &str, request: &OrderRequest) -> Option<Event> {
    if !request.order_type.eq_ignore_ascii_case("limit")
        || venue_tif(venue, request.time_in_force) != TifOutcome::Unsupported
    {
        return None;
    }
    Some(Event::OrderRejected(OrderRejected {
        client_order_id: request.client_order_id.clone(),
        reason: format!(
            "time_in_force {:?} is not supported on {venue} — order refused, never silently \
             coerced",
            request.time_in_force
        )
        .into(),
        ts: request.ts,
    }))
}

/// Today's per-venue TIF reality, one row per `(venue, tif)` — see the module doc for scope
/// (resting-order path) and the market-order divergences. `venue` is the canonical lowercase
/// venue id (each crate's `VENUE` const) — EXCEPT where one venue id fronts two order APIs whose
/// TIF vocabularies genuinely diverge: binance's spot `/api/v3` has NO good-till-date at all
/// while its USDⓈ-M `/fapi` has native GTD, so the binance family keys its perp lane with the
/// LANE sub-key `"binance-perp"` (`vike_binance::perp::TIF_LANE`) and the plain `"binance"` row
/// stays the spot lane. A lane sub-key is NOT a roster venue (the roster-completeness test below
/// classifies roster ids only); it exists solely so the ONE authority still owns both lanes'
/// rows. Venues whose protocol carries no TIF at all (ctrader/fxcm/dukascopy) — and any venue
/// not yet in the table — return [`TifOutcome::NotEmitted`]; a NEW venue that grows a TIF site
/// MUST add its row here.
#[must_use]
pub fn venue_tif(venue: &str, tif: TimeInForce) -> TifOutcome {
    use TimeInForce::{Day, Fok, Gtc, Gtd, Ioc};
    match venue {
        // client.rs::order_type_of — consumes this row. NOTE: Ioc is coerced UP to FOK
        // (partial fills forbidden), the OPPOSITE direction of hyperliquid's Fok->Ioc fold.
        // GTD emits `orderType: "GTD"` AND a real wire `expiration` (unix secs) computed from
        // `OrderRequest::gtd_expiry` by `client.rs::expiration_secs_of`, which refuses a missing
        // or too-near expiry locally. It stays out of the caps row's `supported_tifs` only
        // because that wire path is not yet live-proven — see the cross-pin test below.
        "polymarket" => match tif {
            Gtc => TifOutcome::Mapped("GTC"),
            Ioc => TifOutcome::Coerced { from: Ioc, to: Fok, wire: "FOK" },
            Fok => TifOutcome::Mapped("FOK"),
            Gtd => TifOutcome::Mapped("GTD"),
            Day => TifOutcome::Coerced { from: Day, to: Gtc, wire: "GTC" },
        },
        // exec.rs::hl_tif — consumes this row (plain-limit path; the emulated market hardcodes
        // "Ioc"). HL exposes only Gtc/Ioc/Alo; Alo (post-only) is not expressible via
        // OrderRequest, so Fok is coerced DOWN to Ioc (partial fills allowed) — the OPPOSITE
        // direction of polymarket's Ioc->FOK fold — and Gtd/Day fold to the resting Gtc.
        "hyperliquid" => match tif {
            Gtc => TifOutcome::Mapped("Gtc"),
            Ioc => TifOutcome::Mapped("Ioc"),
            Fok => TifOutcome::Coerced { from: Fok, to: Ioc, wire: "Ioc" },
            Gtd => TifOutcome::Coerced { from: Gtd, to: Gtc, wire: "Gtc" },
            Day => TifOutcome::Coerced { from: Day, to: Gtc, wire: "Gtc" },
        },
        // family/order_map.rs (`build_spot_order_params`, SPOT lane) — FLIPPED: the family
        // builder consumes this row on its LIMIT path, so binance spot honors GTC/IOC/FOK 1:1.
        // GTD/Day are denied loudly at submit ([`deny_unsupported_tif`]): the spot /api/v3 has
        // no GTD at all and no session-day TIF. Demo-smoke-proven (`binance_tif_smoke.rs`).
        "binance" => match tif {
            Gtc => TifOutcome::Mapped("GTC"),
            Ioc => TifOutcome::Mapped("IOC"),
            Fok => TifOutcome::Mapped("FOK"),
            Gtd | Day => TifOutcome::Unsupported,
        },
        // family/order_map.rs (`build_perp_order_params`, PERP lane — the `"binance-perp"` LANE
        // sub-key, `vike_binance::perp::TIF_LANE`; see the `venue` doc above): USDⓈ-M fapi has
        // NATIVE GTD (`timeInForce=GTD` + the venue-mandatory `goodTillDate` companion the
        // builder takes verbatim from `OrderRequest.gtd_expiry` — this static row cannot carry
        // the date, so its validity gate lives venue-side: `vike_binance::perp::deny_invalid_gtd`
        // enforces fapi's now+600s/253402300799000 bounds and NEVER invents a date). Day stays
        // Unsupported — fapi has no session-day TIF. Demo-smoke-proven (`binance_tif_smoke.rs`).
        "binance-perp" => match tif {
            Gtc => TifOutcome::Mapped("GTC"),
            Ioc => TifOutcome::Mapped("IOC"),
            Fok => TifOutcome::Mapped("FOK"),
            Gtd => TifOutcome::Mapped("GTD"),
            Day => TifOutcome::Unsupported,
        },
        // aster shares binance's family builder but stays UNFLIPPED (no demo account to prove a
        // flip by smoke): the builder consumes this Ignored row → `timeInForce` still hardcoded
        // "GTC" on LIMIT; the request TIF is never read.
        "aster" => TifOutcome::Ignored { wire: "GTC" },
        // perp.rs::build_order_params — FLIPPED: consumes this row on its Limit path, so bybit
        // honors GTC/IOC/FOK 1:1 (V5 native `timeInForce` values; PostOnly is not expressible
        // via OrderRequest). GTD/Day are denied loudly at submit ([`deny_unsupported_tif`]) —
        // V5 has no good-till-date/session-day TIF. Demo-smoke-proven (`bybit_tif_smoke.rs`).
        "bybit" => match tif {
            Gtc => TifOutcome::Mapped("GTC"),
            Ioc => TifOutcome::Mapped("IOC"),
            Fok => TifOutcome::Mapped("FOK"),
            Gtd | Day => TifOutcome::Unsupported,
        },
        // exec.rs::build_request: working orders hardcode "GOOD_TILL_CANCELLED"; never read.
        "ig" => TifOutcome::Ignored { wire: "GOOD_TILL_CANCELLED" },
        // perp.rs::build_order_params — FLIPPED: consumes this row on its limit path. OKX has
        // no TIF field at all — a TIF rides `ordType` (`ioc`/`fok` ARE ordTypes): Gtc stays
        // NotEmitted (`ordType:"limit"`, the venue default good-till-cancel rules —
        // byte-identical to pre-flip), Ioc/Fok map to the `ioc`/`fok` ordTypes (still limit
        // orders: px required and sent). Gtd/Day are denied loudly at submit
        // ([`deny_unsupported_tif`]) — V5 has no good-till-date/session-day ordType.
        // Demo-smoke-proven (`okx_tif_smoke.rs`).
        "okx" => match tif {
            Gtc => TifOutcome::NotEmitted,
            Ioc => TifOutcome::Mapped("ioc"),
            Fok => TifOutcome::Mapped("fok"),
            Gtd | Day => TifOutcome::Unsupported,
        },
        // client.rs::build_order_params — FLIPPED: consumes this row on its limit path. Gtc
        // stays NotEmitted (no `time_in_force` param — the venue default `good_til_cancelled`
        // rules; byte-identical to pre-flip). Ioc/Fok/Day map to Deribit's own TIF vocabulary;
        // Gtd is denied loudly at submit ([`deny_unsupported_tif`]) — Deribit has no
        // good-till-DATE, only good_til_day. Demo-smoke-proven (`deribit_tif_smoke.rs`).
        "deribit" => match tif {
            Gtc => TifOutcome::NotEmitted,
            Ioc => TifOutcome::Mapped("immediate_or_cancel"),
            Fok => TifOutcome::Mapped("fill_or_kill"),
            Day => TifOutcome::Mapped("good_til_day"),
            Gtd => TifOutcome::Unsupported,
        },
        // exec.rs::oanda_tif — consumes this row on its working-order arm (the MARKET arm stays
        // local: it coerces non-Ioc -> FOK, see the module doc). A genuine 1:1 mapper.
        "oanda" => match tif {
            Gtc => TifOutcome::Mapped("GTC"),
            Ioc => TifOutcome::Mapped("IOC"),
            Fok => TifOutcome::Mapped("FOK"),
            Gtd => TifOutcome::Mapped("GTD"),
            Day => TifOutcome::Mapped("GFD"),
        },
        // event_mapper.rs::tif_str — consumes this row. Lowercase wire; Alpaca has no GTD,
        // coerced to gtc.
        "alpaca" => match tif {
            Gtc => TifOutcome::Mapped("gtc"),
            Ioc => TifOutcome::Mapped("ioc"),
            Fok => TifOutcome::Mapped("fok"),
            Gtd => TifOutcome::Coerced { from: Gtd, to: Gtc, wire: "gtc" },
            Day => TifOutcome::Mapped("day"),
        },
        // order.rs::tif_of — consumes this row; all five TIF strings are native on the wire.
        // GTD is Mapped but a STUB: neither backend wires the required good-till date from
        // `gtd_expiry` (socket `build_order` keeps `Order::default`'s empty `good_till_date`;
        // the cpapi order body carries no date field) — TWS/CPAPI reject a dateless GTD order.
        "ibkr" => match tif {
            Gtc => TifOutcome::Mapped("GTC"),
            Ioc => TifOutcome::Mapped("IOC"),
            Fok => TifOutcome::Mapped("FOK"),
            Gtd => TifOutcome::Mapped("GTD"),
            Day => TifOutcome::Mapped("DAY"),
        },
        // ctrader/fxcm/dukascopy protocols carry no TIF; unknown venues land here too.
        _ => TifOutcome::NotEmitted,
    }
}

#[cfg(test)]
mod tests {
    use super::TifOutcome::{Coerced, Ignored, Mapped, NotEmitted};
    use super::{TifOutcome, venue_tif};
    use vike_model::TimeInForce::{self, Day, Fok, Gtc, Gtd, Ioc};

    const ALL_TIFS: [TimeInForce; 5] = [Gtc, Ioc, Fok, Gtd, Day];

    /// The CURRENT matrix, verbatim — any drift in a venue row is loud here. THE CONTRADICTION,
    /// pinned on purpose: polymarket's `Ioc -> FOK` row coerces UP (partial fills forbidden)
    /// while hyperliquid's `Fok -> Ioc` row coerces DOWN (partial fills allowed) — the same
    /// Ioc/Fok pair folded in OPPOSITE directions. Step 2 resolves that per venue behind demo
    /// smokes; until then this test IS the documentation of record.
    #[test]
    fn current_tif_matrix_is_pinned() {
        #[rustfmt::skip]
        let rows: &[(&str, TimeInForce, TifOutcome)] = &[
            // polymarket — maps/coerces every TIF; Ioc coerced UP to FOK (contradiction, side A)
            ("polymarket", Gtc, Mapped("GTC")),
            ("polymarket", Ioc, Coerced { from: Ioc, to: Fok, wire: "FOK" }),
            ("polymarket", Fok, Mapped("FOK")),
            ("polymarket", Gtd, Mapped("GTD")), // + a real wire `expiration`; not yet live-proven
            ("polymarket", Day, Coerced { from: Day, to: Gtc, wire: "GTC" }),
            // hyperliquid — Fok coerced DOWN to Ioc (contradiction, side B — same pair!)
            ("hyperliquid", Gtc, Mapped("Gtc")),
            ("hyperliquid", Ioc, Mapped("Ioc")),
            ("hyperliquid", Fok, Coerced { from: Fok, to: Ioc, wire: "Ioc" }),
            ("hyperliquid", Gtd, Coerced { from: Gtd, to: Gtc, wire: "Gtc" }),
            ("hyperliquid", Day, Coerced { from: Day, to: Gtc, wire: "Gtc" }),
            // binance SPOT lane — FLIPPED (smoke-proven): GTC/IOC/FOK map 1:1; GTD/Day loud-deny
            ("binance", Gtc, Mapped("GTC")),
            ("binance", Ioc, Mapped("IOC")),
            ("binance", Fok, Mapped("FOK")),
            ("binance", Gtd, TifOutcome::Unsupported),
            ("binance", Day, TifOutcome::Unsupported),
            // binance PERP lane (the "binance-perp" LANE sub-key — not a roster venue): native
            // fapi GTD is Mapped (goodTillDate rides from `gtd_expiry`, validity gated
            // venue-side); Day stays loud-denied. Smoke-proven (`binance_tif_smoke.rs`).
            ("binance-perp", Gtc, Mapped("GTC")),
            ("binance-perp", Ioc, Mapped("IOC")),
            ("binance-perp", Fok, Mapped("FOK")),
            ("binance-perp", Gtd, Mapped("GTD")),
            ("binance-perp", Day, TifOutcome::Unsupported),
            // the ignores — request TIF never read; LIMIT orders silently rest GTC
            ("aster", Gtc, Ignored { wire: "GTC" }), // shares binance's family builder, unflipped
            ("aster", Ioc, Ignored { wire: "GTC" }),
            ("aster", Fok, Ignored { wire: "GTC" }),
            ("aster", Gtd, Ignored { wire: "GTC" }),
            ("aster", Day, Ignored { wire: "GTC" }),
            // bybit — FLIPPED (smoke-proven): GTC/IOC/FOK map 1:1; GTD/Day loud-deny
            ("bybit", Gtc, Mapped("GTC")),
            ("bybit", Ioc, Mapped("IOC")),
            ("bybit", Fok, Mapped("FOK")),
            ("bybit", Gtd, TifOutcome::Unsupported),
            ("bybit", Day, TifOutcome::Unsupported),
            ("ig", Gtc, Ignored { wire: "GOOD_TILL_CANCELLED" }),
            ("ig", Ioc, Ignored { wire: "GOOD_TILL_CANCELLED" }),
            ("ig", Fok, Ignored { wire: "GOOD_TILL_CANCELLED" }),
            ("ig", Gtd, Ignored { wire: "GOOD_TILL_CANCELLED" }),
            ("ig", Day, Ignored { wire: "GOOD_TILL_CANCELLED" }),
            // okx — FLIPPED (smoke-proven): TIF rides ordType (no separate field) — Gtc stays
            // NotEmitted (ordType "limit", byte-identical); Ioc/Fok map to the ioc/fok
            // ordTypes; Gtd/Day loud-deny
            ("okx", Gtc, NotEmitted),
            ("okx", Ioc, Mapped("ioc")),
            ("okx", Fok, Mapped("fok")),
            ("okx", Gtd, TifOutcome::Unsupported),
            ("okx", Day, TifOutcome::Unsupported),
            // deribit — FLIPPED (smoke-proven): Gtc stays NotEmitted (venue default,
            // byte-identical); Ioc/Fok/Day map to Deribit's TIF vocabulary; Gtd loud-deny
            ("deribit", Gtc, NotEmitted),
            ("deribit", Ioc, Mapped("immediate_or_cancel")),
            ("deribit", Fok, Mapped("fill_or_kill")),
            ("deribit", Gtd, TifOutcome::Unsupported),
            ("deribit", Day, Mapped("good_til_day")),
            // the genuine mappers — rows consumed by their venue fns (oanda: working arm only)
            ("oanda", Gtc, Mapped("GTC")),
            ("oanda", Ioc, Mapped("IOC")),
            ("oanda", Fok, Mapped("FOK")),
            ("oanda", Gtd, Mapped("GTD")),
            ("oanda", Day, Mapped("GFD")),
            ("alpaca", Gtc, Mapped("gtc")),
            ("alpaca", Ioc, Mapped("ioc")),
            ("alpaca", Fok, Mapped("fok")),
            ("alpaca", Gtd, Coerced { from: Gtd, to: Gtc, wire: "gtc" }),
            ("alpaca", Day, Mapped("day")),
            ("ibkr", Gtc, Mapped("GTC")),
            ("ibkr", Ioc, Mapped("IOC")),
            ("ibkr", Fok, Mapped("FOK")),
            ("ibkr", Gtd, Mapped("GTD")), // stub: no backend wires the good-till date (gtd_expiry)
            ("ibkr", Day, Mapped("DAY")),
        ];
        for (venue, tif, want) in rows {
            assert_eq!(venue_tif(venue, *tif), *want, "{venue}/{tif:?}");
        }
        // completeness: every table venue (incl. the binance-perp lane sub-key) is covered for
        // all five TIFs (5 rows each)
        let venues = [
            "polymarket",
            "hyperliquid",
            "binance",
            "binance-perp",
            "aster",
            "bybit",
            "ig",
            "okx",
            "deribit",
            "oanda",
            "alpaca",
            "ibkr",
        ];
        for v in venues {
            assert_eq!(rows.iter().filter(|(rv, _, _)| rv == &v).count(), ALL_TIFS.len(), "{v}");
        }
        assert_eq!(rows.len(), venues.len() * ALL_TIFS.len());
    }

    /// The Ioc/Fok contradiction, spelled out as its own gate so it cannot be "fixed" silently:
    /// resolving EITHER side is a step-2 wire change that must flip this test deliberately.
    #[test]
    fn polymarket_and_hyperliquid_coerce_the_same_pair_in_opposite_directions() {
        assert_eq!(venue_tif("polymarket", Ioc), Coerced { from: Ioc, to: Fok, wire: "FOK" });
        assert_eq!(venue_tif("hyperliquid", Fok), Coerced { from: Fok, to: Ioc, wire: "Ioc" });
    }

    /// Completeness vs the canonical roster (`vike_model::VENUES`): every roster venue is
    /// classified exactly once — either it has an explicit `venue_tif` arm (pinned verbatim in
    /// `current_tif_matrix_is_pinned` above) or it is in the DECLARED tif-less set (its protocol
    /// carries no TIF, so it deliberately rides the `NotEmitted` fallthrough). Adding a venue to
    /// the roster fails here until it is classified one way or the other.
    #[test]
    fn every_roster_venue_is_classified() {
        // ROSTER venues with an explicit arm in `venue_tif` (the pinned matrix additionally
        // covers the non-roster "binance-perp" LANE sub-key — lane keys are not classified
        // here because they are not roster ids)
        const MAPPED: &[&str] = &[
            "polymarket",
            "hyperliquid",
            "binance",
            "aster",
            "bybit",
            "ig",
            "okx",
            "deribit",
            "oanda",
            "alpaca",
            "ibkr",
        ];
        // venues whose protocol carries no TIF — the declared `NotEmitted` fallthrough set
        #[rustfmt::skip]
        const TIFLESS: &[&str] = &[
            "ctrader", "fxcm", "dukascopy",
            // vike:new-venue:row "{venue}", // TODO(new-venue: {venue}): if the protocol DOES carry a TIF, delete this and add a
            // vike:new-venue:row // `venue_tif` arm + five `current_tif_matrix_is_pinned` rows instead. Leaving it here
            // vike:new-venue:row // declares `NotEmitted` for all five TIFs, which is what `venue_caps`' scaffolded
            // vike:new-venue:row // `supported_tifs`/`accepted_tifs` of `&[Gtc]` cross-pins against.
        ];
        assert_eq!(
            MAPPED.len() + TIFLESS.len(),
            vike_model::VENUES.len(),
            "every roster venue classified exactly once"
        );
        for &v in vike_model::VENUES {
            let mapped = MAPPED.contains(&v);
            let tifless = TIFLESS.contains(&v);
            assert!(
                mapped ^ tifless,
                "roster venue {v} must be classified exactly once (explicit venue_tif arm or \
                 declared tif-less)"
            );
            if tifless {
                for t in ALL_TIFS {
                    assert_eq!(venue_tif(v, t), NotEmitted, "{v}/{t:?} declared tif-less");
                }
            }
        }
    }

    /// CROSS-PIN with the `vike_model::venue_caps` registry (w2-task-5): the TWO TIF authorities
    /// — this table (what each adapter DOES per TIF) and each venue's `VenueCaps` row (what a
    /// caller may offer/submit) — are derived views of the same reality, so they must never
    /// drift. Per roster venue, for every TIF:
    ///
    /// - `supported_tifs` (the HONORED set) == `Mapped` rows plus the venue-default `Gtc` of an
    ///   `Ignored`/`NotEmitted` row (a GTC request matches what actually rests). Binance is the
    ///   lane INTERSECTION (spot ∧ perp — the single-keyed registry must not offer perp-only
    ///   GTD); polymarket's `Gtd` is the ONE declared exception (Mapped here, and it now DOES
    ///   emit a real wire `expiration`, but that path has never been exercised against the live
    ///   CLOB — so the caps row still refuses to ADVERTISE it on a mainnet money venue).
    /// - `accepted_tifs` (the core-preflight ADMIT set) == `Mapped` ∪ `Coerced` rows plus the
    ///   same default-Gtc rule; binance is the lane UNION (perp GTD must not be core-refused).
    ///   No stub exception: a stub TIF is still ACCEPTED today, and the preflight refuses
    ///   nothing a venue accepts.
    #[test]
    fn venue_caps_cross_pin_the_tif_table() {
        let honored = |venue: &str, t: TimeInForce| match venue_tif(venue, t) {
            TifOutcome::Mapped(_) => true,
            TifOutcome::Ignored { .. } | TifOutcome::NotEmitted => t == Gtc,
            TifOutcome::Coerced { .. } | TifOutcome::Unsupported => false,
        };
        let accepted = |venue: &str, t: TimeInForce| match venue_tif(venue, t) {
            TifOutcome::Mapped(_) | TifOutcome::Coerced { .. } => true,
            TifOutcome::Ignored { .. } | TifOutcome::NotEmitted => t == Gtc,
            TifOutcome::Unsupported => false,
        };
        for &v in vike_model::VENUES {
            let caps = vike_model::caps_for(v);
            for t in ALL_TIFS {
                let want_supported = if v == "binance" {
                    honored("binance", t) && honored("binance-perp", t)
                } else {
                    honored(v, t) && !(v == "polymarket" && t == Gtd) // declared: not live-proven
                };
                assert_eq!(
                    caps.supported_tifs.contains(&t),
                    want_supported,
                    "{v}/{t:?}: supported_tifs drifted from the venue_tif table"
                );
                let want_accepted = if v == "binance" {
                    accepted("binance", t) || accepted("binance-perp", t)
                } else {
                    accepted(v, t)
                };
                assert_eq!(
                    caps.accepted_tifs.contains(&t),
                    want_accepted,
                    "{v}/{t:?}: accepted_tifs drifted from the venue_tif table"
                );
            }
        }
    }

    /// TIF-less protocols and unknown venues fall through to `NotEmitted`.
    #[test]
    fn tifless_and_unknown_venues_are_not_emitted() {
        for v in ["ctrader", "fxcm", "dukascopy", "no-such-venue"] {
            for t in ALL_TIFS {
                assert_eq!(venue_tif(v, t), NotEmitted, "{v}/{t:?}");
            }
        }
    }

    /// `wire()` — Some for Mapped/Coerced (the request TIF reaches the wire), None otherwise.
    #[test]
    fn wire_projection() {
        assert_eq!(Mapped("GTC").wire(), Some("GTC"));
        assert_eq!(Coerced { from: Ioc, to: Fok, wire: "FOK" }.wire(), Some("FOK"));
        assert_eq!(Ignored { wire: "GTC" }.wire(), None);
        assert_eq!(NotEmitted.wire(), None);
        assert_eq!(TifOutcome::Unsupported.wire(), None);
    }

    /// The submit-side deny gate fires ONLY on `(limit, Unsupported-row)` pairs: every
    /// non-Unsupported row passes through (`None`), and non-limit order types never deny (their
    /// path carries no TIF axis). The per-venue `Some` cases are pinned in each flipped venue's
    /// own tests next to its adapter.
    #[test]
    fn deny_gate_passes_supported_rows_and_non_limit_paths() {
        let req = |venue: &str, order_type: &str, tif: TimeInForce| vike_model::OrderRequest {
            client_order_id: "c-deny".to_string(),
            venue: venue.to_string(),
            symbol: "X".to_string(),
            side: 1,
            qty: 1.0,
            order_type: order_type.to_string(),
            price: Some(1.0),
            time_in_force: tif,
            ..Default::default()
        };
        for v in ["polymarket", "hyperliquid", "oanda", "alpaca", "ibkr", "ig", "no-such-venue"] {
            for t in ALL_TIFS {
                assert!(
                    super::deny_unsupported_tif(v, &req(v, "limit", t)).is_none(),
                    "{v}/{t:?} has no Unsupported row — must pass"
                );
            }
        }
        // non-limit paths never deny, even where the venue's limit row is Unsupported
        for v in ["binance", "bybit", "deribit", "okx", "binance-perp"] {
            for ot in ["market", "stop"] {
                assert!(super::deny_unsupported_tif(v, &req(v, ot, Gtd)).is_none(), "{v}/{ot}");
            }
        }
        // the binance lane split: the perp lane's Gtd row is Mapped so the Unsupported gate
        // passes it (date VALIDITY is gated venue-side, `vike_binance::perp::deny_invalid_gtd`);
        // its Day row — and the spot lane's Gtd — still deny.
        assert!(
            super::deny_unsupported_tif("binance-perp", &req("binance", "limit", Gtd)).is_none()
        );
        assert!(
            super::deny_unsupported_tif("binance-perp", &req("binance", "limit", Day)).is_some()
        );
        assert!(super::deny_unsupported_tif("binance", &req("binance", "limit", Gtd)).is_some());
    }
}
