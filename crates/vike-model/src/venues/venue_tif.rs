//! The ONE cross-venue TimeInForce authority: this table ENCODES what each venue adapter does
//! with `OrderRequest.time_in_force`, verbatim. STEP 1 pinned today's reality byte-identically;
//! STEP 2 flips venues one at a time to HONOR the requested TIF, each flip proven by a gated
//! demo smoke — binance, bybit, deribit and okx are flipped (their limit paths consume their
//! rows: GTC/IOC/FOK map 1:1, deribit additionally Day -> `good_til_day`, and okx's TIF rides
//! `ordType` — `ioc`/`fok` ARE ordTypes there, no separate field exists; an inexpressible TIF
//! is a LOUD terminal deny via [`deny_unsupported_tif`](crate::venues::venue_tif::deny_unsupported_tif), never a silent coercion). Still
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
//!
//! ## Where this lives and why
//!
//! This table lived in `vike-bridge-core` (as its `tif` module) until 2026-09-26 and moved DOWN
//! here, beside the other per-venue capability tables (`venue_caps`, `venue_margin_support`,
//! `fees`). Nothing in it ever needed that crate: it names this crate's types and nothing else, and
//! does no I/O. Its old home made a consumer that wants only the TABLE — the docs-data exporter,
//! which names nothing else from the bridge stack — rank above the transport layer just to read it.
//! One side effect is worth knowing: `venue_caps_cross_pin_the_tif_table` below now sits in the
//! same crate as BOTH tables it compares, so a caps row and a TIF row move under one `cargo test`.
//! Every consumer names it as `vike_model::venues::venue_tif::…`; nothing is re-exported at the crate root,
//! so each symbol has exactly one name.

use crate::events::{Event, OrderRejected};
use crate::orders::order::{OrderRequest, TimeInForce};

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

#[path = "venue_tif_tests.rs"]
#[cfg(test)]
mod venue_tif_tests;
