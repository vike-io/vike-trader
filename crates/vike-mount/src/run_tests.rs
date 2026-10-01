use super::*;
// `super::*` brings in what `run.rs` imports; the node item this module also names is imported
// from its defining module, as vike-run's crate root used to bring it in (docs/decisions/0098).
use crate::node::mount_accounts;

/// **The account an operator NAMED survives every lowering between a [`MountSpec`] and the
/// core** — the three field-for-field copies that stand between a profile row and
/// `vike_core::StrategyMount::account`.
///
/// [`fold_strategy_mount`] (both single-mount builders), [`StrategyMountSpec::into_mount`] (the
/// multi-mount one) and [`mount_accounts`] (what [`build_node`]'s per-account fan-out and
/// `refuse_unarmed_mount_accounts` BOTH read the operator's choice back off) each copy the
/// field and each can drop it in one character. The result of dropping it is not a compile
/// error and not a wrong answer anywhere visible: the mount comes up on the venue's DEFAULT
/// account while the profile says `account = "ALT"`, trades the wrong book, and refuses
/// nothing on the way — because with the field gone there is no named account left to refuse.
///
/// ⚠ It needed writing because every account-aware test in this workspace enters the graph
/// DOWNSTREAM of all three copies: `vike-core`'s build their `StrategyMount` by hand and
/// `vike-tradehub`'s stop at `MountSpec`. Setting any of the three `account:` lines to `None`
/// was measured GREEN across `vike-run`, `vike-tradehub` AND `vike-core` before this test
/// existed. Do that again and this goes red, naming which copy dropped it.
#[test]
fn the_account_survives_every_lowering_from_a_mount_spec_into_the_core() {
    let alt = vike_model::account_keys::AccountLabel::parse("ALT").expect("a legal label");
    let cfg = MakerMountConfig::outcome_token("polymarket", "tok", None);
    let spec = MountSpec { account: Some(alt.clone()), ..cfg.mount_spec() };

    // (1) the MULTI-mount lowering.
    let lowered = StrategyMountSpec { strategy: Box::new(build_maker(&cfg)), spec: spec.clone() }
        .into_mount();
    assert_eq!(
        lowered.account.as_ref(),
        Some(&alt),
        "`StrategyMountSpec::into_mount` must carry the operator's account into the core — \
             dropping it here mounts every multi-mount row on its venue's DEFAULT account"
    );

    // (2) the SINGLE-mount lowering, through the config `build_node` is about to consume.
    let mut node_cfg = NodeConfig {
        registry: &[],
        markets: &[],
        vars: std::collections::HashMap::new(),
        properties_rec: None,
        seed_cash: 10_000.0,
        recon_enabled: false,
        core_config: vike_core::CoreConfig::default(),
        risk_profile: None,
        policy: crate::MountPolicy::default(),
    };
    fold_strategy_mount(&mut node_cfg, Box::new(build_maker(&cfg)), &spec);
    assert_eq!(
        node_cfg.core_config.strategy.as_ref().expect("the mount was folded").account.as_ref(),
        Some(&alt),
        "`fold_strategy_mount` must carry the operator's account into the core config — \
             dropping it here mounts the single-mount builders on the venue's DEFAULT account"
    );

    // (3) …and the read-back the fan-out and the arming refusal both select on. This is the
    // copy whose loss is worst: the mount would still be resolved correctly by `vike_core`,
    // and `refuse_unarmed_mount_accounts` would see no named account to check, so an UNARMED
    // account would stop being refused at the same time.
    node_cfg.core_config.extra_mounts.push(lowered);
    let rows = mount_accounts(&node_cfg.core_config);
    assert_eq!(rows.len(), 2, "one row per mount — the folded one and the extra one");
    for r in &rows {
        assert_eq!(
            r.account.as_ref(),
            Some(&alt),
            "`mount_accounts` must report the account each mount named: it is what \
                 `account_symbols_for` arms that account's engine on and what \
                 `refuse_unarmed_mount_accounts` checks against the arming table"
        );
    }
}

/// The paper book this crate mounts arms the operator HALT sentinel.
///
/// ⚠ This is the seam `vike-tradehub`'s shipped PAPER daemon runs, and it does not go through
/// `crate::make_engine` at all — so arming the eleven fallback arms over there reached this
/// node not at all. `touch <project>/settings/state/HALT` on the box did nothing, silently, on precisely the
/// mount an operator uses to rehearse the switch. Drop the `.with_halt_path(..)` from
/// `paper_client_for` and this goes red.
///
/// Both fee paths are covered: an explicit non-zero fee (the `new` branch) and the registry
/// schedule (the `with_fee_schedule` branch), because the arming sits after the branch and a
/// refactor that moved it inside one arm would otherwise half-disarm the mount.
///
/// It also pins the PATH, not merely its presence: the default must be the PROCESS-WIDE
/// sentinel. Once [`PaperHalt`] made the choice a parameter, "armed" stopped being enough —
/// a mount armed at a path no operator knows about is a mount with no kill switch, and
/// flipping `PaperHalt`'s `#[default]` would otherwise pass an `is_some()` check.
#[test]
fn the_paper_maker_mount_is_halt_armed_on_both_fee_paths() {
    let process_wide = vike_bridge_core::halt::halt_path_from_env();

    let registry = MakerMountConfig::outcome_token("polymarket", "tok", None);
    assert_eq!(
        paper_client_for(&registry.mount_spec(), &PaperHalt::ProcessWide).halt_path(),
        Some(process_wide.as_path()),
        "the registry-fee paper mount must arm the PROCESS-WIDE HALT sentinel"
    );

    let explicit = MakerMountConfig {
        maker_fee: 0.001,
        ..MakerMountConfig::outcome_token("polymarket", "tok", None)
    };
    assert_eq!(
        paper_client_for(&explicit.mount_spec(), &PaperHalt::ProcessWide).halt_path(),
        Some(process_wide.as_path()),
        "the explicit-fee paper mount must arm it too — the arming must not sit inside one fee \
             branch"
    );

    assert_eq!(
        PaperMountOpts::default().halt,
        PaperHalt::ProcessWide,
        "the options bag's DEFAULT must be the armed one: `vike-tradehub`'s shipped paper \
             daemon builds it with `..Default::default()`, so a flip here silently disarms the \
             operator's kill switch on a live node"
    );
}

/// A PINNED sentinel is the one the book watches, and it is the seam every test in this crate
/// that expects a fill must use.
///
/// ⚠ Without this parameter, five tests in this crate flipped red on any box holding a HALT
/// file — see [`PaperHalt`]'s doc for the the CI box measurement. Point `PaperHalt::resolve`'s
/// `Pinned` arm back at `halt_path_from_env()` and this goes red on EVERY box, sentinel or not,
/// which is the property the earlier proof lacked.
#[test]
fn a_pinned_sentinel_is_the_one_the_book_watches() {
    let pinned = PathBuf::from("a-path-this-test-owns-and-never-creates/HALT");
    let cfg = MakerMountConfig::outcome_token("polymarket", "tok", None);
    assert_eq!(
        paper_client_for(&cfg.mount_spec(), &PaperHalt::Pinned(pinned.clone())).halt_path(),
        Some(pinned.as_path()),
        "a pinned mount must watch the caller's path and nothing else"
    );
}

#[test]
fn synth_closes_a_bar_on_the_window_boundary_with_correct_ohlc() {
    let mut s = TickBarSynthesizer::new(60_000);
    // window 0: 0.50 -> 0.40 -> 0.50 (a dip-and-recover); no close yet.
    assert!(s.on_price(1_000, 0.50).is_none());
    assert!(s.on_price(2_000, 0.40).is_none());
    assert!(s.on_price(3_000, 0.50).is_none());
    // a tick in window 1 closes window 0.
    let bar = s.on_price(61_000, 0.50).expect("window 0 closes");
    assert_eq!(bar.ts, 60_000); // (0 + 1) * interval
    assert_eq!(bar.open.to_bits(), 0.50f64.to_bits());
    assert_eq!(bar.high.to_bits(), 0.50f64.to_bits());
    assert_eq!(bar.low.to_bits(), 0.40f64.to_bits()); // the dip
    assert_eq!(bar.close.to_bits(), 0.50f64.to_bits());
    // window 1 is open now; flush closes it.
    let last = s.flush().expect("window 1 flushes");
    assert_eq!(last.ts, 120_000);
    assert!(s.flush().is_none()); // idempotent once drained
}

// --- liquidity-rewards mount wiring (steal/rewards-mount-wiring) ---------------------------

// OFF/default: the recommended Polymarket mount config carries NO reward params, and the maker
// it builds has reward-aware quoting OFF — byte-identical to a pre-rewards mount. This is the
// explicit default-does-not-enable-rewards proof; the opt-in is a caller setting `reward`, never
// the default mount.
#[test]
fn default_polymarket_mount_leaves_rewards_off() {
    let cfg = MakerMountConfig::outcome_token("polymarket", "TOK", None);
    assert!(cfg.reward.is_none(), "default mount config must not carry reward params");
    let maker = build_maker(&cfg);
    assert!(maker.params().reward.is_none(), "default mount must not enable reward-aware quoting");
}

// When the config DOES carry reward params (a caller's opt-in), the mount folds them onto the
// maker verbatim — the actual wiring this feature adds.
#[test]
fn mount_applies_configured_reward_params_to_the_maker() {
    let reward = RewardParams {
        weight: 0.5,
        max_spread_cents: 3.0,
        min_size: 100.0,
        min_order_age_ms: 30_000,
    };
    let mut cfg = MakerMountConfig::outcome_token("polymarket", "TOK", None);
    cfg.reward = Some(reward);
    let maker = build_maker(&cfg);
    assert_eq!(maker.params().reward, Some(reward), "the mount must apply cfg.reward to the maker");
}

// --- 5c flow-toxicity mount wiring -----------------------------------------------------------

// OFF/default: the recommended Polymarket mount config carries NO toxicity params, and the maker
// it builds has the flow-toxicity guard OFF — byte-identical to a pre-toxicity mount. The opt-in
// lives in the bin's `--toxicity` flag, never in the default mount.
#[test]
fn default_polymarket_mount_leaves_toxicity_off() {
    let cfg = MakerMountConfig::outcome_token("polymarket", "TOK", None);
    assert!(cfg.toxicity.is_none(), "default mount config must not carry toxicity params");
    let maker = build_maker(&cfg);
    assert!(
        maker.params().toxicity.is_none(),
        "default mount must not enable the flow-toxicity guard"
    );
}

// When the config DOES carry toxicity params (as the bin's `--toxicity` opt-in sets), the mount
// folds them onto the maker verbatim — the actual wiring this feature adds.
#[test]
fn mount_applies_configured_toxicity_params_to_the_maker() {
    let tox = ToxicityParams { widen: 1.0, size_cut: 0.5 };
    let mut cfg = MakerMountConfig::outcome_token("polymarket", "TOK", None);
    cfg.toxicity = Some(tox);
    let maker = build_maker(&cfg);
    assert_eq!(
        maker.params().toxicity,
        Some(tox),
        "the mount must apply cfg.toxicity to the maker"
    );
}

// --- F9 mount reachability: skew / breaker / refresh tolerance -------------------------------

// OFF/default: NEITHER constructor carries the F9 opt-ins, and the maker each builds keeps the
// neutral skew, a disengaged breaker, and NO refresh-tolerance bag — byte-identical to a
// pre-F9 mount. This is the explicit default-does-not-enable proof, mirroring the reward /
// toxicity twins above.
#[test]
fn default_mounts_leave_skew_breaker_and_refresh_off() {
    for cfg in [
        MakerMountConfig::outcome_token("polymarket", "TOK", None),
        MakerMountConfig::crypto("hyperliquid", "BTC", 1.0, 0.005),
    ] {
        assert!(
            cfg.skew.is_none() && cfg.breaker.is_none() && cfg.refresh_tolerance.is_none(),
            "default mount config must not carry the F9 opt-ins ({})",
            cfg.venue
        );
        let p = build_maker(&cfg).params();
        assert_eq!(p.target_inventory, 0.0, "neutral skew target ({})", cfg.venue);
        assert_eq!(p.max_inventory, 1.0, "neutral skew band ({})", cfg.venue);
        assert_eq!(p.skew, 0.0, "skew intensity off ({})", cfg.venue);
        assert_eq!(p.fill_window_ms, 0, "breaker window off ({})", cfg.venue);
        assert_eq!(p.net_fill_threshold, 0.0, "breaker threshold off ({})", cfg.venue);
        assert_eq!(p.suppress_cooldown_ms, 0, "breaker cooldown off ({})", cfg.venue);
        assert!(p.refresh_tolerance.is_none(), "no tolerance bag at all ({})", cfg.venue);
    }
}

// When the config DOES carry the F9 opt-ins, the mount threads each through its builder onto
// the maker verbatim — the actual reachability wiring this feature adds.
#[test]
fn mount_applies_configured_skew_breaker_and_refresh_to_the_maker() {
    let mut cfg = MakerMountConfig::outcome_token("polymarket", "TOK", None);
    cfg.skew = Some(MakerSkew { target_inventory: 5.0, max_inventory: 40.0, skew: 0.6 });
    cfg.breaker = Some(MakerBreaker {
        fill_window_ms: 5_000,
        net_fill_threshold: 60.0,
        suppress_cooldown_ms: 10_000,
    });
    cfg.refresh_tolerance = Some(RefreshTolerance { price_bps: 25.0, size_bps: 50.0 });
    let p = build_maker(&cfg).params();
    assert_eq!(p.target_inventory, 5.0);
    assert_eq!(p.max_inventory, 40.0);
    assert_eq!(p.skew, 0.6);
    assert_eq!(p.fill_window_ms, 5_000);
    assert_eq!(p.net_fill_threshold, 60.0);
    assert_eq!(p.suppress_cooldown_ms, 10_000);
    assert_eq!(p.refresh_tolerance, Some(RefreshTolerance { price_bps: 25.0, size_bps: 50.0 }));
}

// --- the outcome-token preset (`MakerMountConfig::polymarket` until docs/decisions/0098) ------

/// The outcome-token preset is a PRICE-DOMAIN preset: the venue is the caller's, and nothing
/// else about the preset depends on it (docs/decisions/0098 -- this crate names no venue).
#[test]
fn the_outcome_token_preset_takes_its_venue_from_the_caller() {
    let a = MakerMountConfig::outcome_token("planted-a", "TOK", Some(7));
    let b = MakerMountConfig::outcome_token("planted-b", "TOK", Some(7));
    assert_eq!(a.venue, "planted-a");
    assert_eq!(b.venue, "planted-b");
    assert_eq!(
        format!("{:?}", MakerMountConfig { venue: String::new(), ..a }),
        format!("{:?}", MakerMountConfig { venue: String::new(), ..b })
    );
}

// --- crypto-domain mount (the A-S price-domain generalization) ------------------------------

// The `::crypto` twin tunes the A-S layer for an UNBOUNDED $-scale asset: RawLocal variance (the
// Bernoulli cap is meaningless above 1.0), ConstantTau horizon (never resolves), the Unbounded
// price domain (no [tick,1−tick] wall clamp), and a 2-tick min-half-spread floor (the sub-tick
// $-scale spread guard). venue/token/tick/qty thread straight through, and the maker builds.
#[test]
fn crypto_mount_sets_the_unbounded_dollar_scale_as_params() {
    let cfg = MakerMountConfig::crypto("hyperliquid", "BTC", 1.0, 0.005);
    assert_eq!(cfg.venue, "hyperliquid");
    assert_eq!(cfg.token_id, "BTC");
    assert_eq!(cfg.tick_size.to_bits(), 1.0_f64.to_bits());
    assert_eq!(cfg.qty.to_bits(), 0.005_f64.to_bits());
    let a = cfg.as_params;
    assert_eq!(a.variance_mode, VarianceMode::RawLocal, "raw local vol (no Bernoulli cap)");
    assert_eq!(a.horizon_mode, HorizonMode::ConstantTau, "open-ended horizon");
    assert_eq!(a.price_domain, PriceDomain::Unbounded, "no wall clamp");
    assert_eq!(a.min_half_spread_ticks.to_bits(), 2.0_f64.to_bits(), "2-tick floor");
    assert_eq!(a.max_half_spread_ticks.to_bits(), 60.0_f64.to_bits(), "60-tick spike ceiling");
    // PM rewards / toxicity / underlying anchor stay OFF on a plain $-scale mount.
    assert!(cfg.reward.is_none() && cfg.toxicity.is_none() && cfg.underlying_symbol.is_none());
    let _maker = build_maker(&cfg); // A-S enabled → builds without panicking
}

// --- the break-even fee floor: what the MOUNT arms, and what it refuses to invent -------------

/// The crypto mount ARMS the A-S break-even floor from the venue's own schedule, LANE-keyed —
/// so the maker's break-even bar and its paper book's fills read the same numbers. Pinned on
/// the three venues that make the lane split matter.
#[test]
fn the_crypto_mount_arms_the_break_even_floor_from_the_venue_fee_schedule() {
    // bybit VIP0: 2.0 bps maker × 2 legs = 4 bps round trip — the the CI box mount's real number.
    let bybit = MakerMountConfig::crypto("bybit", "BTCUSDT", 0.1, 0.001);
    assert_eq!(
        bybit.as_params.round_trip_fee_rate.map(f64::to_bits),
        Some((2.0_f64 / 1e4 + 2.0_f64 / 1e4).to_bits()),
        "bybit arms 4 bps"
    );
    // The LANE split is honoured: a `.P` binance symbol is the PERP lane (2 bps maker), a bare
    // one is SPOT (10 bps maker) — five times the floor. Reading the bare id for both would
    // under-floor every perp maker on the venue.
    let perp = MakerMountConfig::crypto("binance", "BTCUSDT.P", 0.1, 0.001);
    let spot = MakerMountConfig::crypto("binance", "BTCUSDT", 0.01, 0.001);
    assert_eq!(perp.as_params.round_trip_fee_rate.map(f64::to_bits), Some(4e-4_f64.to_bits()));
    assert_eq!(spot.as_params.round_trip_fee_rate.map(f64::to_bits), Some(2e-3_f64.to_bits()));
    assert_eq!(
        perp.as_params.round_trip_fee_rate,
        vike_model::maker_round_trip_fee(vike_model::fee_schedule_for(vike_catalog::fee_lane(
            "binance",
            "BTCUSDT.P"
        ))),
        "the mount reads exactly what the lane-keyed registry says — no second derivation"
    );
}

/// ⚠ A venue whose fee SHAPE has no flat fraction of price arms NOTHING — never a silent `0.0`.
/// hyperliquid (the shipped `$`-scale mount) has a percent schedule and IS floored; an FX venue
/// that charges through the spread is not, and `maker_round_trip_fee_for` says so at `warn`.
#[test]
fn a_venue_with_no_flat_fee_rate_arms_no_floor_rather_than_a_zero_one() {
    let hl = MakerMountConfig::crypto("hyperliquid", "BTC", 1.0, 0.005);
    assert_eq!(
        hl.as_params.round_trip_fee_rate.map(f64::to_bits),
        Some(3e-4_f64.to_bits()),
        "hyperliquid VIP0 is 1.5 bps maker ⇒ a 3 bps round trip"
    );
    for spread_charging in ["oanda", "ig", "dukascopy", "ctrader"] {
        let c = MakerMountConfig::crypto(spread_charging, "EURUSD", 0.00001, 1_000.0);
        assert_eq!(
            c.as_params.round_trip_fee_rate, None,
            "{spread_charging} charges through the SPREAD: an absent bar, not a zero fee"
        );
    }
    // ...and the polymarket mount arms nothing either — its real fee is the price-dependent
    // p(1−p) curve, which this scalar cannot express (see `maker_round_trip_fee`'s refusal set).
    assert_eq!(
        MakerMountConfig::outcome_token("polymarket", "TOK", None).as_params.round_trip_fee_rate,
        None
    );
}

// --- fee model follow-up 3: the paper client's fee source (registry vs. config override) ---
use vike_exec::ExecutionClient;
use vike_model::OrderRequest;

fn fee_cfg(venue: &str, maker_fee: f64, taker_fee: f64) -> MakerMountConfig {
    let mut c = MakerMountConfig::outcome_token(venue, "TOK", None);
    c.slippage = 0.0;
    c.maker_fee = maker_fee;
    c.taker_fee = taker_fee;
    c
}

/// Drive one market (taker) fill through the client `paper_client_for` builds and read its fee.
///
/// ⚠ **The book is PINNED to a sentinel this test owns and never creates, and that is the whole
/// point.** This helper submits a plain (non-`reduce_only`) OPENING order, which a HALT-armed
/// book refuses — producing no fill, so `assert_eq!(fills.len(), 1, ..)` below panics with a fee
/// message that never mentions halt. When this passed `PaperHalt::ProcessWide` (the
/// PROCESS-WIDE mount seam), both callers failed on any box holding
/// `<project>/settings/state/HALT` — the exact file an operator touches on a
/// live node — and CI was green only because no runner happened to have one.
///
/// The `halt_path` assertion below is what makes the guard ENVIRONMENT-INDEPENDENT: re-point
/// this at `PaperHalt::ProcessWide` and it fires on every box, sentinel or not, instead of only
/// where the file happens to exist.
fn taker_fee_of(cfg: &MakerMountConfig, qty: f64, px: f64) -> f64 {
    let pinned = PathBuf::from("vike-mount-fee-tests-own-this-sentinel-and-never-create-it/HALT");
    let mut c = paper_client_for(&cfg.mount_spec(), &PaperHalt::Pinned(pinned.clone()));
    assert_eq!(
        c.halt_path(),
        Some(pinned.as_path()),
        "this fee helper must drive a book whose sentinel IT owns. Through the process-wide \
             mount seam, an operator's HALT file refuses the opening submit below and the fee \
             assertion fails as a fee mismatch on any box that has one"
    );
    assert!(
        !pinned.exists(),
        "the pinned sentinel must not exist, or the submit below is refused for the reason \
             this assertion exists to rule out"
    );
    c.submit(&OrderRequest {
        client_order_id: "m".into(),
        venue: cfg.venue.clone(),
        symbol: cfg.token_id.clone(),
        side: 1,
        qty,
        order_type: "market".into(),
        ..Default::default()
    });
    c.on_bar(&Bar {
        ts: 1,
        open: px,
        high: px,
        low: px,
        close: px,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    });
    let fills = c.fills.lock().unwrap();
    assert_eq!(fills.len(), 1, "the market order fills at next open");
    fills[0].fee
}

#[test]
fn paper_fees_default_to_the_venue_registry_schedule() {
    // Polymarket registry = Free ⇒ zero fee (the migrated default for the real mount).
    assert_eq!(taker_fee_of(&fee_cfg("polymarket", 0.0, 0.0), 20.0, 0.5), 0.0);
    // A venue with a non-Free registry schedule proves the registry (not a hardcoded 0) is used:
    // binance taker = 10 bps.
    assert_eq!(
        taker_fee_of(&fee_cfg("binance", 0.0, 0.0), 2.0, 100.0),
        2.0 * 100.0 * (10.0 / 10_000.0)
    );
}

#[test]
fn explicit_profile_fee_override_still_wins() {
    // A non-zero config fee keeps the flat-rate path even on a venue with a registry schedule —
    // the user-facing fee knobs are preserved.
    assert_eq!(taker_fee_of(&fee_cfg("binance", 0.0002, 0.0007), 2.0, 100.0), 2.0 * 100.0 * 0.0007);
}
