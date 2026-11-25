//! The control door's reading of a Hyperliquid symbol (decisions 0174 and 0218, under decision 0290's user names, O-4
//! and O-5): a perpetual is its coin, the row's `USDC` and `.P` (`BTCUSDC.P`; `KPEPEUSDC.P` upper case, the kept row
//! keeping `kPEPE` for the wire), a spot pair its base and quote (`HYPEUSDC`), a builder-dex coin `XYZ:TSLAUSDC.P`.
//! The wire name `@107` is refused by its shape, and the old spellings (`BTC`, `kPEPE`, `HYPE/USDC`, `xyz:TSLA`) name
//! no row.

/// The door's reading of `typed` on hyperliquid over the suites' kept rows (`crate::testutil::door_rows`).
fn door(typed: &str) -> Result<String, String> {
    vike_catalog::door_user_symbol("hyperliquid", typed, &crate::testutil::door_rows())
}

#[test]
fn a_hyperliquid_name_is_its_user_name_in_any_case() {
    for (typed, user) in [
        ("BTCUSDC.P", "BTCUSDC.P"),
        ("ethusdc.p", "ETHUSDC.P"),
        ("  kpepeusdc.p\t", "KPEPEUSDC.P"),
        ("hypeusdc", "HYPEUSDC"),
        ("xyz:tslausdc.p", "XYZ:TSLAUSDC.P"),
    ] {
        assert_eq!(door(typed).as_deref(), Ok(user), "{typed:?}");
    }
}

/// What is no user name is refused at the door with words: the wire name `@N` of a spot pair by its shape (saying
/// how to write the pair), the old spellings for want of a row, and a name with a character no symbol has.
#[test]
fn a_hyperliquid_door_refuses_the_wire_name_and_the_old_spellings() {
    let wire = door("@107").expect_err("the wire name");
    assert!(
        wire.contains("wire name") && wire.contains("HYPEUSDC"),
        "it says how to write it: {wire}"
    );
    for old in ["BTC", "kPEPE", "HYPE/USDC", "xyz:TSLA", "BTC.P"] {
        let why = door(old).expect_err(old);
        assert!(why.contains("carries no row"), "{old}: {why}");
    }
    for bad in ["", "  ", "BTC USDC"] {
        let why = door(bad).expect_err(bad);
        assert!(why.contains("is not a symbol"), "{bad:?}: {why}");
    }
}
