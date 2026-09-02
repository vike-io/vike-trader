//! `AmendSemantics` — the per-venue declaration of **what an amend's quantity MEANS**, and the one
//! venue fact the pre-trade gate needs in order to stop charging a partially filled order's
//! executed lots twice.
//!
//! # The question this table answers
//!
//! An amend names a quantity. THREE conventions exist, and they disagree about how much MORE the
//! order can still execute:
//!
//! * **in-place, TOTAL** — the venue keeps the order (same venue order id, same executions
//!   attached) and the number is its new TOTAL size. The order can still execute
//!   `new_qty − filled_qty`.
//! * **cancel-replace** — the venue kills the order and rests a FRESH one. A replacement carries no
//!   execution history, so it can execute `new_qty`, on top of whatever the dead order already did.
//! * **in-place, REMAINING** — the order survives, but the number REPLACES its outstanding size, so
//!   the whole `new_qty` is still coming. Arithmetically that is the cancel-replace answer; it is a
//!   separate row because the order IDENTITY differs and because its one implementation in this
//!   workspace is not a venue at all — see "the paper exchange" below.
//!
//! `vike_exec::RiskGate` projects the post-order world as `position + side × qty`, and after a
//! partial fill the account position ALREADY holds that order's own executed lots. Under the
//! in-place convention the executed part is therefore on BOTH sides of that sum — the double count
//! this table exists to remove. Under cancel-replace the same expression is exactly RIGHT, because
//! the replacement really can add `new_qty` on top of the position.
//!
//! ONE expression cannot be right for both, which is why the correction was declared and pinned
//! before it was applied (`crates/vike-exec/tests/engine/partial_fill_amend_accounting.rs`, and the
//! analysis in `docs/superpowers/specs/2026-08-07-partial-fill-amend-accounting.md`). This module is
//! step 2 of the per-venue-capability playbook the root `CLAUDE.md` states: declare the reality,
//! then flip behaviour one row at a time.
//!
//! # ⚠ The risk runs ONE WAY, and the table is shaped around that
//!
//! Today's arithmetic errs CONSERVATIVE: it over-states projected exposure by the executed qty, so
//! it can wrongly DENY an amend and can never wrongly admit one. Subtracting the executed qty on a
//! venue that is really cancel-replace would ADMIT an order the gate should refuse — live money, in
//! the direction that costs.
//!
//! So the arithmetic is asymmetric by construction: [`AmendSemantics::already_in_position`] returns
//! a non-zero value for **exactly one** variant, [`AmendSemantics::InPlaceTotal`], and `0.0` — i.e.
//! today's behaviour, byte-identical — for every other variant AND for any venue string this table
//! does not know. A misclassification among the four conservative arms cannot change a verdict at
//! all; only an `InPlaceTotal` row can, and only an `InPlaceTotal` row needs evidence. That is why
//! [`AmendSemantics::Unknown`] is a first-class NAMED value rather than something to avoid: a venue
//! whose convention has not been established is declared as such and keeps the safe arithmetic.
//!
//! # ⚠ The paper exchange is NOT an unrecognised venue string
//!
//! It is tempting to think a paper or backtest mount runs under a made-up id ("sim", "paper") and so
//! falls into the conservative fallback arm for free. **It does not.** `vike_mount::make_engine`
//! builds `ExecutionEngine::new(…, venue, symbol)` with the REAL venue string even when the
//! absent-credentials gate fell back to `vike_paper::PaperExecutionClient`, and
//! `vike_run::build_paper_maker_core` does the same with its profile's `venue` — so a paper mount on
//! binance would look this table up and get `InPlaceTotal`.
//!
//! That matters because the paper book implements the THIRD convention:
//! `crates/vike-paper/src/lib.rs`'s `modify` assigns `resting.size = q`, i.e. the amend's quantity
//! becomes the size that can still fill. Netting `filled_qty` out of it would let the gate assume
//! `q − filled` while the book executes `q`.
//!
//! So the netting is keyed on the CLIENT FIRST and this table second:
//! `vike_exec::ExecutionClient::amend_semantics` returns `None` for every venue adapter (defer to
//! the venue string, unchanged) and `Some(`[`AmendSemantics::InPlaceRemaining`]`)` for the paper
//! exchange, which travels WITH the client so no mount can forget to declare it.
//!
//! # Evidence classes, per row
//!
//! Rows are read from two places, and each row's doc says which:
//!
//! * **in-tree** — the adapter's own code. Hyperliquid is the strongest row on the table for this
//!   reason: `crates/bridges/hyperliquid/src/exec.rs`'s `modify` builds a WHOLE replacement order,
//!   sends it as one `ModifyWire`, and then re-keys the order to the NEW oid the venue returned
//!   (`parse_first_resting_oid`). An adapter that must swap the venue order id after an amend is
//!   cancel-replace, and no vendor doc is needed to see it.
//! * **vendor doc, on the exact case** — the venue's own published statement about amending a
//!   PARTIALLY FILLED order. This is the evidence class `crate::venue_margin_support` already
//!   accepts and labels, and it is the strongest available without placing a real partially-filled
//!   demo order (see "what would upgrade a row" below).
//!
//! A wire FIELD NAME is NOT evidence and no row rests on one — `quantity`, `newSz`, `qty` and
//! `volume` all read the same under every convention.
//!
//! # The KEY is the bare venue string, and one row is narrower than its key
//!
//! [`amend_semantics`] is keyed on the venue id the engine was mounted with, which has no
//! backend/product axis — while an adapter may route internally. `BINANCE`'s evidence is
//! PERP-specific and its row's doc says so; `IBKR` is the mirror case on the sibling table (see
//! [`amend_semantics_agrees_with_venue_caps_supports_modify`], which pins the one declared
//! disagreement with `crate::venue_caps`'s `supports_modify` rather than letting the two tables
//! drift apart unwatched).
//!
//! # What would upgrade a row
//!
//! Amending a genuinely partially-filled order on a demo account and reading the venue's reply: an
//! in-place venue reports the SAME order id with the cumulative executed qty preserved, a
//! cancel-replace venue reports a new one. Every roster venue with credentials already has an
//! `#[ignore]`d demo smoke to hang that on. Until then the three `Unknown` rows stay `Unknown`, and
//! that costs nothing but the over-conservative refusal they have always had.

/// What a venue's amend does to a resting order's identity and its accumulated executions — the
/// axis that decides how much MORE an amended order can still execute.
///
/// Only [`AmendSemantics::InPlaceTotal`] changes any pre-trade verdict (see the module doc); the
/// other four are four different REASONS for the same conservative arithmetic, kept distinct
/// because "we know it replaces the order", "the number is the REMAINING size", "it cannot be
/// amended at all" and "nobody has established this" are different states of knowledge and age
/// differently.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub enum AmendSemantics {
    /// The venue amends the order IN PLACE: same venue order id, executions stay attached, and the
    /// amend's quantity is the order's new TOTAL — so the still-executable part is
    /// `new_qty − filled_qty`.
    InPlaceTotal,
    /// The venue cancel-replaces: the resting order dies and a FRESH order of the amend's quantity
    /// takes its place, carrying no execution history — so the still-executable part is the whole
    /// `new_qty`, on top of whatever the dead order already executed.
    CancelReplace,
    /// The order survives, but the amend's quantity REPLACES its outstanding size rather than its
    /// total — so the still-executable part is the whole `new_qty`, exactly as under
    /// [`AmendSemantics::CancelReplace`], and netting the executed lots out would let the gate
    /// assume less is coming than the book will actually execute.
    ///
    /// No roster venue declares this. It exists because the R7 PAPER EXCHANGE implements it —
    /// `crates/vike-paper/src/lib.rs`'s `modify` assigns `resting.size = q` — and a paper mount
    /// carries the REAL venue string (module doc), so without a row of its own a paper mount on
    /// binance would silently be judged under `InPlaceTotal`. Declared by the CLIENT
    /// (`vike_exec::ExecutionClient::amend_semantics`), never by [`amend_semantics`].
    InPlaceRemaining,
    /// The venue has no native amend at all — the adapter takes `ExecutionClient`'s default no-op
    /// `modify`, so no amend ever reaches a venue and there is no convention to declare.
    Unsupported,
    /// Not established. The conservative default, and the value an unrecognised venue string
    /// resolves to.
    #[default]
    Unknown,
}

impl AmendSemantics {
    /// How much of an amend's requested TOTAL quantity is ALREADY executed and therefore already
    /// reflected in the account position the gate projects against.
    ///
    /// `filled_qty` is the amended order's accumulated execution (`vike_exec::ManagedOrder`'s
    /// `filled_qty`). **Non-zero for [`AmendSemantics::InPlaceTotal`] and for nothing else** — that
    /// asymmetry is the whole safety argument (module doc), so it is pinned by
    /// [`only_in_place_total_subtracts_anything`].
    ///
    /// A non-finite or negative `filled_qty` yields `0.0` rather than propagating: this value is
    /// subtracted from an order quantity inside a risk gate, and a NaN there would make every
    /// `projected > cap` comparison false — silently vacating the ceiling instead of tightening it.
    #[must_use]
    #[inline]
    pub fn already_in_position(self, filled_qty: f64) -> f64 {
        match self {
            AmendSemantics::InPlaceTotal if filled_qty.is_finite() && filled_qty > 0.0 => {
                filled_qty
            }
            _ => 0.0,
        }
    }
}

// ---------------------------------------------------------------------------------------------
// The per-venue rows. Each cites what it was read from, and names its evidence class.
// ---------------------------------------------------------------------------------------------

/// Binance USDⓈ-M futures — `crates/bridges/binance/src/perp.rs`'s `modify_order` sends
/// `PUT /fapi/v1/order` addressed by `origClientOrderId`, carrying `quantity`.
///
/// VENDOR DOC, on the exact case: the Modify Order endpoint documents the amend of a partially
/// filled order as failing "when the order is in partially filled status and the new `quantity` <=
/// `executedQty`" — a rule that only means anything if `quantity` is the order's TOTAL, executed
/// part included. The response carries the SAME `orderId`. Corroborated in-tree: an OPEN binance
/// order is reported WITH its accumulated execution (`crates/bridges/binance/src/family/recon.rs`
/// reads `executedQty` into `filled_qty`), i.e. executions stay attached to a still-resting order.
///
/// ⚠ **THE EVIDENCE IS PERP-ONLY, AND THE KEY IS NOT.** `BinanceExecutionClient` routes spot vs
/// perp internally (`crates/bridges/binance/src/exec.rs`'s `split_symbol`, on the `.P` suffix) while
/// this table is keyed on the bare string `"binance"`. The row is sound today because binance SPOT
/// has no amend at all — `crates/bridges/binance/src/spot.rs`'s `BinanceSpotRest` takes `VenueRest`'s
/// DEFAULT no-op `modify_order`, so a spot amend never reaches a venue and this value cannot move a
/// real order. Binance spot DOES publish `POST /api/v3/order/cancelReplace`, which is
/// [`AmendSemantics::CancelReplace`] — the ANTI-conservative direction for this row. **Wiring it
/// means splitting this row per product before the wiring lands, not after.**
/// `crates/bridges/binance/tests/offline/amend_semantics_scope.rs`'s
/// `spot_has_no_native_amend_so_the_binance_amend_row_stays_perp_only` is the tripwire that fires
/// the moment that override appears.
pub const BINANCE: AmendSemantics = AmendSemantics::InPlaceTotal;

/// OKX SWAP — `crates/bridges/okx/src/perp.rs`'s `modify_order` sends
/// `POST /api/v5/trade/amend-order` addressed by `clOrdId`, carrying `newSz` (in contracts).
///
/// VENDOR DOC, on the exact case: amending a partially-filled order with a `newSz` less than or
/// equal to the already-filled quantity moves the order to FILLED — again a rule that presumes
/// `newSz` is the total. The reply carries the same `ordId`. Corroborated in-tree:
/// `crates/bridges/okx/src/recon_client.rs` reads `acc_fill_sz` off an OPEN order.
pub const OKX: AmendSemantics = AmendSemantics::InPlaceTotal;

/// Bybit V5 linear perp — `crates/bridges/bybit/src/perp.rs`'s `modify_order` sends
/// `POST /v5/order/amend` addressed by `orderLinkId`, carrying `qty`.
///
/// VENDOR DOC: `qty` is documented as "Order quantity after modification", a partially filled order
/// is explicitly amendable, and the ack returns the ORIGINAL `orderId`/`orderLinkId`. Corroborated
/// in-tree: `crates/bridges/bybit/src/recon_client.rs` reads `cumExecQty` off an OPEN order, so
/// executions demonstrably survive on a resting bybit order.
pub const BYBIT: AmendSemantics = AmendSemantics::InPlaceTotal;

/// Hyperliquid — the one row that needs no vendor doc. `crates/bridges/hyperliquid/src/exec.rs`'s
/// `modify` builds a whole replacement order out of the amended terms, submits it as a single
/// `ModifyWire`, then reads the NEW oid out of the reply (`parse_first_resting_oid`), swaps it in
/// under the same cloid and retires the old one. A replacement order carries no execution history,
/// so `position + new_qty` — the arithmetic every other arm keeps — is the CORRECT projection here.
pub const HYPERLIQUID: AmendSemantics = AmendSemantics::CancelReplace;

/// Aster perp — HELD CONSERVATIVELY. `crates/bridges/aster/src/perp.rs`'s `modify_order` is a
/// binance-fapi clone by shape (`PUT /fapi/v3/order`, `origClientOrderId` + `quantity`), and its own
/// doc says so — but a FORK's shape is an analogy, not this venue's statement about its own
/// partially-filled amend, and the same reasoning already holds
/// `crate::venue_margin_support`'s `ASTER` row conservative. Aster additionally runs against
/// MAINNET in practice (no testnet credentials are configured, see the root `CLAUDE.md`), so a
/// wrong `InPlaceTotal` here would be anti-conservative on a REAL account. Upgrade it with a demo
/// amend, not with the binance row.
pub const ASTER: AmendSemantics = AmendSemantics::Unknown;

/// cTrader — HELD CONSERVATIVELY. `crates/bridges/ctrader/src/exec.rs`'s `modify` enqueues a
/// `ProtoOaAmendOrderReq` carrying `volume` against an existing `order_id`, and the venue answers
/// with `ORDER_REPLACED`, which reads in-place — but the protobuf field name is not evidence and
/// nothing in tree establishes what an amend does to a partially filled cTrader pending order.
pub const CTRADER: AmendSemantics = AmendSemantics::Unknown;

/// Interactive Brokers — HELD CONSERVATIVELY. `crates/bridges/vike-ibkr/src/lib.rs`'s `modify`
/// forwards to the actor's transport `modify_order`, which re-places under the SAME order id (the
/// TWS convention), so this is very likely in place — but "very likely" is the wrong standard for
/// the one arm that can admit an order the gate should refuse.
pub const IBKR: AmendSemantics = AmendSemantics::Unknown;

/// Deribit — no native amend: `crates/bridges/deribit/src/exec.rs` takes `ExecutionClient`'s
/// default no-op `modify`, pinned by its own `modify_is_default_noop_for_deribit`.
pub const DERIBIT: AmendSemantics = AmendSemantics::Unsupported;

/// OANDA — no `modify` override in the adapter; the default no-op applies.
pub const OANDA: AmendSemantics = AmendSemantics::Unsupported;

/// IG — no `modify` override in the adapter; the default no-op applies.
pub const IG: AmendSemantics = AmendSemantics::Unsupported;

/// FXCM — no `modify` override in the adapter; the default no-op applies.
pub const FXCM: AmendSemantics = AmendSemantics::Unsupported;

/// Dukascopy — no `modify` override; the JForex sidecar protocol carries no amend command, so an
/// amend never leaves the process.
pub const DUKASCOPY: AmendSemantics = AmendSemantics::Unsupported;

/// Polymarket CLOB — no `modify` override; a re-quote is cancel-then-submit at the caller, which
/// produces a NEW order with its own coid and never reaches this axis.
pub const POLYMARKET: AmendSemantics = AmendSemantics::Unsupported;

/// Alpaca — no `modify` override in the adapter; the default no-op applies.
pub const ALPACA: AmendSemantics = AmendSemantics::Unsupported;

// vike:new-venue:row /// TODO(new-venue: {venue}): `Unsupported` is the scaffolded value because a fresh bridge wires no
// vike:new-venue:row /// `modify` override, and `amend_semantics_agrees_with_venue_caps_supports_modify` requires
// vike:new-venue:row /// exactly that while `venue_caps` says `supports_modify: false`. Move BOTH together.
// vike:new-venue:row pub const {VENUE}: AmendSemantics = AmendSemantics::Unsupported;
// vike:new-venue:row
/// The registry: the declared [`AmendSemantics`] for a canonical venue string — one arm per
/// [`crate::venues::VENUES`] entry, mirroring `crate::venue_margin_support`'s
/// `venue_margin_support`. An unrecognised venue returns [`AmendSemantics::Unknown`], the
/// conservative arithmetic.
///
/// ⚠ **A PAPER MOUNT DOES NOT LAND IN THAT FALLBACK ARM** — it carries the real venue string, so it
/// is the CLIENT's `vike_exec::ExecutionClient::amend_semantics` that keeps it conservative, not
/// this function. The module doc's "the paper exchange" section is the authority.
///
/// [`AmendSemantics::InPlaceRemaining`] is deliberately unreachable here: no venue declares it, and
/// `the_registry_never_serves_the_client_only_row` pins that.
#[must_use]
pub fn amend_semantics(venue: &str) -> AmendSemantics {
    match venue {
        "binance" => BINANCE,
        "bybit" => BYBIT,
        "okx" => OKX,
        "deribit" => DERIBIT,
        "oanda" => OANDA,
        "ig" => IG,
        "fxcm" => FXCM,
        "dukascopy" => DUKASCOPY,
        "polymarket" => POLYMARKET,
        "ibkr" => IBKR,
        "ctrader" => CTRADER,
        "alpaca" => ALPACA,
        "aster" => ASTER,
        "hyperliquid" => HYPERLIQUID,
        // vike:new-venue:row "{venue}" => {VENUE}, // TODO(new-venue: {venue}): a NAMED row, never the Unknown fallback
        _ => AmendSemantics::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every registry venue paired with its declared const — the completeness test below asserts
    /// this covers `crate::venues::VENUES` exactly.
    ///
    /// `#[rustfmt::skip]`: a `just new-venue` marker at the TAIL of a bracketed literal is
    /// re-indented by rustfmt once a row ending in a trailing `//` comment is generated above it,
    /// which defeats `--remove`. Gated by `crates/vike-ops/tests/new_venue_gate.rs`'s
    /// `a_trailing_comment_marker_is_rustfmt_skipped_unless_a_recognised_sibling_follows`.
    #[rustfmt::skip]
    const MATRIX: &[(&str, AmendSemantics)] = &[
        ("binance", BINANCE),
        ("bybit", BYBIT),
        ("okx", OKX),
        ("deribit", DERIBIT),
        ("oanda", OANDA),
        ("ig", IG),
        ("fxcm", FXCM),
        ("dukascopy", DUKASCOPY),
        ("polymarket", POLYMARKET),
        ("ibkr", IBKR),
        ("ctrader", CTRADER),
        ("alpaca", ALPACA),
        ("aster", ASTER),
        ("hyperliquid", HYPERLIQUID),
        // vike:new-venue:row ("{venue}", {VENUE}), // TODO(new-venue: {venue}): declared-row completeness
    ];

    /// The full matrix, pinned verbatim (the `venue_margin_support` idiom). Any drift in a row is
    /// loud here — and a row moving INTO `InPlaceTotal` is the only kind of drift that can change
    /// an admit/deny verdict anywhere.
    #[test]
    fn amend_matrix_is_pinned() {
        use AmendSemantics::{CancelReplace, InPlaceTotal, Unknown, Unsupported};
        #[rustfmt::skip]
        let rows: &[(&str, AmendSemantics)] = &[
            // in-place amend: the qty is the order's new TOTAL, executions stay attached
            ("binance",     InPlaceTotal),
            ("okx",         InPlaceTotal),
            ("bybit",       InPlaceTotal),
            // native cancel-replace (proven from the adapter's own new-oid swap)
            ("hyperliquid", CancelReplace),
            // amendable, convention NOT established — held conservative
            ("aster",       Unknown),
            ("ctrader",     Unknown),
            ("ibkr",        Unknown),
            // no native amend at all (the ExecutionClient default no-op)
            ("deribit",     Unsupported),
            ("oanda",       Unsupported),
            ("ig",          Unsupported),
            ("fxcm",        Unsupported),
            ("dukascopy",   Unsupported),
            ("polymarket",  Unsupported),
            ("alpaca",      Unsupported),
            // vike:new-venue:row ("{venue}",  Unsupported), // TODO(new-venue: {venue}): move with the const above
        ];
        for (venue, want) in rows {
            assert_eq!(amend_semantics(venue), *want, "{venue}");
        }
        assert_eq!(rows.len(), MATRIX.len(), "matrix test row count == registry size");
    }

    /// Completeness vs the canonical roster: every `crate::venues::VENUES` entry has a NAMED
    /// declared row, and the registry serves exactly it. Adding a venue fails here until its row
    /// exists — including when the honest answer is [`AmendSemantics::Unknown`], which must be
    /// DECLARED rather than inherited from the fallback arm.
    #[test]
    fn every_roster_venue_has_a_declared_row() {
        assert_eq!(MATRIX.len(), crate::venues::VENUES.len(), "one declared row per roster venue");
        for &v in crate::venues::VENUES {
            let (_, want) = MATRIX
                .iter()
                .find(|(rv, _)| *rv == v)
                .unwrap_or_else(|| panic!("no AmendSemantics row declared for roster venue {v}"));
            assert_eq!(amend_semantics(v), *want, "{v}: registry must serve its declared row");
        }
    }

    /// THE SAFETY PROPERTY, stated as a test rather than as prose: exactly one variant subtracts
    /// anything, so a row misclassified among the other three cannot move a single verdict. This is
    /// what makes an `Unknown` row cost nothing and an `InPlaceTotal` row the only one needing
    /// evidence.
    #[test]
    fn only_in_place_total_subtracts_anything() {
        assert_eq!(AmendSemantics::InPlaceTotal.already_in_position(4.0), 4.0);
        for s in [
            AmendSemantics::CancelReplace,
            AmendSemantics::InPlaceRemaining,
            AmendSemantics::Unsupported,
            AmendSemantics::Unknown,
        ] {
            assert_eq!(s.already_in_position(4.0), 0.0, "{s:?} must keep today's arithmetic");
        }
    }

    /// [`AmendSemantics::InPlaceRemaining`] is a CLIENT-declared row, never a venue one: the paper
    /// exchange is the only thing in this workspace that implements it. A venue string that started
    /// resolving to it would mean somebody keyed a client fact onto the venue axis.
    #[test]
    fn the_registry_never_serves_the_client_only_row() {
        for &v in crate::venues::VENUES {
            assert_ne!(
                amend_semantics(v),
                AmendSemantics::InPlaceRemaining,
                "{v}: InPlaceRemaining is declared by the CLIENT, not by this table"
            );
        }
        for v in ["sim", "paper", ""] {
            assert_ne!(amend_semantics(v), AmendSemantics::InPlaceRemaining, "{v}");
        }
    }

    /// A garbage `filled_qty` yields `0.0` — the conservative answer — rather than propagating.
    /// A NaN subtracted from an order qty makes `projected > cap` FALSE, which would vacate the
    /// ceiling this value exists to tighten.
    #[test]
    fn a_non_finite_or_negative_filled_qty_subtracts_nothing() {
        for bad in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0, -0.0, 0.0] {
            assert_eq!(
                AmendSemantics::InPlaceTotal.already_in_position(bad),
                0.0,
                "filled_qty {bad} must subtract nothing"
            );
        }
    }

    /// An unrecognised venue is [`AmendSemantics::Unknown`], i.e. byte-identical to the behaviour
    /// before this table existed. ⚠ This is NOT the paper-mount story: a paper mount carries the
    /// REAL venue string (`vike_mount::make_engine`), so it never reaches this arm — see
    /// `the_registry_never_serves_the_client_only_row` and the module doc.
    #[test]
    fn an_unknown_venue_is_conservative() {
        for v in ["sim", "paper", "nasdaq", ""] {
            assert_eq!(amend_semantics(v), AmendSemantics::Unknown, "{v}");
            assert_eq!(amend_semantics(v).already_in_position(9.0), 0.0, "{v}");
        }
        assert_eq!(AmendSemantics::default(), AmendSemantics::Unknown);
    }

    /// The in-place set is exactly the three venues whose vendor doc speaks to the partially-filled
    /// case. Spelled as an explicit both-directions assertion so that adding a fourth venue to the
    /// anti-conservative arm cannot be a one-line diff nobody notices.
    #[test]
    fn the_in_place_set_is_exactly_the_evidence_backed_three() {
        let in_place: Vec<&str> = crate::venues::VENUES
            .iter()
            .copied()
            .filter(|v| amend_semantics(v) == AmendSemantics::InPlaceTotal)
            .collect();
        assert_eq!(in_place, vec!["binance", "bybit", "okx"]);
    }

    /// ⚠ **THE SECOND TABLE ON THIS AXIS.** `crate::venue_caps`'s `supports_modify` already declares
    /// per venue whether an amend exists at all, and nothing tied the two together — the exact shape
    /// this tree has been burned by (`caps.rs` drifted three times while every declaration-pinning
    /// test stayed green). So they are cross-checked, BOTH directions:
    ///
    /// * `supports_modify == false` ⇒ [`AmendSemantics::Unsupported`] — there is no convention to
    ///   declare when no amend leaves the process;
    /// * `supports_modify == true` ⇒ anything BUT `Unsupported` — a venue that amends has a
    ///   convention, even when the honest value for it is `Unknown`.
    ///
    /// `CAPS_DISAGREEMENTS` is the declared-exception list, and it exists because the two tables are
    /// keyed differently: `caps_for` resolves a per-BACKEND row this table has no axis for.
    #[test]
    fn amend_semantics_agrees_with_venue_caps_supports_modify() {
        /// (venue, why the two tables legitimately differ). ⚠ A row here is a promise that the
        /// disagreement was REASONED, not that it is harmless — read the reason before adding one.
        const CAPS_DISAGREEMENTS: &[(&str, &str)] = &[(
            // `caps_for("ibkr")` returns the SOCKET row, whose backend has no native amend;
            // `crate::venue_caps`'s `IBKR_CPAPI` sets `supports_modify: true` for the Client Portal
            // backend, and `vike_ibkr::caps_for_backend` picks between them at runtime. This table
            // is keyed on the bare venue string and has no backend axis, so it must answer for the
            // amending backend — `Unknown`, held conservative, is that answer.
            "ibkr",
            "caps_for returns the socket row; the cpapi backend (IBKR_CPAPI) does amend",
        )];
        for &v in crate::venues::VENUES {
            let sem = amend_semantics(v);
            let modifiable = crate::venue_caps::caps_for(v).supports_modify;
            if CAPS_DISAGREEMENTS.iter().any(|(rv, _)| *rv == v) {
                assert!(
                    !modifiable && sem != AmendSemantics::Unsupported,
                    "{v}: declared as a caps disagreement, but the tables now agree — delete the \
                     CAPS_DISAGREEMENTS row"
                );
                continue;
            }
            if modifiable {
                assert_ne!(
                    sem,
                    AmendSemantics::Unsupported,
                    "{v}: venue_caps says it amends, so its amend convention cannot be Unsupported"
                );
            } else {
                assert_eq!(
                    sem,
                    AmendSemantics::Unsupported,
                    "{v}: venue_caps says it has no amend, so there is no convention to declare"
                );
            }
        }
    }

    #[test]
    fn amend_semantics_serde_round_trips() {
        for s in [
            AmendSemantics::InPlaceTotal,
            AmendSemantics::CancelReplace,
            AmendSemantics::InPlaceRemaining,
            AmendSemantics::Unsupported,
            AmendSemantics::Unknown,
        ] {
            let json = serde_json::to_string(&s).expect("serialize");
            let back: AmendSemantics = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, s);
        }
        assert_eq!(
            serde_json::to_string(&AmendSemantics::InPlaceTotal).unwrap(),
            "\"InPlaceTotal\""
        );
    }
}
