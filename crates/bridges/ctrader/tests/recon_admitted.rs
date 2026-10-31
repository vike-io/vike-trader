//! `CtraderReconClient` reads the symbols the engine admitted on demand (any-symbol, decision 0173),
//! end to end against the in-process fake venue of `tests/common/foreign_book.rs`: an account that
//! trades three forex pairs, reconciled by a client mounted on EURUSD.
//!
//! The three wire reads are account-wide, so what these tests pin is the client-side scope: nothing
//! admitted reads the mounted symbol only (the behaviour before this feature), an admitted symbol is
//! read beside it (its resting orders, its fills out of the one deal list, a position row, flat when the
//! venue omits it), and a pass that cannot read a symbol it claims FAILS whole.

mod common;

use std::time::Duration;

use vike_ctrader::CtraderReconClient;
use vike_ctrader::conn::ConnConfig;
use vike_exec::recon::ReconClient;

use common::{FakeCtrader, ForeignBook};

const EURUSD: i64 = 1;
const GBPUSD: i64 = 2;
const USDJPY: i64 = 3;

/// Orders, positions and deals on EURUSD and GBPUSD, plus an order on USDJPY nobody admits.
fn book() -> ForeignBook {
    ForeignBook {
        orders: vec![(EURUSD, 11, "eur-1"), (GBPUSD, 22, "gbp-1"), (USDJPY, 33, "jpy-1")],
        positions: vec![(EURUSD, 101, 100_000), (GBPUSD, 202, 250_000)],
        deals: vec![(EURUSD, 1, 100_000), (GBPUSD, 2, 250_000), (USDJPY, 3, 300_000)],
        ..Default::default()
    }
}

/// A client mounted on EURUSD over `book`. The revive age is far off, so every pass rides the one socket.
fn client(book: ForeignBook) -> (FakeCtrader, CtraderReconClient) {
    let server = FakeCtrader::start_foreign_book(book);
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let client =
        CtraderReconClient::connect_with_idle_revive(&cfg, "EURUSD", Duration::from_secs(86_400))
            .expect("recon client connects");
    (server, client)
}

fn symbols<T>(rows: &[T], of: impl Fn(&T) -> &String) -> Vec<String> {
    rows.iter().map(|r| of(r).clone()).collect()
}

/// The account's deposit currency travels from the TRADER response through the handshake into the
/// listing, and the any-symbol fence over that listing admits the deposit-quoted pairs and names the rest:
/// the whole facts path of the bridge, over the wire, minus the gatewayhub's cache.
#[test]
fn the_handshake_carries_the_deposit_currency_into_the_fence() {
    use vike_ctrader::catalog::{any_symbol_refusal, listing_of};
    use vike_ctrader::symbols::SymbolMap;

    let server = FakeCtrader::start_foreign_book(book());
    let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
    let lists = vike_ctrader::conn::fetch_symbol_lists(cfg).expect("the handshake lists");
    let map = SymbolMap::from_lists(&lists);
    assert_eq!(map.deposit_currency(), Some("USD"), "ProtoOATrader.deposit_asset_id, named");

    let (deposit, rows) = listing_of(&map).expect("a named deposit currency");
    assert_eq!(deposit, "USD");
    let refusal = |name: &str| {
        let row = rows.iter().find(|r| r.raw_symbol == name).expect(name);
        any_symbol_refusal(row, &deposit)
    };
    assert_eq!(refusal("EURUSD"), None);
    assert_eq!(refusal("GBPUSD"), None);
    let jpy = refusal("USDJPY").expect("quoted in JPY");
    assert!(jpy.contains("quoted in JPY") && jpy.contains("account's currency is USD"), "{jpy}");
}

/// The account TYPE travels the same way, from the TRADER response into the map the exec client
/// reads (`SymbolMap::account_may_hold_a_hedge`): only an account the venue states NETTED is not
/// read as able to hold a hedge. A venue that states nothing, and a spread-betting account, are.
#[test]
fn the_handshake_carries_the_account_type_into_the_hedge_fence() {
    use vike_ctrader::proto::ProtoOaAccountType;
    use vike_ctrader::symbols::SymbolMap;

    for (stated, may_hedge) in [
        (Some(ProtoOaAccountType::Netted as i32), false),
        (Some(ProtoOaAccountType::Hedged as i32), true),
        (Some(ProtoOaAccountType::SpreadBetting as i32), true),
        (None, true),
    ] {
        let server =
            FakeCtrader::start_foreign_book(ForeignBook { account_type: stated, ..book() });
        let cfg = ConnConfig::for_test(server.addr(), "cid", "secret", "token");
        let lists = vike_ctrader::conn::fetch_symbol_lists(cfg).expect("the handshake lists");
        assert_eq!(
            SymbolMap::from_lists(&lists).account_may_hold_a_hedge(),
            may_hedge,
            "the venue states account type {stated:?}"
        );
    }
}

/// With nothing admitted a pass reads the mounted symbol and nothing else, exactly as before.
#[test]
fn nothing_admitted_reads_the_mounted_symbol_only() {
    let (_server, client) = client(book());
    assert_eq!(client.admitted_symbols(), Some(Vec::new()), "a claim of nothing, not no claim");

    let orders = client.fetch_order_status_reports(0).expect("orders");
    assert_eq!(symbols(&orders, |o| &o.symbol), ["EURUSD"]);
    let fills = client.fetch_fill_reports(0).expect("fills");
    assert_eq!(symbols(&fills, |f| &f.symbol), ["EURUSD"]);
    let positions = client.fetch_position_status_reports().expect("positions");
    assert_eq!(symbols(&positions, |p| &p.symbol), ["EURUSD"]);
}

/// An admitted symbol is read beside the mounted one, in the broker's spelling, on all three reads, and a
/// symbol nobody admitted stays out.
#[test]
fn an_admitted_symbol_is_read_beside_the_mounted_one() {
    let (_server, client) = client(book());
    // The engine's spelling is the broker's; the list keeps it upper-cased, the read matches it back.
    client.admit_symbol("gbpusd");
    assert_eq!(client.admitted_symbols(), Some(vec!["GBPUSD".to_string()]));

    let orders = client.fetch_order_status_reports(0).expect("orders");
    assert_eq!(symbols(&orders, |o| &o.symbol), ["EURUSD", "GBPUSD"], "USDJPY is not admitted");
    assert_eq!(orders[1].client_order_id.as_deref(), Some("gbp-1"));

    let fills = client.fetch_fill_reports(0).expect("fills");
    assert_eq!(
        symbols(&fills, |f| &f.symbol),
        ["EURUSD", "GBPUSD"],
        "mounted first, then admitted"
    );
    assert_eq!(fills[1].last_qty, 2500.0, "250_000 centi-units");

    let positions = client.fetch_position_status_reports().expect("positions");
    assert_eq!(symbols(&positions, |p| &p.symbol), ["EURUSD", "GBPUSD"]);
    assert_eq!(positions[1].qty, 2500.0);
}

/// An admitted symbol the venue holds nothing in reports the flat row cTrader's omission calls for, so a
/// stale LOCAL position in it stays detectable; the mounted symbol's own row is unchanged.
#[test]
fn an_admitted_symbol_with_no_position_reports_flat() {
    let (_server, client) = client(book());
    client.admit_symbol("USDJPY");
    let positions = client.fetch_position_status_reports().expect("positions");
    assert_eq!(symbols(&positions, |p| &p.symbol), ["EURUSD", "USDJPY"]);
    assert_eq!((positions[1].qty, positions[1].avg_px), (0.0, 0.0));
}

/// Telling the client its mounted symbol again, in any case, adds nothing; telling it one twice adds one.
#[test]
fn admit_symbol_is_idempotent_and_never_lists_the_mounted_symbol() {
    let (_server, client) = client(book());
    client.admit_symbol("eurusd");
    client.admit_symbol("GBPUSD");
    client.admit_symbol("gbpusd");
    assert_eq!(client.admitted_symbols(), Some(vec!["GBPUSD".to_string()]));
}

/// A pass never claims a symbol it did not read: an admitted name the broker's list no longer carries
/// fails EVERY read (the restore planner reads a missing order as gone), not just its own.
#[test]
fn a_symbol_the_broker_dropped_fails_the_whole_pass() {
    // The list the client handshakes against has no GBPUSD, though the engine admitted it.
    let (_server, client) = client(ForeignBook { omit_symbols: vec![GBPUSD], ..book() });
    client.admit_symbol("GBPUSD");
    for why in [
        client.fetch_order_status_reports(0).expect_err("orders"),
        client.fetch_fill_reports(0).expect_err("fills"),
        client.fetch_position_status_reports().expect_err("positions"),
    ] {
        assert!(
            why.contains("GBPUSD") && why.contains("not in the account's symbol list"),
            "{why}"
        );
    }
}

/// Six deals across the three pairs, in id (= time) order, for the paging tests.
fn six_deals(page: usize, newest_first: bool) -> ForeignBook {
    ForeignBook {
        deals: vec![
            (EURUSD, 1, 100_000),
            (GBPUSD, 2, 200_000),
            (EURUSD, 3, 300_000),
            (GBPUSD, 4, 400_000),
            (USDJPY, 5, 500_000),
            (GBPUSD, 6, 600_000),
        ],
        deals_page: page,
        deals_newest_first: newest_first,
        ..book()
    }
}

fn deal_ids(fills: &[vike_model::FillReport]) -> Vec<(String, f64)> {
    fills.iter().map(|f| (f.symbol.clone(), f.last_qty)).collect()
}

/// A deal list capped at two rows per reply is PAGED when a symbol is admitted, whichever end the venue
/// lists first: every fill of the mounted and of the admitted symbol comes back once, none twice.
#[test]
fn a_capped_deal_list_is_paged_for_an_admitted_symbol_in_either_order() {
    for newest_first in [false, true] {
        let (_server, client) = client(six_deals(2, newest_first));
        client.admit_symbol("GBPUSD");
        let fills = client.fetch_fill_reports(0).expect("paged to the end");
        // The order within a symbol is the venue's (newest first reads backwards): compare as a set.
        let mut got = deal_ids(&fills);
        got.sort_by(|a, b| a.partial_cmp(b).expect("finite"));
        assert_eq!(
            got,
            [
                ("EURUSD".to_string(), 1000.0),
                ("EURUSD".to_string(), 3000.0),
                ("GBPUSD".to_string(), 2000.0),
                ("GBPUSD".to_string(), 4000.0),
                ("GBPUSD".to_string(), 6000.0),
            ],
            "newest_first = {newest_first}"
        );
    }
}

/// Nothing admitted: ONE request, the first chunk, no paging (the behaviour before the any-symbol work).
#[test]
fn a_capped_deal_list_is_one_chunk_when_nothing_is_admitted() {
    let (_server, client) = client(six_deals(2, false));
    let fills = client.fetch_fill_reports(0).expect("the first chunk");
    assert_eq!(deal_ids(&fills), [("EURUSD".to_string(), 1000.0)], "deals 1 and 2 only");
}

/// Paging is bounded: a list that is still truncated after ten requests fails the pass by name.
#[test]
fn a_deal_list_still_truncated_after_ten_pages_fails_the_pass() {
    let deals =
        (1..=30).map(|id| (if id % 2 == 0 { GBPUSD } else { EURUSD }, id, 100_000)).collect();
    let (_server, client) = client(ForeignBook { deals, deals_page: 2, ..book() });
    client.admit_symbol("GBPUSD");
    let why = client.fetch_fill_reports(0).expect_err("30 deals at two a page");
    assert!(why.contains("has_more") && why.contains("paging"), "{why}");
}

/// The deal list is account-wide and capped. With a symbol admitted, a reply that says it has more rows
/// than it carries (and keeps saying so with nothing new to page to) cannot back a claim to have read that
/// symbol's fills, so the pass fails; with nothing admitted the first chunk is read as it always was.
#[test]
fn a_capped_deal_list_fails_the_pass_only_once_a_symbol_is_admitted() {
    let (_server, client) = client(ForeignBook { deals_have_more: true, ..book() });
    let fills = client.fetch_fill_reports(0).expect("nothing admitted: the first chunk, as before");
    assert_eq!(symbols(&fills, |f| &f.symbol), ["EURUSD"]);

    client.admit_symbol("GBPUSD");
    let why = client.fetch_fill_reports(0).expect_err("an admitted symbol, a capped list");
    assert!(why.contains("has_more"), "{why}");
    // The orders and the positions are not capped, and still read both symbols.
    assert_eq!(client.fetch_order_status_reports(0).expect("orders").len(), 2);
}
