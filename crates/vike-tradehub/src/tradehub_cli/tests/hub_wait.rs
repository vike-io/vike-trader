//! The start's wait for the hubs a box cannot run without (`tradehub_cli/hub_wait.rs`), over scripted asks, a
//! recording `say` and a recording `pause` (no clock, no socket), and which refusals of the live start park the daemon
//! (`live_runtime.rs`'s `LiveStartFailed`).

use std::collections::VecDeque;
use std::time::Duration;

use vike_model::AccountTier::{Demo, Paper};
use vike_model::ArmingBlock;
use vike_model::accounts::account_keys::AccountLabel;
use vike_secrets::account_directory::AccountDirectory;
use vike_tradehub_core::{AdapterPolicy, ArmingNotRunning, RuntimeError};

use super::super::hub_wait::{
    HubAsk, HubWaitNews, STOPPED_WAITING, hub_wait, hub_wait_pauses, wait_for_the_hubs,
};
use super::super::live_runtime::LiveStartFailed;
use super::*;

const GATEWAY_DOWN: &str = "the gatewayhub at 127.0.0.1:7900 (connection refused)";
const DATAHUB_DOWN: &str =
    "the datahub (datahub 127.0.0.1:7878: strategy lane: connection refused)";

/// The rows of one bybit `demo` account: a box that runs nothing without the hubs.
fn demo_rows() -> AccountDirectory {
    AdapterPolicy::default().with_account("bybit", &AccountLabel::Default, Demo).accounts
}

/// Run [`hub_wait`] over `answers` (the asks, in order), returning what it returned, what it said and every pause.
fn waited(
    answers: Vec<HubAsk>,
) -> (Result<AccountDirectory, String>, Vec<HubWaitNews>, Vec<Duration>, usize) {
    let mut answers: VecDeque<HubAsk> = answers.into();
    let mut asked = 0;
    let mut news = Vec::new();
    let mut pauses = Vec::new();
    let out = hub_wait(
        || {
            asked += 1;
            answers.pop_front().expect("the script ran out of answers")
        },
        |n| news.push(n),
        |d| pauses.push(d),
    );
    (out, news, pauses, asked)
}

fn wait(why: &str) -> HubAsk {
    HubAsk::Wait(why.to_string())
}

#[test]
fn a_gatewayhub_that_is_down_is_waited_for_on_the_shared_ladder_and_the_start_goes_on_when_it_answers()
 {
    let last =
        AdapterPolicy::default().with_account("binance", &AccountLabel::Default, Demo).accounts;
    let (out, news, pauses, asked) = waited(vec![
        wait(GATEWAY_DOWN),
        wait(GATEWAY_DOWN),
        wait(GATEWAY_DOWN),
        HubAsk::Go(last.clone()),
    ]);
    assert_eq!(out, Ok(last), "the rows of the LAST ask go on to the assembly");
    assert_eq!(asked, 4);
    assert_eq!(pauses, hub_wait_pauses()[..3], "the shared 5, 10, 20 s ladder");
    assert_eq!(
        news,
        [
            HubWaitNews::Waiting(GATEWAY_DOWN.to_string()),
            HubWaitNews::Answered { waited: Duration::from_secs(35) },
        ],
        "one reason is said once, and the answer after it"
    );
}

#[test]
fn past_the_first_ladder_it_asks_every_max_poll_and_says_so_each_round() {
    // The first ladder (5 asks), then two rounds of ten, then the answer.
    let mut answers: Vec<HubAsk> = (0..25).map(|_| wait(GATEWAY_DOWN)).collect();
    answers.push(HubAsk::Go(demo_rows()));
    let (out, news, pauses, asked) = waited(answers);
    assert!(out.is_ok());
    assert_eq!(asked, 26);
    assert_eq!(pauses[..4], hub_wait_pauses());
    assert_eq!(
        hub_wait_pauses(),
        [5, 10, 20, 30].map(Duration::from_secs),
        "the first ladder, verbatim"
    );
    assert!(
        pauses[4..].iter().all(|p| *p == crate::late_arm::MAX_POLL),
        "past the ladder, every pause is the longest: {pauses:?}"
    );
    let rounds: Vec<&HubWaitNews> =
        news.iter().filter(|n| matches!(n, HubWaitNews::StillWaiting { .. })).collect();
    assert_eq!(
        rounds,
        [
            &HubWaitNews::StillWaiting {
                waited: Duration::from_secs(65),
                why: GATEWAY_DOWN.to_string()
            },
            &HubWaitNews::StillWaiting {
                waited: Duration::from_secs(365),
                why: GATEWAY_DOWN.to_string()
            },
            &HubWaitNews::StillWaiting {
                waited: Duration::from_secs(665),
                why: GATEWAY_DOWN.to_string()
            },
        ],
        "one line at the end of the ladder and of each ten-ask round"
    );
    assert_eq!(
        news.iter().filter(|n| matches!(n, HubWaitNews::Waiting(_))).count(),
        1,
        "the reason did not change, so it is said once"
    );
}

#[test]
fn a_datahub_that_is_down_is_waited_for_after_the_gatewayhub_answers() {
    let (out, news, _, _) = waited(vec![
        wait(GATEWAY_DOWN),
        wait(DATAHUB_DOWN),
        wait(DATAHUB_DOWN),
        HubAsk::Go(demo_rows()),
    ]);
    assert!(out.is_ok());
    assert_eq!(
        news,
        [
            HubWaitNews::Waiting(GATEWAY_DOWN.to_string()),
            HubWaitNews::Waiting(DATAHUB_DOWN.to_string()),
            HubWaitNews::Answered { waited: Duration::from_secs(35) },
        ],
        "the phase moves to the datahub's words once the gatewayhub answers"
    );
}

#[test]
fn a_paper_account_activated_during_the_wait_ends_it() {
    let with_paper = AdapterPolicy::default()
        .with_account("bybit", &AccountLabel::Default, Demo)
        .with_account("okx", &AccountLabel::Default, Paper)
        .accounts;
    assert_eq!(
        crate::account_follow::nothing_runs_without_the_hubs(&with_paper, &["bybit".to_string()]),
        None,
        "a paper account runs without the hubs, so the production ask answers Go"
    );
    let (out, _, pauses, _) = waited(vec![wait(GATEWAY_DOWN), HubAsk::Go(with_paper.clone())]);
    assert_eq!(out, Ok(with_paper), "the start goes on over the rows that hold the paper account");
    assert_eq!(pauses, hub_wait_pauses()[..1]);
}

#[test]
fn a_stop_during_the_wait_ends_it_naming_the_wait() {
    let (out, news, _, asked) =
        waited(vec![wait(GATEWAY_DOWN), wait(GATEWAY_DOWN), HubAsk::Stopped]);
    assert_eq!(out, Err(STOPPED_WAITING.to_string()));
    assert_eq!(asked, 3, "nothing is asked after the stop");
    assert!(!news.iter().any(|n| matches!(n, HubWaitNews::Answered { .. })));
    let failed = LiveStartFailed::from(out.expect_err("stopped"));
    assert!(!failed.operator_needed, "a stop is no refusal: it is not parked");
}

/// A dial whose link holds `trade_key` (empty: none).
fn dial(trade_key: &[u8]) -> GatewayDial {
    GatewayDial {
        link: vike_gatewayhub_client::client::GatewayLink::new(
            "127.0.0.1:1",
            vike_hub_protocol::auth::HubKeys::new(Vec::new(), Vec::new())
                .with_trade(trade_key.to_vec()),
        ),
        connect_wait: Duration::from_secs(1),
        progress: None,
        stop: None,
    }
}

#[test]
fn a_box_without_the_gatewayhub_key_does_not_wait_and_is_refused_as_setup() {
    let refused = wait_for_the_hubs(
        demo_rows(),
        &["bybit".to_string()],
        &dial(b""),
        &StubCtors { refusal: None },
    )
    .expect_err("nothing in this process can mint the key, so nothing is waited for");
    assert!(refused.operator_needed, "setup: the supervisor parks it (exit 78)");
    assert!(
        refused.why.contains("setup needed: run `vike-cli gatewayhub setup`"),
        "{}",
        refused.why
    );
}

#[test]
fn a_box_with_a_paper_account_does_not_wait_and_asks_nothing() {
    let rows = AdapterPolicy::default()
        .with_account("bybit", &AccountLabel::Default, Demo)
        .with_account("okx", &AccountLabel::Default, Paper)
        .accounts;
    // A key and an address nothing listens on: an ask would wait, so `Ok` at once proves nothing was asked.
    let out = wait_for_the_hubs(
        rows.clone(),
        &["bybit".to_string()],
        &dial(b"k"),
        &StubCtors { refusal: None },
    );
    assert_eq!(out.map_err(|e| e.why), Ok(rows));
}

fn not_running(venue: &str, block: ArmingBlock) -> ArmingNotRunning {
    ArmingNotRunning {
        venue: venue.to_string(),
        account: AccountLabel::Default,
        tier: Some(Demo),
        block,
    }
}

/// **Which assembly refusals park the daemon** (owner 2026-11-14: never a restart loop): a refusal only an operator
/// mends exits 78; the OS keeps the restart. No account running is always an operator's: the assembly refuses it only
/// when none waits for its venue (one that does, an IB Gateway that is down, starts the node with no engine, owner
/// 2026-11-15; `crates/vike-tradehub-core/src/node/hot_tests.rs`).
#[test]
fn a_refusal_only_an_operator_mends_parks_and_a_failure_of_the_os_does_not() {
    let own_cause = RuntimeError::NoAccountRuns {
        not_running: vec![not_running("bybit", ArmingBlock::NoCredentials)],
    };
    assert!(LiveStartFailed::assembly(&own_cause).operator_needed);
    assert!(
        LiveStartFailed::assembly(&RuntimeError::NoAccountRuns { not_running: Vec::new() })
            .operator_needed,
        "no rows at all"
    );
    assert!(LiveStartFailed::assembly(&RuntimeError::Mount("m".to_string())).operator_needed);
    assert!(
        !LiveStartFailed::assembly(&RuntimeError::ForwarderSpawn(std::io::Error::other("os")))
            .operator_needed
    );
    assert!(
        !LiveStartFailed::from("refusing LIVE mount: a lease is held".to_string()).operator_needed
    );
}
