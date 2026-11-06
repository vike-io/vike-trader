//! The control door's spelling of a Bybit symbol (decision 0231): the venue's own name, upper case. A linear
//! perpetual is bare (`ETHUSDT`, and a `.P` typed is dropped), a DATED linear future keeps its delivery day
//! (`ETHUSDT-27NOV26`), a coin-margined contract is its own bare name (`BTCUSD`, `BTCUSDZ26`), and a SPOT pair
//! is `PAIR.S`, because the bare name of a pair is also the perpetual of the same name. An option is refused by
//! name, not as a misspelling.

use crate::server::symbol_admission::{Spelling, canonical_symbol};

fn canon(raw: &str) -> Result<String, String> {
    canonical_symbol(Spelling::BareLinear, raw)
}

#[test]
fn a_bybit_name_keeps_the_book_it_names() {
    for (typed, canonical) in [
        ("ethusdt", "ETHUSDT"),
        ("ethusdt.p", "ETHUSDT"),
        ("ETHUSDT-27NOV26", "ETHUSDT-27NOV26"),
        (" ethusdt-27nov26 ", "ETHUSDT-27NOV26"),
        ("btcusd", "BTCUSD"),
        ("BTCUSDZ26", "BTCUSDZ26"),
        ("dogeusdt.s", "DOGEUSDT.S"),
        ("1000PEPEUSDT.S", "1000PEPEUSDT.S"),
    ] {
        assert_eq!(canon(typed).as_deref(), Ok(canonical), "{typed}");
    }
}

#[test]
fn what_is_not_a_bybit_name_is_refused_in_words() {
    for raw in [
        "",
        "ETH USDT",
        "ETHUSDT-27NOV2",
        "ETHUSDT-27nov26x",
        "ETH-USDT",
        ".S",
        "ETHUSDT.S.S",
        "ETH/USDT",
    ] {
        let why = canon(raw).expect_err(raw);
        assert!(why.contains("not"), "{raw}: {why}");
    }
    // An option is named as one.
    let why = canon("BTC-31OCT26-75500-C-USDT").expect_err("an option");
    assert!(why.contains("option") && why.contains("payoff"), "{why}");
}
