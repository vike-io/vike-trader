//! **The shim closes a trade by its id the way FXCM's own SDK sample does**, gated on a box with no ForexConnect SDK like
//! `fxcm_shim_subscription.rs`: no CI runner has ever compiled `crates/bridges/fxcm/src/shim/fcshim.cpp`, so what the
//! three close exports send, what they read, which codes they return and how the closing fill is named are pinned from
//! the SOURCE. They cannot prove what FXCM answers; the answers they are held against were measured on the demo login
//! (`docs/ops/fxcm-forexconnect.md`'s "Closing a trade by its id"), and `tests/fxcm_close_smoke.rs` is the live proof of
//! the whole path.
//!
//! Why it matters: on a HEDGING account an opposite market order OPENS a second trade (measured 2026-10-13/14), so every
//! close the core sends was an open until `fc_close_trade` existed, and the close it makes never reaches the node unless
//! the event listener reports the ClosedTrades row under an id the core has not folded.

use std::collections::BTreeSet;
use std::path::PathBuf;

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// `fcshim.cpp` with `//` comments removed (its prose names calls its code must not make).
fn shim_code() -> String {
    let path = crate_dir().join("src/shim/fcshim.cpp");
    let src = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read the shim source {}: {e}", path.display()));
    src.lines()
        .map(|l| match l.find("//") {
            Some(at) if !l[..at].contains('"') => &l[..at],
            _ => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The text of the export `name`, from its signature to the next export, whitespace squeezed.
fn body_of(name: &str) -> String {
    let code = shim_code();
    let start = code
        .find(&format!("FC_EXPORT int {name}("))
        .unwrap_or_else(|| panic!("fcshim.cpp does not export {name}"));
    let rest = &code[start..];
    let end = rest[10..].find("FC_EXPORT").map_or(rest.len(), |at| at + 10);
    rest[..end].split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The request is the SDK sample's (`ClosePosition`'s `createCloseMarketOrderRequest`): `TrueMarketClose` carrying the
/// trade's own account, offer and id, the side AGAINST the trade, and its amount; no rate, no net quantity.
#[test]
fn the_request_is_the_sdks_true_market_close_with_the_trade_id() {
    let body = body_of("fc_close_trade");
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
            "fc_close_trade sends `{stranger}`, which the close request does not carry: {body}"
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
    let body = body_of("fc_close_trade");
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
    let body = body_of("fc_close_trade");
    for code in [
        "return -3;",
        "return -10;",
        "return FC_CLOSE_NOT_PERMITTED;",
        "return ambiguous;",
        "ambiguous = FC_SENT_NO_ORDER_ID;",
        "ambiguous = FC_SENT_NO_ANSWER;",
        "cpy(order_id_out, oid_len, reqId.c_str());",
    ] {
        assert!(body.contains(code), "fc_close_trade lost `{code}`: {body}");
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

/// The keys `fc_open_trades` writes, harvested from its `snprintf` literal: the five `close::parse_open_trades` reads
/// (and whose removal its in-crate test shows to fail the parse).
fn keys_of(text: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let needle = "\\\"";
    let mut i = 0;
    while let Some(off) = text[i..].find(needle) {
        let start = i + off + needle.len();
        let Some(close) = text[start..].find(needle) else { break };
        let key = &text[start..start + close];
        let after = start + close + needle.len();
        if text[after..].starts_with(':')
            && !key.is_empty()
            && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            out.insert(key.to_string());
        }
        i = start;
    }
    out
}

#[test]
fn the_open_trades_row_has_the_five_keys_the_planner_reads_and_is_a_fresh_bounded_read() {
    let body = body_of("fc_open_trades");
    let keys: BTreeSet<String> = ["trade_id", "instrument", "buysell", "amount", "open_time"]
        .iter()
        .map(|k| k.to_string())
        .collect();
    assert_eq!(
        keys_of(&body),
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
    let body = body_of("fc_account_traits");
    assert!(body.contains("firstAccount(h->session)"), "{body}");
    assert!(body.contains("a->getMaintenanceType()"), "{body}");
    assert!(!body.contains("refreshTable"), "a login-time read asks the venue nothing: {body}");
    assert_eq!(
        keys_of(&body),
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
/// `crates/bridges/fxcm/src/commission.rs` takes the open fill's share off it first.
#[test]
fn the_closing_fill_carries_the_closed_rows_commission_and_not_a_constant() {
    let code = shim_code();
    let call = &code[code.find("pushFill(mQ, closeOrder").expect("the closing fill's pushFill")..];
    let call = call[..call.find(");").expect("the call's end")]
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        call.ends_with("c->getAmount(), c->getCloseRate(), c->getCommission()"),
        "the closing fill no longer carries the closed row's `getCommission()`: {call}"
    );
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
    let loader = std::fs::read_to_string(crate_dir().join("src/loader.rs"))
        .expect("read loader.rs")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    for bind in [
        "= bind_opt!(\"fc_account_traits\", FcTable);",
        "= bind_opt!(\"fc_open_trades\", FcTable);",
        "= bind_opt!(\"fc_close_trade\", FcCloseTrade);",
    ] {
        assert!(loader.contains(bind), "loader.rs must bind optionally, by name alone: `{bind}`");
    }
}
