//! The margin guard (`exec/margin.rs`) and the universe reload (`exec/universe.rs`), driven through the
//! SAME fake venue as `exec_tests.rs` (a child module of it, so the harness is shared).
//!
//! Owner rule 2026-10-26: a user must be able to trade ANY ticker with NO hand step. A coin the account
//! holds in ISOLATED margin is switched to cross before its first opening order, and a coin listed after
//! the start is looked up again before it is refused. The fake venue answers `activeAssetData` /
//! `clearinghouseState` / `meta` by scripted body and records every action and every read, so what is
//! SENT, what is REFUSED, what is ASKED and what is ASKED AGAIN are the assertions.
use super::*;

fn eth_req(coid: &str) -> OrderRequest {
    OrderRequest { symbol: "ETH".into(), ..limit_req(coid, 1, 3000.0, 0.01) }
}

/// `activeAssetData`'s answer for one coin.
fn held(kind: &str, value: u64) -> Value {
    json!({"user":"0xmaster","coin":"ETH","leverage":{"type":kind,"value":value},
           "maxTradeSzs":["1.0","1.0"],"availableToTrade":["8.8","8.8"],"markPx":"3000.0"})
}

/// `clearinghouseState`'s `assetPositions`: one row per `(coin, szi)`.
fn positions(rows: &[(&str, &str)]) -> Value {
    let rows: Vec<Value> = rows
        .iter()
        .map(|(coin, szi)| json!({"type":"oneWay","position":{"coin":coin,"szi":szi}}))
        .collect();
    json!({"assetPositions": rows})
}

fn leverage_ok() -> Result<Value, VenueApiError> {
    Ok(json!({"status":"ok","response":{"type":"default"}}))
}

fn resting(oid: u64) -> Result<Value, VenueApiError> {
    Ok(json!({"status":"ok","response":{"data":{"statuses":[{"resting":{"oid":oid}}]}}}))
}

/// A fake venue that holds ETH as `eth` says, positions as `positions` says, and answers `responses`
/// to the `place` calls in order.
fn venue_holding(
    eth: Value,
    positions: Value,
    responses: Vec<Result<Value, VenueApiError>>,
) -> FakeExchange {
    FakeExchange {
        info_answers: HashMap::from([
            ("activeAssetData:ETH".to_string(), eth),
            ("clearinghouseState".to_string(), positions),
        ]),
        responses: Mutex::new(responses.into()),
        ..Default::default()
    }
}

fn sent(st: &ExecState<FakeExchange>) -> Vec<Action> {
    st.exchange.actions.lock().unwrap().clone()
}

fn reads(venue: &FakeExchange) -> Vec<String> {
    let asked = venue.info_asked.lock().unwrap();
    asked.iter().filter_map(|b| b.get("type")?.as_str().map(str::to_string)).collect()
}

fn asked(st: &ExecState<FakeExchange>, ty: &str) -> usize {
    reads(&st.exchange).iter().filter(|t| *t == ty).count()
}

fn rejection(events: &[Event], coid: &str) -> Option<String> {
    events.iter().find_map(|e| match e {
        Event::OrderRejected(r) if r.client_order_id == coid => Some(r.reason.to_string()),
        _ => None,
    })
}

/// THE FIX. ETH is held isolated at 3x and flat: the first order on it is preceded by ONE signed
/// `updateLeverage` on ETH's asset (index 1) with `isCross: true` at the SAME 3x (the mode changes, the
/// figure does not), and then the order itself goes: `Submitted`, `Accepted`, nothing refused.
#[test]
fn an_isolated_flat_coin_is_switched_to_cross_at_its_held_figure_before_its_first_order() {
    let (tx, mut rx) = event_channel(64);
    let venue =
        venue_holding(held("isolated", 3), positions(&[]), vec![leverage_ok(), resting(31)]);
    let mut st = state(venue, tx);
    st.submit(&symbology(), &[eth_req("c1")]);

    let actions = sent(&st);
    assert_eq!(actions.len(), 2, "the switch, then the order: {actions:?}");
    match &actions[0] {
        Action::UpdateLeverage(u) => assert_eq!(
            (u.asset, u.is_cross, u.leverage),
            (1, true, 3),
            "ETH is index 1; cross; the figure the account held"
        ),
        other => panic!("expected the updateLeverage first, got {other:?}"),
    }
    assert_matches!(&actions[1], Action::Order(o) if o.orders[0].asset == 1);
    assert_eq!(drain!(rx).iter().map(kind).collect::<Vec<_>>(), vec!["Submitted", "Accepted"]);

    // The reads went to the account's own book, for that coin.
    let bodies = st.exchange.info_asked.lock().unwrap().clone();
    assert_eq!(bodies[0], json!({"type":"activeAssetData","user":"0xmaster","coin":"ETH"}));
    assert_eq!(bodies[1], json!({"type":"clearinghouseState","user":"0xmaster"}));

    // Idempotent: a second order on the coin asks nothing and sends no second switch.
    st.exchange.responses.lock().unwrap().push_back(resting(32));
    st.submit(&symbology(), &[eth_req("c2")]);
    assert_eq!(sent(&st).len(), 3, "one more Order and no second updateLeverage");
    assert_eq!(asked(&st, "activeAssetData"), 1, "a verified coin is remembered");
}

/// A coin already held cross is never written to — not at its own figure, not ever — and the venue is
/// asked once per session, however many orders follow.
#[test]
fn a_coin_held_cross_is_never_written_and_is_asked_once() {
    let (tx, _rx) = event_channel(64);
    let venue = venue_holding(held("cross", 20), positions(&[]), vec![resting(41), resting(42)]);
    let mut st = state(venue, tx);
    st.submit(&symbology(), &[eth_req("c1")]);
    st.submit(&symbology(), &[eth_req("c2")]);
    let actions = sent(&st);
    assert!(actions.iter().all(|a| matches!(a, Action::Order(_))), "only orders: {actions:?}");
    assert_eq!(asked(&st, "activeAssetData"), 1);
    assert_eq!(asked(&st, "clearinghouseState"), 0, "positions are read only for an isolated coin");
}

/// HL refuses to switch a margin mode under an open position, so none is sent: the order goes exactly
/// as it did before this guard existed (a short counts as open), and the coin is asked again next time
/// — a closed position lets the switch happen then.
#[test]
fn an_isolated_coin_under_an_open_position_is_not_switched_and_still_trades() {
    let (tx, mut rx) = event_channel(64);
    let held_short = positions(&[("ETH", "-0.5")]);
    let mut st =
        state(venue_holding(held("isolated", 3), held_short, vec![resting(51), resting(52)]), tx);
    st.submit(&symbology(), &[eth_req("c1")]);
    st.submit(&symbology(), &[eth_req("c2")]);
    let actions = sent(&st);
    assert!(
        actions.iter().all(|a| matches!(a, Action::Order(_))),
        "no updateLeverage: {actions:?}"
    );
    assert_eq!(actions.len(), 2, "both orders went");
    assert!(rejection(&drain!(rx), "c1").is_none());
    assert_eq!(asked(&st, "activeAssetData"), 2, "not remembered: the position may close");
}

/// A switch the venue refuses (HTTP 200, `status: err`) refuses THIS order in words that name the coin,
/// the mode and the venue's reason, and puts nothing on the wire for it; the other coin of the same
/// batch (held cross) still goes.
#[test]
fn a_refused_switch_refuses_that_order_in_words_and_the_rest_of_the_batch_goes() {
    let (tx, mut rx) = event_channel(64);
    let refused = Ok(json!({"status":"err","response":"a reason the venue gave"}));
    let mut venue = venue_holding(held("isolated", 3), positions(&[]), vec![refused, resting(61)]);
    venue.info_answers.insert("activeAssetData:BTC".into(), held("cross", 40));
    let mut st = state(venue, tx);
    st.submit(&symbology(), &[limit_req("btc", 1, 50000.0, 0.01), eth_req("eth")]);

    let reason = rejection(&drain!(rx), "eth").expect("the isolated coin's order is refused");
    for must in ["ETH", "ISOLATED", "a reason the venue gave", "was not sent"] {
        assert!(reason.contains(must), "{must:?} missing from: {reason}");
    }
    let actions = sent(&st);
    assert_eq!(actions.len(), 2, "the failed switch and ONE order action: {actions:?}");
    assert_matches!(&actions[1], Action::Order(o) if o.orders.len() == 1 && o.orders[0].asset == 0);
    assert!(!st.orders.contains_key("eth"), "no local order state for a refused order");
}

/// A post-send timeout of the switch leaves its outcome unknown: the order is refused saying so (nothing
/// was sent for it), and the coin is NOT remembered, so the next order asks the venue again.
#[test]
fn a_timed_out_switch_refuses_the_order_and_is_asked_again_next_time() {
    let (tx, mut rx) = event_channel(64);
    let timeout = Err(VenueApiError {
        code: vike_bridge_core::transport::E_TIMEOUT_AMBIGUOUS,
        msg: "timeout".into(),
    });
    let mut st = state(venue_holding(held("isolated", 3), positions(&[]), vec![timeout]), tx);
    st.submit(&symbology(), &[eth_req("c1")]);
    let reason = rejection(&drain!(rx), "c1").expect("refused");
    assert!(reason.contains("unknown") && reason.contains("ISOLATED"), "{reason}");
    assert_eq!(sent(&st).len(), 1, "only the switch went");
    st.submit(&symbology(), &[eth_req("c2")]);
    assert_eq!(asked(&st, "activeAssetData"), 2, "asked again");
}

/// A close never waits on the guard: a reduce-only order reads nothing and sends no switch, even on an
/// isolated coin.
#[test]
fn a_reduce_only_order_never_touches_the_guard() {
    let (tx, _rx) = event_channel(64);
    let mut st = state(venue_holding(held("isolated", 3), positions(&[]), vec![resting(71)]), tx);
    st.submit(&symbology(), &[OrderRequest { reduce_only: true, ..eth_req("close") }]);
    assert!(reads(&st.exchange).is_empty(), "nothing was asked");
    assert_eq!(sent(&st).len(), 1);
    assert_matches!(&sent(&st)[0], Action::Order(_));
}

/// OWNER RULING 2026-10-26 ("what do you need live if you have demo"): NO exchange write on a LIVE account.
/// The very coin the testnet test above switches (held ISOLATED, flat) is left exactly as v0.1.92 left it
/// when the exec thread signs on MAINNET (a `live` account): nothing is read, nothing is written, and the
/// order goes (a mode the venue itself refuses is the venue's own refusal text, as it always was).
#[test]
fn a_mainnet_account_is_neither_read_nor_written_by_the_guard() {
    let (tx, mut rx) = event_channel(64);
    let venue = FakeExchange {
        mainnet: true,
        ..venue_holding(held("isolated", 3), positions(&[]), vec![resting(33), resting(34)])
    };
    let mut st = state(venue, tx);
    st.submit(&symbology(), &[eth_req("c1")]);
    st.submit(&symbology(), &[eth_req("c2")]);
    let actions = sent(&st);
    assert!(
        actions.len() == 2 && actions.iter().all(|a| matches!(a, Action::Order(_))),
        "only the two orders, no updateLeverage: {actions:?}"
    );
    assert!(reads(&st.exchange).is_empty(), "the guard asked the venue nothing");
    assert!(rejection(&drain!(rx), "c1").is_none());
}

/// "The venue did not say" is never a refusal: an unreadable `activeAssetData` (the venue down, or a
/// shape this does not know) lets the order go as it always did, and the coin is asked again.
#[test]
fn a_read_that_fails_lets_the_order_go_and_is_asked_again() {
    let (tx, mut rx) = event_channel(64);
    let venue = FakeExchange {
        responses: Mutex::new(VecDeque::from([resting(81), resting(82)])),
        ..Default::default()
    };
    let mut st = state(venue, tx);
    st.submit(&symbology(), &[eth_req("c1")]);
    st.exchange.info_answers.insert("activeAssetData:ETH".into(), json!({"leverage":{"type":"x"}}));
    st.submit(&symbology(), &[eth_req("c2")]);
    let actions = sent(&st);
    assert!(
        actions.iter().all(|a| matches!(a, Action::Order(_))) && actions.len() == 2,
        "{actions:?}"
    );
    let events = drain!(rx);
    assert!(rejection(&events, "c1").is_none() && rejection(&events, "c2").is_none());
    assert_eq!(asked(&st, "activeAssetData"), 2);
}

/// What the guard must not touch: a spot pair (no leverage), an isolated-only coin (cross is not
/// available there; a mounted one is margined isolated) and a builder-dex coin (opt-in, refused
/// upstream). None of them is read or written.
#[test]
fn spot_isolated_only_and_builder_dex_coins_are_left_alone() {
    let meta = json!({"universe":[
        {"name":"BTC","szDecimals":5,"maxLeverage":40},
        {"name":"RLB","szDecimals":0,"maxLeverage":3,"onlyIsolated":true},
        {"name":"STRICT","szDecimals":0,"maxLeverage":3,"marginMode":"strictIsolated"}
    ]});
    let spot = json!({"tokens":[{"name":"USDC","szDecimals":8,"index":0},
                                {"name":"PURR","szDecimals":0,"index":1}],
                      "universe":[{"name":"PURR/USDC","tokens":[1,0],"index":0}]});
    let mut universe = Symbology::from_meta(&meta, &spot);
    let dex = json!({"universe":[{"name":"test:ABC","szDecimals":2,"maxLeverage":10}]});
    universe.extend_with_perp_dex(&dex, 1, "test");
    let (tx, _rx) = event_channel(64);
    let mut st = state(FakeExchange::default(), tx);
    for symbol in ["PURR/USDC", "RLB", "STRICT", "test:ABC"] {
        let req = OrderRequest { symbol: symbol.into(), ..limit_req("c", 1, 1.0, 100.0) };
        assert_eq!(st.ensure_cross(&universe, &req), Ok(()), "{symbol}");
    }
    assert!(reads(&st.exchange).is_empty(), "nothing was read for any of them");
    assert!(sent(&st).is_empty(), "nothing was written");
}

/// A coin the universe lacks is refused in words that name the network it was judged on; the testnet's
/// list is not the mainnet's, and the words say so only there.
#[test]
fn an_unknown_coin_is_refused_naming_the_accounts_own_network() {
    for mainnet in [false, true] {
        let (tx, mut rx) = event_channel(64);
        let mut st = state(FakeExchange { mainnet, ..Default::default() }, tx);
        let xrp = OrderRequest { symbol: "XRP".into(), ..limit_req("c", 1, 0.5, 100.0) };
        st.submit(&symbology(), &[xrp]);
        let reason = rejection(&drain!(rx), "c").expect("refused");
        let network = if mainnet { "mainnet" } else { "testnet" };
        assert!(reason.starts_with("unknown symbol XRP"), "{reason}");
        assert!(reason.contains(&format!("Hyperliquid's {network} list")), "{reason}");
        assert_eq!(reason.contains("the testnet list is not mainnet's"), !mainnet, "{reason}");
        assert!(sent(&st).is_empty(), "nothing went to the venue");
    }
}

/// The universe reload: a coin listed after the start.
mod reload {
    use super::*;
    use crate::exec::universe::{RELOAD_MIN_INTERVAL, Universe};
    use crate::feeds::instruments::{HyperliquidInstruments, SharedInstruments};
    use std::time::Instant;

    fn spot() -> Value {
        json!({"tokens":[],"universe":[]})
    }

    fn started() -> (Universe, SharedInstruments) {
        let meta = json!({"universe":[
            {"name":"BTC","szDecimals":5,"maxLeverage":40},
            {"name":"ETH","szDecimals":4,"maxLeverage":25}
        ]});
        let start = HyperliquidInstruments::build(&meta, &spot(), None);
        let shared = SharedInstruments::new(Arc::new(start));
        (Universe::new(shared.clone()), shared)
    }

    /// The venue's `meta` now: the start's two coins and `kNEW`, listed since.
    fn venue_with_new_coin() -> FakeExchange {
        let meta = json!({"universe":[
            {"name":"BTC","szDecimals":5,"maxLeverage":40},
            {"name":"ETH","szDecimals":4,"maxLeverage":25},
            {"name":"kNEW","szDecimals":0,"maxLeverage":5}
        ]});
        FakeExchange {
            info_answers: HashMap::from([
                ("meta".to_string(), meta),
                ("spotMeta".to_string(), spot()),
            ]),
            ..Default::default()
        }
    }

    /// THE FIX for "a newly listed coin is refused until restart": the order names `kNEW`, the universe
    /// is re-read once, and the order is signed on kNEW's own asset index (2) — while the pump's handle
    /// (the same `SharedInstruments`) now remaps through the new universe too.
    #[test]
    fn a_coin_listed_after_the_start_is_placeable_after_one_reload() {
        let (mut universe, shared) = started();
        let venue = venue_with_new_coin();
        assert!(shared.current().symbology().by_symbol("kNEW").is_none(), "unknown at the start");

        let now = universe.for_symbols_at(Instant::now(), &venue, &["kNEW"]);
        assert!(now.symbology().by_symbol("kNEW").is_some());
        assert!(shared.current().symbology().by_symbol("kNEW").is_some(), "the pump's view too");
        assert_eq!(reads(&venue), ["meta", "spotMeta"], "one re-read, the core pair");

        let (tx, mut rx) = event_channel(64);
        let order_venue = FakeExchange {
            responses: Mutex::new(VecDeque::from([resting(91)])),
            ..venue_with_new_coin()
        };
        let mut st = state(order_venue, tx);
        let req = OrderRequest { symbol: "kNEW".into(), ..limit_req("c1", 1, 1.0, 100.0) };
        st.submit(now.symbology(), &[req]);
        assert_matches!(&sent(&st)[0], Action::Order(o) if o.orders[0].asset == 2);
        assert_eq!(drain!(rx).iter().map(kind).collect::<Vec<_>>(), vec!["Submitted", "Accepted"]);
    }

    /// A coin the start knows costs the venue nothing.
    #[test]
    fn a_known_coin_never_reloads() {
        let (mut universe, _) = started();
        let venue = venue_with_new_coin();
        universe.for_symbols_at(Instant::now(), &venue, &["BTC", "ETH"]);
        assert!(reads(&venue).is_empty());
    }

    /// An unlisted coin asked for in a loop costs one re-read per interval, not one per order: a typo
    /// is not a flood.
    #[test]
    fn an_unknown_coin_reloads_once_per_interval() {
        let (mut universe, _) = started();
        let venue = venue_with_new_coin();
        let t0 = Instant::now();
        universe.for_symbols_at(t0, &venue, &["kNOPE"]);
        universe.for_symbols_at(t0 + RELOAD_MIN_INTERVAL / 2, &venue, &["kNOPE"]);
        assert_eq!(reads(&venue).len(), 2, "one pair of reads inside the interval");
        universe.for_symbols_at(t0 + RELOAD_MIN_INTERVAL, &venue, &["kNOPE"]);
        assert_eq!(reads(&venue).len(), 4, "the next interval may ask again");
    }

    /// A failed read, and a read that came back SMALLER than the start's universe (a half answer), both
    /// keep the start's universe: publishing a smaller one would turn every known coin into unknown.
    #[test]
    fn a_failed_or_smaller_reload_keeps_the_starts_universe() {
        let (mut universe, shared) = started();
        let kept = universe.for_symbols_at(Instant::now(), &FakeExchange::default(), &["kNEW"]);
        assert!(kept.symbology().by_symbol("ETH").is_some(), "a failed read changes nothing");
        assert!(kept.symbology().by_symbol("kNEW").is_none());

        let (mut universe, shared_two) = started();
        let btc_only = json!({"universe":[{"name":"BTC","szDecimals":5,"maxLeverage":40}]});
        let half = FakeExchange {
            info_answers: HashMap::from([
                ("meta".to_string(), btc_only),
                ("spotMeta".to_string(), spot()),
            ]),
            ..Default::default()
        };
        universe.for_symbols_at(Instant::now(), &half, &["kNEW"]);
        assert!(shared_two.current().symbology().by_symbol("ETH").is_some(), "not published");
        assert!(shared.current().symbology().by_symbol("ETH").is_some());
    }

    /// Asset ids are ARRAY POSITIONS and an order is signed by id: a re-read that MOVES a coin the start
    /// knows (here ETH and BTC trade places, and a coin is listed on top, so it is not smaller) must never
    /// be published, or the next order on ETH would be signed on BTC's asset.
    #[test]
    fn a_reload_that_moves_a_known_coins_asset_id_is_not_published() {
        let (mut universe, shared) = started();
        let moved = json!({"universe":[
            {"name":"ETH","szDecimals":4,"maxLeverage":25},
            {"name":"BTC","szDecimals":5,"maxLeverage":40},
            {"name":"kNEW","szDecimals":0,"maxLeverage":5}
        ]});
        let venue = FakeExchange {
            info_answers: HashMap::from([
                ("meta".to_string(), moved),
                ("spotMeta".to_string(), spot()),
            ]),
            ..Default::default()
        };
        let kept = universe.for_symbols_at(Instant::now(), &venue, &["kNEW"]);
        assert_eq!(kept.symbology().asset_id_for("ETH"), Some(1), "ETH is where the start put it");
        assert_eq!(kept.symbology().asset_id_for("BTC"), Some(0));
        assert!(kept.symbology().by_symbol("kNEW").is_none(), "nothing from that read is used");
        assert_eq!(
            shared.current().symbology().asset_id_for("ETH"),
            Some(1),
            "the pump's view too"
        );
    }

    /// The re-read asks for what the start was loaded with: a HIP-3 universe is re-read with its
    /// builder dexs, a core one without.
    #[test]
    fn the_reload_keeps_the_hip3_setting_of_the_start() {
        let meta = json!({"universe":[{"name":"BTC","szDecimals":5,"maxLeverage":40}]});
        let venue = FakeExchange {
            info_answers: HashMap::from([
                ("meta".to_string(), meta),
                ("spotMeta".to_string(), spot()),
                ("perpDexs".to_string(), json!([null])),
            ]),
            ..Default::default()
        };
        for hip3 in [false, true] {
            let start = HyperliquidInstruments::load_from(|b| venue.info(b), None, hip3).unwrap();
            assert_eq!(start.hip3(), hip3);
            let mut universe = Universe::new(SharedInstruments::new(Arc::new(start)));
            venue.info_asked.lock().unwrap().clear();
            universe.for_symbols_at(Instant::now(), &venue, &["kNEW"]);
            assert_eq!(reads(&venue).iter().any(|t| t == "perpDexs"), hip3, "hip3={hip3}");
        }
    }

    /// **A builder-dex coin is placeable with the HIP-3 flag OFF** (decision 0218): the order names `xyz:TSLA`, the
    /// universe is re-read with `perpDexs` and THAT dex's `meta` alone (never another dex, never every dex), the
    /// coin lands on the HIP-3 asset id of its dex's position, and the next reload (another coin, a minute later)
    /// folds the same dex again instead of dropping it.
    #[test]
    fn a_builder_dex_coin_is_folded_in_alone_with_the_flag_off_and_kept_by_the_next_reload() {
        let (mut universe, shared) = started();
        let core = json!({"universe":[
            {"name":"BTC","szDecimals":5,"maxLeverage":40},
            {"name":"ETH","szDecimals":4,"maxLeverage":25},
            {"name":"kNEW","szDecimals":0,"maxLeverage":5}
        ]});
        let venue = FakeExchange {
            info_answers: HashMap::from([
                ("meta".to_string(), core),
                ("spotMeta".to_string(), spot()),
                (
                    "perpDexs".to_string(),
                    json!([null, {"name":"flx"}, {"name":"xyz"}, {"name":"other"}]),
                ),
                (
                    "meta:dex:xyz".to_string(),
                    json!({"universe":[
                        {"name":"xyz:AAPL","szDecimals":3,"maxLeverage":5},
                        {"name":"xyz:TSLA","szDecimals":3,"maxLeverage":10,"onlyIsolated":true}
                    ]}),
                ),
                ("meta:dex:other".to_string(), json!({"universe":[]})),
            ]),
            ..Default::default()
        };
        let t0 = Instant::now();
        let now = universe.for_symbols_at(t0, &venue, &["xyz:TSLA"]);
        let tsla = now.symbology().by_symbol("xyz:TSLA").expect("folded in");
        assert_eq!(tsla.asset_id, 100_000 + 2 * 10_000 + 1, "dex position 2, second market");
        assert_eq!(tsla.dex.as_deref(), Some("xyz"));
        assert!(tsla.only_isolated);
        assert!(
            shared.current().symbology().by_symbol("xyz:TSLA").is_some(),
            "the pump's view too"
        );
        let asked: Vec<Value> = venue.info_asked.lock().unwrap().clone();
        let metas: Vec<Option<&str>> =
            asked.iter().filter(|b| b["type"] == "meta").map(|b| b["dex"].as_str()).collect();
        assert_eq!(metas, [None, Some("xyz")], "the core meta and ONE dex's, no other dex");
        assert_eq!(now.dexes(), ["xyz"]);

        // A coin of another dex, a minute on: the first dex is folded again, so nothing is dropped.
        let later =
            universe.for_symbols_at(t0 + RELOAD_MIN_INTERVAL, &venue, &["kNEW", "other:ZZZ"]);
        assert!(later.symbology().by_symbol("xyz:TSLA").is_some(), "the earlier dex stays");
        assert!(later.symbology().by_symbol("kNEW").is_some());
        assert_eq!(later.dexes(), ["xyz", "other"]);
    }
}
