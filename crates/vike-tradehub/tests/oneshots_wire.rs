//! **The `venues` and `catalog` one-shots as gatewayhub clients**, driven through their real entry
//! points (`venues_cli::run`, `catalog_cli::run`) over a project whose hub-key store and
//! `config.gatewayhub_addr` row point at the gatewayhub client crate's own fake gatewayhub, so every
//! claim is read off what the peer was ASKED and what the command did to its exit code and its file.
//!
//! * `venues` asks ONE `ArmingQuery`, under `Scope::Trade`; a gatewayhub that does not answer is exit
//!   1 (never an empty table, which would read as "no account runs").
//! * `catalog list` asks the gatewayhub per credentialed venue (`CredentialPresence`, `Scope::Read`).
//! * `catalog refresh` asks FIRST whether the keys are there (exit 3 when they are not, with no
//!   fetch), then fetches with `FetchCredentialed` of `CatalogList` under `Scope::Write` and records
//!   what came back; a fetch that fails or a gatewayhub that cannot be asked is exit 1 and writes
//!   nothing.
//!
//! The stdout of both commands is the renderers' (`venues_cli_parity_tests.rs` holds the screen
//! byte-identical); these hold the exit codes, the frames and the document.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use vike_config::ArmingBlock;
use vike_gatewayhub_client::credential::CredentialPresence;
use vike_gatewayhub_client::proto::{
    ArmingRow, ArmingTable, FetchProgress, FetchRows, FetchState, FetchWhat, GridSource, Request,
    Response, VenueFacts,
};
use vike_hub_proto::auth::Scope;
use vike_model::{AccountTier, Contract, Instrument};
use vike_tradehub::{catalog_cli, venues_cli};

#[path = "../../vike-gatewayhub-client/tests/support/fake_peer.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod fake_peer;
#[path = "../../vike-gatewayhub-client/tests/support/fake_trade_peer.rs"]
#[allow(dead_code, clippy::allow_attributes)]
mod fake_trade_peer;

/// A project holding the three hub keys the fake peers verify (the gatewayhub's observe, control and
/// order-scope keys, under the names the key table gives them) and the address of the gatewayhub to
/// dial. Returns the project and the environment that points a command at it.
fn project(addr: &str) -> (tempfile::TempDir, HashMap<String, String>) {
    let dir = tempfile::tempdir().expect("a temp project");
    vike_secrets::create_store(dir.path().to_str()).expect("an empty settings database");
    let [observe, control] = vike_venues::credential_keys::gatewayhub_key_pair();
    let rows = [
        (observe.to_string(), "observe-key-of-the-tests".to_string()),
        (control.to_string(), "control-key-of-the-tests".to_string()),
        (
            vike_venues::credential_keys::gatewayhub_trade_key().to_string(),
            "trade-key-of-the-tests".to_string(),
        ),
    ];
    vike_secrets::save_credentials_to_store_unjournalled(
        dir.path(),
        vike_secrets::Table::HubKey,
        &rows,
        vike_secrets::UpsertIntent::Add,
    )
    .expect("plant the hub keys");
    vike_config::write_setting_row(
        dir.path(),
        "config.gatewayhub_addr",
        addr,
        Duration::from_millis(500),
    )
    .expect("plant the gatewayhub address");
    let env = HashMap::from([(
        vike_secrets::SETTINGS_DIR_ENV.to_string(),
        dir.path().display().to_string(),
    )]);
    (dir, env)
}

fn exit(code: u8) -> String {
    format!("{:?}", ExitCode::from(code))
}

fn args(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| (*w).to_string()).collect()
}

/// The recorded catalog document under `dir`, wherever the project's state directory put it.
fn recorded(dir: &Path) -> Option<PathBuf> {
    fn walk(dir: &Path, out: &mut Option<PathBuf>) {
        let Ok(read) = std::fs::read_dir(dir) else { return };
        for entry in read.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.file_name().is_some_and(|n| n == vike_catalog::LOCAL_FILE) {
                *out = Some(path);
            }
        }
    }
    let mut found = None;
    walk(dir, &mut found);
    found
}

// ---- venues -----------------------------------------------------------------------------------------

fn arming_table() -> ArmingTable {
    ArmingTable {
        facts: BTreeMap::from([(
            "binance".to_string(),
            VenueFacts {
                adapter_present: true,
                addresses_accounts: true,
                takes_recon_trigger: false,
                grid_source: GridSource::NoGrid,
                process_exclusive: None,
                clock_decl_word: "wired".to_string(),
            },
        )]),
        rows: vec![ArmingRow {
            venue: "binance".to_string(),
            label: None,
            tier: Some(AccountTier::Demo),
            effective: Some(AccountTier::Demo),
            block_word: ArmingBlock::None.word().to_string(),
            account_ids: vec![3],
            book: None,
        }],
        key_changed: Vec::new(),
    }
}

/// `venues` asks the gatewayhub ONE question under the Trade scope and prints what it gets.
#[test]
fn venues_asks_one_arming_query_under_the_trade_scope() {
    let peer = fake_trade_peer::TradePeer::start(fake_trade_peer::TradePeerConfig::new().with_rpc(
        |req| match req {
            Request::ArmingQuery => vec![Response::ArmingTable(Box::new(arming_table()))],
            other => vec![Response::Error(format!("unscripted: {other:?}"))],
        },
    ));
    let (dir, env) = project(&peer.addr_string());
    let got = venues_cli::run(&env, Some(dir.path()), &args(&["--venue", "binance"]));
    assert_eq!(format!("{got:?}"), format!("{:?}", ExitCode::SUCCESS));
    let asks = peer.rpcs().iter().filter(|r| matches!(r, Request::ArmingQuery)).count();
    assert_eq!(asks, 1, "one ArmingQuery");
    assert_eq!(peer.auths(), vec![(Scope::Trade, true)], "signed with the order-scope key");
}

/// **A gatewayhub that does not answer is an ERROR, never an empty table.**
#[test]
fn venues_without_a_gatewayhub_is_exit_1_and_not_an_empty_screen() {
    let (dir, env) = project(&fake_trade_peer::dead_addr());
    let got = venues_cli::run(&env, Some(dir.path()), &args(&[]));
    assert_eq!(format!("{got:?}"), exit(1));
}

// ---- catalog ----------------------------------------------------------------------------------------

fn instrument(i: usize) -> Instrument {
    Instrument::new("alpaca", format!("SYM{i}"), "AAA", "USD", Contract::CryptoSpot)
}

/// A healthy daemon's answers: `alpaca` has keys, `oanda` has none, `ctrader` is not a lane it serves;
/// a credentialed list of `listing` for alpaca.
fn catalog_peer(listing: Vec<Instrument>) -> fake_peer::FakePeer {
    fake_peer::FakePeer::start(fake_peer::PeerConfig::new().with_rpc(move |req| match req {
        Request::CredentialPresence { venue } => {
            let presence = match venue.as_str() {
                "alpaca" => CredentialPresence::Present,
                "oanda" => CredentialPresence::Absent,
                _ => CredentialPresence::NotNeeded,
            };
            vec![fake_peer::Step::Send(Response::PresenceIs { presence })]
        }
        Request::FetchCredentialed { what: FetchWhat::CatalogList { .. }, .. } => {
            let pages: Vec<FetchRows> =
                FetchRows::Instruments(listing.clone()).into_pages(64 * 1024).collect();
            let n = u32::try_from(pages.len()).expect("a few pages");
            let mut steps = vec![fake_peer::Step::Send(Response::FetchProgress(FetchProgress {
                rid: 1,
                state: FetchState::Running,
                pages: 0,
                rows: 0,
                elapsed_ms: 1,
                next_in_ms: 100,
            }))];
            for (i, rows) in pages.into_iter().enumerate() {
                steps.push(fake_peer::Step::Send(Response::FetchPage {
                    rid: 1,
                    page: u32::try_from(i).expect("a few pages"),
                    rows,
                }));
            }
            steps.push(fake_peer::Step::Send(Response::FetchDone {
                rid: 1,
                pages: n,
                rows: listing.len() as u64,
            }));
            steps
        }
        other => vec![fake_peer::Step::Send(Response::Error(format!("unscripted: {other:?}")))],
    }))
}

/// `catalog list` asks per credentialed venue, under the Read scope, and succeeds.
#[test]
fn catalog_list_asks_the_gatewayhub_per_venue_under_the_read_scope() {
    let peer = catalog_peer(Vec::new());
    let (dir, env) = project(&peer.addr_string());
    let got = catalog_cli::run(&env, Some(dir.path()), &args(&["list"]));
    assert_eq!(format!("{got:?}"), format!("{:?}", ExitCode::SUCCESS));
    let mut asked: Vec<String> = peer
        .requests()
        .into_iter()
        .filter_map(|r| match r {
            Request::CredentialPresence { venue } => Some(venue),
            _ => None,
        })
        .collect();
    asked.sort();
    assert_eq!(
        asked,
        ["alpaca", "ctrader", "oanda"],
        "one presence question per credentialed venue"
    );
    assert!(
        peer.auths().iter().all(|(scope, ok)| *scope == Scope::Read && *ok),
        "{:?}",
        peer.auths()
    );
}

/// **No keys at the gatewayhub is exit 3 with NO fetch** — never an empty list recorded.
#[test]
fn catalog_refresh_with_no_keys_is_exit_3_and_fetches_nothing() {
    let peer = catalog_peer(vec![instrument(1)]);
    let (dir, env) = project(&peer.addr_string());
    let got = catalog_cli::run(&env, Some(dir.path()), &args(&["refresh", "oanda"]));
    assert_eq!(format!("{got:?}"), exit(3));
    assert!(
        !peer.requests().iter().any(|r| matches!(r, Request::FetchCredentialed { .. })),
        "a venue with no keys is not fetched"
    );
    assert!(recorded(dir.path()).is_none(), "and nothing is recorded");
}

/// **A refresh fetches with the operator's identity under the Write scope and records what came
/// back**, in the document the symbol picker reads.
#[test]
fn catalog_refresh_fetches_credentialed_under_the_write_scope_and_records_the_list() {
    let listing: Vec<Instrument> = (0..40).map(instrument).collect();
    let peer = catalog_peer(listing.clone());
    let (dir, env) = project(&peer.addr_string());
    let got = catalog_cli::run(&env, Some(dir.path()), &args(&["refresh", "alpaca"]));
    assert_eq!(format!("{got:?}"), format!("{:?}", ExitCode::SUCCESS));

    let fetches: Vec<_> = peer
        .requests()
        .into_iter()
        .filter(|r| matches!(r, Request::FetchCredentialed { .. }))
        .collect();
    assert_eq!(fetches.len(), 1, "one credentialed fetch");
    assert!(
        matches!(&fetches[0], Request::FetchCredentialed { what: FetchWhat::CatalogList { venue }, .. } if venue == "alpaca")
    );
    assert!(peer.auths().contains(&(Scope::Write, true)), "{:?}", peer.auths());

    let path = recorded(dir.path()).expect("the document was written");
    let doc = vike_catalog::BaselineCatalog::parse_as(
        &std::fs::read(&path).expect("readable"),
        vike_catalog::Provenance::Operator,
    )
    .expect("what was written is what the loader reads");
    let row = doc.venue("alpaca").expect("alpaca is recorded");
    assert_eq!(row.instruments.len(), listing.len());
    assert_eq!(row.qualifier, "demo");
}

/// **A gatewayhub that cannot be asked is exit 1, nothing fetched and nothing recorded** — not exit
/// 3, which would send the operator to store keys they may have stored.
#[test]
fn catalog_refresh_without_a_gatewayhub_is_exit_1_and_records_nothing() {
    let (dir, env) = project(&fake_peer::dead_addr());
    let got = catalog_cli::run(&env, Some(dir.path()), &args(&["refresh", "alpaca"]));
    assert_eq!(format!("{got:?}"), exit(1));
    assert!(recorded(dir.path()).is_none());
}

/// A presence the gatewayhub cannot give (`NotNeeded`: it serves no credential lane for the venue) is
/// exit 1 as well, and the fetch is not attempted.
#[test]
fn catalog_refresh_for_a_venue_the_gatewayhub_serves_no_lane_for_is_exit_1() {
    let peer = catalog_peer(vec![instrument(1)]);
    let (dir, env) = project(&peer.addr_string());
    let got = catalog_cli::run(&env, Some(dir.path()), &args(&["refresh", "ctrader"]));
    assert_eq!(format!("{got:?}"), exit(1));
    assert!(!peer.requests().iter().any(|r| matches!(r, Request::FetchCredentialed { .. })));
}
