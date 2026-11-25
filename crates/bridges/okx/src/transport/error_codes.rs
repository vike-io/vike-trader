//! OKX's official V5 error-code table, as a [`VenueTaxonomy`] (venue-error-taxonomy lane).
//!
//! Source: <https://www.okx.com/docs-v5/en/#error-code> (the V5 "Error Code" reference — the
//! general class with the envelope's `1`/`2`, the 501xx authentication block, the trade class (51xxx and
//! its 54xxx rows) and the account rows of the leverage, position-mode, margin and settings writes
//! (59xxx)). Every documented code of those classes is a row, the product codes this bridge never meets
//! (bots, copy trading, spreads) included, so an order-path code is never unmapped (read 2026-11-23; a
//! code with several wordings is one row, a `subCode` family one row). Not a row: `51412`, an umbrella whose two
//! wordings disagree ("Cancellation timed out" is unknown, "has been triggered and can't be canceled" a refusal,
//! and `crates/bridges/okx/src/execution/rest/stop_algo.rs`'s `ALGO_NOT_CANCELED` mints it for the second); a
//! cancel's answer is the cancel path's to read. `51119` is kept though the page no longer lists it.
//!
//! Same shape as the binance/bybit siblings: an exact-code table consulted before the venue-neutral
//! layer ([`vike_bridge_core::transport::ErrorKind::classify`], which reads no venue's codes: it held a seed of 7
//! OKX codes until 2026-11-23, rows here now, `tests::the_moved_seed_codes_keep_their_disposition`), plus a
//! message fallback. A code this table does not map is `ErrorKind::UnmappedCode` (never final), reported once at WARN
//! ([`vike_bridge_core::error_kind::classify_venue`]).
//!
//! ## OKX's one venue-specific nuance
//! OKX runs SCHEDULED matching-engine maintenance around the funding settlements (00:00 / 08:00 /
//! 16:00 UTC+8). The page names the windows `51029` "Contract is being settled" and `51030` "Funding fee
//! is being settled"; the table files them, `50005` (endpoint offline) and the busy answers met inside such
//! a window, `50001` ("Service temporarily unavailable") and `50013` ("Systems are busy"), under
//! [`ErrorKind::VenueMaintenance`]. Those are minutes-long windows, not millisecond blips, which is exactly
//! what that kind exists to distinguish (both are NotNow; on a WRITE `50001` and `50013` leave the order
//! open, [`server_had_trouble`]).
//!
//! ## `sCode` vs `code`
//! OKX batch endpoints report the PER-ORDER outcome in `sCode`/`sMsg` while the envelope `code` is
//! `0`. This table classifies whichever numeric the adapter hands it — a caller that reads `sCode`
//! must pass THAT, not the envelope code (a `0` envelope reaches the pre-send `Network`
//! sentinel, which would be plainly wrong for a per-order reject).
//!
//! Every caller that decides on an OKX code reads it through [`TAXONOMY`]: the order-write rule
//! ([`VenueTaxonomy::submit_outcome_unknown`]), the start's failure kind, the rate-limit checks of the resync and
//! the reconcile pass.

use vike_bridge_core::error_kind::VenueTaxonomy;
use vike_bridge_core::transport::{ErrorKind, VenueApiError};
use vike_model::events::Event;

/// OKX's documented V5 error codes → [`ErrorKind`]. `None` = not mapped: an unmapped code
/// ([`vike_bridge_core::error_kind::classify_venue`]).
pub fn by_code(code: i64) -> Option<ErrorKind> {
    Some(match code {
        // ---- 1, 2: the envelope's own verdict (a row's `sCode` is read first, `unwrap_okx`) -----
        1 => ErrorKind::InvalidRequest, // Operation failed.
        2 => ErrorKind::InvalidRequest, // Bulk operation partially succeeded.
        // ---- 500xx: general ---------------------------------------------------------------------
        50000 => ErrorKind::InvalidRequest, // Body for POST request cannot be empty.
        50001 => ErrorKind::VenueMaintenance, // service temporarily unavailable
        50002 => ErrorKind::InvalidRequest, // JSON syntax error
        // 50004 is AMBIGUOUS, not transient. OKX's official text is "Endpoint request timeout (does
        // not mean the request was successful or failed, please check the request result)" — the
        // venue is explicitly telling the caller it does not know the outcome. Classifying it
        // retryable would re-POST /api/v5/trade/order over an order the venue may already hold:
        // double-submit, double position. It belongs in the SAME bucket as the transport's
        // E_TIMEOUT_AMBIGUOUS sentinel — Timeout ⇒ must_requery.
        50004 => ErrorKind::Timeout, // endpoint request timeout — OUTCOME UNKNOWN, must re-query
        50005 => ErrorKind::VenueMaintenance, // API endpoint is inactive or unavailable.
        50006 => ErrorKind::InvalidRequest, // invalid Content-Type
        50007 => ErrorKind::Auth,    // User blocked.
        50008 => ErrorKind::Auth,    // User doesn't exist.
        50009 => ErrorKind::Auth,    // Account is frozen due to stop-out.
        50010 => ErrorKind::InvalidRequest, // User ID cannot be empty.
        50011 => ErrorKind::RateLimited, // rate limit reached
        50012 => ErrorKind::Auth,    // Account status invalid. Check account status
        50013 => ErrorKind::VenueMaintenance, // Systems are busy. Please try again later.
        50014 => ErrorKind::InvalidRequest, // Parameter {param0} can not be empty.
        50015 => ErrorKind::InvalidRequest, // Either parameter {param0} or {param1} is required
        50016 => ErrorKind::InvalidRequest, // Parameter {param0} does not match parameter {param1}
        50017 => ErrorKind::InvalidRequest, // position frozen (ADL)
        50018 => ErrorKind::InvalidRequest, // currency frozen (ADL)
        50019 => ErrorKind::InvalidRequest, // account frozen (ADL)
        50020 => ErrorKind::InvalidRequest, // position frozen (forced liquidation)
        50021 => ErrorKind::InvalidRequest, // currency frozen (liquidation)
        50022 => ErrorKind::InvalidRequest, // account frozen (forced liquidation)
        50023 => ErrorKind::InvalidRequest, // funding fees frozen
        50024 => ErrorKind::InvalidRequest, // two parameters cannot coexist
        50025 => ErrorKind::InvalidRequest, // Parameter {param0} count exceeds the limit {param1}.
        50026 => ErrorKind::ServerError, // System error. Try again later
        50027 => ErrorKind::Auth,    // account restricted from trading
        50028 => ErrorKind::Auth,    // unable to place the order (contact support)
        50029 => ErrorKind::Auth,    // risk-control restriction
        50030 => ErrorKind::Auth,    // You don't have permission to use this API endpoint
        50032 => ErrorKind::InvalidRequest, // currency prohibited by account setting
        50033 => ErrorKind::InvalidRequest, // instrument blocked by account setting
        50035 => ErrorKind::Auth,    // This endpoint requires that APIKey must be bound to IP
        50036 => ErrorKind::InvalidRequest, // expTime already past
        50037 => ErrorKind::InvalidRequest, // Order expired.
        50038 => ErrorKind::InvalidRequest, // This feature is unavailable in demo trading
        50039 => ErrorKind::InvalidRequest, // pagination parameter unsupported
        50040 => ErrorKind::RateLimited, // Too frequent operations, please try again later
        50041 => ErrorKind::Auth,    // user ID not allowlisted
        50042 => ErrorKind::InvalidRequest, // Repeated request
        50044 => ErrorKind::InvalidRequest, // Must select one broker type
        50045 => ErrorKind::InvalidRequest, // position builder parameter
        50046 => ErrorKind::VenueMaintenance, // feature temporarily unavailable (improvements)
        50047 => ErrorKind::InvalidRequest, // instrument settled (candles)
        50048 => ErrorKind::InvalidRequest, // risk unit switch would raise risk
        50049 => ErrorKind::InvalidRequest, // no position tier: no margin trading
        50050 => ErrorKind::InvalidRequest, // options already activated
        50051 => ErrorKind::Auth,    // compliance: feature unavailable in region
        50052 => ErrorKind::Auth,    // compliance: crypto not tradable locally
        50053 => ErrorKind::InvalidRequest, // This feature is only available in demo trading.
        50055 => ErrorKind::InvalidRequest, // demo asset reset limit
        50056 => ErrorKind::InvalidRequest, // demo asset reset: open orders/positions
        50057 => ErrorKind::ServerError, // Reset unsuccessful. Try again later.
        50058 => ErrorKind::InvalidRequest, // This crypto is not supported in an asset reset.
        50059 => ErrorKind::Auth,    // regulator steps required
        50060 => ErrorKind::Auth,    // identity verification required
        50061 => ErrorKind::RateLimited, // account (sub-account) order rate limit
        50062 => ErrorKind::VenueMaintenance, // This feature is currently unavailable.
        50063 => ErrorKind::InvalidRequest, // credits cannot be activated
        50064 => ErrorKind::VenueMaintenance, // borrowing system unavailable
        50067 => ErrorKind::InvalidRequest, // The API doesn't support cross site trading feature
        50069 => ErrorKind::InsufficientFunds, // risk-unit margin ratio check failed
        50071 => ErrorKind::InvalidRequest, // {param} already exists e.g. clOrdId already exists
        50072 => ErrorKind::InvalidRequest, // isolated margin unsupported in this account mode
        50075 => ErrorKind::InvalidRequest, // The token is restricted in Islamic sub-account
        50076 => ErrorKind::InvalidRequest, // date range over limit (days)
        50077 => ErrorKind::InvalidRequest, // date range over limit (months)
        // ---- 501xx: authentication --------------------------------------------------------------
        50100 => ErrorKind::Auth, // API frozen, please contact customer service.
        50101 => ErrorKind::Auth, // APIKey does not match current environment.
        50102 => ErrorKind::InvalidRequest, // Timestamp request expired.
        50103 => ErrorKind::Auth, // Request header \"OK-ACCESS-KEY\" cannot be empty.
        50104 => ErrorKind::Auth, // Request header \"OK-ACCESS-PASSPHRASE\" cannot be empty.
        50105 => ErrorKind::Auth, // Request header \"OK-ACCESS-PASSPHRASE\" incorrect.
        50106 => ErrorKind::Auth, // Request header \"OK-ACCESS-SIGN\" cannot be empty.
        50107 => ErrorKind::Auth, // Request header \"OK-ACCESS-TIMESTAMP\" cannot be empty.
        50108 => ErrorKind::Auth, // Exchange ID does not exist.
        50109 => ErrorKind::Auth, // Exchange domain does not exist.
        50110 => ErrorKind::Auth, // IP not in key allowlist
        50111 => ErrorKind::Auth, // Invalid OK-ACCESS-KEY.
        50112 => ErrorKind::Auth, // Invalid OK-ACCESS-TIMESTAMP.
        50113 => ErrorKind::Auth, // Invalid signature.
        50114 => ErrorKind::Auth, // Invalid authorization.
        50115 => ErrorKind::InvalidRequest, // Invalid request method.
        50116 => ErrorKind::InvalidRequest, // Fast API is allowed to create only one API key
        50118 => ErrorKind::Auth, // broker IP must be allowlisted
        50119 => ErrorKind::Auth, // API key doesn't exist
        50120 => ErrorKind::Auth, // This API key doesn't have permission to use this function
        50121 => ErrorKind::Auth, // You can't access our services through the IP address ({param0})
        50122 => ErrorKind::InvalidRequest, // Order amount must exceed minimum amount
        50123 => ErrorKind::Auth, // This API Key does not have trading permission for the Crypto.
        // ---- 51xxx: trade -----------------------------------------------------------------------
        51000 => ErrorKind::InvalidRequest, // Parameter {param0} error
        51001 => ErrorKind::InvalidRequest, // instrument does not exist
        51002 => ErrorKind::InvalidRequest, // Instrument ID doesn't match underlying index.
        51003 => ErrorKind::InvalidRequest, // Either client order ID or order ID is required.
        51004 => ErrorKind::InvalidRequest, // over the position limit at current leverage
        51005 => ErrorKind::InvalidRequest, // Your order amount exceeds the max order amount.
        51006 => ErrorKind::InvalidRequest, // price outside a rule or limit
        51007 => ErrorKind::InvalidRequest, // size / value outside a limit
        51008 => ErrorKind::InsufficientFunds, // insufficient balance / margin / borrow quota
        51009 => ErrorKind::Auth, // Order blocked. Please contact customer support for assistance.
        51010 => ErrorKind::InvalidRequest, // Request unsupported under current account mode
        51011 => ErrorKind::InvalidRequest, // Order ID already exists.
        51012 => ErrorKind::InvalidRequest, // Token doesn't exist.
        51014 => ErrorKind::InvalidRequest, // Index doesn't exist.
        51015 => ErrorKind::InvalidRequest, // Instrument ID doesn't match instrument type.
        51016 => ErrorKind::InvalidRequest, // Client order ID already exists.
        51017 => ErrorKind::InsufficientFunds, // Loan amount exceeds borrowing limit.
        51018 => ErrorKind::InvalidRequest, // position rule
        51019 => ErrorKind::InvalidRequest, // position rule
        51020 => ErrorKind::InvalidRequest, // size / value outside a limit
        51021 => ErrorKind::InvalidRequest, // The pair or contract is not yet listed
        51022 => ErrorKind::InvalidRequest, // Contract suspended.
        51023 => ErrorKind::InvalidRequest, // Position doesn't exist.
        51024 => ErrorKind::Auth, // Trading account is blocked.
        51025 => ErrorKind::InvalidRequest, // Order count exceeds the limit.
        51026 => ErrorKind::InvalidRequest, // Instrument type doesn't match underlying index.
        51027 => ErrorKind::InvalidRequest, // Contract expired.
        51028 => ErrorKind::InvalidRequest, // Contract under delivery.
        51029 => ErrorKind::VenueMaintenance, // Contract is being settled.
        51030 => ErrorKind::VenueMaintenance, // Funding fee is being settled.
        51031 => ErrorKind::InvalidRequest, // price outside a rule or limit
        51032 => ErrorKind::InvalidRequest, // Closing all the positions at the market price.
        51033 => ErrorKind::InvalidRequest, // size / value outside a limit
        51034 => ErrorKind::InvalidRequest, // refused by a venue rule
        51035 => ErrorKind::Auth, // This account doesn't have permission to submit MM quote order.
        51036 => ErrorKind::InvalidRequest, // options rule
        51037 => ErrorKind::InvalidRequest, // account risk state limits orders
        51038 => ErrorKind::InvalidRequest, // refused by a venue rule
        51039 => ErrorKind::InvalidRequest, // position rule
        51040 => ErrorKind::InvalidRequest, // position rule
        51041 => ErrorKind::InvalidRequest, // account / margin mode rule
        51042 => ErrorKind::InvalidRequest, // account / margin mode rule
        51043 => ErrorKind::InvalidRequest, // This isolated position doesn't exist.
        51044 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51046 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51047 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51048 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51049 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51050 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51051 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51052 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51053 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        // 51054 and 51149 are 50004's twins on the order path (HTTP 500): "Request timed out" and
        // "Order timed out ... (does not mean that the request was successful or failed, please
        // check the request result)". The order may rest, so they re-query like 50004.
        51054 => ErrorKind::Timeout, // Request timed out. Please try again.
        51055 => ErrorKind::InvalidRequest, // bot rule (the bridge places no bots)
        51056 => ErrorKind::InvalidRequest, // Action not allowed
        51057 => ErrorKind::InvalidRequest, // bot rule (the bridge places no bots)
        51058 => ErrorKind::InvalidRequest, // No available position for this algo order
        51059 => ErrorKind::InvalidRequest, // bot / strategy rule
        51063 => ErrorKind::NotFound, // OrdId does not exist
        51065 => ErrorKind::InvalidRequest, // algoClOrdId already exists.
        51066 => ErrorKind::InvalidRequest, // position rule
        51068 => ErrorKind::InvalidRequest, // id already in algoClOrdId/attachAlgoClOrdId
        51069 => ErrorKind::InvalidRequest, // named item does not exist
        51070 => ErrorKind::InvalidRequest, // account / margin mode rule
        51071 => ErrorKind::InvalidRequest, // refused by a venue rule
        51072 => ErrorKind::InvalidRequest, // copy-trading rule
        51073 => ErrorKind::InvalidRequest, // copy-trading rule
        51074 => ErrorKind::InvalidRequest, // copy-trading rule
        51075 => ErrorKind::InvalidRequest, // copy-trading rule
        51076 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51077 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51078 => ErrorKind::InvalidRequest, // You are a lead trader. Split TPs are not supported.
        51079 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51080 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51081 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51082 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51083 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51084 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51085 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51086 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51087 => ErrorKind::InvalidRequest, // Listing canceled for this crypto
        51088 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51089 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51090 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51091 => ErrorKind::InvalidRequest, // All TP orders in one order must be of the same type.
        51092 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51093 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51094 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51095 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51096 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51098 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51099 => ErrorKind::InvalidRequest, // You can't place TP limit orders as a lead trader.
        51100 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51101 => ErrorKind::InvalidRequest, // size / value outside a limit
        51102 => ErrorKind::InvalidRequest, // order count limit
        51103 => ErrorKind::InvalidRequest, // order count limit
        51104 => ErrorKind::InvalidRequest, // order count limit
        51105 => ErrorKind::InvalidRequest, // order count limit
        51106 => ErrorKind::InvalidRequest, // order count limit
        51107 => ErrorKind::InvalidRequest, // order count limit
        51108 => ErrorKind::InvalidRequest, // price outside a rule or limit
        51109 => ErrorKind::InvalidRequest, // No available offers.
        51110 => ErrorKind::InvalidRequest, // auction / pre-market phase rule
        51111 => ErrorKind::InvalidRequest, // Maximum {param0} orders can be placed in bulk.
        51112 => ErrorKind::InvalidRequest, // close size exceeds the position's available size
        51113 => ErrorKind::RateLimited, // Market-price liquidation requests too frequent.
        51115 => ErrorKind::InvalidRequest, // Cancel all pending close-orders before liquidation.
        51116 => ErrorKind::InvalidRequest, // Order price or trigger price exceeds {param0}.
        51117 => ErrorKind::InvalidRequest, // Pending close-orders count exceeds limit.
        51119 => ErrorKind::InsufficientFunds, // insufficient balance (no longer on the V5...
        51120 => ErrorKind::InvalidRequest, // Order amount is less than {param0}, please try again.
        51121 => ErrorKind::InvalidRequest, // Order quantity must be a multiple of the lot size.
        51122 => ErrorKind::InvalidRequest, // Order price should higher than the min price {param0}
        51123 => ErrorKind::InvalidRequest, // Min price increment is null.
        51124 => ErrorKind::InvalidRequest, // You can only place limit orders during call auction.
        51125 => ErrorKind::InvalidRequest, // position rule
        51126 => ErrorKind::InvalidRequest, // reduce-only rule
        51127 => ErrorKind::InsufficientFunds, // Available balance is 0.
        51128 => ErrorKind::InvalidRequest, // refused by a venue rule
        51129 => ErrorKind::InvalidRequest, // size / value outside a limit
        51130 => ErrorKind::InvalidRequest, // Fixed margin currency error.
        51131 => ErrorKind::InsufficientFunds, // Insufficient balance.
        51132 => ErrorKind::InvalidRequest, // size / value outside a limit
        51133 => ErrorKind::InvalidRequest, // reduce-only rule
        51134 => ErrorKind::InvalidRequest, // reduce-only rule
        51135 => ErrorKind::InvalidRequest, // price outside a rule or limit
        51136 => ErrorKind::InvalidRequest, // closing price hit the limit (min sell price)
        51137 => ErrorKind::InvalidRequest, // The highest price limit for buy orders is {param0}.
        51138 => ErrorKind::InvalidRequest, // The lowest price limit for sell orders is {param0}.
        51139 => ErrorKind::InvalidRequest, // reduce-only rule
        51140 => ErrorKind::InvalidRequest, // not enough sell orders (liquidity)
        51142 => ErrorKind::InvalidRequest, // account / margin mode rule
        51143 => ErrorKind::InvalidRequest, // Insufficient conversion amount
        51144 => ErrorKind::InvalidRequest, // Please use {param0} for closing.
        51147 => ErrorKind::InvalidRequest, // options rule
        51148 => ErrorKind::InvalidRequest, // reduce-only rule
        51149 => ErrorKind::Timeout, // Order timed out: status unknown (51054's twin)
        51150 => ErrorKind::InvalidRequest, // price outside a rule or limit
        51152 => ErrorKind::InvalidRequest, // account / margin mode rule
        51153 => ErrorKind::InvalidRequest, // size / value outside a limit
        51154 => ErrorKind::InvalidRequest, // size / value outside a limit
        51155 => ErrorKind::Auth,    // pair restricted by local compliance
        51156 => ErrorKind::InvalidRequest, // copy-trading rule
        51158 => ErrorKind::InvalidRequest, // account / margin mode rule
        51159 => ErrorKind::InvalidRequest, // open orders/positions block this action
        51162 => ErrorKind::InvalidRequest, // open orders/positions block this action
        51163 => ErrorKind::InvalidRequest, // position rule
        51164 => ErrorKind::InvalidRequest, // copy-trading rule
        51165 => ErrorKind::InvalidRequest, // reduce-only rule
        51166 => ErrorKind::InvalidRequest, // copy-trading rule
        51167 => ErrorKind::InvalidRequest, // open orders/positions block this action
        51168 => ErrorKind::InvalidRequest, // open orders/positions block this action
        51169 => ErrorKind::InvalidRequest, // no position in this direction to reduce/close
        51170 => ErrorKind::InvalidRequest, // reduce-only rule
        51171 => ErrorKind::InvalidRequest, // reduce-only rule
        51173 => ErrorKind::InvalidRequest, // price outside a rule or limit
        51174 => ErrorKind::InvalidRequest, // order count limit
        51175 => ErrorKind::InvalidRequest, // required parameter missing
        51176 => ErrorKind::InvalidRequest, // parameter rule
        51177 => ErrorKind::InvalidRequest, // price outside a rule or limit
        51178 => ErrorKind::InvalidRequest, // required parameter missing
        51179 => ErrorKind::InvalidRequest, // account / margin mode rule
        51180 => ErrorKind::InvalidRequest, // The range of {param0} should be ({param1}~{param2})
        51181 => ErrorKind::InvalidRequest, // ordType must be limit while placing {param0} orders
        51182 => ErrorKind::InvalidRequest, // price outside a rule or limit
        51185 => ErrorKind::InvalidRequest, // size / value outside a limit
        51186 => ErrorKind::InvalidRequest, // order leverage above the platform limit
        51187 => ErrorKind::InvalidRequest, // open orders/positions block this action
        51192 => ErrorKind::InvalidRequest, // price outside a rule or limit
        51193 => ErrorKind::InvalidRequest, // price outside a rule or limit
        51194 => ErrorKind::InvalidRequest, // price outside a rule or limit
        51195 => ErrorKind::InvalidRequest, // price outside a rule or limit
        51196 => ErrorKind::InvalidRequest, // auction / pre-market phase rule
        51197 => ErrorKind::InvalidRequest, // auction / pre-market phase rule
        51201 => ErrorKind::InvalidRequest, // The value of a market order can't exceed {param0}.
        51202 => ErrorKind::InvalidRequest, // Market order amount exceeds the maximum amount.
        51203 => ErrorKind::InvalidRequest, // Order amount exceeds the limit {param0}.
        51204 => ErrorKind::InvalidRequest, // The price for the limit order cannot be empty.
        51205 => ErrorKind::InvalidRequest, // Reduce Only is not available.
        51206 => ErrorKind::InvalidRequest, // reduce-only rule
        51207 => ErrorKind::InvalidRequest, // price outside a rule or limit
        51209 => ErrorKind::InvalidRequest, // account risk state limits orders
        51210 => ErrorKind::InvalidRequest, // account risk state limits orders
        51211 => ErrorKind::InvalidRequest, // account risk state limits orders
        51212 => ErrorKind::InvalidRequest, // refused by a venue rule
        51213 => ErrorKind::InvalidRequest, // account risk state limits orders
        51214 => ErrorKind::InvalidRequest, // account risk state limits orders
        51220 => ErrorKind::InvalidRequest, // bot rule (the bridge places no bots)
        51221 => ErrorKind::InvalidRequest, // The profit-sharing ratio must be between 0% and 30%
        51222 => ErrorKind::InvalidRequest, // Profit sharing isn't supported for this type of bot
        51223 => ErrorKind::InvalidRequest, // Only lead bot creators can set profit-sharing ratio
        51224 => ErrorKind::InvalidRequest, // Profit sharing isn't supported for this crypto pair
        51225 => ErrorKind::InvalidRequest, // Instant trigger isn't available for follow bots
        51226 => ErrorKind::InvalidRequest, // Editing parameters isn't available for follow bots
        51250 => ErrorKind::InvalidRequest, // Algo order price is out of the available range.
        51251 => ErrorKind::InvalidRequest, // bot rule (the bridge places no bots)
        51252 => ErrorKind::InvalidRequest, // Algo order amount is out of the available range.
        51253 => ErrorKind::InvalidRequest, // bot rule (the bridge places no bots)
        51254 => ErrorKind::InvalidRequest, // Iceberg average amount error occurred.
        51255 => ErrorKind::InvalidRequest, // bot rule (the bridge places no bots)
        51256 => ErrorKind::InvalidRequest, // Iceberg order price variance error.
        51257 => ErrorKind::InvalidRequest, // trailing order rule
        51258 => ErrorKind::InvalidRequest, // trailing order rule
        51259 => ErrorKind::InvalidRequest, // trailing order rule
        51260 => ErrorKind::InvalidRequest, // trailing order rule
        51261 => ErrorKind::InvalidRequest, // parameter rule
        51262 => ErrorKind::InvalidRequest, // bot rule (the bridge places no bots)
        51263 => ErrorKind::InvalidRequest, // bot rule (the bridge places no bots)
        51264 => ErrorKind::InvalidRequest, // bot rule (the bridge places no bots)
        51265 => ErrorKind::InvalidRequest, // Time-weighted order limit error.
        51267 => ErrorKind::InvalidRequest, // Time-weighted order strategy initiative rate error.
        51268 => ErrorKind::InvalidRequest, // Time-weighted order strategy initiative range error.
        51269 => ErrorKind::InvalidRequest, // bot rule (the bridge places no bots)
        51270 => ErrorKind::InvalidRequest, // bot rule (the bridge places no bots)
        51271 => ErrorKind::InvalidRequest, // Sweep ratio must be 0 < x <= 100%.
        51272 => ErrorKind::InvalidRequest, // Price variance must be 0 < x <= 1%.
        51273 => ErrorKind::InvalidRequest, // Total amount must be greater than {param0}.
        51274 => ErrorKind::InvalidRequest, // bot rule (the bridge places no bots)
        51275 => ErrorKind::InvalidRequest, // size / value outside a limit
        51276 => ErrorKind::InvalidRequest, // Prices cannot be specified for stop market orders.
        51277 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51278 => ErrorKind::InvalidRequest, // SL trigger price cannot be lower than the last price.
        51279 => ErrorKind::InvalidRequest, // TP trigger price cannot be lower than the last price.
        51280 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51281 => ErrorKind::InvalidRequest, // Trigger order do not support the tgtCcy parameter.
        51282 => ErrorKind::InvalidRequest, // The range of Price variance is {param0}~{param1}
        51283 => ErrorKind::InvalidRequest, // The range of Time interval is {param0}~{param1}
        51284 => ErrorKind::InvalidRequest, // The range of Average amount is {param0}~{param1}
        51285 => ErrorKind::InvalidRequest, // The range of Total amount is {param0}~{param1}
        51286 => ErrorKind::InvalidRequest, // The total amount should not be less than {param0}
        51287 => ErrorKind::InvalidRequest, // This bot doesn't support current instrument
        51288 => ErrorKind::InvalidRequest, // bot rule (the bridge places no bots)
        51289 => ErrorKind::InvalidRequest, // bot rule (the bridge places no bots)
        51290 => ErrorKind::VenueMaintenance, // bot engine upgrading
        51291 => ErrorKind::InvalidRequest, // This Bot does not exist or has been stopped
        51292 => ErrorKind::InvalidRequest, // This Bot type does not exist
        51293 => ErrorKind::InvalidRequest, // This Bot does not exist
        51294 => ErrorKind::VenueMaintenance, // bot cannot be created temporarily
        51295 => ErrorKind::InvalidRequest, // bot rule (the bridge places no bots)
        51298 => ErrorKind::InvalidRequest, // account / margin mode rule
        51299 => ErrorKind::InvalidRequest, // order count limit
        51300 => ErrorKind::InvalidRequest, // TP trigger price cannot be higher than the mark price
        51302 => ErrorKind::InvalidRequest, // SL trigger price cannot be lower than the mark price
        51303 => ErrorKind::InvalidRequest, // TP trigger price cannot be lower than the mark price
        51304 => ErrorKind::InvalidRequest, // SL trigger price cannot be higher than the mark price
        51305 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51306 => ErrorKind::InvalidRequest, // SL trigger price cannot be lower than the index price
        51307 => ErrorKind::InvalidRequest, // TP trigger price cannot be lower than the index price
        51308 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51309 => ErrorKind::InvalidRequest, // Cannot create trading bot during call auction
        51310 => ErrorKind::InvalidRequest, // bot rule (the bridge places no bots)
        51311 => ErrorKind::InvalidRequest, // trailing order rule
        51312 => ErrorKind::InvalidRequest, // trailing order rule
        51313 => ErrorKind::InvalidRequest, // bot rule (the bridge places no bots)
        51317 => ErrorKind::InvalidRequest, // Trigger orders are not available by margin
        51320 => ErrorKind::InvalidRequest, // The range of coin percentage is {0}%-{1}%
        51321 => ErrorKind::InvalidRequest, // copy-trading rule
        51322 => ErrorKind::InvalidRequest, // copy-trading rule
        51323 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51324 => ErrorKind::InvalidRequest, // copy-trading rule
        51325 => ErrorKind::InvalidRequest, // copy-trading rule
        51326 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51327 => ErrorKind::InvalidRequest, // close-fraction rule
        51328 => ErrorKind::InvalidRequest, // closeFraction is only available for reduceOnly orders
        51329 => ErrorKind::InvalidRequest, // closeFraction is only available in NET mode
        51330 => ErrorKind::InvalidRequest, // close-fraction rule
        51331 => ErrorKind::InvalidRequest, // close-fraction rule
        51332 => ErrorKind::InvalidRequest, // closeFraction is not applicable to Portfolio Margin
        51333 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51340 => ErrorKind::InvalidRequest, // Used margin must be greater than {0}{1}
        51341 => ErrorKind::InvalidRequest, // Position closing not allowed
        51342 => ErrorKind::InvalidRequest, // Closing order already exists. Please try again later
        51343 => ErrorKind::InvalidRequest, // TP price must be less than the lower price
        51344 => ErrorKind::InvalidRequest, // SL price must be greater than the upper price
        51345 => ErrorKind::InvalidRequest, // Policy type is not grid policy
        51346 => ErrorKind::InvalidRequest, // bot rule (the bridge places no bots)
        51347 => ErrorKind::InvalidRequest, // No profit available
        51348 => ErrorKind::InvalidRequest, // bot rule (the bridge places no bots)
        51349 => ErrorKind::InvalidRequest, // bot rule (the bridge places no bots)
        51350 => ErrorKind::InvalidRequest, // No recommended parameters
        51351 => ErrorKind::InvalidRequest, // Single income must be greater than 0
        51352 => ErrorKind::InvalidRequest, // You can have {0} to {1} trading pairs
        51353 => ErrorKind::InvalidRequest, // Trading pair {0} already exists
        51354 => ErrorKind::InvalidRequest, // bot rule (the bridge places no bots)
        51355 => ErrorKind::InvalidRequest, // Select a date within {0} - {1}
        51356 => ErrorKind::InvalidRequest, // Select a time within {0} - {1}
        51357 => ErrorKind::InvalidRequest, // Select a time zone within {0} - {1}
        51358 => ErrorKind::InvalidRequest, // bot rule (the bridge places no bots)
        51359 => ErrorKind::InvalidRequest, // bot rule (the bridge places no bots)
        51370 => ErrorKind::InvalidRequest, // The range of lever is {0}~{1}
        51380 => ErrorKind::InvalidRequest, // bot rule (the bridge places no bots)
        51381 => ErrorKind::InvalidRequest, // bot rule (the bridge places no bots)
        51382 => ErrorKind::InvalidRequest, // bot rule (the bridge places no bots)
        51383 => ErrorKind::InvalidRequest, // The min_price is lower than the last price
        51384 => ErrorKind::InvalidRequest, // The trigger price must be greater than the min price
        51385 => ErrorKind::InvalidRequest, // bot rule (the bridge places no bots)
        51386 => ErrorKind::InvalidRequest, // bot rule (the bridge places no bots)
        51387 => ErrorKind::InvalidRequest, // Stop loss price must be less than the bottom price
        51388 => ErrorKind::InvalidRequest, // This Bot is in running status
        51389 => ErrorKind::InvalidRequest, // Trigger price should be lower than {0}
        51390 => ErrorKind::InvalidRequest, // Trigger price should be lower than the TP price
        51391 => ErrorKind::InvalidRequest, // Trigger price should be higher than the SL price
        51392 => ErrorKind::InvalidRequest, // TP price should be higher than the trigger price
        51393 => ErrorKind::InvalidRequest, // SL price should be lower than the trigger price
        51394 => ErrorKind::InvalidRequest, // Trigger price should be higher than the TP price
        51395 => ErrorKind::InvalidRequest, // Trigger price should be lower than the SL price
        51396 => ErrorKind::InvalidRequest, // TP price should be lower than the trigger price
        51397 => ErrorKind::InvalidRequest, // SL price should be higher than the trigger price
        51398 => ErrorKind::InvalidRequest, // bot rule (the bridge places no bots)
        51399 => ErrorKind::InvalidRequest, // bot rule (the bridge places no bots)
        51400 => ErrorKind::NotFound, // cancel: order filled, cancelled or absent
        51401 => ErrorKind::NotFound, // cancel: already cancelled
        51402 => ErrorKind::NotFound, // cancel: already completed
        51403 => ErrorKind::InvalidRequest, // order type does not support cancellation
        51404 => ErrorKind::InvalidRequest, // cancel unavailable in call auction phase 2
        51405 => ErrorKind::NotFound, // Cancellation failed as you don't have any pending orders.
        51406 => ErrorKind::InvalidRequest, // Canceled - order count exceeds the limit {param0}.
        51407 => ErrorKind::InvalidRequest, // Either order ID or client order ID is required.
        51408 => ErrorKind::InvalidRequest, // Pair ID or name doesn't match the order info.
        51409 => ErrorKind::InvalidRequest, // Either pair ID or pair name ID is required.
        51410 => ErrorKind::NotFound, // cancel: already cancelling or pending settlement
        51411 => ErrorKind::Auth,    // Account does not have permission for mass cancellation.
        51413 => ErrorKind::InvalidRequest, // order type not supported by this endpoint
        51415 => ErrorKind::InvalidRequest, // spot trigger must use last price
        51416 => ErrorKind::InvalidRequest, // Order has been triggered and can't be canceled
        51500 => ErrorKind::InvalidRequest, // You must enter a price, quantity, or TP/SL
        51501 => ErrorKind::InvalidRequest, // Maximum {param0} orders can be modified.
        51502 => ErrorKind::InsufficientFunds, // amend: insufficient balance / margin / borrow
        51503 => ErrorKind::NotFound, // Your order has already been filled or canceled.
        51505 => ErrorKind::InvalidRequest, // {instId} is not in call auction
        51506 => ErrorKind::InvalidRequest, // Order modification unavailable for the order type.
        51507 => ErrorKind::InvalidRequest, // market orders only 5 min after listing
        51508 => ErrorKind::InvalidRequest, // auction / pre-market phase rule
        51509 => ErrorKind::NotFound, // amend: order cancelled
        51510 => ErrorKind::NotFound, // amend: order completed
        51511 => ErrorKind::InvalidRequest, // amend price violates post-only
        51512 => ErrorKind::InvalidRequest, // duplicate orders in one amend batch
        51513 => ErrorKind::RateLimited, // 3 amends already in progress
        51514 => ErrorKind::InvalidRequest, // price outside a rule or limit
        51521 => ErrorKind::InvalidRequest, // reduce-only rule
        51522 => ErrorKind::InvalidRequest, // reduce-only rule
        51523 => ErrorKind::InvalidRequest, // price outside a rule or limit
        51524 => ErrorKind::InvalidRequest, // price outside a rule or limit
        51525 => ErrorKind::InvalidRequest, // refused by a venue rule
        51526 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51527 => ErrorKind::NotFound, // amend: attached TP/SL or stop order does not exist
        51528 => ErrorKind::InvalidRequest, // Unable to modify trigger price type
        51529 => ErrorKind::InvalidRequest, // refused by a venue rule
        51530 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51531 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51532 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51536 => ErrorKind::InvalidRequest, // price outside a rule or limit
        51537 => ErrorKind::InvalidRequest, // options rule
        51538 => ErrorKind::InvalidRequest, // refused by a venue rule
        51539 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        51543 => ErrorKind::InvalidRequest, // price outside a rule or limit
        51600 => ErrorKind::InvalidRequest, // Status not found.
        51601 => ErrorKind::InvalidRequest, // state and order id cannot both be given
        51602 => ErrorKind::InvalidRequest, // Either order status or order ID is required.
        51603 => ErrorKind::NotFound, // Order does not exist.
        51604 => ErrorKind::InvalidRequest, // download / affiliate / repay / convert endpoint...
        51605 => ErrorKind::InvalidRequest, // download / affiliate / repay / convert endpoint...
        51606 => ErrorKind::InvalidRequest, // download / affiliate / repay / convert endpoint...
        51607 => ErrorKind::InvalidRequest, // download / affiliate / repay / convert endpoint...
        51608 => ErrorKind::InvalidRequest, // No transaction data found for the current quarter
        51610 => ErrorKind::InvalidRequest, // download / affiliate / repay / convert endpoint...
        51611 => ErrorKind::InvalidRequest, // download / affiliate / repay / convert endpoint...
        51620 => ErrorKind::InvalidRequest, // Only affiliates can perform this action
        51621 => ErrorKind::InvalidRequest, // The user isn't your invitee
        51625 => ErrorKind::InvalidRequest, // download / affiliate / repay / convert endpoint...
        51820 => ErrorKind::InvalidRequest, // Request failed
        51821 => ErrorKind::InvalidRequest, // The payment method is not supported
        51822 => ErrorKind::InvalidRequest, // Quote expired
        51823 => ErrorKind::InvalidRequest, // download / affiliate / repay / convert endpoint...
        51824 => ErrorKind::InvalidRequest, // download / affiliate / repay / convert endpoint...
        51825 => ErrorKind::InvalidRequest, // download / affiliate / repay / convert endpoint...
        // ---- 54xxx: trade -----------------------------------------------------------------------
        54000 => ErrorKind::InvalidRequest, // Margin trading is not supported.
        54001 => ErrorKind::InvalidRequest, // only multi-currency margin can auto-borrow
        54004 => ErrorKind::InvalidRequest, // batch: refused because another order in it failed
        54005 => ErrorKind::InvalidRequest, // auction / pre-market phase rule
        54006 => ErrorKind::InvalidRequest, // auction / pre-market phase rule
        54007 => ErrorKind::InvalidRequest, // Instrument {instId} is not supported
        54008 => ErrorKind::InvalidRequest, // disabled by the mass-cancel endpoint
        54009 => ErrorKind::InvalidRequest, // The range of {param0} should be [{param1}, {param2}].
        54011 => ErrorKind::InvalidRequest, // auction / pre-market phase rule
        54012 => ErrorKind::InvalidRequest, // thin order book: only reducing actions allowed
        54018 => ErrorKind::InvalidRequest, // auction / pre-market phase rule
        54019 => ErrorKind::InvalidRequest, // auction / pre-market phase rule
        54024 => ErrorKind::InvalidRequest, // collateral setting required
        54025 => ErrorKind::InvalidRequest, // collateral setting required
        54026 => ErrorKind::InvalidRequest, // collateral setting required
        54027 => ErrorKind::InvalidRequest, // collateral setting required
        54028 => ErrorKind::InvalidRequest, // collateral setting required
        54029 => ErrorKind::InvalidRequest, // {param0} doesn't exist within {param1}.
        54030 => ErrorKind::InvalidRequest, // order count limit
        54031 => ErrorKind::InvalidRequest, // order count limit
        54035 => ErrorKind::InvalidRequest, // reduce-only rule
        54036 => ErrorKind::InvalidRequest, // FOK not allowed with self-trade prevention on...
        54039 => ErrorKind::InvalidRequest, // RPI orders can't be reduce-only orders.
        54040 => ErrorKind::InvalidRequest, // RPI orders can't be used with TP/SL settings.
        54041 => ErrorKind::InvalidRequest, // RPI orders aren't supported for {param0}.
        54042 => ErrorKind::Auth, // You don't have permission to place RPI orders for {param0}.
        54043 => ErrorKind::InvalidRequest, // RPI order rule
        54044 => ErrorKind::InvalidRequest, // RPI order rule
        54045 => ErrorKind::InvalidRequest, // refused by a venue rule
        54046 => ErrorKind::InvalidRequest, // You can't place orders to take RPI liquidity.
        54047 => ErrorKind::InvalidRequest, // amend: same id is in speed bump
        54048 => ErrorKind::InvalidRequest, // cancel: same id is in speed bump
        54049 => ErrorKind::ServerError, // RPI-taking orders refused now (system busy)
        54051 => ErrorKind::InvalidRequest, // required parameter missing
        54070 => ErrorKind::InvalidRequest, // refused by a venue rule
        54071 => ErrorKind::InvalidRequest, // order no longer amendable after upgrade
        54072 => ErrorKind::InvalidRequest, // instrument state forbids it
        54073 => ErrorKind::InvalidRequest, // settlement currency at depeg risk
        54074 => ErrorKind::InvalidRequest, // settings failed: USD contract positions/orders
        54075 => ErrorKind::InvalidRequest, // cross-crypto contracts unsupported in this...
        54078 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        54079 => ErrorKind::InvalidRequest, // TP/SL attachment rule
        54092 => ErrorKind::Auth, // TradFi perps disclaimer not accepted
        54094 => ErrorKind::InvalidRequest, // cool-off period active for instId
        54101 => ErrorKind::Auth, // required questionnaire not completed
        54109 => ErrorKind::Auth, // trading not activated for this pair
        // ---- 59xxx: account (leverage, position mode, margin, settings) -------------------------
        59000 => ErrorKind::InvalidRequest, // settings change needs no open positions/orders
        59001 => ErrorKind::InvalidRequest, // Switching unavailable as you have borrowings.
        59002 => ErrorKind::InvalidRequest, // sub-account settings: close positions/orders/bots
        59004 => ErrorKind::InvalidRequest, // Only IDs with the same instrument type are supported
        59101 => ErrorKind::InvalidRequest, // leverage change: cancel pending isolated orders
        59102 => ErrorKind::InvalidRequest, // leverage above the maximum
        59103 => ErrorKind::InsufficientFunds, // margin insufficient, leverage too low
        59104 => ErrorKind::InvalidRequest, // borrowed position over the max for this leverage
        59105 => ErrorKind::InvalidRequest, // leverage below the minimum
        59106 => ErrorKind::InvalidRequest, // max available margin of the tier
        59107 => ErrorKind::InvalidRequest, // leverage change: cancel pending cross orders
        59108 => ErrorKind::InsufficientFunds, // leverage too low, margin insufficient
        59109 => ErrorKind::InsufficientFunds, // equity below required margin after adjustment
        59110 => ErrorKind::InvalidRequest, // instrument does not support tgtCcy
        59111 => ErrorKind::InvalidRequest, // leverage query unsupported in portfolio margin
        59112 => ErrorKind::InvalidRequest, // leverage change: cancel isolated/cross pending orders
        59113 => ErrorKind::Auth,           // margin trading unavailable in region
        59114 => ErrorKind::Auth,           // margin trading unavailable in region
        59200 => ErrorKind::InsufficientFunds, // Insufficient account balance.
        59201 => ErrorKind::InsufficientFunds, // Negative account balance.
        59509 => ErrorKind::Auth,           // Account does not have permission to reset MMP status
        59668 => ErrorKind::InvalidRequest, // leverage change: cancel isolated algo orders/bots
        59669 => ErrorKind::InvalidRequest, // leverage change: cancel cross algo orders/bots
        59670 => ErrorKind::InvalidRequest, // leverage change: too many open orders
        _ => return None,
    })
}

/// Does this code say only that the SERVER had trouble, so that it says nothing about what became of
/// the request? [`by_code`]'s rows, from the V5 reference this module cites: `50001` "service temporarily
/// unavailable", `50013` "system busy", `50026` "system error; try again later". `50005` ("API endpoint
/// offline or unavailable") is not one: an endpoint that is offline took nothing. With the transport's
/// sentinel and the rows filed under [`ErrorKind::Timeout`] (`50004`, `51054`, `51149`: the venue itself says the
/// request may have succeeded or failed; each arrives as the body's code, so it is not the sentinel), it makes [`TAXONOMY`]'s
/// [`VenueTaxonomy::submit_outcome_unknown`]: `true` means re-query (`resolve_ambiguous_submit`, `order-algo` for
/// a stop), never a terminal `OrderRejected`. The ONE rule behind every submit arm in
/// `crates/bridges/okx/src/execution/rest/mod.rs`, `execution/rest/batch.rs` and `execution/rest/stop_algo.rs`.
pub fn server_had_trouble(code: i64) -> bool {
    matches!(code, 50001 | 50013 | 50026)
}

/// Does this code say an order already rests under the submit's `clOrdId` (a stop's `algoClOrdId`)? `51016`
/// "Client order ID already exists." (HTTP 200, a per-order `sCode`), `50071` "{param} already exists e.g.
/// clOrdId already exists" and, on `order-algo`, `51065` "algoClOrdId already exists." (the V5 page, read
/// 2026-11-23), under the v5 guide's rule that a `clOrdId` is unique among the account's PENDING (live or
/// partially filled) orders. It is no refusal of OUR order: the core refuses an order client id it has already
/// seen (`crates/vike-core/src/runtime/recent_order_client_ids.rs`), so the pending order under it is one we sent
/// (decision 0303). With [`server_had_trouble`] it is [`TAXONOMY`]'s `leaves_write_open`.
pub fn says_order_client_id_taken(code: i64) -> bool {
    matches!(code, 51016 | 50071 | 51065)
}

/// Does a CANCEL answered `code` say the order is gone? The filled / already-canceled / does-not-exist
/// family, the REGULAR book's "unknown" answer. A resting STOP (`order-algo`) is not in that book, so
/// this answer is about the wrong endpoint and proves nothing about a stop: `cancel_order` retries it on
/// the algo endpoint (`crates/bridges/okx/src/execution/rest/stop_algo.rs`'s `cancel_algo`) before it reads the
/// order as gone, and the cancel batch does the same per row. `algo_unknown` and the regular lookup's
/// "absent" read this set too.
///
/// **The set is exactly the words `cancel_order` swallowed before stops were cancelled on
/// `cancel-algos`, so every OTHER regular answer (`51603`, `51413`, `51403`, ...) stays the `Err` it
/// always was.** Measured 2026-10-23 on the demo account (`tests/okx_smokes/okx_algo_stop_smoke.rs`'s
/// `MEASURED regular cancel-order` line): `cancel-order` of a STOP's order client id answers `51400`, and the
/// stop survives it. To add a code, add exactly the one a printed line shows, with a row in
/// `tests/offline/algo_stop.rs`'s pin tests.
pub fn cancel_says_gone(code: i64) -> bool {
    matches!(code, 51400..=51402)
}

/// The non-terminal advisory for a failed amend. A status-unknown code
/// ([`VenueTaxonomy::submit_outcome_unknown`]) is worded as the timeout sentinel is (`modify outcome unknown`:
/// the amend may have been applied) instead of as a refusal; a definite one keeps the venue's words. Stamped
/// with `stated`, the refusal reply row's own `ts`, else when the error was received
/// (`vike_bridge_core::rest::amend_answer_time`): a [`VenueApiError`] keeps no time, so the caller reads it off
/// the reply before unwrapping it.
pub fn modify_failed(order_client_id: &str, exc: &VenueApiError, stated: Option<i64>) -> Event {
    let ts = vike_bridge_core::rest::amend_answer_time(stated);
    let unknown = |code| TAXONOMY.submit_outcome_unknown(code);
    vike_bridge_core::rest::modify_failed_with(order_client_id, exc, unknown, ts)
}

/// Message fallback for OKX bodies that arrive without a usable numeric (gateway HTML, an
/// `sCode`-less envelope).
pub fn by_msg(msg: &str) -> Option<ErrorKind> {
    let m = msg.to_ascii_lowercase();
    // ANCHORED to balance/margin wording: a bare `contains("insufficient")` also matches bodies that
    // have nothing to do with the account ("insufficient permissions" on an auth reject, "insufficient
    // upstream capacity" from a gateway), and InsufficientFunds is TERMINAL — misfiring there turns a
    // transient outage into rejected orders. (The `classify_venue` disposition guard now blocks that
    // flip structurally too; this keeps the table honest on its own.)
    if m.contains("insufficient")
        && ["balance", "margin", "funds", "equity"].iter().any(|w| m.contains(w))
    {
        return Some(ErrorKind::InsufficientFunds);
    }
    if m.contains("maintenance") || m.contains("system busy") || m.contains("upgrade") {
        return Some(ErrorKind::VenueMaintenance);
    }
    if m.contains("too frequent") || m.contains("rate limit") {
        return Some(ErrorKind::RateLimited);
    }
    if m.contains("does not exist") || m.contains("already canceled") {
        return Some(ErrorKind::NotFound);
    }
    None
}

/// OKX's taxonomy, ready to thread into [`vike_bridge_core::error_kind::classify_venue`].
pub const TAXONOMY: VenueTaxonomy = VenueTaxonomy {
    venue: "okx",
    by_code,
    by_msg,
    leaves_write_open: |code| server_had_trouble(code) || says_order_client_id_taken(code),
};

#[cfg(test)]
mod tests {
    use super::*;
    use vike_bridge_core::error_kind::classify_venue;
    use vike_bridge_core::transport::VenueApiError;

    fn kind(code: i64, msg: &str) -> ErrorKind {
        classify_venue(Some(&TAXONOMY), &VenueApiError { code, msg: msg.to_string() })
    }

    #[test]
    fn documented_codes_classify() {
        let cases = [
            (50111, ErrorKind::Auth),
            (50113, ErrorKind::Auth),
            (50105, ErrorKind::Auth),
            (50110, ErrorKind::Auth),
            (50007, ErrorKind::Auth),
            (50011, ErrorKind::RateLimited),
            (50061, ErrorKind::RateLimited),
            (50001, ErrorKind::VenueMaintenance),
            (50004, ErrorKind::Timeout),
            (50005, ErrorKind::VenueMaintenance),
            (50013, ErrorKind::VenueMaintenance),
            (50026, ErrorKind::ServerError),
            (50000, ErrorKind::InvalidRequest),
            (50102, ErrorKind::InvalidRequest),
            (51000, ErrorKind::InvalidRequest),
            (51001, ErrorKind::InvalidRequest),
            (51020, ErrorKind::InvalidRequest),
            (51008, ErrorKind::InsufficientFunds),
            (51119, ErrorKind::InsufficientFunds),
            (51131, ErrorKind::InsufficientFunds),
            (51400, ErrorKind::NotFound),
            (51401, ErrorKind::NotFound),
            (51503, ErrorKind::NotFound),
            (51603, ErrorKind::NotFound),
            (51054, ErrorKind::Timeout),
            (51149, ErrorKind::Timeout),
            (1, ErrorKind::InvalidRequest),
            (2, ErrorKind::InvalidRequest),
            (59000, ErrorKind::InvalidRequest),
            (51029, ErrorKind::VenueMaintenance),
            (51030, ErrorKind::VenueMaintenance),
        ];
        for (code, want) in cases {
            assert_eq!(kind(code, ""), want, "okx {code}");
        }
    }

    /// REGRESSION (adversarial review, critical): `50004` is "Endpoint request timeout (does not
    /// mean the request was successful or failed, please check the request result)" — an AMBIGUOUS
    /// outcome. It must land in the must-re-query bucket, NEVER a retry: re-issuing a POST
    /// /api/v5/trade/order the venue may already hold double-submits the order.
    #[test]
    fn okx_50004_is_ambiguous_and_must_requery_never_retry() {
        let k = kind(50004, "Endpoint request timeout");
        assert_eq!(
            k,
            ErrorKind::Timeout,
            "50004 is an ambiguous outcome, not a maintenance window"
        );
        assert!(k.must_requery(), "50004 must force a status re-query");
        assert!(!k.is_retryable(), "blind-retrying 50004 double-submits a live order");
        assert!(!k.is_terminal(), "50004 must not synthesize a reject either (phantom position)");
        assert!(TAXONOMY.submit_outcome_unknown(50004), "the write rule re-queries it");
    }

    /// The write rule over EVERY row: exactly the three `Timeout` rows, the server trouble and the taken order
    /// client id leave a write open, besides the sentinel. The envelope's `1`/`2` and the account writes' `59000` are
    /// refusals by their rows (a write's verdict, not an unmapped code).
    #[test]
    fn submit_outcome_unknown_is_exactly_the_sentinel_the_timeout_rows_and_the_server_trouble() {
        use vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS;
        // rows only: an unmapped code would be reported (a process-wide, bounded set)
        let rows = (1..60_000).filter(|c| by_code(*c).is_some());
        let open: Vec<i64> = rows.filter(|c| TAXONOMY.submit_outcome_unknown(*c)).collect();
        assert_eq!(open, [50001, 50004, 50013, 50026, 50071, 51016, 51054, 51065, 51149]);
        assert!(TAXONOMY.submit_outcome_unknown(E_TIMEOUT_AMBIGUOUS));
        // definite refusals, and an endpoint that is offline (it took nothing)
        for code in [0, 1, 2, 51000, 51008, 51603, 50005, 59000, 500] {
            assert!(!TAXONOMY.submit_outcome_unknown(code), "{code}");
        }
    }

    /// The OKX nuance: the funding-window codes are a MAINTENANCE window, not an ordinary 5xx — still
    /// retryable, but minutes long.
    #[test]
    fn funding_window_codes_are_maintenance_not_a_server_blip() {
        for code in [50001, 50005, 50013, 51029, 51030] {
            let k = kind(code, "");
            assert_eq!(k, ErrorKind::VenueMaintenance, "okx {code}");
            assert!(k.is_retryable(), "a maintenance window is still retryable");
        }
    }

    /// Today's set, code by code, and what the table files under `NotFound` beyond it.
    #[test]
    fn cancel_says_gone_is_exactly_todays_set() {
        let codes = 0..200_000i64;
        let gone: Vec<i64> = codes.clone().filter(|c| cancel_says_gone(*c)).collect();
        assert_eq!(gone, [51400, 51401, 51402]);
        let table_only: Vec<i64> = codes
            .filter(|c| by_code(*c) == Some(ErrorKind::NotFound) && !cancel_says_gone(*c))
            .collect();
        assert_eq!(table_only, [51063, 51405, 51410, 51503, 51509, 51510, 51527, 51603]);
    }

    /// An undocumented code is unmapped: never final (its outcome unknown); a code this bridge mints
    /// for a refusal of its own stays a refusal.
    #[test]
    fn an_unmapped_code_is_never_final() {
        let k = kind(59_999, "brand new failure");
        assert_eq!(k, ErrorKind::UnmappedCode);
        assert!(!k.is_terminal() && !k.is_retryable());
        for own in
            [crate::execution::E_NO_INSTRUMENT_ROW, crate::execution::rest::E_UNCLASSED_INSTRUMENT]
        {
            assert_eq!(kind(own, "refused here"), ErrorKind::Unknown, "{own}");
        }
    }

    #[test]
    fn message_fallback_catches_codeless_bodies() {
        assert_eq!(kind(502, "system busy, try later"), ErrorKind::VenueMaintenance);
        assert_eq!(kind(400, "Insufficient USDT balance"), ErrorKind::InsufficientFunds);
    }

    /// The codes `ErrorKind::classify` held for OKX until 2026-11-23 keep their disposition as this
    /// table's rows. `50001` is deliberately re-bucketed within its disposition (`ServerError` →
    /// `VenueMaintenance`, both retryable).
    #[test]
    fn the_moved_seed_codes_keep_their_disposition() {
        use vike_bridge_core::transport::ErrorKind::{
            Auth, InvalidRequest, NotFound, RateLimited, ServerError,
        };
        vike_bridge_core::error_table::assert_keeps_the_moved_seed_dispositions(
            &TAXONOMY,
            &[
                (50011, RateLimited),
                (50061, RateLimited),
                (50111, Auth),
                (50113, Auth),
                (51603, NotFound),
                (50001, ServerError),
                (51000, InvalidRequest),
            ],
        );
    }

    /// Every row has a venue answer recorded in this repository behind it, or is named here as having
    /// none yet (the list only shrinks: a row given a sample leaves it). The 486 rows transcribed from the
    /// V5 page on 2026-11-23 joined it with their rows: none has a captured answer.
    #[test]
    fn every_row_has_a_recorded_sample_or_is_listed_without_one() {
        use vike_bridge_core::error_table::RowSample;
        vike_bridge_core::error_table::assert_every_row_has_a_sample(
            &TAXONOMY,
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."),
            (1..=2).chain(50_000..60_000i64),
            &[
                RowSample {
                    code: 50016,
                    words: "instId and instType don't match",
                    recorded_in: "crates/bridges/okx/src/reconcile/mod.rs",
                },
                RowSample {
                    code: 51000,
                    words: "Parameter tdMode error",
                    recorded_in: "crates/bridges/okx/tests/offline/classes.rs",
                },
            ],
            &[
                1, 2, 50000, 50001, 50002, 50004, 50005, 50006, 50007, 50008, 50009, 50010, 50011,
                50012, 50013, 50014, 50015, 50017, 50018, 50019, 50020, 50021, 50022, 50023, 50024,
                50025, 50026, 50027, 50028, 50029, 50030, 50032, 50033, 50035, 50036, 50037, 50038,
                50039, 50040, 50041, 50042, 50044, 50045, 50046, 50047, 50048, 50049, 50050, 50051,
                50052, 50053, 50055, 50056, 50057, 50058, 50059, 50060, 50061, 50062, 50063, 50064,
                50067, 50069, 50071, 50072, 50075, 50076, 50077, 50100, 50101, 50102, 50103, 50104,
                50105, 50106, 50107, 50108, 50109, 50110, 50111, 50112, 50113, 50114, 50115, 50116,
                50118, 50119, 50120, 50121, 50122, 50123, 51001, 51002, 51003, 51004, 51005, 51006,
                51007, 51008, 51009, 51010, 51011, 51012, 51014, 51015, 51016, 51017, 51018, 51019,
                51020, 51021, 51022, 51023, 51024, 51025, 51026, 51027, 51028, 51029, 51030, 51031,
                51032, 51033, 51034, 51035, 51036, 51037, 51038, 51039, 51040, 51041, 51042, 51043,
                51044, 51046, 51047, 51048, 51049, 51050, 51051, 51052, 51053, 51054, 51055, 51056,
                51057, 51058, 51059, 51063, 51065, 51066, 51068, 51069, 51070, 51071, 51072, 51073,
                51074, 51075, 51076, 51077, 51078, 51079, 51080, 51081, 51082, 51083, 51084, 51085,
                51086, 51087, 51088, 51089, 51090, 51091, 51092, 51093, 51094, 51095, 51096, 51098,
                51099, 51100, 51101, 51102, 51103, 51104, 51105, 51106, 51107, 51108, 51109, 51110,
                51111, 51112, 51113, 51115, 51116, 51117, 51119, 51120, 51121, 51122, 51123, 51124,
                51125, 51126, 51127, 51128, 51129, 51130, 51131, 51132, 51133, 51134, 51135, 51136,
                51137, 51138, 51139, 51140, 51142, 51143, 51144, 51147, 51148, 51149, 51150, 51152,
                51153, 51154, 51155, 51156, 51158, 51159, 51162, 51163, 51164, 51165, 51166, 51167,
                51168, 51169, 51170, 51171, 51173, 51174, 51175, 51176, 51177, 51178, 51179, 51180,
                51181, 51182, 51185, 51186, 51187, 51192, 51193, 51194, 51195, 51196, 51197, 51201,
                51202, 51203, 51204, 51205, 51206, 51207, 51209, 51210, 51211, 51212, 51213, 51214,
                51220, 51221, 51222, 51223, 51224, 51225, 51226, 51250, 51251, 51252, 51253, 51254,
                51255, 51256, 51257, 51258, 51259, 51260, 51261, 51262, 51263, 51264, 51265, 51267,
                51268, 51269, 51270, 51271, 51272, 51273, 51274, 51275, 51276, 51277, 51278, 51279,
                51280, 51281, 51282, 51283, 51284, 51285, 51286, 51287, 51288, 51289, 51290, 51291,
                51292, 51293, 51294, 51295, 51298, 51299, 51300, 51302, 51303, 51304, 51305, 51306,
                51307, 51308, 51309, 51310, 51311, 51312, 51313, 51317, 51320, 51321, 51322, 51323,
                51324, 51325, 51326, 51327, 51328, 51329, 51330, 51331, 51332, 51333, 51340, 51341,
                51342, 51343, 51344, 51345, 51346, 51347, 51348, 51349, 51350, 51351, 51352, 51353,
                51354, 51355, 51356, 51357, 51358, 51359, 51370, 51380, 51381, 51382, 51383, 51384,
                51385, 51386, 51387, 51388, 51389, 51390, 51391, 51392, 51393, 51394, 51395, 51396,
                51397, 51398, 51399, 51400, 51401, 51402, 51403, 51404, 51405, 51406, 51407, 51408,
                51409, 51410, 51411, 51413, 51415, 51416, 51500, 51501, 51502, 51503, 51505, 51506,
                51507, 51508, 51509, 51510, 51511, 51512, 51513, 51514, 51521, 51522, 51523, 51524,
                51525, 51526, 51527, 51528, 51529, 51530, 51531, 51532, 51536, 51537, 51538, 51539,
                51543, 51600, 51601, 51602, 51603, 51604, 51605, 51606, 51607, 51608, 51610, 51611,
                51620, 51621, 51625, 51820, 51821, 51822, 51823, 51824, 51825, 54000, 54001, 54004,
                54005, 54006, 54007, 54008, 54009, 54011, 54012, 54018, 54019, 54024, 54025, 54026,
                54027, 54028, 54029, 54030, 54031, 54035, 54036, 54039, 54040, 54041, 54042, 54043,
                54044, 54045, 54046, 54047, 54048, 54049, 54051, 54070, 54071, 54072, 54073, 54074,
                54075, 54078, 54079, 54092, 54094, 54101, 54109, 59000, 59001, 59002, 59004, 59101,
                59102, 59103, 59104, 59105, 59106, 59107, 59108, 59109, 59110, 59111, 59112, 59113,
                59114, 59200, 59201, 59509, 59668, 59669, 59670,
            ],
        );
    }

    /// REGRESSION (adversarial review, major/minor): `classify_venue` consults `by_msg` for EVERY error,
    /// so a disposition flip was reachable through a MESSAGE. This runs the FULL classifier over (code ×
    /// representative message) pairs, including the HTTP-status codes a gateway body arrives as: whatever
    /// the code already says (its row, else the HTTP status) keeps its disposition.
    #[test]
    fn full_classifier_never_changes_a_codes_disposition() {
        let messages = [
            "",
            "insufficient upstream capacity",
            "insufficient permissions for this endpoint",
            "Insufficient USDT balance",
            "system busy, try later",
            "scheduled upgrade in progress",
            "requests too frequent",
            "order does not exist",
            "service temporarily unavailable",
        ];
        let codes: Vec<i64> = (1..=2).chain(50_000..60_000).chain(100..600).collect();
        for code in codes {
            let neutral = VenueApiError { code, msg: String::new() }.kind();
            let standing = by_code(code).unwrap_or(neutral);
            if standing == ErrorKind::Unknown {
                continue; // free to ADD resolution where nothing has an opinion
            }
            for msg in messages {
                let ours = kind(code, msg);
                assert!(
                    vike_bridge_core::error_kind::same_disposition(ours, standing),
                    "okx {code} + {msg:?}: {ours} changes the disposition of {standing}"
                );
            }
        }
    }

    /// The anchored `insufficient` rule: only account-balance wording may reach the TERMINAL
    /// InsufficientFunds bucket. A 5xx outage body that merely contains the word must stay retryable.
    #[test]
    fn insufficient_is_anchored_to_balance_wording() {
        assert_eq!(by_msg("insufficient upstream capacity"), None);
        assert_eq!(by_msg("insufficient permissions"), None);
        assert_eq!(by_msg("Insufficient USDT balance"), Some(ErrorKind::InsufficientFunds));
        assert_eq!(by_msg("insufficient margin"), Some(ErrorKind::InsufficientFunds));
        assert!(kind(503, "insufficient upstream capacity").is_retryable());
    }

    #[test]
    fn table_covers_the_documented_set() {
        let n = (50000..52000i64).filter(|c| by_code(*c).is_some()).count();
        assert!(n >= 30, "expected the full documented OKX set, got {n} codes");
    }

    /// A failed modify: this venue's status-unknown code (50004) and an unmapped code are worded as the timeout
    /// sentinel is, keeping the venue's message; a refusal row (51000) keeps the venue's words verbatim.
    #[test]
    fn modify_failed_words_the_unknown_code_as_the_sentinel() {
        vike_bridge_core::error_table::assert_modify_failed_words_unknown_as_the_sentinel(
            |c, e| modify_failed(c, e, None),
            50004,
            51000,
        );
    }
}
