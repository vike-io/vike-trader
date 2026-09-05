//! `AttributionMechanic` — the per-venue order-attribution capability table (broker/builder codes).
//!
//! The unified twin of [`crate::fees::fee_schedule_for`] and [`crate::venue_caps::caps_for`],
//! living here in `vike-model` (the bottom layer) so every bridge crate reaches DOWN to it. It
//! declares, per venue, HOW an attribution code rides an order — because the wire mechanic is
//! irreducibly per-venue (client-order-id prefix vs HTTP header vs order tag vs signed field), the
//! table does not abstract the stamping (each adapter does that); it exists to (a) force the
//! "does this venue attribute?" decision for every roster venue via the completeness test, and
//! (b) declare the per-venue constraints (tag/coid length caps, on-chain-approval requirement) in
//! one auditable place. Absent a configured code the adapters stamp nothing — byte-identical.
//!
//! Verified 2026-07-25 (see `docs/research/2026-07-25-venue-attribution-unified/README.md`):
//! 6 venues stamp on the order (binance/bybit/okx/aster/hyperliquid/polymarket); 8 have only
//! account/relationship-level programs (deribit — its `label` is a user tag, not a broker code —
//! plus the FX/CFD/equity venues).

/// How a venue carries an attribution code on an order. `Copy` (scalar/`'static` fields).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttributionMechanic {
    /// Prefix on the client order id — Binance `x-<LinkID>-<coid>`. `max_total_len` is the venue's
    /// `newClientOrderId` ceiling the whole prefixed id must fit under (Binance: 36).
    CoidPrefix { max_total_len: usize },
    /// An HTTP header carrying the code on every order request — Bybit `X-Referer`.
    Header { name: &'static str },
    /// A dedicated order field/param — OKX `tag`. `max_len` is the venue's cap; `field` the wire key.
    OrderTag { field: &'static str, max_len: usize },
    /// A code signed into the order/action — Polymarket bytes32 `builder`, HL/Aster builder
    /// address+fee. `needs_onchain_approval` is true where the venue requires a one-time
    /// `approveBuilderFee`/`approveBuilder` action before it accepts a nonzero builder fee
    /// (Hyperliquid, Aster); false where the code is a bare signed field (Polymarket).
    SignedBuilder { needs_onchain_approval: bool },
    /// No order-level attribution mechanism (account/relationship/white-label level only).
    None,
}

impl AttributionMechanic {
    /// True for the `None` variant — the classifier the completeness test reads.
    pub fn is_none(&self) -> bool {
        matches!(self, AttributionMechanic::None)
    }

    /// Reject a code that cannot ride this venue's mechanic. An empty code is always invalid
    /// (callers treat absent-from-`.env` as unset, never as an empty stamp). `None` accepts nothing.
    pub fn validate_code(&self, code: &str) -> Result<(), String> {
        if code.is_empty() {
            return Err("empty attribution code".to_string());
        }
        match self {
            // `x-<code>-<coid>` must fit `max_total_len`. Reserve room for a usable client-order-id
            // (the generator emits `<8-hex-session><seq>`, ~16 chars); the code gets what's left after
            // `x-` (2) + `-` (1) + the reserved coid. Task 6 truncates the coid TAIL to fit, so this only
            // rejects a code so long it would leave no usable coid.
            AttributionMechanic::CoidPrefix { max_total_len } => {
                const RESERVED_COID: usize = 16;
                let budget = max_total_len.saturating_sub(2 + 1 + RESERVED_COID);
                if code.len() > budget {
                    return Err(format!(
                        "code len {} exceeds coid-prefix budget {budget}",
                        code.len()
                    ));
                }
                Ok(())
            }
            AttributionMechanic::OrderTag { max_len, .. } => {
                if code.len() > *max_len {
                    return Err(format!("code len {} exceeds tag max_len {max_len}", code.len()));
                }
                Ok(())
            }
            AttributionMechanic::Header { .. } | AttributionMechanic::SignedBuilder { .. } => {
                Ok(())
            }
            AttributionMechanic::None => Err("venue has no order-level attribution".to_string()),
        }
    }
}

/// The per-venue attribution registry. Twin of [`crate::fees::fee_schedule_for`]; an unknown venue
/// is [`AttributionMechanic::None`] (fail-closed: never invent an attribution surface). Every ROSTER
/// venue is a NAMED arm — `every_roster_venue_is_classified` enforces it.
pub fn attribution_for(venue: &str) -> AttributionMechanic {
    match venue {
        // Binance Broker/Link Program: `x-<LinkID>` prefix on newClientOrderId, whole id < 36.
        "binance" => AttributionMechanic::CoidPrefix { max_total_len: 36 },
        // Bybit API Broker Program: `X-Referer` (a.k.a. `Referer`) header carrying the Broker ID.
        "bybit" => AttributionMechanic::Header { name: "X-Referer" },
        // OKX Fully Disclosed Broker: the order `tag` field (case-sensitive alnum, ~16 char cap).
        "okx" => AttributionMechanic::OrderTag { field: "tag", max_len: 16 },
        // Aster "Aster Code": builder address + feeRate on /fapi/v3/order after one-time approveBuilder.
        "aster" => AttributionMechanic::SignedBuilder { needs_onchain_approval: true },
        // Hyperliquid Builder Codes: signed action `builder:{b,f}` after one-time approveBuilderFee.
        "hyperliquid" => AttributionMechanic::SignedBuilder { needs_onchain_approval: true },
        // Polymarket Builders Program: bytes32 `builderCode` in the signed CLOB V2 order — no
        // separate on-chain approval (the code is a bare signed field).
        "polymarket" => AttributionMechanic::SignedBuilder { needs_onchain_approval: false },
        // Deribit: `label` is a USER order tag (cancel_by_label), NOT a broker code; attribution is
        // account-level (affiliate/IB/partner). FX/CFD/equity: IB/white-label programs, no per-order
        // tag. All classified None on purpose (a NAMED arm, not the `_` fallback).
        "deribit" | "oanda" | "ig" | "fxcm" | "dukascopy" | "ibkr" | "ctrader" | "alpaca" => {
            AttributionMechanic::None
        }
        // vike:new-venue:row // TODO(new-venue: {venue}): does this venue run a broker/builder programme? A NAMED `None`
        // vike:new-venue:row // arm says "asked and answered"; the `_` fallback below says nothing at all.
        // vike:new-venue:row "{venue}" => AttributionMechanic::None,
        _ => AttributionMechanic::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_the_verified_mechanics() {
        assert_eq!(
            attribution_for("binance"),
            AttributionMechanic::CoidPrefix { max_total_len: 36 }
        );
        assert_eq!(attribution_for("bybit"), AttributionMechanic::Header { name: "X-Referer" });
        assert_eq!(
            attribution_for("okx"),
            AttributionMechanic::OrderTag { field: "tag", max_len: 16 }
        );
        assert_eq!(
            attribution_for("aster"),
            AttributionMechanic::SignedBuilder { needs_onchain_approval: true }
        );
        assert_eq!(
            attribution_for("hyperliquid"),
            AttributionMechanic::SignedBuilder { needs_onchain_approval: true }
        );
        assert_eq!(
            attribution_for("polymarket"),
            AttributionMechanic::SignedBuilder { needs_onchain_approval: false }
        );
        for v in
            ["deribit", "oanda", "ig", "fxcm", "dukascopy", "ibkr", "ctrader", "alpaca", "nope"]
        {
            assert_eq!(attribution_for(v), AttributionMechanic::None, "{v}");
        }
    }

    #[test]
    fn validate_code_enforces_venue_limits() {
        // OKX tag: <=16 chars.
        let okx = attribution_for("okx");
        assert!(okx.validate_code("5328c82e5542BCDE").is_ok());
        assert!(okx.validate_code("this_tag_is_far_too_long").is_err());
        // Binance coid prefix: `x-<code>-` plus a 32-char coid must fit 36 → code budget is tight.
        let bnb = attribution_for("binance");
        assert!(bnb.validate_code("ABC123").is_ok());
        assert!(bnb.validate_code("WAY_TOO_LONG_A_LINK_ID").is_err());
        // None never accepts a code.
        assert!(attribution_for("deribit").validate_code("anything").is_err());
        // empty code is always rejected (callers treat absent as unset, not empty-string)
        assert!(okx.validate_code("").is_err());
    }

    /// Completeness vs `crate::venues::VENUES`: every roster venue is classified exactly once —
    /// either an ON-ORDER mechanism or an explicit `None`. Same shape as fees' completeness test.
    #[test]
    fn every_roster_venue_is_classified() {
        const ON_ORDER: &[&str] =
            &["binance", "bybit", "okx", "aster", "hyperliquid", "polymarket"];
        #[rustfmt::skip]
        const NO_ATTRIBUTION: &[&str] = &[
            "deribit", "oanda", "ig", "fxcm", "dukascopy", "ibkr", "ctrader", "alpaca",
            // vike:new-venue:row "{venue}", // TODO(new-venue: {venue}): move to ON_ORDER if the venue stamps a code
        ];
        assert_eq!(
            ON_ORDER.len() + NO_ATTRIBUTION.len(),
            crate::venues::VENUES.len(),
            "every roster venue classified exactly once"
        );
        for &v in crate::venues::VENUES {
            let on = ON_ORDER.contains(&v);
            let none = NO_ATTRIBUTION.contains(&v);
            assert!(on ^ none, "roster venue {v} must be classified exactly once");
            assert_eq!(attribution_for(v).is_none(), none, "{v}");
        }
    }
}
