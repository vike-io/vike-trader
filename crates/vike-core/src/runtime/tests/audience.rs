//! The audience rule, pinned per lane — `crates/vike-core/src/runtime/strategy_drive/subscriptions.rs`'s
//! `mount_hears`, the ONE place that says which mounts a market message reaches.
//!
//! One test per [`Audience`] variant (plus one on how the tick rule reads a leg's venue). Each asks
//! the rule directly (no lane, no dispatch) about a fixed set of mounts and pins a case that hears
//! (the mount's own symbol, a declared leg) and a case that does not (the wrong venue, the wrong
//! interval, a leg the lane never consults). The tests that go through `pin` then tombstone one mount
//! that WOULD hear and pin that its slot goes quiet; the Tick-vs-Reference disjointness test only
//! asks. The lanes' own behaviour (who runs, in which order, under which mount index) stays pinned
//! by `multi_mount_tests` and `mount_account_tests`; this file is what keeps a lane's rule and the
//! tick lane's table from drifting apart.
//!
//! **The tick table equals the rule.** The tick lane does not ask the rule per slot; it reads the
//! table `rebuild_tick_audience` builds from it. The second half of this file holds the table to the
//! rule (same slots, same order, for every probed pair and for every pair the table holds) on a fresh
//! core, after a runtime unmount, after runtime mounts, and on a two-account core: a table that
//! drifted from the rule is a mount silently deaf to its ticks, and no lane test would say why. The
//! one deliberate exception is a slot a strategy-hook PANIC leaves empty, which the tick lane's
//! `is_some()` re-check covers (`the_tick_lane_skips_a_slot_left_empty_by_a_hook_panic`).
//!
//! White-box, in-crate: it needs `Audience` (crate-private) and `core_with` (the runtime's in-crate
//! synchronous builder). `use super::*` imports the runtime module, the sibling-test-module idiom of
//! `multi_mount_tests`.

use super::*;
use std::sync::Mutex;
use vike_exec::testing::RecordingClient;
use vike_exec::{Account, MountSpec, RiskGate};
use vike_model::QuoteTick;
use vike_model::RiskLimits;
use vike_model::accounts::account_keys::AccountLabel;

use crate::runtime::strategy_drive::subscriptions::Audience;
// The runtime's shared white-box builders: a synchronous `CoreThread` over caller-supplied engines
// (`core_of`) or over the default sim engine (`core_with`).
use crate::runtime::test_support::{core_of, core_with};

/// A mount that does nothing: the rule reads only a mount's coordinates.
struct Mute;

impl Strategy<LiveBroker> for Mute {}

/// A mount on the `sim` venue (the only engine the core has) with its own `controller_id`, so mounts
/// that share a series do not collide on the derived mount id.
fn mount(
    controller_id: &str,
    symbol: &str,
    interval: &str,
    symbols: Vec<MountLeg>,
    underlying_symbol: Option<&str>,
) -> StrategyMount {
    StrategyMount {
        account: None,
        symbols,
        controller_id: Some(controller_id.to_string()),
        underlying_symbol: underlying_symbol.map(str::to_string),
        venue: "sim".into(),
        symbol: symbol.into(),
        interval: interval.into(),
        strategy: Box::new(Mute),
    }
}

/// The slot of the mount the tests tombstone: it hears on every lane that a mount of its coordinates
/// can, which is what makes its silence after the unmount a fact about the slot and not about the
/// probe.
const GONE: usize = 4;

/// Six mounts, all on `sim`, in this slot order:
///
/// - 0 `leg-a`: BTCUSDT 1m, declares ETHUSDT (same venue);
/// - 1 `5m-under`: BTCUSDT 5m, watches SPOT as its underlying;
/// - 2 `plain`: ETHUSDT 1m, nothing declared;
/// - 3 `cross`: BTCUSDT 1m, declares BTC-USDT-SWAP on `okx`;
/// - 4 `gone` ([`GONE`]): BTCUSDT 1m, declares both legs above and watches SPOT;
/// - 5 `own-venue-leg`: ADAUSDT 1m, declares XRPUSDT on `sim`, ITS OWN venue (last, so the
///   tombstoned slot's index does not move): the only mount that can tell `Audience::Reference`'s
///   venue guard from its leg test, since a foreign-venue leg alone would be rejected either way.
fn audience_core() -> CoreThread<RecordingClient> {
    core_with(CoreConfig {
        seed_cash: 10_000.0,
        strategy: Some(mount(
            "leg-a",
            "BTCUSDT",
            "1m",
            vec![MountLeg::same_venue("ETHUSDT")],
            None,
        )),
        extra_mounts: vec![
            mount("5m-under", "BTCUSDT", "5m", Vec::new(), Some("SPOT")),
            mount("plain", "ETHUSDT", "1m", Vec::new(), None),
            mount("cross", "BTCUSDT", "1m", vec![MountLeg::at("BTC-USDT-SWAP", "okx")], None),
            mount(
                "gone",
                "BTCUSDT",
                "1m",
                vec![MountLeg::same_venue("ETHUSDT"), MountLeg::at("BTC-USDT-SWAP", "okx")],
                Some("SPOT"),
            ),
            mount("own-venue-leg", "ADAUSDT", "1m", vec![MountLeg::at("XRPUSDT", "sim")], None),
        ],
        ..CoreConfig::default()
    })
}

/// The slots that hear `(venue, symbol)` on `audience`, in slot order.
fn hearers(
    core: &CoreThread<RecordingClient>,
    venue: &str,
    symbol: &str,
    audience: Audience<'_>,
) -> Vec<usize> {
    (0..core.mounts.len()).filter(|&i| core.mount_hears(i, venue, symbol, audience)).collect()
}

/// One probe: `(venue, symbol, audience, the slots that hear it while every mount is live)`.
type Probe<'a> = (&'a str, &'a str, Audience<'a>, &'a [usize]);

/// Pin every probe on the fresh core, then unmount [`GONE`] and pin them again with its slot removed
/// from every expectation: a tombstoned slot hears nothing, and the others do not move.
fn pin(probes: &[Probe<'_>]) {
    let mut core = audience_core();
    for &(venue, symbol, audience, want) in probes {
        assert_eq!(
            hearers(&core, venue, symbol, audience),
            want,
            "every mount live: ({venue}, {symbol}) on {audience:?}"
        );
    }
    core.unmount_strategy_runtime("gone");
    assert!(core.mounts[GONE].is_none(), "the unmount left slot {GONE} a tombstone");
    for &(venue, symbol, audience, want) in probes {
        let want: Vec<usize> = want.iter().copied().filter(|&i| i != GONE).collect();
        assert_eq!(
            hearers(&core, venue, symbol, audience),
            want,
            "slot {GONE} tombstoned: ({venue}, {symbol}) on {audience:?}"
        );
        assert!(
            !core.mount_hears(GONE, venue, symbol, audience),
            "a tombstoned slot hears nothing: ({venue}, {symbol}) on {audience:?}"
        );
    }
}

/// A CLOSED bar reaches a mount's own symbol or a declared leg's, on the mount's venue, at the
/// mount's interval.
#[test]
fn a_bar_reaches_the_own_symbol_or_a_leg_at_the_mounts_interval() {
    let one_m = Audience::Bar { interval: "1m" };
    let five_m = Audience::Bar { interval: "5m" };
    pin(&[
        // the own symbol: every 1m mount on BTCUSDT (the 5m one is another series)
        ("sim", "BTCUSDT", one_m, &[0, 3, 4]),
        ("sim", "BTCUSDT", five_m, &[1]),
        // a declared same-venue leg: slots 0 and 4 hear ETHUSDT beside its own mount, slot 2
        ("sim", "ETHUSDT", one_m, &[0, 2, 4]),
        // the wrong interval: nobody trades ETHUSDT at 5m, and a leg is a leg at its mount's interval
        ("sim", "ETHUSDT", five_m, &[]),
        ("sim", "BTCUSDT", Audience::Bar { interval: "15m" }, &[]),
        // the wrong venue: a leg declared on `okx` is not a bar of `okx`
        ("okx", "BTCUSDT", one_m, &[]),
        ("okx", "BTC-USDT-SWAP", one_m, &[]),
        // slot 5: its own symbol and its leg, both on its own venue
        ("sim", "ADAUSDT", one_m, &[5]),
        ("sim", "XRPUSDT", one_m, &[5]),
    ]);
}

/// A quote / trade / book reaches a mount's own symbol or a declared leg's SYMBOL, at any interval,
/// on the mount's own venue, and never a message of another venue.
#[test]
fn a_tick_reaches_the_own_symbol_or_a_leg_at_any_interval() {
    pin(&[
        // the own symbol, at either interval (a tick belongs to no interval): slot 1 is the 5m mount
        ("sim", "BTCUSDT", Audience::Tick, &[0, 1, 3, 4]),
        // a declared leg beside the plain mount on the same symbol
        ("sim", "ETHUSDT", Audience::Tick, &[0, 2, 4]),
        // the wrong venue: nobody trades on `okx`, so nothing on it is anyone's tick
        ("okx", "BTCUSDT", Audience::Tick, &[]),
        ("okx", "BTC-USDT-SWAP", Audience::Tick, &[]),
        ("sim", "SOLUSDT", Audience::Tick, &[]),
        // slot 5: its own symbol and its leg (a leg naming the mount's own venue)
        ("sim", "ADAUSDT", Audience::Tick, &[5]),
        ("sim", "XRPUSDT", Audience::Tick, &[5]),
    ]);
}

/// The tick rule matches a leg by SYMBOL and ignores the leg's venue: slots 3 and 4 declare
/// BTC-USDT-SWAP on `okx` and still hear the `sim` message of that symbol, on their own venue. The
/// message of `okx` itself never reaches them here (the venue test), it rides `Audience::Reference`.
/// This is current behaviour, pinned so that it is only ever changed on purpose.
#[test]
fn a_tick_ignores_the_venue_a_leg_names() {
    pin(&[
        ("sim", "BTC-USDT-SWAP", Audience::Tick, &[3, 4]),
        ("okx", "BTC-USDT-SWAP", Audience::Tick, &[]),
        ("okx", "BTC-USDT-SWAP", Audience::Reference, &[3, 4]),
    ]);
}

/// Feed status and flow reach the mount's OWN pair only, at any interval; a declared leg is not
/// consulted.
#[test]
fn feed_status_and_flow_reach_the_own_pair_only() {
    pin(&[
        ("sim", "BTCUSDT", Audience::OwnPair, &[0, 1, 3, 4]),
        // ETHUSDT is slot 2's own pair and a LEG of slots 0 and 4: only the own pair hears
        ("sim", "ETHUSDT", Audience::OwnPair, &[2]),
        ("sim", "ADAUSDT", Audience::OwnPair, &[5]),
        // ...and slot 5's leg is nobody's own pair
        ("sim", "XRPUSDT", Audience::OwnPair, &[]),
        ("okx", "BTCUSDT", Audience::OwnPair, &[]),
        ("okx", "BTC-USDT-SWAP", Audience::OwnPair, &[]),
    ]);
}

/// A params update reaches the mount's own series exactly: venue, symbol AND interval, no leg.
#[test]
fn a_params_update_reaches_the_exact_series() {
    let one_m = Audience::OwnSeries { interval: "1m" };
    pin(&[
        ("sim", "BTCUSDT", one_m, &[0, 3, 4]),
        ("sim", "BTCUSDT", Audience::OwnSeries { interval: "5m" }, &[1]),
        // the wrong interval
        ("sim", "BTCUSDT", Audience::OwnSeries { interval: "15m" }, &[]),
        // a leg is not a series: ETHUSDT 1m is slot 2's alone
        ("sim", "ETHUSDT", one_m, &[2]),
        // the wrong venue
        ("okx", "BTCUSDT", one_m, &[]),
    ]);
}

/// An underlying mark reaches the mounts whose `underlying_symbol` it is, on their venue — never the
/// mount that merely trades that symbol.
#[test]
fn an_underlying_mark_reaches_the_mounts_that_watch_it() {
    pin(&[
        ("sim", "SPOT", Audience::Underlying, &[1, 4]),
        // a mount's own symbol is not an underlying
        ("sim", "BTCUSDT", Audience::Underlying, &[]),
        // the wrong venue
        ("okx", "SPOT", Audience::Underlying, &[]),
    ]);
}

/// A foreign venue's quote reaches the mounts on ANOTHER venue that declared exactly that
/// `(symbol, venue)` as a leg, and is disjoint from the tick lane.
#[test]
fn a_reference_quote_reaches_the_mounts_that_declared_the_foreign_leg() {
    pin(&[
        ("okx", "BTC-USDT-SWAP", Audience::Reference, &[3, 4]),
        // the mounts' own venue is the tick lane's, not this one's
        ("sim", "BTC-USDT-SWAP", Audience::Reference, &[]),
        // a leg declared on `okx` is not a leg on `binance`
        ("binance", "BTC-USDT-SWAP", Audience::Reference, &[]),
        // a same-venue leg names no foreign venue: ETHUSDT on `okx` is nobody's reference
        ("okx", "ETHUSDT", Audience::Reference, &[]),
        // slot 5 declared XRPUSDT on `sim`, ITS OWN venue: `Tick` hears it (above), and `Reference`
        // must not, whatever the leg says: the venue guard is what keeps the lanes disjoint
        ("sim", "XRPUSDT", Audience::Reference, &[]),
    ]);
}

/// The reference lane and the tick lane never share an audience, so one message never reaches one
/// mount through both (`Audience::Reference`'s doc).
#[test]
fn no_mount_hears_one_message_on_both_the_tick_and_the_reference_lane() {
    let core = audience_core();
    for venue in ["sim", "okx", "binance"] {
        for symbol in ["BTCUSDT", "ETHUSDT", "BTC-USDT-SWAP", "SPOT", "SOLUSDT", "XRPUSDT"] {
            for idx in 0..core.mounts.len() {
                assert!(
                    !(core.mount_hears(idx, venue, symbol, Audience::Tick)
                        && core.mount_hears(idx, venue, symbol, Audience::Reference)),
                    "slot {idx} hears ({venue}, {symbol}) on both the tick and the reference lane"
                );
            }
        }
    }
}

// ---- the tick table equals the rule ---------------------------------------------------------------

/// A factory for the runtime-mount half: every spec resolves to a [`Mute`].
fn mute_factory() -> StrategyFactory {
    Box::new(|_spec: &MountSpec| Ok(Box::new(Mute)))
}

/// A runtime mount spec on `sim` at 1m, under its own `controller_id`.
fn sim_spec(controller_id: &str, symbol: &str) -> MountSpec {
    MountSpec {
        venue: "sim".into(),
        symbol: symbol.into(),
        interval: "1m".into(),
        account: None,
        controller_id: Some(controller_id.to_string()),
        name: Some("mute".into()),
        rhai: None,
        params: serde_json::json!({}),
    }
}

/// The slot of the mount with declared legs, the one [`the_tick_table_follows_a_runtime_unmount`]
/// takes out: it sits in the MIDDLE, so the slots after it keep their indices.
const LEGS: usize = 2;

/// Five mounts, all on `sim`, in this slot order, plus a strategy factory for runtime mounts:
///
/// - 0 `pair-1m`: BTCUSDT 1m;
/// - 1 `pair-5m`: BTCUSDT 5m, the second mount on one pair;
/// - 2 `legs` ([`LEGS`]): BTCUSDT 1m, declaring ETHUSDT TWICE, its own BTCUSDT, and BTC-USDT-SWAP on
///   `okx` (the repeats are what "a slot enters a pair at most once" is about);
/// - 3 `eth`: ETHUSDT 1m, nothing declared, a plain mount on the leg's symbol;
/// - 4 `sol`: SOLUSDT 1m, a mount on another symbol.
fn table_core() -> CoreThread<RecordingClient> {
    core_with(CoreConfig {
        seed_cash: 10_000.0,
        strategy: Some(mount("pair-1m", "BTCUSDT", "1m", Vec::new(), None)),
        extra_mounts: vec![
            mount("pair-5m", "BTCUSDT", "5m", Vec::new(), None),
            mount(
                "legs",
                "BTCUSDT",
                "1m",
                vec![
                    MountLeg::same_venue("ETHUSDT"),
                    MountLeg::same_venue("ETHUSDT"),
                    MountLeg::same_venue("BTCUSDT"),
                    MountLeg::at("BTC-USDT-SWAP", "okx"),
                ],
                None,
            ),
            mount("eth", "ETHUSDT", "1m", Vec::new(), None),
            mount("sol", "SOLUSDT", "1m", Vec::new(), None),
        ],
        strategy_factory: Some(mute_factory()),
        ..CoreConfig::default()
    })
}

/// Every pair a mount of [`table_core`] (or a runtime mount added to it) is a tick candidate for,
/// plus pairs nobody hears: an unknown symbol, an unknown venue, and the foreign leg's own venue.
const TABLE_PROBES: &[(&str, &str)] = &[
    ("sim", "BTCUSDT"),
    ("sim", "ETHUSDT"),
    ("sim", "BTC-USDT-SWAP"),
    ("sim", "SOLUSDT"),
    ("sim", "XRPUSDT"),
    ("sim", "NOPE"),
    ("other", "BTCUSDT"),
    ("okx", "BTC-USDT-SWAP"),
    ("okx", "BTCUSDT"),
];

/// The tick table's answer for `(venue, symbol)` as a list, `None` read as nobody.
fn table(core: &CoreThread<RecordingClient>, venue: &str, symbol: &str) -> Vec<usize> {
    core.tick_audience_of(venue, symbol).map(|slots| slots.to_vec()).unwrap_or_default()
}

/// **The table equals the rule**: for every probe, the table's slots are exactly the slots
/// `mount_hears` answers `true` for on [`Audience::Tick`], in the same order; and every pair the
/// table holds at all, probed or not, passes the same check, so it holds nothing the rule rejects.
fn assert_table_is_the_rule(
    core: &CoreThread<RecordingClient>,
    probes: &[(&str, &str)],
    when: &str,
) {
    for &(venue, symbol) in probes {
        assert_eq!(
            table(core, venue, symbol),
            hearers(core, venue, symbol, Audience::Tick),
            "{when}: the tick table disagrees with the rule on ({venue}, {symbol})"
        );
    }
    for (venue, by_symbol) in &core.tick_audience {
        for (symbol, slots) in by_symbol {
            assert_eq!(
                slots.to_vec(),
                hearers(core, venue, symbol, Audience::Tick),
                "{when}: the tick table's own entry ({venue}, {symbol}) disagrees with the rule"
            );
        }
    }
}

/// At assembly: two mounts on one pair, a mount with declared (and repeated) legs, a plain mount on
/// the leg's symbol and a mount on another symbol.
#[test]
fn the_tick_table_is_the_rule_at_assembly() {
    let core = table_core();
    assert_table_is_the_rule(&core, TABLE_PROBES, "at assembly");
    // ...and the check is not vacuous here: the shapes above are in the table, each slot once
    assert_eq!(table(&core, "sim", "BTCUSDT"), [0, 1, LEGS]);
    assert_eq!(table(&core, "sim", "ETHUSDT"), [LEGS, 3]);
    assert_eq!(table(&core, "sim", "SOLUSDT"), [4]);
    assert!(core.tick_audience_of("sim", "NOPE").is_none());
    assert!(core.tick_audience_of("other", "BTCUSDT").is_none());
}

/// After a runtime UNMOUNT of the middle mount: its slot leaves every pair, the others keep theirs.
#[test]
fn the_tick_table_follows_a_runtime_unmount() {
    let mut core = table_core();
    core.unmount_strategy_runtime("legs");
    assert!(core.mounts[LEGS].is_none(), "the unmount left slot {LEGS} a tombstone");
    assert_table_is_the_rule(&core, TABLE_PROBES, "after unmounting slot 2");
    assert_eq!(table(&core, "sim", "BTCUSDT"), [0, 1]);
    assert_eq!(table(&core, "sim", "ETHUSDT"), [3]);
    assert!(core.tick_audience_of("sim", "BTC-USDT-SWAP").is_none());
}

/// After runtime MOUNTS behind a tombstone: one on a pair the table already holds, one on a pair it
/// did not; both land on fresh slots, appended in mount order.
#[test]
fn the_tick_table_follows_a_runtime_mount() {
    let mut core = table_core();
    core.unmount_strategy_runtime("legs");
    core.mount_strategy_runtime(sim_spec("late-btc", "BTCUSDT"));
    core.mount_strategy_runtime(sim_spec("late-xrp", "XRPUSDT"));
    assert_eq!(core.mounts.len(), 7, "two fresh slots appended: {:?}", core.recent);
    assert!(core.mounts[5].is_some() && core.mounts[6].is_some(), "both runtime mounts are live");
    assert_table_is_the_rule(&core, TABLE_PROBES, "after two runtime mounts");
    assert_eq!(table(&core, "sim", "BTCUSDT"), [0, 1, 5]);
    assert_eq!(table(&core, "sim", "XRPUSDT"), [6]);
}

/// One engine on `binance` whose ROUTING key is `route_key`: `mount_account_tests`' fixture shape
/// (canonical venue everywhere, `route_key` decorated).
fn binance_engine(route_key: &str) -> ExecutionEngine<RecordingClient> {
    let mut e = ExecutionEngine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "binance",
        "BTCUSDT",
    );
    e.route_key = route_key.to_string();
    e
}

/// A mount on `binance` at 1m naming `account`, under its own `controller_id`.
fn account_mount(
    controller_id: &str,
    account: AccountLabel,
    symbol: &str,
    symbols: Vec<MountLeg>,
) -> StrategyMount {
    StrategyMount {
        account: Some(account),
        symbols,
        controller_id: Some(controller_id.to_string()),
        underlying_symbol: None,
        venue: "binance".into(),
        symbol: symbol.into(),
        interval: "1m".into(),
        strategy: Box::new(Mute),
    }
}

/// Two accounts of one exchange: the table is keyed by the mount's VENUE, which both accounts share,
/// so both accounts' mounts on one symbol hear its ticks, and an account's route key is no venue.
#[test]
fn the_tick_table_is_the_rule_on_a_two_account_core() {
    let alt = AccountLabel::parse("ALT").expect("a legal label");
    let config = CoreConfig {
        seed_cash: 1_000.0,
        strategy: Some(account_mount("d-btc", AccountLabel::Default, "BTCUSDT", Vec::new())),
        extra_mounts: vec![
            account_mount("alt-btc", alt.clone(), "BTCUSDT", Vec::new()),
            account_mount("alt-sol", alt, "SOLUSDT", vec![MountLeg::same_venue("BTCUSDT")]),
        ],
        ..CoreConfig::default()
    };
    let mut core =
        core_of(binance_engine("binance"), vec![(1_000.0, binance_engine("binance#ALT"))], config);
    let probes: &[(&str, &str)] = &[
        ("binance", "BTCUSDT"),
        ("binance", "SOLUSDT"),
        ("binance", "NOPE"),
        ("binance#ALT", "BTCUSDT"),
        ("sim", "BTCUSDT"),
    ];
    assert_table_is_the_rule(&core, probes, "two accounts");
    assert_eq!(table(&core, "binance", "BTCUSDT"), [0, 1, 2]);
    assert_eq!(table(&core, "binance", "SOLUSDT"), [2]);
    assert!(core.tick_audience_of("binance#ALT", "BTCUSDT").is_none());

    core.unmount_strategy_runtime("alt-btc");
    assert_table_is_the_rule(&core, probes, "two accounts, slot 1 unmounted");
    assert_eq!(table(&core, "binance", "BTCUSDT"), [0, 2]);
}

// ---- a slot a hook panic leaves empty -------------------------------------------------------------

/// Records its slot on every quote tick it hears; when `panic_next` is set, the next one panics.
struct Hearer {
    slot: usize,
    heard: Arc<Mutex<Vec<usize>>>,
    panic_next: bool,
}

impl Strategy<LiveBroker> for Hearer {
    fn on_quote_tick(&mut self, _broker: &mut LiveBroker, _q: &QuoteTick) {
        self.heard.lock().unwrap().push(self.slot);
        if std::mem::take(&mut self.panic_next) {
            panic!("a strategy hook panicked");
        }
    }
}

/// A [`Hearer`] mount on `(sim, BTCUSDT, 1m)` under its own `controller_id`.
fn hearer_mount(
    controller_id: &str,
    slot: usize,
    panic_next: bool,
    heard: &Arc<Mutex<Vec<usize>>>,
) -> StrategyMount {
    StrategyMount {
        account: None,
        symbols: Vec::new(),
        controller_id: Some(controller_id.to_string()),
        underlying_symbol: None,
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        strategy: Box::new(Hearer { slot, heard: Arc::clone(heard), panic_next }),
    }
}

/// Two [`Hearer`] mounts on one pair, slot 0 first: the table lists `[0, 1]`.
fn hearer_core(panic_first: bool, heard: &Arc<Mutex<Vec<usize>>>) -> CoreThread<RecordingClient> {
    core_with(CoreConfig {
        seed_cash: 10_000.0,
        strategy: Some(hearer_mount("first", 0, panic_first, heard)),
        extra_mounts: vec![hearer_mount("second", 1, false, heard)],
        ..CoreConfig::default()
    })
}

/// One quote on `(sim, BTCUSDT)` through the real `Ingest::Quote` arm.
fn quote(core: &mut CoreThread<RecordingClient>, ts: i64) {
    core.dispatch(Ingest::Quote(Box::new(QuoteUpdate {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        quote: QuoteTick {
            ts,
            local_ts: 0,
            bid: 99.0,
            ask: 101.0,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: String::new(),
        },
    })));
}

/// [`quote`] under the panic policy of `run_loop.rs`'s `handle`: a panic is caught, counted in
/// `panics`, and the core enters safe-state and carries on.
fn guarded_quote(core: &mut CoreThread<RecordingClient>, ts: i64, panics: &mut usize) {
    if let Err(payload) =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| quote(&mut *core, ts)))
    {
        *panics += 1;
        core.enter_safe_state(crate::runtime::run_loop::panic_text(payload));
    }
}

/// **A hook panic leaves its slot `None` with no table rebuild, and the tick lane steps past it.**
/// A strategy that panics inside its tick hook unwinds between the `take` and the restore of its
/// slot; the guard enters safe-state and the core keeps dispatching with that slot empty while the
/// table still lists it. Without the lane's `is_some()` re-check the next quote would panic at the
/// `expect` in `step_mount_on_tick`, and slot 1 would never hear it (safe-state re-entered per
/// tick).
///
/// REAL-PANIC variant: `Hearer`'s `on_quote_tick` really panics, through `core.dispatch`, under a
/// test-local `catch_unwind` that mirrors `handle`'s policy (`handle` is private to `run_loop.rs`,
/// so it cannot be called white-box; the unwind through `step_mount_on_tick` is the real one). The
/// panicking quote itself never reaches slot 1 (the unwind aborts the loop first), so slot 1 is
/// asserted on the quotes AFTER it; `an_emptied_slot_the_table_still_lists_is_skipped` pins the
/// both-quotes shape directly.
#[test]
fn the_tick_lane_skips_a_slot_left_empty_by_a_hook_panic() {
    let heard = Arc::new(Mutex::new(Vec::new()));
    let mut core = hearer_core(true, &heard);
    assert_eq!(table(&core, "sim", "BTCUSDT"), [0, 1]);
    let mut panics = 0;

    guarded_quote(&mut core, 1, &mut panics);
    assert_eq!(panics, 1, "the first quote panics inside slot 0's hook");
    assert!(core.mounts[0].is_none(), "the unwind left slot 0 empty");
    assert_eq!(table(&core, "sim", "BTCUSDT"), [0, 1], "...and nothing rebuilt the table");
    assert!(core.fault.is_some(), "the guard entered safe-state");
    let fault = core.fault.clone();
    assert_eq!(*heard.lock().unwrap(), [0], "slot 0 ran; the unwind cut the loop before slot 1");

    guarded_quote(&mut core, 2, &mut panics);
    guarded_quote(&mut core, 3, &mut panics);
    assert_eq!(panics, 1, "safe-state was entered exactly once: later quotes do not panic");
    assert_eq!(core.fault, fault, "the fault is the first panic's, unchanged");
    assert_eq!(*heard.lock().unwrap(), [0, 1, 1], "slot 1 heard both later quotes, slot 0 none");
    assert!(core.mounts[0].is_none() && core.mounts[1].is_some());
}

/// The same state without the panic: slot 0 is emptied DIRECTLY, no rebuild, so the table still
/// lists it. Slot 1 steps on BOTH quotes and nothing panics.
#[test]
fn an_emptied_slot_the_table_still_lists_is_skipped() {
    let heard = Arc::new(Mutex::new(Vec::new()));
    let mut core = hearer_core(false, &heard);
    core.mounts[0] = None;
    assert_eq!(table(&core, "sim", "BTCUSDT"), [0, 1], "the table was not rebuilt");
    quote(&mut core, 1);
    quote(&mut core, 2);
    assert_eq!(*heard.lock().unwrap(), [1, 1], "slot 1 stepped on both quotes, slot 0 on none");
    assert!(core.fault.is_none(), "nothing panicked");
}
