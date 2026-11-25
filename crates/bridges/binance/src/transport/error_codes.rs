//! Binance's official error-code table, as a [`VenueTaxonomy`] (venue-error-taxonomy lane).
//!
//! Sources, each the venue's own page: spot <https://developers.binance.com/docs/binance-spot-api-docs/errors>,
//! USDⓈ-M <https://developers.binance.com/docs/derivatives/usds-margined-futures/error-code> (and its general-info
//! page for the HTTP statuses), COIN-M <https://developers.binance.com/docs/derivatives/coin-margined-futures/error-code>,
//! and aster's `asterdex/api-docs` V3 futures and spot pages (the codes where aster adds or changes a meaning).
//!
//! ## One row per code, across four books
//! Spot, USDⓈ-M, COIN-M and aster share one numbering, and a few codes mean different things per book: `-2021`,
//! `-2022`, `-2026` (spot and futures), `-4087`, `-4088`, `-4202` (USDⓈ-M and COIN-M), `-5047` (binance and aster).
//! Each such pair names the same outcome, so one row serves both, the comment naming both meanings. The one
//! exception is spot's `-2021` (a cancel-replace that half landed), which `crate::execution::rest::spot`'s
//! `CANCEL_REPLACE_PARTIAL` reads by its number before this table and settles by a read.
//!
//! ## The one reader of Binance's codes
//! [`vike_bridge_core::transport::ErrorKind::classify`] reads no venue's codes (it held a seed of 15
//! Binance codes until 2026-11-23; they are rows here, `tests::the_moved_seed_codes_keep_their_disposition`).
//! A code this table does not map is `ErrorKind::UnmappedCode` (never final), reported once at WARN
//! ([`vike_bridge_core::error_kind::classify_venue`]). Every caller that decides on a Binance code reads it
//! through [`TAXONOMY`]: the order-write rule ([`VenueTaxonomy::submit_outcome_unknown`]), the start's failure
//! kind, the rate-limit checks of the resync and the reconcile pass. The binance family shares it with aster.
//!
//! ## Binance's message-overloading problem
//! `-1010 ERROR_MSG_RECEIVED`, `-2010 NEW_ORDER_REJECTED`, `-2011 CANCEL_REJECTED`, `-2038 ORDER_AMEND_REJECTED`
//! and `-1013` (the spot filter failures) are umbrella codes: the real cause is only in the MESSAGE. "Account has
//! insufficient balance for requested action" and "Duplicate order sent" both arrive as `-2010`. Their rows give
//! the code's disposition (terminal), and [`by_msg`] carries the substring rules that refine it — chiefly
//! insufficient balance, which is a sizing signal rather than a bug. So a crossing spot `LIMIT_MAKER` (`-2010`
//! "Order would immediately match and take.") cannot be told by a row: [`post_only_cause`] reads its words.

use vike_bridge_core::error_kind::VenueTaxonomy;
use vike_bridge_core::transport::{ErrorKind, VenueApiError};
use vike_model::events::Event;

/// Binance's documented error codes → [`ErrorKind`], spot, USDⓈ-M, COIN-M and aster in one table (the module
/// doc); each comment is the code's published NAME, with the book when not every book publishes it. `None` =
/// undocumented here: an unmapped code ([`vike_bridge_core::error_kind::classify_venue`]).
pub fn by_code(code: i64) -> Option<ErrorKind> {
    Some(match code {
        // ---- HTTP statuses the futures general-info page gives a meaning (status-as-code) ----
        // A bare 408 is "a timeout has occurred while waiting for a response from the backend server": the write
        // may have landed (`ErrorKind::from_http_status` would call it a 4xx refusal).
        408 => ErrorKind::Timeout,
        // ---- 10xx: general server / request ----------------------------------------------
        -1000 => ErrorKind::ServerError, // UNKNOWN: unknown error handling the request
        -1001 => ErrorKind::ServerError, // DISCONNECTED: internal failure; retry
        -1002 => ErrorKind::Auth,        // UNAUTHORIZED: not authorized for this request
        -1003 => ErrorKind::RateLimited, // TOO_MANY_REQUESTS: queue full / IP banned
        -1006 => ErrorKind::Timeout,     // UNEXPECTED_RESP: message bus; execution status UNKNOWN
        -1007 => ErrorKind::Timeout,     // TIMEOUT: backend never answered — status UNKNOWN
        -1008 => ErrorKind::RateLimited, // SERVER_BUSY (spot) / REQUEST_THROTTLED (futures): back off
        -1010 => ErrorKind::InvalidRequest, // ERROR_MSG_RECEIVED: the matching engine's error, an umbrella
        -1011 => ErrorKind::Auth, // NON_WHITE_LIST: this IP may not use the route (futures)
        -1013 => ErrorKind::InvalidRequest, // INVALID_MESSAGE: rejected pre-matching-engine
        -1014 => ErrorKind::InvalidRequest, // UNKNOWN_ORDER_COMPOSITION
        -1015 => ErrorKind::RateLimited, // TOO_MANY_ORDERS: new-order rate limit
        -1016 => ErrorKind::VenueMaintenance, // SERVICE_SHUTTING_DOWN
        -1018 => ErrorKind::InvalidRequest, // PARAM_OVERFLOW (spot)
        -1020 => ErrorKind::InvalidRequest, // UNSUPPORTED_OPERATION
        -1021 => ErrorKind::InvalidRequest, // INVALID_TIMESTAMP: outside recvWindow
        -1022 => ErrorKind::Auth, // INVALID_SIGNATURE
        -1023 => ErrorKind::InvalidRequest, // START_TIME_GREATER_THAN_END_TIME (futures)
        -1099 => ErrorKind::Auth, // NOT_FOUND: not found, unauthenticated or unauthorized (USDⓈ-M)
        // ---- 11xx-12xx: request parameters -----------------------------------------------
        -1100 => ErrorKind::InvalidRequest, // ILLEGAL_CHARS
        -1101 => ErrorKind::InvalidRequest, // TOO_MANY_PARAMETERS
        -1102 => ErrorKind::InvalidRequest, // MANDATORY_PARAM_EMPTY_OR_MALFORMED
        -1103 => ErrorKind::InvalidRequest, // UNKNOWN_PARAM
        -1104 => ErrorKind::InvalidRequest, // UNREAD_PARAMETERS
        -1105 => ErrorKind::InvalidRequest, // PARAM_EMPTY
        -1106 => ErrorKind::InvalidRequest, // PARAM_NOT_REQUIRED
        -1108 => ErrorKind::InvalidRequest, // BAD_ASSET
        -1109 => ErrorKind::InvalidRequest, // BAD_ACCOUNT (futures)
        -1110 => ErrorKind::InvalidRequest, // BAD_INSTRUMENT_TYPE (futures)
        -1111 => ErrorKind::InvalidRequest, // BAD_PRECISION: too much precision for the step/tick
        -1112 => ErrorKind::InvalidRequest, // NO_DEPTH: no orders on the book
        -1114 => ErrorKind::InvalidRequest, // TIF_NOT_REQUIRED
        -1115 => ErrorKind::InvalidRequest, // INVALID_TIF
        -1116 => ErrorKind::InvalidRequest, // INVALID_ORDER_TYPE
        -1117 => ErrorKind::InvalidRequest, // INVALID_SIDE
        -1118 => ErrorKind::InvalidRequest, // EMPTY_NEW_CL_ORD_ID
        -1119 => ErrorKind::InvalidRequest, // EMPTY_ORG_CL_ORD_ID
        -1120 => ErrorKind::InvalidRequest, // BAD_INTERVAL
        -1121 => ErrorKind::InvalidRequest, // BAD_SYMBOL
        -1122 => ErrorKind::InvalidRequest, // INVALID_SYMBOLSTATUS (spot, USDⓈ-M)
        -1125 => ErrorKind::NotFound,       // INVALID_LISTEN_KEY: the listen key does not exist
        -1126 => ErrorKind::InvalidRequest, // ASSET_NOT_SUPPORTED (USDⓈ-M)
        -1127 => ErrorKind::InvalidRequest, // MORE_THAN_XX_HOURS: lookup interval too big
        -1128 => ErrorKind::InvalidRequest, // OPTIONAL_PARAMS_BAD_COMBO
        -1130 => ErrorKind::InvalidRequest, // INVALID_PARAMETER
        -1134 => ErrorKind::InvalidRequest, // BAD_STRATEGY_TYPE (spot)
        -1135 => ErrorKind::InvalidRequest, // INVALID_JSON
        -1136 => ErrorKind::InvalidRequest, // INVALID_NEW_ORDER_RESP_TYPE (futures)
        -1145 => ErrorKind::InvalidRequest, // INVALID_CANCEL_RESTRICTIONS (spot)
        -1210 => ErrorKind::InvalidRequest, // INVALID_PEG_PRICE_TYPE (spot)
        -1211 => ErrorKind::InvalidRequest, // INVALID_PEG_OFFSET_TYPE (spot)
        // ---- 2xxx: order / account -------------------------------------------------------
        // -2010 NEW_ORDER_REJECTED, -2011 CANCEL_REJECTED and -2038 ORDER_AMEND_REJECTED are UMBRELLA codes:
        // the real cause is only in the message, which `by_msg` reads to REFINE these rows within their
        // disposition (insufficient balance; -2011's "Unknown order sent." is NotFound).
        -2010 => ErrorKind::InvalidRequest, // NEW_ORDER_REJECTED (spot)
        -2011 => ErrorKind::InvalidRequest, // CANCEL_REJECTED
        -2012 => ErrorKind::ServerError,    // CANCEL_ALL_FAIL: batch cancel failure (USDⓈ-M)
        -2013 => ErrorKind::NotFound,       // NO_SUCH_ORDER
        -2014 => ErrorKind::Auth,           // BAD_API_KEY_FMT
        -2015 => ErrorKind::Auth,           // REJECTED_MBX_KEY: bad key / IP / permissions
        -2017 => ErrorKind::Auth,           // API_KEYS_LOCKED (USDⓈ-M)
        -2018 => ErrorKind::InsufficientFunds, // BALANCE_NOT_SUFFICIENT (futures)
        -2019 => ErrorKind::InsufficientFunds, // MARGIN_NOT_SUFFICIEN (futures)
        -2020 => ErrorKind::InvalidRequest, // UNABLE_TO_FILL (futures)
        // futures and aster spot: ORDER_WOULD_IMMEDIATELY_TRIGGER; spot: the half-landed cancel-replace, read by
        // number in `crate::execution::rest::spot` first
        -2021 => ErrorKind::InvalidRequest,
        // futures: REDUCE_ONLY_REJECT; spot: a cancel-replace that changed nothing
        -2022 => ErrorKind::InvalidRequest,
        -2023 => ErrorKind::InvalidRequest, // USER_IN_LIQUIDATION (futures)
        -2024 => ErrorKind::InvalidRequest, // POSITION_NOT_SUFFICIENT (futures)
        -2025 => ErrorKind::InvalidRequest, // MAX_OPEN_ORDER_EXCEEDED (futures)
        // futures: REDUCE_ONLY_ORDER_TYPE_NOT_SUPPORTED; spot: ORDER_ARCHIVED (90+ days old)
        -2026 => ErrorKind::InvalidRequest,
        -2027 => ErrorKind::InvalidRequest, // MAX_LEVERAGE_RATIO (futures)
        -2028 => ErrorKind::InsufficientFunds, // MIN_LEVERAGE_RATIO (futures)
        -2031 => ErrorKind::InvalidRequest, // INVALID_BUILDER_PARAMETER (aster futures)
        -2038 => ErrorKind::InvalidRequest, // ORDER_AMEND_REJECTED (spot)
        -2039 => ErrorKind::InvalidRequest, // CLIENT_ORDER_ID_INVALID
        // ---- 40xx: futures order parameters ---------------------------------------------
        -4000 => ErrorKind::InvalidRequest, // INVALID_ORDER_STATUS
        -4001 => ErrorKind::InvalidRequest, // PRICE_LESS_THAN_ZERO
        -4002 => ErrorKind::InvalidRequest, // PRICE_GREATER_THAN_MAX_PRICE
        -4003 => ErrorKind::InvalidRequest, // QTY_LESS_THAN_ZERO
        -4004 => ErrorKind::InvalidRequest, // QTY_LESS_THAN_MIN_QTY
        -4005 => ErrorKind::InvalidRequest, // QTY_GREATER_THAN_MAX_QTY
        -4006 => ErrorKind::InvalidRequest, // STOP_PRICE_LESS_THAN_ZERO
        -4007 => ErrorKind::InvalidRequest, // STOP_PRICE_GREATER_THAN_MAX_PRICE
        -4008 => ErrorKind::InvalidRequest, // TICK_SIZE_LESS_THAN_ZERO
        -4009 => ErrorKind::InvalidRequest, // MAX_PRICE_LESS_THAN_MIN_PRICE
        -4010 => ErrorKind::InvalidRequest, // MAX_QTY_LESS_THAN_MIN_QTY
        -4011 => ErrorKind::InvalidRequest, // STEP_SIZE_LESS_THAN_ZERO
        -4012 => ErrorKind::InvalidRequest, // MAX_NUM_ORDERS_LESS_THAN_ZERO
        -4013 => ErrorKind::InvalidRequest, // PRICE_LESS_THAN_MIN_PRICE
        -4014 => ErrorKind::InvalidRequest, // PRICE_NOT_INCREASED_BY_TICK_SIZE
        -4015 => ErrorKind::InvalidRequest, // INVALID_CL_ORD_ID_LEN
        -4016 => ErrorKind::InvalidRequest, // PRICE_HIGHTER_THAN_MULTIPLIER_UP
        -4017 => ErrorKind::InvalidRequest, // MULTIPLIER_UP_LESS_THAN_ZERO
        -4018 => ErrorKind::InvalidRequest, // MULTIPLIER_DOWN_LESS_THAN_ZERO
        -4019 => ErrorKind::InvalidRequest, // COMPOSITE_SCALE_OVERFLOW
        -4020 => ErrorKind::InvalidRequest, // TARGET_STRATEGY_INVALID
        -4021 => ErrorKind::InvalidRequest, // INVALID_DEPTH_LIMIT
        -4022 => ErrorKind::InvalidRequest, // WRONG_MARKET_STATUS
        -4023 => ErrorKind::InvalidRequest, // QTY_NOT_INCREASED_BY_STEP_SIZE
        -4024 => ErrorKind::InvalidRequest, // PRICE_LOWER_THAN_MULTIPLIER_DOWN
        -4025 => ErrorKind::InvalidRequest, // MULTIPLIER_DECIMAL_LESS_THAN_ZERO
        -4026 => ErrorKind::InvalidRequest, // COMMISSION_INVALID
        -4027 => ErrorKind::InvalidRequest, // INVALID_ACCOUNT_TYPE
        -4028 => ErrorKind::InvalidRequest, // INVALID_LEVERAGE
        -4029 => ErrorKind::InvalidRequest, // INVALID_TICK_SIZE_PRECISION
        -4030 => ErrorKind::InvalidRequest, // INVALID_STEP_SIZE_PRECISION
        -4031 => ErrorKind::InvalidRequest, // INVALID_WORKING_TYPE
        -4032 => ErrorKind::InvalidRequest, // EXCEED_MAX_CANCEL_ORDER_SIZE
        -4033 => ErrorKind::InvalidRequest, // INSURANCE_ACCOUNT_NOT_FOUND
        -4045 => ErrorKind::InvalidRequest, // MAX_STOP_ORDER_EXCEEDED
        -4046 => ErrorKind::InvalidRequest, // NO_NEED_TO_CHANGE_MARGIN_TYPE (`crate::account::modes`: success)
        -4047 => ErrorKind::InvalidRequest, // THERE_EXISTS_OPEN_ORDERS: no margin switch under orders
        -4048 => ErrorKind::InvalidRequest, // THERE_EXISTS_QUANTITY: no margin switch under a position
        -4056 => ErrorKind::Auth,           // INVALID_API_KEY_TYPE
        -4057 => ErrorKind::Auth,           // INVALID_RSA_PUBLIC_KEY
        -4059 => ErrorKind::InvalidRequest, // NO_NEED_TO_CHANGE_POSITION_SIDE (`crate::account::modes`: success)
        -4060 => ErrorKind::InvalidRequest, // INVALID_POSITION_SIDE
        -4061 => ErrorKind::InvalidRequest, // POSITION_SIDE_NOT_MATCH: a one-way order on a hedge account
        -4062 => ErrorKind::InvalidRequest, // REDUCE_ONLY_CONFLICT
        -4067 => ErrorKind::InvalidRequest, // POSITION_SIDE_CHANGE_EXISTS_OPEN_ORDERS
        -4068 => ErrorKind::InvalidRequest, // POSITION_SIDE_CHANGE_EXISTS_QUANTITY
        -4082 => ErrorKind::InvalidRequest, // INVALID_BATCH_PLACE_ORDER_SIZE
        -4083 => ErrorKind::ServerError, // PLACE_BATCH_ORDERS_FAIL: no cause, no word that nothing landed
        -4084 => ErrorKind::InvalidRequest, // UPCOMING_METHOD
        // USDⓈ-M: REDUCE_ONLY_ORDER_PERMISSION / NO_PLACE_ORDER_PERMISSION; COIN-M: INVALID_PAIR /
        // INVALID_TIME_INTERVAL
        -4087 | -4088 => ErrorKind::Auth,
        -4089 => ErrorKind::Auth, // REDUCE_ONLY_ORDER_PERMISSION (COIN-M)
        -4090 => ErrorKind::Auth, // NO_PLACE_ORDER_PERMISSION (COIN-M)
        // ---- 41xx-42xx: futures order, risk and leverage rules -----------------------------
        -4105 => ErrorKind::InvalidRequest, // SYMBOL_REDUCE_ONLY (USDⓈ-M)
        -4106 => ErrorKind::InvalidRequest, // SYMBOL_REDUCE_ONLY_BUY (USDⓈ-M)
        -4107 => ErrorKind::InvalidRequest, // SYMBOL_REDUCE_ONLY_SELL (USDⓈ-M)
        -4109 => ErrorKind::Auth,           // INACTIVE_ACCOUNT (USDⓈ-M)
        -4112 => ErrorKind::InsufficientFunds, // REDUCE_ONLY_MARGIN_CHECK_FAILED (COIN-M)
        -4113 => ErrorKind::InvalidRequest, // MARKET_ORDER_REJECT (COIN-M)
        -4116 => ErrorKind::InvalidRequest, // DUPLICATED_CLIENT_ORDER_ID (USDⓈ-M; `says_order_client_id_taken`)
        -4117 => ErrorKind::ServerError,    // STOP_ORDER_TRIGGERING: try again later (USDⓈ-M)
        -4118 => ErrorKind::InsufficientFunds, // REDUCE_ONLY_MARGIN_CHECK_FAILED (USDⓈ-M)
        -4120 => ErrorKind::InvalidRequest, // STOP_ORDER_SWITCH_ALGO: a conditional order off the Algo Service
        -4131 => ErrorKind::InvalidRequest, // MARKET_ORDER_REJECT (USDⓈ-M)
        -4135 => ErrorKind::InvalidRequest, // INVALID_ACTIVATION_PRICE
        -4137 => ErrorKind::InvalidRequest, // QUANTITY_EXISTS_WITH_CLOSE_POSITION
        -4138 => ErrorKind::InvalidRequest, // REDUCE_ONLY_MUST_BE_TRUE
        -4139 => ErrorKind::InvalidRequest, // ORDER_TYPE_CANNOT_BE_MKT
        -4140 => ErrorKind::InvalidRequest, // INVALID_OPENING_POSITION_STATUS (USDⓈ-M)
        -4141 => ErrorKind::InvalidRequest, // SYMBOL_ALREADY_CLOSED (USDⓈ-M)
        -4142 => ErrorKind::InvalidRequest, // STRATEGY_INVALID_TRIGGER_PRICE
        -4144 => ErrorKind::InvalidRequest, // INVALID_PAIR (USDⓈ-M)
        -4150 => ErrorKind::InvalidRequest, // ISOLATED_LEVERAGE_REJECT_WITH_POSITION (COIN-M)
        -4151 => ErrorKind::InvalidRequest, // PRICE_HIGHTER_THAN_STOP_MULTIPLIER_UP (COIN-M)
        -4152 => ErrorKind::InvalidRequest, // PRICE_LOWER_THAN_STOP_MULTIPLIER_DOWN (COIN-M)
        -4154 => ErrorKind::InvalidRequest, // STOP_PRICE_HIGHER_THAN_PRICE_MULTIPLIER_LIMIT (COIN-M)
        -4155 => ErrorKind::InvalidRequest, // STOP_PRICE_LOWER_THAN_PRICE_MULTIPLIER_LIMIT (COIN-M)
        -4161 => ErrorKind::InvalidRequest, // ISOLATED_LEVERAGE_REJECT_WITH_POSITION (USDⓈ-M)
        -4164 => ErrorKind::InvalidRequest, // MIN_NOTIONAL (USDⓈ-M)
        -4165 => ErrorKind::InvalidRequest, // INVALID_TIME_INTERVAL: a window over seven days
        -4168 => ErrorKind::InvalidRequest, // JOINT_MARGIN_REJECT_WITH_ISOLATED (USDⓈ-M)
        -4178 => ErrorKind::InvalidRequest, // MIN_NOTIONAL (COIN-M)
        -4183 => ErrorKind::InvalidRequest, // PRICE_HIGHTER_THAN_STOP_MULTIPLIER_UP (USDⓈ-M)
        -4184 => ErrorKind::InvalidRequest, // PRICE_LOWER_THAN_STOP_MULTIPLIER_DOWN (USDⓈ-M)
        -4188 => ErrorKind::InvalidRequest, // ME_INVALID_TIMESTAMP (COIN-M)
        -4189 => ErrorKind::Auth,           // ACCOUNT_REDUCE_ONLY
        -4192 => ErrorKind::Auth,           // COOLING_OFF_PERIOD
        -4194 => ErrorKind::Auth,           // ADJUST_LEVERAGE_KYC_FAILED (COIN-M)
        -4195 => ErrorKind::Auth,           // ADJUST_LEVERAGE_ONE_MONTH_FAILED (COIN-M)
        -4196 => ErrorKind::InvalidRequest, // LIMIT_ORDER_ONLY (COIN-M)
        -4197 => ErrorKind::InvalidRequest, // SAME_ORDER (COIN-M)
        -4198 => ErrorKind::InvalidRequest, // EXCEED_MAX_MODIFY_ORDER_LIMIT (COIN-M)
        -4199 => ErrorKind::InvalidRequest, // MOVE_ORDER_NOT_ALLOWED_SYMBOL_REASON (COIN-M)
        -4200 => ErrorKind::Auth,           // ADJUST_LEVERAGE_X_DAYS_FAILED (COIN-M)
        -4201 => ErrorKind::Auth,           // ADJUST_LEVERAGE_KYC_LIMIT (COIN-M)
        // USDⓈ-M: ADJUST_LEVERAGE_KYC_FAILED; COIN-M: ADJUST_LEVERAGE_ACCOUNT_SYMBOL_FAILED
        -4202 => ErrorKind::Auth,
        -4203 => ErrorKind::Auth, // ADJUST_LEVERAGE_ONE_MONTH_FAILED (USDⓈ-M)
        -4205 => ErrorKind::Auth, // ADJUST_LEVERAGE_X_DAYS_FAILED (USDⓈ-M)
        -4206 => ErrorKind::Auth, // ADJUST_LEVERAGE_KYC_LIMIT (USDⓈ-M)
        -4208 => ErrorKind::InvalidRequest, // ADJUST_LEVERAGE_ACCOUNT_SYMBOL_FAILED (USDⓈ-M)
        -4209 => ErrorKind::InvalidRequest, // ADJUST_LEVERAGE_SYMBOL_FAILED (USDⓈ-M)
        -4210 => ErrorKind::InvalidRequest, // STOP_PRICE_HIGHER_THAN_PRICE_MULTIPLIER_LIMIT (USDⓈ-M)
        -4211 => ErrorKind::InvalidRequest, // STOP_PRICE_LOWER_THAN_PRICE_MULTIPLIER_LIMIT (USDⓈ-M)
        -4220 => ErrorKind::InvalidRequest, // CANNOT_CHANGE_DUAL_SIDE (COIN-M)
        -4227 => ErrorKind::ServerError, // past the nonceWindow: rejected, never executed (aster)
        // ---- 44xx-45xx: USDⓈ-M account restrictions ---------------------------------------
        -4400 => ErrorKind::Auth,           // TRADING_QUANTITATIVE_RULE
        -4401 => ErrorKind::Auth,           // LARGE_POSITION_SYM_RULE
        -4402 => ErrorKind::Auth,           // COMPLIANCE_BLACK_SYMBOL_RESTRICTION
        -4403 => ErrorKind::Auth,           // ADJUST_LEVERAGE_COMPLIANCE_FAILED
        -4411 => ErrorKind::Auth, // the TradFi-Perps agreement is unsigned (measured; on no error page)
        -4531 => ErrorKind::InvalidRequest, // a position-mode switch needs UM and CM in sync (change log)
        // ---- 50xx: USDⓈ-M order lifetime and amend rules, aster's additions ------------------
        -5021 => ErrorKind::InvalidRequest, // FOK_ORDER_REJECT
        -5022 => ErrorKind::InvalidRequest, // GTX_ORDER_REJECT: a post-only order would cross (`post_only_cause`)
        -5024 => ErrorKind::InvalidRequest, // MOVE_ORDER_NOT_ALLOWED_SYMBOL_REASON
        -5025 => ErrorKind::InvalidRequest, // LIMIT_ORDER_ONLY
        -5026 => ErrorKind::InvalidRequest, // Exceed_Maximum_Modify_Order_Limit
        -5027 => ErrorKind::InvalidRequest, // SAME_ORDER: no need to modify the order
        -5028 => ErrorKind::InvalidRequest, // ME_RECVWINDOW_REJECT
        -5029 => ErrorKind::InvalidRequest, // MODIFICATION_MIN_NOTIONAL
        -5035 => ErrorKind::InvalidRequest, // INVALID_PEG_PRICE (aster)
        -5036 => ErrorKind::InvalidRequest, // PEG_PRICE_NOT_SUPPORTED (aster)
        -5037 => ErrorKind::InvalidRequest, // INVALID_PRICE_MATCH
        -5038 => ErrorKind::InvalidRequest, // UNSUPPORTED_ORDER_TYPE_PRICE_MATCH
        -5039 => ErrorKind::InvalidRequest, // INVALID_SELF_TRADE_PREVENTION_MODE
        -5040 => ErrorKind::InvalidRequest, // FUTURE_GOOD_TILL_DATE
        -5041 => ErrorKind::InvalidRequest, // BBO_ORDER_REJECT
        -5043 => ErrorKind::InvalidRequest, // Existing_Pending_Modification
        // binance: NOT_REDUCE_ONLY_ORDER (an amend); aster: INVALID_START_TIME (a read)
        -5047 => ErrorKind::InvalidRequest,
        -5048 => ErrorKind::InvalidRequest, // VAULT_SYMBOL_NOT_ALLOWED (aster)
        -5049 => ErrorKind::Auth,           // BUILDER_QUERY_NOT_ENABLED (aster)
        -5050 => ErrorKind::Auth,           // DEPOSIT_REQUIRED (aster)
        -5051 => ErrorKind::ServerError, // PRIVACY_CHECK_FAILED: the venue could not run a check (aster)
        -5052 => ErrorKind::Auth,        // USER_PRIVACY_MODE_ENABLED (aster)
        -5053 => ErrorKind::Auth,        // USER_NOT_BOUND_TO_BUILDER (aster)
        _ => return None,
    })
}

/// Does this code say only that the SERVER had trouble, so that it says nothing about what became of
/// a write? `-1000 UNKNOWN` and `-1001 DISCONNECTED`, which `UreqTransport::run` already reads as saying
/// nothing on a 5xx and which say no more on a 4xx; USDⓈ-M's `-4083 PLACE_BATCH_ORDERS_FAIL` and
/// `-2012 CANCEL_ALL_FAIL`, which name no cause and do not say nothing landed; aster's `-5051
/// PRIVACY_CHECK_FAILED` (the venue could not run a check). With the transport's sentinel and the
/// [`ErrorKind::Timeout`] rows (`-1006`/`-1007`, which the transport hands over as the body's code, e.g. on an
/// HTTP 503, aster's tables carrying the same two; a bare HTTP 408), it makes [`TAXONOMY`]'s
/// [`VenueTaxonomy::submit_outcome_unknown`], the ONE rule behind every submit arm of binance and aster:
/// `true` means re-query by order client id (`resolve_ambiguous_submit`), never a terminal `OrderRejected`,
/// and a re-query that fails says nothing (`family::execution::confirm`'s `resolve_submit`).
pub fn server_had_trouble(code: i64) -> bool {
    matches!(code, -1000 | -1001 | -2012 | -4083 | -5051)
}

/// Does a refused SUBMIT say the venue already holds an order under this order client id? USDⓈ-M's
/// `-4116 DUPLICATED_CLIENT_ORDER_ID` ("clientOrderId is duplicated", the futures error-code page), and spot's
/// `-2010` umbrella in its "Duplicate order sent." words ("The `clOrdId` is already in use.", the spot
/// `errors.md`); both keep the id unique among OPEN orders only. It is no refusal of OUR order: an order
/// rests under its id, and the core refuses an id it has already seen
/// (`crates/vike-core/src/runtime/recent_order_client_ids.rs`), so the order is one we sent. A terminal
/// `OrderRejected` would strand it; the submit arms read it as status-unknown and ask by the id instead
/// (decision 0303). COIN-M and aster document no such code.
pub fn says_order_client_id_taken(code: i64, msg: &str) -> bool {
    code == -4116 || (code == -2010 && msg.to_ascii_lowercase().contains("duplicate order"))
}

/// Does a CANCEL's refusal say the order is not there? `-2011 CANCEL_REJECTED` is an umbrella (the
/// module doc): "Unknown order sent." is the idempotent "already gone", while "This account may not place
/// or cancel orders." is a refusal over a LIVE order. So the WORDS decide ([`by_msg`]), and any other
/// words stay an error: a non-terminal `OrderCancelRejected`, never an `OrderCanceled` over a resting
/// order.
pub fn cancel_says_order_unknown(exc: &VenueApiError) -> bool {
    exc.code == -2011 && by_msg(&exc.msg) == Some(ErrorKind::NotFound)
}

/// Does an order endpoint's answer say it has no such order? `-2013 NO_SUCH_ORDER` by its code, `-2011`
/// by its words ([`cancel_says_order_unknown`]). Anything else says nothing about the order.
pub fn says_order_unknown(exc: &VenueApiError) -> bool {
    exc.code == -2013 || cancel_says_order_unknown(exc)
}

/// The typed cause of a refused POST-ONLY submit, or `None`: the venue's OWN answer that the order
/// would have taken liquidity, read off the code (never the prose alone) and only for an order that
/// asked to be post-only. USD-M futures answers a crossing `GTX` order `-5022` ("Due to the order
/// could not be executed as maker, the Post Only order will be rejected"; the order is not
/// recorded); spot answers a crossing `LIMIT_MAKER` with the `-2010` umbrella code, whose MESSAGE
/// ("Order would immediately match and take.") is the only thing telling it from an insufficient
/// balance (the message-overloading problem in this module's doc), so spot also reads the words.
#[must_use]
pub fn post_only_cause(
    request: &vike_model::OrderRequest,
    code: i64,
    msg: &str,
) -> Option<vike_model::events::RejectCause> {
    if request.time_in_force != vike_model::TimeInForce::PostOnly {
        return None;
    }
    let crossed =
        code == -5022 || (code == -2010 && msg.to_ascii_lowercase().contains("immediately match"));
    crossed.then_some(vike_model::events::RejectCause::PostOnlyWouldCross)
}

/// The non-terminal advisory for a failed native (perp) MODIFY. A status-unknown code
/// ([`VenueTaxonomy::submit_outcome_unknown`]) is worded as the timeout sentinel is (`modify outcome unknown`:
/// the PUT may have been applied) instead of as a refusal; a definite one keeps the venue's words. The
/// ONE modify rule of binance's and aster's perp arms (spot re-queries via `settle_replace`). Stamped
/// when the error was received: it carries no venue time (`vike_bridge_core::rest::amend_answer_time`).
pub fn modify_failed(order_client_id: &str, exc: &VenueApiError) -> Event {
    let ts = vike_bridge_core::rest::amend_answer_time(None);
    let unknown = |code| TAXONOMY.submit_outcome_unknown(code);
    vike_bridge_core::rest::modify_failed_with(order_client_id, exc, unknown, ts)
}

/// What a perp start's failed leverage write leaves the account holding, in the words of its log line (binance's
/// and aster's start, which go on either way): a refusal keeps the CURRENT leverage, while a write whose outcome is
/// unknown ([`VenueTaxonomy::submit_outcome_unknown`]) may have applied the configured one.
#[must_use]
pub fn start_leverage_words(code: i64) -> &'static str {
    if TAXONOMY.submit_outcome_unknown(code) {
        "the write got no verdict, so the account holds the configured leverage or its CURRENT one"
    } else {
        "the account keeps its CURRENT leverage"
    }
}

/// Message-substring rules for Binance's umbrella codes (chiefly `-2010`/`-2011`, whose text is the
/// only place the real cause appears). Case-insensitive. `None` = no rule matched.
pub fn by_msg(msg: &str) -> Option<ErrorKind> {
    let m = msg.to_ascii_lowercase();
    // Insufficient balance is the one that most changes a caller's response (resize, don't abort).
    if m.contains("insufficient balance") || m.contains("insufficient margin") {
        return Some(ErrorKind::InsufficientFunds);
    }
    // "Market is closed." is deliberately NOT VenueMaintenance. Binance returns it as
    // `-2010 NEW_ORDER_REJECTED` when a symbol is in BREAK/HALT — a rejection of THIS order that
    // stands until the venue reopens the symbol, i.e. terminal, and the baseline for -2010 is
    // terminal too. Classifying it retryable flipped a baseline code's disposition and spun a halted
    // symbol's order in a backoff loop instead of rejecting it once.
    if m.contains("market is closed") {
        return Some(ErrorKind::InvalidRequest);
    }
    if m.contains("system maintenance") {
        return Some(ErrorKind::VenueMaintenance);
    }
    if m.contains("unknown order") || m.contains("order does not exist") {
        return Some(ErrorKind::NotFound);
    }
    if m.contains("duplicate order")
        || m.contains("too much precision")
        || m.contains("maximum defined limit")
        || m.contains("is zero or less")
    {
        return Some(ErrorKind::InvalidRequest);
    }
    if m.contains("too many request") || m.contains("rate limit") {
        return Some(ErrorKind::RateLimited);
    }
    None
}

/// Binance's taxonomy, ready to thread into
/// [`vike_bridge_core::error_kind::classify_venue`].
pub const TAXONOMY: VenueTaxonomy =
    VenueTaxonomy { venue: "binance", by_code, by_msg, leaves_write_open: server_had_trouble };

#[cfg(test)]
mod tests {
    use super::*;
    use vike_bridge_core::error_kind::classify_venue;

    fn kind(code: i64, msg: &str) -> ErrorKind {
        classify_venue(Some(&TAXONOMY), &VenueApiError { code, msg: msg.to_string() })
    }

    /// Documented codes land in the right bucket, spanning every category the lane asked for.
    #[test]
    fn documented_codes_classify() {
        let cases = [
            (-1002, ErrorKind::Auth),
            (-1022, ErrorKind::Auth),
            (-2014, ErrorKind::Auth),
            (-2015, ErrorKind::Auth),
            (-1003, ErrorKind::RateLimited),
            (-1008, ErrorKind::RateLimited),
            (-1015, ErrorKind::RateLimited),
            (-1016, ErrorKind::VenueMaintenance),
            (-1000, ErrorKind::ServerError),
            (-1001, ErrorKind::ServerError),
            (-1006, ErrorKind::Timeout),
            (-1007, ErrorKind::Timeout),
            (-2013, ErrorKind::NotFound),
            (-1125, ErrorKind::NotFound),
            (-1013, ErrorKind::InvalidRequest),
            (-1021, ErrorKind::InvalidRequest),
            (-1100, ErrorKind::InvalidRequest),
            (-1111, ErrorKind::InvalidRequest),
            (-1121, ErrorKind::InvalidRequest),
            (-2039, ErrorKind::InvalidRequest),
            // the futures order path's refusals, each a row so none waits on a confirm
            (-2019, ErrorKind::InsufficientFunds),
            (-2021, ErrorKind::InvalidRequest),
            (-2026, ErrorKind::InvalidRequest),
            (-4028, ErrorKind::InvalidRequest),
            (-4061, ErrorKind::InvalidRequest),
            (-4120, ErrorKind::InvalidRequest),
            (-4411, ErrorKind::Auth),
            (-5022, ErrorKind::InvalidRequest),
            (-4117, ErrorKind::ServerError),
            (-4227, ErrorKind::ServerError),
            (408, ErrorKind::Timeout),
        ];
        for (code, want) in cases {
            assert_eq!(kind(code, ""), want, "binance {code}");
        }
    }

    /// A submit may still be live on the sentinel, on the "execution status unknown" codes (and a bare
    /// futures HTTP 408, BN-1), and on the "the server had trouble" codes, and on nothing else: a definite
    /// refusal must stay a terminal reject. `-1008` (overloaded), `-1016` (shutting down), `-4117` (a stop in
    /// transition) and aster's `-4227` (past its nonce window, never executed) stay refusals at the door.
    #[test]
    fn submit_outcome_unknown_is_exactly_the_sentinel_and_the_status_unknown_codes() {
        use vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS;
        for code in [E_TIMEOUT_AMBIGUOUS, -1000, -1001, -1006, -1007, 408, -2012, -4083, -5051] {
            assert!(TAXONOMY.submit_outcome_unknown(code), "{code}");
        }
        for code in [
            0, -1003, -1008, -1013, -1016, -1021, -1111, -2010, -2013, -2015, -2019, -2021, -4028,
            -4061, -4117, -4227, -5022, 400, 429, 503,
        ] {
            assert!(!TAXONOMY.submit_outcome_unknown(code), "{code}");
        }
    }

    /// A start's failed leverage write is worded by its outcome (S4b BN-3): one with no verdict may have
    /// applied the figure, so it is never said to keep the current leverage.
    #[test]
    fn a_start_leverage_write_without_a_verdict_is_not_said_to_keep_the_leverage() {
        use vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS;
        let kept = "the account keeps its CURRENT leverage";
        for code in [E_TIMEOUT_AMBIGUOUS, -1007, -1000, 408] {
            assert_eq!(
                start_leverage_words(code),
                "the write got no verdict, so the account holds the configured leverage or its CURRENT one",
                "{code}"
            );
        }
        for code in [-4028, -2015, -4209] {
            assert_eq!(start_leverage_words(code), kept, "{code}");
        }
    }

    /// The duplicate-id answers the venue documents, and nothing near them: the same words under another
    /// code, another `-2010` cause, a `-4116` neighbour.
    #[test]
    fn only_the_documented_duplicate_answers_say_the_order_client_id_is_taken() {
        assert!(says_order_client_id_taken(-4116, "clientOrderId is duplicated"));
        assert!(says_order_client_id_taken(-2010, "Duplicate order sent."));
        assert!(!says_order_client_id_taken(-2010, "Account has insufficient balance"));
        assert!(!says_order_client_id_taken(-2011, "Duplicate order sent."));
        assert!(!says_order_client_id_taken(-4115, "clientOrderId is duplicated"));
    }

    /// The umbrella codes resolve through the MESSAGE — the whole reason `by_msg` exists.
    #[test]
    fn umbrella_codes_resolve_via_the_message() {
        assert_eq!(
            kind(-2010, "Account has insufficient balance for requested action"),
            ErrorKind::InsufficientFunds
        );
        assert_eq!(kind(-2010, "Duplicate order sent."), ErrorKind::InvalidRequest);
        assert_eq!(kind(-2010, "Market is closed."), ErrorKind::InvalidRequest);
        // No message rule → the umbrella code's own row; only the cancel's "not there" words say NotFound.
        assert_eq!(kind(-2010, "some novel reject"), ErrorKind::InvalidRequest);
        assert_eq!(kind(-2011, "some novel reject"), ErrorKind::InvalidRequest);
        assert_eq!(kind(-2011, "Unknown order sent."), ErrorKind::NotFound);
    }

    /// `-2011` is an umbrella: only its "the order is not there" words say so, on any spelling the
    /// venue was recorded using; every other word over the code is a refusal of a possibly live order.
    /// `-2013` says it by its code.
    #[test]
    fn only_the_order_is_not_there_words_of_the_umbrella_code_say_unknown() {
        let e = |code, msg: &str| VenueApiError { code, msg: msg.to_string() };
        for words in ["Unknown order sent.", "Order does not exist.", "UNKNOWN ORDER SENT."] {
            assert!(cancel_says_order_unknown(&e(-2011, words)), "{words}");
            assert!(says_order_unknown(&e(-2011, words)), "{words}");
        }
        for words in [
            "This account may not place or cancel orders.",
            "Order was not canceled due to cancel restrictions.",
            "Cancel order is invalid. Check origClientOrderId and orderId.",
            "",
        ] {
            assert!(!cancel_says_order_unknown(&e(-2011, words)), "{words}");
            assert!(!says_order_unknown(&e(-2011, words)), "{words}");
        }
        assert!(says_order_unknown(&e(-2013, "whatever the words")));
        assert!(
            !cancel_says_order_unknown(&e(-2013, "Order does not exist.")),
            "not a cancel's word"
        );
        for code in [0, -1003, -1021, -2010, -2015, -2021, -4117, -4120, 400, 404, 503] {
            assert!(!says_order_unknown(&e(code, "Unknown order sent.")), "{code}");
        }
    }

    /// An undocumented code is unmapped: never final (its outcome unknown), never retried blindly; a
    /// code this bridge mints for a refusal of its own stays a refusal.
    #[test]
    fn an_unmapped_code_is_never_final() {
        let k = kind(-8_999, "brand new failure");
        assert_eq!(k, ErrorKind::UnmappedCode);
        assert!(!k.is_terminal() && !k.is_retryable());
        let own = crate::execution::rest::coin_m::E_NO_COIN_M_ROW;
        assert_eq!(kind(own, "no row"), ErrorKind::Unknown);
    }

    /// The codes `ErrorKind::classify` held for Binance until 2026-11-23 keep their disposition as this
    /// table's rows. `-1016 SERVICE_SHUTTING_DOWN` is deliberately re-bucketed within its disposition
    /// (`ServerError` → `VenueMaintenance`, both retryable).
    #[test]
    fn the_moved_seed_codes_keep_their_disposition() {
        use vike_bridge_core::transport::ErrorKind::{
            Auth, InvalidRequest, NotFound, RateLimited, ServerError,
        };
        vike_bridge_core::error_table::assert_keeps_the_moved_seed_dispositions(
            &TAXONOMY,
            &[
                (-1003, RateLimited),
                (-1015, RateLimited),
                (-2014, Auth),
                (-2015, Auth),
                (-1022, Auth),
                (-2011, NotFound),
                (-2013, NotFound),
                (-1000, ServerError),
                (-1001, ServerError),
                (-1016, ServerError),
                (-1013, InvalidRequest),
                (-1100, InvalidRequest),
                (-1102, InvalidRequest),
                (-1111, InvalidRequest),
                (-2010, InvalidRequest),
            ],
        );
    }

    /// Every row has a venue answer recorded in this repository behind it, or is named here as having
    /// none yet (the list only shrinks: a row given a sample leaves it). The rows added on 2026-11-23 from
    /// the venues' own pages (S4b) joined the list with them, but `-2027` and `-4411`, measured on the demo host.
    #[test]
    fn every_row_has_a_recorded_sample_or_is_listed_without_one() {
        use vike_bridge_core::error_table::RowSample;
        vike_bridge_core::error_table::assert_every_row_has_a_sample(
            &TAXONOMY,
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."),
            (-5100..=-1000i64).chain(100..600),
            &[
                RowSample {
                    code: -2014,
                    words: "API-key format invalid.",
                    recorded_in: "crates/bridges/binance/src/adapter.rs",
                },
                RowSample {
                    code: -4165,
                    words: "Maximum time interval is 7 days.",
                    recorded_in: "crates/bridges/binance/tests/offline/algo_old_stop.rs",
                },
                RowSample {
                    code: -2027,
                    words: "Exceeded the maximum allowable position at current leverage",
                    recorded_in: "crates/bridges/binance/tests/binance_smokes/binance_classes_smoke.rs",
                },
                RowSample {
                    code: -4411,
                    words: "Please sign TradFi-Perps agreement contract fapi.",
                    recorded_in: "crates/bridges/binance/src/account/class_prep.rs",
                },
            ],
            &[
                -1000, -1001, -1002, -1003, -1006, -1007, -1008, -1010, -1011, -1013, -1014, -1015,
                -1016, -1018, -1020, -1021, -1022, -1023, -1099, -1100, -1101, -1102, -1103, -1104,
                -1105, -1106, -1108, -1109, -1110, -1111, -1112, -1114, -1115, -1116, -1117, -1118,
                -1119, -1120, -1121, -1122, -1125, -1126, -1127, -1128, -1130, -1134, -1135, -1136,
                -1145, -1210, -1211, -2010, -2011, -2012, -2013, -2015, -2017, -2018, -2019, -2020,
                -2021, -2022, -2023, -2024, -2025, -2026, -2028, -2031, -2038, -2039, -4000, -4001,
                -4002, -4003, -4004, -4005, -4006, -4007, -4008, -4009, -4010, -4011, -4012, -4013,
                -4014, -4015, -4016, -4017, -4018, -4019, -4020, -4021, -4022, -4023, -4024, -4025,
                -4026, -4027, -4028, -4029, -4030, -4031, -4032, -4033, -4045, -4046, -4047, -4048,
                -4056, -4057, -4059, -4060, -4061, -4062, -4067, -4068, -4082, -4083, -4084, -4087,
                -4088, -4089, -4090, -4105, -4106, -4107, -4109, -4112, -4113, -4116, -4117, -4118,
                -4120, -4131, -4135, -4137, -4138, -4139, -4140, -4141, -4142, -4144, -4150, -4151,
                -4152, -4154, -4155, -4161, -4164, -4168, -4178, -4183, -4184, -4188, -4189, -4192,
                -4194, -4195, -4196, -4197, -4198, -4199, -4200, -4201, -4202, -4203, -4205, -4206,
                -4208, -4209, -4210, -4211, -4220, -4227, -4400, -4401, -4402, -4403, -4531, -5021,
                -5022, -5024, -5025, -5026, -5027, -5028, -5029, -5035, -5036, -5037, -5038, -5039,
                -5040, -5041, -5043, -5047, -5048, -5049, -5050, -5051, -5052, -5053, 408,
            ],
        );
    }

    /// REGRESSION (adversarial review, major): `classify_venue` consults `by_msg` for EVERY error, so a
    /// disposition flip was reachable through a MESSAGE, and `-2010 NEW_ORDER_REJECTED / "Market is
    /// closed."` was a live example (terminal → retryable). This runs the FULL classifier over (code ×
    /// representative message) pairs, umbrella codes included: whatever the code already says (its row,
    /// else the HTTP status) keeps its disposition.
    #[test]
    fn full_classifier_never_changes_a_codes_disposition() {
        let messages = [
            "",
            "Market is closed.",
            "system maintenance in progress",
            "Account has insufficient balance for requested action",
            "Duplicate order sent.",
            "Unknown order sent.",
            "Too many requests.",
            "insufficient upstream capacity",
        ];
        for code in (-5100..=-1000i64).chain(100..600) {
            let neutral = VenueApiError { code, msg: String::new() }.kind();
            let standing = by_code(code).unwrap_or(neutral);
            if standing == ErrorKind::Unknown {
                continue;
            }
            for msg in messages {
                let ours = kind(code, msg);
                assert!(
                    vike_bridge_core::error_kind::same_disposition(ours, standing),
                    "binance {code} + {msg:?}: {ours} changes the disposition of {standing}"
                );
            }
        }
    }

    /// REGRESSION: a HALTED symbol's reject is TERMINAL. Retrying it spins until the session
    /// reopens and (before the seam's retry budget) emitted no terminal event at all.
    #[test]
    fn market_is_closed_stays_terminal() {
        let k = kind(-2010, "Market is closed.");
        assert!(k.is_terminal(), "a halted symbol rejects the order, it does not defer it");
        assert!(!k.is_retryable());
        assert_eq!(by_msg("Market is closed."), Some(ErrorKind::InvalidRequest));
    }

    /// A crossing post-only order is a TYPED refusal, read off the venue's own code: futures `-5022`,
    /// spot `-2010` + its "immediately match" words; the same `-2010` as an insufficient balance, a
    /// non-post-only order and an unrelated code are not.
    #[test]
    fn a_crossing_post_only_order_is_typed_and_nothing_else_is() {
        use vike_model::events::RejectCause::PostOnlyWouldCross;
        let req = |tif| vike_model::OrderRequest { time_in_force: tif, ..Default::default() };
        let po = req(vike_model::TimeInForce::PostOnly);
        assert_eq!(
            post_only_cause(&po, -5022, "Due to the order could not be executed as maker"),
            Some(PostOnlyWouldCross)
        );
        assert_eq!(
            post_only_cause(&po, -2010, "Order would immediately match and take."),
            Some(PostOnlyWouldCross)
        );
        assert_eq!(
            post_only_cause(&po, -2010, "Account has insufficient balance for requested action."),
            None
        );
        assert_eq!(post_only_cause(&po, -1013, "Filter failure: PRICE_FILTER"), None);
        let gtc = req(vike_model::TimeInForce::Gtc);
        assert_eq!(
            post_only_cause(&gtc, -5022, "x"),
            None,
            "only an order that asked to be post-only"
        );
    }

    /// Sanity on breadth: the lane asked for a real table, not a token one.
    #[test]
    fn table_covers_the_documented_set() {
        let n = (-5100..=-1000i64).filter(|c| by_code(*c).is_some()).count();
        assert!(n >= 200, "expected the full documented Binance set, got {n} codes");
    }

    /// A failed modify: this venue's status-unknown code (-1007) is worded as the timeout sentinel
    /// is, keeping the venue's message; a definite code (`-5027 SAME_ORDER`, a row) keeps the venue's words
    /// verbatim.
    #[test]
    fn modify_failed_words_the_unknown_code_as_the_sentinel() {
        let reason = |code| match modify_failed("c1", &VenueApiError { code, msg: "m".into() }) {
            Event::OrderModifyRejected(r) => r.reason.to_string(),
            other => panic!("expected OrderModifyRejected, got {other:?}"),
        };
        assert_eq!(
            reason(-1007),
            "modify outcome unknown (timed out after send; the venue may have applied it): m"
        );
        assert_eq!(reason(-5027), "m");
    }
}
