//! The listing rule over the venue's own rows: the captured `public/get_instruments` option rows
//! (`tests/fixtures/deribit_get_instruments.json`, 2026-10-04 and 2026-10-11), plus rows in the SAME
//! documented shape for the cases the capture holds none of (a linear BTC option, an altcoin other
//! than SOL, an inactive row, a row missing its index).

use serde_json::{Value, json};

use super::*;

const FIXTURE: &str = include_str!("../tests/fixtures/deribit_get_instruments.json");

/// The captured option rows of the three books, as one `currency=any&kind=option` answer holds them.
fn captured_option_rows() -> Vec<Value> {
    let all: Value = serde_json::from_str(FIXTURE).expect("the fixture is valid JSON");
    ["option_btc", "option_eth", "option_usdc"]
        .iter()
        .flat_map(|book| all[*book]["result"].as_array().cloned().unwrap_or_default())
        .collect()
}

/// A row in the venue's documented option shape (the captured `SOL_USDC-12OCT26-96-P` row's fields).
fn option_row(name: &str, base: &str, settle: &str, index: &str) -> Value {
    json!({
        "kind": "option", "is_active": true, "instrument_name": name,
        "base_currency": base, "settlement_currency": settle, "price_index": index,
        "instrument_type": "linear", "option_type": "put", "strike": 1.0,
        "expiration_timestamp": 1_791_792_000_000_i64
    })
}

fn listed(name: &str, book: &str, index: &str) -> OptionUnderlying {
    OptionUnderlying { name: name.into(), book: book.into(), index: index.into() }
}

/// The captured rows name the three underlyings the surface served when the list was a constant, each
/// with the book its chain was read from and the index its markprice channel took: the rule reproduces
/// the old table from the venue's answer alone.
#[test]
fn the_captured_rows_list_btc_eth_and_sol_with_their_books_and_indexes() {
    assert_eq!(
        option_underlyings(&captured_option_rows()),
        vec![
            listed("BTC", "BTC", "btc_usd"),
            listed("ETH", "ETH", "eth_usd"),
            listed("SOL", "USDC", "sol_usdc"),
        ]
    );
}

/// A new underlying the venue lists is listed with no edit here; a coin listed in both books is served
/// from its coin book (its linear options are not this surface's); the order is the listing's: BTC
/// now lists two options, every other underlying one, and a tie goes by name.
#[test]
fn a_new_underlying_appears_and_a_coin_book_wins_over_the_usdc_book() {
    let mut rows = captured_option_rows();
    rows.push(option_row("BTC_USDC-12OCT26-115000-P", "BTC", "USDC", "btc_usdc"));
    rows.push(option_row("XRP_USDC-12OCT26-2d5-C", "XRP", "USDC", "xrp_usdc"));
    rows.push(option_row("AVAX_USDC-12OCT26-30-P", "AVAX", "usdc", "avax_usdc"));
    let got = option_underlyings(&rows);
    assert_eq!(
        got,
        vec![
            listed("BTC", "BTC", "btc_usd"),
            listed("AVAX", "USDC", "avax_usdc"),
            listed("ETH", "ETH", "eth_usd"),
            listed("SOL", "USDC", "sol_usdc"),
            listed("XRP", "USDC", "xrp_usdc"),
        ]
    );
    assert!(!got[0].usd_quoted(), "a coin book quotes in coin units");
    assert!(got[1].usd_quoted(), "the USDC book quotes in USD");
}

/// The order follows the venue's listing, not a name: the underlying with the most listed options
/// leads, whatever it is called.
#[test]
fn the_underlying_listing_the_most_options_leads() {
    let mut rows = captured_option_rows();
    for strike in ["1", "2", "3"] {
        let name = format!("XRP_USDC-12OCT26-{strike}-C");
        rows.push(option_row(&name, "XRP", "USDC", "xrp_usdc"));
    }
    let order: Vec<String> = option_underlyings(&rows).into_iter().map(|u| u.name).collect();
    assert_eq!(order, ["XRP", "BTC", "ETH", "SOL"]);
}

/// An inactive row, a row of another kind and a row missing its index list nothing: a delisted
/// underlying goes, and a field is never guessed.
#[test]
fn inactive_rows_other_kinds_and_incomplete_rows_list_nothing() {
    let mut inactive = option_row("TRX_USDC-12OCT26-1-C", "TRX", "USDC", "trx_usdc");
    inactive["is_active"] = json!(false);
    let mut future = option_row("HYPE_USDC-12OCT26", "HYPE", "USDC", "hype_usdc");
    future["kind"] = json!("future");
    let mut no_index = option_row("DOGE_USDC-12OCT26-1-C", "DOGE", "USDC", "doge_usdc");
    no_index.as_object_mut().expect("an object").remove("price_index");
    assert!(option_underlyings(&[inactive, future, no_index]).is_empty());
    assert!(option_underlyings(&[]).is_empty());
}
