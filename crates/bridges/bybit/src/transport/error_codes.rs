//! Bybit's official V5 error-code table, as a [`VenueTaxonomy`] (venue-error-taxonomy lane).
//!
//! Source: <https://bybit-exchange.github.io/docs/v5/error> (the V5 "Error Codes" reference — the HTTP codes,
//! the WS order-entry block, the 10xxx UTA/general block, the 110xxx derivatives-order block, the 170xxx spot-trade
//! block, the collateral and unified-margin blocks), every code an order place, cancel or amend, an account-setting
//! write this crate makes, or any request can answer; spot margin, loans, earn, options and the other products this
//! crate never calls are left out.
//!
//! Shape mirrors the binance sibling: an exact-code table consulted before the venue-neutral layer
//! ([`vike_bridge_core::transport::ErrorKind::classify`], which reads no venue's codes: it held a seed of 8 Bybit
//! codes until 2026-11-23, rows here now, `tests::the_moved_seed_codes_keep_their_disposition`), plus a message
//! fallback. A code this table does not map is `ErrorKind::UnmappedCode` (never final), reported once at WARN
//! ([`vike_bridge_core::error_kind::classify_venue`]). Bybit is much
//! LESS message-overloaded than Binance — its codes are specific — so [`by_msg`] is thin and exists
//! mainly for the Cloudflare-fronted HTML bodies that arrive with no venue code at all.
//!
//! ## Bybit's one venue-specific nuance
//! Bybit fronts REST with Cloudflare, which answers an IP block with **HTTP 403**. The central
//! baseline maps 403 → [`ErrorKind::Auth`] (its standard meaning), which for Bybit is misleading:
//! the key is fine, the IP is throttled. [`by_code`] remaps `403` → [`ErrorKind::RateLimited`]
//! here — exactly the override the central `kind_with` doc names as the motivating example.
//!
//! Every caller that decides on a Bybit code reads it through [`TAXONOMY`]: the order-write rule
//! ([`VenueTaxonomy::submit_outcome_unknown`]: submit, amend, set-leverage, a cancel's [`cancel_failed`], the
//! account-mode writes), the start's failure kind, the rate-limit checks of the resync and the reconcile pass (the
//! Cloudflare `403` row included).

use vike_bridge_core::error_kind::VenueTaxonomy;
use vike_bridge_core::transport::{E_TIMEOUT_AMBIGUOUS, ErrorKind, VenueApiError};
use vike_model::events::Event;

/// Bybit's documented V5 error codes → [`ErrorKind`]. `None` = not mapped: an unmapped code
/// ([`vike_bridge_core::error_kind::classify_venue`]). Each row's comment is the venue's own words (cut short).
///
/// The reference never says "status unknown" in a sentence, so the rows filed [`ErrorKind::Timeout`] are the
/// codes whose WORDS leave a write open: a timeout (`10000`, `170007`, `170146`, `170147` cancel, `170310` amend),
/// "still processing" (`170035` "Submitted to the system for processing!", `170212` "Cancel order request
/// processing"), an amend that raced a replace (`40004` "please check the order status again"), a duplicate of a
/// write that may have landed (`10014`, `110030`, `110072`, `170141`) and the spot ETP price warnings `170195` and
/// `170196` ("your order may not be filled", which reads like a warning on a PLACED order). Each is read back by
/// `orderLinkId`, never refused; so are the [`ErrorKind::ServerError`] rows [`server_had_trouble`] names.
pub fn by_code(code: i64) -> Option<ErrorKind> {
    Some(match code {
        // ---- negative codes: the request time window, the spot key -------------------------
        -2015 => ErrorKind::Auth,        // (Spot) Your api key has expired
        -1 => ErrorKind::InvalidRequest, // request expired: o@0, now[] diff[]
        // ---- HTTP-status-as-code -----------------------------------------------------------
        400 => ErrorKind::InvalidRequest, // Bad request. Need to send the request with GET /...
        401 => ErrorKind::Auth,           // Invalid request. Need to use the correct key / put...
        // Cloudflare answers an IP block with 403 (NOT an auth failure: see the module doc).
        403 => ErrorKind::RateLimited, // Forbidden request. 1. IP rate limit breached; 2. GET...
        404 => ErrorKind::NotFound,    // Cannot find path. 1. Wrong path; 2. Category value does...
        429 => ErrorKind::RateLimited, // System level frequency protection. Please retry when...
        // ---- 10xxx / 20xxx: general, auth, rate limit, system (10404, 10429, 20006: WS) ---------
        10000 => ErrorKind::Timeout,          // Server Timeout
        10001 => ErrorKind::InvalidRequest,   // Request parameter error.
        10002 => ErrorKind::InvalidRequest,   // The request time exceeds the time window range.
        10003 => ErrorKind::Auth, // API key is invalid. Check whether the key and domain are...
        10004 => ErrorKind::Auth, // Error sign, please check your signature generation algorithm.
        10005 => ErrorKind::Auth, // Permission denied, please check your API key permissions.
        10006 => ErrorKind::RateLimited, // Too many visits. Exceeded the API Rate Limit.
        10007 => ErrorKind::Auth, // User authentication failed.
        10008 => ErrorKind::Auth, // Common banned, please check your account mode
        10009 => ErrorKind::Auth, // Service Restricted: Access is currently unavailable for...
        10010 => ErrorKind::Auth, // Unmatched IP, please check your API key's bound IP addresses.
        10014 => ErrorKind::Timeout, // Invalid duplicate request.
        10016 => ErrorKind::ServerError, // Server error. (ws-oe: 1. internal server error; 2....
        10017 => ErrorKind::InvalidRequest, // Route not found.
        10018 => ErrorKind::RateLimited, // Exceeded the IP Rate Limit.
        10019 => ErrorKind::VenueMaintenance, // ws trade service is restarting, do not accept...
        10024 => ErrorKind::Auth, // Compliance rules triggered
        10027 => ErrorKind::Auth, // Transactions are banned.
        10028 => ErrorKind::InvalidRequest, // The API can only be accessed by unified account...
        10029 => ErrorKind::InvalidRequest, // The requested symbol is invalid, please check...
        10404 => ErrorKind::InvalidRequest, // 1. op type is not found; 2. category is not...
        10429 => ErrorKind::RateLimited, // System level frequency protection
        20003 => ErrorKind::RateLimited, // Too frequent requests under the same session
        20006 => ErrorKind::InvalidRequest, // reqId is duplicated
        // ---- 30xxx / 33xxx / 40xxx / 100xxx: price limits, loans, key expiry, amend --------
        30133 => ErrorKind::InvalidRequest, // OTC loan: The symbol you select for USDT...
        30134 => ErrorKind::InvalidRequest, // OTC loan: The symbol you select for USDC Contract...
        30135 => ErrorKind::InvalidRequest, // The leverage you select for USDT Perpetual...
        30136 => ErrorKind::InvalidRequest, // The leverage you select for USDC Perpetual or...
        30208 => ErrorKind::InvalidRequest, // Failed to submit order(s). The order price is...
        30209 => ErrorKind::InvalidRequest, // Failed to submit order(s). The order price is...
        30228 => ErrorKind::InvalidRequest, // No new positions during delisting
        30256 => ErrorKind::InsufficientFunds, // Margin limit exceeded. The maintenance margin...
        33004 => ErrorKind::Auth,           // (Derivatives) Your api key has expired
        40004 => ErrorKind::Timeout, // the order is modified during the process of replacing,...
        100028 => ErrorKind::InvalidRequest, // The API cannot be accessed by unified account users.
        // ---- 110xxx: derivatives order errors ----------------------------------------------
        110001 => ErrorKind::NotFound,          // Order does not exist
        110003 => ErrorKind::InvalidRequest,    // Order price exceeds the allowable range.
        110004 => ErrorKind::InsufficientFunds, // Wallet balance is insufficient
        110005 => ErrorKind::InvalidRequest,    // position status error
        110006 => ErrorKind::InsufficientFunds, // The assets are estimated to be unable to...
        110007 => ErrorKind::InsufficientFunds, // Available balance is insufficient
        110008 => ErrorKind::NotFound,          // The order has been completed or cancelled.
        110009 => ErrorKind::InvalidRequest,    // The number of stop orders exceeds the maximum...
        110010 => ErrorKind::NotFound,          // The order has been cancelled
        110011 => ErrorKind::InvalidRequest,    // Liquidation will be triggered immediately by...
        110012 => ErrorKind::InsufficientFunds, // Insufficient available balance.
        110013 => ErrorKind::InvalidRequest,    // Cannot set leverage due to risk limit level.
        110015 => ErrorKind::InvalidRequest,    // The position is in cross margin mode.
        110016 => ErrorKind::InvalidRequest, // The quantity of contracts requested exceeds the...
        110017 => ErrorKind::InvalidRequest, // orderQty will be truncated to zero
        110018 => ErrorKind::InvalidRequest, // User ID is illegal.
        110019 => ErrorKind::InvalidRequest, // Order ID is illegal.
        110020 => ErrorKind::InvalidRequest, // Not allowed to have more than 500 active orders.
        110021 => ErrorKind::InvalidRequest, // Not allowed to exceeded position limits due to...
        110022 => ErrorKind::InvalidRequest, // Quantity has been restricted and orders cannot...
        110023 => ErrorKind::InvalidRequest, // Currently you can only reduce your position on...
        110024 => ErrorKind::InvalidRequest, // You have an existing position, so the position...
        110025 => ErrorKind::InvalidRequest, // Position mode has not been modified.
        110026 => ErrorKind::InvalidRequest, // Cross/isolated margin mode has not been modified.
        110028 => ErrorKind::InvalidRequest, // You have existing open orders, so the position...
        110029 => ErrorKind::InvalidRequest, // Hedge mode is not supported for this symbol.
        110030 => ErrorKind::Timeout,        // Duplicate orderId
        110031 => ErrorKind::InvalidRequest, // Non-existing risk limit info, please check the...
        110032 => ErrorKind::InvalidRequest, // Order is illegal
        110035 => ErrorKind::InvalidRequest, // Cancellation of orders was not completed before...
        110036 => ErrorKind::InvalidRequest, // You are not allowed to change leverage due to...
        110037 => ErrorKind::InvalidRequest, // User setting list does not have this symbol
        110038 => ErrorKind::InvalidRequest, // You are not allowed to change leverage due to...
        110039 => ErrorKind::InsufficientFunds, // Maintenance margin rate is too high. This may...
        110040 => ErrorKind::InsufficientFunds, // The order will trigger a forced liquidation,...
        110042 => ErrorKind::InvalidRequest, // Currently,due to pre-delivery status, you can...
        110043 => ErrorKind::InvalidRequest, // Set leverage has not been modified.
        110044 => ErrorKind::InsufficientFunds, // Available margin is insufficient.
        110045 => ErrorKind::InsufficientFunds, // Wallet balance is insufficient.
        110046 => ErrorKind::InvalidRequest, // Liquidation will be triggered immediately by...
        110049 => ErrorKind::InvalidRequest, // Tick notes can only be numbers
        110050 => ErrorKind::InvalidRequest, // Invalid coin
        110051 => ErrorKind::InsufficientFunds, // The user's available balance cannot cover the...
        110052 => ErrorKind::InsufficientFunds, // Your available balance is insufficient to set...
        110053 => ErrorKind::InsufficientFunds, // The user's available balance cannot cover the...
        110057 => ErrorKind::InvalidRequest, // Conditional order or limit order contains TP/SL...
        110058 => ErrorKind::InvalidRequest, // You can't set take profit and stop loss due to...
        110059 => ErrorKind::InvalidRequest, // Not allowed to modify the TP/SL of a partially...
        110060 => ErrorKind::InvalidRequest, // Under full TP/SL mode, it is not allowed to...
        110061 => ErrorKind::InvalidRequest, // Not allowed to have more than 20 TP/SLs under...
        110063 => ErrorKind::VenueMaintenance, // Settlement in progress! {{key0}} not available...
        110064 => ErrorKind::InvalidRequest, // The modified contract quantity cannot be less...
        110066 => ErrorKind::InvalidRequest, // Trading is currently not allowed.
        110067 => ErrorKind::InvalidRequest, // Unified account is not supported.
        110068 => ErrorKind::InvalidRequest, // Leveraged trading is not allowed.
        110069 => ErrorKind::InvalidRequest, // Ins lending customer is not allowed to trade.
        110070 => ErrorKind::InvalidRequest, // ETP symbols cannot be traded.
        110071 => ErrorKind::InvalidRequest, // Sorry, we're revamping the Unified Margin...
        110072 => ErrorKind::Timeout,        // OrderLinkedID is duplicate
        110073 => ErrorKind::InvalidRequest, // Set margin mode failed
        110074 => ErrorKind::InvalidRequest, // This contract is not live
        110077 => ErrorKind::InvalidRequest, // Pm mode cannot support
        110079 => ErrorKind::ServerError,    // The order is processing and can not be operated,...
        110080 => ErrorKind::InvalidRequest, // Operations Restriction: The current LTV ratio of...
        110085 => ErrorKind::InvalidRequest, // The risk limit and margin ratio for this...
        110086 => ErrorKind::InvalidRequest, // Current order leverage exceeds the maximum...
        110087 => ErrorKind::InvalidRequest, // Leverage for Perpetual or Futures contracts...
        110088 => ErrorKind::InvalidRequest, // Please Upgrade to UTA to trade
        110089 => ErrorKind::InvalidRequest, // Exceeds the maximum risk limit level
        110090 => ErrorKind::InvalidRequest, // Order placement failed as your position may...
        110092 => ErrorKind::InvalidRequest, // expect Rising, but trigger_price[XXXXX] <=...
        110093 => ErrorKind::InvalidRequest, // expect Falling, but trigger_price[XXXXX] >=...
        110094 => ErrorKind::InvalidRequest, // Order notional value below the lower limit
        110095 => ErrorKind::InvalidRequest, // You cannot create, modify or cancel Pre-Market...
        110096 => ErrorKind::InvalidRequest, // Pre-Market Perpetual Trading does not support...
        110097 => ErrorKind::InvalidRequest, // Non-UTA users cannot access Pre-Market Perpetual...
        110098 => ErrorKind::InvalidRequest, // Only Good-Till-Canceled (GTC) orders are...
        110099 => ErrorKind::InvalidRequest, // You cannot create TP/SL orders during the Call...
        110100 => ErrorKind::InvalidRequest, // You cannot place, modify, or cancel Pre-Market...
        110101 => ErrorKind::InvalidRequest, // Trading inverse contracts under Cross and...
        110102 => ErrorKind::InvalidRequest, // The user does not support trading Inverse...
        110103 => ErrorKind::InvalidRequest, // Only Post-Only orders are available at this stage
        110104 => ErrorKind::InvalidRequest, // The LTV for ins Loan has exceeded the limit, and...
        110105 => ErrorKind::InvalidRequest, // The LTV for ins Loan has exceeded the limit, and...
        110106 => ErrorKind::InvalidRequest, // Restrictions on Ins Loan; inverse contracts are...
        110107 => ErrorKind::InvalidRequest, // Restrictions on ins Loan; leverage exceeding the...
        110108 => ErrorKind::InvalidRequest, // Allowable range: 1 to 10000 tick size
        110109 => ErrorKind::InvalidRequest, // Allowable range: 0.01% to 10%
        110118 => ErrorKind::InvalidRequest, // Unable to retrieve a pruce of the market order...
        110119 => ErrorKind::InvalidRequest, // Order failed. RPI orders are restricted to...
        110120 => ErrorKind::InvalidRequest, // Order price cannot be smaller than xxxx, the...
        110121 => ErrorKind::InvalidRequest, // Order price cannot be higher than xxxx, the...
        110123 => ErrorKind::Auth,           // You must agree to the Trading Terms
        110124 => ErrorKind::Auth, // Trading is temporarily restricted due to an active...
        110125 => ErrorKind::Auth, // You must agree to the Trading Terms before trading this...
        110131 => ErrorKind::InsufficientFunds, // Margin limit exceeded. The maintenance margin...
        110132 => ErrorKind::Auth, // Product not available in your region
        110134 => ErrorKind::VenueMaintenance, // Trading on this symbol is suspended. Please...
        110135 => ErrorKind::InvalidRequest, // You currently hold a position in an Event...
        110136 => ErrorKind::InvalidRequest, // Event Futures are currently available for...
        110137 => ErrorKind::InvalidRequest, // This Event Futures contract has entered the...
        // ---- 170xxx: spot trade errors -----------------------------------------------------
        170001 => ErrorKind::ServerError,       // Internal error.
        170005 => ErrorKind::RateLimited, // Too many new orders; current limit is %s orders per %s.
        170007 => ErrorKind::Timeout,     // Timeout waiting for response from backend server.
        170010 => ErrorKind::InvalidRequest, // Purchase failed: Exceed the maximum position...
        170011 => ErrorKind::InvalidRequest, // Purchase failed: Exceed the maximum position...
        170019 => ErrorKind::InvalidRequest, // the current available limit is...
        170031 => ErrorKind::VenueMaintenance, // The feature has been suspended
        170032 => ErrorKind::ServerError, // Network error. Please try again later
        170033 => ErrorKind::InsufficientFunds, // margin Insufficient account balance
        170034 => ErrorKind::InsufficientFunds, // Liability over flow in spot leverage trade!
        170035 => ErrorKind::Timeout,     // Submitted to the system for processing!
        170036 => ErrorKind::InvalidRequest, // You haven't enabled Cross Margin Trading yet.
        170037 => ErrorKind::InvalidRequest, // Cross Margin Trading not yet supported by the...
        170105 => ErrorKind::InvalidRequest, // Parameter '%s' was empty.
        170115 => ErrorKind::InvalidRequest, // Invalid timeInForce.
        170116 => ErrorKind::InvalidRequest, // Invalid orderType.
        170117 => ErrorKind::InvalidRequest, // Invalid side.
        170121 => ErrorKind::InvalidRequest, // Invalid symbol.
        170124 => ErrorKind::InvalidRequest, // Order amount too large.
        170130 => ErrorKind::InvalidRequest, // Data sent for paramter '%s' is not valid.
        170131 => ErrorKind::InsufficientFunds, // Balance insufficient
        170132 => ErrorKind::InvalidRequest, // Order price too high.
        170133 => ErrorKind::InvalidRequest, // Order price lower than the minimum.
        170134 => ErrorKind::InvalidRequest, // Order price decimal too long.
        170136 => ErrorKind::InvalidRequest, // Order quantity lower than the minimum.
        170137 => ErrorKind::InvalidRequest, // Order volume decimal too long
        170139 => ErrorKind::NotFound,    // Order has been filled.
        170140 => ErrorKind::InvalidRequest, // Order value exceeded lower limit
        170141 => ErrorKind::Timeout,     // Duplicate clientOrderId
        170142 => ErrorKind::NotFound,    // Order has been cancelled
        170143 => ErrorKind::NotFound,    // Cannot be found on order book
        170144 => ErrorKind::ServerError, // Order has been locked
        170145 => ErrorKind::InvalidRequest, // This order type does not support cancellation
        170146 => ErrorKind::Timeout,     // Order creation timeout
        170147 => ErrorKind::Timeout,     // Order cancellation timeout
        170148 => ErrorKind::InvalidRequest, // Market order amount decimal too long
        170149 => ErrorKind::InvalidRequest, // Create order failed
        170150 => ErrorKind::InvalidRequest, // Cancel order failed
        170151 => ErrorKind::InvalidRequest, // The trading pair is not open yet
        170157 => ErrorKind::InvalidRequest, // The trading pair is not available for api trading
        170159 => ErrorKind::InvalidRequest, // Market Order is not supported within the first...
        170190 => ErrorKind::NotFound,    // Cancel order has been finished
        170191 => ErrorKind::ServerError, // Can not cancel order, please try again later
        170192 => ErrorKind::InvalidRequest, // Order price cannot be higher than %s .
        170193 => ErrorKind::InvalidRequest, // Buy order price cannot be higher than %s.
        170194 => ErrorKind::InvalidRequest, // Sell order price cannot be lower than %s.
        170195 => ErrorKind::Timeout, // Please note that your order may not be filled. ETP buy...
        170196 => ErrorKind::Timeout, // Please note that your order may not be filled. ETP sell...
        170197 => ErrorKind::InvalidRequest, // Your order quantity to buy is too large. The...
        170198 => ErrorKind::InvalidRequest, // Your order quantity to sell is too large. The...
        170199 => ErrorKind::InvalidRequest, // Your order quantity to buy is too large. The...
        170200 => ErrorKind::InvalidRequest, // Your order quantity to sell is too large. The...
        170201 => ErrorKind::InvalidRequest, // Invalid orderFilter parameter
        170202 => ErrorKind::InvalidRequest, // Please enter the TP/SL price.
        170203 => ErrorKind::InvalidRequest, // trigger price cannot be higher than 110% price.
        170204 => ErrorKind::InvalidRequest, // trigger price cannot be lower than 90% of qty.
        170206 => ErrorKind::InvalidRequest, // Stop_limit Order is not supported within the...
        170207 => ErrorKind::InsufficientFunds, // The loan amount of the platform is not enough.
        170209 => ErrorKind::Auth,    // This trading pair is only available to the xxx, yyyy,etc...
        170210 => ErrorKind::InvalidRequest, // New order rejected.
        170212 => ErrorKind::Timeout, // Cancel order request processing
        170213 => ErrorKind::NotFound, // Order does not exist.
        170215 => ErrorKind::InvalidRequest, // Spot Trading (Buy) Restriction: The current LTV...
        170216 => ErrorKind::InvalidRequest, // The leverage you select for Spot Trading cannot...
        170217 => ErrorKind::InvalidRequest, // Only LIMIT-MAKER order is supported for the...
        170218 => ErrorKind::InvalidRequest, // The LIMIT-MAKER order is rejected due to invalid...
        170219 => ErrorKind::Auth,    // UID {{xxx}} is not available to this feature
        170220 => ErrorKind::InvalidRequest, // Spot Trading Restriction: The current LTV ratio...
        170221 => ErrorKind::InvalidRequest, // This coin does not exist.
        170222 => ErrorKind::RateLimited, // Too many requests in this time frame.
        170223 => ErrorKind::InvalidRequest, // Your Spot Account with Institutional Lending...
        170224 => ErrorKind::Auth,    // You're not a user of the Innovation Zone.
        170226 => ErrorKind::InvalidRequest, // Your Spot Account for Margin Trading is being...
        170227 => ErrorKind::InvalidRequest, // This feature is not supported.
        170228 => ErrorKind::InsufficientFunds, // The purchase amount of each order exceeds the...
        170229 => ErrorKind::InsufficientFunds, // The sell quantity per order exceeds the...
        170230 => ErrorKind::InvalidRequest, // Operations Restriction: Due to the deactivation...
        170234 => ErrorKind::ServerError, // System Error
        170241 => ErrorKind::Auth,    // To proceed with trading, users must read through and...
        170310 => ErrorKind::Timeout, // Order modification timeout
        170311 => ErrorKind::InvalidRequest, // Order modification failed
        170312 => ErrorKind::InvalidRequest, // The current order does not support modification
        170313 => ErrorKind::InvalidRequest, // The modified contract quantity cannot be less...
        170341 => ErrorKind::InvalidRequest, // Request order quantity exceeds maximum limit
        170344 => ErrorKind::InvalidRequest, // Symbol is not supported on Margin Trading
        170346 => ErrorKind::InvalidRequest, // Settle coin is not a collateral coin, cannot trade
        170348 => ErrorKind::Auth,    // Please go to (https://www.bybit-tr.com) to proceed.
        170355 => ErrorKind::InvalidRequest, // RPI orders are restricted to approved market...
        170358 => ErrorKind::InvalidRequest, // The current site does not support ETP
        170359 => ErrorKind::InvalidRequest, // TThe current site does not support leveraged trading
        170371 => ErrorKind::InvalidRequest, // Order price cannot be lower than {}, the price...
        170372 => ErrorKind::InvalidRequest, // Order price cannot be higher than 0, the price...
        170381 => ErrorKind::InvalidRequest, // Order quantity too large.
        170382 => ErrorKind::InvalidRequest, // Order quantity too large.
        170709 => ErrorKind::InvalidRequest, // OTC loan: The select trading pair is not in the...
        170810 => ErrorKind::InvalidRequest, // Cannot exceed maximum of 500 conditional, TP/SL...
        // ---- 182xxx: the collateral switch -------------------------------------------------
        182011 => ErrorKind::InvalidRequest, // The {{coin}} collateral amount has reached the...
        182012 => ErrorKind::Auth, // Due to regulatory requirements, collateral services are...
        182108 => ErrorKind::InvalidRequest, // Switching failed, margin verification failed,...
        182110 => ErrorKind::InvalidRequest, // Failed to switch
        182111 => ErrorKind::InvalidRequest, // The requested currency has a non guaranteed gold...
        // ---- 3200xxx / 3400xxx: unified-account margin -------------------------------------
        3200306 => ErrorKind::InsufficientFunds, // Failed to place an order with the contract!...
        3200320 => ErrorKind::InvalidRequest,    // Operations Restriction: The current LTV ratio...
        3200403 => ErrorKind::InvalidRequest,    // isolated margin can not create order
        3200641 => ErrorKind::InvalidRequest,    // {{coin}} collateral has reached the platform...
        3400045 => ErrorKind::InvalidRequest,    // Set margin mode failed
        3400139 => ErrorKind::InvalidRequest,    // The total value of your positions and orders...
        _ => return None,
    })
}

/// Does this code say only that the SERVER had trouble, so that it says nothing about what became of
/// the request? `10016` "Server error." (the WS block: "internal server error; Service is restarting"), and
/// spot's `170001` "Internal error.", `170032` "Network error. Please try again later" and `170234` "System
/// Error" ([`by_code`]'s rows, from the V5 error reference this module cites; none says the order was not
/// processed). With the transport's sentinel and the rows filed under [`ErrorKind::Timeout`] (bybit answers
/// those as HTTP 200 + `retCode`, so they are not the sentinel), it makes [`TAXONOMY`]'s
/// [`VenueTaxonomy::submit_outcome_unknown`]: `true` means re-query by `orderLinkId` (`resolve_ambiguous_submit`),
/// never a terminal `OrderRejected`. The ONE rule behind every submit arm in
/// `crates/bridges/bybit/src/execution/rest/mod.rs`.
pub fn server_had_trouble(code: i64) -> bool {
    matches!(code, 10016 | 170001 | 170032 | 170234)
}

/// Does this code say an order already rests under the submit's `orderLinkId`? `110072` "OrderLinkedID is
/// duplicate" (the UTA block) and `170141` "Duplicate clientOrderId" (the spot block) of the V5 error
/// reference. It is no refusal of OUR order: the core refuses an order client id it has already seen
/// (`crates/vike-core/src/runtime/recent_order_client_ids.rs`), so the order resting under it is one we sent
/// (decision 0303). With [`server_had_trouble`] it is [`TAXONOMY`]'s `leaves_write_open`.
pub fn says_order_client_id_taken(code: i64) -> bool {
    matches!(code, 110072 | 170141)
}

/// Does a CANCEL answered `code` say the order is gone (`CancelAck::AlreadyGone`, the gap sentinel's
/// to settle), rather than refuse? `110001` and `170213`, "order does not exist". Measured on the demo host
/// (2026-11-08, a second cancel by `orderLinkId`): `linear` answers `110001` for a cancelled limit, a cancelled
/// stop and an IOC the venue killed, `spot` answers `170213` for a cancelled limit and a killed IOC
/// (`crates/bridges/bybit/tests/offline/cancel_already_gone.rs` pins the replies). [`by_code`] files more under
/// `NotFound` (`110008`, `110010`, `170139`, `170142`, `170143`, `170190`; `404` is a wrong path, never an order)
/// and no cancel produced one; each joins only behind a demo reply that shows it, as its own step. Until then it
/// stays the `Err` that is a bare `OrderCancelRejected`.
pub fn cancel_says_gone(code: i64) -> bool {
    matches!(code, 110001 | 170213)
}

/// A failed CANCEL as the shared cancel answer is handed it: a code [`VenueTaxonomy::submit_outcome_unknown`]
/// reads as status-unknown (`170147` "Order cancellation timeout", `170212` "Cancel order request processing", the
/// server-trouble codes) leaves the cancel's outcome UNKNOWN, so it goes on as the transport's sentinel
/// ([`E_TIMEOUT_AMBIGUOUS`], the venue's words kept), as [`modify_failed`] does for an amend; a cancel that landed
/// is ended by the stream's `Cancelled` row. A definite code is the venue's refusal of the cancel, as it came.
pub fn cancel_failed(exc: VenueApiError) -> VenueApiError {
    if TAXONOMY.submit_outcome_unknown(exc.code) {
        VenueApiError { code: E_TIMEOUT_AMBIGUOUS, ..exc }
    } else {
        exc
    }
}

/// The non-terminal advisory for a failed amend. A status-unknown code
/// ([`VenueTaxonomy::submit_outcome_unknown`]) is worded as the timeout sentinel is (`modify outcome unknown`:
/// the amend may have been applied) instead of as a refusal; a definite one keeps the venue's words. Stamped
/// with `stated`, the refusal envelope's own `time`, else when the error was received
/// (`vike_bridge_core::rest::amend_answer_time`): a [`VenueApiError`] keeps no time, so the caller reads it off
/// the envelope before unwrapping it.
pub fn modify_failed(order_client_id: &str, exc: &VenueApiError, stated: Option<i64>) -> Event {
    let ts = vike_bridge_core::rest::amend_answer_time(stated);
    let unknown = |code| TAXONOMY.submit_outcome_unknown(code);
    vike_bridge_core::rest::modify_failed_with(order_client_id, exc, unknown, ts)
}

/// Message fallback. Bybit codes are specific, so this covers only the code-less bodies (a
/// Cloudflare HTML page, a gateway error) that reach the classifier as raw text.
pub fn by_msg(msg: &str) -> Option<ErrorKind> {
    let m = msg.to_ascii_lowercase();
    // ANCHORED to balance/margin wording: a bare `contains("insufficient")` also matches bodies with
    // nothing to do with the account ("insufficient permissions" on an auth reject, "insufficient
    // upstream capacity" from the Cloudflare edge), and InsufficientFunds is TERMINAL — misfiring
    // there turns a transient outage into rejected orders. (The `classify_venue` disposition guard
    // now blocks that flip structurally too; this keeps the table honest on its own.)
    if m.contains("insufficient")
        && ["balance", "margin", "funds", "equity"].iter().any(|w| m.contains(w))
    {
        return Some(ErrorKind::InsufficientFunds);
    }
    if m.contains("restarting") || m.contains("system maintenance") || m.contains("upgrading") {
        return Some(ErrorKind::VenueMaintenance);
    }
    if m.contains("too many visits") || m.contains("rate limit") || m.contains("too frequent") {
        return Some(ErrorKind::RateLimited);
    }
    None
}

/// Bybit's taxonomy, ready to thread into [`vike_bridge_core::error_kind::classify_venue`].
pub const TAXONOMY: VenueTaxonomy = VenueTaxonomy {
    venue: "bybit",
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

    /// Every code range a row of [`by_code`] lies in, for the walks below.
    fn walked_codes() -> impl Iterator<Item = i64> {
        (-2015..0i64)
            .chain(100..600)
            .chain(10000..40100)
            .chain(100000..110200)
            .chain(170000..171000)
            .chain(182000..182200)
            .chain(3_200_000..3_200_700)
            .chain(3_400_000..3_400_200)
    }

    #[test]
    fn documented_codes_classify() {
        let cases = [
            (10003, ErrorKind::Auth),
            (10004, ErrorKind::Auth),
            (10005, ErrorKind::Auth),
            (10010, ErrorKind::Auth),
            (33004, ErrorKind::Auth),
            (10006, ErrorKind::RateLimited),
            (10018, ErrorKind::RateLimited),
            (20003, ErrorKind::RateLimited),
            (10016, ErrorKind::ServerError),
            (170001, ErrorKind::ServerError),
            (110134, ErrorKind::VenueMaintenance),
            (10001, ErrorKind::InvalidRequest),
            (10002, ErrorKind::InvalidRequest),
            (110001, ErrorKind::NotFound),
            (110008, ErrorKind::NotFound),
            (110010, ErrorKind::NotFound),
            (170143, ErrorKind::NotFound),
            (110004, ErrorKind::InsufficientFunds),
            (110007, ErrorKind::InsufficientFunds),
            (110052, ErrorKind::InsufficientFunds),
            (170131, ErrorKind::InsufficientFunds),
            (110094, ErrorKind::InvalidRequest),
            (110120, ErrorKind::InvalidRequest),
            (170133, ErrorKind::InvalidRequest),
            (170007, ErrorKind::Timeout),
            (10000, ErrorKind::Timeout),
            (170146, ErrorKind::Timeout),
            (170147, ErrorKind::Timeout),
            (-1, ErrorKind::InvalidRequest),
        ];
        for (code, want) in cases {
            assert_eq!(kind(code, ""), want, "bybit {code}");
        }
    }

    /// The write rule over every code the table knows, exactly: the `Timeout` rows and the server trouble.
    /// Base: spot's `170001`, `170234`, `170032`, `170035` and `10014` were terminal rejects, the amend's `170310`
    /// and `40004` were worded as refusals, and the cancel's `170147` and `170212` as refused cancels.
    #[test]
    fn submit_outcome_unknown_is_exactly_the_sentinel_the_timeout_codes_and_the_server_trouble() {
        let known =
            walked_codes().filter(|c| by_code(*c).is_some() || (TAXONOMY.leaves_write_open)(*c));
        let unknown: Vec<i64> = known.filter(|c| TAXONOMY.submit_outcome_unknown(*c)).collect();
        assert_eq!(
            unknown,
            [
                10000, 10014, 10016, 40004, 110030, 110072, 170001, 170007, 170032, 170035, 170141,
                170146, 170147, 170195, 170196, 170212, 170234, 170310
            ]
        );
        assert!(TAXONOMY.submit_outcome_unknown(E_TIMEOUT_AMBIGUOUS));
        // definite refusals: a NOT SENT, a rate limit, a refusal, an HTTP status with no row
        for code in [0, 10006, 110001, 110007, 500] {
            assert!(!TAXONOMY.submit_outcome_unknown(code), "{code}");
        }
    }

    /// Today's set, code by code, and what the table files under `NotFound` beyond it.
    #[test]
    fn cancel_says_gone_is_exactly_todays_set() {
        let codes = 0..200_000i64;
        let gone: Vec<i64> = codes.clone().filter(|c| cancel_says_gone(*c)).collect();
        assert_eq!(gone, [110001, 170213]);
        let table_only: Vec<i64> = codes
            .filter(|c| by_code(*c) == Some(ErrorKind::NotFound) && !cancel_says_gone(*c))
            .collect();
        assert_eq!(table_only, [404, 110008, 110010, 170139, 170142, 170143, 170190]);
    }

    /// The Bybit-specific nuance: Cloudflare's 403 is an IP THROTTLE, not an auth failure. This is
    /// the one place the taxonomy deliberately departs from the venue-neutral HTTP mapping.
    #[test]
    fn cloudflare_403_is_a_throttle_not_an_auth_failure() {
        let e = VenueApiError { code: 403, msg: "Forbidden".into() };
        assert_eq!(e.kind(), ErrorKind::Auth, "the neutral layer treats 403 as auth");
        assert_eq!(kind(403, "Forbidden"), ErrorKind::RateLimited, "bybit override");
        assert!(ErrorKind::RateLimited.is_retryable(), "so the caller backs off instead of dying");
    }

    /// An undocumented code is unmapped: never final (its outcome unknown); a code this bridge mints
    /// for a refusal of its own stays a refusal.
    #[test]
    fn an_unmapped_code_is_never_final() {
        let k = kind(999_999, "brand new failure");
        assert_eq!(k, ErrorKind::UnmappedCode);
        assert!(!k.is_terminal() && !k.is_retryable());
        let own = crate::execution::E_NO_INSTRUMENT_ROW;
        assert_eq!(kind(own, "no row"), ErrorKind::Unknown);
    }

    #[test]
    fn message_fallback_catches_codeless_bodies() {
        assert_eq!(kind(502, "upstream restarting"), ErrorKind::VenueMaintenance);
        assert_eq!(kind(418, "<html>rate limit</html>"), ErrorKind::RateLimited);
    }

    /// The codes `ErrorKind::classify` held for Bybit until 2026-11-23 keep their disposition as this
    /// table's rows.
    #[test]
    fn the_moved_seed_codes_keep_their_disposition() {
        use vike_bridge_core::transport::ErrorKind::{
            Auth, InvalidRequest, NotFound, RateLimited, ServerError,
        };
        vike_bridge_core::error_table::assert_keeps_the_moved_seed_dispositions(
            &TAXONOMY,
            &[
                (10006, RateLimited),
                (10018, RateLimited),
                (10003, Auth),
                (10004, Auth),
                (10005, Auth),
                (110001, NotFound),
                (10016, ServerError),
                (10001, InvalidRequest),
            ],
        );
    }

    /// Every row has a venue answer recorded in this repository behind it, or is named here as having
    /// none yet (the list only shrinks: a row given a sample leaves it). The 200 rows the V5 reference gave on
    /// 2026-11-23 joined it with their rows, none with a recorded answer.
    #[test]
    fn every_row_has_a_recorded_sample_or_is_listed_without_one() {
        use vike_bridge_core::error_table::RowSample;
        vike_bridge_core::error_table::assert_every_row_has_a_sample(
            &TAXONOMY,
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."),
            walked_codes(),
            &[
                RowSample {
                    code: 10001,
                    words: "position idx not match position mode",
                    recorded_in: "crates/bridges/bybit/tests/offline/modes.rs",
                },
                RowSample {
                    code: 110001,
                    words: "order not exists or too late to cancel",
                    recorded_in: "crates/bridges/bybit/tests/offline/cancel_already_gone.rs",
                },
                RowSample {
                    code: 110101,
                    words: "Trading contracts under Cross and Portfolio modes requires enabling the \
                            settlement asset as collateral.",
                    recorded_in: "crates/bridges/bybit/tests/fixtures/classes/create_refused_110101_inverse.json",
                },
                RowSample {
                    code: 170131,
                    words: "Insufficient balance.",
                    recorded_in: "crates/bridges/bybit/tests/fixtures/classes/create_refused_170131_spot_sell.json",
                },
                RowSample {
                    code: 170213,
                    words: "Order does not exist.",
                    recorded_in: "crates/bridges/bybit/tests/offline/cancel_already_gone.rs",
                },
            ],
            &[
                -2015, -1, 400, 401, 403, 404, 429, 10000, 10002, 10003, 10004, 10005, 10006,
                10007, 10008, 10009, 10010, 10014, 10016, 10017, 10018, 10019, 10024, 10027, 10028,
                10029, 10404, 10429, 20003, 20006, 30133, 30134, 30135, 30136, 30208, 30209, 30228,
                30256, 33004, 40004, 100028, 110003, 110004, 110005, 110006, 110007, 110008,
                110009, 110010, 110011, 110012, 110013, 110015, 110016, 110017, 110018, 110019,
                110020, 110021, 110022, 110023, 110024, 110025, 110026, 110028, 110029, 110030,
                110031, 110032, 110035, 110036, 110037, 110038, 110039, 110040, 110042, 110043,
                110044, 110045, 110046, 110049, 110050, 110051, 110052, 110053, 110057, 110058,
                110059, 110060, 110061, 110063, 110064, 110066, 110067, 110068, 110069, 110070,
                110071, 110072, 110073, 110074, 110077, 110079, 110080, 110085, 110086, 110087,
                110088, 110089, 110090, 110092, 110093, 110094, 110095, 110096, 110097, 110098,
                110099, 110100, 110102, 110103, 110104, 110105, 110106, 110107, 110108, 110109,
                110118, 110119, 110120, 110121, 110123, 110124, 110125, 110131, 110132, 110134,
                110135, 110136, 110137, 170001, 170005, 170007, 170010, 170011, 170019, 170031,
                170032, 170033, 170034, 170035, 170036, 170037, 170105, 170115, 170116, 170117,
                170121, 170124, 170130, 170132, 170133, 170134, 170136, 170137, 170139, 170140,
                170141, 170142, 170143, 170144, 170145, 170146, 170147, 170148, 170149, 170150,
                170151, 170157, 170159, 170190, 170191, 170192, 170193, 170194, 170195, 170196,
                170197, 170198, 170199, 170200, 170201, 170202, 170203, 170204, 170206, 170207,
                170209, 170210, 170212, 170215, 170216, 170217, 170218, 170219, 170220, 170221,
                170222, 170223, 170224, 170226, 170227, 170228, 170229, 170230, 170234, 170241,
                170310, 170311, 170312, 170313, 170341, 170344, 170346, 170348, 170355, 170358,
                170359, 170371, 170372, 170381, 170382, 170709, 170810, 182011, 182012, 182108,
                182110, 182111, 3200306, 3200320, 3200403, 3200641, 3400045, 3400139,
            ],
        );
    }

    /// REGRESSION (adversarial review): `classify_venue` consults `by_msg` for EVERY error, so a
    /// disposition flip was reachable through a MESSAGE. This runs the FULL classifier over (code ×
    /// representative message) pairs: whatever the code already says (its row, else the HTTP status)
    /// keeps its disposition. `403` is excluded: it is the ONE documented deliberate flip (Cloudflare
    /// throttle), asserted on its own above.
    #[test]
    fn full_classifier_never_changes_a_codes_disposition() {
        let messages = [
            "",
            "insufficient upstream capacity",
            "insufficient permissions",
            "wallet balance is insufficient",
            "upstream restarting",
            "system maintenance",
            "upgrading",
            "too many visits",
            "rate limit",
        ];
        for code in walked_codes() {
            if code == 403 {
                continue;
            }
            let neutral = VenueApiError { code, msg: String::new() }.kind();
            let standing = by_code(code).unwrap_or(neutral);
            if standing == ErrorKind::Unknown {
                continue;
            }
            for msg in messages {
                let ours = kind(code, msg);
                assert!(
                    vike_bridge_core::error_kind::same_disposition(ours, standing),
                    "bybit {code} + {msg:?}: {ours} changes the disposition of {standing}"
                );
            }
        }
    }

    /// The anchored `insufficient` rule: only account-balance wording reaches the TERMINAL bucket.
    #[test]
    fn insufficient_is_anchored_to_balance_wording() {
        assert_eq!(by_msg("insufficient upstream capacity"), None);
        assert_eq!(by_msg("insufficient permissions"), None);
        assert_eq!(by_msg("Wallet balance is insufficient"), Some(ErrorKind::InsufficientFunds));
        assert!(kind(503, "insufficient upstream capacity").is_retryable());
    }

    #[test]
    fn table_covers_the_documented_set() {
        let n = (10000..20100i64)
            .chain(110000..110200)
            .chain(170000..170300)
            .filter(|c| by_code(*c).is_some())
            .count();
        assert!(n >= 30, "expected the full documented Bybit set, got {n} codes");
    }

    /// A failed modify: this venue's status-unknown code (10000) and an unmapped code are worded as the timeout
    /// sentinel is, keeping the venue's message; a refusal row (10001) keeps the venue's words verbatim.
    #[test]
    fn modify_failed_words_the_unknown_code_as_the_sentinel() {
        vike_bridge_core::error_table::assert_modify_failed_words_unknown_as_the_sentinel(
            |c, e| modify_failed(c, e, None),
            10000,
            10001,
        );
    }
}
