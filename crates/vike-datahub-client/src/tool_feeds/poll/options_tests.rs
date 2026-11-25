use std::collections::BTreeMap;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use vike_marketdata::{
    OptionChain, OptionChainExpiry, OptionChainSet, OptionKind, OptionUnderlyingKind,
};
use vike_options::OptionIndex;
use vike_options::test_support::{test_delta_one, test_option};

use super::*;

const ADDRESS: &str = "<host>:7879";

fn pick(underlying: Option<&str>, expiry: Option<&str>) -> OptionsPick {
    OptionsPick { underlying: underlying.map(str::to_string), expiry: expiry.map(str::to_string) }
}

fn expiry(date: &str) -> OptionChainExpiry {
    OptionChainExpiry { date: date.to_string(), dte: 1, label: date.to_string() }
}

/// A bundle whose strip LISTS `listed` and whose chains hold `fetched` (a subset), front first.
fn bundle(underlying: &str, listed: &[&str], fetched: &[&str]) -> OptionChainSet {
    let chains: BTreeMap<String, OptionChain> = fetched
        .iter()
        .map(|d| {
            let chain = OptionChain {
                underlying: underlying.to_string(),
                underlying_kind: OptionUnderlyingKind::Crypto,
                underlying_price: Some(100.0),
                expiry: expiry(d),
                asof_ms: 0,
                source: "deribit".into(),
                rows: Vec::new(),
            };
            (d.to_string(), chain)
        })
        .collect();
    OptionChainSet {
        default_expiry: listed.first().map(|d| d.to_string()).unwrap_or_default(),
        expiries: listed.iter().map(|d| expiry(d)).collect(),
        chains,
    }
}

fn reply(underlying: &str, outcome: OptionsChainOutcome) -> OptionsChainReply {
    reply_serving(underlying, &[], outcome)
}

fn reply_serving(
    underlying: &str,
    served: &[&str],
    outcome: OptionsChainOutcome,
) -> OptionsChainReply {
    OptionsChainReply {
        venue: "deribit".into(),
        underlying: underlying.into(),
        served_underlyings: served.iter().map(|u| u.to_string()).collect(),
        outcome,
    }
}

fn state_knowing(underlyings: &[&str]) -> OptionsPollState {
    OptionsPollState {
        catalog_underlyings: underlyings.iter().map(|u| u.to_string()).collect(),
        ..OptionsPollState::default()
    }
}

// ---- the link: TTL and positions -----------------------------------------------------------------

#[test]
fn a_pick_not_renewed_within_the_ttl_lapses() {
    let (tx, _rx) = channel();
    let link = OptionsLink::new(tx);
    let t0 = Instant::now();
    link.want_at(pick(Some("BTC"), None), t0);
    assert_eq!(link.live_picks(t0 + INTEREST_TTL - Duration::from_millis(1)).len(), 1);
    assert!(link.live_picks(t0 + INTEREST_TTL).is_empty(), "a closed window stops being asked for");
}

#[test]
fn a_lapsed_pick_recorded_again_wakes_the_thread_again() {
    let (tx, rx) = channel();
    let link = OptionsLink::new(tx);
    let t0 = Instant::now();
    link.want_at(pick(None, None), t0);
    link.want_at(pick(None, None), t0 + Duration::from_millis(500));
    assert_eq!(rx.try_iter().count(), 1, "a renewed pick is not news");
    link.want_at(pick(None, None), t0 + Duration::from_millis(500) + INTEREST_TTL);
    assert_eq!(rx.try_iter().count(), 1, "a lapsed pick is");
}

/// The Greeks tool asks for each held option's OWN expiry and a hedge leg's front (its spot); a
/// name the index does not hold asks for nothing — the same treatment it gets when it is priced.
#[test]
fn positions_ask_for_their_own_expiry_and_a_hedge_leg_for_the_front() {
    let index = OptionIndex::from_instruments(&[
        test_option(
            "BTC-25SEP26-120000-C",
            "BTC",
            "2026-09-25",
            120_000.0,
            OptionKind::Call,
            "BTC",
        ),
        test_delta_one("ETH-PERPETUAL", "ETH"),
    ]);
    let link = OptionsLink::default();
    link.want_positions(["BTC-25SEP26-120000-C", "ETH-PERPETUAL", "NOT-INDEXED"], &index);
    let mut picks = link.live_picks(Instant::now());
    picks.sort_by_key(|p| format!("{p:?}"));
    let mut expected =
        vec![pick(None, None), pick(Some("BTC"), Some("2026-09-25")), pick(Some("ETH"), None)];
    expected.sort_by_key(|p| format!("{p:?}"));
    assert_eq!(picks, expected);
}

/// A USDC-settled option (linear, base SOL) asks for its own expiry of its BASE: the half of
/// `vike-app-core`'s `a_sol_usdc_position_prices_once_the_index_holds_the_usdc_row` that is the
/// poll's.
#[test]
fn a_usdc_position_asks_for_its_own_expiry_of_its_base() {
    let name = "SOL_USDC-12OCT26-96-P";
    let row = vike_model::Instrument::new(
        "deribit",
        name,
        "SOL",
        "USDC",
        vike_model::Contract::Option(vike_model::ContractOption {
            right: OptionKind::Put,
            strike: 96.0,
            expiry_ms: 1_791_792_000_000,
            style: vike_model::ContractOptionExerciseStyle::European,
            margining: vike_model::Margining::Linear,
            settle: "USDC".into(),
            value_currency: Some("SOL".into()),
            underlying: Some("SOL".into()),
        }),
    );
    let index = OptionIndex::from_instruments(&[row]);
    let link = OptionsLink::default();
    link.want_positions([name], &index);
    let wanted = link.live_picks(Instant::now());
    assert!(
        wanted
            .iter()
            .any(|p| p.underlying.as_deref() == Some("SOL")
                && p.expiry.as_deref() == Some("2026-10-12")),
        "the position asks for its own expiry of SOL: {wanted:?}"
    );
}

/// The interest outlives one poll by enough that a window redrawn once per pass never lapses, and
/// the background asks are rarer than the foreground ones.
#[test]
fn the_cadence_keeps_an_open_window_asked_for() {
    let poll = Duration::from_millis(OPTIONS_POLL_MS);
    assert_eq!(poll, Duration::from_secs(1), "the contract's cadence");
    assert!(INTEREST_TTL >= 2 * poll, "a pick must survive one slow pass");
    assert!(BACKGROUND_EVERY > poll, "unshown underlyings are asked less often than the shown one");
}

// ---- the plan ------------------------------------------------------------------------------------

#[test]
fn no_open_window_plans_nothing() {
    let mut state = state_knowing(&["BTC"]);
    assert!(state.plan(&[], &BTreeMap::new(), Instant::now()).is_empty());
}

/// The listing's order is the selector's (no name is ranked here), so its first is the default.
#[test]
fn the_default_is_the_first_known_underlying_and_the_rest_ride_in_the_background() {
    let mut state = state_knowing(&["SOL", "ETH", "BTC"]);
    let t0 = Instant::now();
    let plan = state.plan(&[pick(None, None)], &BTreeMap::new(), t0);
    let asked: Vec<&str> = plan.iter().map(|(u, _)| u.as_str()).collect();
    assert_eq!(asked, ["SOL", "ETH", "BTC"], "the listing's order, every one front-only");
    assert!(plan.iter().all(|(_, e)| e.is_empty()));

    let next = state.plan(&[pick(None, None)], &BTreeMap::new(), t0 + Duration::from_secs(1));
    assert_eq!(next, vec![("SOL".to_string(), Vec::new())], "one second later: the shown one only");

    let later = state.plan(&[pick(None, None)], &BTreeMap::new(), t0 + BACKGROUND_EVERY);
    assert_eq!(later.len(), 3, "the background comes round again");
}

#[test]
fn with_nothing_known_a_default_pick_plans_nothing() {
    let mut state = OptionsPollState::default();
    assert!(state.plan(&[pick(None, None)], &BTreeMap::new(), Instant::now()).is_empty());
}

/// A picked expiry and a position's expiry ride WITH the front, by the front's date.
#[test]
fn explicit_expiries_ride_with_the_front_once_its_date_is_known() {
    let mut state = state_knowing(&["BTC"]);
    let books =
        BTreeMap::from([("BTC".to_string(), bundle("BTC", &["2026-06-26"], &["2026-06-26"]))]);
    let picks = [
        pick(Some("BTC"), None),
        pick(Some("BTC"), Some("2026-09-25")),
        pick(Some("BTC"), Some("2026-07-31")),
    ];
    let plan = state.plan(&picks, &books, Instant::now());
    assert_eq!(
        plan,
        vec![(
            "BTC".to_string(),
            vec!["2026-06-26".to_string(), "2026-07-31".to_string(), "2026-09-25".to_string()]
        )]
    );
}

#[test]
fn an_unknown_front_is_asked_for_alone_first() {
    let mut state = state_knowing(&["BTC"]);
    let picks = [pick(Some("BTC"), None), pick(Some("BTC"), Some("2026-09-25"))];
    let plan = state.plan(&picks, &BTreeMap::new(), Instant::now());
    assert_eq!(plan, vec![("BTC".to_string(), Vec::new())]);
}

#[test]
fn a_too_many_expiries_refusal_capabilities_the_next_request() {
    let mut state = state_knowing(&["BTC"]);
    state.learn(&reply(
        "BTC",
        OptionsChainOutcome::Refused(OptionsRefusal::TooManyExpiries { max: 2 }),
    ));
    let picks = [
        pick(Some("BTC"), Some("2026-06-26")),
        pick(Some("BTC"), Some("2026-07-31")),
        pick(Some("BTC"), Some("2026-09-25")),
    ];
    let plan = state.plan(&picks, &BTreeMap::new(), Instant::now());
    assert_eq!(plan[0].1.len(), 2);
}

/// Every answer names the set the server serves, and that set — not the catalog's guess and not
/// only what has answered — is what the selector offers and the background asks for.
#[test]
fn the_served_set_feeds_the_selector_and_the_background() {
    let mut state = state_knowing(&["BTC", "XRP"]);
    let mut td = DatahubPollSnapshot::default();
    let served = ["SOL", "BTC", "ETH"];
    state.merge(
        &mut td,
        asked(vec![(
            "BTC",
            Ok(reply_serving(
                "BTC",
                &served,
                OptionsChainOutcome::Chains(bundle("BTC", &["2026-06-26"], &["2026-06-26"])),
            )),
        )]),
    );
    assert_eq!(td.options_underlyings, ["SOL", "BTC", "ETH"], "served, in the server's order");
    assert_eq!(state.known(&td.options_by_underlying), ["SOL", "BTC", "ETH"], "XRP is not served");
    let plan = state.plan(&[pick(Some("BTC"), None)], &td.options_by_underlying, Instant::now());
    let asked: Vec<&str> = plan.iter().map(|(u, _)| u.as_str()).collect();
    assert_eq!(asked, ["SOL", "BTC", "ETH"], "the unshown served ones ride in the background");
    assert!(plan.iter().all(|(_, e)| e.is_empty()), "front expiry only");
}

/// A server that serves the venue nothing (`NotArmed`) empties the served set, and the selector
/// falls back to what has answered.
#[test]
fn an_empty_served_set_falls_back_to_what_answered() {
    let mut state = OptionsPollState::default();
    state.learn(&reply_serving("BTC", &["BTC", "ETH"], OptionsChainOutcome::NotArmed));
    state.learn(&reply("BTC", OptionsChainOutcome::NotArmed));
    let books = BTreeMap::from([("ETH".to_string(), bundle("ETH", &["d"], &["d"]))]);
    assert_eq!(state.selector(&books), ["ETH"]);
}

// ---- a switched datahub --------------------------------------------------------------------------

fn dial(addr: &str, epoch: u64) -> DatahubDial {
    DatahubDial { address: addr.into(), keys: None, epoch }
}

/// **Another server's data is never shown as last good.** A changed address, a new device key (its
/// epoch moved) and a lost datahub each clear the chains, the selector, the index and the status at once, and
/// forget what the old server taught; the same dial again clears nothing.
#[test]
fn a_switched_datahub_clears_everything_the_old_one_answered() {
    let filled = || {
        let mut td = td_with(&[("BTC", bundle("BTC", &["2026-06-26"], &["2026-06-26"]))]);
        td.options_underlyings = vec!["BTC".into()];
        td.options_index = OptionIndex::from_instruments(&[test_delta_one("BTC-PERPETUAL", "BTC")]);
        td.options_status = "LIVE · Deribit via the datahub at a:1".into();
        td
    };
    let a = dial("a:1", 0);
    for next in [Some(dial("b:1", 0)), Some(dial("a:1", 1)), None] {
        let mut state = OptionsPollState::default();
        let mut td = filled();
        assert!(!state.observe_dial(&mut td, Some(&a)), "the first dial clears nothing");
        assert!(!state.observe_dial(&mut td, Some(&a)), "nor does the same dial again");
        assert_eq!(td.options_by_underlying.len(), 1);
        state.served_underlyings = ["BTC".to_string()].into();
        state.expiry_cap = Some(2);

        assert!(state.observe_dial(&mut td, next.as_ref()), "{next:?}");
        assert!(td.options_by_underlying.is_empty(), "{next:?}: the chains");
        assert!(td.options_underlyings.is_empty(), "{next:?}: the selector");
        assert!(td.options_index.is_empty(), "{next:?}: the index");
        assert!(td.options_status.is_empty(), "{next:?}: the status");
        assert!(state.served_underlyings.is_empty() && state.expiry_cap.is_none(), "{next:?}");
        assert!(
            !state.observe_dial(&mut td, next.as_ref()),
            "{next:?}: and it is now the seen one"
        );
    }
}

// ---- the merge and its sentences -----------------------------------------------------------------

fn asked(replies: Vec<(&str, Result<OptionsChainReply, DatahubLinkError>)>) -> OptionsPass {
    OptionsPass::Asked {
        addr: ADDRESS.to_string(),
        replies: replies.into_iter().map(|(u, r)| (u.to_string(), r)).collect(),
    }
}

fn td_with(books: &[(&str, OptionChainSet)]) -> DatahubPollSnapshot {
    let mut td = DatahubPollSnapshot::default();
    for (u, b) in books {
        td.options_by_underlying.insert(u.to_string(), b.clone());
    }
    td
}

#[test]
fn an_answer_replaces_its_underlying_and_a_refusal_keeps_the_last_good() {
    let old_btc = bundle("BTC", &["2026-06-26"], &["2026-06-26"]);
    let eth = bundle("ETH", &["2026-06-26"], &["2026-06-26"]);
    let mut td = td_with(&[("BTC", old_btc), ("ETH", eth.clone())]);
    let new_btc = bundle("BTC", &["2026-06-26", "2026-07-31"], &["2026-07-31"]);
    let unarmed = reply("ETH", OptionsChainOutcome::NotArmed);
    let sentence = unarmed.describe();
    OptionsPollState::default().merge(
        &mut td,
        asked(vec![
            ("BTC", Ok(reply("BTC", OptionsChainOutcome::Chains(new_btc.clone())))),
            ("ETH", Ok(unarmed)),
        ]),
    );
    assert_eq!(td.options_by_underlying["BTC"], new_btc, "an answer replaces the bundle whole");
    assert_eq!(td.options_by_underlying["ETH"], eth, "a refusal keeps the last good bundle");
    assert_eq!(td.options_status, sentence, "the status is the server's own sentence");
}

#[test]
fn a_failed_request_keeps_the_chains_and_names_the_server() {
    let btc = bundle("BTC", &["2026-06-26"], &["2026-06-26"]);
    let mut td = td_with(&[("BTC", btc.clone())]);
    // A desync leaves the client usable: the datahub answered, so it is said at once.
    let desync = DatahubLinkError { cause: "protocol desync".into(), transient: false };
    OptionsPollState::default().merge(&mut td, asked(vec![("BTC", Err(desync))]));
    assert_eq!(td.options_by_underlying["BTC"], btc);
    assert!(td.options_status.contains(ADDRESS), "{}", td.options_status);
    assert!(td.options_status.contains("protocol desync"), "{}", td.options_status);
    assert!(!td.options_status.starts_with("LIVE"), "{}", td.options_status);
}

/// **A failed LINK is transient**: the first two passes of a streak keep the chains under the quiet
/// reconnecting line, the third names the cause, and a pass that answers ends the streak.
#[test]
fn a_link_failure_streak_is_quiet_for_two_passes_and_names_its_cause_on_the_third() {
    let btc = bundle("BTC", &["2026-06-26"], &["2026-06-26"]);
    let mut td = td_with(&[("BTC", btc.clone())]);
    let mut state = OptionsPollState::default();
    let aborted = || DatahubLinkError {
        cause: "connection aborted (os error 10053)".into(),
        transient: true,
    };
    for pass in 1..=2 {
        state.merge(&mut td, asked(vec![("BTC", Err(aborted()))]));
        assert_eq!(td.options_status, RECONNECTING_LINE, "pass {pass} stays quiet");
        assert_eq!(td.options_by_underlying["BTC"], btc, "the chains stay");
    }
    let dial = OptionsPass::ConnectFailed { addr: ADDRESS.into(), error: aborted() };
    state.merge(&mut td, dial);
    assert!(
        td.options_status.contains("os error 10053"),
        "the third names it: {}",
        td.options_status
    );
    assert!(td.options_status.contains(ADDRESS), "{}", td.options_status);

    let chains = OptionsChainOutcome::Chains(btc.clone());
    state.merge(&mut td, asked(vec![("BTC", Ok(reply("BTC", chains)))]));
    assert!(td.options_status.starts_with("LIVE"), "an answer ends it: {}", td.options_status);
    state.merge(&mut td, asked(vec![("BTC", Err(aborted()))]));
    assert_eq!(td.options_status, RECONNECTING_LINE, "a new streak starts quiet");
}

/// No backend, a dead link and an old server are three different facts, and none of them may read
/// as LIVE or wipe the chains already on screen.
#[test]
fn every_pass_that_asked_nothing_says_why_and_keeps_the_chains() {
    let btc = bundle("BTC", &["2026-06-26"], &["2026-06-26"]);
    let cases = [
        (OptionsPass::NoBackend, NO_BACKEND_NOTE.to_string()),
        (
            // A dial the datahub REFUSED (a denied mac) is said at once; a dead link is not
            // (`a_link_failure_streak_is_quiet_for_two_passes_and_names_its_cause_on_the_third`).
            OptionsPass::ConnectFailed {
                addr: ADDRESS.into(),
                error: DatahubLinkError { cause: "auth denied".into(), transient: false },
            },
            "could not be reached".to_string(),
        ),
        (
            OptionsPass::ServerUnsupported {
                addr: ADDRESS.into(),
                features: vec!["load_bars".into(), "venue_catalog".into()],
            },
            format!("\"{FEATURE_OPTIONS_CHAIN}\""),
        ),
    ];
    for (pass, needle) in cases {
        let mut td = td_with(&[("BTC", btc.clone())]);
        OptionsPollState::default().merge(&mut td, pass);
        assert!(td.options_status.contains(&needle), "{needle}: {}", td.options_status);
        assert!(!td.options_status.starts_with("LIVE"), "{}", td.options_status);
        assert_eq!(td.options_by_underlying["BTC"], btc, "no unasked pass touches the chains");
    }
    let unsupported = OptionsPass::ServerUnsupported {
        addr: ADDRESS.into(),
        features: vec!["load_bars".into(), "venue_catalog".into()],
    };
    let line = unsupported.not_asked_line().expect("a sentence");
    assert!(line.contains("load_bars, venue_catalog"), "the advertised set is named: {line}");
    assert!(line.contains("nothing was sent"), "{line}");
}

#[test]
fn a_clean_pass_reads_live_and_orders_the_selector_by_rank() {
    let mut td = DatahubPollSnapshot {
        options_index: OptionIndex::from_instruments(&[test_delta_one("BTC-PERPETUAL", "BTC")]),
        ..DatahubPollSnapshot::default()
    };
    OptionsPollState::default().merge(
        &mut td,
        asked(vec![
            ("SOL", Ok(reply("SOL", OptionsChainOutcome::Chains(bundle("SOL", &["d"], &["d"]))))),
            ("BTC", Ok(reply("BTC", OptionsChainOutcome::Chains(bundle("BTC", &["d"], &["d"]))))),
        ]),
    );
    assert_eq!(td.options_underlyings, ["BTC", "SOL"]);
    assert!(td.options_status.starts_with("LIVE"), "{}", td.options_status);
    assert!(td.options_status.contains(ADDRESS), "{}", td.options_status);
}

#[test]
fn an_empty_index_says_why_beside_the_live_line() {
    let mut td = DatahubPollSnapshot::default();
    let mut state = OptionsPollState::default();
    assert!(
        state.adopt_index(CatalogFetchReport::Refused { why: "lane refused".into() }).is_none()
    );
    state.merge(
        &mut td,
        asked(vec![(
            "BTC",
            Ok(reply("BTC", OptionsChainOutcome::Chains(bundle("BTC", &["d"], &["d"])))),
        )]),
    );
    assert!(td.options_status.starts_with("LIVE"), "{}", td.options_status);
    assert!(td.options_status.contains("lane refused"), "{}", td.options_status);
}

// ---- the index -----------------------------------------------------------------------------------

#[test]
fn a_listing_becomes_the_index_and_names_the_option_underlyings() {
    let mut state = OptionsPollState::default();
    let index = state
        .adopt_index(CatalogFetchReport::Listed {
            instruments: vec![
                test_option(
                    "BTC-26JUN26-100000-C",
                    "BTC",
                    "2026-06-26",
                    100_000.0,
                    OptionKind::Call,
                    "BTC",
                ),
                test_option(
                    "SOL_USDC-26JUN26-45-P",
                    "SOL",
                    "2026-06-26",
                    45.0,
                    OptionKind::Put,
                    "USDC",
                ),
                test_delta_one("ETH-PERPETUAL", "ETH"),
            ],
            truncated: false,
            cached: true,
        })
        .expect("a listing is adopted");
    assert_eq!(index.len(), 3);
    assert!(index.address("SOL_USDC-26JUN26-45-P").is_some(), "looked up, never parsed");
    let known: Vec<&str> = state.catalog_underlyings.iter().map(String::as_str).collect();
    assert_eq!(known, ["BTC", "SOL"], "only OPTION underlyings fill the selector");
    assert!(state.index_note.is_none());
}

#[test]
fn a_refused_listing_keeps_the_last_good_index_and_says_why() {
    let mut state = OptionsPollState::default();
    let refused = state.adopt_index(CatalogFetchReport::NoBackend);
    assert!(refused.is_none(), "the caller keeps the index it had");
    assert!(state.index_note.as_deref().is_some_and(|n| n.contains("config.datahub_addr")));
}

// ---- the selection over a partial bundle ---------------------------------------------------------

#[test]
fn a_pick_the_server_lists_but_has_not_sent_survives_and_is_asked_for() {
    let (tx, rx) = channel();
    let mut td = td_with(&[("BTC", bundle("BTC", &["2026-06-26", "2026-07-31"], &["2026-06-26"]))]);
    td.options_underlyings = vec!["BTC".into()];
    td.options_link = OptionsLink::new(tx);
    let mut expiry_selection = Some("2026-07-31".to_string());
    let shown = options_selection(&td, &None, &mut expiry_selection).expect("a bundle");
    assert_eq!(shown.default_expiry, "2026-06-26");
    assert_eq!(
        expiry_selection.as_deref(),
        Some("2026-07-31"),
        "a pending pick is not an invalid one"
    );
    assert!(rx.try_recv().is_ok(), "a new pick wakes the thread");
    assert_eq!(td.options_link.live_picks(Instant::now()), vec![pick(None, Some("2026-07-31"))]);
}

#[test]
fn a_pick_the_server_does_not_list_is_cleared() {
    let mut td = td_with(&[("BTC", bundle("BTC", &["2026-06-26"], &["2026-06-26"]))]);
    td.options_underlyings = vec!["BTC".into()];
    let mut expiry_selection = Some("2027-12-31".to_string());
    assert!(options_selection(&td, &None, &mut expiry_selection).is_some());
    assert_eq!(expiry_selection, None, "the underlying's front takes over");
    assert_eq!(td.options_link.live_picks(Instant::now()), vec![pick(None, None)]);
}

/// The selector lists every SERVED underlying, so a pick can name one that has not answered yet:
/// the grid shows nothing for it — never another underlying's chains under its name — and the
/// pick is asked for.
#[test]
fn a_served_underlying_that_has_not_answered_shows_nothing_and_is_asked_for() {
    let mut td = td_with(&[("BTC", bundle("BTC", &["2026-06-26"], &["2026-06-26"]))]);
    td.options_underlyings = vec!["BTC".into(), "ETH".into()];
    let mut expiry_selection = None;
    assert!(options_selection(&td, &Some("ETH".into()), &mut expiry_selection).is_none());
    assert_eq!(td.options_link.live_picks(Instant::now()), vec![pick(Some("ETH"), None)]);
}
