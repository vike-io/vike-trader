//! `run`'s unit tests: account lowering, and the halt and fee lane a run mount arms.

use super::*;
// `super::*` is `run.rs`'s imports; the node item below comes from its defining module.
use crate::node::runnable_accounts;
use crate::test_support::paper_maker::{outcome_maker, outcome_spec};
use vike_strategy_lab::makers::spread_maker::SpreadMaker;

/// **The account an operator NAMED survives every lowering from a [`MountPlacement`] to
/// `vike_core::Mount::account`.**
///
/// [`fold_strategy_mount`] (single-mount builders), [`MountBuild::into_mount`]
/// (multi-mount) and [`runnable_accounts`] (read back by [`assemble_runtime`]'s per-account fan-out AND
/// `refuse_unarmed_mount_accounts`) each copy the field and can drop it in one character —
/// silently: the mount trades the venue's DEFAULT account while the profile says
/// `account = "ALT"`, with no named account left to refuse.
///
/// ⚠ Every other account-aware test enters DOWNSTREAM of all three copies (`vike-core`'s build
/// `Mount` by hand, `vike-tradehub`'s stop at `MountPlacement`): setting any `account:` line
/// to `None` was measured GREEN across `vike-run`, `vike-tradehub` AND `vike-core`. This goes
/// red, naming which copy dropped it.
#[test]
fn the_account_survives_every_lowering_from_a_mount_spec_into_the_core() {
    let alt =
        vike_model::accounts::account_keys::AccountLabel::parse("ALT").expect("a legal label");
    let spec = MountPlacement { account: Some(alt.clone()), ..outcome_spec("polymarket", "tok") };

    // (1) the MULTI-mount lowering.
    let lowered = MountBuild {
        strategy: Box::new(outcome_maker(SpreadMaker::from_params, None)),
        spec: spec.clone(),
    }
    .into_mount();
    assert_eq!(
        lowered.account.as_ref(),
        Some(&alt),
        "`MountBuild::into_mount` must carry the operator's account into the core — \
             dropping it here mounts every multi-mount row on its venue's DEFAULT account"
    );

    // (2) the SINGLE-mount lowering, through the config `assemble_runtime` is about to consume.
    let mut runtime_config = RuntimeConfig {
        backend: std::sync::Arc::new(crate::VenueBackendPaper::new(
            &crate::AdapterPolicy::default().accounts,
        )),
        properties_rec: None,
        seed_cash: 10_000.0,
        reconcile_enabled: false,
        core_config: vike_core::CoreConfig::default(),
        risk_profile: None,
        policy: crate::AdapterPolicy::default(),
    };
    fold_strategy_mount(
        &mut runtime_config,
        Box::new(outcome_maker(SpreadMaker::from_params, None)),
        &spec,
    );
    assert_eq!(
        runtime_config
            .core_config
            .strategy
            .as_ref()
            .expect("the mount was folded")
            .account
            .as_ref(),
        Some(&alt),
        "`fold_strategy_mount` must carry the operator's account into the core config — \
             dropping it here mounts the single-mount builders on the venue's DEFAULT account"
    );

    // (3) …and the read-back the fan-out and the arming refusal select on: the worst loss, since
    // `vike_core` still resolves the mount while `refuse_unarmed_mount_accounts` sees no named
    // account, so an UNARMED account stops being refused too.
    runtime_config.core_config.extra_mounts.push(lowered);
    let rows = runnable_accounts(&runtime_config.core_config);
    assert_eq!(rows.len(), 2, "one row per mount — the folded one and the extra one");
    for r in &rows {
        assert_eq!(
            r.account.as_ref(),
            Some(&alt),
            "`runnable_accounts` must report the account each mount named: it is what \
                 `account_symbols_for` arms that account's engine on and what \
                 `refuse_unarmed_mount_accounts` checks against the arming table"
        );
    }
}

/// The paper book this crate mounts arms the operator HALT sentinel.
///
/// ⚠ `vike-tradehub`'s shipped PAPER daemon runs this seam, NOT `crate::make_engine`, so arming
/// the eleven fallback arms there never reached it: `touch <project>/settings/state/HALT` did
/// nothing, silently, on the mount an operator rehearses the switch on. Drop
/// `.with_halt_path(..)` from `paper_client_for` -> red.
///
/// Both fee paths (explicit fee = `new`, registry = `with_fee_schedule`): the arming sits after
/// the branch, and moving it inside one arm would half-disarm the mount.
///
/// It pins the PATH, not just presence: with [`StrategyCorePaperHalt`] a parameter, a mount armed at a path
/// no operator knows has no kill switch, and flipping `StrategyCorePaperHalt`'s `#[default]` would pass
/// `is_some()`.
#[test]
fn the_paper_maker_mount_is_halt_armed_on_both_fee_paths() {
    let process_wide = vike_boot::halt::halt_path_from_env();

    let registry = outcome_spec("polymarket", "tok");
    assert_eq!(
        paper_client_for(&registry, &StrategyCorePaperHalt::ProcessWide).halt_path(),
        Some(process_wide.as_path()),
        "the registry-fee paper mount must arm the PROCESS-WIDE HALT sentinel"
    );

    let explicit = MountPlacement { maker_fee: 0.001, ..outcome_spec("polymarket", "tok") };
    assert_eq!(
        paper_client_for(&explicit, &StrategyCorePaperHalt::ProcessWide).halt_path(),
        Some(process_wide.as_path()),
        "the explicit-fee paper mount must arm it too — the arming must not sit inside one fee \
             branch"
    );

    assert_eq!(
        StrategyCorePaperConfig::default().halt,
        StrategyCorePaperHalt::ProcessWide,
        "the options bag's DEFAULT must be the armed one: `vike-tradehub`'s shipped paper \
             daemon builds it with `..Default::default()`, so a flip here silently disarms the \
             operator's kill switch on a live node"
    );
}

/// A PINNED sentinel is the one the book watches: the seam every fill-expecting test here uses.
///
/// ⚠ Without it, five tests in this crate flipped red on any box holding a HALT file
/// ([`StrategyCorePaperHalt`]'s doc has the the CI box measurement). Point `StrategyCorePaperHalt::resolve`'s `Pinned` arm
/// back at `halt_path_from_env()` and this goes red on EVERY box, sentinel or not.
#[test]
fn a_pinned_sentinel_is_the_one_the_book_watches() {
    let pinned = PathBuf::from("a-path-this-test-owns-and-never-creates/HALT");
    let spec = outcome_spec("polymarket", "tok");
    assert_eq!(
        paper_client_for(&spec, &StrategyCorePaperHalt::Pinned(pinned.clone())).halt_path(),
        Some(pinned.as_path()),
        "a pinned mount must watch the caller's path and nothing else"
    );
}

fn quote(ts: i64, bid: f64, ask: f64) -> vike_marketdata::TickQuote {
    vike_marketdata::TickQuote {
        timestamp: ts,
        local_timestamp: 0,
        bid,
        ask,
        bid_size: 0.0,
        ask_size: 0.0,
        symbol: Default::default(),
    }
}

/// The mid bar a strategy reads, stamped at its window's close, and beside it each side's own path over
/// the same window, stamped at the window's START (a paper book's close fill reads the end off it): what
/// the paper book fills a buy (the ask) and a sell (the bid) on.
#[test]
fn synth_closes_a_bar_on_the_window_boundary_with_correct_ohlc_and_sides() {
    let mut s = TickBarSynthesizer::new(60_000);
    // window 0: mid 0.50 -> 0.40 -> 0.50 (a dip-and-recover); no close yet.
    assert!(s.on_quote(&quote(1_000, 0.49, 0.51)).is_none());
    assert!(s.on_quote(&quote(2_000, 0.38, 0.42)).is_none());
    assert!(s.on_quote(&quote(3_000, 0.48, 0.52)).is_none());
    // a tick in window 1 closes window 0.
    let (bar, sides) = s.on_quote(&quote(61_000, 0.49, 0.51)).expect("window 0 closes");
    let sides = sides.expect("window 0 was quoted");
    let ohlc = |b: &Bar| [b.open, b.high, b.low, b.close].map(f64::to_bits);
    assert_eq!(bar.timestamp, 60_000); // (0 + 1) * interval
    assert_eq!(ohlc(&bar), [0.50f64, 0.50, 0.40, 0.50].map(f64::to_bits), "the mid, the dip");
    assert_eq!(ohlc(&sides.bid), [0.49f64, 0.49, 0.38, 0.48].map(f64::to_bits), "the bid's path");
    assert_eq!(ohlc(&sides.ask), [0.51f64, 0.52, 0.42, 0.52].map(f64::to_bits), "the ask's path");
    assert_eq!((sides.bid.timestamp, sides.ask.timestamp), (0, 0), "the window's start");
    // window 1 is open now; flush closes it. Its side paths open at the quote that stood when it
    // began (window 0's last, 0.48 / 0.52), then take its own (0.49 / 0.51).
    let (last, last_sides) = s.flush().expect("window 1 flushes");
    let last_sides = last_sides.expect("window 1 was quoted");
    assert_eq!(last.timestamp, 120_000);
    assert_eq!(ohlc(&last_sides.bid), [0.48f64, 0.49, 0.48, 0.49].map(f64::to_bits));
    assert_eq!(ohlc(&last_sides.ask), [0.52f64, 0.52, 0.51, 0.51].map(f64::to_bits));
    assert!(s.flush().is_none()); // idempotent once drained
}

// ---- the static fee lane a maker's break-even floor is armed from ----
//
// The maker's presets, its opt-in bags and their defaults are vike-market-making's (`SpreadMakerPreset`, pinned
// by the maker file's `the_presets_hold_the_knobs_the_mount_presets_held`); what stays here is the
// VENUE half, the static lane `maker_round_trip_fee_for` reads.

/// The floor arms from the venue's LANE-keyed schedule, so the break-even bar and the paper fills
/// read the same numbers.
#[test]
fn the_static_lane_arms_the_break_even_floor_from_the_venue_fee_schedule() {
    // bybit VIP0: 2.0 bps maker × 2 legs = 4 bps round trip — the the CI box mount's real number, on the linear perpetual
    // (decision 0290's `BTCUSDT.P`; the bare `BTCUSDT` is the spot pair).
    assert_eq!(
        maker_round_trip_fee_for("bybit", "BTCUSDT.P").map(f64::to_bits),
        Some((2.0_f64 / 1e4 + 2.0_f64 / 1e4).to_bits()),
        "bybit arms 4 bps"
    );
    // The LANE split: binance `.P` is PERP (2 bps maker), bare is SPOT (10 bps, five times the
    // floor); reading the bare id for both would under-floor every perp maker.
    let perp = maker_round_trip_fee_for("binance", "BTCUSDT.P");
    assert_eq!(perp.map(f64::to_bits), Some(4e-4_f64.to_bits()));
    assert_eq!(
        maker_round_trip_fee_for("binance", "BTCUSDT").map(f64::to_bits),
        Some(2e-3_f64.to_bits())
    );
    assert_eq!(
        perp,
        vike_model::maker_round_trip_fee(vike_venues::fee_schedule_for(vike_venues::fee_lane(
            "binance",
            "BTCUSDT.P"
        ))),
        "the lane reads exactly what the lane-keyed registry says — no second derivation"
    );
}

/// ⚠ A fee SHAPE with no flat fraction of price arms NOTHING, never a silent `0.0`. hyperliquid
/// (percent schedule) IS floored; a spread-charging FX venue is not, and `maker_round_trip_fee_for`
/// says so at `warn`.
#[test]
fn a_venue_with_no_flat_fee_rate_arms_no_floor_rather_than_a_zero_one() {
    assert_eq!(
        maker_round_trip_fee_for("hyperliquid", "BTCUSDC.P").map(f64::to_bits),
        Some(3e-4_f64.to_bits()),
        "hyperliquid VIP0 is 1.5 bps maker ⇒ a 3 bps round trip"
    );
    for spread_charging in ["oanda", "ig"] {
        assert_eq!(
            maker_round_trip_fee_for(spread_charging, "EURUSD"),
            None,
            "{spread_charging} charges through the SPREAD: an absent bar, not a zero fee"
        );
    }
    // dukascopy and ctrader charge a measured $30 a million (0.3 bps) on EURUSD (commission audit
    // 2026-11-08), a flat rate, so their floor arms at 0.6 bps a round trip.
    for commissioned in ["dukascopy", "ctrader"] {
        assert_eq!(
            maker_round_trip_fee_for(commissioned, "EURUSD").map(f64::to_bits),
            Some((0.3_f64 / 10_000.0 + 0.3_f64 / 10_000.0).to_bits()),
            "{commissioned}"
        );
    }
    // ...nor polymarket: its p(1−p) fee curve is no scalar (`maker_round_trip_fee`'s refusals).
    assert_eq!(maker_round_trip_fee_for("polymarket", "TOK"), None);
}

// ---- the paper client's fee source (registry vs. config override) ----
use vike_execution::ExecutionClient;
use vike_model::OrderRequest;

fn fee_config(venue: &str, maker_fee: f64, taker_fee: f64) -> MountPlacement {
    MountPlacement { slippage: 0.0, maker_fee, taker_fee, ..outcome_spec(venue, "TOK") }
}

/// Drive one market (taker) fill through the client `paper_client_for` builds and read its fee.
///
/// ⚠ **The book is PINNED to a sentinel this test owns and never creates.** The OPENING
/// (non-`reduce_only`) order is refused by a HALT-armed book, so `fills.len()` panics with a fee
/// message that never mentions halt. Through `StrategyCorePaperHalt::ProcessWide`, both callers failed on any
/// box holding `<project>/settings/state/HALT` (the file an operator touches on a live node); CI
/// was green only because no runner had one.
///
/// The `halt_path` assertion makes it ENVIRONMENT-INDEPENDENT: re-pointed at
/// `StrategyCorePaperHalt::ProcessWide`, it fires on every box, sentinel or not.
fn taker_fee_of(spec: &MountPlacement, qty: f64, px: f64) -> f64 {
    let pinned =
        PathBuf::from("vike-tradehub-core-fee-tests-own-this-sentinel-and-never-create-it/HALT");
    let mut c = paper_client_for(spec, &StrategyCorePaperHalt::Pinned(pinned.clone()));
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
        order_client_id: "m".into(),
        venue: spec.venue.clone(),
        symbol: spec.symbol.clone(),
        side: 1,
        qty,
        order_type: vike_model::OrderType::Market,
        ..Default::default()
    });
    c.on_bar(&Bar {
        timestamp: 1,
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
    assert_eq!(taker_fee_of(&fee_config("polymarket", 0.0, 0.0), 20.0, 0.5), 0.0);
    // A non-Free registry (binance taker = 10 bps) proves the registry, not a hardcoded 0.
    assert_eq!(
        taker_fee_of(&fee_config("binance", 0.0, 0.0), 2.0, 100.0),
        2.0 * 100.0 * (10.0 / 10_000.0)
    );
}

#[test]
fn explicit_profile_fee_override_still_wins() {
    // A non-zero config fee keeps the flat-rate path even over a registry schedule.
    assert_eq!(
        taker_fee_of(&fee_config("binance", 0.0002, 0.0007), 2.0, 100.0),
        2.0 * 100.0 * 0.0007
    );
}
