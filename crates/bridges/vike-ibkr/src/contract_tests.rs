use super::*;
use std::assert_matches;

#[test]
fn parses_us_equity_simplified() {
    let c = parse_simplified("AAPL.SMART.USD").unwrap();
    assert_eq!(c.sec_type, SecType::Stk);
    assert_eq!(c.symbol, "AAPL");
    assert_eq!(c.exchange, "SMART");
    assert_eq!(c.currency, "USD");
}

/// The BYTE-IDENTICAL guard for the two canonicals this tree actually writes — the started
/// `crates/vike-venues/src/venue_wiring.rs`'s `VENUE_WIRING` ibkr row symbol and the forex spelling. Asserted on the
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
    assert_eq!(c.right, Some(OptionKind::Call));
    assert!(
        parse_simplified("SPY.SMART.USD.OPT.20251219.499.5.PUT.100").is_err(),
        "a '.' inside the strike splits the canonical — refused, not mis-read"
    );
    let q = parse_simplified("SPY.SMART.USD.OPT.20251219.500.PUT.100").unwrap();
    assert_eq!(q.right, Some(OptionKind::Put));
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
    // A stock's fifth field is its contract id (decision 0218); a sixth, or a fifth that is not an id, is not used.
    assert!(parse_simplified("AAPL.SMART.USD.STK.265598.20251219").is_err());
    assert!(parse_simplified("AAPL.SMART.USD.STK.DEC25").is_err());
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
    let f = contract_details_to_properties(&SecType::Stk, 0.01, 1.0, 1.0, 0.0);
    assert_eq!(f.tick_size, 0.01);
    assert_eq!(f.step_size, 1.0, "size_increment → step_size");
    assert_eq!(f.min_qty, 1.0, "min_size → min_qty");
    assert_eq!(f.contract_size, 0.0, "a share names no multiplier: the inert absent");
    // A futures-style tick with the size grid unreported (older TWS): 0.0 = unconstrained, which
    // `RiskLimits::from_properties` folds to inert — byte-identical to no grid.
    let f2 = contract_details_to_properties(&SecType::Fut, 0.25, 0.0, 0.0, 50.0);
    assert_eq!(f2.tick_size, 0.25);
    assert_eq!(f2.step_size, 0.0);
    assert_eq!(f2.min_qty, 0.0);
    assert_eq!(f2.contract_size, 50.0, "the resolved multiplier is the contract size");
}

/// Every secType an operator may claim answers a class, and the class travels on the grid
/// (`contract_details_to_properties`). Driven through
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
            contract_details_to_properties(&sec_type, 0.01, 1.0, 1.0, 0.0).asset_class,
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
/// ones — they pin an ARGUED absence (`NEWS`, `CONTFUT`), and the classes decision 0126 added
/// pin that `WAR` is a `Warrant` rather than an `Option` and `FUND` a `Fund` rather than an `Etf`.
#[test]
fn the_inbound_only_vocabulary_answers_one_decision_per_word() {
    for (code, expected) in [
        ("CFD", Some(AssetClass::Cfd)),
        ("FOP", Some(AssetClass::Option)),
        ("BOND", Some(AssetClass::Bond)),
        ("WAR", Some(AssetClass::Warrant)),
        ("CMDTY", Some(AssetClass::Commodity)),
        ("FUND", Some(AssetClass::Fund)),
        ("BAG", Some(AssetClass::Combo)),
        ("NEWS", None),
        ("CONTFUT", None),
        ("", None),
        ("A_WORD_IB_HAS_NOT_INVENTED_YET", None),
    ] {
        let sec_type = SecType::from_ib_code(code);
        assert_matches!(sec_type, SecType::Other(_), "{code} must stay inbound-only");
        assert_eq!(sec_type.asset_class(), expected, "{code}");
        assert_eq!(
            contract_details_to_properties(&sec_type, 0.01, 1.0, 1.0, 0.0).asset_class,
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

/// A contract with every field the typed mapping reads, for the secTypes no canonical can spell
/// (`Other` is inbound-only, so `parse_simplified` never builds one).
fn inbound(code: &str) -> IbkrContract {
    IbkrContract {
        sec_type: SecType::from_ib_code(code),
        symbol: "ES".into(),
        exchange: "CME".into(),
        currency: "USD".into(),
        expiry: Some("20251219".into()),
        strike: Some(6000.0),
        right: Some(OptionKind::Call),
        multiplier: Some("50".into()),
        con_id: None,
    }
}

/// 2025-12-19 00:00 UTC in epoch ms — the day a `20251219` expiry names.
const DEC_19_2025_MS: i64 = 1_766_102_400_000;

/// Every claimable spelling, through the real grammar, maps to the EXACT typed contract.
#[test]
fn every_claimable_canonical_maps_to_its_typed_contract() {
    let usd = || Some("USD".to_string());
    for (canonical, expected) in [
        ("AAPL.SMART.USD", Contract::Equity(EquitySpec { exchange: None, currency: usd() })),
        (
            "VOD.LSE.GBP.STK",
            Contract::Equity(EquitySpec {
                exchange: Some("LSE".into()),
                currency: Some("GBP".into()),
            }),
        ),
        (
            "EUR.USD.IDEALPRO",
            Contract::Fx(FxSpec {
                pip_size: None,
                lot_size: None,
                swap_long: None,
                swap_short: None,
            }),
        ),
        ("SPX.CBOE.USD.IND", Contract::Index),
        ("BTC.PAXOS.USD.CRYPTO", Contract::CryptoSpot),
        (
            "ES.GLOBEX.USD.FUT.20251219.50",
            Contract::Future(FutureSpec {
                margining: Margining::Linear,
                settle: "USD".into(),
                value_ccy: usd(),
                expiry_ms: DEC_19_2025_MS,
                tenor: None,
                delivery: Delivery::Unknown,
                family: FutureFamily::Traditional,
            }),
        ),
        (
            "SPY.SMART.USD.OPT.20251219.500.P",
            Contract::Option(OptionSpec {
                right: OptionKind::Put,
                strike: 500.0,
                expiry_ms: DEC_19_2025_MS,
                style: ExerciseStyle::Unknown,
                margining: Margining::Linear,
                settle: "USD".into(),
                value_ccy: usd(),
                underlying: Some("SPY".into()),
            }),
        ),
    ] {
        let c = parse_simplified(canonical).unwrap();
        assert_eq!(c.to_contract(), Some(expected), "{canonical}");
    }
}

/// The inbound words that map, with the fields an `Option` needs, and the ones that do not.
#[test]
fn every_inbound_word_maps_or_answers_none_for_its_written_reason() {
    assert_eq!(
        inbound("CFD").to_contract(),
        Some(Contract::Cfd(CfdSpec {
            underlying_kind: CfdUnderlying::Other,
            underlying: None,
            product: CfdProduct::Cfd,
            expiry: CfdExpiry::Unknown,
            margin_rate: None,
            swap_long: None,
            swap_short: None,
        }))
    );
    let Some(Contract::Option(fop)) = inbound("FOP").to_contract() else {
        panic!("FOP is an option on a future")
    };
    assert_eq!((fop.right, fop.strike, fop.expiry_ms), (OptionKind::Call, 6000.0, DEC_19_2025_MS));
    assert_eq!(fop.underlying.as_deref(), Some("ES"), "the future's root is the underlying");
    assert_eq!(inbound("BOND").to_contract(), Some(Contract::Bond));
    assert_eq!(inbound("WAR").to_contract(), Some(Contract::Warrant));
    assert_eq!(inbound("CMDTY").to_contract(), Some(Contract::Commodity));
    assert_eq!(inbound("FUND").to_contract(), Some(Contract::Fund));
    for code in ["BAG", "CONTFUT", "NEWS", "cfd", "A_WORD_IB_HAS_NOT_INVENTED_YET"] {
        assert_eq!(inbound(code).to_contract(), None, "{code}");
    }
}

/// `SecType::asset_class` is the projection of `to_contract`: for every word — claimable or
/// inbound — whose contract maps, the class is the contract's own.
#[test]
fn asset_class_is_the_class_of_the_typed_contract() {
    let words = CLAIMABLE_SEC_TYPES
        .iter()
        .copied()
        .chain(["CFD", "FOP", "BOND", "WAR", "CMDTY", "FUND", "BAG", "NEWS", "CONTFUT"]);
    let mut mapped = 0;
    for code in words {
        let c = inbound(code);
        if let Some(contract) = c.to_contract() {
            assert_eq!(Some(contract.class()), c.sec_type.asset_class(), "{code}");
            mapped += 1;
        }
    }
    // Every claimable word maps (given its fields), plus CFD/FOP/BOND/WAR/CMDTY/FUND.
    assert_eq!(mapped, CLAIMABLE_SEC_TYPES.len() + 6);
}

/// ⚠ A month-precision expiry is NOT given a day: the whole contract answers `None`. So do a
/// missing right/strike/expiry and a day the month does not have.
#[test]
fn a_contract_missing_a_defining_field_answers_none_rather_than_a_guess() {
    let month = parse_simplified("ES.GLOBEX.USD.FUT.202512").unwrap();
    assert_eq!(month.sec_type.asset_class(), Some(AssetClass::Future), "the class still answers");
    assert_eq!(month.to_contract(), None, "YYYYMM names a month, not a last trading day");
    let opt_month = parse_simplified("SPY.SMART.USD.OPT.202512.500.C").unwrap();
    assert_eq!(opt_month.to_contract(), None);
    for c in [
        IbkrContract { right: None, ..inbound("OPT") },
        IbkrContract { strike: None, ..inbound("OPT") },
        IbkrContract { strike: Some(0.0), ..inbound("OPT") },
        IbkrContract { expiry: None, ..inbound("OPT") },
        IbkrContract { expiry: None, ..inbound("FUT") },
        IbkrContract { expiry: Some("20250231".into()), ..inbound("FUT") },
        IbkrContract { expiry: Some("2025123".into()), ..inbound("FUT") },
    ] {
        assert_eq!(c.to_contract(), None, "{c:?}");
    }
    // A leap day is a real day.
    let leap = IbkrContract { expiry: Some("20240229".into()), ..inbound("FUT") };
    let Some(Contract::Future(f)) = leap.to_contract() else { panic!("2024-02-29 exists") };
    assert_eq!(f.expiry_ms, 1_709_164_800_000);
}

/// The multiplier: a claim wins, a unanimous reply answers, anything else is the inert `0.0`.
#[test]
fn the_multiplier_is_the_claim_or_a_unanimous_reply_never_the_first_row() {
    assert_eq!(resolve_multiplier(Some("50"), ["5", "25"]), 50.0, "the claim is what is sent");
    assert_eq!(resolve_multiplier(None, ["50", "50", "50"]), 50.0, "an E-mini ladder agrees");
    assert_eq!(resolve_multiplier(None, ["25", "5", "1"]), 0.0, "EUREX DAX: three sizes");
    assert_eq!(resolve_multiplier(None, [""]), 0.0, "a share names none");
    assert_eq!(resolve_multiplier(None, ["50", ""]), 0.0, "a silent row breaks unanimity");
    assert_eq!(resolve_multiplier(None, std::iter::empty()), 0.0);
    for bad in ["0", "-5", "NaN", "inf", "abc"] {
        assert_eq!(resolve_multiplier(Some(bad), [bad]), 0.0, "{bad}");
    }
    assert_eq!(resolve_multiplier(Some(""), ["100"]), 100.0, "an empty claim falls through");
}

/// **A stock can PIN its contract id** (decision 0218): `SYMBOL.EXCHANGE.CCY.STK.CONID` carries IB's `conId`, which
/// the order path already sends (`contract_id`), so a ticker listed on several exchanges names exactly one.
/// The unpinned claim `.STK` is the contract it always was.
#[test]
fn a_stock_claim_may_pin_its_contract_id() {
    let pinned = parse_simplified("AAPL.SMART.USD.STK.265598").unwrap();
    assert_eq!(pinned.sec_type, SecType::Stk);
    assert_eq!((pinned.symbol.as_str(), pinned.exchange.as_str()), ("AAPL", "SMART"));
    assert_eq!(pinned.con_id, Some(265_598));
    // Any exchange: the explicit claim already allowed it.
    let lse = parse_simplified("VOD.LSE.GBP.STK.12345").unwrap();
    assert_eq!(
        (lse.exchange.as_str(), lse.currency.as_str(), lse.con_id),
        ("LSE", "GBP", Some(12_345))
    );
    assert_eq!(parse_simplified("VOD.LSE.GBP.STK").unwrap().con_id, None, "unpinned is unchanged");
    // The pin is only for a stock: a forex pair or an index takes no trailing field, as before.
    for bad in [
        "EUR.IDEALPRO.USD.CASH.1",
        "SPX.CBOE.USD.IND.1",
        "AAPL.SMART.USD.STK.0",
        "AAPL.SMART.USD.STK.-1",
        "AAPL.SMART.USD.STK.1.2",
        "AAPL.SMART.USD.STK.12a",
        "AAPL.SMART.USD.STK.2147483648",
        "AAPL.SMART.USD.STK.+5",
    ] {
        assert!(parse_simplified(bad).is_err(), "{bad} must be refused");
    }
    let why = parse_simplified("AAPL.SMART.USD.STK.abc").unwrap_err();
    assert!(why.contains("contract id") && why.contains("abc"), "{why}");
}

/// **A class share is `BRK_B` in the engine and `BRK B` on the wire** (decision 0218): a dot cannot be written in
/// this grammar, so the engine spells IB's space with `_`, in the three-field form and the pinned one. A stock that
/// has no `_` is unchanged, and the pairing helpers read either side.
#[test]
fn a_class_share_is_spelled_with_an_underscore_and_sent_with_a_space() {
    assert_eq!(parse_simplified("BRK_B.SMART.USD").unwrap().symbol, "BRK B");
    let pinned = parse_simplified("BRK_B.SMART.USD.STK.72063691").unwrap();
    assert_eq!((pinned.symbol.as_str(), pinned.con_id), ("BRK B", Some(72_063_691)));
    assert_eq!(parse_simplified("AAPL.SMART.USD").unwrap().symbol, "AAPL", "no `_`, no change");
    assert_eq!(ib_symbol("BF_B"), "BF B");
    assert_eq!(ib_symbol("AAPL"), "AAPL");
    for (a, b) in [("BRK B", "BRK_B"), ("BRK.B", "brk-b"), ("AAPL", "aapl"), ("BRK B", "BRKB")] {
        assert!(same_ticker(a, b), "{a} / {b}");
    }
    for (a, b) in [("BRK B", "BRK A"), ("AAPL", "AAPLX"), ("", ""), ("._", "-")] {
        assert!(!same_ticker(a, b), "{a} / {b}");
    }
}
