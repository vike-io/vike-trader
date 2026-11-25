//! **The shim closes a trade by its id the way FXCM's own SDK sample does**, gated on a box with no ForexConnect SDK like
//! `fxcm_shim_subscription.rs`: no CI runner has ever compiled `crates/bridges/fxcm/src/shim/fcshim.cpp`, so what the
//! three close exports send, what they read, which codes they return and how the closing fill is named are pinned from
//! the SOURCE. They cannot prove what FXCM answers; the answers they are held against were measured on the demo login
//! (`docs/ops/fxcm-forexconnect.md`'s "Closing a trade by its id"), and `tests/fxcm_smokes/fxcm_close_smoke.rs` is the live proof of
//! the whole path.
//!
//! Why it matters: on a HEDGING account an opposite market order OPENS a second trade (measured 2026-10-13/14), so every
//! close the core sends was an open until `forex_connect_close_trade` existed, and the close it makes never reaches the node unless
//! the event listener reports the ClosedTrades row under an id the core has not folded.

use crate::shim_source::{body_of, harvest_keys, shim_code};
use std::collections::BTreeSet;
use std::path::PathBuf;

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The request is the SDK sample's (`ClosePosition`'s `createCloseMarketOrderRequest`): `TrueMarketClose` carrying the
/// trade's own account, offer and id, the side AGAINST the trade, and its amount; no rate, no net quantity.
#[test]
fn the_request_is_the_sdks_true_market_close_with_the_trade_id() {
    let body = body_of("forex_connect_close_trade");
    for needle in [
        "vm->setString(Command, O2G2::Commands::CreateOrder);",
        "vm->setString(OrderType, O2G2::Orders::TrueMarketClose);",
        "vm->setString(AccountID, accountId.c_str());",
        "vm->setString(OfferID, offerId.c_str());",
        "vm->setString(TradeID, tradeId);",
        "vm->setString(BuySell, wasBuy ? O2G2::Sell : O2G2::Buy);",
        "vm->setInt(Amount, amount);",
    ] {
        assert!(body.contains(needle), "the request lost `{needle}`: {body}");
    }
    for stranger in
        ["TrueMarketOpen", "LimitEntry", "Rate,", "NetQuantity", "RateStop", "RateLimit"]
    {
        assert!(
            !body.contains(stranger),
            "forex_connect_close_trade sends `{stranger}`, which the close request does not carry: {body}"
        );
    }
    assert_eq!(body.matches("vm->set").count(), 8, "exactly eight request parameters: {body}");
    // The ids come from the trade's OWN row, never from a caller's guess.
    for needle in [
        "accountId = t->getAccountID()",
        "offerId = t->getOfferID()",
        "wasBuy = sameStr(t->getBuySell(), O2G2::Buy)",
    ] {
        assert!(body.contains(needle), "the close lost `{needle}`: {body}");
    }
}

/// The trade is read from a FRESH Trades table of the account inside the order path's bound (the login-time snapshot
/// holds no trades at all), the permission is asked BEFORE anything is sent, and the listener is armed first.
#[test]
fn the_trade_is_read_fresh_the_permission_is_asked_first_and_the_listener_is_armed_before_the_send()
{
    let body = body_of("forex_connect_close_trade");
    assert!(
        body.contains("refreshTable(h->session, Trades, account->getAccountID(), resp, err, PRESEND_REFRESH_MS)"),
        "the trade must come from a bounded refresh-table request of the account's Trades: {body}"
    );
    assert!(
        !body.contains("getTableRefreshResponse"),
        "the login snapshot holds no trades: {body}"
    );
    let found = body.find("sameStr(t->getTradeID(), tradeId)").expect("the trade is found by id");
    let permission = body
        .find("canCreateMarketCloseOrder(instrument.c_str()) != PermissionEnabled")
        .expect("the close permission is checked");
    let arm = body.find("rl->setRequestID(").expect("the listener is armed");
    let send = body.find("sendRequest(req)").expect("the request is sent");
    assert!(
        found < permission && permission < arm && arm < send,
        "order: find, permission, arm, send: {body}"
    );
    assert!(body.contains("unsubscribeResponse(rl); rl->release();"), "the listener leaks: {body}");
    // The reply wait is ORDER_WAIT_MS (the listener's default), the bound a placement has.
    assert!(body.contains("rl->wait()"), "the reply wait lost its bound: {body}");
}

/// The returns the Rust side reads: -3 gone, -10 a bad amount, -15 no close permission, -11/-12 sent with no usable answer
/// (and the REQUEST id in the order-id buffer then), and the `refreshTable` codes passed through.
#[test]
fn the_returns_are_the_ones_the_rust_side_reads() {
    let body = body_of("forex_connect_close_trade");
    for code in [
        "return -3;",
        "return -10;",
        "return FC_CLOSE_NOT_PERMITTED;",
        "return ambiguous;",
        "ambiguous = FC_SENT_NO_ORDER_ID;",
        "ambiguous = FC_SENT_NO_ANSWER;",
        "cpy(order_id_out, oid_len, reqId.c_str());",
    ] {
        assert!(body.contains(code), "forex_connect_close_trade lost `{code}`: {body}");
    }
    let value: i32 = shim_code()
        .lines()
        .find_map(|l| l.trim().strip_prefix("#define FC_CLOSE_NOT_PERMITTED"))
        .and_then(|v| v.trim().trim_start_matches('(').trim_end_matches(')').parse().ok())
        .expect("fcshim.cpp defines FC_CLOSE_NOT_PERMITTED as a number");
    assert_eq!(value, -15, "the documented code");
    // An amount larger than the trade, or none, is refused BEFORE anything is sent.
    let over = body.find("amount > tradeAmount").expect("the amount is bounded by the trade's");
    assert!(over < body.find("sendRequest(req)").unwrap(), "{body}");
}

#[test]
fn the_open_trades_row_has_the_five_keys_the_planner_reads_and_is_a_fresh_bounded_read() {
    let body = body_of("forex_connect_open_trades");
    let keys: BTreeSet<String> = ["trade_id", "instrument", "buysell", "amount", "open_time"]
        .iter()
        .map(|k| k.to_string())
        .collect();
    assert_eq!(
        harvest_keys(&body),
        keys,
        "the shim's row and `close::parse_open_trades` disagree: {body}"
    );
    assert!(
        body.contains("refreshTable(h->session, Trades, account->getAccountID(), resp, err, PRESEND_REFRESH_MS)"),
        "the open trades must be a bounded refresh-table request, not the login snapshot: {body}"
    );
    assert!(!body.contains("getTableRefreshResponse"), "{body}");
    // The FIFO key is the trade's own open time, written finite.
    assert!(body.contains("finite_or_zero(t->getOpenTime())"), "{body}");
}

/// The traits are a login-time read of the row `firstAccount` holds: no request, so no timeout, and the discriminator is
/// the one the SDK's samples use.
#[test]
fn the_traits_are_the_maintenance_type_of_the_account_the_orders_go_to() {
    let body = body_of("forex_connect_account_traits");
    assert!(body.contains("firstAccount(h->session)"), "{body}");
    assert!(body.contains("a->getMaintenanceType()"), "{body}");
    assert!(!body.contains("refreshTable"), "a login-time read asks the venue nothing: {body}");
    assert_eq!(
        harvest_keys(&body),
        ["maintenance_type", "account_kind", "hedge_margin_pct"]
            .iter()
            .map(|k| k.to_string())
            .collect(),
        "{body}"
    );
}

/// **The closing fill has an id of its own.** The core drops a fill whose trade id it has folded (`Account::apply_fill`
/// and the FSM's own set), the opening fill of a trade carries the bare trade id, so a close reported under it is
/// dropped as a replay and the position never closes (the class that bit Dukascopy on 2026-10-26). Both fills are built
/// by ONE `snprintf` literal, so the open and the close cannot drift apart.
#[test]
fn the_listener_reports_a_closed_trade_under_its_own_id_through_the_one_fill_literal() {
    let code = shim_code();
    let listener = &code[code.find("class EventListener").expect("the listener")..];
    let listener = &listener[..listener.find("struct Handle").expect("the listener's end")];
    for needle in [
        "r->getUpdateTable(i) == ClosedTrades && r->getUpdateType(i) == Insert",
        "r->getClosedTradeRow(i)",
        "c->getCloseOrderID()",
        "\"-close-\"",
        "(was && *was == 'B') ? \"S\" : \"B\"",
    ] {
        assert!(listener.contains(needle), "the listener lost `{needle}`: {listener}");
    }
    // The remainder of a PARTIAL close arrives as a Trades Insert with a non-empty origin: it is no execution and is
    // never reported as a fill (it would book the remainder twice).
    let skip = listener.find("origin && *origin").expect("the split remainder is no fill");
    let open_fill = listener.find("pushFill(mQ, t->getOpenOrderID()").expect("the opening fill");
    assert!(
        listener.contains("t->getTradeIDOrigin()") && skip < open_fill,
        "the skip of a split remainder must come BEFORE the opening fill is pushed: {listener}"
    );
    // The closing fill is routed by the CLOSING order, and its id is the trade's id plus that order's.
    assert!(
        listener.contains("tradeId = std::string(c->getTradeID() ? c->getTradeID() : \"\") + \"-close-\" + closeOrder"),
        "the closing fill's id is no longer `<trade id>-close-<closing order id>`: {listener}"
    );
    assert!(listener.contains("pushFill(mQ, closeOrder, tradeId.c_str()"), "{listener}");
    assert!(listener.contains("pushFill(mQ, t->getOpenOrderID(), t->getTradeID()"), "{listener}");
    // ONE literal: a second spelling of the fill envelope would escape the grammar gate's comparison.
    assert_eq!(
        code.matches("\\\"kind\\\":\\\"fill\\\"").count(),
        1,
        "the fill envelope is spelled more than once"
    );
}

/// **The closing fill carries the CLOSED row's commission, raw.** It was a hard-coded `0.0`, so an account that pays at the
/// close (or half at each deal) was booked nothing for it. The figure is not what the fill books:
/// `crates/bridges/fxcm/src/execution/commission.rs` takes the open fill's share off it first.
#[test]
fn the_closing_fill_carries_the_closed_rows_commission_and_not_a_constant() {
    let code = shim_code();
    let call = &code[code.find("pushFill(mQ, closeOrder").expect("the closing fill's pushFill")..];
    let call = call[..call.find(");").expect("the call's end")]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        call.contains("c->getAmount(), c->getCloseRate(), c->getCommission(),"),
        "the closing fill no longer carries the closed row's `getCommission()`: {call}"
    );
    // ...and, since A26, the closed row's own rollover and gross figures as the LAST arguments (the opening fill passes
    // 0.0 for both). Not the net: `getNetPL()` is the Table Manager row's, `IO2GClosedTradeRow` has none.
    assert!(
        call.ends_with("c->getRolloverInterest(), c->getGrossPL()"),
        "the closing fill no longer carries the closed row's rollover and gross figures: {call}"
    );
    assert!(!call.contains("getNetPL"), "getNetPL does not exist on IO2GClosedTradeRow: {call}");
}

/// **A partial close's remainder is NAMED to the Rust side.** The remainder's Trades Insert is skipped (it is no execution),
/// so the only place the node can learn the NEW trade id is the closing fill: the closed row's `TradeIDRemain` rides on it
/// as `remain_trade_id` (empty on an opening fill), and the reconcile read of the Trades table names each trade's
/// `trade_id_origin`. With them the exits follow the trade and the reconcile fill report reads the remainder as the
/// trade the core folded (`crates/bridges/fxcm/src/execution/trade_roots.rs`).
#[test]
fn the_closing_fill_names_the_remainder_and_the_trades_read_names_the_origin() {
    let code = shim_code();
    let fill = &code[code.find("static void pushFill(").expect("the one fill literal")..];
    let fill = fill[..fill.find("q->push(buf)").expect("the literal's end")]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(fill.contains("const char* remainTradeId"), "{fill}");
    assert!(fill.contains("\\\"remain_trade_id\\\":\\\"%s\\\""), "{fill}");
    assert!(fill.contains("jesc(remainTradeId).c_str()"), "{fill}");

    let listener = &code[code.find("class EventListener").expect("the listener")..];
    let listener = &listener[..listener.find("struct Handle").expect("the listener's end")];
    assert!(
        listener.contains("c->getTradeIDRemain()"),
        "the closed row's own remainder: {listener}"
    );
    let closing = &listener[listener.find("pushFill(mQ, closeOrder").expect("the closing fill")..];
    let closing = closing[..closing.find(");").expect("the call's end")]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        closing.contains("remain ? remain : \"\", c->getRolloverInterest()"),
        "the closing fill passes the remainder, then the closed row's rollover: {closing}"
    );
    assert!(
        listener.contains("t->getCommission(), \"\", 0.0, 0.0);"),
        "an opening fill names no remainder: {listener}"
    );

    let trades = body_of("forex_connect_trades");
    let keys: BTreeSet<String> = [
        "trade_id",
        "order_id",
        "instrument",
        "buysell",
        "amount",
        "open_rate",
        "commission",
        "trade_id_origin",
    ]
    .iter()
    .map(|k| k.to_string())
    .collect();
    assert_eq!(harvest_keys(&trades), keys, "the Trades read and `parse_fills` disagree: {trades}");
    assert!(trades.contains("jesc(t->getTradeIDOrigin()).c_str()"), "{trades}");
}

/// The three exports are OPTIONAL (no ABI bump): an installed shim without them still loads, and the Rust side then
/// refuses a reduce-only market order by name instead of sending the opening order.
#[test]
fn the_exports_are_optional_and_the_abi_does_not_move() {
    let code = shim_code();
    let version: u32 = code
        .lines()
        .find_map(|l| l.trim().strip_prefix("#define FC_ABI_VERSION"))
        .and_then(|v| v.trim().parse().ok())
        .expect("fcshim.cpp no longer defines FC_ABI_VERSION as a number");
    assert_eq!(version, 2, "a bump would strand installed shims; these exports are bound by name");
    let loader = std::fs::read_to_string(crate_dir().join("src/transport/loader.rs"))
        .expect("read loader.rs")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for bind in [
        "= bind_opt!(\"forex_connect_account_traits\", FcTable);",
        "= bind_opt!(\"forex_connect_open_trades\", FcTable);",
        "= bind_opt!(\"forex_connect_close_trade\", FcCloseTrade);",
        "= bind_opt!(\"forex_connect_close_trade_limit\", FcCloseTradeLimit);",
        "= bind_opt!(\"forex_connect_quote\", FcQuote);",
    ] {
        assert!(loader.contains(bind), "loader.rs must bind optionally, by name alone: `{bind}`");
    }
}

/// **The close under a price is the SDK's Close Limit order** (the help's "Create Close Limit Order": `CL` + `TradeID` + `Rate`,
/// executed at the rate or better, IOC by default): `forex_connect_close_trade`'s request with the order type and the rate
/// as the only differences, the rate snapped to the instrument's grid and the permission asked before the send. What FXCM
/// answers is UNMEASURED; the demo check at the FX open is its proof.
#[test]
fn the_close_under_a_price_is_the_sdks_close_limit_with_the_trade_id() {
    let body = body_of("forex_connect_close_trade_limit");
    for needle in [
        "vm->setString(Command, O2G2::Commands::CreateOrder);",
        "vm->setString(OrderType, O2G2::Orders::CloseLimit);",
        "vm->setString(AccountID, accountId.c_str());",
        "vm->setString(OfferID, offerId.c_str());",
        "vm->setString(TradeID, tradeId);",
        "vm->setString(BuySell, wasBuy ? O2G2::Sell : O2G2::Buy);",
        "vm->setInt(Amount, amount);",
        "vm->setDouble(Rate, bound);",
        "accountId = t->getAccountID()",
        "offerId = t->getOfferID()",
        "return FC_CLOSE_NOT_PERMITTED;",
        "return ambiguous;",
        "cpy(order_id_out, oid_len, reqId.c_str());",
    ] {
        assert!(body.contains(needle), "the close under a price lost `{needle}`: {body}");
    }
    for stranger in
        ["TrueMarketClose", "TrueMarketOpen", "LimitEntry", "NetQuantity", "RateStop", "RateLimit"]
    {
        assert!(!body.contains(stranger), "it sends `{stranger}`: {body}");
    }
    assert_eq!(body.matches("vm->set").count(), 9, "exactly nine request parameters: {body}");
    let send = body.find("sendRequest(req)").expect("the request is sent");
    for before in [
        "refreshTable(h->session, Trades, account->getAccountID(), resp, err, PRESEND_REFRESH_MS)",
        "amount > tradeAmount",
        "canCreateMarketCloseOrder(instrument.c_str()) != PermissionEnabled",
        "snapRate(offer.row, rate, bound, gridErr)",
        "rl->setRequestID(",
    ] {
        let at = body.find(before).unwrap_or_else(|| panic!("it lost `{before}`: {body}"));
        assert!(at < send, "`{before}` comes before the send: {body}");
    }
}

/// **The quote a Close Limit is judged by is ONE row of a FRESH read** (the review of 2026-11-16, finding 6): `findOffer` with
/// `fresh` (its refresh is bounded by `PRESEND_REFRESH_MS`, not the 8 s of `forex_connect_offers`), the asked row's bid and ask,
/// and no table written out. It sends nothing.
#[test]
fn the_quote_is_one_fresh_row_and_sends_nothing() {
    let body = body_of("forex_connect_quote");
    for needle in [
        "findOffer(h->session, instrument, /*fresh=*/true, offer, err)",
        "*bid_out = offer->getBid();",
        "*ask_out = offer->getAsk();",
    ] {
        assert!(body.contains(needle), "the quote lost `{needle}`: {body}");
    }
    for stranger in ["sendRequest", "emit_json", "FACTS_REFRESH_MS", "vm->set"] {
        assert!(!body.contains(stranger), "the quote does `{stranger}`: {body}");
    }
    let find_offer_once = body_of_static("findOfferOnce");
    assert!(
        find_offer_once.contains("refreshTable(s, Offers, 0, out.resp, err, PRESEND_REFRESH_MS)"),
        "a fresh offer is one refresh under the pre-send bound: {find_offer_once}"
    );
}

/// **The quote reads the VENUE's row for the ENGINE's name** (the live demo of 2026-11-16): the exec thread names the
/// instrument `EURUSD`, the venue's row is `EUR/USD`, and the read without `venueName` found no row (`-3 offer not found`),
/// so a fired stop-limit was never judged against the market and went out as the trade's `L`, refused ORA-20115.
#[test]
fn the_quote_finds_the_venues_row_for_the_engines_name() {
    let body = body_of("forex_connect_quote");
    let mapped = body
        .find("const std::string venue_name = venueName(h->session, instrument);")
        .expect("the quote maps the engine's name to the venue's row");
    let rebound = body.find("instrument = venue_name.c_str();").expect("and reads by it");
    let read = body.find("findOffer(h->session, instrument,").expect("the read");
    assert!(mapped < rebound && rebound < read, "mapped before the read: {body}");
}

/// The text of the `static int name(` helper, to the next `static int`.
fn body_of_static(name: &str) -> String {
    let code = shim_code();
    let start = code
        .find(&format!("static int {name}("))
        .unwrap_or_else(|| panic!("fcshim.cpp has no {name}"));
    let rest = &code[start..];
    let end = rest[10..].find("static int").map_or(rest.len(), |at| at + 10);
    rest[..end].split_whitespace().collect::<Vec<_>>().join(" ")
}
