use super::*;

#[test]
fn parses_us_equity_simplified() {
    let c = parse_simplified("AAPL.SMART.USD").unwrap();
    assert_eq!(c.sec_type, SecType::Stk);
    assert_eq!(c.symbol, "AAPL");
    assert_eq!(c.exchange, "SMART");
    assert_eq!(c.currency, "USD");
}

/// The BYTE-IDENTICAL guard for the two canonicals this tree actually writes — the mounted
/// `crates/vike-run/src/node.rs` `IBKR_MARKET` symbol and the forex spelling. Asserted on the
/// WHOLE struct, field by field, so a refusal or a changed default reddens here first.
#[test]
fn the_two_canonicals_this_tree_writes_are_unchanged() {
    assert_eq!(
        parse_simplified("AAPL.SMART.USD").unwrap(),
        IbkrContract {
            sec_type: SecType::Stk,
            symbol: "AAPL".into(),
            exchange: "SMART".into(),
            currency: "USD".into(),
            expiry: None,
            strike: None,
            right: None,
            multiplier: None,
            con_id: None,
        }
    );
    assert_eq!(
        parse_simplified("EUR.USD.IDEALPRO").unwrap(),
        IbkrContract {
            sec_type: SecType::Cash,
            symbol: "EUR".into(),
            exchange: "IDEALPRO".into(),
            currency: "USD".into(),
            expiry: None,
            strike: None,
            right: None,
            multiplier: None,
            con_id: None,
        }
    );
}

#[test]
fn forex_pair_maps_to_cash_on_idealpro() {
    let c = parse_simplified("EUR.USD.IDEALPRO").unwrap();
    assert_eq!(c.sec_type, SecType::Cash);
    assert_eq!(c.symbol, "EUR");
    assert_eq!(c.currency, "USD");
    assert_eq!(c.exchange, "IDEALPRO");
}

/// The trap this crate's `CLAUDE.md` documents: the spelling that OBEYS the stated field order
/// used to produce an EQUITY named EUR. Both orders now name the one contract.
#[test]
fn the_stated_forex_field_order_names_the_same_contract_as_the_legacy_one() {
    assert_eq!(
        parse_simplified("EUR.IDEALPRO.USD").unwrap(),
        parse_simplified("EUR.USD.IDEALPRO").unwrap()
    );
}

/// ⚠ THE DEFECT, pinned. `ESZ5.GLOBEX.USD` used to return `SecType::Stk` and be SUBMITTED as a
/// stock. It is now refused, and the refusal names the field it could not read and the
/// spellings that would work.
#[test]
fn a_futures_canonical_is_refused_rather_than_defaulted_to_equity() {
    let err = parse_simplified("ESZ5.GLOBEX.USD").unwrap_err();
    assert!(err.contains("ESZ5.GLOBEX.USD"), "names the canonical it rejected: {err}");
    assert!(err.contains("GLOBEX"), "names the field it could not read: {err}");
    assert!(err.contains("FUT.YYYYMMDD"), "names an accepted spelling: {err}");
    assert!(err.contains("STK"), "names the accepted secType claims: {err}");
}

/// The other half of the same bug: an unknown exchange is refused too, so a derivatives venue
/// missing from `EQUITY_EXCHANGES` cannot fall through to equity either.
#[test]
fn an_unknown_exchange_is_refused_in_the_three_field_form() {
    for s in ["CL.NYMEX.USD", "6E.CME.USD", "FDAX.EUREX.EUR", "VX.CFE.USD"] {
        let err = parse_simplified(s).unwrap_err();
        assert!(err.contains("not a known cash-equity exchange"), "{s}: {err}");
    }
}

/// The operator's remedy, asserted on the CONSTRUCTED CONTRACT rather than on any string:
/// secType FUT, the expiry carried verbatim into IBKR's `lastTradeDateOrContractMonth`, and
/// the multiplier when one is given.
#[test]
fn a_claimed_future_reaches_the_contract_as_a_future() {
    let c = parse_simplified("ES.GLOBEX.USD.FUT.20251219").unwrap();
    assert_eq!(c.sec_type, SecType::Fut);
    assert_eq!(c.sec_type.as_ib_code(), "FUT");
    assert_eq!(c.symbol, "ES");
    assert_eq!(c.exchange, "GLOBEX");
    assert_eq!(c.currency, "USD");
    assert_eq!(c.expiry.as_deref(), Some("20251219"));
    assert_eq!(c.multiplier, None);
    assert_eq!(c.strike, None);

    let m = parse_simplified("ES.GLOBEX.USD.FUT.202512.50").unwrap();
    assert_eq!(m.expiry.as_deref(), Some("202512"), "month precision is IBKR-legal");
    assert_eq!(m.multiplier.as_deref(), Some("50"));
}

#[test]
fn a_future_without_an_expiry_is_refused() {
    let err = parse_simplified("ES.GLOBEX.USD.FUT").unwrap_err();
    assert!(err.contains("FUT.YYYYMMDD"), "{err}");
    assert!(err.contains("ambiguous across decades"), "argues the month code: {err}");
}

/// ⚠ The 2035 question, asserted rather than promised: the exchange month code is REFUSED in
/// the expiry field, in every form, because `Z5` names December 2025, 2035 and 2015 alike.
#[test]
fn an_exchange_month_code_is_never_decoded_into_an_expiry() {
    for bad in ["Z5", "Z25", "DEC25", "2025", "20251", "202513", "20251232", "2025121X"] {
        let s = format!("ES.GLOBEX.USD.FUT.{bad}");
        let err = parse_simplified(&s).unwrap_err();
        assert!(err.contains("YYYYMM"), "{s}: {err}");
    }
}

#[test]
fn a_claimed_option_carries_expiry_strike_and_right() {
    let c = parse_simplified("SPY.SMART.USD.OPT.20251219.500.C").unwrap();
    assert_eq!(c.sec_type, SecType::Opt);
    assert_eq!(c.expiry.as_deref(), Some("20251219"));
    assert_eq!(c.strike, Some(500.0));
    assert_eq!(c.right, Some('C'));
    assert!(
        parse_simplified("SPY.SMART.USD.OPT.20251219.499.5.PUT.100").is_err(),
        "a '.' inside the strike splits the canonical — refused, not mis-read"
    );
    let q = parse_simplified("SPY.SMART.USD.OPT.20251219.500.PUT.100").unwrap();
    assert_eq!(q.right, Some('P'));
    assert_eq!(q.multiplier.as_deref(), Some("100"));
}

#[test]
fn an_incomplete_option_is_refused() {
    for s in [
        "SPY.SMART.USD.OPT",
        "SPY.SMART.USD.OPT.20251219",
        "SPY.SMART.USD.OPT.20251219.500",
        "SPY.SMART.USD.OPT.20251219.500.X",
        "SPY.SMART.USD.OPT.20251219.0.C",
    ] {
        assert!(parse_simplified(s).is_err(), "{s} parsed");
    }
}

/// An explicit claim works on ANY exchange — that is what makes the allowlist's refusal a
/// remedy rather than a wall.
#[test]
fn an_explicit_equity_claim_needs_no_allowlisted_exchange() {
    assert_eq!(parse_simplified("VOD.LSE.GBP.STK").unwrap().sec_type, SecType::Stk);
    let d = parse_simplified("SOMETHING.A_VENUE_NOBODY_LISTED.SEK.STK").unwrap();
    assert_eq!(d.sec_type, SecType::Stk);
    assert_eq!(d.exchange, "A_VENUE_NOBODY_LISTED");
}

#[test]
fn an_unrecognised_security_type_claim_is_refused() {
    // `SecType::from_ib_code` would answer `Other("BAG")` and the wire would MEAN it.
    let err = parse_simplified("X.SMART.USD.BAG").unwrap_err();
    assert!(err.contains("not a security type this venue accepts"), "{err}");
    assert!(err.contains("CRYPTO"), "lists what is accepted: {err}");
}

#[test]
fn a_trailing_field_a_claim_does_not_use_is_refused_not_ignored() {
    assert!(parse_simplified("AAPL.SMART.USD.STK.20251219").is_err());
    assert!(parse_simplified("BTC.PAXOS.USD.CRYPTO.1").is_err());
}

#[test]
fn the_remaining_claims_parse() {
    assert_eq!(parse_simplified("BTC.PAXOS.USD.CRYPTO").unwrap().sec_type, SecType::Crypto);
    assert_eq!(parse_simplified("SPX.CBOE.USD.IND").unwrap().sec_type, SecType::Ind);
    let fx = parse_simplified("EUR.IDEALPRO.USD.CASH").unwrap();
    assert_eq!(fx.sec_type, SecType::Cash);
    assert_eq!(fx.currency, "USD");
}

#[test]
fn a_malformed_canonical_is_refused() {
    for s in ["", "AAPL", "AAPL.SMART", "AAPL..USD", ".SMART.USD", "AAPL.SMART."] {
        assert!(parse_simplified(s).is_err(), "{s:?} parsed");
    }
}

#[test]
fn a_non_currency_third_field_is_refused() {
    // The transposition that used to sail through as an equity priced in "NASDAQ".
    let err = parse_simplified("AAPL.SMART.NASDAQ").unwrap_err();
    assert!(err.contains("three-letter currency code"), "{err}");
}

/// The ambiguity flag the cpapi conId lanes gate on: an expiry-bearing contract cannot be
/// resolved by a symbol+secType search, an equity can.
#[test]
fn only_an_expiry_bearing_contract_is_conid_search_ambiguous() {
    assert!(!parse_simplified("AAPL.SMART.USD").unwrap().conid_search_is_ambiguous());
    assert!(!parse_simplified("EUR.USD.IDEALPRO").unwrap().conid_search_is_ambiguous());
    assert!(parse_simplified("ES.GLOBEX.USD.FUT.20251219").unwrap().conid_search_is_ambiguous());
    assert!(
        parse_simplified("SPY.SMART.USD.OPT.20251219.500.C").unwrap().conid_search_is_ambiguous()
    );
}

#[test]
fn sec_type_code_roundtrip() {
    for (st, code) in [
        (SecType::Stk, "STK"),
        (SecType::Opt, "OPT"),
        (SecType::Fut, "FUT"),
        (SecType::Cash, "CASH"),
        (SecType::Ind, "IND"),
        (SecType::Crypto, "CRYPTO"),
    ] {
        assert_eq!(st.as_ib_code(), code);
        assert_eq!(SecType::from_ib_code(code), st);
    }
    assert_eq!(SecType::from_ib_code("BAG"), SecType::Other("BAG".into()));
}

/// Every code an operator may claim is a code `SecType::from_ib_code` actually knows — so the
/// claim table cannot drift into admitting a word that resolves to `Other` and reaches the
/// wire as whatever IBKR makes of it.
#[test]
fn every_claimable_sec_type_resolves_to_a_named_variant() {
    for code in CLAIMABLE_SEC_TYPES {
        assert!(
            !matches!(SecType::from_ib_code(code), SecType::Other(_)),
            "{code} is claimable but resolves to Other"
        );
    }
}

/// The allowlist is the one table that can reinstate the silent equity default, so its rows are
/// held to the shape a dot-delimited grammar can express and kept clear of derivatives venues.
#[test]
fn the_equity_exchange_allowlist_is_well_formed() {
    for e in EQUITY_EXCHANGES {
        assert!(!e.contains('.'), "{e} cannot be written in a dot-delimited canonical");
        assert!(!e.is_empty());
        assert_eq!(*e, e.to_ascii_uppercase(), "{e} must be spelled as IBKR spells it");
    }
    assert!(
        !EQUITY_EXCHANGES.iter().any(|e| e.eq_ignore_ascii_case(FOREX_EXCHANGE)),
        "IDEALPRO is a forex venue and must never be read as a cash-equity exchange"
    );
    for derivative in ["GLOBEX", "CME", "NYMEX", "COMEX", "CBOT", "ECBOT", "EUREX", "CFE"] {
        assert!(
            !EQUITY_EXCHANGES.iter().any(|e| e.eq_ignore_ascii_case(derivative)),
            "{derivative} is a derivatives venue — admitting it reinstates the equity default"
        );
    }
}

#[test]
fn conid_map_is_bidirectional() {
    let mut m = ConIdMap::default();
    m.insert(265598, "AAPL.SMART.USD");
    assert_eq!(m.symbol_of(265598), Some("AAPL.SMART.USD"));
    assert_eq!(m.con_id_of("AAPL.SMART.USD"), Some(265598));
}

#[test]
fn contract_details_map_to_properties() {
    // A US equity: penny tick, whole-share size grid.
    let f = contract_details_to_properties(&SecType::Stk, 0.01, 1.0, 1.0);
    assert_eq!(f.tick_size, 0.01);
    assert_eq!(f.step_size, 1.0, "size_increment → step_size");
    assert_eq!(f.min_qty, 1.0, "min_size → min_qty");
    assert_eq!(f.contract_size, 0.0, "multiplier deferred (deribit-path follow-up)");
    // A futures-style tick with the size grid unreported (older TWS): 0.0 = unconstrained, which
    // `RiskLimits::from_properties` folds to inert — byte-identical to no grid.
    let f2 = contract_details_to_properties(&SecType::Fut, 0.25, 0.0, 0.0);
    assert_eq!(f2.tick_size, 0.25);
    assert_eq!(f2.step_size, 0.0);
    assert_eq!(f2.min_qty, 0.0);
}

/// Every secType an operator may claim answers a class, and the class travels on the grid
/// (`docs/decisions/0061-an-instrument-names-its-kind.md`). Driven through
/// [`SecType::from_ib_code`] over [`CLAIMABLE_SEC_TYPES`] so a seventh claimable code cannot
/// join without an author deciding what it IS — the list is the venue's vocabulary, and this
/// is the test that makes it a form to fill in rather than a silent `None`.
#[test]
fn every_claimable_sec_type_names_a_class() {
    for code in CLAIMABLE_SEC_TYPES {
        let sec_type = SecType::from_ib_code(code);
        let class = sec_type.asset_class();
        assert!(class.is_some(), "{code} is claimable but names no asset class");
        assert_eq!(
            contract_details_to_properties(&sec_type, 0.01, 1.0, 1.0).asset_class,
            class,
            "{code}: the grid must carry the same class the secType names"
        );
    }
    // Spot-check the two that a symbol-text reading would get wrong in opposite directions:
    // `ESZ5` and `AAPL` are both bare uppercase tickers.
    assert_eq!(SecType::from_ib_code("FUT").asset_class(), Some(AssetClass::Future));
    assert_eq!(SecType::from_ib_code("STK").asset_class(), Some(AssetClass::Equity));
    // IB's `CASH` is a currency pair, not cash equities.
    assert_eq!(SecType::from_ib_code("CASH").asset_class(), Some(AssetClass::Fx));
}

/// ⚠ The INBOUND-only variant is no longer ONE answer. IBKR's remaining secType vocabulary is
/// a table with a written decision per word (`SecType::other_asset_class`), and this test is
/// that table as a form: the rows are IB's own wire codes, and a row that ever changes its
/// mind has to change it here too. The `None` rows are the deliverable as much as the `Some`
/// ones — they pin an ARGUED absence, so a later reader cannot mistake them for an oversight
/// and fold `WAR` onto `Option` or `FUND` onto `Etf`.
#[test]
fn the_inbound_only_vocabulary_answers_one_decision_per_word() {
    for (code, expected) in [
        ("CFD", Some(AssetClass::Cfd)),
        ("FOP", Some(AssetClass::Option)),
        ("BOND", None),
        ("WAR", None),
        ("CMDTY", None),
        ("FUND", None),
        ("BAG", None),
        ("NEWS", None),
        ("CONTFUT", None),
        ("", None),
        ("A_WORD_IB_HAS_NOT_INVENTED_YET", None),
    ] {
        let sec_type = SecType::from_ib_code(code);
        assert!(matches!(sec_type, SecType::Other(_)), "{code} must stay inbound-only");
        assert_eq!(sec_type.asset_class(), expected, "{code}");
        assert_eq!(
            contract_details_to_properties(&sec_type, 0.01, 1.0, 1.0).asset_class,
            expected,
            "{code}: the grid must carry the same class the secType names"
        );
    }
}

/// ⚠ The catch-all table is matched on the word as IBKR SPELLS it. A lowercase code is a
/// caller bug, not a spelling to forgive: `from_ib_code`'s own named arms are already
/// case-sensitive (`stk` lands in `Other`), so forgiving case in the catch-all alone would
/// make one type answer two ways about one defect.
#[test]
fn the_inbound_table_is_case_sensitive_like_every_other_arm() {
    for spelling in ["cfd", "Cfd", "fop", "Fop"] {
        assert_eq!(SecType::from_ib_code(spelling).asset_class(), None, "{spelling}");
    }
    // ...which is exactly what the NAMED arms already do, and the reason this one matches them.
    assert_eq!(SecType::from_ib_code("stk"), SecType::Other("stk".into()));
    assert_eq!(SecType::from_ib_code("stk").asset_class(), None);
}

/// Deciding a class for an inbound word does NOT widen what an operator may claim. The order
/// path is `CLAIMABLE_SEC_TYPES` and it is untouched: `CFD` naming a class must not put `CFD`
/// on the wire, because reading a word IBKR said is a different act from forwarding one.
#[test]
fn a_classified_inbound_code_is_still_not_claimable() {
    for code in ["CFD", "FOP"] {
        assert!(!CLAIMABLE_SEC_TYPES.contains(&code), "{code} joined the claim table");
        assert!(
            parse_simplified(&format!("X.SMART.USD.{code}")).is_err(),
            "{code} became claimable"
        );
    }
}
