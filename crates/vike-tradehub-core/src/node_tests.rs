//! `node`'s unit tests: account refusal by name, engine rank order, and which legs reach a mount.
//!
//! The keep half of the crate is bridge-free (wave 2, W9a), so the arming table these tests judge a
//! mount against comes from the backend that dials nothing, `VenueBackendPaper`: every `demo`/`live`
//! account is NOT RUNNING on it, and a `paper` account runs the simulator — which is all the refusal
//! reads. The same refusal over a table with armed rows is the venue half's
//! (`crates/vike-gatewayhub/src/trade/adapters/contract_tests/arming.rs`).

/// The arming snapshot a config's backend answers.
fn table_of(config: &super::RuntimeConfig) -> crate::ArmingTable {
    config.backend.arming_table().expect("the paper backend answers")
}

/// **THE LOUD REFUSAL fires, and it fires on the arming table the fan-out selects with.**
///
/// [`super::refuse_unarmed_mount_accounts`] is this crate's one refusal that does not degrade. A
/// mutation replacing its body with `Ok(())` (`account = "ALT"` accepted, no engine for it,
/// `vike_core` resolving the mount onto the DEFAULT account) was measured GREEN across
/// `vike-run` and `vike-tradehub`: cited in four doc comments, covered by nothing.
///
/// Three cases, each a different wrong answer:
///
/// * an account NO account row names is refused, naming venue, account, symbol and the commands
///   that would arm it (a refusal an operator cannot act on is a crash);
/// * an account named but resolving PAPER (no `__ALT` credentials) is refused too — the case
///   that matters most: `venue_account_arming` returns a row for it, so an "is there a row" check
///   would pass it to the default engine;
/// * an account-LESS mount (every shipped mount) names the venue's DEFAULT account, which since
///   decision 0156 runs only at its row's tier: with no row it is refused like any other, never
///   handed a paper engine in its place; a `paper` row runs the simulator and passes.
#[test]
fn a_mount_naming_an_unarmed_account_is_refused_by_name() {
    let alt =
        vike_model::accounts::account_keys::AccountLabel::parse("ALT").expect("a legal label");
    let config = |policy: crate::AdapterPolicy| super::RuntimeConfig {
        backend: std::sync::Arc::new(crate::VenueBackendPaper::new(&policy.accounts)),
        properties_rec: None,
        seed_cash: 10_000.0,
        reconcile_enabled: false,
        core_config: vike_core::CoreConfig::default(),
        risk_profile: None,
        policy,
    };
    let mount = |account: Option<vike_model::accounts::account_keys::AccountLabel>| {
        vec![super::MountRoute {
            venue: "hyperliquid".to_string(),
            symbol: "BTC".to_string(),
            account,
        }]
    };

    // (1) an account no account row names.
    let bare = config(crate::AdapterPolicy::default());
    let err =
        super::refuse_unarmed_mount_accounts(&bare, &table_of(&bare), &mount(Some(alt.clone())))
            .expect_err("an unarmed account must REFUSE, never fall through to the default one");
    let said = err.to_string();
    for needle in ["hyperliquid", "ALT", "BTC", "vike-cli account activate"] {
        assert!(said.contains(needle), "the refusal must name `{needle}`: {said}");
    }

    // (2) …and one an ACTIVE row DOES name, resolved paper (no credentials): the check is on ARMED.
    let default = vike_model::accounts::account_keys::AccountLabel::Default;
    let named = config(
        crate::AdapterPolicy::default()
            .with_account("hyperliquid", &default, vike_model::AccountTier::Demo)
            .with_account("hyperliquid", &alt, vike_model::AccountTier::Demo),
    );
    assert!(
        super::refuse_unarmed_mount_accounts(&named, &table_of(&named), &mount(Some(alt.clone())))
            .is_err(),
        "an account row arms nothing on its own — with no `__ALT` credentials the account \
             resolves PAPER, has no engine, and the mount must still be refused"
    );

    // (3) an account-LESS mount names the DEFAULT account: no row, so it does not run, and the
    // mount is refused naming why — never resolved onto a paper engine standing in for it.
    let err = super::refuse_unarmed_mount_accounts(&bare, &table_of(&bare), &mount(None))
        .expect_err("a default account no row names does not run");
    let said = err.to_string();
    for needle in ["hyperliquid", "BTC", vike_model::ArmingBlock::NoAccountRow.as_str()] {
        assert!(said.contains(needle), "the refusal must name `{needle}`: {said}");
    }
    // …and with a `paper` row it runs the simulator, so the mount passes.
    let paper_default = config(crate::AdapterPolicy::default().with_account(
        "hyperliquid",
        &default,
        vike_model::AccountTier::Paper,
    ));
    assert!(
        super::refuse_unarmed_mount_accounts(
            &paper_default,
            &table_of(&paper_default),
            &mount(None)
        )
        .is_ok(),
        "a `paper` default account runs, and its mount resolves onto its engine"
    );

    // (4) a venue of LABELLED accounts only (decision 0172: the table lists no default row for it):
    // a mount naming the default account, or none, is still refused — now by "no account row names
    // it" — and a mount naming the labelled account that runs still passes.
    let labelled_only = config(crate::AdapterPolicy::default().with_account(
        "hyperliquid",
        &alt,
        vike_model::AccountTier::Paper,
    ));
    for account in [None, Some(default.clone())] {
        let err = super::refuse_unarmed_mount_accounts(
            &labelled_only,
            &table_of(&labelled_only),
            &mount(account),
        )
        .expect_err("the default account has no row on a venue of labelled accounts");
        let said = err.to_string();
        assert!(said.contains("no account row names it"), "{said}");
        assert!(said.contains("DEFAULT account"), "{said}");
    }
    assert!(
        super::refuse_unarmed_mount_accounts(
            &labelled_only,
            &table_of(&labelled_only),
            &mount(Some(alt))
        )
        .is_ok(),
        "the labelled account runs, so its mount passes"
    );
}

/// [`super::in_engine_order`]'s rule: default engines held by ascending `engine_rank`. A tie
/// keeps MOUNT order (stable sort), though a debug `assemble_runtime` panics on one first (its
/// `debug_assert!`; the table's ranks are pinned distinct in
/// `crates/vike-venues/src/venue_wiring_tests.rs`'s
/// `the_core_holds_the_default_engines_in_rank_order`).
#[test]
fn the_default_engines_are_held_in_rank_order() {
    let row = |venue, engine_rank| super::RuntimeMarket { venue, engine_rank };
    let mounted =
        vec![(row("a", 2), 'a'), (row("b", 0), 'b'), (row("c", 1), 'c'), (row("d", 1), 'd')];
    let got: Vec<char> = super::in_engine_order(mounted).into_iter().map(|(_, t)| t).collect();
    assert_eq!(got, ['b', 'c', 'd', 'a']);
}

/// [`ArmingTable::wired_markets`] keeps the wiring table's engine rows whose venue the table says
/// has an adapter, in table order, and drops a venue whose adapter is absent and a venue the table
/// does not name; the registry-less [`super::wired_markets`] is the whole wiring table.
#[test]
fn the_table_wires_the_markets_whose_adapter_it_carries() {
    let present = vike_model::VenueAdapterDeclarationView {
        adapter_present: true,
        ..vike_model::VenueAdapterDeclarationView::absent()
    };
    let facts = std::collections::BTreeMap::from([
        ("hyperliquid".to_string(), present),
        ("ibkr".to_string(), vike_model::VenueAdapterDeclarationView::absent()),
    ]);
    let table = crate::ArmingTable::new(facts, Vec::new());
    let got: Vec<&str> = table.wired_markets().iter().map(|m| m.venue).collect();
    assert_eq!(got, ["hyperliquid"]);
    assert!(
        crate::ArmingTable::default().wired_markets().is_empty(),
        "no table row, nothing mounted"
    );
    assert!(
        super::wired_markets().iter().any(|m| m.venue == "hyperliquid"),
        "the registry-less list is the whole wiring table"
    );
}

// ---- the declared-leg derivation feeding `crate::make_engine_with_legs` ----

use vike_core::MountLeg;

/// THE BYTE-IDENTICAL PROPERTY at the derivation end: a leg-free mount (every mount today)
/// contributes nothing to ANY venue's engine, so `make_engine_with_legs` gets an empty list.
///
/// ⚠ NON-VACUOUS only because of the SECOND mount: alone, it holds under any implementation,
/// even one ignoring `venue` and the default account's symbol. Beside a mount that DOES declare legs, a leak
/// onto a leg-free venue fails the first assertion; the second pins it is not merely misfiled.
#[test]
fn a_leg_free_mount_gets_nothing_even_beside_a_mount_that_declares_legs() {
    let none: Vec<MountLeg> = Vec::new();
    let some = vec![MountLeg::same_venue("ETHUSDT")];
    let mounts = || [("binance", none.as_slice()), ("bybit", some.as_slice())].into_iter();
    assert!(
        super::legs_for_venue(mounts(), "binance", Some("BTCUSDT")).is_empty(),
        "binance declares no legs, so bybit's must not reach it"
    );
    assert_eq!(
        super::legs_for_venue(mounts(), "bybit", Some("SOLUSDT")),
        vec!["ETHUSDT".to_string()],
        "...while the mount that DOES declare one still gets it"
    );
}

/// A SAME-VENUE leg (`MountLeg::same_venue`) belongs to the MOUNT's venue only.
///
/// NON-VACUOUS: the OTHER venue is asserted empty, so handing every leg to every engine
/// (silently gridding a bybit engine with a binance symbol's tick) fails the second assertion.
#[test]
fn a_same_venue_leg_reaches_only_its_own_mounts_venue() {
    let legs = vec![MountLeg::same_venue("ETHUSDT")];
    let mounts = || [("binance", legs.as_slice())].into_iter();
    assert_eq!(
        super::legs_for_venue(mounts(), "binance", Some("BTCUSDT")),
        vec!["ETHUSDT".to_string()]
    );
    assert!(super::legs_for_venue(mounts(), "bybit", Some("BTCUSDT")).is_empty());
}

/// A CROSS-VENUE leg (`MountLeg::at`, the xEMM hedge) is gridded on the venue it is SENT to —
/// the resolution `resolve_intent_venue` applies when routing its orders.
///
/// NON-VACUOUS: the mount's own venue is asserted EMPTY, so using `mount.venue` unconditionally
/// fails (the dangerous direction: gridding the maker for a symbol only the taker sees).
#[test]
fn a_cross_venue_leg_is_gridded_on_the_venue_it_routes_to() {
    let legs = vec![MountLeg::at("BTC-USDT-SWAP", "okx")];
    let mounts = || [("hyperliquid", legs.as_slice())].into_iter();
    assert_eq!(
        super::legs_for_venue(mounts(), "okx", Some("ETH-USDT-SWAP")),
        vec!["BTC-USDT-SWAP".to_string()]
    );
    assert!(super::legs_for_venue(mounts(), "hyperliquid", Some("BTC")).is_empty());
}

/// The symbol the venue's DEFAULT account is mounted on never becomes a leg (the engine's scalars are its
/// grid; a second row could only drift). The live xEMM shape: `crate::validate_xemm_mount` refuses a hedge
/// symbol the taker's default account is not mounted on, so its one leg lands here and is dropped. With no
/// default symbol (that account states none, and does not run) nothing is excluded.
#[test]
fn the_default_accounts_own_symbol_is_never_a_leg() {
    let legs = vec![MountLeg::at("BTC-USDT-SWAP", "okx")];
    let mounts = || [("hyperliquid", legs.as_slice())].into_iter();
    assert!(super::legs_for_venue(mounts(), "okx", Some("BTC-USDT-SWAP")).is_empty());
    assert_eq!(super::legs_for_venue(mounts(), "okx", None), vec!["BTC-USDT-SWAP".to_string()]);
}

/// The SAME symbol from two mounts on one venue is ONE entry (a duplicate grid lookup can be a
/// wasted round trip), in declaration order (`grid_by_symbol` is an `IndexMap` whose order
/// reaches the serialized limits); a blank symbol is none.
#[test]
fn repeats_collapse_and_blanks_are_dropped_in_declaration_order() {
    let a = vec![MountLeg::same_venue("ETHUSDT"), MountLeg::same_venue("   ")];
    let b = vec![MountLeg::same_venue("SOLUSDT"), MountLeg::same_venue("ETHUSDT")];
    let mounts = [("binance", a.as_slice()), ("binance", b.as_slice())];
    assert_eq!(
        super::legs_for_venue(mounts.into_iter(), "binance", Some("BTCUSDT")),
        vec!["ETHUSDT".to_string(), "SOLUSDT".to_string()]
    );
}

/// The rows of a binance demo default account (id 3) that states `BTCUSDT` and a leverage, and states `twin` as its
/// lane twin's symbol (`lane_twin_symbol`), plus (when `real_perp`) an operator's own account spelling the twin's
/// label and stating no symbol, read the way a root reads them: a derived twin is MARKED in the directory
/// (`AccountDirectory::is_derived_lane_twin`).
fn binance_rows_twin(
    leverage: Option<f64>,
    real_perp: bool,
    twin: Option<&str>,
) -> vike_secrets::account_directory::AccountDirectory {
    let row = |id, label: Option<&str>, leverage, symbol: Option<&str>| vike_secrets::AccountRow {
        id,
        venue: "binance".to_string(),
        tier: vike_model::AccountTier::Demo,
        label: label.map(str::to_string),
        venue_account_id: None,
        parent_id: None,
        active: true,
        last_verified_at: None,
        max_exposure: None,
        leverage,
        symbol: symbol.map(str::to_string),
        facts: Default::default(),
    };
    let mut parent =
        row(3, None, leverage.and_then(vike_secrets::AccountLeverage::new), Some("BTCUSDT"));
    if let Some(twin) = twin {
        parent.facts.insert(
            vike_secrets::AccountFact::LaneTwinSymbol.as_str().to_string(),
            twin.to_string(),
        );
    }
    let mut rows = vec![parent];
    if real_perp {
        rows.push(row(9, Some("PERP"), None, None));
    }
    vike_secrets::account_directory::AccountDirectory::read(
        Ok::<_, String>(vike_secrets::AccountRows::Known(rows)),
        Ok::<_, String>(None),
        Ok::<_, String>(vike_secrets::account_credentials::CredentialRegistry::new()),
    )
}

/// [`binance_rows_twin`] with the parent stating `BTCUSDT.P` as its twin's symbol.
fn binance_rows(
    leverage: Option<f64>,
    real_perp: bool,
) -> vike_secrets::account_directory::AccountDirectory {
    binance_rows_twin(leverage, real_perp, Some("BTCUSDT.P"))
}

/// **A DERIVED lane twin mounts on the symbol its row states (its parent's `lane_twin_symbol` fact), never on the
/// default engine's**, and the default account on the symbol ITS row states. A venue with no second lane (and no
/// row) lists nothing, and a mount that already names the twin's label keeps it. NOTHING falls back to another
/// account's symbol (owner, 2026-11-13): a directory that derived nothing, a parent that states no twin symbol, an
/// operator's own `PERP` account that states none, or an unread directory leave the twin's label with no entry
/// (`symbol_for_account` answers `""`, and such an account does not run: `NoAccountSymbol`).
#[test]
fn a_derived_lane_twin_mounts_on_its_lanes_symbol_and_a_real_perp_account_does_not() {
    use vike_model::accounts::account_keys::AccountLabel;
    let perp = AccountLabel::parse("PERP").expect("a legal label");
    let spot = (AccountLabel::Default, "BTCUSDT".to_string());
    let derived = binance_rows(Some(5.0), false);
    let account_symbols = super::account_symbols_for(&[], "binance", &derived);
    assert_eq!(account_symbols, vec![spot.clone(), (perp.clone(), "BTCUSDT.P".into())]);
    assert_eq!(crate::symbol_for_account(&account_symbols, &AccountLabel::Default), "BTCUSDT");
    assert_eq!(crate::symbol_for_account(&account_symbols, &perp), "BTCUSDT.P");
    // Another venue: no row of it, so nothing at all.
    assert!(super::account_symbols_for(&[], "bybit", &derived).is_empty());
    // Nothing derived (no leverage stated), a parent stating no twin symbol, or a REAL account under the label
    // that states none: the default account alone, and NO symbol for the label.
    for rows in [
        binance_rows(None, false),
        binance_rows_twin(Some(5.0), false, None),
        binance_rows(Some(5.0), true),
    ] {
        let account_symbols = super::account_symbols_for(&[], "binance", &rows);
        assert_eq!(account_symbols, vec![spot.clone()]);
        assert_eq!(crate::symbol_for_account(&account_symbols, &perp), "");
    }
    // An unread directory states nothing: no entry at all.
    let unread = vike_secrets::account_directory::AccountDirectory::unread();
    assert!(super::account_symbols_for(&[], "binance", &unread).is_empty());
    // An operator's own mount of that label is NOT overridden by the row.
    let mount = super::MountRoute {
        venue: "binance".to_string(),
        symbol: "ETHUSDT".to_string(),
        account: Some(perp.clone()),
    };
    assert_eq!(
        super::account_symbols_for(&[mount], "binance", &derived),
        vec![spot, (perp, "ETHUSDT".to_string())]
    );
}

/// The rows of a bybit demo box: the default account (id 4, stating `BTCUSDT`) and a labelled `INV` account (id 19)
/// on the same keys, `INV` stating `symbol` (or nothing) and active or not, read the way a root reads them.
fn bybit_rows(
    symbol: Option<&str>,
    active: bool,
) -> vike_secrets::account_directory::AccountDirectory {
    let row = |id, label: Option<&str>, symbol: Option<&str>, active| vike_secrets::AccountRow {
        id,
        venue: "bybit".to_string(),
        tier: vike_model::AccountTier::Demo,
        label: label.map(str::to_string),
        venue_account_id: None,
        parent_id: None,
        active,
        last_verified_at: None,
        max_exposure: None,
        leverage: None,
        symbol: symbol.map(str::to_string),
        facts: Default::default(),
    };
    vike_secrets::account_directory::AccountDirectory::read(
        Ok::<_, String>(vike_secrets::AccountRows::Known(vec![
            row(4, None, Some("BTCUSDT"), true),
            row(19, Some("INV"), symbol, active),
        ])),
        Ok::<_, String>(None),
        Ok::<_, String>(vike_secrets::account_credentials::CredentialRegistry::new()),
    )
}

/// **A labelled account no mount names is mounted on the symbol its row STATES** (the inverse `BTCUSD`
/// engine beside the default account's linear `BTCUSDT`: on that symbol it would double-count the USDT
/// wallet the default account already trades), and the default account on the symbol ITS row states. A
/// row stating nothing, or an inactive one, gets NO symbol (owner, 2026-11-13: nothing falls back to another
/// account's; such an account does not run); a strategy mount naming the account still wins.
#[test]
fn a_labelled_account_is_mounted_on_its_stated_symbol_unless_a_mount_names_it() {
    use vike_model::accounts::account_keys::AccountLabel;
    let inverse = AccountLabel::parse("INV").expect("a legal label");
    let default = (AccountLabel::Default, "BTCUSDT".to_string());

    let stated = bybit_rows(Some("BTCUSD"), true);
    let account_symbols = super::account_symbols_for(&[], "bybit", &stated);
    assert_eq!(account_symbols, vec![default.clone(), (inverse.clone(), "BTCUSD".to_string())]);
    assert_eq!(crate::symbol_for_account(&account_symbols, &inverse), "BTCUSD");
    assert_eq!(crate::symbol_for_account(&account_symbols, &AccountLabel::Default), "BTCUSDT");
    // Another venue's rows contribute nothing.
    assert!(super::account_symbols_for(&[], "okx", &stated).is_empty());

    // Nothing stated, or an inactive row: the default account alone, and no symbol for `INV`.
    for rows in [bybit_rows(None, true), bybit_rows(Some("BTCUSD"), false)] {
        let account_symbols = super::account_symbols_for(&[], "bybit", &rows);
        assert_eq!(account_symbols, vec![default.clone()]);
        assert_eq!(crate::symbol_for_account(&account_symbols, &inverse), "");
    }
    // An unread directory states nothing at all.
    let unread = vike_secrets::account_directory::AccountDirectory::unread();
    assert!(super::account_symbols_for(&[], "bybit", &unread).is_empty());

    // A strategy mount naming the account wins over its row.
    let mount = super::MountRoute {
        venue: "bybit".to_string(),
        symbol: "ETHUSD".to_string(),
        account: Some(inverse.clone()),
    };
    assert_eq!(
        super::account_symbols_for(&[mount], "bybit", &stated),
        vec![default, (inverse, "ETHUSD".to_string())]
    );
}

/// bybit's kept list as the datahub keeps it: the spot pair under its user name, the inverse perpetual `.P`. Every
/// test of this process that holds bybit's list holds THESE rows (one list per process).
fn hold_bybit_kept_list() {
    use vike_model::{Contract, ContractPerp, Instrument, Margining};
    let inverse = Contract::Perp(ContractPerp {
        margining: Margining::Inverse,
        settle: "BTC".into(),
        value_currency: None,
        funding_interval_ms: None,
        settling_ms: None,
    });
    vike_catalog::hold_kept_list(
        "bybit",
        vec![
            Instrument::new("bybit", "ETHUSDT", "ETH", "USDT", Contract::CryptoSpot),
            Instrument::new("bybit", "BTCUSD.P", "BTC", "USD", inverse),
        ],
    );
}

/// **An ENGINE's fee lane reads its USER name's KEPT row** (`vike_catalog::kept_row_of`, decision 0290; BYB-3, BYB-7):
/// the perpetual `ETHUSDT.P` an engine trades is never the SPOT row the list keeps under `ETHUSDT` (the row rule must
/// not price a live perpetual as spot), and with no row of its own it keeps the perpetual's lane, read off its name;
/// `ETHUSDT` is that spot row, and `BTCUSD.P` the inverse perpetual.
#[test]
fn an_engines_fee_lane_reads_its_user_names_kept_row() {
    hold_bybit_kept_list();
    assert_eq!(vike_catalog::kept_row_of("bybit", "ETHUSDT.P"), None, "no perpetual row is kept");
    assert_eq!(crate::engine::kept_fee_lane("bybit", "ETHUSDT.P"), "bybit");
    assert_eq!(crate::engine::kept_fee_lane("bybit", "ETHUSDT"), "bybit-spot");
    let inverse = vike_catalog::kept_row_of("bybit", "BTCUSD.P").map(|r| r.raw_symbol);
    assert_eq!(inverse.as_deref(), Some("BTCUSD.P"));
    assert_eq!(crate::engine::kept_fee_lane("bybit", "BTCUSD.P"), "bybit");
}

/// **A row stated any way but `account set-symbol` is mounted on its user name** (the C-D4 follow-up, decision
/// 0290): a hand-written `ethusdt` is mounted on bybit's spot pair `ETHUSDT`, `btcusd.p` on the inverse perpetual
/// `BTCUSD.P`; a name bybit's KEPT list carries no row of is mounted as stated, as before (the adapter answers it).
#[test]
fn a_stated_symbol_written_another_way_is_mounted_on_the_doors_spelling() {
    use vike_model::accounts::account_keys::AccountLabel;
    hold_bybit_kept_list();
    let inverse = AccountLabel::parse("INV").expect("a legal label");
    for (stated, mounted) in
        [("ethusdt", "ETHUSDT"), ("btcusd.p", "BTCUSD.P"), ("BTCUSDX", "BTCUSDX")]
    {
        let account_symbols =
            super::account_symbols_for(&[], "bybit", &bybit_rows(Some(stated), true));
        assert_eq!(crate::symbol_for_account(&account_symbols, &inverse), mounted, "{stated}");
    }
}

/// **F4 of the twin review: stating a leverage must not refuse every strategy on the venue.** The core
/// refuses an account-less mount on a venue it runs two engines of, and a DERIVED lane twin makes binance
/// one. Account-less mounts of that venue (every shipped mount) name the DEFAULT account instead, which
/// routes to the same spot engine at any engine count; a mount that names an account, a mount on another
/// venue, and a venue whose `PERP` account is the operator's own are left exactly as they were.
#[test]
fn an_account_less_mount_names_the_default_account_where_a_derived_lane_twin_runs() {
    use vike_model::accounts::account_keys::AccountLabel;
    use vike_strategy_lab::makers::spread_maker::SpreadMaker;
    let alt = AccountLabel::parse("ALT").expect("a legal label");
    let mount = |venue: &str, account: Option<AccountLabel>| vike_core::Mount {
        order_client_id_code: None,
        account,
        symbols: Vec::new(),
        controller_id: None,
        underlying_symbol: None,
        venue: venue.to_string(),
        symbol: "BTCUSDT".to_string(),
        interval: "1m".to_string(),
        strategy: Box::new(SpreadMaker::new(1.0, 0.5)),
    };
    let config = || vike_core::CoreConfig {
        strategy: Some(mount("binance", None)),
        extra_mounts: vec![
            mount("bybit", None),
            mount("binance", Some(alt.clone())),
            mount("binance", None),
        ],
        ..vike_core::CoreConfig::default()
    };
    let accounts_of = |core: &vike_core::CoreConfig| -> Vec<Option<AccountLabel>> {
        core.strategy.iter().chain(core.extra_mounts.iter()).map(|m| m.account.clone()).collect()
    };
    let engines = vec![
        ("binance".to_string(), "binance".to_string()),
        ("binance".to_string(), "binance#PERP".to_string()),
        ("bybit".to_string(), "bybit".to_string()),
    ];

    let mut core = config();
    super::accounts::name_the_default_account_where_a_lane_twin_runs(
        &mut core,
        &engines,
        &binance_rows(Some(5.0), false),
    );
    assert_eq!(
        accounts_of(&core),
        [Some(AccountLabel::Default), None, Some(alt.clone()), Some(AccountLabel::Default)],
        "binance's account-less mounts name DEFAULT; bybit's and the named one are untouched"
    );

    // No derived twin (no leverage stated; or the operator's OWN `PERP` account): nothing changes.
    for rows in [binance_rows(None, false), binance_rows(Some(5.0), true)] {
        let mut core = config();
        super::accounts::name_the_default_account_where_a_lane_twin_runs(
            &mut core, &engines, &rows,
        );
        assert_eq!(accounts_of(&core), accounts_of(&config()));
    }
    // A twin that is not among the engines this core runs: nothing changes either.
    let mut core = config();
    let spot_only = vec![("binance".to_string(), "binance".to_string())];
    super::accounts::name_the_default_account_where_a_lane_twin_runs(
        &mut core,
        &spot_only,
        &binance_rows(Some(5.0), false),
    );
    assert_eq!(accounts_of(&core), accounts_of(&config()));
}

// ── a journaled engine whose ledger label was the guess is restored under the label it now states ──

/// **The first restart after an engine is told its currency must not drop its journaled state** (decision 0183,
/// item 1). `assemble_engine` relabels a converting account's engine from the constructor's `"USDT"` guess to the
/// currency the venue states; `vike_core`'s boot restore skips a default account's snapshot whose label differs,
/// and a venue with no reconcile client (dukascopy) has nothing to put the position back.
mod ledger_label_restore {
    use super::super::accounts::adopt_stated_currency_in_restored_engines;
    use std::time::{Duration, Instant};
    use vike_execution::execution_engine::DEFAULT_QUOTE_ASSET;
    use vike_execution::testing::RecordingClient;
    use vike_execution::{Account, BalanceMode, Engine, EngineSnapshot, ExecutionClient, RiskGate};
    use vike_model::RiskLimits;
    use vike_model::events::{FillEvent, PositionSide};

    type DynClient = Box<dyn ExecutionClient + Send>;

    const VENUE: &str = "dukascopy";

    fn engine(symbol: &str) -> Engine<DynClient> {
        engine_on(VENUE, symbol)
    }

    fn engine_on(venue: &str, symbol: &str) -> Engine<DynClient> {
        Engine::new(
            Account::new(1.0, venue, None, BalanceMode::Delta),
            RiskGate::new(RiskLimits::new()),
            Box::new(RecordingClient::default()) as DynClient,
            venue,
            symbol,
        )
    }

    /// The engine a converting account's start builds: told the currency the venue stated.
    fn told(symbol: &str, currency: &str) -> Engine<DynClient> {
        told_on(VENUE, symbol, currency)
    }

    fn told_on(venue: &str, symbol: &str, currency: &str) -> Engine<DynClient> {
        let mut e = engine_on(venue, symbol);
        e.set_account_currency(currency).expect("a currency");
        e
    }

    /// What the run before the relabel journaled: an unstated engine holding 2000 units of `symbol`.
    fn journaled(symbol: &str) -> EngineSnapshot {
        journaled_on(VENUE, symbol)
    }

    fn journaled_on(venue: &str, symbol: &str) -> EngineSnapshot {
        let mut e = engine_on(venue, symbol);
        e.account.apply_fill(&FillEvent {
            trade_id: "t1".into(),
            order_client_id: String::new(),
            venue: venue.into(),
            symbol: symbol.into(),
            side: 1,
            last_qty: 2_000.0,
            last_price: 1.1,
            commission: 0.0,
            commission_asset: String::new().into(),
            liquidity_side: "taker".into(),
            timestamp: 0,
            mark_price: None,
            position_side: PositionSide::Both,
        });
        e.snapshot_state()
    }

    fn label_after(snap: EngineSnapshot, live: &Engine<DynClient>) -> (String, EngineSnapshot) {
        let mut snaps = [snap];
        adopt_stated_currency_in_restored_engines(&mut snaps, &[live]);
        let [snap] = snaps;
        (snap.quote_asset.clone(), snap)
    }

    #[test]
    fn an_untold_engine_keeps_the_guess_and_a_told_one_its_currency() {
        assert!(!engine("EURUSD").account_currency_known());
        assert_eq!(told("EURUSD", "usd").quote_asset, "USD");
    }

    #[test]
    fn a_guessed_label_takes_the_currency_the_mounted_pair_is_quoted_in() {
        let snap = journaled("EURUSD");
        assert_eq!(snap.quote_asset, "USDT");
        let (label, snap) = label_after(snap, &told("EURUSD", "USD"));
        assert_eq!(label, "USD");
        assert_eq!(
            snap.account.positions.len(),
            1,
            "only the label moved; the state is the journal's"
        );
    }

    #[test]
    fn nothing_else_is_relabelled() {
        let kept = |snap: EngineSnapshot, live: &Engine<DynClient>| {
            let before = snap.quote_asset.clone();
            assert_eq!(label_after(snap, live).0, before);
        };
        // an account whose money is not the mounted pair's quote (a EUR account on EURUSD): the numbers the old
        // run booked at par are dollars, not euros
        kept(journaled("EURUSD"), &told("EURUSD", "EUR"));
        // an engine nobody told: its label is the guess on both sides
        kept(journaled("EURUSD"), &engine("EURUSD"));
        // another symbol, or a mount that is not a six-letter pair
        kept(journaled("EURUSD"), &told("GBPUSD", "USD"));
        kept(journaled("BTCUSDT"), &told("BTCUSDT", "USD"));
        // a snapshot that WAS told its currency, even the guess's own spelling (`account_currency_set`)
        let mut stated = engine("EURUSD");
        stated.set_account_currency("USDT").expect("a currency");
        let snap = stated.snapshot_state();
        assert!(snap.account_currency_set);
        kept(snap, &told("EURUSD", "USD"));
        // a labelled account's snapshot is matched by its route key and never skipped: left alone
        let mut labelled = journaled("EURUSD");
        labelled.route_key = Some("dukascopy#ALT".into());
        kept(labelled, &told("EURUSD", "USD"));
    }

    /// A snapshot is adopted only by the engine that IS its venue's: the same market journaled under another venue,
    /// and a default snapshot facing a labelled account's engine (route key `venue#LABEL`), keep their label.
    #[test]
    fn a_snapshot_of_another_venue_or_account_is_not_adopted() {
        let mut elsewhere = journaled("EURUSD");
        elsewhere.venue = "oanda".into();
        assert_eq!(label_after(elsewhere, &told("EURUSD", "USD")).0, DEFAULT_QUOTE_ASSET);
        let mut labelled_engine = told("EURUSD", "USD");
        labelled_engine.route_key = "dukascopy#ALT".into();
        assert_eq!(label_after(journaled("EURUSD"), &labelled_engine).0, DEFAULT_QUOTE_ASSET);
    }

    /// A snapshot that states ANOTHER tier than the live engine's is another book (`EngineSnapshot::tier`: a
    /// `paper` run's snapshot facing the `demo` engine after a tier flip) and is not adopted, exactly as the core's
    /// restore refuses it; one that states none (journaled before the field existed) is unknown and is adopted as
    /// it always was.
    #[test]
    fn a_snapshot_of_another_tier_is_not_adopted_and_an_unknown_one_is() {
        use vike_model::AccountTier;
        let mut demo = told("EURUSD", "USD");
        demo.set_tier(AccountTier::Demo);
        let journaled_at = |tier: Option<AccountTier>| {
            let mut snap = journaled("EURUSD");
            snap.tier = tier;
            snap
        };
        for other in [AccountTier::Paper, AccountTier::Live] {
            assert_eq!(
                label_after(journaled_at(Some(other)), &demo).0,
                DEFAULT_QUOTE_ASSET,
                "a {other:?} snapshot keeps its label"
            );
        }
        assert_eq!(label_after(journaled_at(Some(AccountTier::Demo)), &demo).0, "USD");
        assert_eq!(
            label_after(journaled_at(None), &demo).0,
            "USD",
            "an unknown tier is adopted as it always was"
        );
    }

    /// The restore runs once per boot, but the adoption is idempotent: a second call finds the label already stated
    /// and moves nothing.
    #[test]
    fn adopting_twice_is_adopting_once() {
        let live = told("EURUSD", "USD");
        let mut snaps = [journaled("EURUSD")];
        adopt_stated_currency_in_restored_engines(&mut snaps, &[&live]);
        let once = snaps.clone();
        adopt_stated_currency_in_restored_engines(&mut snaps, &[&live]);
        assert_eq!(snaps, once);
        assert_eq!(snaps[0].quote_asset, "USD");
    }

    /// The position the real core PUBLISHES for `EURUSD`, read from the first snapshot it has published (`seq > 0`:
    /// the cell holds a placeholder with no venue block until then). The wait is the same for both arms and a core
    /// that publishes nothing PANICS, so a skipped restore (size 0) is only ever read off a book the core really
    /// built: an arm that expects 0 cannot pass on a core that said nothing.
    fn restored_size(adopt: bool) -> f64 {
        restored_size_on(VENUE, "EURUSD", adopt)
    }

    /// [`restored_size`] of `symbol` on `venue`'s default account.
    fn restored_size_on(venue: &str, symbol: &str, adopt: bool) -> f64 {
        let live = told_on(venue, symbol, "USD");
        let mut config = vike_core::CoreConfig {
            seed_cash: 10_000.0,
            restored_engines: vec![journaled_on(venue, symbol)],
            ..vike_core::CoreConfig::default()
        };
        if adopt {
            adopt_stated_currency_in_restored_engines(&mut config.restored_engines, &[&live]);
        }
        let handle = vike_core::spawn_core_multi(live, vec![], config);
        let cell = handle.snapshot_cell();
        // The core publishes after a message, not at boot: any command makes it.
        handle.send_command(vike_execution::Command::SetTradingState(
            vike_model::TradingState::Active,
        ));
        let until = Instant::now() + Duration::from_secs(10);
        let published = loop {
            let snap = cell.load_full();
            if snap.sequence > 0 || Instant::now() > until {
                break snap;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        handle.shutdown_and_join();
        assert!(published.sequence > 0, "the core published nothing in 10 s (adopt = {adopt})");
        let venue = published.portfolio.venues.first().expect("the published book has the venue");
        venue.positions.iter().find(|p| p.symbol == symbol).map_or(0.0, |p| p.size)
    }

    /// **Through the real core**: with the label adopted the journaled position is back on the book; without it the
    /// core skips the snapshot (the defect this exists for, shown on the same fixture). Both arms read the first
    /// book the core published ([`restored_size`]), so the skip arm is a measurement and not a timeout.
    #[test]
    fn the_core_restores_the_position_only_when_the_labels_agree() {
        assert_eq!(
            restored_size(true),
            2_000.0,
            "the relabelled engine got its journaled position back"
        );
        assert_eq!(restored_size(false), 0.0, "without the adoption the core skips the snapshot");
    }

    /// ibkr's kept list as the operator keeps it (decision 0290's O-10): the dollar stock `AAPL`. Every test of this
    /// process that holds ibkr's list holds THESE rows (one list per process).
    fn hold_ibkr_rows() {
        use vike_model::{Contract, ContractEquity, Instrument};
        let equity = ContractEquity { exchange: None, currency: Some("USD".into()) };
        vike_catalog::hold_kept_list(
            "ibkr",
            vec![Instrument::new(
                "ibkr",
                "AAPL.SMART.USD",
                "AAPL",
                "USD",
                Contract::Equity(equity),
            )],
        );
    }

    /// **An ibkr `AAPL` book journaled under the guess `USDT` is restored as USD** (the CI box, 2026-11-17: the engine
    /// journaled `AAPL/USDT` while ibkr had no kept list, and the start after the operator's row anchored it to the
    /// account's USD and skipped the book). The row states AAPL's money, so the label is adopted and the core puts the
    /// position back; a stock no row states the money of keeps the guess (the core skips it, loudly), and a crypto
    /// pair's guess is untouched: `USDT` names no fiat money, so nothing is read off it.
    #[test]
    fn an_ibkr_stock_journaled_under_the_guess_is_restored_as_the_dollars_its_row_states() {
        hold_ibkr_rows();
        let (label, _) = label_after(journaled_on("ibkr", "AAPL"), &told_on("ibkr", "AAPL", "USD"));
        assert_eq!(label, "USD");
        assert_eq!(restored_size_on("ibkr", "AAPL", true), 2_000.0, "the book is back");
        let (label, _) = label_after(journaled_on("ibkr", "MSFT"), &told_on("ibkr", "MSFT", "USD"));
        assert_eq!(label, DEFAULT_QUOTE_ASSET, "no row states MSFT's money");
        assert_eq!(restored_size_on("ibkr", "MSFT", true), 0.0, "skipped, never guessed");
        for (venue, symbol) in [("binance", "BTCUSDT"), ("bybit", "BTCUSDT.P")] {
            let (label, _) =
                label_after(journaled_on(venue, symbol), &told_on(venue, symbol, "USD"));
            assert_eq!(label, DEFAULT_QUOTE_ASSET, "{venue} {symbol}: the crypto guess stands");
        }
    }
}

// ── a journaled spot position on a spot mount is restored `Cash` ──────────────────────────────

/// **A book journaled before a spot mount stated `Cash` must not keep its `Cross` entries**: an entry keeps the mode
/// it was born in for good, so without the restore's rewrite the margin-call sweep would go on judging the holding
/// against a wallet net of its cost after every restart.
mod spot_position_restore {
    use super::super::accounts::restore_spot_positions_cash;
    use vike_execution::testing::RecordingClient;
    use vike_execution::{Account, BalanceMode, Engine, EngineSnapshot, ExecutionClient, RiskGate};
    use vike_model::MarginMode::{self, Cash, Cross};
    use vike_model::RiskLimits;
    use vike_model::events::{FillEvent, PositionSide, TradeId};

    type DynClient = Box<dyn ExecutionClient + Send>;

    fn engine(symbol: &str) -> Engine<DynClient> {
        engine_on("binance", symbol)
    }

    fn engine_on(venue: &str, symbol: &str) -> Engine<DynClient> {
        Engine::new(
            Account::new(1.0, venue, None, BalanceMode::Delta),
            RiskGate::new(RiskLimits::new()),
            Box::new(RecordingClient::default()) as DynClient,
            venue,
            symbol,
        )
    }

    /// The engine a fully funded mount's start builds: `Cash` stated for its own symbol (`assemble_engine`).
    fn spot(symbol: &str) -> Engine<DynClient> {
        stating_cash("binance", symbol)
    }

    fn stating_cash(venue: &str, symbol: &str) -> Engine<DynClient> {
        let mut e = engine_on(venue, symbol);
        e.account.admit_margin_mode(symbol, Some(Cash));
        e
    }

    /// What a run before the statement journaled: an engine on `mount` holding one `Cross` entry per symbol held.
    fn journaled(mount: &str, held: &[&str]) -> EngineSnapshot {
        journaled_on("binance", mount, held)
    }

    fn journaled_on(venue: &str, mount: &str, held: &[&str]) -> EngineSnapshot {
        let mut e = engine_on(venue, mount);
        for symbol in held {
            e.account.apply_fill(&FillEvent {
                trade_id: TradeId::prefixed("t-", symbol),
                order_client_id: String::new(),
                venue: venue.into(),
                symbol: (*symbol).into(),
                side: 1,
                last_qty: 0.01,
                last_price: 77_450.0,
                commission: 0.0,
                commission_asset: String::new().into(),
                liquidity_side: "taker".into(),
                timestamp: 0,
                mark_price: None,
                position_side: PositionSide::Both,
            });
        }
        e.snapshot_state()
    }

    fn modes(snap: &EngineSnapshot) -> Vec<(String, MarginMode)> {
        snap.account.positions.iter().map(|((_, s, _), p)| (s.to_string(), p.margin_mode)).collect()
    }

    #[test]
    fn a_spot_mounts_journaled_spot_entries_come_back_cash_and_nothing_else_moves() {
        let live = spot("BTCUSDT");
        let mut snaps = [journaled("BTCUSDT", &["BTCUSDT", "ETHUSDT", "ETHUSDT.P"])];
        restore_spot_positions_cash(&mut snaps, &[&live]);
        assert_eq!(
            modes(&snaps[0]),
            vec![("BTCUSDT".into(), Cash), ("ETHUSDT".into(), Cash), ("ETHUSDT.P".into(), Cross)],
            "the spot pairs move, a perpetual's entry is the journal's"
        );
        assert_eq!(snaps[0].account.positions[0].1.size, 0.01, "only the mode moved");
        let once = snaps.clone();
        restore_spot_positions_cash(&mut snaps, &[&live]);
        assert_eq!(snaps, once, "idempotent");

        // an engine that states no `Cash` (a perp mount, a paper engine): its snapshot is the journal's
        let mut perp = [journaled("BTCUSDT.P", &["BTCUSDT.P", "BTCUSDT"])];
        restore_spot_positions_cash(&mut perp, &[&engine("BTCUSDT.P")]);
        assert_eq!(modes(&perp[0]), vec![("BTCUSDT.P".into(), Cross), ("BTCUSDT".into(), Cross)]);
        // a snapshot of another account is not this engine's
        let mut other = journaled("BTCUSDT", &["BTCUSDT"]);
        other.route_key = Some("binance#ALT".into());
        let mut others = [other];
        restore_spot_positions_cash(&mut others, &[&live]);
        assert_eq!(modes(&others[0]), vec![("BTCUSDT".into(), Cross)]);
    }

    /// A venue whose ONLY margin mode is `Cash` (polymarket: an outcome share is bought outright) has every
    /// journaled entry restored `Cash`, whatever the symbol's shape; its engine states `Cash` for its own mount.
    #[test]
    fn a_cash_only_venues_journaled_entries_all_come_back_cash() {
        let (yes, no) = ("7132104567925221", "1484359444963781");
        let live = stating_cash("polymarket", "");
        let mut snaps = [journaled_on("polymarket", "", &[yes, no])];
        restore_spot_positions_cash(&mut snaps, &[&live]);
        assert_eq!(modes(&snaps[0]), vec![(yes.into(), Cash), (no.into(), Cash)]);
        // ...and only on an engine that states it: the same snapshot beside an engine that does not is the journal's
        let mut untold = [journaled_on("polymarket", "", &[yes])];
        restore_spot_positions_cash(&mut untold, &[&engine_on("polymarket", "")]);
        assert_eq!(modes(&untold[0]), vec![(yes.into(), Cross)]);
    }
}

// ── the `live-event-forward` relay (journal v18) ──────────────────────────────────────────────

mod forwarder {
    use super::super::build::forward_ingest;
    use vike_execution::{
        Command, FlowUpdate, GatewayEvent, Ingest, StreamStatusUpdate, TickSender, event_channel,
    };
    use vike_model::EventRingCursor;
    use vike_model::FeedStatus;
    use vike_model::events::{Event, OrderSubmitted};

    fn submitted(order_client_id: &str) -> Event {
        Event::OrderSubmitted(OrderSubmitted {
            order_client_id: order_client_id.into(),
            timestamp: 7,
        })
    }

    /// The core's ingest as the relay sees it: one queue, an event door and a tick door onto it.
    fn core() -> (vike_execution::EventSender, TickSender, tokio::sync::mpsc::Receiver<Ingest>) {
        let (events, rx) = event_channel(8);
        let ticks = TickSender { ingest: events.ingest.clone() };
        (events, ticks, rx)
    }

    /// A plain venue event reaches the core as a plain event: the relay's old, only behaviour.
    #[test]
    fn a_plain_event_reaches_the_core_as_an_event() {
        let (events, ticks, mut rx) = core();
        forward_ingest(Ingest::Event(submitted("c1")), &events, &ticks).unwrap();
        assert!(matches!(rx.try_recv(), Ok(Ingest::Event(Event::OrderSubmitted(_)))));
    }

    /// **The silent drop this relay used to be.** An event that came over the trade link carries
    /// its ring cursor, and the core journals the pair as ONE record. A relay that unwrapped only
    /// `Ingest::Event` (the `if let` it was) would deliver nothing here, and the cursor would die
    /// between the remote client and the journal. The planted-variant test: this fails when the
    /// arm is a `_ =>`.
    #[test]
    fn a_gateway_event_reaches_the_core_with_its_cursor() {
        let (events, ticks, mut rx) = core();
        let cursor = EventRingCursor { gateway_epoch: 77, sequence: 12, timestamp_ms: 99 };
        let gw = GatewayEvent { event: submitted("c2"), route_key: "binance#alt".into(), cursor };
        forward_ingest(Ingest::GatewayEvent(Box::new(gw)), &events, &ticks).unwrap();
        match rx.try_recv() {
            Ok(Ingest::GatewayEvent(g)) => {
                assert_eq!(g.cursor, cursor, "the cursor is not dropped or rewritten");
                assert_eq!(g.route_key.as_str(), "binance#alt", "and the ring it belongs to");
                assert!(matches!(g.event, Event::OrderSubmitted(_)));
            }
            other => panic!("the cursor never reached the core: {other:?}"),
        }
        assert!(rx.try_recv().is_err(), "one record, not an event and a cursor");
    }

    /// The link's own disclosure reaches the core as a status (the dead-man's and the strategies'
    /// input), not as an event and not nowhere.
    #[test]
    fn a_link_status_reaches_the_core_as_a_status() {
        let (events, ticks, mut rx) = core();
        let status = StreamStatusUpdate {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            stream: "gateway-trade".into(),
            status: FeedStatus::Disconnected,
        };
        forward_ingest(Ingest::StreamStatus(Box::new(status)), &events, &ticks).unwrap();
        match rx.try_recv() {
            Ok(Ingest::StreamStatus(s)) => {
                assert_eq!((s.venue.as_str(), s.status), ("binance", FeedStatus::Disconnected));
            }
            other => panic!("the link status never reached the core: {other:?}"),
        }
    }

    /// A trade-link transition reaches the core on its own variant: the stuck-order watchdog's
    /// input. Dropped here (`_ => Ok(())`), the watchdog would reject orders of an account whose
    /// link is merely down. The planted-variant test: this fails when the arm is a `_ =>`.
    #[test]
    fn an_execution_link_transition_reaches_the_core() {
        let (events, ticks, mut rx) = core();
        let update = vike_execution::TradeLinkUpdate {
            route_key: "binance#alt".into(),
            venue: "binance".into(),
            up: false,
            at_timestamp_ms: 31,
        };
        forward_ingest(Ingest::TradeLink(Box::new(update.clone())), &events, &ticks).unwrap();
        match rx.try_recv() {
            Ok(Ingest::TradeLink(u)) => assert_eq!(*u, update, "the transition arrives unchanged"),
            other => panic!("the link transition never reached the core: {other:?}"),
        }
        assert!(rx.try_recv().is_err());
    }

    /// Traffic of other lanes was never relayed and still is not; the relay does not stop for it.
    #[test]
    fn other_lanes_traffic_is_dropped_as_before() {
        let (events, ticks, mut rx) = core();
        for ing in [
            Ingest::Watchdog,
            Ingest::Market,
            Ingest::Command(Command::Shutdown),
            Ingest::Flow(Box::new(FlowUpdate {
                venue: "polymarket".into(),
                symbol: "TOK".into(),
                flow: vike_model::FlowToxicity { bid: 0.1, ask: 0.2, timestamp: 1 },
            })),
        ] {
            forward_ingest(ing, &events, &ticks).unwrap();
        }
        assert!(rx.try_recv().is_err(), "nothing of another lane reaches the core's ingest");
    }

    /// **W13b finding 2: the boot-time replay must not stall the boot.** A lease resumed from the
    /// journal's cursor is replayed a whole ring (20 000 events) while the boot thread is still
    /// assembling, and the boot thread waits for reply frames that same lease reader completes. The
    /// stand-in below is that reader: it delivers the replay into the live lane (4096 slots, as in
    /// production), THEN completes the boot thread's request. Nothing may be waiting for the core
    /// before it exists: the request completes with no core, and when the core appears every event
    /// arrives exactly once, in order.
    #[test]
    fn a_replay_larger_than_the_lane_is_drained_before_the_core_exists_and_relayed_once_in_order() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;
        use std::time::Duration;
        const REPLAY: u64 = 20_000;

        let (live, live_rx) = event_channel(4096);
        let (door, door_rx) = std::sync::mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let relay = {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                super::super::build::run_live_forwarder(live_rx, &door_rx, &stop);
            })
        };
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            for seq in 1..=REPLAY {
                let cursor = EventRingCursor { gateway_epoch: 9, sequence: seq, timestamp_ms: 0 };
                let gw = GatewayEvent {
                    event: submitted("replayed"),
                    route_key: "binance".into(),
                    cursor,
                };
                live.blocking_send_gateway(gw).expect("the lane stays open");
            }
            // The reply the boot thread is waiting for sits behind the replay in the stream.
            reply_tx.send(()).expect("the boot thread waits");
        });
        reply_rx.recv_timeout(Duration::from_secs(20)).expect(
            "the reader blocked on an undrained lane: the boot thread would never get its reply",
        );

        // The core appears (a small ingest, so the relay also has to wait for the core's drain).
        let (core_events, mut core_rx) = event_channel(64);
        let core_ticks = TickSender { ingest: core_events.ingest.clone() };
        door.send((core_events, core_ticks)).expect("the relay listens");
        for want in 1..=REPLAY {
            match core_rx.blocking_recv() {
                Some(Ingest::GatewayEvent(g)) => {
                    assert_eq!(g.cursor.sequence, want, "in order, none lost, none twice");
                }
                other => panic!("event {want} never reached the core: {other:?}"),
            }
        }
        reader.join().expect("the reader ends");
        relay.join().expect("the relay ends when the lane closes");
        assert!(core_rx.try_recv().is_err(), "and nothing more: no event is folded twice");
    }

    /// **W13c: at the cap the forwarder sheds, it does not stall the reply path.** Before: a backlog at the
    /// bound stopped draining, the 4096-slot lane filled, the lease reader (which is also the thread that
    /// completes the boot thread's `AdapterReply` / `WireIdsReply`) parked inside its delivery, and the boot
    /// thread timed out into a restart loop. Now the lane stays drained, the overflow is dropped loudly, an
    /// account that lost an event relays everything after it WITHOUT its cursor (the journal's resume cursor
    /// never jumps the gap), another account keeps its cursors, and a link's status is never shed.
    #[test]
    fn a_backlog_at_its_cap_sheds_the_overflow_keeps_the_reply_path_open_and_unpins_the_gapped_account()
     {
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;
        use std::time::Duration;
        const CAP: usize = 1_000;
        const SENT: u64 = 5_000;

        let (live, live_rx) = event_channel(64);
        let (door, door_rx) = std::sync::mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let relay = {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                super::super::build::relay_with_cap(live_rx, &door_rx, &stop, CAP);
            })
        };
        let gateway = |route: &str, seq: u64| GatewayEvent {
            event: submitted("replayed"),
            route_key: route.into(),
            cursor: EventRingCursor { gateway_epoch: 9, sequence: seq, timestamp_ms: 0 },
        };
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        let reader = {
            let live = live.clone();
            std::thread::spawn(move || {
                for seq in 1..=SENT {
                    live.blocking_send_gateway(gateway("binance", seq))
                        .expect("the lane stays open");
                }
                // A link status past the cap: tiny, and the dead-man needs it.
                live.execution_link(vike_execution::TradeLinkUpdate {
                    route_key: "binance".into(),
                    venue: "binance".into(),
                    up: false,
                    at_timestamp_ms: 5,
                })
                .expect("the lane stays open");
                // The reply the boot thread waits for sits behind the whole replay.
                reply_tx.send(()).expect("the boot thread waits");
            })
        };
        reply_rx.recv_timeout(Duration::from_secs(20)).expect(
            "the reader parked behind a full backlog: the boot thread would never get its reply",
        );
        reader.join().expect("the reader ends");
        // Everything the reader sent has been taken off the lane (the last of it may still be in flight).
        let drained = std::time::Instant::now() + Duration::from_secs(20);
        while live.ingest.capacity() < live.ingest.max_capacity() {
            assert!(std::time::Instant::now() < drained, "the forwarder keeps the lane drained");
            std::thread::sleep(Duration::from_millis(2));
        }

        let (core_events, mut core_rx) = event_channel(64);
        let core_ticks = TickSender { ingest: core_events.ingest.clone() };
        door.send((core_events, core_ticks)).expect("the relay listens");
        for want in 1..=CAP as u64 {
            match core_rx.blocking_recv() {
                Some(Ingest::GatewayEvent(g)) => {
                    assert_eq!(
                        g.cursor.sequence, want,
                        "the kept prefix, in order, with its cursors"
                    );
                }
                other => panic!("event {want} of the kept prefix never arrived: {other:?}"),
            }
        }
        assert!(
            matches!(core_rx.blocking_recv(), Some(Ingest::TradeLink(u)) if !u.up),
            "the link status past the cap is kept"
        );
        // From here on, live traffic: the gapped account loses its cursor, another keeps it.
        live.blocking_send_gateway(gateway("binance", SENT + 1)).expect("open");
        live.blocking_send_gateway(gateway("bybit", 1)).expect("open");
        match core_rx.blocking_recv() {
            Some(Ingest::Event(Event::OrderSubmitted(_))) => {}
            other => panic!("the gapped account's next event must carry no cursor: {other:?}"),
        }
        match core_rx.blocking_recv() {
            Some(Ingest::GatewayEvent(g)) => assert_eq!(g.route_key.as_str(), "bybit"),
            other => panic!("another account keeps its cursor: {other:?}"),
        }
        drop(live);
        relay.join().expect("the relay ends when the lane closes");
        assert!(core_rx.try_recv().is_err(), "and nothing more");
    }

    /// The memory the cap allows, from the REAL sizes (`vike_execution`'s own guard holds `Ingest` at 168
    /// bytes): an `Ingest` inline in the queue plus the boxed `GatewayEvent` it points at, times the cap.
    /// The strings an event owns and the queue's last doubling come on top (documented at the constant:
    /// 120 to 170 MB at the cap). A field that grows either type moves this number and fails here, where
    /// the cap is.
    #[test]
    fn the_boot_backlog_cap_is_bounded_by_the_real_size_of_what_it_holds() {
        use super::super::build::PRECORE_BACKLOG_CAP;
        let per_event = std::mem::size_of::<Ingest>() + std::mem::size_of::<GatewayEvent>();
        let at_cap = PRECORE_BACKLOG_CAP * per_event;
        assert!(per_event <= 400, "an Ingest and its boxed GatewayEvent grew to {per_event} bytes");
        assert!(
            at_cap <= 100 * 1024 * 1024,
            "the backlog at its cap is {at_cap} bytes before any string: raise the documented \
             estimate at PRECORE_BACKLOG_CAP or lower the cap"
        );
    }

    /// An assembly that fails before the core exists closes the door: the relay ends instead of
    /// waiting for a core that will never come.
    #[test]
    fn a_door_closed_without_a_core_ends_the_relay() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;
        let (live, live_rx) = event_channel(8);
        let (door, door_rx) = std::sync::mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let relay = std::thread::spawn(move || {
            super::super::build::run_live_forwarder(live_rx, &door_rx, &stop);
        });
        drop(door);
        relay.join().expect("the relay ends with its door");
        assert!(live.blocking_send(submitted("late")).is_err(), "the lane is closed with it");
    }

    /// A core that is gone ends the relay (the thread `break`s on the error).
    #[test]
    fn a_gone_core_is_an_error() {
        let (events, ticks, rx) = core();
        drop(rx);
        assert!(forward_ingest(Ingest::Event(submitted("c3")), &events, &ticks).is_err());
    }
}
