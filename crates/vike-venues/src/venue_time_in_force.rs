//! The ONE cross-venue TimeInForce authority: this table ENCODES what each venue adapter does
//! with `OrderRequest.time_in_force`, verbatim. STEP 1 pinned today's reality byte-identically;
//! STEP 2 flips venues one at a time to HONOR the requested TIF, each flip proven by a gated
//! demo smoke — binance, bybit, deribit and okx are flipped (their limit paths consume their
//! rows: GTC/IOC/FOK map 1:1, deribit additionally Day -> `good_til_day`, and okx's TIF rides
//! `ordType` — `ioc`/`fok` ARE ordTypes there, no separate field exists; an inexpressible TIF
//! is a LOUD terminal deny via [`deny_unsupported_time_in_force`](crate::venue_time_in_force::deny_unsupported_time_in_force), never a silent coercion). Still
//! encoded as-is, unflipped: polymarket coerces `Ioc -> FOK` (upgrades to fill-or-kill: partial
//! fills FORBIDDEN) while hyperliquid coerces `Fok -> Ioc` (downgrades to immediate-or-cancel:
//! partial fills ALLOWED) — the SAME pair, folded in OPPOSITE directions; aster/ig still never
//! read the field (orders silently rest GTC). Binance is the one LANE-SPLIT venue: its perp
//! lane (`"binance-perp"` sub-key) additionally maps native fapi GTD while its spot lane
//! (`"binance"`) keeps the GTD deny — see the [`venue_time_in_force`] doc.
//!
//! [`venue_time_in_force`] describes the venue's RESTING-order path (limit / working order) — the only path
//! where a TIF axis exists at most venues. Known market-order divergences, documented here rather
//! than modeled (none of them read a *request* TIF except oanda):
//!
//! - binance/aster (shared family builder), bybit: NO TIF param on market/stop orders.
//! - ig: the MARKET body carries no `timeInForce` (only working orders do).
//! - hyperliquid: the emulated market order hardcodes `"Ioc"`; trigger orders carry no TIF.
//! - polymarket: one submit path for everything — its row applies to ALL orders.
//! - oanda: MARKET accepts only FOK/IOC, so `Ioc -> "IOC"` and EVERYTHING else coerces to
//!   `"FOK"` — a third coercion shape, live at `crates/bridges/oanda/src/execution/mod.rs::oanda_time_in_force`.
//!
//! Row ownership: every MAPPING venue CONSUMES its row — the venue fn is the table lookup:
//! polymarket (`client.rs::order_type_of`), hyperliquid (`exec.rs::hl_time_in_force`), oanda
//! (`exec.rs::oanda_time_in_force`, working-order arm; its MARKET arm stays local — the genuine venue
//! constraint above), alpaca (`event_mapper.rs::time_in_force_str`), ibkr (`order.rs::time_in_force_of`). No local
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
//! One side effect is worth knowing: `venue_capabilities_cross_pin_the_time_in_force_table` below now sits in the
//! same crate as BOTH tables it compares, so a caps row and a TIF row move under one `cargo test`.
//! Every consumer names it as `vike_venues::venue_time_in_force::…`; nothing is re-exported at the crate root,
//! so each symbol has exactly one name.

use vike_model::events::{Event, OrderRejected};
use vike_model::orders::order::{OrderRequest, OrderType, TimeInForce};

/// What one venue adapter does with `OrderRequest.time_in_force` on its resting-order path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeInForceOutcome {
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
    /// [`deny_unsupported_time_in_force`], pushed after the synchronous `OrderSubmitted` per the emitter
    /// split) instead of a silent coercion. The roster-gate philosophy: deny loudly, never
    /// quietly substitute a different lifetime.
    Unsupported,
}

impl TimeInForceOutcome {
    /// The wire string a MAPPING venue emits for the requested TIF (`Mapped`/`Coerced`); `None`
    /// when the request's TIF never reaches the wire (`Ignored` — the hardcoded default is sent
    /// instead — or `NotEmitted`).
    #[must_use]
    pub fn wire(self) -> Option<&'static str> {
        match self {
            TimeInForceOutcome::Mapped(w) | TimeInForceOutcome::Coerced { wire: w, .. } => Some(w),
            TimeInForceOutcome::Ignored { .. }
            | TimeInForceOutcome::NotEmitted
            | TimeInForceOutcome::Unsupported => None,
        }
    }

    /// The wire string a builder that always WRITES a TIF field on the resting path puts there:
    /// [`TimeInForceOutcome::wire`] plus an `Ignored` row's hardcoded word (the field is sent, only the
    /// request's TIF is not read). `None` when no TIF field goes out (`NotEmitted`, `Unsupported`).
    #[must_use]
    pub fn sent(self) -> Option<&'static str> {
        match self {
            TimeInForceOutcome::Mapped(w)
            | TimeInForceOutcome::Coerced { wire: w, .. }
            | TimeInForceOutcome::Ignored { wire: w } => Some(w),
            TimeInForceOutcome::NotEmitted | TimeInForceOutcome::Unsupported => None,
        }
    }
}

/// The submit-side TIF gate for a FLIPPED venue: `Some(OrderRejected)` — terminal, per the
/// emitter split (the adapter pushes it after the synchronous `OrderSubmitted` and returns
/// WITHOUT touching the wire) — when the request asks a TIF whose row is
/// [`TimeInForceOutcome::Unsupported`] on the path where a TIF axis exists (a resting `limit` order).
/// Market/stop paths carry no request TIF on any flipped venue (the module-doc divergences),
/// so they pass through untouched; every other row returns `None` (submit proceeds).
///
/// The exceptions are two. `PostOnly`: a post-only order that is not a LIMIT order (a market order
/// has no resting price to protect, a stop fires as a taker) is refused on every path, because the
/// flag would otherwise be dropped silently and the order would take liquidity. And an AUCTION
/// lifetime on a MARKET order (or a market-to-limit, IB's `AUC` at the open) is judged against the
/// row as a limit order's is ([`TimeInForce::is_auction`]): a market-on-open a venue cannot express
/// would execute NOW. The same path as `crate::venue_caps::preflight_order_at`'s TIF axis.
#[must_use]
pub fn deny_unsupported_time_in_force(venue: &str, request: &OrderRequest) -> Option<Event> {
    let post_only_off_limit =
        request.time_in_force == TimeInForce::PostOnly && request.order_type != OrderType::Limit;
    let time_in_force_path = request.order_type == OrderType::Limit
        || (matches!(request.order_type, OrderType::Market | OrderType::MarketToLimit)
            && request.time_in_force.is_auction());
    if !post_only_off_limit
        && (!time_in_force_path
            || venue_time_in_force(venue, request.time_in_force) != TimeInForceOutcome::Unsupported)
    {
        return None;
    }
    let reason = if post_only_off_limit {
        format!(
            "time_in_force PostOnly needs a limit order on {venue} (a {} order would take \
             liquidity) — order refused, never silently sent as a taker",
            request.order_type
        )
    } else {
        format!(
            "time_in_force {:?} is not supported on {venue} — order refused, never silently \
             coerced",
            request.time_in_force
        )
    };
    Some(Event::OrderRejected(OrderRejected {
        cause: None,
        order_client_id: request.order_client_id.clone(),
        reason: reason.into(),
        timestamp: request.timestamp,
    }))
}

/// Today's per-venue TIF reality, one row per `(venue, tif)` — see the module doc for scope
/// (resting-order path) and the market-order divergences. `venue` is the canonical lowercase
/// venue id (each crate's `VENUE` const) — EXCEPT where one venue id fronts two order APIs whose
/// TIF vocabularies genuinely diverge: binance's spot `/api/v3` has NO good-till-date at all
/// while its USDⓈ-M `/fapi` has native GTD, so the binance family keys its perp lane with the
/// LANE sub-key `"binance-perp"` (`vike_binance::execution::rest::perp::TIME_IN_FORCE_LANE`) and the plain `"binance"` row
/// stays the spot lane. A lane sub-key is NOT a roster venue (the roster-completeness test below
/// classifies roster ids only); it exists solely so the ONE authority still owns both lanes'
/// rows. Venues whose protocol carries no TIF at all (ctrader/fxcm/dukascopy) — and any venue
/// not yet in the table — return [`TimeInForceOutcome::NotEmitted`]; a NEW venue that grows a TIF site
/// MUST add its row here.
#[must_use]
pub fn venue_time_in_force(venue: &str, tif: TimeInForce) -> TimeInForceOutcome {
    use TimeInForce::{AtClose, AtOpen, Day, Fok, Gtc, Gtd, Ioc, PostOnly};
    match venue {
        // client.rs::order_type_of — consumes this row. NOTE: Ioc is coerced UP to FOK
        // (partial fills forbidden), the OPPOSITE direction of hyperliquid's Fok->Ioc fold.
        // GTD emits `orderType: "GTD"` AND a real wire `expiration` (unix secs) computed from
        // `OrderRequest::gtd_expiry` by `client.rs::expiration_secs_of`, which refuses a missing
        // or too-near expiry locally. It stays out of the caps row's `supported_tifs` only
        // because that wire path is not yet live-proven — see the cross-pin test below.
        "polymarket" => match tif {
            Gtc => TimeInForceOutcome::Mapped("GTC"),
            Ioc => TimeInForceOutcome::Coerced { from: Ioc, to: Fok, wire: "FOK" },
            Fok => TimeInForceOutcome::Mapped("FOK"),
            Gtd => TimeInForceOutcome::Mapped("GTD"),
            Day => TimeInForceOutcome::Coerced { from: Day, to: Gtc, wire: "GTC" },
            // The CLOB has a post-only flag, but no order has ever been placed on this venue from
            // a build carrying it (a live money venue: NO orders in any proof), so it is not
            // advertised: refused by name until a mainnet-free proof exists.
            PostOnly => TimeInForceOutcome::Unsupported,
            // a prediction market has no auction
            AtOpen | AtClose => TimeInForceOutcome::Unsupported,
        },
        // exec.rs::hl_time_in_force — consumes this row (plain-limit path; the emulated market hardcodes
        // "Ioc"). HL exposes only Gtc/Ioc/Alo, so Fok is coerced DOWN to Ioc (partial fills
        // allowed) — the OPPOSITE direction of polymarket's Ioc->FOK fold — and Gtd/Day fold to
        // the resting Gtc. `Alo` (add liquidity only) IS post-only: a crossing `Alo` order is
        // refused by the venue, never filled as a taker.
        "hyperliquid" => match tif {
            Gtc => TimeInForceOutcome::Mapped("Gtc"),
            Ioc => TimeInForceOutcome::Mapped("Ioc"),
            Fok => TimeInForceOutcome::Coerced { from: Fok, to: Ioc, wire: "Ioc" },
            Gtd => TimeInForceOutcome::Coerced { from: Gtd, to: Gtc, wire: "Gtc" },
            Day => TimeInForceOutcome::Coerced { from: Day, to: Gtc, wire: "Gtc" },
            PostOnly => TimeInForceOutcome::Mapped("Alo"),
            // a 24/7 venue has no session to open or close
            AtOpen | AtClose => TimeInForceOutcome::Unsupported,
        },
        // family/order_map.rs (`build_spot_order_params`, SPOT lane) — FLIPPED: the family
        // builder consumes this row on its LIMIT path, so binance spot honors GTC/IOC/FOK 1:1.
        // GTD/Day are denied loudly at submit ([`deny_unsupported_time_in_force`]): the spot /api/v3 has
        // no GTD at all and no session-day TIF. Demo-smoke-proven (`binance_tif_smoke.rs`).
        // POST-ONLY on spot is not a TIF at all: it is the order TYPE `LIMIT_MAKER` (a crossing
        // one is refused `-2010 "Order would immediately match and take."`), and the venue
        // refuses a `timeInForce` beside it. The row's wire string names the type;
        // `crates/bridges/binance/src/execution/rest/spot.rs`'s `build_order_params` swaps the type and drops
        // the family builder's `timeInForce` for it.
        "binance" => match tif {
            Gtc => TimeInForceOutcome::Mapped("GTC"),
            Ioc => TimeInForceOutcome::Mapped("IOC"),
            Fok => TimeInForceOutcome::Mapped("FOK"),
            Gtd | Day => TimeInForceOutcome::Unsupported,
            PostOnly => TimeInForceOutcome::Mapped("LIMIT_MAKER"),
            // 24/7: no opening or closing auction
            AtOpen | AtClose => TimeInForceOutcome::Unsupported,
        },
        // family/order_map.rs (`build_perp_order_params`, PERP lane — the `"binance-perp"` LANE
        // sub-key, `vike_binance::execution::rest::perp::TIME_IN_FORCE_LANE`; see the `venue` doc above): USDⓈ-M fapi has
        // NATIVE GTD (`timeInForce=GTD` + the venue-mandatory `goodTillDate` companion the
        // builder takes verbatim from `OrderRequest.gtd_expiry` — this static row cannot carry
        // the date, so its validity gate lives venue-side: `vike_binance::execution::rest::perp::deny_invalid_gtd`
        // enforces fapi's now+600s/253402300799000 bounds and NEVER invents a date). Day stays
        // Unsupported — fapi has no session-day TIF. Demo-smoke-proven (`binance_tif_smoke.rs`).
        // POST-ONLY is the native TIF `GTX` (good till crossing): a crossing one is refused
        // `-5022` and never recorded.
        "binance-perp" => match tif {
            Gtc => TimeInForceOutcome::Mapped("GTC"),
            Ioc => TimeInForceOutcome::Mapped("IOC"),
            Fok => TimeInForceOutcome::Mapped("FOK"),
            Gtd => TimeInForceOutcome::Mapped("GTD"),
            Day => TimeInForceOutcome::Unsupported,
            PostOnly => TimeInForceOutcome::Mapped("GTX"),
            AtOpen | AtClose => TimeInForceOutcome::Unsupported,
        },
        // aster shares binance's family builder but stays UNFLIPPED (no demo account to prove a
        // flip by smoke): the builder consumes this Ignored row → `timeInForce` still hardcoded
        // "GTC" on LIMIT; the request TIF is never read. That includes `PostOnly`: the row
        // (`accepted_tifs: &[Gtc]`) makes the core preflight refuse it by name, so it never
        // rests as a taker.
        "aster" => TimeInForceOutcome::Ignored { wire: "GTC" },
        // perp.rs::build_order_params — FLIPPED: consumes this row on its Limit path, so bybit
        // honors GTC/IOC/FOK 1:1 (V5 native `timeInForce` values; PostOnly is not expressible
        // via OrderRequest). GTD/Day are denied loudly at submit ([`deny_unsupported_time_in_force`]) —
        // V5 has no good-till-date/session-day TIF. Demo-smoke-proven (`bybit_tif_smoke.rs`).
        // POST-ONLY is V5's own `timeInForce=PostOnly`: a crossing one is created and cancelled
        // by the venue (`rejectReason=EC_PostOnlyWillTakeLiquidity`).
        "bybit" => match tif {
            Gtc => TimeInForceOutcome::Mapped("GTC"),
            Ioc => TimeInForceOutcome::Mapped("IOC"),
            Fok => TimeInForceOutcome::Mapped("FOK"),
            Gtd | Day => TimeInForceOutcome::Unsupported,
            PostOnly => TimeInForceOutcome::Mapped("PostOnly"),
            AtOpen | AtClose => TimeInForceOutcome::Unsupported,
        },
        // exec.rs::build_request: working orders hardcode "GOOD_TILL_CANCELLED"; never read.
        "ig" => TimeInForceOutcome::Ignored { wire: "GOOD_TILL_CANCELLED" },
        // perp.rs::build_order_params — FLIPPED: consumes this row on its limit path. OKX has
        // no TIF field at all — a TIF rides `ordType` (`ioc`/`fok` ARE ordTypes): Gtc stays
        // NotEmitted (`ordType:"limit"`, the venue default good-till-cancel rules —
        // byte-identical to pre-flip), Ioc/Fok map to the `ioc`/`fok` ordTypes (still limit
        // orders: px required and sent). Gtd/Day are denied loudly at submit
        // ([`deny_unsupported_time_in_force`]) — V5 has no good-till-date/session-day ordType.
        // Demo-smoke-proven (`okx_time_in_force_smoke.rs`). POST-ONLY is the ordType `post_only` (still a
        // limit order: px required and sent).
        "okx" => match tif {
            Gtc => TimeInForceOutcome::NotEmitted,
            Ioc => TimeInForceOutcome::Mapped("ioc"),
            Fok => TimeInForceOutcome::Mapped("fok"),
            Gtd | Day => TimeInForceOutcome::Unsupported,
            PostOnly => TimeInForceOutcome::Mapped("post_only"),
            AtOpen | AtClose => TimeInForceOutcome::Unsupported,
        },
        // client.rs::build_order_params — FLIPPED: consumes this row on its limit path. Gtc
        // stays NotEmitted (no `time_in_force` param — the venue default `good_til_cancelled`
        // rules; byte-identical to pre-flip). Ioc/Fok/Day map to Deribit's own TIF vocabulary;
        // Gtd is denied loudly at submit ([`deny_unsupported_time_in_force`]) — Deribit has no
        // good-till-DATE, only good_til_day. Demo-smoke-proven (`deribit_tif_smoke.rs`).
        // POST-ONLY is NOT a `time_in_force` there but the flag pair `post_only` + `reject_post_only`
        // (the second makes a crossing order REFUSED; without it the venue silently re-prices it to
        // the best bid/ask): the row's wire string names the flag, and `client.rs`'s
        // `build_order_params` writes the pair and no `time_in_force`.
        "deribit" => match tif {
            Gtc => TimeInForceOutcome::NotEmitted,
            Ioc => TimeInForceOutcome::Mapped("immediate_or_cancel"),
            Fok => TimeInForceOutcome::Mapped("fill_or_kill"),
            Day => TimeInForceOutcome::Mapped("good_til_day"),
            Gtd => TimeInForceOutcome::Unsupported,
            PostOnly => TimeInForceOutcome::Mapped("post_only"),
            AtOpen | AtClose => TimeInForceOutcome::Unsupported,
        },
        // exec.rs::oanda_time_in_force — consumes this row on its working-order arm (the MARKET arm stays
        // local: it coerces non-Ioc -> FOK, see the module doc). A genuine 1:1 mapper.
        "oanda" => match tif {
            Gtc => TimeInForceOutcome::Mapped("GTC"),
            Ioc => TimeInForceOutcome::Mapped("IOC"),
            Fok => TimeInForceOutcome::Mapped("FOK"),
            Gtd => TimeInForceOutcome::Mapped("GTD"),
            Day => TimeInForceOutcome::Mapped("GFD"),
            // v20 has no post-only order: refused by name (the core preflight), never sent as a
            // plain GTC limit that could cross.
            PostOnly => TimeInForceOutcome::Unsupported,
            // v20 has no auction order
            AtOpen | AtClose => TimeInForceOutcome::Unsupported,
        },
        // event_mapper.rs::time_in_force_str — consumes this row. Lowercase wire; Alpaca has no GTD,
        // coerced to gtc.
        "alpaca" => match tif {
            Gtc => TimeInForceOutcome::Mapped("gtc"),
            Ioc => TimeInForceOutcome::Mapped("ioc"),
            Fok => TimeInForceOutcome::Mapped("fok"),
            Gtd => TimeInForceOutcome::Coerced { from: Gtd, to: Gtc, wire: "gtc" },
            Day => TimeInForceOutcome::Mapped("day"),
            // alpaca has no post-only order (and is deferred): refused by name by the preflight.
            PostOnly => TimeInForceOutcome::Unsupported,
            // `opg`/`cls` exist at the venue but the adapter (deferred) does not wire them
            AtOpen | AtClose => TimeInForceOutcome::Unsupported,
        },
        // order.rs::time_in_force_of — consumes this row; GTC/IOC/FOK/DAY are native strings on the wire.
        // GTD: the socket sends `goodTillDate` from `gtd_expiry` (`crates/bridges/vike-ibkr/src/execution/order.rs`'s
        // `map_order_request`, a GTD with no expiry refused by name; the Client Portal body has no
        // date field and refuses one by name), MAPPED behind the paper-gateway pass of
        // `crates/bridges/vike-ibkr/tests/ibkr_smokes/ibkr_attributes_smoke.rs`. The extended-hours
        // switch judges the other four by a table (`crates/bridges/vike-ibkr/src/transport/socket.rs`'s
        // `extended_hours_flags`). IB has no post-only order.
        "ibkr" => match tif {
            Gtc => TimeInForceOutcome::Mapped("GTC"),
            Ioc => TimeInForceOutcome::Mapped("IOC"),
            Fok => TimeInForceOutcome::Mapped("FOK"),
            Gtd => TimeInForceOutcome::Mapped("GTD"),
            PostOnly => TimeInForceOutcome::Unsupported,
            Day => TimeInForceOutcome::Mapped("DAY"),
            // the socket backend builds both (`map_order_request`, not `time_in_force_of`): the open is TIF
            // `OPG` on a `MKT`/`LMT`, the close the order types `MOC`/`LOC` sent `DAY` (the Client
            // Portal refuses `MOC`/`LOC` by name), MAPPED behind the paper-gateway pass of
            // `crates/bridges/vike-ibkr/tests/ibkr_smokes/ibkr_order_kinds_w2_smoke.rs`'s
            // `ibkr_order_kinds_w2_auctions_place_cancel`
            AtOpen => TimeInForceOutcome::Mapped("OPG"),
            AtClose => TimeInForceOutcome::Mapped("DAY"),
        },
        // ctrader/fxcm/dukascopy protocols carry no TIF; unknown venues land here too.
        _ => TimeInForceOutcome::NotEmitted,
    }
}

#[path = "venue_time_in_force_tests.rs"]
#[cfg(test)]
mod venue_time_in_force_tests;
