//! FXCM's order-execution error codes, as a [`VenueTaxonomy`]: every code FXCM's list documents.
//!
//! Source: <https://www.fxcm.com/public-docs/fclite/commonresources/OrdersSys/ReferenceMaterials.OrderExecutionErrors.html>
//! (44 codes, `20001..20222`, a code and a description each; the ForexConnect SDK reference is gone). On the wire a code
//! arrives INSIDE `onRequestFailed`'s text as `ORA-<n>` (the shim's `-6`; `tests/fixtures/order_permission_probe.json`), so
//! `crate::execution::event_mapper::ora_code` reads it out once, at the bridge's edge, and every decision is this table's
//! (decision 0301). The kinds are the S4b research's proposals over the list's descriptions; five codes were measured on
//! the demo (`20102`, `20105`, `20114`, `20115`, `20134`).
//!
//! **A write's fate** ([`TAXONOMY`]'s [`VenueTaxonomy::submit_outcome_unknown`], read by
//! `crate::execution::event_mapper::venue_failure_outcome`): `20001` "Internal system error", generated while the order
//! was processed, leaves the write OUTCOME UNKNOWN ([`server_had_trouble`]), and so does a code the list does not name
//! (decision 0303); `20131`, the account locked or in the daily rollover, is NOT NOW; every other row is the venue's
//! refusal. **A cancel** of an order the venue no longer holds ([`cancel_says_gone`]) is no refusal at all: the order
//! filled or was cancelled, and its own event says which (`crate::transport::sys::FxcmSession::delete_order`).

use vike_bridge_core::error_kind::VenueTaxonomy;
use vike_bridge_core::transport::ErrorKind;

/// FXCM's documented codes → [`ErrorKind`]. `None` = not on FXCM's list: an unmapped code. Each comment is the list's
/// description, cut short.
pub(crate) fn by_code(code: i64) -> Option<ErrorKind> {
    Some(match code {
        20001 => ErrorKind::ServerError, // internal system error (the order may stand: `server_had_trouble`)
        20008 => ErrorKind::InvalidRequest, // missing or invalid parameter (instrument, type, amount, range)
        20102 => ErrorKind::Auth, // Access Violation: the order type is not allowed for the trader
        20105 => ErrorKind::InvalidRequest, // Order price did not pass validation (too close to the market)
        20112 => ErrorKind::InvalidRequest, // no tradable price / market price outside the range
        20113 => ErrorKind::InsufficientFunds, // not enough margin to open a position
        20114 => ErrorKind::InvalidRequest, // more than one order of this type within the order group
        20115 => ErrorKind::InvalidRequest, // Stop/Limit price did not pass validation
        20120 => ErrorKind::NotFound,       // conditional order expired
        20121 => ErrorKind::InvalidRequest, // conditional order activated
        20123 => ErrorKind::Auth,           // order from a closed account
        20126 => ErrorKind::InvalidRequest, // order during market close hours
        20131 => ErrorKind::VenueMaintenance, // account locked or under rollover: not now
        20132 => ErrorKind::InsufficientFunds, // account under margin call limitation
        20134 => ErrorKind::InvalidRequest, // quantity should be the same as in the trade
        20136 => ErrorKind::InvalidRequest, // order being executed
        20143 => ErrorKind::InvalidRequest, // price beyond the allowed distance from the market
        20148 => ErrorKind::InsufficientFunds, // account under equity alert limitation
        20149 => ErrorKind::InsufficientFunds, // account under equity stop limitation
        20168 => ErrorKind::NotFound, // position / entry order / closing reference does not exist
        20169 => ErrorKind::NotFound, // conditional order to be changed does not exist
        20172 => ErrorKind::InvalidRequest, // amount below the minimum
        20173 => ErrorKind::InvalidRequest, // amount is not a whole number of lots
        20177 => ErrorKind::InvalidRequest, // contingent type unsupported or incorrect
        20178 => ErrorKind::InvalidRequest, // ELS contingent order on a hedging account
        20179 => ErrorKind::NotFound, // stop/limit references a wrong or missing position
        20180 => ErrorKind::InvalidRequest, // time in force incorrect or unsupported
        20181 => ErrorKind::InvalidRequest, // stop/limit for a position already closing
        20183 => ErrorKind::InvalidRequest, // contingent type differs from the contingent order's
        20184 => ErrorKind::InvalidRequest, // an existing stop/limit is activated
        20185 => ErrorKind::NotFound, // cannot close an already closed position
        20186 => ErrorKind::NotFound, // order with the specified id does not exist
        20187 => ErrorKind::InvalidRequest, // stop/limit has the same trade operation as the position
        20188 => ErrorKind::InvalidRequest, // OCO reference missing or incorrect
        20189 => ErrorKind::InvalidRequest, // OTO/OTOCO reference missing or incorrect
        20190 => ErrorKind::InvalidRequest, // primary OTO order is executing
        20191 => ErrorKind::InvalidRequest, // entry order is executing
        20192 => ErrorKind::InvalidRequest, // stop/limit price missing
        20193 => ErrorKind::InvalidRequest, // secondary price must be a peg when the primary trails
        20194 => ErrorKind::InvalidRequest, // peg price not allowed here
        20195 => ErrorKind::InvalidRequest, // entry order specified as closing
        20198 => ErrorKind::InvalidRequest, // same trade operation as the position(s) to close
        20199 => ErrorKind::InvalidRequest, // request already used with another order
        20222 => ErrorKind::InvalidRequest, // closing order type not allowed under NFA FIFO rules
        _ => return None,
    })
}

/// Does this code say only that FXCM had trouble while it processed the write, so that the write may stand? `20001`
/// alone. It makes [`TAXONOMY`]'s [`VenueTaxonomy::submit_outcome_unknown`] with the unmapped codes.
pub(crate) fn server_had_trouble(code: i64) -> bool {
    code == 20001
}

/// Does a CANCEL answered `code` say the venue no longer holds the order? `20186` (no order with that id) and `20169`
/// (no conditional order to change): it filled or was cancelled already, which only its own event tells apart.
pub(crate) fn cancel_says_gone(code: i64) -> bool {
    matches!(code, 20169 | 20186)
}

/// FXCM's taxonomy. Its codes are specific, so it reads no message.
pub(crate) const TAXONOMY: VenueTaxonomy = VenueTaxonomy {
    venue: "fxcm",
    by_code,
    by_msg: |_| None,
    leaves_write_open: server_had_trouble,
};

#[cfg(test)]
mod tests {
    use super::*;
    use vike_bridge_core::error_kind::classify_venue;
    use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, VenueApiError};

    /// The codes `by_code` is walked over: FXCM's order-execution range.
    fn walked() -> impl Iterator<Item = i64> {
        20000..21000
    }

    /// A write's fate is open on the sentinel, on `20001` and on a code the list does not name, and on no other row.
    #[test]
    fn the_write_rule_is_the_sentinel_the_internal_error_and_the_unmapped_codes() {
        assert!(TAXONOMY.submit_outcome_unknown(E_TIMEOUT_AMBIGUOUS));
        assert!(TAXONOMY.submit_outcome_unknown(20103), "not on FXCM's list");
        let open: Vec<i64> = walked()
            .filter(|&code| by_code(code).is_some() && TAXONOMY.submit_outcome_unknown(code))
            .collect();
        assert_eq!(open, [20001]);
        let kind = |code| classify_venue(Some(&TAXONOMY), &VenueApiError { code, msg: "m".into() });
        assert_eq!(kind(20131), ErrorKind::VenueMaintenance, "the rollover: not now");
        assert_eq!(kind(20186), ErrorKind::NotFound);
        assert_eq!(kind(20103), ErrorKind::UnmappedCode);
    }

    /// Only "the order does not exist" says a cancel found nothing; every such code is a `NotFound` row.
    #[test]
    fn a_cancel_is_gone_on_the_two_does_not_exist_codes_only() {
        let gone: Vec<i64> = walked().filter(|&code| cancel_says_gone(code)).collect();
        assert_eq!(gone, [20169, 20186]);
        assert!(gone.iter().all(|&code| by_code(code) == Some(ErrorKind::NotFound)));
    }

    /// Every row has a venue answer recorded in this repository behind it, or is named here as having none yet (the
    /// list only shrinks: a row given a sample leaves it). The four samples are the demo's measured words.
    #[test]
    fn every_row_has_a_recorded_sample_or_is_listed_without_one() {
        use vike_bridge_core::error_table::RowSample;
        vike_bridge_core::error_table::assert_every_row_has_a_sample(
            &TAXONOMY,
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."),
            walked(),
            &[
                RowSample {
                    code: 20102,
                    words: "Access Violation",
                    recorded_in: "crates/bridges/fxcm/tests/fixtures/order_permission_probe.json",
                },
                RowSample {
                    code: 20105,
                    words: "Order price did not pass validation.",
                    recorded_in: "crates/bridges/fxcm/src/transport/sys_tests.rs",
                },
                RowSample {
                    code: 20114,
                    words: "Cannot place more than one order of this type within order group",
                    recorded_in: "crates/bridges/fxcm/src/execution/event_mapper_tests.rs",
                },
                RowSample {
                    code: 20115,
                    words: "Stop/Limit price did not pass validation",
                    recorded_in: "crates/bridges/fxcm/src/execution/execution_tests.rs",
                },
            ],
            &[
                20001, 20008, 20112, 20113, 20120, 20121, 20123, 20126, 20131, 20132, 20134, 20136,
                20143, 20148, 20149, 20168, 20169, 20172, 20173, 20177, 20178, 20179, 20180, 20181,
                20183, 20184, 20185, 20186, 20187, 20188, 20189, 20190, 20191, 20192, 20193, 20194,
                20195, 20198, 20199, 20222,
            ],
        );
    }
}
