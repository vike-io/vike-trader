//! The control door's spelling of a Hyperliquid perpetual (decision 0174): the coin name exactly as
//! the venue lists it. `BTC`, `ETH` and `kPEPE` are three different spellings of three different
//! coins, and `KPEPE` is not `kPEPE`, so the door must not upper-case what it admits.
//!
//! This file tests the SPELLING, which is independent of whether the venue's `SYMBOL_RULES` row
//! admits yet; the row's own tests (`admits_any_symbol`) ride with the row.

use crate::server::symbol_admission::{Spelling, canonical_symbol};

#[test]
fn a_hyperliquid_coin_keeps_its_case() {
    for coin in ["BTC", "ETH", "kPEPE", "kBONK", "1000FLOKI", "HYPE", "k1000X"] {
        assert_eq!(canonical_symbol(Spelling::CaseKept, coin).as_deref(), Ok(coin), "{coin}");
    }
    assert_eq!(canonical_symbol(Spelling::CaseKept, "  kPEPE\t").as_deref(), Ok("kPEPE"));
    assert_eq!(
        canonical_symbol(Spelling::CaseKept, "btc").as_deref(),
        Ok("btc"),
        "no guessing the case: the listing decides whether `btc` is a coin"
    );
}

/// What is not a core perp's name is refused at the door with the usual words: a spot pair
/// (`BASE/QUOTE`, `@N`), a builder-dex coin (`dex:COIN`), a `.P` lane suffix, a separator and a
/// name too long or empty. None of these reaches a signed request.
#[test]
fn a_hyperliquid_door_refuses_what_is_not_a_core_perp_name() {
    let too_long = "A".repeat(33);
    for bad in [
        "HYPE/USDC",
        "@107",
        "test:ABC",
        "BTC.P",
        "BTC-PERP",
        "BTC USDC",
        "",
        "  ",
        too_long.as_str(),
    ] {
        let why = canonical_symbol(Spelling::CaseKept, bad).expect_err(bad);
        assert!(why.contains("is not an exchange symbol"), "{bad:?}: {why}");
    }
}

/// The two existing spellings are untouched by the third.
#[test]
fn the_other_spellings_still_upper_case() {
    assert_eq!(canonical_symbol(Spelling::LaneSuffixed, "ethusdt.p").as_deref(), Ok("ETHUSDT.P"));
    assert_eq!(canonical_symbol(Spelling::LaneSuffixed, "ethusdt").as_deref(), Ok("ETHUSDT"));
    assert_eq!(canonical_symbol(Spelling::BareLinear, "ethusdt.p").as_deref(), Ok("ETHUSDT"));
}
