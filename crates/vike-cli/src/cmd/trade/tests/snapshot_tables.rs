//! The REPL's orders table: the coid and symbol columns, truncation, and the pre-fold stamp.

use super::*;

// -- the orders table ----------------------------------------------------------------------

/// A Polymarket-shaped order: a long decimal token id for a symbol, and a coid at the upper end
/// of what `submit --coid` accepts.
fn order(coid: &str, symbol: &str) -> vike_tradehub_client::wire::WireOrderView {
    vike_tradehub_client::wire::WireOrderView {
        client_order_id: coid.to_string(),
        venue: "polymarket".to_string(),
        account: None,
        symbol: symbol.to_string(),
        side: 1,
        qty: 10.0,
        order_type: "Limit".to_string(),
        price: Some(0.51),
        trigger_price: None,
        status: "Accepted".to_string(),
        venue_order_id: None,
        filled_qty: 0.0,
        avg_fill_px: 0.0,
    }
}

fn snap_with(orders: Vec<vike_tradehub_client::wire::WireOrderView>) -> WireSnapshot {
    WireSnapshot { orders, ..WireSnapshot::empty() }
}

/// The coid is the HANDLE — it is what the operator types back at `cancel <coid>` — so it must
/// survive the table whole. The column was a fixed 20 while `submit --coid` accepts 32, so a
/// pinned id came back `aaaaaaaaaaaaaaaaaaa…` and could not be retyped at all.
#[test]
fn the_coid_column_is_never_truncated() {
    let coid = "A".repeat(32);
    let table = orders_table(&snap_with(vec![order(&coid, "BTCUSDT")]), None);
    assert!(table.contains(&coid), "the coid must survive whole:\n{table}");
    assert!(!table.contains('…'), "nothing on this row is long enough to shorten:\n{table}");
}

/// The symbol IS capped (a Polymarket token id is a ~77-digit decimal and would push every later
/// column off the terminal) but truncated in the MIDDLE: the tokens of one up/down family share
/// a long prefix and differ at the TAIL, so a head-only cell named every order in the family
/// identically — the reported `DUMMYTOKEN000…`.
#[test]
fn a_long_symbol_keeps_both_ends() {
    let a = format!("{}0001", "7".repeat(72));
    let b = format!("{}9999", "7".repeat(72));
    let table = orders_table(&snap_with(vec![order("c-1", &a), order("c-2", &b)]), None);

    assert!(table.contains("0001") && table.contains("9999"), "tails must survive:\n{table}");
    assert!(table.contains("7777"), "…and so must the head:\n{table}");
    // The two rows must be TELLABLE APART, which is the whole defect.
    let lines: Vec<&str> = table.lines().filter(|l| l.contains('…')).collect();
    assert_eq!(lines.len(), 2, "both rows truncate:\n{table}");
    assert_ne!(lines[0], lines[1], "two token ids rendered identically:\n{table}");
}

/// An ordinary instrument is untouched — the cap is generous enough that widening it cost the
/// common case nothing.
#[test]
fn ordinary_symbols_are_not_truncated() {
    for symbol in ["BTCUSDT", "EUR/USD", "BTC-30AUG26-120000-C"] {
        let table = orders_table(&snap_with(vec![order("c-1", symbol)]), None);
        assert!(table.contains(symbol), "{symbol} was shortened:\n{table}");
    }
}

#[test]
fn trunc_mid_keeps_both_ends_and_degrades_cleanly() {
    assert_eq!(trunc_mid("abcdef", 6), "abcdef", "a fitting string is untouched");
    assert_eq!(trunc_mid("abcdefghij", 5), "ab…ij");
    assert_eq!(trunc_mid("abcdefghij", 4), "ab…j", "an odd budget favours the head");
    assert_eq!(trunc_mid("abcdefghij", 2), "a…");
    assert_eq!(trunc_mid("abcdefghij", 1), "…");
    assert_eq!(trunc_mid("abcdefghij", 0), "");
    // Char-counted, not byte-counted — a multi-byte symbol must not panic on a slice boundary.
    assert_eq!(trunc_mid("ααααββββ", 5), "αα…ββ");
}

#[test]
fn width_of_fits_the_header_and_honours_the_cap() {
    assert_eq!(width_of("coid", ["ab"].into_iter(), None), 4, "never narrower than the header");
    assert_eq!(width_of("coid", ["abcdefgh"].into_iter(), None), 8, "uncapped grows to fit");
    assert_eq!(width_of("coid", ["abcdefgh"].into_iter(), Some(5)), 5, "capped stops");
    assert_eq!(width_of("symbol", std::iter::empty(), Some(24)), 6, "no rows ⇒ the header");
}

/// The empty view still renders (and still carries its `[seq N]` stamp). A BUILT frame with no
/// orders, deliberately: `seq: 0` is the pre-fold case, pinned by the test below.
#[test]
fn an_empty_orders_table_says_none() {
    let table = orders_table(&WireSnapshot { seq: 5, ..WireSnapshot::empty() }, None);
    assert!(table.contains("(none)"), "{table}");
    assert!(table.contains("[seq 5]"), "{table}");
}

/// **A frame with nothing built must not read as the node's state.** `[seq 0] orders` over
/// `(none)` is what an operator reads as "there are no orders", and on a frame the node published
/// before its first fold — or on this client's own placeholder, before any node frame arrived —
/// that is not a reading at all. Every REPL table header carries this stamp (`orders`,
/// `positions`, `equity`, `recent`, the mode line), so the mark rides on it; a built frame keeps
/// the bare `[seq N]` it always had.
#[test]
fn a_pre_fold_frame_is_marked_in_every_table_header() {
    let table = orders_table(&snap_with(Vec::new()), None);
    assert!(
        table.contains("PRE-FOLD"),
        "a seq-0 frame must be marked, not shown as state:\n{table}"
    );
    assert_eq!(stamp(&WireSnapshot::empty()), "[seq 0 PRE-FOLD]");
    assert_eq!(stamp(&WireSnapshot { seq: 3, ..WireSnapshot::empty() }), "[seq 3]");
}

/// The line printed above a pre-fold render says WHICH placeholder it is — the node's own (it is
/// up and has folded nothing) or this client's own (nothing from the node arrived in time) — and a
/// built frame gets none. The same split `node_snapshot`'s `pre_fold_note` makes, read off the
/// same [`is_node_frame`].
#[test]
fn the_pre_fold_line_names_whose_placeholder_it_is_and_a_built_frame_gets_none() {
    let identity = vike_tradehub_client::wire::WireNodeIdentity {
        name: "idle".to_string(),
        strategy: "spread_maker".to_string(),
        params: String::new(),
        live: false,
        build: "test".to_string(),
        advertise_addr: String::new(),
    };
    let node = pre_fold_line(&WireSnapshot { identity: Some(identity), ..WireSnapshot::empty() })
        .expect("the node's pre-fold placeholder is marked");
    assert!(
        node.contains("the node is up") && node.contains("stamped with its identity"),
        "{node}"
    );
    let client =
        pre_fold_line(&WireSnapshot::empty()).expect("the client's own placeholder is marked");
    assert!(client.contains("client's OWN") && !client.contains("the node is up"), "{client}");
    for line in [node, client] {
        assert!(line.starts_with("[pre-fold]") && line.contains("does NOT mean"), "{line}");
    }
    assert_eq!(pre_fold_line(&WireSnapshot { seq: 1, ..WireSnapshot::empty() }), None);
}
