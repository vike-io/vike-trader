//! `RemoteBackend` without a network: the cases the wire is not needed for. The wire cases (the
//! lease before the adapter, a refused lease, the bound, the interlock) are
//! `tests/remote_backend_wire.rs`, over the client crate's own fake gatewayhub.

use vike_hub_proto::auth::HubKeys;
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub_core::AdapterPolicy;

use super::*;

fn accounts(rows: &[(&str, AccountTier)]) -> AccountDirectory {
    rows.iter()
        .fold(AdapterPolicy::default(), |p, (venue, tier)| {
            p.with_account(venue, &AccountLabel::Default, *tier)
        })
        .accounts
}

fn link_with_trade_key(addr: &str) -> GatewayLink {
    GatewayLink::new(
        addr,
        HubKeys::new(Vec::new(), Vec::new()).with_trade(b"a-secret-key".to_vec()),
    )
}

fn down(rows: &[(&str, AccountTier)], why: &str) -> RemoteBackend {
    RemoteBackend::from_parts(
        link_with_trade_key("127.0.0.1:1"),
        accounts(rows),
        "vike-tradehub-test".to_string(),
        Contact::Down(why.into()),
    )
}

/// **A box asks the gatewayhub only when an account needs a venue**: `demo` or `live` does,
/// `paper` does not, nor does a box with no account row at all.
#[test]
fn only_an_account_at_demo_or_live_needs_the_gatewayhub() {
    assert!(!needs_gateway(&accounts(&[])));
    assert!(!needs_gateway(&accounts(&[
        ("binance", AccountTier::Paper),
        ("bybit", AccountTier::Paper)
    ])));
    assert!(needs_gateway(&accounts(&[
        ("binance", AccountTier::Paper),
        ("bybit", AccountTier::Demo)
    ])));
    assert!(needs_gateway(&accounts(&[("aster", AccountTier::Live)])));
}

/// **A paper-only box never dials**: `connect` returns at once with no wait and no connection, on an
/// address nothing listens on, with a wait that would be a minute.
#[test]
fn a_paper_only_box_never_waits_or_dials() {
    let started = std::time::Instant::now();
    let backend = RemoteBackend::connect(
        link_with_trade_key("127.0.0.1:1"),
        accounts(&[("binance", AccountTier::Paper)]),
        "id".to_string(),
        Duration::from_secs(60),
    );
    assert!(started.elapsed() < Duration::from_secs(5), "it must not have waited");
    assert!(backend.gateway_down().is_none(), "nothing was asked, so nothing is down");
    let table = backend.arming_table().expect("it answers");
    let row = table.row_of_account("binance", &AccountLabel::Default).expect("a row");
    assert_eq!(row.arming.block, ArmingBlock::PaperTier);
    assert_eq!(row.arming.effective, Some(AccountTier::Paper));
}

/// **A box with no Trade key does not wait for one**: nothing will mint it in a minute, so the start
/// goes straight to `gatewayhub unavailable`, naming the key.
#[test]
fn a_missing_trade_key_is_unavailable_at_once_and_named() {
    let started = std::time::Instant::now();
    let backend = RemoteBackend::connect(
        GatewayLink::new("127.0.0.1:1", HubKeys::new(Vec::new(), Vec::new())),
        accounts(&[("bybit", AccountTier::Demo)]),
        "id".to_string(),
        Duration::from_secs(60),
    );
    assert!(started.elapsed() < Duration::from_secs(5), "no key is not a thing to wait for");
    let why = backend.gateway_down().expect("it is down");
    let key = vike_venues::credential_keys::gatewayhub_trade_key();
    assert!(why.contains(key), "the key is named: {why}");
}

/// **The gatewayhub is down: the demo/live account is NOT RUNNING with its own word, the paper account
/// runs, and no table row arms** — the table the fold assembles over.
#[test]
fn a_down_gatewayhub_stops_the_demo_account_and_leaves_the_paper_one() {
    let backend = down(&[("bybit", AccountTier::Demo), ("binance", AccountTier::Paper)], "refused");
    let table = backend.arming_table().expect("it answers");
    let bybit = &table.row_of_account("bybit", &AccountLabel::Default).expect("a row").arming;
    assert_eq!((bybit.effective, bybit.block), (None, ArmingBlock::GatewayUnavailable));
    let binance = &table.row_of_account("binance", &AccountLabel::Default).expect("a row").arming;
    assert_eq!(
        (binance.effective, binance.block),
        (Some(AccountTier::Paper), ArmingBlock::PaperTier)
    );
    assert_eq!(backend.gateway_down(), Some("refused"));
}

/// **A `data_only` venue is NOT RUNNING for want of exec credentials in the table**, over the same
/// connection, while the plan view (no declaration) still shows it armed.
#[test]
fn a_view_applies_its_own_data_only_declaration() {
    use vike_gatewayhub_client::proto::{ArmingRow, ArmingTable as WireTable};
    let wire = WireTable {
        facts: std::collections::BTreeMap::new(),
        rows: vec![ArmingRow {
            venue: "oanda".to_string(),
            label: None,
            tier: Some(AccountTier::Demo),
            effective: Some(AccountTier::Demo),
            block_word: ArmingBlock::None.word().to_string(),
            account_ids: vec![3],
            book: None,
        }],
    };
    let up = RemoteBackend::from_parts(
        link_with_trade_key("127.0.0.1:1"),
        accounts(&[("oanda", AccountTier::Demo)]),
        "id".to_string(),
        Contact::Up(Arc::new(wire::table_of_wire(&wire))),
    );
    let plan = up.view(accounts(&[("oanda", AccountTier::Demo)]), HashSet::new());
    let mount = up.view(
        accounts(&[("oanda", AccountTier::Demo)]),
        ["oanda".to_string()].into_iter().collect(),
    );
    let row = |b: &RemoteBackend| {
        b.arming_table()
            .unwrap()
            .row_of_account("oanda", &AccountLabel::Default)
            .unwrap()
            .arming
            .clone()
    };
    assert_eq!(row(&plan).block, ArmingBlock::None);
    assert_eq!(row(&mount).block, ArmingBlock::NoCredentials);
    assert_eq!(row(&mount).effective, None);
}

/// **A start against a gatewayhub that is down is `Unavailable`**, which the fold turns into
/// `gatewayhub unavailable` — and never a bind.
#[test]
fn a_start_against_a_down_gatewayhub_is_unavailable() {
    use vike_tradehub_core::{AdapterStartQuery, BackendError};
    let backend = down(&[("bybit", AccountTier::Demo)], "refused");
    let (events, _lane) = vike_exec::event_channel(8);
    let q = AdapterStartQuery {
        venue: "bybit",
        account: &AccountLabel::Default,
        tier: AccountTier::Demo,
        symbol: "BTCUSDT",
        declared_legs: &[],
        recon_enabled: false,
        risk_profile: None,
        market_slippage: None,
        halt_admit: vike_model::HaltAdmit::default(),
        withhold: None,
        route_key: "bybit",
        properties_rec: None,
    };
    match backend.start_adapter(q, &events, None) {
        Err(BackendError::Unavailable(why)) => assert!(why.contains("refused"), "{why}"),
        other => panic!("expected Unavailable, got {other:?}"),
    }
}

/// **The preflight of a down gatewayhub shows a WARN row for every armed venue and a green one for
/// none**: with no table to arm anything, the plan lists no credential venue and no clock read.
#[test]
fn the_preflight_plan_of_a_down_gatewayhub_is_empty_and_its_reads_are_gaps() {
    let backend = down(&[("bybit", AccountTier::Demo)], "refused");
    let plan = backend
        .preflight_plan(&PreflightQuery { clock_venues: &[] })
        .expect("a plan, never an Err that would drop the legs");
    assert!(plan.credential_venues.is_empty() && plan.wired_clock_reads == 0);
    assert!(matches!(
        backend.server_time_ms("bybit", AccountTier::Demo),
        Err(ServerTimeGap::Unreachable(_))
    ));
    assert!(matches!(backend.authed_read("bybit"), Err(CredentialGap::Unanswered { .. })));
    assert_eq!(backend.net_reading(), NetReading::Unknown);
}

/// No key value reaches `Debug`.
#[test]
fn debug_shows_the_address_and_never_the_key() {
    let backend = down(&[("bybit", AccountTier::Demo)], "refused");
    let shown = format!("{backend:?}");
    assert!(shown.contains("127.0.0.1:1"), "{shown}");
    assert!(!shown.contains("a-secret-key"), "{shown}");
}

/// **Which dial targets are "this host".** Loopback literals and `localhost` are; a LAN or documentation
/// address, a wildcard, an unparsable address and an empty one are not (review 15, finding 7).
#[test]
fn only_a_loopback_dial_target_is_this_host() {
    for local in ["127.0.0.1:7883", "127.8.9.1:7883", "[::1]:7883", "localhost:7883"] {
        assert!(dial_is_local(local), "{local} is this host");
    }
    for remote in ["192.0.2.5:7883", "<host>:7883", "0.0.0.0:7883", "no-port", "", ":7883"] {
        assert!(!dial_is_local(remote), "{remote} is not provably this host");
    }
}

/// **A gatewayhub dialled off-host gives no clock skew, and the leg is a WARN, never a green row.** The
/// backend never asks it (the address here is a documentation address nothing answers on), and the answer
/// is an unmeasured hazard that says why. The control: the same backend over a loopback address reaches the
/// fold's usual "no plan was asked" answer, so the gate is the address and nothing else.
#[test]
fn a_remote_gatewayhub_makes_the_clock_leg_unmeasured_not_green() {
    let remote = RemoteBackend::from_parts(
        link_with_trade_key("192.0.2.5:7883"),
        accounts(&[("binance", AccountTier::Demo)]),
        "vike-tradehub-test".to_string(),
        Contact::NotNeeded,
    );
    match remote.server_time_ms("binance", AccountTier::Demo) {
        Err(ServerTimeGap::UnmeasuredRisk { reason, at_stake }) => {
            assert!(reason.contains("remote gatewayhub"), "{reason}");
            assert!(at_stake.contains("signed"), "{at_stake}");
        }
        other => panic!("a remote gatewayhub's clock must be an unmeasured risk: {other:?}"),
    }
    let local = down(&[("binance", AccountTier::Demo)], "refused");
    assert!(
        matches!(
            local.server_time_ms("binance", AccountTier::Demo),
            Err(ServerTimeGap::Unreachable(_))
        ),
        "a loopback gatewayhub keeps the fold's own answers"
    );
}

/// **The project comparison, case by case.** The same token (trailing newline and all) is the same project;
/// a different token, an absent or empty file and "no project" against a
/// gatewayhub that has one are not; a gatewayhub stating no token leaves nothing to compare. No answer
/// prints a token.
#[test]
fn the_project_token_comparison_refuses_everything_but_the_same_project() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let file = dir.path().join(PROJECT_TOKEN_FILE);
    std::fs::write(&file, "token-a\n").expect("write");
    assert_eq!(project_refusal(Some(dir.path()), "token-a"), None);
    let differs = project_refusal(Some(dir.path()), "token-b").expect("a different token");
    assert!(differs.contains("different projects"), "{differs}");
    assert!(!differs.contains("token-a") && !differs.contains("token-b"), "no token is printed");
    std::fs::write(&file, "").expect("empty");
    assert!(project_refusal(Some(dir.path()), "token-a").is_some(), "an empty file proves nothing");
    std::fs::remove_file(&file).expect("remove");
    let absent = project_refusal(Some(dir.path()), "token-a").expect("absent file");
    assert!(absent.contains("cannot read"), "{absent}");
    assert!(project_refusal(None, "token-a").expect("no project").contains("no project state"));
    assert_eq!(
        project_refusal(None, ""),
        None,
        "a gatewayhub with no token leaves nothing to compare"
    );
    assert_eq!(project_refusal(Some(dir.path()), ""), None);
}

/// The two spellings of the token file's name are the same file: the gatewayhub makes it, this tradehub reads it.
#[test]
fn the_token_file_is_the_one_the_gatewayhub_makes() {
    assert_eq!(PROJECT_TOKEN_FILE, vike_gatewayhub::trade::state_dir::PROJECT_TOKEN_FILE);
}
