//! Deribit's JSON-RPC error codes, as a [`VenueTaxonomy`]: every code the venue documents for a request this
//! bridge sends.
//!
//! Sources: [1] the complete list, <https://docs.deribit.com/articles/errors#complete-rpc-error-codes-reference>
//! (read 2026-11-23), with the JSON-RPC 2.0 reserved `-32xxx` codes; [2] the order-management article
//! (`10050` `lock_time_exceeded`, absent from [1]); [3] the same page's "Common Error Codes" summary, for the ten
//! codes only it names (`10008`, `11000`-`11007`, `11009`, each refused by its words). Where the summary renames a
//! code of [1] (it calls `10002` `insufficient_funds`), [1] wins: the testnet's answers in this repository match it.
//! Left out: the endpoints the bridge never calls (wallet, subaccounts, 2FA, API-key edits, KYC, MMP, block RFQ).
//! Deribit's codes overlap Bybit's `10xxx` range, which is why they are read here and by no table shared across
//! venues (the old shared seed read `10004` `order_not_found` as Bybit's "auth").
//!
//! Every caller that decides on a deribit code reads it through [`TAXONOMY`]: the order-write rule
//! ([`VenueTaxonomy::submit_outcome_unknown`]: `crate::execution::rest`'s submit arms, its edit and its cancel) and
//! the start's failure kind (`crate::adapter`). A code this table does not map is `ErrorKind::UnmappedCode` (never
//! final), reported once at WARN ([`vike_bridge_core::error_kind::classify_venue`]).
//!
//! The codes that leave a write's fate open are the venue's own words, and no capture of any exists: `13888`
//! `timed_out` "Server did not manage to process request when it was valid (`valid_until`)" (this client sends no
//! `valid_until`, so it has no stated cause here either), and the server's trouble ([`server_had_trouble`]):
//! `10001` `error` (a general failure with no public detail), `11031` `other_error <Error>` (a rare error, its
//! detail in the words: an error, not a reject), `11094` `internal_server_error` "Some unhandled error on server",
//! `13008` `request_failed` (invalid input OR an internal failure, one code for both causes) and `13028`
//! `temporarily_unavailable` "The requested service is not responding or processing the response takes too long".
//! The page says of none whether the order may stand. `10040` `retry` and `10050` `lock_time_exceeded` say the
//! request was not processed, so they are a server error that leaves nothing open.

use vike_bridge_core::error_kind::VenueTaxonomy;
use vike_bridge_core::transport::ErrorKind;

/// Deribit's documented codes → [`ErrorKind`] (the module doc's sources). `None` = not mapped: an unmapped code.
pub fn by_code(code: i64) -> Option<ErrorKind> {
    Some(match code {
        // ---- JSON-RPC 2.0 [1] --------------------------------------------------------------
        -32700 => ErrorKind::InvalidRequest, // Parse error
        -32602 => ErrorKind::InvalidRequest, // Invalid params
        -32601 => ErrorKind::InvalidRequest, // Method not found
        -32600 => ErrorKind::InvalidRequest, // request entity too large (over 32 kB)
        -32000 => ErrorKind::InvalidRequest, // Missing params
        // ---- 10xxx [1] [2] --------------------------------------------------------------------
        10000 => ErrorKind::Auth,              // authorization_required
        10001 => ErrorKind::ServerError,       // error: some general failure (server trouble)
        10002 => ErrorKind::InvalidRequest,    // qty_too_low
        10003 => ErrorKind::InvalidRequest,    // order_overlap: self-trading not enabled
        10004 => ErrorKind::NotFound,          // order_not_found
        10005 => ErrorKind::InvalidRequest,    // price_too_low <Limit>
        10006 => ErrorKind::InvalidRequest,    // price_too_low4idx <Limit>: NOT bybit's rate limit
        10007 => ErrorKind::InvalidRequest,    // price_too_high <Limit>
        10009 => ErrorKind::InsufficientFunds, // not_enough_funds
        10010 => ErrorKind::NotFound,          // already_closed
        10011 => ErrorKind::InvalidRequest,    // price_not_allowed
        10012 => ErrorKind::InvalidRequest,    // book_closed
        10013 => ErrorKind::InvalidRequest,    // pme_max_total_open_orders <Limit>
        10014 => ErrorKind::InvalidRequest,    // pme_max_future_open_orders <Limit>
        10015 => ErrorKind::InvalidRequest,    // pme_max_option_open_orders <Limit>
        10016 => ErrorKind::InvalidRequest, // pme_max_future_open_orders_size: NOT bybit's server error
        10017 => ErrorKind::InvalidRequest, // pme_max_option_open_orders_size <Limit>
        10018 => ErrorKind::InvalidRequest, // non_pme_max_future_position_size <Limit>
        10019 => ErrorKind::Auth,           // locked_by_admin: the operator must act
        10020 => ErrorKind::InvalidRequest, // invalid_or_unsupported_instrument
        10021 => ErrorKind::InvalidRequest, // invalid_amount
        10022 => ErrorKind::InvalidRequest, // invalid_quantity
        10023 => ErrorKind::InvalidRequest, // invalid_price
        10024 => ErrorKind::InvalidRequest, // invalid_max_show
        10025 => ErrorKind::InvalidRequest, // invalid_order_id: NOT a gone code
        10026 => ErrorKind::InvalidRequest, // price_precision_exceeded
        10027 => ErrorKind::InvalidRequest, // non_integer_contract_amount
        crate::history::RATE_LIMIT_CODE => ErrorKind::RateLimited, // 10028 too_many_requests
        10029 => ErrorKind::InvalidRequest, // not_owner_of_order: NOT a gone code
        10030 => ErrorKind::InvalidRequest, // must_be_websocket_request
        10031 => ErrorKind::InvalidRequest, // invalid_args_for_instrument
        10032 => ErrorKind::InvalidRequest, // whole_cost_too_low
        10033 => ErrorKind::InvalidRequest, // not_implemented
        10034 => ErrorKind::InvalidRequest, // trigger_price_too_high
        10035 => ErrorKind::InvalidRequest, // trigger_price_too_low
        10036 => ErrorKind::InvalidRequest, // invalid_max_show_amount
        10037 => ErrorKind::InvalidRequest, // non_pme_total_short_options_positions_size <Limit>
        10038 => ErrorKind::InvalidRequest, // pme_max_risk_reducing_orders <Limit>
        10039 => ErrorKind::InsufficientFunds, // not_enough_funds_in_currency <Currency>
        10040 => ErrorKind::ServerError,    // retry: not processed, may be retried
        10041 => ErrorKind::VenueMaintenance, // settlement_in_progress: paused for seconds
        10043 => ErrorKind::InvalidRequest, // price_wrong_tick
        10044 => ErrorKind::InvalidRequest, // trigger_price_wrong_tick
        10045 => ErrorKind::InvalidRequest, // can_not_cancel_liquidation_order
        10046 => ErrorKind::InvalidRequest, // can_not_edit_liquidation_order
        10047 => ErrorKind::RateLimited,    // matching_engine_queue_full
        10048 => ErrorKind::InvalidRequest, // not_on_this_server
        10050 => ErrorKind::ServerError, // lock_time_exceeded [2]: timed out before its risk check
        10066 => ErrorKind::RateLimited, // too_many_concurrent_requests (public requests)
        10072 => ErrorKind::InvalidRequest, // disabled_while_position_lock (spot)
        // ---- 11xxx [1] ------------------------------------------------------------------------
        11008 => ErrorKind::NotFound,         // already_filled
        11013 => ErrorKind::InvalidRequest,   // max_spot_open_orders
        11021 => ErrorKind::InvalidRequest,   // post_only_price_modification_not_possible
        11022 => ErrorKind::InvalidRequest,   // max_spot_order_quantity
        11029 => ErrorKind::InvalidRequest,   // invalid_arguments
        11030 => ErrorKind::InvalidRequest,   // other_reject <Reason>: a rare reject
        11031 => ErrorKind::ServerError,      // other_error <Error>: a rare error (server trouble)
        11035 => ErrorKind::InvalidRequest,   // no_more_triggers <Limit>
        11036 => ErrorKind::InvalidRequest,   // invalid_trigger_price
        11037 => ErrorKind::InvalidRequest,   // outdated_instrument_for_IV_order
        11038 => ErrorKind::InvalidRequest,   // no_adv_for_futures
        11039 => ErrorKind::InvalidRequest,   // no_adv_postonly
        11041 => ErrorKind::InvalidRequest,   // not_adv_order
        11042 => ErrorKind::Auth,             // permission_denied
        11043 => ErrorKind::InvalidRequest,   // bad_argument
        11044 => ErrorKind::NotFound,         // not_open_order
        11045 => ErrorKind::InvalidRequest,   // invalid_event
        11046 => ErrorKind::InvalidRequest,   // outdated_instrument
        11047 => ErrorKind::InvalidRequest,   // unsupported_arg_combination
        11048 => ErrorKind::InvalidRequest,   // wrong_max_show_for_option
        11049 => ErrorKind::InvalidRequest,   // bad_arguments
        11050 => ErrorKind::InvalidRequest,   // bad_request
        11051 => ErrorKind::VenueMaintenance, // system_maintenance
        11052 => ErrorKind::InvalidRequest,   // subscribe_error_unsubscribed
        11054 => ErrorKind::InvalidRequest,   // post_only_reject
        11055 => ErrorKind::InvalidRequest,   // post_only_not_allowed
        11056 => ErrorKind::RateLimited, // unauthenticated_public_requests_temporarily_disabled
        11059 => ErrorKind::InvalidRequest, // iceberg_not_allowed
        11060 => ErrorKind::InvalidRequest, // not_supported_for_coinbase_routed_spot
        11094 => ErrorKind::ServerError, // internal_server_error (server trouble)
        11098 => ErrorKind::Auth,        // account_locked
        // ---- 12xxx, 13xxx [1] -----------------------------------------------------------------
        12003 => ErrorKind::RateLimited,      // login_over_limit
        12005 => ErrorKind::Auth,             // country_is_banned
        13004 => ErrorKind::Auth,             // invalid_credentials
        13008 => ErrorKind::ServerError,      // request_failed: invalid input OR internal failure
        13009 => ErrorKind::Auth, // unauthorized: the access token expired or was refused
        13010 => ErrorKind::InvalidRequest, // value_required
        13011 => ErrorKind::InvalidRequest, // value_too_short
        13012 => ErrorKind::Auth, // unavailable_in_subaccount
        13016 => ErrorKind::InvalidRequest, // invalid_input
        13018 => ErrorKind::InvalidRequest, // invalid_content_type
        13019 => ErrorKind::InvalidRequest, // orderbook_closed
        13020 => ErrorKind::NotFound, // not_found: an instrument the venue does not list
        13021 => ErrorKind::Auth, // forbidden
        13025 => ErrorKind::VenueMaintenance, // method_switched_off_by_admin
        13028 => ErrorKind::ServerError, // temporarily_unavailable (server trouble)
        13030 => ErrorKind::InvalidRequest, // mmp_trigger
        13031 => ErrorKind::Auth, // verification_required
        13032 => ErrorKind::InvalidRequest, // non_unique_order_label
        13035 => ErrorKind::RateLimited, // active_combo_limit_reached
        13036 => ErrorKind::VenueMaintenance, // unavailable_for_combo_books
        13503 => ErrorKind::VenueMaintenance, // unavailable
        13666 => ErrorKind::InvalidRequest, // request_cancelled_by_user: withdrawn before the book
        13777 => ErrorKind::InvalidRequest, // replaced: a later edit applied instead
        13778 => ErrorKind::Auth, // raw_subscriptions_not_available_for_unauthorized
        13888 => ErrorKind::Timeout, // timed_out: the write's fate is open
        13901 => ErrorKind::InvalidRequest, // no_more_oto_orders
        13922 => ErrorKind::InvalidRequest, // linked_order_type_not_supported_for_csr_spot
        // ---- named only by the "Common Error Codes" summary [3] -------------------------------
        10008 => ErrorKind::InvalidRequest, // price_too_low
        11000 => ErrorKind::NotFound,       // order_not_found
        11001 => ErrorKind::NotFound,       // order_closed
        11002 => ErrorKind::InvalidRequest, // order_in_liquidation
        11003 => ErrorKind::InvalidRequest, // price_out_of_range
        11004 => ErrorKind::InvalidRequest, // amount_too_small
        11005 => ErrorKind::InvalidRequest, // amount_too_large
        11006 => ErrorKind::InvalidRequest, // post_only_reject (the testnet answers 11054)
        11007 => ErrorKind::InvalidRequest, // reduce_only_reject (the testnet answers 11030)
        11009 => ErrorKind::InvalidRequest, // self_trade_reject
        _ => return None,
    })
}

/// Does this code say only that the SERVER had trouble, so that it says nothing about what became of a write?
/// `10001`, `11031`, `11094`, `13008` and `13028` (the module doc). With the transport's sentinel and the `13888`
/// row filed under [`ErrorKind::Timeout`], it makes [`TAXONOMY`]'s [`VenueTaxonomy::submit_outcome_unknown`]:
/// `true` asks by label (`crate::execution::rest::VenueRestDeribit`'s `resolve_ambiguous_submit`) or reads the
/// edited order back (its `settle_edit`), never a terminal reject, and so does an unmapped code; a clear refusal
/// (any other row) stays a reject.
pub fn server_had_trouble(code: i64) -> bool {
    matches!(code, 10001 | 11031 | 11094 | 13008 | 13028)
}

/// Deribit's taxonomy, ready to thread into [`vike_bridge_core::error_kind::classify_venue`]. Deribit's codes are
/// specific, so it reads no message.
pub const TAXONOMY: VenueTaxonomy = VenueTaxonomy {
    venue: "deribit",
    by_code,
    by_msg: |_| None,
    leaves_write_open: server_had_trouble,
};

#[cfg(test)]
mod tests {
    use super::*;
    use vike_bridge_core::error_kind::classify_venue;
    use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, VenueApiError};

    fn kind(code: i64) -> ErrorKind {
        classify_venue(Some(&TAXONOMY), &VenueApiError { code, msg: String::new() })
    }

    /// The codes `by_code` is walked over: the JSON-RPC range and deribit's own.
    fn walked() -> impl Iterator<Item = i64> {
        (-32768..=-32000i64).chain(10000..14000)
    }

    /// A write's fate is open on the sentinel, the `13888` row and the five server-trouble codes, and on no other
    /// row: `10001` `error`, `11031` `other_error` and `13008` `request_failed` used to end a submit in a terminal
    /// reject. `10040` `retry` and `10050` `lock_time_exceeded` say the request was not processed. A code the page
    /// does not name leaves it open too.
    #[test]
    fn the_write_rule_is_the_sentinel_and_the_open_codes() {
        assert!(TAXONOMY.submit_outcome_unknown(E_TIMEOUT_AMBIGUOUS));
        assert!(TAXONOMY.submit_outcome_unknown(19999), "unmapped");
        let open: Vec<i64> = walked()
            .filter(|&code| by_code(code).is_some() && TAXONOMY.submit_outcome_unknown(code))
            .collect();
        assert_eq!(open, [10001, 11031, 11094, 13008, 13028, 13888]);
        for code in [0, 10004, 10009, 10028, 10040, 10050, 11054, 13009, -32602, 500] {
            assert!(!TAXONOMY.submit_outcome_unknown(code), "{code}");
        }
    }

    /// Deribit's own codes, never another venue's: `10004` is `order_not_found` (the old shared seed read it as
    /// Bybit's auth code), `10028` the rate limit, `13009` a refused token, and the `10xxx` codes Bybit spends on its
    /// rate limit and its server error are deribit's order limits.
    #[test]
    fn deribit_codes_are_deribits() {
        assert_eq!(kind(10004), ErrorKind::NotFound);
        assert_eq!(kind(10028), ErrorKind::RateLimited);
        assert_eq!(kind(13009), ErrorKind::Auth);
        assert_eq!(
            kind(13020),
            ErrorKind::NotFound,
            "an instrument it does not list parks the start"
        );
        assert_eq!(kind(13888), ErrorKind::Timeout);
        assert_eq!(kind(11094), ErrorKind::ServerError);
        assert_eq!(kind(10009), ErrorKind::InsufficientFunds);
        assert_eq!(
            kind(10006),
            ErrorKind::InvalidRequest,
            "price_too_low4idx, not Bybit's rate limit"
        );
        assert_eq!(
            kind(10016),
            ErrorKind::InvalidRequest,
            "a PME size limit, not Bybit's server error"
        );
        assert_eq!(kind(19999), ErrorKind::UnmappedCode, "a code the page does not name");
    }

    /// Every row has a venue answer recorded in this repository behind it, or is named here as having none yet
    /// (the list only shrinks: a row given a sample leaves it).
    #[test]
    fn every_row_has_a_recorded_sample_or_is_listed_without_one() {
        use vike_bridge_core::error_table::RowSample;
        vike_bridge_core::error_table::assert_every_row_has_a_sample(
            &TAXONOMY,
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."),
            walked(),
            &[
                RowSample {
                    code: -32602,
                    words: "Invalid params",
                    recorded_in: "crates/bridges/deribit/tests/fixtures/deribit_option_trades_invalid_currency.json",
                },
                RowSample {
                    code: 13009,
                    words: "unauthorized",
                    recorded_in: "crates/bridges/deribit/tests/offline/fake_deribit_websocket.rs",
                },
                RowSample {
                    code: 10034,
                    words: "trigger_price_too_high",
                    recorded_in: "crates/bridges/deribit/tests/deribit_smokes/deribit_native_stop_testnet_smoke.rs",
                },
                RowSample {
                    code: 11030,
                    words: "invalid_reduce_only_order",
                    recorded_in: "crates/bridges/deribit/tests/deribit_any_symbol_testnet_smoke.rs",
                },
            ],
            &[
                -32700, -32601, -32600, -32000, 10000, 10001, 10002, 10003, 10004, 10005, 10006,
                10007, 10008, 10009, 10010, 10011, 10012, 10013, 10014, 10015, 10016, 10017, 10018,
                10019, 10020, 10021, 10022, 10023, 10024, 10025, 10026, 10027, 10028, 10029, 10030,
                10031, 10032, 10033, 10035, 10036, 10037, 10038, 10039, 10040, 10041, 10043, 10044,
                10045, 10046, 10047, 10048, 10050, 10066, 10072, 11000, 11001, 11002, 11003, 11004,
                11005, 11006, 11007, 11008, 11009, 11013, 11021, 11022, 11029, 11031, 11035, 11036,
                11037, 11038, 11039, 11041, 11042, 11043, 11044, 11045, 11046, 11047, 11048, 11049,
                11050, 11051, 11052, 11054, 11055, 11056, 11059, 11060, 11094, 11098, 12003, 12005,
                13004, 13008, 13010, 13011, 13012, 13016, 13018, 13019, 13020, 13021, 13025, 13028,
                13030, 13031, 13032, 13035, 13036, 13503, 13666, 13777, 13778, 13888, 13901, 13922,
            ],
        );
    }
}
