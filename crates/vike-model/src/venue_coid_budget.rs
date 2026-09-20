//! `CoidBudget` — the per-venue client-order-id budget table: how many characters of id each
//! venue actually accepts, how many of them this workspace's own adapter spends before the id
//! starts, and whether the venue ECHOES the id back on the reports reconcile reads.
//!
//! The unified twin of [`crate::attribution::attribution_for`] and
//! [`crate::venue_caps::caps_for`], and it exists for one reason:
//! [`crate::instance_origin::InstanceOrigin`] puts extra characters ON THE WIRE, and "it probably
//! fits" is not an answer for a field a real venue rejects orders over. The table forces the
//! question per venue (the completeness test iterates [`crate::venues::VENUES`]) and
//! [`origin_fits`] answers it arithmetically, so the assertion is per venue rather than a general
//! claim.
//!
//! # What the numbers mean
//!
//! An origin-tagged coid is `<origin>V<8-hex-session><seq>` (see
//! [`crate::instance_origin`]). The id must fit
//! [`CoidWire::Verbatim`]'s `max_len` after `adapter_overhead`, and what is left over after the
//! fixed head is SEQUENCE RANGE — decimal digits, i.e. how many orders one session can mint before
//! the id gets truncated. [`MIN_SEQ_DIGITS`] is the floor every venue must keep WITH a maximal
//! origin present, and `every_venue_keeps_its_sequence_range_with_a_maximal_origin` asserts it
//! venue by venue.
//!
//! # The one venue where the head is not fixed
//!
//! Binance is the only venue whose adapter prepends anything: `binance_broker_coid`
//! (`crates/bridges/binance/src/family/order_map.rs`) stamps `x-<link_id>-` and then TRUNCATES THE
//! COID TAIL to Binance's 36-character ceiling. `adapter_overhead` is therefore `2 + 1 + <longest
//! link id the workspace will accept>`, and the second term is not a guess: it is exactly what
//! `AttributionMechanic::validate_code` (`crates/vike-model/src/attribution.rs`) admits, and that
//! function reserves [`crate::instance_origin::ORIGIN_OVERHEAD`] unconditionally so this table's
//! arithmetic holds whether or not an origin is configured. The two constants are pinned equal by
//! `the_binance_overhead_matches_what_the_attribution_validator_admits`.
//!
//! # `echoes_coid` is about RECOGNITION, not about length
//!
//! Origin recognition on reconcile reads the coid off a venue REPORT
//! (`vike_exec::recon::Divergence`'s `origin_for`). Where the venue does not hand our id back —
//! because it hashes it, because it mints its own reference, or because it has no client-id field
//! at all — a foreign instance's order is still reported, but nothing in it says whose it is, and
//! recognition degrades to exactly today's behaviour. That is a real limitation of this feature and
//! it is declared here per venue rather than in prose: `docs/ops/double-live-instances.md` is the
//! operator page that carries the consequence.

use crate::attribution::max_coid_prefix_code_len;
use crate::instance_origin::SESSION_HEX_LEN;

/// Binance's `newClientOrderId` ceiling — the one venue number this table and
/// [`crate::attribution::attribution_for`] both have to know. They spell it separately (a
/// `CoidPrefix` mechanic is not a coid budget) and
/// `the_binance_overhead_matches_what_the_attribution_validator_admits` asserts the two equal, so
/// a change to one that misses the other goes red rather than quietly re-sizing the origin's room.
const BINANCE_COID_MAX_LEN: usize = 36;

/// The sequence range, in decimal digits, every venue must keep for the id WITH a maximal origin
/// tag present — 10^8 orders in one coid session.
///
/// Chosen as a MEASUREMENT, not a preference: it is the range the tightest venue
/// (Binance, with a broker code at the longest length the attribution validator admits) had
/// BEFORE this feature existed, and `AttributionMechanic::validate_code`'s reservation is sized so
/// that the origin costs that venue nothing. `the_origin_costs_the_tightest_venue_no_sequence_range` pins
/// that equality rather than restating it.
pub const MIN_SEQ_DIGITS: usize = 8;

/// How a venue carries (or fails to carry) the client order id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoidWire {
    /// The id goes to the venue VERBATIM, in a field with a length cap.
    Verbatim {
        /// The venue's own ceiling for that field, in characters.
        max_len: usize,
        /// What this workspace's adapter spends BEFORE the id inside the same field (Binance's
        /// broker prefix is the only non-zero one).
        adapter_overhead: usize,
        /// Does a reconcile REPORT from this venue carry the id back? See the module doc.
        echoes_coid: bool,
    },
    /// The id never reaches the venue verbatim: it is hashed, mapped to a venue-minted reference
    /// through a local table, or the venue has no client-id field at all. No length budget applies
    /// — and no origin can be recognised from that venue's reports either.
    NotOnTheWire,
}

impl CoidWire {
    /// Sequence digits left for an id carrying an origin tag of `origin_overhead` characters.
    /// `None` for [`CoidWire::NotOnTheWire`] — the question does not apply.
    pub fn seq_digits(&self, origin_overhead: usize) -> Option<usize> {
        match *self {
            CoidWire::Verbatim { max_len, adapter_overhead, .. } => Some(
                max_len
                    .saturating_sub(adapter_overhead)
                    .saturating_sub(origin_overhead)
                    .saturating_sub(SESSION_HEX_LEN),
            ),
            CoidWire::NotOnTheWire => None,
        }
    }

    /// True when this venue's reports can carry an origin claim at all.
    pub fn echoes_coid(&self) -> bool {
        matches!(*self, CoidWire::Verbatim { echoes_coid: true, .. })
    }
}

/// The per-venue registry. An unknown venue is [`CoidWire::NotOnTheWire`] — fail-CLOSED, because
/// the alternative (inventing a length budget for a venue nobody has measured) is how an order
/// gets rejected on the wire. Every ROSTER venue is a NAMED arm —
/// `every_roster_venue_has_a_coid_budget` enforces it.
pub fn coid_budget_for(venue: &str) -> CoidWire {
    match venue {
        // `newClientOrderId`, 36 chars. Overhead is `x-` + `-` + the longest broker code
        // `AttributionMechanic::validate_code` admits — see the module doc, and
        // `binance_broker_coid` (`crates/bridges/binance/src/family/order_map.rs`) for the
        // truncation this budget exists to keep away from the sequence.
        "binance" => CoidWire::Verbatim {
            max_len: BINANCE_COID_MAX_LEN,
            adapter_overhead: "x-".len()
                + "-".len()
                + max_coid_prefix_code_len(BINANCE_COID_MAX_LEN),
            echoes_coid: true,
        },
        // Aster is the binance FAMILY without the broker programme: `attribution_for("aster")` is
        // a `SignedBuilder`, so its callers pass `link_id: None` and the coid is sent bare.
        "aster" => CoidWire::Verbatim { max_len: 36, adapter_overhead: 0, echoes_coid: true },
        // `orderLinkId`, 36 chars. Bybit's attribution is an `X-Referer` HEADER, so it costs the
        // id nothing.
        "bybit" => CoidWire::Verbatim { max_len: 36, adapter_overhead: 0, echoes_coid: true },
        // `clOrdId`, 32 alphanumeric chars — the venue that set `is_valid_crypto_coid`'s ceiling
        // (`crates/vike-model/src/client_order_id.rs`). Attribution rides the separate `tag` field.
        "okx" => CoidWire::Verbatim { max_len: 32, adapter_overhead: 0, echoes_coid: true },
        // `label`, 64 chars. `crates/bridges/deribit/src/recon_client.rs` maps an empty/absent
        // label to `client_order_id: None` rather than dropping the row, so a labelled order is
        // recognisable and an unlabelled one reads as untagged.
        "deribit" => CoidWire::Verbatim { max_len: 64, adapter_overhead: 0, echoes_coid: true },
        // `clientExtensions.id` (`crates/bridges/oanda/src/exec.rs` sends it on every order), and
        // `crates/bridges/oanda/src/recon_client.rs` reads it back on both orders and trades.
        // OANDA's ClientID is a 128-character field; the workspace never approaches it because
        // `is_valid_crypto_coid` caps every minted id at 32.
        "oanda" => CoidWire::Verbatim { max_len: 128, adapter_overhead: 0, echoes_coid: true },
        // IBKR `orderRef`, read back by `crates/bridges/vike-ibkr/src/recon_client.rs`. TWS does
        // not document a tighter cap than the workspace's own 32; recorded conservatively AT 32,
        // which is the strictest thing any minted id can be.
        "ibkr" => CoidWire::Verbatim { max_len: 32, adapter_overhead: 0, echoes_coid: true },
        // Hyperliquid hashes the id: `cloid_from_client_order_id`
        // (`crates/bridges/hyperliquid/src/event_mapper.rs`) keccaks it to a 128-bit `0x…` value,
        // and that hash — not the id — is what its reports echo. Recognition is impossible from an
        // HL report by construction, and an HL `client_order_id` field therefore parses as
        // untagged (pinned in `crates/vike-model/src/instance_origin.rs`'s
        // `the_parse_refuses_every_near_miss_rather_than_guessing`).
        "hyperliquid" => CoidWire::NotOnTheWire,
        // IG mints its own `dealReference` and has no get-order-by-client-id endpoint;
        // `crates/bridges/ig/src/exec.rs`'s `DealRefs` keeps the mapping in this process, and
        // `crates/bridges/ig/src/recon_client.rs` reports `client_order_id: None`.
        "ig" => CoidWire::NotOnTheWire,
        // cTrader correlates by the venue's numeric `orderId` through `conn::OrderIdMap`
        // (`crates/bridges/ctrader/src/exec.rs`); its recon client reports
        // `client_order_id: None`.
        "ctrader" => CoidWire::NotOnTheWire,
        // Alpaca accepts a `client_order_id` on submit, but `crates/bridges/alpaca/src/exec.rs`
        // resolves the venue id through a fresh `orders:by_client_order_id` lookup and its recon
        // client reports `client_order_id: None` — nothing an origin could be read from.
        "alpaca" => CoidWire::NotOnTheWire,
        // The FXCM ForexConnect SDK routes fills through an in-process order-id map
        // (`crates/bridges/fxcm/src/exec.rs`); no client-id field survives to a report.
        "fxcm" => CoidWire::NotOnTheWire,
        // Dukascopy's id goes to OUR OWN Java sidecar over JSON-lines stdio
        // (`crates/bridges/dukascopy/src/proto.rs`), not to a venue field, and JForex is
        // position-per-order — there is no venue-side client id at all.
        "dukascopy" => CoidWire::NotOnTheWire,
        // Polymarket's CLOB order is a SIGNED struct with no client-id member; the crate's exec
        // plane carries none (`crates/bridges/polymarket/src/lib.rs`).
        "polymarket" => CoidWire::NotOnTheWire,
        // vike:new-venue:row // TODO(new-venue: {venue}): does this venue accept a client order id VERBATIM, and does it
        // vike:new-venue:row // echo it on reconcile reports? A NAMED `NotOnTheWire` says "asked and answered".
        // vike:new-venue:row "{venue}" => CoidWire::NotOnTheWire,
        _ => CoidWire::NotOnTheWire,
    }
}

/// Does an origin tag costing `origin_overhead` characters fit `venue` while leaving at least
/// [`MIN_SEQ_DIGITS`] of sequence range? A [`CoidWire::NotOnTheWire`] venue always fits — there is
/// no field to overflow.
pub fn origin_fits(venue: &str, origin_overhead: usize) -> bool {
    match coid_budget_for(venue).seq_digits(origin_overhead) {
        Some(digits) => digits >= MIN_SEQ_DIGITS,
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attribution::{AttributionMechanic, attribution_for};
    use crate::instance_origin::{MAX_ORIGIN_LEN, ORIGIN_OVERHEAD};

    /// Completeness vs [`crate::venues::VENUES`]: every roster venue is classified exactly once —
    /// either a length-capped verbatim field or an explicit `NotOnTheWire`. Same shape as
    /// attribution's `every_roster_venue_is_classified`.
    #[test]
    fn every_roster_venue_has_a_coid_budget() {
        const VERBATIM: &[&str] = &["binance", "bybit", "okx", "deribit", "oanda", "ibkr", "aster"];
        #[rustfmt::skip]
        const NOT_ON_THE_WIRE: &[&str] = &[
            "hyperliquid", "ig", "ctrader", "alpaca", "fxcm", "dukascopy", "polymarket",
            // vike:new-venue:row "{venue}", // TODO(new-venue: {venue}): move to VERBATIM if the venue takes the id itself
        ];
        assert_eq!(
            VERBATIM.len() + NOT_ON_THE_WIRE.len(),
            crate::venues::VENUES.len(),
            "every roster venue classified exactly once"
        );
        for &v in crate::venues::VENUES {
            let verbatim = VERBATIM.contains(&v);
            let absent = NOT_ON_THE_WIRE.contains(&v);
            assert!(verbatim ^ absent, "roster venue {v} must be classified exactly once");
            assert_eq!(matches!(coid_budget_for(v), CoidWire::Verbatim { .. }), verbatim, "{v}");
        }
        // An unknown venue must fail CLOSED rather than inherit a neighbour's budget.
        assert_eq!(coid_budget_for("nope"), CoidWire::NotOnTheWire);
    }

    /// THE assertion this table exists for, made venue by venue rather than in general: with a
    /// MAXIMAL origin tag on the wire, every venue still mints [`MIN_SEQ_DIGITS`] decimal digits
    /// of sequence.
    #[test]
    fn every_venue_keeps_its_sequence_range_with_a_maximal_origin() {
        for &v in crate::venues::VENUES {
            assert!(
                origin_fits(v, ORIGIN_OVERHEAD),
                "{v}: a maximal origin leaves only {:?} sequence digits, below the \
                 {MIN_SEQ_DIGITS} floor",
                coid_budget_for(v).seq_digits(ORIGIN_OVERHEAD),
            );
        }
    }

    /// ...and the property that makes the floor above affordable: **the origin costs the tightest
    /// venue NOTHING relative to what it had before the feature existed.**
    ///
    /// The arithmetic, because the claim is easy to state loosely and mean nothing by. Before
    /// this feature, `validate_code`'s coid-prefix reservation was a flat sixteen characters, so
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
        /// `RESERVED_COID` as `crates/vike-model/src/attribution.rs` spelled it before instance
        /// origins existed — session + sequence, no tag.
        const RESERVED_COID_BEFORE_ORIGINS: usize = 16;

        let AttributionMechanic::CoidPrefix { max_total_len } = attribution_for("binance") else {
            panic!("binance attributes through a coid prefix");
        };
        let before_head = "x-".len() + "-".len();
        let old_code_budget = max_total_len - before_head - RESERVED_COID_BEFORE_ORIGINS;
        let old_seq_digits = max_total_len - before_head - old_code_budget - SESSION_HEX_LEN;

        assert_eq!(
            coid_budget_for("binance").seq_digits(ORIGIN_OVERHEAD),
            Some(old_seq_digits),
            "the origin took sequence range away from the tightest venue"
        );
        assert_eq!(old_seq_digits, MIN_SEQ_DIGITS, "the floor IS the pre-feature range");
    }

    /// The binance row's `adapter_overhead` is DERIVED from the attribution validator, not copied
    /// beside it — this pins the two spellings equal so a widened broker-code budget cannot
    /// silently eat the origin's room.
    #[test]
    fn the_binance_overhead_matches_what_the_attribution_validator_admits() {
        let mech = attribution_for("binance");
        let AttributionMechanic::CoidPrefix { max_total_len } = mech else {
            panic!("binance must attribute through a coid prefix");
        };
        let longest = "x".repeat(max_coid_prefix_code_len(max_total_len));
        assert!(mech.validate_code(&longest).is_ok(), "the longest admitted code must validate");
        let one_over = format!("{longest}x");
        assert!(mech.validate_code(&one_over).is_err(), "one character longer must be refused");

        let CoidWire::Verbatim { max_len, adapter_overhead, .. } = coid_budget_for("binance")
        else {
            panic!("binance must be a verbatim-coid venue");
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
    /// rather than described: the venues whose recon clients report `client_order_id: None` (or a
    /// hash) must be exactly the `NotOnTheWire` set.
    #[test]
    fn the_venues_that_can_carry_a_recognisable_origin_are_named() {
        // The split, by name. Half the roster CANNOT carry an origin claim, and a reader who does
        // not know which half will read the feature as covering everything — which is why this
        // asserts the partition rather than the field's own definition.
        const RECOGNISABLE: &[&str] =
            &["binance", "bybit", "okx", "deribit", "oanda", "ibkr", "aster"];
        for &v in crate::venues::VENUES {
            assert_eq!(
                coid_budget_for(v).echoes_coid(),
                RECOGNISABLE.contains(&v),
                "{v}: the recognisable set moved — say so on the row and in \
                 docs/ops/double-live-instances.md, which promises this list to an operator"
            );
        }
        // The two shapes a venue fails in, each named for WHY, so a row flipped without its
        // reasoning fails here rather than silently widening what the feature claims to cover.
        assert!(!coid_budget_for("hyperliquid").echoes_coid(), "HL echoes the cloid HASH");
        assert!(!coid_budget_for("alpaca").echoes_coid(), "alpaca's recon reports carry no coid");
        // ...and a venue whose reports carry no id must never advertise a length budget either:
        // there is nothing to spend it on, and a budget implies the claim can be read back.
        for &v in crate::venues::VENUES {
            if !RECOGNISABLE.contains(&v) {
                assert_eq!(coid_budget_for(v), CoidWire::NotOnTheWire, "{v}");
            }
        }
    }
}
