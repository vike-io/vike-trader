use std::collections::BTreeMap;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use vike_datahub_client::FEATURE_OPTIONS_CHAIN;
use vike_datahub_client::options::{OptionsChainOutcome, OptionsChainReply, OptionsRefusal};
use vike_marketdata::{Expiry, OptionChain, OptionKind, UnderlyingChains, UnderlyingKind};
use vike_options::OptionIndex;

use super::super::options_wire::OptionsPass;
use super::super::view::ToolData;
use super::*;
use crate::data::catalog_wire::{CatalogFetchReport, NO_BACKEND_NOTE};

const ADDR: &str = "<host>:7879";

fn pick(underlying: Option<&str>, expiry: Option<&str>) -> Pick {
    Pick { underlying: underlying.map(str::to_string), expiry: expiry.map(str::to_string) }
}

fn expiry(date: &str) -> Expiry {
    Expiry { date: date.to_string(), dte: 1, label: date.to_string() }
}

/// A bundle whose strip LISTS `listed` and whose chains hold `fetched` (a subset), front first.
fn bundle(underlying: &str, listed: &[&str], fetched: &[&str]) -> UnderlyingChains {
    let chains: BTreeMap<String, OptionChain> = fetched
        .iter()
        .map(|d| {
            let chain = OptionChain {
                underlying: underlying.to_string(),
                underlying_kind: UnderlyingKind::Crypto,
                underlying_price: Some(100.0),
                expiry: expiry(d),
                asof_ms: 0,
                source: "deribit".into(),
                rows: Vec::new(),
            };
            (d.to_string(), chain)
        })
        .collect();
    UnderlyingChains {
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

fn state_knowing(underlyings: &[&str]) -> PollState {
    PollState {
        catalog_underlyings: underlyings.iter().map(|u| u.to_string()).collect(),
        ..PollState::default()
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

/// The interest outlives one poll by enough that a window redrawn once per pass never lapses, and
/// the background asks are rarer than the foreground ones.
#[test]
fn the_cadence_keeps_an_open_window_asked_for() {
    let poll = Duration::from_millis(OPT_POLL_MS);
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

#[test]
fn the_default_is_the_first_known_underlying_and_the_rest_ride_in_the_background() {
    let mut state = state_knowing(&["SOL", "ETH", "BTC"]);
    let t0 = Instant::now();
    let plan = state.plan(&[pick(None, None)], &BTreeMap::new(), t0);
    let asked: Vec<&str> = plan.iter().map(|(u, _)| u.as_str()).collect();
    assert_eq!(asked, ["BTC", "ETH", "SOL"], "selector order, every one front-only");
    assert!(plan.iter().all(|(_, e)| e.is_empty()));

    let next = state.plan(&[pick(None, None)], &BTreeMap::new(), t0 + Duration::from_secs(1));
    assert_eq!(next, vec![("BTC".to_string(), Vec::new())], "one second later: the shown one only");

    let later = state.plan(&[pick(None, None)], &BTreeMap::new(), t0 + BACKGROUND_EVERY);
    assert_eq!(later.len(), 3, "the background comes round again");
}

#[test]
fn with_nothing_known_a_default_pick_plans_nothing() {
    let mut state = PollState::default();
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
fn a_too_many_expiries_refusal_caps_the_next_request() {
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
    let mut td = ToolData::default();
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
    assert_eq!(td.opt_underlyings, ["BTC", "ETH", "SOL"], "served, in canonical order");
    assert_eq!(state.known(&td.opt_by_underlying), ["BTC", "ETH", "SOL"], "XRP is not served");
    let plan = state.plan(&[pick(Some("BTC"), None)], &td.opt_by_underlying, Instant::now());
    let asked: Vec<&str> = plan.iter().map(|(u, _)| u.as_str()).collect();
    assert_eq!(asked, ["BTC", "ETH", "SOL"], "the unshown served ones ride in the background");
    assert!(plan.iter().all(|(_, e)| e.is_empty()), "front expiry only");
}

/// A server that serves the venue nothing (`NotArmed`) empties the served set, and the selector
/// falls back to what has answered.
#[test]
fn an_empty_served_set_falls_back_to_what_answered() {
    let mut state = PollState::default();
    state.learn(&reply_serving("BTC", &["BTC", "ETH"], OptionsChainOutcome::NotArmed));
    state.learn(&reply("BTC", OptionsChainOutcome::NotArmed));
    let books = BTreeMap::from([("ETH".to_string(), bundle("ETH", &["d"], &["d"]))]);
    assert_eq!(state.selector(&books), ["ETH"]);
}

// ---- a switched datahub --------------------------------------------------------------------------

fn dial(addr: &str, key_name: &str) -> CatalogDial {
    CatalogDial { addr: addr.into(), key_name: key_name.into(), keys: None }
}

/// **Another server's data is never shown as last good.** A changed address, a changed key name
/// and a lost datahub each clear the chains, the selector, the index and the status at once, and
/// forget what the old server taught; the same dial again clears nothing.
#[test]
fn a_switched_datahub_clears_everything_the_old_one_answered() {
    let filled = || {
        let mut td = td_with(&[("BTC", bundle("BTC", &["2026-06-26"], &["2026-06-26"]))]);
        td.opt_underlyings = vec!["BTC".into()];
        td.opt_index = OptionIndex::from_instruments(&[test_delta_one("BTC-PERPETUAL", "BTC")]);
        td.opt_status = "LIVE · Deribit via the datahub at a:1".into();
        td
    };
    let a = dial("a:1", "VIKE_DATAHUB_OBSERVE_KEY");
    for next in [
        Some(dial("b:1", "VIKE_DATAHUB_OBSERVE_KEY")),
        Some(dial("a:1", "OTHER_OBSERVE_KEY")),
        None,
    ] {
        let mut state = PollState::default();
        let mut td = filled();
        assert!(!state.observe_dial(&mut td, Some(&a)), "the first dial clears nothing");
        assert!(!state.observe_dial(&mut td, Some(&a)), "nor does the same dial again");
        assert_eq!(td.opt_by_underlying.len(), 1);
        state.served_underlyings = ["BTC".to_string()].into();
        state.expiry_cap = Some(2);

        assert!(state.observe_dial(&mut td, next.as_ref()), "{next:?}");
        assert!(td.opt_by_underlying.is_empty(), "{next:?}: the chains");
        assert!(td.opt_underlyings.is_empty(), "{next:?}: the selector");
        assert!(td.opt_index.is_empty(), "{next:?}: the index");
        assert!(td.opt_status.is_empty(), "{next:?}: the status");
        assert!(state.served_underlyings.is_empty() && state.expiry_cap.is_none(), "{next:?}");
        assert!(
            !state.observe_dial(&mut td, next.as_ref()),
            "{next:?}: and it is now the seen one"
        );
    }
}

// ---- the merge and its sentences -----------------------------------------------------------------

fn asked(replies: Vec<(&str, Result<OptionsChainReply, String>)>) -> OptionsPass {
    OptionsPass::Asked {
        addr: ADDR.to_string(),
        replies: replies.into_iter().map(|(u, r)| (u.to_string(), r)).collect(),
    }
}

fn td_with(books: &[(&str, UnderlyingChains)]) -> ToolData {
    let mut td = ToolData::default();
    for (u, b) in books {
        td.opt_by_underlying.insert(u.to_string(), b.clone());
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
    PollState::default().merge(
        &mut td,
        asked(vec![
            ("BTC", Ok(reply("BTC", OptionsChainOutcome::Chains(new_btc.clone())))),
            ("ETH", Ok(unarmed)),
        ]),
    );
    assert_eq!(td.opt_by_underlying["BTC"], new_btc, "an answer replaces the bundle whole");
    assert_eq!(td.opt_by_underlying["ETH"], eth, "a refusal keeps the last good bundle");
    assert_eq!(td.opt_status, sentence, "the status is the server's own sentence");
}

#[test]
fn a_failed_request_keeps_the_chains_and_names_the_server() {
    let btc = bundle("BTC", &["2026-06-26"], &["2026-06-26"]);
    let mut td = td_with(&[("BTC", btc.clone())]);
    PollState::default().merge(&mut td, asked(vec![("BTC", Err("protocol desync".into()))]));
    assert_eq!(td.opt_by_underlying["BTC"], btc);
    assert!(td.opt_status.contains(ADDR), "{}", td.opt_status);
    assert!(td.opt_status.contains("protocol desync"), "{}", td.opt_status);
    assert!(!td.opt_status.starts_with("LIVE"), "{}", td.opt_status);
}

/// No backend, a dead link and an old server are three different facts, and none of them may read
/// as LIVE or wipe the chains already on screen.
#[test]
fn every_pass_that_asked_nothing_says_why_and_keeps_the_chains() {
    let btc = bundle("BTC", &["2026-06-26"], &["2026-06-26"]);
    let cases = [
        (OptionsPass::NoBackend, NO_BACKEND_NOTE.to_string()),
        (
            OptionsPass::ConnectFailed { addr: ADDR.into(), error: "connection refused".into() },
            "could not be reached".to_string(),
        ),
        (
            OptionsPass::ServerUnsupported {
                addr: ADDR.into(),
                features: vec!["load_bars".into(), "venue_catalog".into()],
            },
            format!("\"{FEATURE_OPTIONS_CHAIN}\""),
        ),
    ];
    for (pass, needle) in cases {
        let mut td = td_with(&[("BTC", btc.clone())]);
        PollState::default().merge(&mut td, pass);
        assert!(td.opt_status.contains(&needle), "{needle}: {}", td.opt_status);
        assert!(!td.opt_status.starts_with("LIVE"), "{}", td.opt_status);
        assert_eq!(td.opt_by_underlying["BTC"], btc, "no unasked pass touches the chains");
    }
    let unsupported = OptionsPass::ServerUnsupported {
        addr: ADDR.into(),
        features: vec!["load_bars".into(), "venue_catalog".into()],
    };
    let line = unsupported.not_asked_line().expect("a sentence");
    assert!(line.contains("load_bars, venue_catalog"), "the advertised set is named: {line}");
    assert!(line.contains("nothing was sent"), "{line}");
}

#[test]
fn a_clean_pass_reads_live_and_orders_the_selector_by_rank() {
    let mut td = ToolData {
        opt_index: OptionIndex::from_instruments(&[test_delta_one("BTC-PERPETUAL", "BTC")]),
        ..ToolData::default()
    };
    PollState::default().merge(
        &mut td,
        asked(vec![
            ("SOL", Ok(reply("SOL", OptionsChainOutcome::Chains(bundle("SOL", &["d"], &["d"]))))),
            ("BTC", Ok(reply("BTC", OptionsChainOutcome::Chains(bundle("BTC", &["d"], &["d"]))))),
        ]),
    );
    assert_eq!(td.opt_underlyings, ["BTC", "SOL"]);
    assert!(td.opt_status.starts_with("LIVE"), "{}", td.opt_status);
    assert!(td.opt_status.contains(ADDR), "{}", td.opt_status);
}

#[test]
fn an_empty_index_says_why_beside_the_live_line() {
    let mut td = ToolData::default();
    let mut state = PollState::default();
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
    assert!(td.opt_status.starts_with("LIVE"), "{}", td.opt_status);
    assert!(td.opt_status.contains("lane refused"), "{}", td.opt_status);
}

// ---- the index -----------------------------------------------------------------------------------

#[test]
fn a_listing_becomes_the_index_and_names_the_option_underlyings() {
    let mut state = PollState::default();
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
    let mut state = PollState::default();
    let refused = state.adopt_index(CatalogFetchReport::NoBackend);
    assert!(refused.is_none(), "the caller keeps the index it had");
    assert!(state.index_note.as_deref().is_some_and(|n| n.contains("config.datahub_addr")));
}

// ---- the selection over a partial bundle ---------------------------------------------------------

#[test]
fn a_pick_the_server_lists_but_has_not_sent_survives_and_is_asked_for() {
    let (tx, rx) = channel();
    let mut td = td_with(&[("BTC", bundle("BTC", &["2026-06-26", "2026-07-31"], &["2026-06-26"]))]);
    td.opt_underlyings = vec!["BTC".into()];
    td.opt_link = OptionsLink::new(tx);
    let mut expiry_sel = Some("2026-07-31".to_string());
    let shown = options_selection(&td, &None, &mut expiry_sel).expect("a bundle");
    assert_eq!(shown.default_expiry, "2026-06-26");
    assert_eq!(expiry_sel.as_deref(), Some("2026-07-31"), "a pending pick is not an invalid one");
    assert!(rx.try_recv().is_ok(), "a new pick wakes the thread");
    assert_eq!(td.opt_link.live_picks(Instant::now()), vec![pick(None, Some("2026-07-31"))]);
}

#[test]
fn a_pick_the_server_does_not_list_is_cleared() {
    let mut td = td_with(&[("BTC", bundle("BTC", &["2026-06-26"], &["2026-06-26"]))]);
    td.opt_underlyings = vec!["BTC".into()];
    let mut expiry_sel = Some("2027-12-31".to_string());
    assert!(options_selection(&td, &None, &mut expiry_sel).is_some());
    assert_eq!(expiry_sel, None, "the underlying's front takes over");
    assert_eq!(td.opt_link.live_picks(Instant::now()), vec![pick(None, None)]);
}

/// The selector lists every SERVED underlying, so a pick can name one that has not answered yet:
/// the grid shows nothing for it — never another underlying's chains under its name — and the
/// pick is asked for.
#[test]
fn a_served_underlying_that_has_not_answered_shows_nothing_and_is_asked_for() {
    let mut td = td_with(&[("BTC", bundle("BTC", &["2026-06-26"], &["2026-06-26"]))]);
    td.opt_underlyings = vec!["BTC".into(), "ETH".into()];
    let mut expiry_sel = None;
    assert!(options_selection(&td, &Some("ETH".into()), &mut expiry_sel).is_none());
    assert_eq!(td.opt_link.live_picks(Instant::now()), vec![pick(Some("ETH"), None)]);
}
