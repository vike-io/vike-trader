use super::*;
use crate::run::MountPlacement;
use std::assert_matches;
use vike_model::XemmParams;
use vike_strategy_lab::makers::cross_exchange_maker::XemmMaker;

/// The v1 pair: hyperliquid maker `BTCUSDC.P` × okx taker `BTC-USDT-SWAP`.
fn v1() -> MountPlacement {
    xemm_mount("hyperliquid", "BTCUSDC.P", "okx", "BTC-USDT-SWAP", 0.5)
}

/// The v1 pair's tunables.
fn v1_params() -> XemmParams {
    xemm_params(0.01, 0.0005)
}

/// The maker the mount builders are handed (no library builds a strategy): the strategy file's
/// [`XemmMaker`] of a mount [`validate_xemm_mount`] passed at `total_fee`, `params` whole with the
/// two mount facts written over it (the round trip `total_fee` and the maker venue's grid, the
/// mount's `tick_size`), applied once over `XemmMaker::new` (so the maker's params epoch is 1). A
/// knob the bag leaves at its default keeps the ARMED default (the polarity inversion on
/// [`XemmParams`]).
fn build_xemm_maker(spec: &MountPlacement, params: &XemmParams, total_fee: f64) -> XemmMaker {
    let [hedge] = spec.legs.as_slice() else { panic!("validate_xemm_mount passed this mount") };
    let mut m = XemmMaker::new(
        &spec.symbol,
        &hedge.name,
        params.qty,
        params.min_profitability,
        total_fee,
        spec.tick_size,
    );
    m.apply_params(&XemmParams { total_fee, maker_tick_size: spec.tick_size, ..*params });
    m
}

/// `spec` with its one hedge leg at `symbol` on `venue`.
fn hedged_at(mut spec: MountPlacement, symbol: &str, venue: &str) -> MountPlacement {
    spec.legs = vec![MountLeg::at(symbol, venue)];
    spec
}

/// A PLANTED copy of the daemon's ten default rows
/// (`crates/vike-venues/src/venue_wiring.rs`'s `VENUE_WIRING` engine rows, no optional venue): this crate holds no
/// table (docs/decisions/0098).
/// `crates/vike-gatewayhub/tests/trade_roster/registry_shape/wired_markets.rs`'s `the_xemm_v1_pair_is_wired_in_the_real_table`
/// asserts the v1 pair against the real one.
const WIRED: &[RuntimeMarket] = &[
    RuntimeMarket { venue: "binance", engine_rank: 0 },
    RuntimeMarket { venue: "bybit", engine_rank: 1 },
    RuntimeMarket { venue: "okx", engine_rank: 2 },
    RuntimeMarket { venue: "deribit", engine_rank: 5 },
    RuntimeMarket { venue: "hyperliquid", engine_rank: 3 },
    RuntimeMarket { venue: "aster", engine_rank: 4 },
    RuntimeMarket { venue: "alpaca", engine_rank: 6 },
    RuntimeMarket { venue: "ctrader", engine_rank: 7 },
    RuntimeMarket { venue: "ig", engine_rank: 8 },
    RuntimeMarket { venue: "oanda", engine_rank: 9 },
];

/// The default account of each [`WIRED`] venue, stating the symbol its engine is mounted on (the account's row,
/// `account.symbol`: the taker's is what a hedge leg must name).
fn wired_accounts() -> vike_secrets::account_directory::AccountDirectory {
    let rows = [
        ("binance", "BTCUSDT"),
        ("bybit", "BTCUSDT"),
        ("okx", "BTC-USDT-SWAP"),
        ("deribit", "BTCUSD.P"),
        ("hyperliquid", "BTCUSDC.P"),
        ("aster", "BTCUSDT.P"),
        ("alpaca", "AAPL"),
        ("ctrader", "EURUSD"),
        ("ig", "CS.D.EURUSD.MINI.IP"),
        ("oanda", "EURUSD"),
    ]
    .into_iter()
    .zip(1..)
    .map(|((venue, symbol), id)| vike_secrets::AccountRow {
        id,
        venue: venue.to_string(),
        tier: vike_model::AccountTier::Demo,
        label: None,
        venue_account_id: None,
        parent_id: None,
        active: true,
        last_verified_at: None,
        max_exposure: None,
        leverage: None,
        symbol: Some(symbol.to_string()),
        facts: Default::default(),
    })
    .collect();
    vike_secrets::account_directory::AccountDirectory::from_rows(
        vike_secrets::AccountRows::Known(rows),
        None,
    )
}

/// The two paper books the generic multi-leg builder constructs for the v1 mount: the maker's own on
/// its venue and the hedge leg's on the TAKER venue, each through `paper_book`, the call the builder
/// makes per `(venue, symbol)`.
fn v1_books(
    halt: &StrategyCorePaperHalt,
) -> (vike_paper::ExecutionClientPaper, vike_paper::ExecutionClientPaper) {
    let spec = v1();
    let maker = crate::run::paper::paper_book(&spec.venue, &spec.symbol, &spec, halt);
    let hedge = crate::run::paper::paper_book("okx", "BTC-USDT-SWAP", &spec, halt);
    (maker, hedge)
}

/// BOTH paper books this mount stands up arm the operator HALT sentinel.
///
/// ⚠ `build_paper_xemm_core` was the unarmed third seam while `crates/vike-paper/src/client.rs`'s
/// `halt_path` doc claimed `crate::paper_client`/`paper_client_for` asserts would catch one. The
/// gate over the SET of seams is `crates/vike-ops/tests/architecture/paper_mount_arming_gate.rs`; this
/// is THIS mount's half: its two books are the generic builder's `paper_book`, so dropping that
/// function's `.with_halt_path(..)` -> red.
///
/// Both legs separately: a halt stopping quotes but not hedges drifts the rehearsal's inventory
/// as the live mount never would. It also pins WHICH sentinel (process-wide): with [`StrategyCorePaperHalt`]
/// a parameter, a mount armed at a path no operator knows has no kill switch, and `is_some()`
/// cannot tell.
#[test]
fn the_xemm_paper_mount_arms_both_books() {
    let process_wide = vike_boot::halt::halt_path_from_env();
    let (maker, hedge) = v1_books(&StrategyCorePaperHalt::ProcessWide);
    assert_eq!(
        maker.halt_path(),
        Some(process_wide.as_path()),
        "the MAKER paper book must arm the PROCESS-WIDE HALT sentinel — this is a mount, not a \
             simulation"
    );
    assert_eq!(
        hedge.halt_path(),
        Some(process_wide.as_path()),
        "the HEDGE paper book must arm it too: a halt that stops quotes but not hedges drifts \
             the rehearsal's inventory away from what the live mount would do"
    );
    assert_eq!(
        maker.halt_path(),
        hedge.halt_path(),
        "both legs must watch ONE sentinel — a cross-venue rehearsal has one file to `touch`"
    );
}

/// A PINNED sentinel reaches BOTH books: `crates/vike-tradehub-core/tests/scripted/xemm_scripted.rs` needs it so
/// its fills do not depend on the box holding a HALT file.
#[test]
fn a_pinned_sentinel_reaches_both_xemm_books() {
    let pinned = std::path::PathBuf::from("xemm-tests-own-this-sentinel-and-never-make-it/HALT");
    let (maker, hedge) = v1_books(&StrategyCorePaperHalt::Pinned(pinned.clone()));
    assert_eq!(maker.halt_path(), Some(pinned.as_path()), "the maker leg must be pinned");
    assert_eq!(hedge.halt_path(), Some(pinned.as_path()), "the hedge leg must be pinned too");
}

/// **A leg on ANOTHER venue rehearses on the generic paper builder** (Q5 of the one broker plan): the
/// leg's venue gets an engine and a book of its own, its symbol stamped on its own fills, seeded at
/// the mount's `seed_cash`. Before, the builder refused any leg on another venue (no seed declared its
/// paper account), so the cross-exchange maker kept a two-books builder of its own.
#[test]
fn a_leg_on_another_venue_gets_its_own_venues_engine_and_book() {
    let opts = crate::run::StrategyCorePaperConfig {
        halt: StrategyCorePaperHalt::Pinned(
            "xemm-tests-own-this-sentinel-and-never-make-it/HALT".into(),
        ),
        ..Default::default()
    };
    let strategy = Box::new(build_xemm_maker(&v1(), &v1_params(), 0.00065));
    let core = crate::run::build_paper_multi_strategy_core_with(
        vec![crate::run::MountBuild { strategy, spec: v1() }],
        opts,
    );
    let books: Vec<(String, String)> = core.fills.iter().map(|(book, _)| book.clone()).collect();
    core.handle.shutdown_and_join();
    assert_eq!(
        books,
        [("hyperliquid".into(), "BTCUSDC.P".into()), ("okx".into(), "BTC-USDT-SWAP".into())],
        "the maker's own book first, then the hedge leg's on its own venue"
    );
}

/// The v1 pair validates, and resolves the 6.5 bp round-trip fee (hyperliquid maker 1.5 +
/// okx taker 5.0) bit-for-bit — that number IS the maker's break-even offset.
#[test]
fn the_v1_pair_validates_and_resolves_six_and_a_half_bps() {
    let fee = validate_xemm_mount(&v1(), &v1_params(), Some((WIRED, &wired_accounts())))
        .expect("the v1 pair is wired and fee-expressible");
    assert_eq!(fee.to_bits(), (1.5_f64 / 10_000.0 + 5.0 / 10_000.0).to_bits());
}

/// THE HIJACK GUARD. A hedge NAMED like the maker's own symbol is refused: a strategy reaches a leg by
/// its name, and that name is the maker's own instrument, so the hedge would land on the maker's book.
#[test]
fn a_same_symbol_pair_is_refused() {
    let spec = hedged_at(v1(), "BTCUSDC.P", "bybit");
    assert_eq!(
        validate_xemm_mount(&spec, &v1_params(), None),
        Err(XemmConfigError::SameSymbol("BTCUSDC.P".into()))
    );
    let words = XemmConfigError::SameSymbol("BTCUSDT.P".into()).to_string();
    assert!(
        words.contains("`BTCUSDT.P@<taker venue>/<LABEL>`"),
        "the refusal names the fix: {words}"
    );
}

/// **One user name on two venues** (binance × bybit `BTCUSDT.P`, decision 0293): a hedge named by its
/// account is told apart from the maker's own name, so the pair validates, and its leg books on that
/// account. Live, the account the hedge names is the one whose row must state the hedge's symbol.
#[test]
fn a_same_symbol_pair_with_the_hedge_named_by_its_account_validates() {
    let spec = xemm_mount("binance", "BTCUSDT.P", "bybit", "BTCUSDT.P@bybit/DEMO", 0.1);
    let [hedge] = spec.legs.as_slice() else { panic!("one hedge leg") };
    assert_eq!(
        (hedge.name.as_str(), hedge.symbol.as_str(), hedge.venue.as_deref()),
        ("BTCUSDT.P@bybit/DEMO", "BTCUSDT.P", Some("bybit"))
    );
    assert_eq!(
        hedge.account,
        Some(vike_model::accounts::account_keys::AccountLabel::Named("DEMO".into()))
    );
    assert_matches!(validate_xemm_mount(&spec, &v1_params(), None), Ok(_));
    // the bare venue too, where that venue runs one account
    let bare = xemm_mount("binance", "BTCUSDT.P", "bybit", "BTCUSDT.P@bybit", 0.1);
    assert_matches!(validate_xemm_mount(&bare, &v1_params(), None), Ok(_));
    // …and live, the DEFAULT account's row states `BTCUSDT` (not the hedge's `BTCUSDT.P`): refused by
    // name, as the plain hedge's account would be
    assert_matches!(
        validate_xemm_mount(&bare, &v1_params(), Some((WIRED, &wired_accounts()))),
        Err(XemmConfigError::HedgeSymbolNotAccepted { .. })
    );
}

/// A hedge name the leg rule refuses is refused in its words, and a qualifier naming the MAKER's own
/// venue is not a cross-exchange hedge.
#[test]
fn a_hedge_leg_name_the_rule_refuses_is_refused() {
    let route_key = xemm_mount("binance", "BTCUSDT.P", "bybit", "BTCUSDT.P@bybit#DEMO", 0.1);
    assert_matches!(
        validate_xemm_mount(&route_key, &v1_params(), None),
        Err(XemmConfigError::HedgeLegName(why)) if why.contains("ROUTE KEY")
    );
    let own = xemm_mount("binance", "BTCUSDT", "bybit", "BTCUSDT.P@binance", 0.1);
    assert_matches!(
        validate_xemm_mount(&own, &v1_params(), None),
        Err(XemmConfigError::HedgeLegName(why)) if why.contains("the mount's own venue")
    );
}

/// Two legs on one venue is not a cross-exchange maker, and the reference lane (which requires
/// `m.venue != venue`) would never deliver a tick. A leg that names no venue is on the mount's.
#[test]
fn a_same_venue_pair_is_refused() {
    let named = hedged_at(v1(), "BTC-USDT-SWAP", "hyperliquid");
    let unnamed = MountPlacement { legs: vec![MountLeg::same_venue("BTC-USDT-SWAP")], ..v1() };
    for spec in [named, unnamed] {
        assert_eq!(
            validate_xemm_mount(&spec, &v1_params(), None),
            Err(XemmConfigError::SameVenue("hyperliquid".into()))
        );
    }
}

/// The mount declares exactly ONE leg — the hedge leg, on the taker venue. Declaring the maker's
/// own symbol as well would hijack every symbol-less intent (see [`XemmConfigError`]), and a mount
/// with no leg has no hedge.
#[test]
fn the_mount_declares_only_the_hedge_leg() {
    assert_eq!(v1().legs, vec![MountLeg::at("BTC-USDT-SWAP", "okx")]);
    let mut two = v1();
    two.legs.push(MountLeg::at("BTCUSDC.P", "okx"));
    let none = MountPlacement { legs: Vec::new(), ..v1() };
    assert_eq!(
        validate_xemm_mount(&two, &v1_params(), None),
        Err(XemmConfigError::NotOneHedgeLeg(2))
    );
    assert_eq!(
        validate_xemm_mount(&none, &v1_params(), None),
        Err(XemmConfigError::NotOneHedgeLeg(0))
    );
}

/// An unwired taker venue is a STARTUP error on the LIVE path: `apply_intent`'s `unwrap_or(0)`
/// would route the hedge to the MAKER engine, doubling the exposure silently.
///
/// PAPER consults no wired table, so the same config refuses DIFFERENTLY (an unknown venue's
/// fee is `Free`, no flat fraction): two errors for one config prove the wiring check is gated
/// on `Some(wired)`.
#[test]
fn an_unwired_taker_venue_is_refused_on_the_live_path_only() {
    let spec = hedged_at(v1(), "BTC-USDT-SWAP", "kalshi");
    assert_eq!(
        validate_xemm_mount(&spec, &v1_params(), Some((WIRED, &wired_accounts()))),
        Err(XemmConfigError::TakerVenueNotWired("kalshi".into()))
    );
    assert_matches!(
        validate_xemm_mount(&spec, &v1_params(), None),
        Err(XemmConfigError::FeeNotExpressible { .. }),
        "paper skips the wiring check and refuses on the fee shape instead"
    );
}

/// The taker venue's DEFAULT engine trades the ONE symbol its account's row states and `make_engine` sets no
/// `extra_symbols`, so a hedge symbol it is not mounted on would be dropped at the engine boundary. Refuse it by
/// name; a taker whose default account states no symbol refuses every hedge (it does not run).
#[test]
fn a_hedge_symbol_the_taker_engine_does_not_accept_is_refused() {
    let spec = hedged_at(v1(), "ETH-USDT-SWAP", "okx");
    assert_eq!(
        validate_xemm_mount(&spec, &v1_params(), Some((WIRED, &wired_accounts()))),
        Err(XemmConfigError::HedgeSymbolNotAccepted {
            venue: "okx".into(),
            wired: "BTC-USDT-SWAP".into(),
            requested: "ETH-USDT-SWAP".into(),
        })
    );
    let unread = vike_secrets::account_directory::AccountDirectory::unread();
    assert_eq!(
        validate_xemm_mount(&v1(), &v1_params(), Some((WIRED, &unread))),
        Err(XemmConfigError::HedgeSymbolNotAccepted {
            venue: "okx".into(),
            wired: String::new(),
            requested: "BTC-USDT-SWAP".into(),
        })
    );
}

/// A fee SHAPE with no flat fraction is refused on either leg, not defaulted to `0.0` (a free
/// round trip rests every quote inside break-even).
#[test]
fn a_fee_shape_without_a_flat_rate_is_refused_on_either_leg() {
    // oanda is `Free` (spread-charging) — a shape, not a value.
    let taker_free = hedged_at(v1(), "EURUSD", "oanda");
    assert_matches!(
        validate_xemm_mount(&taker_free, &v1_params(), Some((WIRED, &wired_accounts()))),
        Err(XemmConfigError::FeeNotExpressible { .. })
    );
    // a deribit OPTION is `PercentOfUnderlying`: its flat reading UNDERSTATES the real fee. (A deribit
    // PERPETUAL is a flat 0/5 bps since 2026-10-27: the `deribit-futures` lane is expressible, asserted after this case.)
    let maker_deribit =
        MountPlacement { venue: "deribit".into(), symbol: "BTC-27MAR26-80000-C".into(), ..v1() };
    assert_matches!(
        validate_xemm_mount(&maker_deribit, &v1_params(), Some((WIRED, &wired_accounts()))),
        Err(XemmConfigError::FeeNotExpressible { .. })
    );
    let maker_deribit_perp =
        MountPlacement { venue: "deribit".into(), symbol: "BTCUSD.P".into(), ..v1() };
    assert!(
        !matches!(
            validate_xemm_mount(
                &maker_deribit_perp,
                &v1_params(),
                Some((WIRED, &wired_accounts()))
            ),
            Err(XemmConfigError::FeeNotExpressible { .. })
        ),
        "a perpetual's fee is a flat rate and the leg is priced"
    );
}

/// The degenerate tunings that would make the maker either donate fees or refuse every tick.
#[test]
fn a_zero_edge_a_sub_tick_standoff_and_a_zero_size_are_each_refused() {
    // With a real fee the edge is positive even at zero profitability, so force it negative.
    let no_edge = XemmParams { min_profitability: -1.0, ..v1_params() };
    assert_eq!(validate_xemm_mount(&v1(), &no_edge, None), Err(XemmConfigError::NoEdge));

    let sub_tick = XemmParams { min_edge_ticks: 0.5, ..v1_params() };
    assert_eq!(validate_xemm_mount(&v1(), &sub_tick, None), Err(XemmConfigError::NoStandoff));
    let no_grid = MountPlacement { tick_size: 0.0, ..v1() };
    assert_eq!(validate_xemm_mount(&no_grid, &v1_params(), None), Err(XemmConfigError::NoStandoff));

    let no_size = xemm_params(0.0, 0.0005);
    assert_eq!(validate_xemm_mount(&v1(), &no_size, None), Err(XemmConfigError::NoSize));
}

/// The builder threads every named knob into the maker and leaves the ARMED defaults where a
/// knob was not named — the polarity inversion `XemmParams` documents — and writes the two mount
/// facts over the bag: the validated round trip and the mount's grid.
#[test]
fn the_builder_threads_named_knobs_and_keeps_armed_defaults() {
    let params = XemmParams {
        max_basis_bps: 40.0,
        basis_halflife_ms: 30_000,
        basis_clamp: 0.05,
        naked_band: 0.02,
        naked_hard_band: 0.05,
        fill_window_ms: 1_000,
        net_fill_threshold: 2.5,
        suppress_cooldown_ms: 5_000,
        total_fee: 9.0,
        maker_tick_size: 9.0,
        ..v1_params()
    };
    let fee = validate_xemm_mount(&v1(), &params, Some((WIRED, &wired_accounts()))).expect("valid");
    let m = build_xemm_maker(&v1(), &params, fee);
    let p = m.params();
    assert_eq!(m.legs(), ("BTCUSDC.P", "BTC-USDT-SWAP"));
    assert_eq!(p.total_fee.to_bits(), fee.to_bits(), "the validated round trip, not the bag's");
    assert_eq!(p.maker_tick_size.to_bits(), 0.5_f64.to_bits(), "the mount's grid, not the bag's");
    assert_eq!(p.max_basis_bps.to_bits(), 40.0_f64.to_bits());
    assert_eq!(p.naked_hard_band.to_bits(), 0.05_f64.to_bits());
    assert_eq!(p.net_fill_threshold.to_bits(), 2.5_f64.to_bits());
    // unnamed: the ARMED defaults, not neutral ones.
    assert_eq!(p.max_ref_age_ms, 2_000);
    assert_eq!(p.hedge_max_attempts, 3);
    assert_eq!(p.resume_after_halt_ms, 0, "auto-resume stays manual");
    assert_eq!(m.params_epoch, 1, "one apply over the constructed maker, as before");
}

/// The paper rehearsal stands up TWO engines over TWO single-symbol books and refuses a bad
/// config before spawning anything.
#[test]
fn the_paper_rehearsal_spawns_two_engines_and_refuses_a_bad_config() {
    let mount =
        build_paper_xemm_core(&v1(), &v1_params(), |s, p, f| Box::new(build_xemm_maker(s, p, f)))
            .expect("the v1 pair is valid");
    assert!(mount.maker_fills.lock().unwrap().is_empty());
    assert!(mount.hedge_fills.lock().unwrap().is_empty());
    mount.handle.shutdown_and_join();

    let bad = hedged_at(v1(), "BTCUSDC.P", "okx");
    assert!(
        build_paper_xemm_core(&bad, &v1_params(), |s, p, f| Box::new(build_xemm_maker(s, p, f)))
            .is_err(),
        "a hijacking config never spawns"
    );
}
