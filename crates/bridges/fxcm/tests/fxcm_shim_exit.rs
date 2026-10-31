//! **The shim binds a stop-loss and a take-profit to ONE trade the way FXCM's own SDK sample does**, gated on a box with
//! no ForexConnect SDK like `fxcm_shim_close.rs`: no CI runner has ever compiled `crates/bridges/fxcm/src/shim/fcshim.cpp`,
//! so what the two exit exports send, what they read, which codes they return and which table they scan are pinned from
//! the SOURCE. They cannot prove what FXCM answers; the answers they are held against were measured on the demo login
//! (`docs/ops/fxcm-forexconnect.md`'s "Trade-bound stop-loss and take-profit"), and `tests/fxcm_stop_smoke.rs` is the
//! live proof of the whole path.
//!
//! Why it matters: on a HEDGING account a plain resting opposite order is an ENTRY order that OPENS a trade when it is hit
//! (measured 2026-10-26), so the only stop that protects a trade is the one bound to it (`OrderType` `S` / `L` with the
//! trade's `TradeID`); a request that lost one of those keys would be accepted by the venue as something else.

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

/// The request is the SDK sample's `attachStopOrderRequest`: `Stop` / `Limit` carrying the trade's own account, offer and
/// id, the side AGAINST the trade, its whole amount and the rate. Never an entry order, never a net or market order.
#[test]
fn the_request_is_the_sdks_trade_bound_stop_or_limit() {
    let body = body_of("fc_place_exit");
    for needle in [
        "vm->setString(Command, O2G2::Commands::CreateOrder);",
        "vm->setString(OrderType, limit ? O2G2::Orders::Limit : O2G2::Orders::Stop);",
        "vm->setString(AccountID, accountId.c_str());",
        "vm->setString(OfferID, offerId.c_str());",
        "vm->setString(TradeID, tradeId);",
        "vm->setString(BuySell, buysell);",
        "vm->setInt(Amount, amount);",
        "vm->setDouble(Rate, snapped);",
        "const char* buysell = wasBuy ? O2G2::Sell : O2G2::Buy;",
    ] {
        assert!(body.contains(needle), "the request lost `{needle}`: {body}");
    }
    for stranger in [
        "TrueMarket",
        "LimitEntry",
        "StopEntry",
        "NetQuantity",
        "RateStop",
        "RateLimit",
        "PegType",
        "ContingencyType",
    ] {
        assert!(
            !body.contains(stranger),
            "fc_place_exit sends `{stranger}`, which a trade-bound exit does not carry: {body}"
        );
    }
    assert_eq!(body.matches("vm->set").count(), 9, "exactly nine request parameters: {body}");
    // The ids come from the trade's OWN row, never from a caller's guess; the kind is S or L and nothing else.
    for needle in [
        "accountId = t->getAccountID()",
        "offerId = t->getOfferID()",
        "wasBuy = sameStr(t->getBuySell(), O2G2::Buy)",
        "strcmp(kind, \"S\") != 0 && strcmp(kind, \"L\") != 0",
    ] {
        assert!(body.contains(needle), "the exit lost `{needle}`: {body}");
    }
}

/// The trade is read from a FRESH Trades table of the account inside the order path's bound, the amount (a WHOLE trade:
/// the venue refuses a part, `ORA-20134`), the permission and the price grid are judged BEFORE anything is sent, and the
/// listener is armed first.
#[test]
fn the_trade_is_read_fresh_everything_is_judged_before_the_send_and_the_listener_is_armed_first() {
    let body = body_of("fc_place_exit");
    assert!(
        body.contains("refreshTable(h->session, Trades, account->getAccountID(), resp, err, PRESEND_REFRESH_MS)"),
        "the trade must come from a bounded refresh-table request of the account's Trades: {body}"
    );
    assert!(
        !body.contains("getTableRefreshResponse"),
        "the login snapshot holds no trades: {body}"
    );
    let found = body.find("sameStr(t->getTradeID(), tradeId)").expect("the trade is found by id");
    let whole =
        body.find("amount != tradeAmount").expect("the amount must be the trade's whole amount");
    let permission = body
        .find("canCreateStopLimitOrder(instrument.c_str()) != PermissionEnabled")
        .expect("the stop/limit permission is checked");
    let grid =
        body.find("snapRate(offer.row, rate, snapped, gridErr)").expect("the rate is grid-checked");
    let arm = body.find("rl->setRequestID(").expect("the listener is armed");
    let send = body.find("sendRequest(req)").expect("the request is sent");
    assert!(
        found < whole && whole < permission && permission < grid && grid < arm && arm < send,
        "order: find, whole amount, permission, grid, arm, send: {body}"
    );
    assert!(body.contains("unsubscribeResponse(rl); rl->release();"), "the listener leaks: {body}");
    // The reply wait is ORDER_WAIT_MS (the listener's default), the bound a placement has.
    assert!(body.contains("rl->wait()"), "the reply wait lost its bound: {body}");
}

/// The returns the Rust side reads: -3 gone, -10 a bad argument/amount/rate, `FC_EXIT_NOT_PERMITTED`, -11/-12 sent with no usable
/// answer (the exit RESTS, so the Orders table is looked up by the RequestID before the outcome is called unknown, and the
/// REQUEST id is in the order-id buffer when it is), and the `refreshTable` codes passed through.
#[test]
fn the_returns_are_the_ones_the_rust_side_reads() {
    let body = body_of("fc_place_exit");
    for code in [
        "return -3;",
        "return -10;",
        "return FC_EXIT_NOT_PERMITTED;",
        "return ambiguous;",
        "ambiguous = FC_SENT_NO_ORDER_ID;",
        "ambiguous = FC_SENT_NO_ANSWER;",
        "cpy(order_id_out, oid_len, reqId.c_str());",
        "findOrderByRequestId(h->session, accountId.c_str(), reqId.c_str(), sent,",
    ] {
        assert!(body.contains(code), "fc_place_exit lost `{code}`: {body}");
    }
    let value: i32 = shim_code()
        .lines()
        .find_map(|l| l.trim().strip_prefix("#define FC_EXIT_NOT_PERMITTED"))
        .and_then(|v| v.trim().trim_start_matches('(').trim_end_matches(')').parse().ok())
        .expect("fcshim.cpp defines FC_EXIT_NOT_PERMITTED as a number");
    // -16 is `server/fxcm-exotic-pairs`' `FC_ORDER_NOT_PERMITTED`; `fxcm_shim_return_codes.rs` holds the whole set distinct.
    assert_eq!(value, -17, "the documented code");
}

/// The keys of an `snprintf` literal, harvested the way the other shim gates do.
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

/// `fc_open_exits` writes the keys `exit::parse_open_exits` reads (the five the binder cannot do without, and the order's
/// `rate` and its account and offer ids, which only the takeover of an exit a restart left reads), from a FRESH bounded
/// Orders refresh of the account, and only `S` / `L` orders that NAME a trade: an entry order (`SE`, `LE`) carries the id of
/// the trade it WOULD open and must never read as cover for a trade.
#[test]
fn the_open_exits_row_has_the_keys_the_binder_reads_and_scans_the_orders_table_for_s_and_l() {
    let body = body_of("fc_open_exits");
    let keys: BTreeSet<String> =
        ["order_id", "trade_id", "type", "buysell", "amount", "rate", "account_id", "offer_id"]
            .iter()
            .map(|k| k.to_string())
            .collect();
    assert_eq!(
        keys_of(&body),
        keys,
        "the shim's row and `exit::parse_open_exits` disagree: {body}"
    );
    assert!(
        body.contains("refreshTable(h->session, Orders, account->getAccountID(), resp, err, PRESEND_REFRESH_MS)"),
        "the exits must be a bounded refresh-table request, not the login snapshot: {body}"
    );
    assert!(!body.contains("getTableRefreshResponse"), "{body}");
    assert!(
        body.contains("strcmp(type, \"S\") != 0 && strcmp(type, \"L\") != 0")
            && body.contains("!trade || !*trade"),
        "an exit is an `S` or `L` order that names its trade: {body}"
    );
    assert!(body.contains("o->getTradeID()"), "{body}");
    // The three extra keys are the order's OWN terms: a rate read from nowhere else would let the takeover adopt an exit
    // that is not the one asked for.
    for needle in
        ["finite_or_zero(o->getRate())", "jesc(o->getAccountID())", "jesc(o->getOfferID())"]
    {
        assert!(body.contains(needle), "the takeover's keys lost `{needle}`: {body}");
    }
}

/// **A fired exit reaches the node through the listener that already existed.** The ClosedTrades row's `CloseOrderID` is
/// the exit's own order id (measured), so the closing fill routes to the exit's client order id, and the venue's removal of
/// the sibling exit is an Orders Delete with status `C`, which the same listener turns into a `canceled` envelope for the
/// sibling's order id. This pins that both arms stay as the measurements need them; nothing was added to the listener.
#[test]
fn a_fired_exit_and_the_removal_of_its_sibling_ride_the_listeners_existing_arms() {
    let code = shim_code();
    let listener = &code[code.find("class EventListener").expect("the listener")..];
    let listener = &listener[..listener.find("struct Handle").expect("the listener's end")];
    for needle in [
        "r->getUpdateTable(i) == ClosedTrades && r->getUpdateType(i) == Insert",
        "pushFill(mQ, closeOrder, tradeId.c_str()",
        "r->getUpdateTable(i) == Orders && r->getUpdateType(i) == Delete",
        "if (s == 'C')",
        "\\\"kind\\\":\\\"canceled\\\"",
    ] {
        assert!(listener.contains(needle), "the listener lost `{needle}`: {listener}");
    }
    // The executed exit's own Orders Delete (status `F`) is no cancel: only `C` (and `R`) read as one, so a fired stop is
    // not also reported cancelled ahead of its own closing fill.
    assert!(
        !listener.contains("s == 'F'"),
        "an executed order must not read as a cancel: {listener}"
    );
}

/// The two exports are OPTIONAL (no ABI bump): an installed shim without them still loads, and the Rust side then refuses a
/// reduce-only stop or limit by name instead of sending the entry order.
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
    for bind in
        ["= bind_opt!(\"fc_open_exits\", FcTable);", "= bind_opt!(\"fc_place_exit\", FcPlaceExit);"]
    {
        assert!(loader.contains(bind), "loader.rs must bind optionally, by name alone: `{bind}`");
    }
}
