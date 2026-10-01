use super::*;

/// The v1 pair: hyperliquid maker `BTC` × okx taker `BTC-USDT-SWAP`.
fn v1() -> XemmMountConfig {
    XemmMountConfig::crypto("hyperliquid", "BTC", "okx", "BTC-USDT-SWAP", 0.01, 0.0005, 0.5)
}

/// A PLANTED wired-market table: a verbatim copy of the daemon's ten default rows
/// (`vike_tradehub::wired_markets::WIRED_MARKETS` in a build with no optional venue). This crate
/// names no venue and holds no table (docs/decisions/0098), so the live-path rule is tested over
/// this copy; `crates/vike-tradehub/src/wired_markets.rs`'s
/// `the_xemm_v1_pair_is_wired_in_the_real_table` asserts the v1 pair against the real one.
const WIRED: &[WiredMarket] = &[
    WiredMarket { venue: "binance", symbol: "BTCUSDT", reconnect_poke: true, engine_rank: 0 },
    WiredMarket { venue: "bybit", symbol: "BTCUSDT", reconnect_poke: true, engine_rank: 1 },
    WiredMarket { venue: "okx", symbol: "BTC-USDT-SWAP", reconnect_poke: true, engine_rank: 2 },
    WiredMarket {
        venue: "deribit",
        symbol: "BTC-PERPETUAL",
        reconnect_poke: false,
        engine_rank: 5,
    },
    WiredMarket { venue: "hyperliquid", symbol: "BTC", reconnect_poke: true, engine_rank: 3 },
    WiredMarket { venue: "aster", symbol: "BTCUSDT.P", reconnect_poke: false, engine_rank: 4 },
    WiredMarket { venue: "alpaca", symbol: "AAPL", reconnect_poke: false, engine_rank: 6 },
    WiredMarket { venue: "ctrader", symbol: "EURUSD", reconnect_poke: false, engine_rank: 7 },
    WiredMarket {
        venue: "ig",
        symbol: "CS.D.EURUSD.MINI.IP",
        reconnect_poke: false,
        engine_rank: 8,
    },
    WiredMarket { venue: "oanda", symbol: "EURUSD", reconnect_poke: false, engine_rank: 9 },
];

/// BOTH paper books this mount stands up arm the operator HALT sentinel.
///
/// ⚠ This mount was the counter-example to a claim the tree made in writing.
/// `crates/vike-paper/src/lib.rs`'s `halt_path` doc said `crate::paper_client` and
/// `paper_client_for` "each assert on this, so adding a third mount seam that forgets to arm
/// would fail" — and `build_paper_xemm_core` was already that third seam, unarmed, when the
/// sentence was written. Two per-crate assertions are not a gate over the SET of seams; that is
/// `crates/vike-ops/tests/paper_mount_arming_gate.rs`. This test is the per-crate half for THIS
/// seam: drop either `.with_halt_path(..)` in `xemm_paper_books` and it goes red.
///
/// Both legs are asserted separately. A halt that stopped the maker's quotes while still letting
/// hedges out would leave the rehearsal drifting inventory in a way the live mount never would,
/// so a half-armed pair is its own failure mode rather than a partial fix.
/// It also pins WHICH sentinel the default resolves — the process-wide one. "Armed" alone stops
/// being enough once [`PaperHalt`] makes the choice a parameter: a mount armed at a path no
/// operator knows about has no kill switch, and an `is_some()` check cannot tell the two apart.
#[test]
fn the_xemm_paper_mount_arms_both_books() {
    let process_wide = vike_bridge_core::halt::halt_path_from_env();
    let (maker, hedge) = xemm_paper_books(&v1(), &PaperHalt::ProcessWide);
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

/// A PINNED sentinel reaches BOTH books — the seam `crates/vike-mount/tests/xemm_scripted.rs`
/// needs so its scripted fills do not depend on whether the box running it holds a HALT file.
#[test]
fn a_pinned_sentinel_reaches_both_xemm_books() {
    let pinned = std::path::PathBuf::from("xemm-tests-own-this-sentinel-and-never-make-it/HALT");
    let (maker, hedge) = xemm_paper_books(&v1(), &PaperHalt::Pinned(pinned.clone()));
    assert_eq!(maker.halt_path(), Some(pinned.as_path()), "the maker leg must be pinned");
    assert_eq!(hedge.halt_path(), Some(pinned.as_path()), "the hedge leg must be pinned too");
}

/// The v1 pair validates, and resolves the 6.5 bp round-trip fee (hyperliquid maker 1.5 +
/// okx taker 5.0) bit-for-bit — that number IS the maker's break-even offset.
#[test]
fn the_v1_pair_validates_and_resolves_six_and_a_half_bps() {
    let fee = v1().validate(Some(WIRED)).expect("the v1 pair is wired and fee-expressible");
    assert_eq!(fee.to_bits(), (1.5_f64 / 10_000.0 + 5.0 / 10_000.0).to_bits());
}

/// THE HIJACK GUARD. Same-symbol legs are refused, which rules out every same-ticker CEX pair
/// (binance `BTCUSDT` × bybit `BTCUSDT`) — the pairing an operator would reach for first.
#[test]
fn a_same_symbol_pair_is_refused() {
    let mut cfg = v1();
    cfg.taker_venue = "bybit".into();
    cfg.hedge_symbol = cfg.maker_symbol.clone();
    assert_eq!(cfg.validate(None), Err(XemmConfigError::SameSymbol("BTC".into())));
}

/// Two legs on one venue is not a cross-exchange maker, and the reference lane (which requires
/// `m.venue != venue`) would never deliver a tick.
#[test]
fn a_same_venue_pair_is_refused() {
    let mut cfg = v1();
    cfg.taker_venue = cfg.maker_venue.clone();
    assert_eq!(cfg.validate(None), Err(XemmConfigError::SameVenue("hyperliquid".into())));
}

/// An unwired taker venue is a STARTUP error on the LIVE path: `apply_intent`'s
/// `unwrap_or(0)` would otherwise route the hedge to the MAKER engine, doubling the exposure
/// with no error anywhere.
///
/// The PAPER path builds its own two engines and so does not consult the wired table at all —
/// which is observable here as a DIFFERENT refusal for the same config (an unknown venue's fee
/// schedule is `Free`, which has no flat fraction). Two distinct errors for one config is the
/// proof that the wiring check is gated on `Some(wired)` rather than always-on.
#[test]
fn an_unwired_taker_venue_is_refused_on_the_live_path_only() {
    let mut cfg = v1();
    cfg.taker_venue = "kalshi".into();
    assert_eq!(
        cfg.validate(Some(WIRED)),
        Err(XemmConfigError::TakerVenueNotWired("kalshi".into()))
    );
    assert!(
        matches!(cfg.validate(None), Err(XemmConfigError::FeeNotExpressible { .. })),
        "paper skips the wiring check and refuses on the fee shape instead"
    );
}

/// The taker venue's engine trades ONE hardcoded symbol and `make_engine` sets no
/// `extra_symbols`, so a hedge symbol that venue is not wired for would be dropped at the
/// engine boundary. Refuse it by name.
#[test]
fn a_hedge_symbol_the_taker_engine_does_not_accept_is_refused() {
    let mut cfg = v1();
    cfg.hedge_symbol = "ETH-USDT-SWAP".into();
    assert_eq!(
        cfg.validate(Some(WIRED)),
        Err(XemmConfigError::HedgeSymbolNotAccepted {
            venue: "okx".into(),
            wired: "BTC-USDT-SWAP".into(),
            requested: "ETH-USDT-SWAP".into(),
        })
    );
}

/// A leg whose fee SHAPE has no flat fraction is refused rather than defaulted to `0.0` — which
/// would price a fee-bearing round trip as free and rest every quote inside break-even. Both
/// leg positions are checked.
#[test]
fn a_fee_shape_without_a_flat_rate_is_refused_on_either_leg() {
    // oanda is `Free` (spread-charging) — a shape, not a value.
    let mut taker_free = v1();
    taker_free.taker_venue = "oanda".into();
    taker_free.hedge_symbol = "EURUSD".into();
    assert!(matches!(
        taker_free.validate(Some(WIRED)),
        Err(XemmConfigError::FeeNotExpressible { .. })
    ));
    // deribit is `PercentOfUnderlying`: its flat reading UNDERSTATES the real fee.
    let mut maker_deribit = v1();
    maker_deribit.maker_venue = "deribit".into();
    maker_deribit.maker_symbol = "BTC-PERPETUAL".into();
    assert!(matches!(
        maker_deribit.validate(Some(WIRED)),
        Err(XemmConfigError::FeeNotExpressible { .. })
    ));
}

/// The degenerate tunings that would make the maker either donate fees or refuse every tick.
#[test]
fn a_zero_edge_a_sub_tick_standoff_and_a_zero_size_are_each_refused() {
    let mut no_edge = v1();
    no_edge.min_profitability = 0.0;
    no_edge.maker_venue = "polymarket".into(); // Free ⇒ …but that refuses on fees first
    no_edge.maker_venue = "hyperliquid".into();
    // With a real fee the edge is positive even at zero profitability, so force it negative.
    no_edge.min_profitability = -1.0;
    assert_eq!(no_edge.validate(None), Err(XemmConfigError::NoEdge));

    let mut sub_tick = v1();
    sub_tick.min_edge_ticks = 0.5;
    assert_eq!(sub_tick.validate(None), Err(XemmConfigError::NoStandoff));
    let mut no_grid = v1();
    no_grid.maker_tick_size = 0.0;
    assert_eq!(no_grid.validate(None), Err(XemmConfigError::NoStandoff));

    let mut no_size = v1();
    no_size.qty = 0.0;
    assert_eq!(no_size.validate(None), Err(XemmConfigError::NoSize));
}

/// The mount declares exactly ONE leg — the hedge leg, on the taker venue. Declaring the
/// maker's own symbol as well would hijack every symbol-less intent (see [`XemmConfigError`]).
#[test]
fn the_mount_declares_only_the_hedge_leg() {
    let legs = xemm_mount_legs(&v1());
    assert_eq!(legs, vec![MountLeg::at("BTC-USDT-SWAP", "okx")]);
}

/// The builder threads every named knob into the maker and leaves the ARMED defaults where a
/// knob was not named — the polarity inversion `XemmParams` documents.
#[test]
fn the_builder_threads_named_knobs_and_keeps_armed_defaults() {
    let mut cfg = v1();
    cfg.basis_band = Some((40.0, 30_000, 0.05));
    cfg.naked_bands = Some((0.02, 0.05));
    cfg.breaker = Some((1_000, 2.5, 5_000));
    cfg.hedge_ratio = 1.0;
    let fee = cfg.validate(Some(WIRED)).expect("valid");
    let m = build_xemm_maker(&cfg, fee);
    let p = m.params();
    assert_eq!(m.legs(), ("BTC", "BTC-USDT-SWAP"));
    assert_eq!(p.total_fee.to_bits(), fee.to_bits());
    assert_eq!(p.max_basis_bps.to_bits(), 40.0_f64.to_bits());
    assert_eq!(p.naked_hard_band.to_bits(), 0.05_f64.to_bits());
    assert_eq!(p.net_fill_threshold.to_bits(), 2.5_f64.to_bits());
    // unnamed: the ARMED defaults, not neutral ones.
    assert_eq!(p.max_ref_age_ms, 2_000);
    assert_eq!(p.hedge_max_attempts, 3);
    assert_eq!(p.resume_after_halt_ms, 0, "auto-resume stays manual");
}

/// The paper rehearsal stands up TWO engines over TWO single-symbol books and refuses a bad
/// config before spawning anything.
#[test]
fn the_paper_rehearsal_spawns_two_engines_and_refuses_a_bad_config() {
    let mount = build_paper_xemm_core(&v1()).expect("the v1 pair is valid");
    assert!(mount.maker_fills.lock().unwrap().is_empty());
    assert!(mount.hedge_fills.lock().unwrap().is_empty());
    mount.handle.shutdown_and_join();

    let mut bad = v1();
    bad.hedge_symbol = bad.maker_symbol.clone();
    assert!(build_paper_xemm_core(&bad).is_err(), "a hijacking config never spawns");
}
