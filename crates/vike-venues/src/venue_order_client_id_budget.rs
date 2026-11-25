//! `OrderClientIdBudget` — the per-venue order-client-id budget table: how many characters of id each
//! venue actually accepts, how many of them this workspace's own adapter spends before the id
//! starts, and whether the venue ECHOES the id back on the reports reconcile reads.
//!
//! The unified twin of [`crate::attribution::attribution_for`] and
//! [`crate::venue_caps::capabilities_for`], and it exists for one reason:
//! [`vike_model::orders::client_id::origin::OrderClientIdOrigin`] puts extra characters ON THE WIRE, and "it probably
//! fits" is not an answer for a field a real venue rejects orders over. The table forces the
//! question per venue (the completeness test iterates [`vike_model::VENUES`]) and
//! [`origin_fits`] answers it arithmetically, so the assertion is per venue rather than a general
//! claim.
//!
//! # What the numbers mean (a v1 id)
//!
//! A v1 origin-tagged order client id is `<origin>V<8-hex-session><seq>` (see
//! [`vike_model::orders::client_id::origin`]), and every session minted before order client id v2 (and resumed from a
//! journal since) still mints one. The id must fit [`OrderClientIdCarriage::Verbatim`]'s `max_len` after
//! `adapter_overhead`, and what is left over after the fixed head is SEQUENCE RANGE — decimal
//! digits, i.e. how many orders one session can mint before the id outgrows the field.
//! [`MIN_SEQUENCE_DIGITS`] is the floor every venue must keep WITH a maximal origin present, and
//! `every_venue_keeps_its_sequence_range_with_a_maximal_origin` asserts it venue by venue.
//!
//! # The v2 id: one shape per engine ([`order_client_id_shape_for`])
//!
//! A v2 id (`docs/decisions/0270-client-order-ids-carry-source-strategy-account-intent-and-group.md`)
//! has a fixed-width unique part, so nothing of it is sequence range: what a venue needs is a SHAPE
//! whose longest id fits. [`order_client_id_shape_for`] answers it once per engine (at assembly, or when an
//! engine is appended; never per order): the full shape (at most 27 characters, 32 tagged) on every
//! venue but Binance, whose documented fields all take 32 (`the_full_maximum_fits_every_documented_field`),
//! and on Binance the compact shape fitted to the broker code (`BINANCE_FIT`).
//!
//! # The one venue where the head is not fixed
//!
//! Binance is the only venue whose adapter prepends anything: `binance_broker_order_client_id`
//! (`crates/bridges/binance/src/family/execution/order_map.rs`) stamps `x-<link_id>-` on an id when the whole
//! fits [`BINANCE_BROKER_ORDER_CLIENT_ID_BUDGET`] and otherwise sends the id BARE (never cut): that order loses
//! its broker attribution, never its uniqueness. `adapter_overhead` is therefore `2 + 1 + <longest
//! link id the workspace will accept>`, and the second term is not a guess: it is exactly what
//! `AttributionMechanic::validate_code` (`crates/vike-venues/src/attribution.rs`) admits, and that
//! function reserves [`vike_model::orders::client_id::origin::ORIGIN_OVERHEAD`] unconditionally so this table's
//! arithmetic holds whether or not an origin is configured. The two constants are pinned equal by
//! `the_binance_overhead_matches_what_the_attribution_validator_admits`.
//!
//! # `echoes_order_client_id` is about RECOGNITION, not about length
//!
//! Origin recognition on reconcile reads the order client id off a venue REPORT
//! (`vike_execution::recon::ReconcileDivergence`'s `is_foreign_instance`). Where the report does not hand our id
//! back — because the adapter sends a hash of it or a fixed string, wraps it in a label, or the venue
//! has no client-id field at all — a foreign instance's order is still reported, but nothing in it
//! says whose it is, and recognition degrades to exactly today's behaviour. That is a real
//! limitation of this feature and it is declared here per venue rather than in prose:
//! `docs/ops/double-live-instances.md` is the operator page that carries the consequence. Each
//! row's limit and echo are the venue's documented ones (fetched 2026-11-09 for order client id v2), not the
//! habits of our recon clients.

use crate::attribution::max_order_client_id_prefix_code_len;
use vike_model::orders::client_id::origin::SESSION_HEX_LEN;
use vike_model::orders::client_id::{FULL_U_WIDTH, MAX_GENERATION, OrderClientIdShape, U_WIDTHS};

/// Binance's `newClientOrderId` ceiling — the one venue number this table and
/// [`crate::attribution::attribution_for`] both have to know. They spell it separately (a
/// `OrderClientIdPrefix` mechanic is not an order client id budget) and
/// `the_binance_overhead_matches_what_the_attribution_validator_admits` asserts the two equal, so
/// a change to one that misses the other goes red rather than quietly re-sizing the origin's room.
const BINANCE_ORDER_CLIENT_ID_MAX_LEN: usize = 36;

/// The whole Binance order-client-id field a v2 shape is fitted to, broker prefix included: the
/// Link FAQ's "should be under 36 characters" read as at most 35 (the API regex admits 36, the
/// binance row's `max_len`). `binance_broker_order_client_id` (`crates/bridges/binance/src/family/execution/order_map.rs`)
/// prefixes only an id whose whole wire form fits it.
pub const BINANCE_BROKER_ORDER_CLIENT_ID_BUDGET: usize = 35;

/// The sequence range, in decimal digits, every venue must keep for the id WITH a maximal origin
/// tag present — 10^8 orders in one order client id session.
///
/// Chosen as a MEASUREMENT, not a preference: it is the range the tightest venue
/// (Binance, with a broker code at the longest length the attribution validator admits) had
/// BEFORE this feature existed, and `AttributionMechanic::validate_code`'s reservation is sized so
/// that the origin costs that venue nothing. `the_origin_costs_the_tightest_venue_no_sequence_range` pins
/// that equality rather than restating it.
pub const MIN_SEQUENCE_DIGITS: usize = 8;

/// How a venue carries (or fails to carry) the order client id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderClientIdCarriage {
    /// The id goes to the venue VERBATIM, in a field with a length cap.
    Verbatim {
        /// The venue's own ceiling for that field, in characters.
        max_len: usize,
        /// What this workspace's adapter spends BEFORE the id inside the same field (Binance's
        /// broker prefix is the only non-zero one).
        adapter_overhead: usize,
        /// Does a reconcile REPORT from this venue carry the id back? See the module doc.
        echoes_order_client_id: bool,
    },
    /// The id never reaches the venue verbatim: it is hashed, mapped to a venue-minted reference
    /// through a local table, or the venue has no client-id field at all. No length budget applies
    /// — and no origin can be recognised from that venue's reports either.
    NotOnTheWire,
}

impl OrderClientIdCarriage {
    /// Sequence digits left for an id carrying an origin tag of `origin_overhead` characters.
    /// `None` for [`OrderClientIdCarriage::NotOnTheWire`] — the question does not apply.
    pub fn sequence_digits(&self, origin_overhead: usize) -> Option<usize> {
        match *self {
            OrderClientIdCarriage::Verbatim { max_len, adapter_overhead, .. } => Some(
                max_len
                    .saturating_sub(adapter_overhead)
                    .saturating_sub(origin_overhead)
                    .saturating_sub(SESSION_HEX_LEN),
            ),
            OrderClientIdCarriage::NotOnTheWire => None,
        }
    }

    /// True when this venue's reports can carry an origin claim at all.
    pub fn echoes_order_client_id(&self) -> bool {
        matches!(*self, OrderClientIdCarriage::Verbatim { echoes_order_client_id: true, .. })
    }
}

/// The per-venue registry. An unknown venue is [`OrderClientIdCarriage::NotOnTheWire`] — fail-CLOSED, because
/// the alternative (inventing a length budget for a venue nobody has measured) is how an order
/// gets rejected on the wire. Every ROSTER venue is a NAMED arm —
/// `every_roster_venue_has_an_order_client_id_budget` enforces it.
pub fn order_client_id_budget_for(venue: &str) -> OrderClientIdCarriage {
    match venue {
        // `newClientOrderId`, 36 chars by the API regex (and `clientAlgoId`, the same regex).
        // Overhead is `x-` + `-` + the longest broker code `AttributionMechanic::validate_code`
        // admits — see the module doc. `binance_broker_order_client_id`
        // (`crates/bridges/binance/src/family/execution/order_map.rs`) never cuts an id: one too long for the
        // prefix goes out bare, and the v2 fit (`BINANCE_FIT`) keeps every v2 id short enough.
        "binance" => OrderClientIdCarriage::Verbatim {
            max_len: BINANCE_ORDER_CLIENT_ID_MAX_LEN,
            adapter_overhead: "x-".len()
                + "-".len()
                + max_order_client_id_prefix_code_len(BINANCE_ORDER_CLIENT_ID_MAX_LEN),
            echoes_order_client_id: true,
        },
        // Aster is the binance FAMILY without the broker programme: `attribution_for("aster")` is
        // a `SignedBuilder`, so its callers pass `link_id: None` and the order client id is sent bare.
        "aster" => OrderClientIdCarriage::Verbatim {
            max_len: 36,
            adapter_overhead: 0,
            echoes_order_client_id: true,
        },
        // `orderLinkId`, 36 chars. Bybit's attribution is an `X-Referer` HEADER, so it costs the
        // id nothing.
        "bybit" => OrderClientIdCarriage::Verbatim {
            max_len: 36,
            adapter_overhead: 0,
            echoes_order_client_id: true,
        },
        // `clOrdId`, 32 alphanumeric chars — the venue that set `is_valid_crypto_order_client_id`'s ceiling
        // (`crates/vike-model/src/orders/client_id/mod.rs`). Attribution rides the separate `tag` field.
        "okx" => OrderClientIdCarriage::Verbatim {
            max_len: 32,
            adapter_overhead: 0,
            echoes_order_client_id: true,
        },
        // `label`, 64 chars. `crates/bridges/deribit/src/reconcile/mod.rs` maps an empty/absent
        // label to `order_client_id: None` rather than dropping the row, so a labelled order is
        // recognisable and an unlabelled one reads as untagged.
        "deribit" => OrderClientIdCarriage::Verbatim {
            max_len: 64,
            adapter_overhead: 0,
            echoes_order_client_id: true,
        },
        // `clientExtensions.id` (`crates/bridges/oanda/src/execution/mod.rs` sends it on every order), and
        // `crates/bridges/oanda/src/reconcile/mod.rs` reads it back on both orders and trades.
        // ClientID length not documented by OANDA (its v20 definitions say only "string"; the 128
        // this row carried came from the third-party `oandapyV20` wrapper), so the row records 32
        // EFFECTIVE: `is_valid_crypto_order_client_id`'s ceiling, which every id we mint stays within.
        "oanda" => OrderClientIdCarriage::Verbatim {
            max_len: 32,
            adapter_overhead: 0,
            echoes_order_client_id: true,
        },
        // IBKR TWS `orderRef`, read back by `crates/bridges/vike-ibkr/src/reconcile/cpapi.rs`. TWS
        // documents no cap; recorded conservatively AT 32, the tagged v2 maximum and the
        // strictest thing any minted id can be. The Client Portal `cOID` is documented at 64, and
        // it "must be unique for a 24 hour span": our ids are never reused by construction, and an
        // explicit id a caller reuses after a restart is refused by the node's late-retry guard
        // (order client id v2, the same 24 hours) before IBKR would refuse it.
        "ibkr" => OrderClientIdCarriage::Verbatim {
            max_len: 32,
            adapter_overhead: 0,
            echoes_order_client_id: true,
        },
        // Hyperliquid hashes the id: `cloid_from_order_client_id`
        // (`crates/bridges/hyperliquid/src/execution/event_mapper.rs`) keccaks it to a 128-bit `0x…` value,
        // and that hash — not the id — is what its reports echo. Recognition is impossible from an
        // HL report by construction, and an HL `order_client_id` field therefore parses as
        // untagged (pinned in `crates/vike-model/src/orders/client_id/origin.rs`'s
        // `the_parse_refuses_every_near_miss_rather_than_guessing`).
        "hyperliquid" => OrderClientIdCarriage::NotOnTheWire,
        // IG's `dealReference` is user-defined (`[A-Za-z0-9_-]{1,30}`), but we do not send the order client id
        // in it: `crates/bridges/ig/src/execution/mod.rs`'s `deal_reference` sends `vk` + 16-hex FNV of the
        // order client id, 18 characters, and `crates/bridges/ig/src/reconcile/mod.rs` reports
        // `order_client_id: None`.
        "ig" => OrderClientIdCarriage::NotOnTheWire,
        // `clientOrderId` (50 chars) and `label` (100) on `ProtoOANewOrderReq`, both echoed:
        // `crates/bridges/ctrader/src/feed/decode.rs`'s `order_to_new_order` sends the order client id
        // verbatim in both, and `crates/bridges/ctrader/src/reconcile/mod.rs`'s `parse_orders` reads
        // it back on orders. The tighter field is the cap; a bound exit's legs
        // (`crates/bridges/ctrader/src/execution/exit_legs.rs`'s `leg_order_client_id`, `<order client id>L<n>`) stay inside it.
        "ctrader" => OrderClientIdCarriage::Verbatim {
            max_len: 50,
            adapter_overhead: 0,
            echoes_order_client_id: true,
        },
        // `order_client_id`, 128 chars, echoed on the order entity, `orders:by_order_client_id`
        // and the `trade_updates` stream; `crates/bridges/alpaca/src/reconcile/mod.rs`'s
        // `parse_orders` reads it back on orders (its fill activities carry no client id).
        "alpaca" => OrderClientIdCarriage::Verbatim {
            max_len: 128,
            adapter_overhead: 0,
            echoes_order_client_id: true,
        },
        // ForexConnect's `CustomID` exists and is echoed (`RequestTXT`, `OpenOrderRequestTXT`), but
        // the shim sends fixed strings (`crates/bridges/fxcm/src/shim/fcshim.cpp`'s
        // `RustShimEntry`), never the order client id; fills route through an in-process order-id map
        // (`crates/bridges/fxcm/src/execution/mod.rs`).
        "fxcm" => OrderClientIdCarriage::NotOnTheWire,
        // JForex `label` is 256 chars of `[A-Za-z0-9_]`, unique among current orders, but the
        // sidecar sends `v` + the sanitised order client id + `_` + seq
        // (`crates/bridges/dukascopy/jforex-bridge/src/main/java/vike/jforex/OrderMap.java`'s
        // `label`), not the order client id verbatim.
        "dukascopy" => OrderClientIdCarriage::NotOnTheWire,
        // Polymarket's CLOB order is a SIGNED struct with no client-id member; the crate's exec
        // plane carries none (`crates/bridges/polymarket/src/lib.rs`).
        "polymarket" => OrderClientIdCarriage::NotOnTheWire,
        // vike:new-venue:row // TODO(new-venue: {venue}): does this venue accept an order client id VERBATIM, and does it
        // vike:new-venue:row // echo it on reconcile reports? A NAMED `NotOnTheWire` says "asked and answered".
        // vike:new-venue:row "{venue}" => OrderClientIdCarriage::NotOnTheWire,
        _ => OrderClientIdCarriage::NotOnTheWire,
    }
}

/// Does an origin tag costing `origin_overhead` characters fit `venue` while leaving at least
/// [`MIN_SEQUENCE_DIGITS`] of sequence range? A [`OrderClientIdCarriage::NotOnTheWire`] venue always fits — there is
/// no field to overflow.
pub fn origin_fits(venue: &str, origin_overhead: usize) -> bool {
    match order_client_id_budget_for(venue).sequence_digits(origin_overhead) {
        Some(digits) => digits >= MIN_SEQUENCE_DIGITS,
        None => true,
    }
}

/// Whether a venue's exec client can look an order up by ORDER CLIENT id behind `ExecutionClient::confirm` (the
/// answer to an UNKNOWN submit after a gatewayhub epoch change), the column beside
/// [`OrderClientIdCarriage::echoes_order_client_id`]: a venue that hashes the id or keeps it in-process can still look
/// it up, so the two answers differ.
///
/// The gatewayhub's lane reads it (its own `ORDER_CLIENT_ID_LOOKUP` list was deleted for it, 2026-11-22, B3), and the
/// gatewayhub's lookup test pins this column to the bridge sources (`with_confirm(` in a venue's crate if and only if
/// its row says `true`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrderClientIdLookup {
    /// `true` only where the bridge builds its exec client `with_confirm(..)` over a venue query by order client id.
    pub looks_up: bool,
    /// Why, citing the bridge code (a `path`'s `symbol`, never a line number).
    pub why: &'static str,
}

const fn looks_up(why: &'static str) -> OrderClientIdLookup {
    OrderClientIdLookup { looks_up: true, why }
}

const fn no_lookup(why: &'static str) -> OrderClientIdLookup {
    OrderClientIdLookup { looks_up: false, why }
}

/// The `why` of a venue whose `confirm` is the `ExecutionClient` default.
const NO_CONFIRM: &str = "no `with_confirm(` in its bridge: its `confirm` is the `ExecutionClient` default, a clean \
     no-op, so reconcile and the stuck-order watchdog answer an UNKNOWN submit";

/// The [`OrderClientIdLookup`] of `venue`; an unknown venue string looks nothing up.
#[must_use]
pub fn order_client_id_lookup_for(venue: &str) -> OrderClientIdLookup {
    match venue {
        "binance" => looks_up(
            "`crates/bridges/binance/src/execution/mod.rs`'s `with_confirm`: `GET /fapi/v1/order` or \
             `/api/v3/order` by `origClientOrderId`, with the broker link prefix",
        ),
        "bybit" => looks_up(
            "`crates/bridges/bybit/src/execution/mod.rs`'s `with_confirm`: the order-link-id query, realtime then \
             history",
        ),
        "okx" => {
            looks_up("`crates/bridges/okx/src/execution/mod.rs`'s `with_confirm`: by `clOrdId`")
        }
        "aster" => looks_up(
            "`crates/bridges/aster/src/execution/mod.rs`'s `with_confirm`: binance's query on `/fapi/v3/order` or \
             `/api/v3/order`, the id bare",
        ),
        "oanda" => looks_up(
            "`crates/bridges/oanda/src/execution/mod.rs`'s `with_confirm`: `GET /orders/@{order_client_id}`; found \
             answers accepted, not found or an error silence",
        ),
        "polymarket" => looks_up(
            "`crates/bridges/polymarket/src/execution/mod.rs`'s `with_confirm`: `GET /data/order/<derived order \
             hash>` for the submits THIS process left unknown, same-epoch only",
        ),
        "ig" => looks_up(
            "`crates/bridges/ig/src/execution/mod.rs`'s `with_confirm`: the in-process deal reference to the \
             confirms, so nothing after a daemon restart",
        ),
        "fxcm" => looks_up(
            "`crates/bridges/fxcm/src/execution/mod.rs`'s `with_confirm`: the Orders table by the RequestID of a \
             placement whose reply was lost, in-process only",
        ),
        "hyperliquid" => looks_up(
            "`crates/bridges/hyperliquid/src/execution/client.rs`'s `with_confirm`: `orderStatus` by the cloid, \
             `keccak(order client id)`",
        ),
        "deribit" => no_lookup(
            "its wrapper delegates to an actor built without `with_confirm`; \
             `crates/bridges/deribit/src/execution/rest.rs`'s `resolve_ambiguous_submit` runs only inside the \
             adapter's own submit-timeout path",
        ),
        "ctrader" | "alpaca" | "ibkr" | "dukascopy" => no_lookup(NO_CONFIRM),
        // vike:new-venue:row "{venue}" => no_lookup(NO_CONFIRM), // TODO(new-venue: {venue}): true only if its bridge builds `with_confirm(..)` over a venue query by order client id
        _ => no_lookup("not a roster venue"),
    }
}

/// The Binance compact fit (decision 0270), tried in order: `(U width, replacement cap)`, each width
/// capped at [`FULL_U_WIDTH`] (the parser reads no compact `U` wider than the full one). The worst
/// cases are 25, 24, 23, 21, 20 and 17: the `R` cap goes before `U` drops below 11 (entropy first),
/// and the last step, U = 7, is for a TAGGED engine only (the tag keeps instances apart).
const BINANCE_FIT: [(u8, u8); 6] = [
    (U_WIDTHS[0], MAX_GENERATION),
    (U_WIDTHS[0], 9),
    (U_WIDTHS[1], 9),
    (U_WIDTHS[1], 0),
    (U_WIDTHS[2], 0),
    (U_WIDTHS[3], 0),
];

/// The v2 shape an engine on `venue` mints, chosen ONCE per engine (when the core is assembled or
/// an engine appended, never per order): `broker_code_len` is the configured broker code's length
/// (`0` for none) and `tag_cost` the instance tag's (`0`, or its length plus the `W` mark).
///
/// Every venue but Binance, an unknown one included, gets [`OrderClientIdShape::FULL`]: its at most 32
/// characters fit every documented field. A Binance engine gets the compact shape (no account
/// code), fitted to [`BINANCE_BROKER_ORDER_CLIENT_ID_BUDGET`] after `x-<code>-`: the first `BINANCE_FIT` step
/// whose longest id plus the tag fits; when none does, the tag is dropped on THIS engine
/// (`tag: false`, the caller's boot warning) and the fit starts again untagged. A code longer than
/// the attribution validator admits fits nothing; it gets the narrowest untagged step, whose ids
/// the bridge then sends bare (never cut).
pub fn order_client_id_shape_for(
    venue: &str,
    broker_code_len: usize,
    tag_cost: usize,
) -> OrderClientIdShape {
    match venue {
        "binance" => binance_fit(broker_code_len, tag_cost),
        _ => OrderClientIdShape::FULL,
    }
}

fn binance_fit(broker_code_len: usize, tag_cost: usize) -> OrderClientIdShape {
    let prefix = if broker_code_len == 0 { 0 } else { "x-".len() + broker_code_len + "-".len() };
    let room = BINANCE_BROKER_ORDER_CLIENT_ID_BUDGET.saturating_sub(prefix);
    let step = |(width, max_gen): (u8, u8), tag: bool| OrderClientIdShape {
        account: false,
        u_width: width.min(FULL_U_WIDTH),
        max_gen,
        tag,
    };
    let tagged_only = BINANCE_FIT.len() - 1;
    // The second pass is step 7: the tag dropped on this engine, the fit again with t = 0.
    for (tag, t) in [(true, tag_cost), (false, 0)] {
        for (i, &s) in BINANCE_FIT.iter().enumerate() {
            let shape = step(s, tag);
            if (t > 0 || i != tagged_only) && worst_len(shape) + t <= room {
                return shape;
            }
        }
    }
    step(BINANCE_FIT[4], false)
}

/// The longest id `shape` mints, without the instance tag: source 1, strategy 4, the account 2 when
/// carried, `U`, intent 2, the longest role (`TP<n>`, 3) and the longest `R<n>` its cap admits.
fn worst_len(shape: OrderClientIdShape) -> usize {
    let account = if shape.account { 2 } else { 0 };
    let generation = match shape.max_gen {
        0 => 0,
        1..=9 => 2,
        _ => 3,
    };
    1 + 4 + account + usize::from(shape.u_width) + 2 + 3 + generation
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attribution::{AttributionMechanic, attribution_for};
    use vike_model::orders::client_id::origin::{MAX_ORIGIN_LEN, ORIGIN_OVERHEAD};
    use vike_model::orders::client_id::{
        MAX_ROLE_NUMBER, OrderClientIdAccountCode, OrderClientIdGenerator, OrderClientIdIntent,
        OrderClientIdLabels, OrderClientIdLegRole, OrderClientIdPlan, OrderClientIdSource,
        OrderClientIdStrategyCode,
    };

    /// Completeness vs [`vike_model::VENUES`]: every roster venue is classified exactly once —
    /// either a length-capped verbatim field or an explicit `NotOnTheWire`. Same shape as
    /// attribution's `every_roster_venue_is_classified`.
    #[test]
    fn every_roster_venue_has_an_order_client_id_budget() {
        const VERBATIM: &[&str] =
            &["binance", "bybit", "okx", "deribit", "oanda", "ibkr", "aster", "ctrader", "alpaca"];
        #[rustfmt::skip]
        const NOT_ON_THE_WIRE: &[&str] = &[
            "hyperliquid", "ig", "fxcm", "dukascopy", "polymarket",
            // vike:new-venue:row "{venue}", // TODO(new-venue: {venue}): move to VERBATIM if the venue takes the id itself
        ];
        assert_eq!(
            VERBATIM.len() + NOT_ON_THE_WIRE.len(),
            vike_model::VENUES.len(),
            "every roster venue classified exactly once"
        );
        for &v in vike_model::VENUES {
            let verbatim = VERBATIM.contains(&v);
            let absent = NOT_ON_THE_WIRE.contains(&v);
            assert!(verbatim ^ absent, "roster venue {v} must be classified exactly once");
            assert_eq!(
                matches!(order_client_id_budget_for(v), OrderClientIdCarriage::Verbatim { .. }),
                verbatim,
                "{v}"
            );
        }
        // An unknown venue must fail CLOSED rather than inherit a neighbour's budget.
        assert_eq!(order_client_id_budget_for("nope"), OrderClientIdCarriage::NotOnTheWire);
    }

    /// THE assertion this table exists for, made venue by venue rather than in general: with a
    /// MAXIMAL origin tag on the wire, every venue still mints [`MIN_SEQUENCE_DIGITS`] decimal digits
    /// of sequence.
    #[test]
    fn every_venue_keeps_its_sequence_range_with_a_maximal_origin() {
        for &v in vike_model::VENUES {
            assert!(
                origin_fits(v, ORIGIN_OVERHEAD),
                "{v}: a maximal origin leaves only {:?} sequence digits, below the \
                 {MIN_SEQUENCE_DIGITS} floor",
                order_client_id_budget_for(v).sequence_digits(ORIGIN_OVERHEAD),
            );
        }
    }

    /// ...and the property that makes the floor above affordable: **the origin costs the tightest
    /// venue NOTHING relative to what it had before the feature existed.**
    ///
    /// The arithmetic, because the claim is easy to state loosely and mean nothing by. Before
    /// this feature, `validate_code`'s order-client-id-prefix reservation was a flat sixteen characters, so
    /// a Binance broker code at the longest admitted length left a sixteen-character id: eight of
    /// session and eight of sequence. The reservation now also covers a maximal origin tag, so the
    /// same worst case leaves twenty-one: five of tag-and-marker, eight of session, and eight of
    /// sequence — the SAME sequence range, bought by shortening the longest admitted broker code
    /// rather than by taking orders-per-session away from anyone.
    ///
    /// The pre-feature number is a literal here on purpose: it no longer exists anywhere in the
    /// tree, so this is a comparison against history, not against a constant that would move with
    /// the thing it is meant to hold still.
    #[test]
    fn the_origin_costs_the_tightest_venue_no_sequence_range() {
        /// `RESERVED_ORDER_CLIENT_ID` as `crates/vike-venues/src/attribution.rs` spelled it before instance
        /// origins existed — session + sequence, no tag.
        const RESERVED_ORDER_CLIENT_ID_BEFORE_ORIGINS: usize = 16;

        let AttributionMechanic::OrderClientIdPrefix { max_total_len } = attribution_for("binance")
        else {
            panic!("binance attributes through an order client id prefix");
        };
        let before_head = "x-".len() + "-".len();
        let old_code_budget = max_total_len - before_head - RESERVED_ORDER_CLIENT_ID_BEFORE_ORIGINS;
        let old_sequence_digits = max_total_len - before_head - old_code_budget - SESSION_HEX_LEN;

        assert_eq!(
            order_client_id_budget_for("binance").sequence_digits(ORIGIN_OVERHEAD),
            Some(old_sequence_digits),
            "the origin took sequence range away from the tightest venue"
        );
        assert_eq!(old_sequence_digits, MIN_SEQUENCE_DIGITS, "the floor IS the pre-feature range");
    }

    /// The binance row's `adapter_overhead` is DERIVED from the attribution validator, not copied
    /// beside it — this pins the two spellings equal so a widened broker-code budget cannot
    /// silently eat the origin's room.
    #[test]
    fn the_binance_overhead_matches_what_the_attribution_validator_admits() {
        let mech = attribution_for("binance");
        let AttributionMechanic::OrderClientIdPrefix { max_total_len } = mech else {
            panic!("binance must attribute through an order client id prefix");
        };
        let longest = "x".repeat(max_order_client_id_prefix_code_len(max_total_len));
        assert!(mech.validate_code(&longest).is_ok(), "the longest admitted code must validate");
        let one_over = format!("{longest}x");
        assert!(mech.validate_code(&one_over).is_err(), "one character longer must be refused");

        let OrderClientIdCarriage::Verbatim { max_len, adapter_overhead, .. } =
            order_client_id_budget_for("binance")
        else {
            panic!("binance must be a verbatim-order-client-id venue");
        };
        assert_eq!(max_len, max_total_len, "one venue ceiling, spelled in two places");
        assert_eq!(adapter_overhead, "x-".len() + "-".len() + longest.len());
    }

    /// A tag ONE character over the ceiling must not fit somewhere — otherwise
    /// [`MAX_ORIGIN_LEN`] is decorative and the per-venue assertion above proves nothing.
    #[test]
    fn the_ceiling_is_load_bearing_at_the_tightest_venue() {
        assert!(origin_fits("binance", ORIGIN_OVERHEAD));
        assert!(!origin_fits("binance", MAX_ORIGIN_LEN + 2));
    }

    /// Recognition is only possible where the venue hands the id back, and the split is asserted
    /// rather than described: the venues whose recon clients report `order_client_id: None` (or a
    /// hash) must be exactly the `NotOnTheWire` set.
    #[test]
    fn the_venues_that_can_carry_a_recognisable_origin_are_named() {
        // The split, by name. Half the roster CANNOT carry an origin claim, and a reader who does
        // not know which half will read the feature as covering everything — which is why this
        // asserts the partition rather than the field's own definition.
        const RECOGNISABLE: &[&str] =
            &["binance", "bybit", "okx", "deribit", "oanda", "ibkr", "aster", "ctrader", "alpaca"];
        for &v in vike_model::VENUES {
            assert_eq!(
                order_client_id_budget_for(v).echoes_order_client_id(),
                RECOGNISABLE.contains(&v),
                "{v}: the recognisable set moved — say so on the row and in \
                 docs/ops/double-live-instances.md, which promises this list to an operator"
            );
        }
        // The two shapes a venue fails in, each named for WHY, so a row flipped without its
        // reasoning fails here rather than silently widening what the feature claims to cover.
        assert!(
            !order_client_id_budget_for("hyperliquid").echoes_order_client_id(),
            "HL echoes the cloid HASH"
        );
        assert!(
            !order_client_id_budget_for("ig").echoes_order_client_id(),
            "IG is sent a HASH of the order client id"
        );
        assert!(
            !order_client_id_budget_for("polymarket").echoes_order_client_id(),
            "polymarket has no client-id field"
        );
        // ...and a venue whose reports carry no id must never advertise a length budget either:
        // there is nothing to spend it on, and a budget implies the claim can be read back.
        for &v in vike_model::VENUES {
            if !RECOGNISABLE.contains(&v) {
                assert_eq!(
                    order_client_id_budget_for(v),
                    OrderClientIdCarriage::NotOnTheWire,
                    "{v}"
                );
            }
        }
    }

    /// The whole table, verbatim, at the venues' documented limits (order client id v2's survey of
    /// 2026-11-09): oanda 32 effective (ClientID length not documented), ctrader's
    /// `clientOrderId` 50 and alpaca's `client_order_id` 128, both echoed.
    #[test]
    fn the_rows_are_the_documented_limits() {
        let v = |max_len, adapter_overhead| OrderClientIdCarriage::Verbatim {
            max_len,
            adapter_overhead,
            echoes_order_client_id: true,
        };
        #[rustfmt::skip]
        let matrix = [
            ("binance", v(36, 15)), ("aster", v(36, 0)), ("bybit", v(36, 0)), ("okx", v(32, 0)),
            ("deribit", v(64, 0)), ("oanda", v(32, 0)), ("ibkr", v(32, 0)), ("ctrader", v(50, 0)),
            ("alpaca", v(128, 0)), ("hyperliquid", OrderClientIdCarriage::NotOnTheWire),
            ("ig", OrderClientIdCarriage::NotOnTheWire), ("fxcm", OrderClientIdCarriage::NotOnTheWire),
            ("dukascopy", OrderClientIdCarriage::NotOnTheWire), ("polymarket", OrderClientIdCarriage::NotOnTheWire),
            // vike:new-venue:row ("{venue}", OrderClientIdCarriage::NotOnTheWire), // TODO(new-venue: {venue}): move with the row above
        ];
        assert_eq!(matrix.len(), vike_model::VENUES.len(), "the pin names every roster venue once");
        for (venue, row) in matrix {
            assert_eq!(order_client_id_budget_for(venue), row, "{venue}");
        }
    }

    /// `x-<code>-` for a code of `k` characters, nothing without one.
    fn prefix(k: usize) -> usize {
        if k == 0 { 0 } else { "x-".len() + k + "-".len() }
    }

    /// What a tagged session costs an id of `shape`: the tag cost when it is stamped, else nothing.
    fn tag_paid(shape: OrderClientIdShape, tag_cost: usize) -> usize {
        if shape.tag { tag_cost } else { 0 }
    }

    /// The longest id `shape` really mints (vike-model's mint, not this file's arithmetic), from a
    /// session tagged `tag` (`""` for none).
    fn mint_worst(shape: OrderClientIdShape, tag: &str) -> String {
        let plan = OrderClientIdPlan::new(
            shape,
            OrderClientIdAccountCode::of_id(OrderClientIdAccountCode::MAX_ID),
        );
        let worst = OrderClientIdLabels {
            src: OrderClientIdSource::Strategy,
            strat: OrderClientIdStrategyCode::parse("ZZZZ").expect("a strategy code"),
            intent: OrderClientIdIntent::CloseShort,
            role: Some(OrderClientIdLegRole::Target(MAX_ROLE_NUMBER)),
            generation: shape.max_gen,
        };
        plan.mint(&mut OrderClientIdGenerator::resume(format!("{tag}Wzzzzzzzzzzzz"), 12_345), worst)
    }

    /// Completeness vs [`vike_model::VENUES`]: Binance alone is fitted; every other roster venue,
    /// and an unknown one, mints the full shape whatever the code and tag.
    #[test]
    fn every_roster_venue_has_an_order_client_id_shape() {
        const COMPACT: &[&str] = &["binance"];
        for &v in vike_model::VENUES {
            for (k, t) in [(0, 0), (8, 0), (12, ORIGIN_OVERHEAD)] {
                let shape = order_client_id_shape_for(v, k, t);
                assert_eq!(shape.account, !COMPACT.contains(&v), "{v}");
                if !COMPACT.contains(&v) {
                    assert_eq!(shape, OrderClientIdShape::FULL, "{v} k={k} t={t}");
                }
            }
        }
        assert_eq!(
            order_client_id_shape_for("nope", 12, ORIGIN_OVERHEAD),
            OrderClientIdShape::FULL
        );
    }

    /// The full shape's longest id, a maximal tag included, is 32, and it fits every documented
    /// field but Binance's (which is why Binance alone is fitted). One over does not fit OKX: the
    /// 32 is load-bearing there.
    #[test]
    fn the_full_maximum_fits_every_documented_field() {
        let full_max = worst_len(OrderClientIdShape::FULL) + ORIGIN_OVERHEAD;
        assert_eq!(full_max, 32);
        assert_eq!(mint_worst(OrderClientIdShape::FULL, "wxyz").len(), full_max);
        for &v in vike_model::VENUES {
            let OrderClientIdCarriage::Verbatim { max_len, adapter_overhead, .. } =
                order_client_id_budget_for(v)
            else {
                continue; // hashed, wrapped or not sent: no field of ours to overflow
            };
            let room = max_len - adapter_overhead;
            assert_eq!(full_max <= room, v != "binance", "{v}: room {room}");
        }
        let OrderClientIdCarriage::Verbatim { max_len: okx, .. } =
            order_client_id_budget_for("okx")
        else {
            panic!()
        };
        let one_over = full_max + 1;
        assert!(one_over > okx, "a 33-character id must not fit okx");
    }

    /// The Binance fit, row by row: `(code length, tag cost)` gives `(U width, R cap, tag
    /// stamped)`, the longest id (tag included) and the longest wire id (prefix included). Every
    /// row is the step list's answer; the tagged rows behind a code are the ones a hand count gets
    /// wrong (a short tag fits at a wider step than the budget alone suggests).
    #[test]
    fn the_binance_fit_is_the_decision_table() {
        /// `(code length, tag cost, (U width, R cap, tag stamped), longest id, longest wire id)`.
        type FitRow = (usize, usize, (u8, u8, bool), usize, usize);
        #[rustfmt::skip]
        const ROWS: &[FitRow] = &[
            (0,  0, (12, 99, true),  25, 25),
            (8,  0, (12, 9,  true),  24, 35),
            (9,  0, (11, 9,  true),  23, 35),
            (10, 0, (11, 0,  true),  21, 34),
            (11, 0, (11, 0,  true),  21, 35),
            (12, 0, (10, 0,  true),  20, 35),
            (0,  5, (12, 99, true),  30, 30),
            // A 1-character tag behind an 8-character code: step 4 already fits (21 + 2 <= 24).
            (8,  2, (11, 0,  true),  23, 34),
            // A 4-character tag behind an 8-character code: U = 7 still fits WITH the tag.
            (8,  5, (7,  0,  true),  22, 33),
            // Behind the longest code a short tag still fits at U = 7; a longer one is dropped.
            (12, 2, (7,  0,  true),  19, 34),
            (12, 3, (7,  0,  true),  20, 35),
            (12, 4, (10, 0,  false), 20, 35),
            (12, 5, (10, 0,  false), 20, 35),
        ];
        for &(k, t, (u_width, max_gen, tag), max, wire) in ROWS {
            let shape = order_client_id_shape_for("binance", k, t);
            assert_eq!(
                shape,
                OrderClientIdShape { account: false, u_width, max_gen, tag },
                "k={k} t={t}"
            );
            assert_eq!(worst_len(shape) + tag_paid(shape, t), max, "k={k} t={t}");
            assert_eq!(prefix(k) + max, wire, "k={k} t={t}");
        }
        // Binance at 36 with a code: the step before the chosen one is one character too long.
        let one_wider = OrderClientIdShape {
            max_gen: MAX_GENERATION,
            ..order_client_id_shape_for("binance", 8, 0)
        };
        assert_eq!(prefix(8) + worst_len(one_wider), BINANCE_BROKER_ORDER_CLIENT_ID_BUDGET + 1);
    }

    /// THE Binance pin: for every code length the validator admits (0..=12) and every tag cost
    /// (0..=5), `x-` + code + `-` + the fitted longest id fits the 35-character budget, the shape is
    /// the FIRST step that fits (the tag dropped only when no tagged step does), and vike-model's
    /// own plan accepts it and mints exactly that longest id.
    #[test]
    fn every_admitted_code_and_tag_fits_the_binance_budget() {
        let AttributionMechanic::OrderClientIdPrefix { max_total_len } = attribution_for("binance")
        else {
            panic!("binance attributes through an order client id prefix");
        };
        let longest_code = max_order_client_id_prefix_code_len(max_total_len);
        assert_eq!(longest_code, 12);
        for k in 0..=longest_code {
            for t in 0..=ORIGIN_OVERHEAD {
                let shape = order_client_id_shape_for("binance", k, t);
                let wire = prefix(k) + worst_len(shape) + tag_paid(shape, t);
                assert!(wire <= BINANCE_BROKER_ORDER_CLIENT_ID_BUDGET, "k={k} t={t}: {wire}");
                // A tag cost of 1 is arithmetic only: a real tag is 1-4 characters plus `W`.
                if t != 1 {
                    let tag = &"wxyz"[..t.saturating_sub(1)];
                    assert_eq!(
                        mint_worst(shape, tag).len(),
                        worst_len(shape) + tag_paid(shape, t),
                        "k={k} t={t}"
                    );
                }
                let room = BINANCE_BROKER_ORDER_CLIENT_ID_BUDGET - prefix(k);
                for (i, &(w, g)) in BINANCE_FIT.iter().enumerate() {
                    let u_width = w.min(FULL_U_WIDTH);
                    let earlier =
                        OrderClientIdShape { account: false, u_width, max_gen: g, tag: true };
                    if earlier == shape {
                        break;
                    }
                    let usable = t > 0 || i != BINANCE_FIT.len() - 1;
                    assert!(
                        !usable || worst_len(earlier) + t > room,
                        "k={k} t={t}: {earlier:?} fits before {shape:?}"
                    );
                }
            }
        }
    }

    /// The longest code the validator admits still leaves room for the narrowest UNTAGGED step, so
    /// no admitted code makes an engine's v2 ids go out bare: `x-` + 12 + `-` + 20 = 35. The
    /// attribution reservation (21, a v1 id with a maximal origin and 8 sequence digits) is sized
    /// to the API's 36, one more.
    #[test]
    fn the_longest_admitted_code_leaves_room_for_the_narrowest_untagged_step() {
        let AttributionMechanic::OrderClientIdPrefix { max_total_len } = attribution_for("binance")
        else {
            panic!("binance attributes through an order client id prefix");
        };
        let head = prefix(max_order_client_id_prefix_code_len(max_total_len));
        let narrowest = order_client_id_shape_for(
            "binance",
            max_order_client_id_prefix_code_len(max_total_len),
            0,
        );
        assert_eq!(head + worst_len(narrowest), BINANCE_BROKER_ORDER_CLIENT_ID_BUDGET);
        assert_eq!(
            head + SESSION_HEX_LEN + ORIGIN_OVERHEAD + MIN_SEQUENCE_DIGITS,
            BINANCE_ORDER_CLIENT_ID_MAX_LEN,
            "the reservation fills the API ceiling exactly"
        );
    }

    /// A code longer than the validator admits fits nothing: the engine gets the narrowest
    /// untagged step, which its plan still accepts, and the bridge sends those ids bare.
    #[test]
    fn a_code_the_validator_refuses_gets_the_narrowest_untagged_step() {
        for k in 13..=20 {
            for t in [0, ORIGIN_OVERHEAD] {
                let shape = order_client_id_shape_for("binance", k, t);
                assert_eq!(
                    shape,
                    OrderClientIdShape { account: false, u_width: 10, max_gen: 0, tag: false }
                );
                assert!(
                    prefix(k) + worst_len(shape) > BINANCE_BROKER_ORDER_CLIENT_ID_BUDGET,
                    "k={k}"
                );
                let _ = OrderClientIdPlan::new(shape, OrderClientIdAccountCode::NONE);
            }
        }
    }

    /// This file's length arithmetic is vike-model's mint: every fit step and the full shape, with
    /// and without a maximal tag, mints exactly `worst_len` (plus the tag when stamped).
    #[test]
    fn worst_len_is_what_the_mint_renders() {
        let steps = BINANCE_FIT.iter().map(|&(w, g)| OrderClientIdShape {
            account: false,
            u_width: w.min(FULL_U_WIDTH),
            max_gen: g,
            tag: true,
        });
        for shape in steps.chain([OrderClientIdShape::FULL]) {
            assert_eq!(mint_worst(shape, "").len(), worst_len(shape), "{shape:?}");
            assert_eq!(mint_worst(shape, "wxyz").len(), worst_len(shape) + 5, "{shape:?}");
            let untagged = OrderClientIdShape { tag: false, ..shape };
            assert_eq!(mint_worst(untagged, "wxyz").len(), worst_len(shape), "{shape:?}");
        }
    }

    /// The lookup column, verbatim (in the order of the gatewayhub list it replaced), and the two columns are
    /// different facts, apart in both directions: ig, fxcm, polymarket and hyperliquid look an order up by an id their
    /// reports never echo, and deribit, ibkr, ctrader and alpaca echo an id they cannot look up behind `confirm`.
    #[test]
    fn the_order_client_id_lookup_column_is_pinned_verbatim() {
        #[rustfmt::skip]
        const MATRIX: &[(&str, bool)] = &[
            ("binance",     true),
            ("bybit",       true),
            ("okx",         true),
            ("deribit",     false),
            ("oanda",       true),
            ("ig",          true),
            ("fxcm",        true),
            ("dukascopy",   false),
            ("polymarket",  true),
            ("ibkr",        false),
            ("ctrader",     false),
            ("alpaca",      false),
            ("aster",       true),
            ("hyperliquid", true),
            // vike:new-venue:row ("{venue}", false), // TODO(new-venue: {venue}): move with the row above
        ];
        assert_eq!(MATRIX.len(), vike_model::VENUES.len(), "the pin names every roster venue once");
        let fallback = order_client_id_lookup_for("no-such-venue");
        for &(venue, looks_up) in MATRIX {
            assert!(vike_model::VENUES.contains(&venue), "{venue} is not a roster venue");
            let row = order_client_id_lookup_for(venue);
            assert_eq!(row.looks_up, looks_up, "{venue}");
            assert_ne!(row, fallback, "{venue} rides the unknown-venue fallback");
        }
        assert!(!fallback.looks_up, "an unknown venue looks nothing up");
        let differ: Vec<&str> = MATRIX
            .iter()
            .filter(|(v, _)| {
                order_client_id_budget_for(v).echoes_order_client_id()
                    != order_client_id_lookup_for(v).looks_up
            })
            .map(|(v, _)| *v)
            .collect();
        assert_eq!(
            differ,
            ["deribit", "ig", "fxcm", "polymarket", "ibkr", "ctrader", "alpaca", "hyperliquid"],
            "the two columns part only here"
        );
    }
}
