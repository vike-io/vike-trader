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

/// What is not a name the venue lists is refused at the door with the usual words: the wire name `@N`
/// of a spot pair, a `.P` lane suffix, a separator, a half pair, two separators and a name too long or
/// empty. None of these reaches a signed request.
#[test]
fn a_hyperliquid_door_refuses_what_is_not_a_name_the_venue_lists() {
    let too_long = "A".repeat(33);
    let long_pair = format!("{}/USDC", "A".repeat(21));
    for bad in [
        "@107",
        "BTC.P",
        "BTC-PERP",
        "BTC USDC",
        "HYPE/",
        "/USDC",
        "HYPE/USDC/X",
        "test:",
        ":ABC",
        "a:b:c",
        "HYPE/USDC:X",
        "HY PE/USDC",
        "",
        "  ",
        too_long.as_str(),
        long_pair.as_str(),
    ] {
        let why = canonical_symbol(Spelling::CaseKept, bad).expect_err(bad);
        assert!(why.contains("is not an exchange symbol"), "{bad:?}: {why}");
    }
    let wire = canonical_symbol(Spelling::CaseKept, "@107").expect_err("the wire name");
    assert!(
        wire.contains("BASE/QUOTE") && wire.contains("HYPE/USDC"),
        "it says how to write it: {wire}"
    );
}

/// **A spot pair and a builder-dex coin are names the door spells** (decision 0218): `BASE/QUOTE` and
/// `dex:COIN`, case kept, each part letters and digits.
#[test]
fn a_hyperliquid_door_spells_a_spot_pair_and_a_builder_dex_coin() {
    for name in ["HYPE/USDC", "PURR/USDC", "kHYPE/USDC", "test:ABC", "xyz:TSLA", "xyz:1000FLOKI"] {
        assert_eq!(canonical_symbol(Spelling::CaseKept, name).as_deref(), Ok(name), "{name}");
    }
    assert_eq!(canonical_symbol(Spelling::CaseKept, " HYPE/USDC ").as_deref(), Ok("HYPE/USDC"));
    assert_eq!(
        canonical_symbol(Spelling::CaseKept, "hype/usdc").as_deref(),
        Ok("hype/usdc"),
        "no guessing the case: the listing decides"
    );
}

/// The two existing spellings are untouched by the third.
#[test]
fn the_other_spellings_still_upper_case() {
    assert_eq!(canonical_symbol(Spelling::LaneSuffixed, "ethusdt.p").as_deref(), Ok("ETHUSDT.P"));
    assert_eq!(canonical_symbol(Spelling::LaneSuffixed, "ethusdt").as_deref(), Ok("ETHUSDT"));
    assert_eq!(canonical_symbol(Spelling::BareLinear, "ethusdt.p").as_deref(), Ok("ETHUSDT"));
}
