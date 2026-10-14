//! The command line, the boot's bind guard, the run order and the wire hook, with NO venue client
//! anywhere.
//!
//! The in-process tests drive [`run_with`] over a throwaway project directory named by
//! `VIKE_SETTINGS_DIR` in the `vars` map they hand it (the daemon reads no process environment), with
//! a recording closure as the wire, or with the REAL server (`wire::serve_ready`) over a minimal local
//! double of the two lanes, which is the seam the real `BridgeData`/`VenueRest` are put through. The
//! shipped binary's own behaviour (status codes and streams) is
//! `crates/vike-gatewayhub/tests/help_cli.rs`'s.

use std::cell::Cell;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use vike_config::config::DEFAULT_GATEWAYHUB_ADDR;
use vike_data::{DataClient, LiveDataSink};
use vike_gatewayhub_client::client::{GatewayClient, GatewayLink};
use vike_gatewayhub_client::credential::CredentialPresence;
use vike_gatewayhub_client::proto::{
    FetchFailure, FetchRows, FetchWhat, OptionsCall, OptionsFeedSpec, OptionsReply,
};
use vike_hub_proto::auth::{HubKeys, Scope};
use vike_secrets::venue_setting::SettingTier;
use vike_secrets::{SettingRow, StoredSettings, VenueSettingRow};
use vike_venues::Market;

use super::args::read_args;
use super::boot::{BindRefusal, resolve_bind, venue_settings_of};
use super::keys::{KEYS_NOT_PROVISIONED, KEYS_STORE_UNREADABLE, gateway_keys, pair_names};
use super::wire::{bind_ready, budget_counters, serve_ready};
use super::*;
use crate::budget::ceiling::Axis;
use crate::budget::class::Class;
use crate::data::{DataLane, Holder};
use crate::rest::{
    CatalogTraits, FetchCtx, OptionsFeedHandle, OptionsFeedSink, RestFacts, RestLane,
};
use crate::server::lanes::Lanes;
use crate::session::ConnectionScope;

/// `vars` naming `dir` as the project's settings directory, and nothing else.
fn vars_for(dir: &Path) -> HashMap<String, String> {
    HashMap::from([(
        "VIKE_SETTINGS_DIR".to_string(),
        dir.to_str().expect("a utf-8 temp path").to_string(),
    )])
}

/// A settings database holding `rows` (`section`, `key`, JSON scalar).
///
/// ⚠ No `venue_setting` rows are planted: that table has its own writer, which
/// `crates/vike-ops/tests/settings_secrets/credential_writer_gate.rs` pins to a short list of callers,
/// and a test of this daemon is not one of them. The venue-settings hand-over is tested on the
/// values themselves (`the_venue_settings_read_is_handed_on_whole`).
fn planted(rows: &[(&str, &str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().expect("a temp settings dir");
    let settings = rows
        .iter()
        .map(|(section, key, value)| SettingRow {
            section: (*section).to_string(),
            key: (*key).to_string(),
            value: (*value).to_string(),
        })
        .collect();
    vike_secrets::plant_settings_rows(
        dir.path(),
        &StoredSettings { settings, ..Default::default() },
    )
    .expect("plant the rows");
    dir
}

/// An exit status as text: `ExitCode` is not comparable, and its `Debug` carries the number.
fn code(c: ExitCode) -> String {
    format!("{c:?}")
}

// -- the command line ---------------------------------------------------------------------------

fn argv(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| (*a).to_string()).collect()
}

/// `--help` and `-h`: exit 0, usage on STDOUT, and the settings page names the row and its default —
/// the default read off the constant, so the help cannot drift from the thing it describes.
#[test]
fn help_is_a_success_on_stdout_and_names_the_row_and_its_default() {
    for flag in ["--help", "-h"] {
        let exit = read_args(argv(&[flag])).expect_err("help ends the parse");
        assert_eq!((exit.code, exit.to_stdout), (0, true), "{flag}");
        assert!(exit.text.contains("Usage:"), "{flag}: {}", exit.text);
        assert!(exit.text.contains("config.gatewayhub_addr"), "{flag}: {}", exit.text);
        assert!(
            exit.text.contains(DEFAULT_GATEWAYHUB_ADDR),
            "{flag}: the help states the compiled default {DEFAULT_GATEWAYHUB_ADDR}"
        );
        assert!(exit.text.contains("NON-LOOPBACK"), "{flag}: the bind rule is on the page");
    }
}

/// `--version`/`-V`: the build identity line on stdout, exit 0.
#[test]
fn version_prints_the_build_identity_on_stdout() {
    for flag in ["--version", "-V"] {
        let exit = read_args(argv(&[flag])).expect_err("version ends the parse");
        assert_eq!((exit.code, exit.to_stdout), (0, true), "{flag}");
        assert!(
            exit.text.contains(env!("CARGO_PKG_NAME"))
                && exit.text.contains(env!("CARGO_PKG_VERSION")),
            "{flag}: {}",
            exit.text
        );
    }
}

/// The daemon takes NO flags, and what it is given beyond that is refused rather than ignored: a
/// typo in a unit's `ExecStart=` used to start the server as if nothing were wrong.
#[test]
fn an_unknown_flag_or_a_stray_positional_is_refused() {
    assert!(read_args(Vec::new()).is_ok(), "a plain start parses");
    let cases: [&[&str]; 3] =
        [&["--adr=127.0.0.1:7883"], &["--addr", "127.0.0.1:7883"], &["serve"]];
    for bad in cases {
        let exit = read_args(argv(bad)).expect_err("refused");
        assert_eq!((exit.code, exit.to_stdout), (2, false), "{bad:?}");
        assert!(exit.text.contains("vike-gatewayhub:"), "{bad:?}: {}", exit.text);
        assert!(exit.text.contains("Usage:"), "{bad:?}: {}", exit.text);
    }
}

// -- the bind guard -----------------------------------------------------------------------------

/// Loopback addresses are accepted, in both families.
#[test]
fn a_loopback_address_is_accepted() {
    for ok in ["127.0.0.1:7883", "127.0.0.1:0", "[::1]:7883", "127.1.2.3:7883"] {
        let targets = resolve_bind(ok).unwrap_or_else(|e| panic!("{ok}: {e:?}"));
        assert!(!targets.is_empty() && targets.iter().all(|a| a.ip().is_loopback()), "{ok}");
    }
    assert!(resolve_bind(DEFAULT_GATEWAYHUB_ADDR).is_ok(), "the compiled default is loopback");
}

/// Anything reachable off-box is refused naming the offending address: a wildcard bind, a LAN
/// address, a public one. There is no argument that could lift it.
#[test]
fn a_non_loopback_address_is_refused_outright() {
    for bad in ["0.0.0.0:7883", "[::]:7883", "<host>:7883", "<host>:7883", "8.8.8.8:7883"] {
        match resolve_bind(bad) {
            Err(BindRefusal::NotLoopback(exposed)) => {
                assert_eq!(exposed.port(), 7883, "{bad}");
                assert!(!exposed.ip().is_loopback(), "{bad}");
            }
            other => panic!("{bad}: expected a non-loopback refusal, got {other:?}"),
        }
    }
}

/// An address that resolves to nothing is its OWN refusal — a typo is not "exposed" — and is still
/// a refusal: `bind` could not have succeeded either way.
#[test]
fn an_address_that_resolves_to_nothing_is_refused_apart() {
    for bad in ["127.0.0.1:notaport", "127.0.0.1:99999", "127.0.0.1", ""] {
        assert_eq!(resolve_bind(bad), Err(BindRefusal::Unresolvable), "{bad:?}");
    }
}

// -- the run order, in process ------------------------------------------------------------------

/// With no row at all the wire is handed the compiled default on loopback, every earlier phase
/// done: the stop signal armed but not raised, an empty venue-settings read, an empty shadow ledger.
#[test]
fn the_wire_is_handed_everything_the_earlier_phases_made() {
    let dir = planted(&[]);
    let called = Cell::new(0);
    let exit = run_with(&vars_for(dir.path()), None, &[], |ready| {
        called.set(called.get() + 1);
        assert_eq!(ready.bind.len(), 1);
        assert_eq!(ready.bind[0].to_string(), DEFAULT_GATEWAYHUB_ADDR);
        assert!(!ready.stop.is_requested(), "armed, not raised");
        assert!(ready.venue_settings.is_empty());
        let snap = ready.budget.snapshot(std::time::Instant::now());
        assert!(snap.lines.is_empty() && snap.unmetered == 0, "an empty ledger: {snap:?}");
        assert!(
            ready
                .booted
                .settings_dir
                .as_deref()
                .is_some_and(|d| d.ends_with(dir.path().file_name().expect("a name"))),
            "the boot walked to the project the vars named"
        );
        ExitCode::from(7)
    });
    assert_eq!(called.get(), 1, "the wire runs exactly once");
    assert_eq!(code(exit), code(ExitCode::from(7)), "and its status is the process's");
}

/// The row decides the bind: `config.gatewayhub_addr` beats the compiled default.
#[test]
fn the_row_decides_the_bind() {
    let dir = planted(&[("config", "gatewayhub_addr", "\"127.0.0.1:7999\"")]);
    let seen = Cell::new(String::new());
    run_with(&vars_for(dir.path()), None, &[], |ready| {
        seen.set(ready.bind[0].to_string());
        ExitCode::SUCCESS
    });
    assert_eq!(seen.into_inner(), "127.0.0.1:7999");
}

/// A non-loopback row exits 2 and the wire is NEVER called: nothing is bound, nothing dialled.
#[test]
fn a_non_loopback_row_exits_two_before_the_wire() {
    for bad in ["0.0.0.0:7883", "<host>:7883"] {
        let dir = planted(&[("config", "gatewayhub_addr", &format!("\"{bad}\""))]);
        let called = Cell::new(false);
        let exit = run_with(&vars_for(dir.path()), None, &[], |_| {
            called.set(true);
            ExitCode::SUCCESS
        });
        assert_eq!(code(exit), code(ExitCode::from(2)), "{bad}");
        assert!(!called.get(), "{bad}: the wire must not run behind a refused bind");
    }
}

/// The ONE read of the `venue_setting` table is handed on whole to the venue clients, and an
/// unreadable store degrades to every venue's built-in defaults rather than failing the boot. A
/// build that links the bridge also hands the same read to the egress declaration, in every shape.
#[test]
fn the_venue_settings_read_is_handed_on_whole() {
    let rows = [VenueSettingRow {
        venue: "polymarket".to_string(),
        tier: None,
        field: "PROXY_HOST".to_string(),
        value: "127.0.0.1".to_string(),
    }];
    let all =
        BTreeMap::from([("polymarket".to_string(), VenueSettings::from_rows("polymarket", &rows))]);
    let readable = Ok(all.clone());
    let kept = venue_settings_of(Some(&readable));
    assert_eq!(kept, all, "a readable table is handed on whole");
    assert_eq!(
        kept.get("polymarket").and_then(|v| v.get(SettingTier::Any, "PROXY_HOST")),
        Some("127.0.0.1")
    );
    assert!(venue_settings_of(None).is_empty(), "no settings directory: no rows");

    let unreadable = Err(vike_secrets::DbError {
        path: std::path::PathBuf::from("vike.db"),
        kind: vike_secrets::DbErrorKind::VanishedDatabase,
    });
    assert!(venue_settings_of(Some(&unreadable)).is_empty(), "an unreadable table reads as empty");

    // The declaration takes the same read in every shape (a build that links the bridge only). The
    // bridge keeps ONE process-wide egress, so these three shapes all declare the built-in default:
    // a declaration that differed from an earlier one would only be logged, but a lib test binary
    // is one process and must not leave a custom proxy behind for the next test.
    #[cfg(feature = "data")]
    {
        let none_stored: Result<BTreeMap<String, VenueSettings>, vike_secrets::DbError> =
            Ok(BTreeMap::new());
        declare_polymarket_egress(Some(none_stored));
        declare_polymarket_egress(Some(unreadable));
        declare_polymarket_egress(None);
    }
    drop(readable);
}

/// The real wire refuses to look like a running gateway: over a clean project it exits 2 and the log
/// says WHY, in the build's own words. With the venue bridges (`data`) the reason is the hub keys,
/// which a clean project does not hold (always keyed: no keys, no start, and no socket was opened);
/// without them the reason is that the build has no venue lane at all. Either way a unit that waits
/// for this process sees it fail instead of a gateway that answers nothing.
#[test]
fn the_default_wire_refuses_to_look_like_a_running_gateway() {
    let dir = planted(&[]);
    let (exit, events) = vike_log::capture::captured(|| run(&vars_for(dir.path()), None, &[]));
    assert_eq!(code(exit), code(ExitCode::from(2)));
    let errors: Vec<&str> = events
        .iter()
        .filter(|e| e.level == tracing::Level::ERROR)
        .map(|e| e.message.as_str())
        .collect();
    let why = if cfg!(feature = "data") {
        KEYS_NOT_PROVISIONED
    } else {
        "built without the `data` feature: no venue lane"
    };
    assert!(errors.iter().any(|m| m.contains(why)), "the refusal says why ({why}): {errors:?}");
    assert!(
        !events.iter().any(|e| e.target.contains("server::accept")),
        "the server must not have been entered behind a refusal: {events:?}"
    );
}

// -- the hub keys ------------------------------------------------------------------------------

/// The handle the boot would hand this root for a settings directory `dir` (the full scope).
fn store_at(dir: &Path) -> vike_secrets::CredentialStore {
    vike_secrets::CredentialStore::open(
        Some(vike_secrets::StoreLocation::new(dir)),
        vike_secrets::CredentialScope::Full,
    )
}

/// `(name, value)` rows written into the `hub_key` table of the store [`planted`] made, with the
/// writer's record-less `test-support` body. Every value is a fake literal of the test.
fn plant_hub_keys(dir: &Path, rows: &[(&str, &str)]) {
    let updates: Vec<(String, String)> =
        rows.iter().map(|(name, value)| ((*name).to_string(), (*value).to_string())).collect();
    vike_secrets::save_credentials_to_store_unjournalled(
        dir,
        vike_secrets::Table::HubKey,
        &updates,
        vike_secrets::UpsertIntent::Add,
    )
    .expect("plant the hub keys");
}

/// The value of every key another service owns (the tradehub's pair and admin key, the datahub's
/// pair).
const OTHER_SERVICES_VALUE: &str = "a-key-of-another-service";
/// The gatewayhub's observe key, as these tests plant it.
const GW_OBSERVE_VALUE: &str = "gatewayhub-observe-key-of-this-test";
/// The gatewayhub's control key, as these tests plant it.
const GW_CONTROL_VALUE: &str = "gatewayhub-control-key-of-this-test";

/// The five names that are NOT this daemon's: `PLATFORM_KEYS` before the gatewayhub's pair.
fn other_services_keys() -> Vec<(&'static str, &'static str)> {
    vike_venues::credential_keys::PLATFORM_KEYS[..5]
        .iter()
        .map(|name| (*name, OTHER_SERVICES_VALUE))
        .collect()
}

/// **No key of the daemon's own in the store is a refusal, whatever else the store holds** - a box
/// that has not run `vike-cli gatewayhub setup`: no settings directory at all, a directory without a
/// database, a database without hub keys, and a database that holds EVERY OTHER service's keys (the
/// daemon must refuse rather than borrow the datahub's pair or the tradehub's). The words name both
/// key names and the remedy, and carry no value.
#[test]
fn gateway_keys_refuse_when_the_store_holds_no_gatewayhub_key() {
    let [observe, control] = pair_names();
    let no_database = tempfile::TempDir::new().expect("a temp dir");
    let empty = planted(&[]);
    let others = planted(&[]);
    plant_hub_keys(others.path(), &other_services_keys());

    let cases = [
        (
            "no location",
            vike_secrets::CredentialStore::open(None, vike_secrets::CredentialScope::Full),
        ),
        ("a directory without a database", store_at(no_database.path())),
        ("a database without hub keys", store_at(empty.path())),
        ("a database with only other services' keys", store_at(others.path())),
    ];
    for (what, store) in &cases {
        let refused = gateway_keys(store).expect_err(what);
        assert!(refused.starts_with(KEYS_NOT_PROVISIONED), "{what}: {refused}");
        assert!(refused.contains(observe) && refused.contains(control), "{what}: {refused}");
        assert!(refused.contains("vike-cli gatewayhub setup"), "{what}: {refused}");
        assert!(!refused.contains(OTHER_SERVICES_VALUE), "{what}: a value in the refusal");
    }
}

/// **The pair the store holds is the pair the daemon verifies with**: the observe key signs the
/// `Read` scope, the control key the `Write` scope, and the `Account` scope stays without a key (that
/// refusal is structural, as the datahub's is). The store also holds every other service's keys,
/// each under its own value, and none of them is the daemon's.
#[test]
fn gateway_keys_hold_the_pair_the_store_holds() {
    let [observe, control] = pair_names();
    let dir = planted(&[]);
    let mut rows = other_services_keys();
    rows.push((observe, GW_OBSERVE_VALUE));
    rows.push((control, GW_CONTROL_VALUE));
    plant_hub_keys(dir.path(), &rows);

    let keys = gateway_keys(&store_at(dir.path())).expect("the pair is provisioned");
    assert!(keys.has(Scope::Read) && keys.has(Scope::Write), "{keys:?}");
    assert!(!keys.has(Scope::Account), "this daemon grants no Account scope: {keys:?}");
    assert_eq!(keys.key_for(Scope::Read), GW_OBSERVE_VALUE.as_bytes());
    assert_eq!(keys.key_for(Scope::Write), GW_CONTROL_VALUE.as_bytes());
    assert_ne!(keys.key_for(Scope::Read), OTHER_SERVICES_VALUE.as_bytes());
}

/// One key is enough to start: the scope without a key is then refused at the handshake (the ready
/// line shows which key is `absent`). NEITHER is the refusal above.
#[test]
fn a_single_key_is_enough_to_start() {
    let [observe, control] = pair_names();
    let only_observe = planted(&[]);
    plant_hub_keys(only_observe.path(), &[(observe, GW_OBSERVE_VALUE)]);
    let keys = gateway_keys(&store_at(only_observe.path())).expect("one key is a pair of one");
    assert!(keys.has(Scope::Read) && !keys.has(Scope::Write), "{keys:?}");

    let only_control = planted(&[]);
    plant_hub_keys(only_control.path(), &[(control, GW_CONTROL_VALUE)]);
    let keys = gateway_keys(&store_at(only_control.path())).expect("one key is a pair of one");
    assert!(!keys.has(Scope::Read) && keys.has(Scope::Write), "{keys:?}");
}

/// **A store that EXISTS and will not read is an ERROR, never "no keys configured".** Bytes that are
/// not a database where the database belongs (`chmod 000` proves nothing as root, which CI is): the
/// refusal is the UNREADABLE one, not the not-provisioned one (the remedies differ), the cause is
/// logged once at `error`, and no value reaches the log.
#[test]
fn an_unreadable_hub_key_store_is_an_error_and_not_an_absent_one() {
    let dir = tempfile::TempDir::new().expect("a temp dir");
    std::fs::create_dir_all(dir.path().join("db")).expect("the db directory");
    std::fs::write(vike_secrets::db_path_in(dir.path()), b"not a sqlite database at all")
        .expect("plant the garbage");

    let (refused, events) = vike_log::capture::captured(|| gateway_keys(&store_at(dir.path())));
    let refused = refused.expect_err("an unreadable store gives no keys");
    assert_eq!(refused, KEYS_STORE_UNREADABLE);
    assert!(!refused.starts_with(KEYS_NOT_PROVISIONED), "it must not wear the unconfigured answer");
    let errors: Vec<&str> = events
        .iter()
        .filter(|e| e.level == tracing::Level::ERROR)
        .map(|e| e.message.as_str())
        .collect();
    assert_eq!(errors.len(), 1, "the unreadable store is logged, once: {events:?}");
    assert!(errors[0].contains("NOT the same as no keys being provisioned"), "{}", errors[0]);
}

/// ⚠ The pair is read out of `PLATFORM_KEYS` by INDEX, so this is the assertion that the indexing is
/// right: both names are the gatewayhub's, observe first and control second, and the daemon's family
/// predicate admits them (a reorder of the table makes the family read return no key and the daemon
/// refuse to start, rather than serve under another service's name).
#[test]
fn the_pair_names_are_the_gatewayhubs_and_classified_as_such() {
    let [observe, control] = pair_names();
    assert!(observe.contains("GATEWAYHUB") && observe.contains("OBSERVE"), "{observe}");
    assert!(control.contains("GATEWAYHUB") && control.contains("CONTROL"), "{control}");
    for name in [observe, control] {
        assert_eq!(
            vike_venues::credential_keys::platform_key_service(name),
            Some(vike_venues::credential_keys::GATEWAYHUB_SERVICE)
        );
        assert!(vike_venues::credential_keys::is_gatewayhub_daemon_key(name), "{name}");
    }
}

/// The wire hook never serves under empty keys, even if something above it let them through: the
/// server itself refuses, the process exits non-zero, and nothing is left listening.
#[test]
fn the_server_never_runs_with_empty_keys() {
    let dir = planted(&[("config", "gatewayhub_addr", "\"127.0.0.1:0\"")]);
    let exit = run_with(&vars_for(dir.path()), None, &[], |ready| {
        serve_ready(ready, HubKeys::new(Vec::new(), Vec::new()), stub_lanes())
    });
    assert_eq!(code(exit), code(ExitCode::FAILURE), "empty keys end the server at once");
}

// -- the wire: the real server over a minimal double of the lanes -------------------------------

/// The observe/control keys the loopback test signs with.
fn loopback_keys() -> HubKeys {
    HubKeys::new(b"observe-key-of-this-test".to_vec(), b"control-key-of-this-test".to_vec())
}

/// The smallest `DataLane` the server accepts: it advertises one venue and builds no client. The
/// stand-in for `BridgeData` at the seam [`run_with`] offers, so the test does not depend on the
/// bridges (a default build links none) or on the crate's test kit.
struct StubData;

impl DataLane for StubData {
    fn venues(&self) -> Vec<String> {
        vec!["binance".to_string()]
    }

    fn open(
        &self,
        _venue: &str,
        _holder: &Holder,
        _sink: Arc<dyn LiveDataSink>,
    ) -> Result<Box<dyn DataClient + Send>, String> {
        Err("the stub lane builds no venue client".to_string())
    }
}

/// The smallest `RestLane`: it serves no fetch lane and answers every call with a refusal.
struct StubRest;

impl RestLane for StubRest {
    fn facts(&self) -> RestFacts {
        RestFacts::default()
    }

    fn fetch(
        &self,
        _scope: &ConnectionScope,
        _what: &FetchWhat,
        _credentialed: bool,
        _ctx: &FetchCtx,
    ) -> Result<FetchRows, FetchFailure> {
        Err(FetchFailure::Fetch("the stub lane fetches nothing".to_string()))
    }

    fn catalog_describe(
        &self,
        _scope: &ConnectionScope,
        _venue: &str,
    ) -> Result<CatalogTraits, String> {
        Err("the stub lane describes nothing".to_string())
    }

    fn options(
        &self,
        _scope: &ConnectionScope,
        _call: &OptionsCall,
    ) -> Result<OptionsReply, String> {
        Err("the stub lane serves no options".to_string())
    }

    fn open_options_feed(
        &self,
        _scope: &ConnectionScope,
        _spec: &OptionsFeedSpec,
        _sink: Arc<dyn OptionsFeedSink>,
    ) -> Result<Box<dyn OptionsFeedHandle>, String> {
        Err("the stub lane opens no feed".to_string())
    }

    fn resolve_family(
        &self,
        _scope: &ConnectionScope,
        _venue: &str,
        _family: &str,
        _now_ms: i64,
    ) -> Result<Vec<String>, String> {
        Err("the stub lane resolves no family".to_string())
    }

    fn credential_presence(&self, _venue: &str) -> CredentialPresence {
        CredentialPresence::NotChecked
    }
}

/// The pair, as the server takes it.
fn stub_lanes() -> Lanes {
    Lanes { data: Arc::new(StubData), rest: Arc::new(StubRest) }
}

/// A loopback port nobody holds right now: bound to 0, read, released.
fn free_loopback_port() -> u16 {
    let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
    probe.local_addr().expect("the bound address").port()
}

/// How long the test waits for anything that should be quick: the daemon coming up, the stop.
/// Enormous next to the work (the server's own drain budget is five seconds) — it is the line
/// between "did it" and "hung", not a measurement.
const BOUND: Duration = Duration::from_secs(20);

/// Dial `link` as a reader until the daemon answers, or `BOUND` passes. The server binds on its own
/// thread, so the first dials can find the port closed.
fn connect_within(link: &GatewayLink, bound: Duration) -> GatewayClient {
    let deadline = Instant::now() + bound;
    loop {
        match GatewayClient::connect(link, Scope::Read) {
            Ok(client) => return client,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => panic!("the daemon never answered a handshake within {bound:?}: {e}"),
        }
    }
}

/// **`run_with` SERVES, answers a client handshake and STOPS cleanly on the stop flag.** The whole
/// chain a unit depends on, in process: the settings row picks the address, the boot and the guards
/// run, the real server binds exactly `Ready::bind` and the real client authenticates and reads the
/// status; raising the flag the boot armed (the one SIGTERM raises) ends the accept loop, drains,
/// and the process's exit status is 0 — within a bound. The lanes are the minimal local double of
/// `BridgeData`/`VenueRest`.
#[test]
fn run_with_serves_answers_a_handshake_and_stops_on_the_flag() {
    let port = free_loopback_port();
    let addr = format!("127.0.0.1:{port}");
    let dir = planted(&[("config", "gatewayhub_addr", &format!("\"{addr}\""))]);
    let vars = vars_for(dir.path());

    let (flag_tx, flag_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let daemon = std::thread::Builder::new()
        .name("gwh-test-run".into())
        .spawn(move || {
            let exit = run_with(&vars, None, &[], move |ready| {
                flag_tx.send(ready.stop.flag()).expect("the test is waiting for the flag");
                serve_ready(ready, loopback_keys(), stub_lanes())
            });
            done_tx.send(code(exit)).expect("the test is waiting for the exit status");
        })
        .expect("spawn the daemon thread");

    let stop = flag_rx.recv_timeout(BOUND).expect("the boot reached the wire hook");
    let link = GatewayLink::new(addr, loopback_keys());
    let mut client = connect_within(&link, BOUND);
    let status = client.status().expect("an authenticated reader reads the status");
    assert_eq!(status.md_venues, ["binance"], "the Welcome and the status speak the lane's roster");
    assert!(status.connections >= 1, "this very connection: {status:?}");
    assert!(status.budget.is_empty(), "no lane has spent against the shadow budget: {status:?}");
    drop(client);

    // A reader with no key for the scope is refused before it dials, and a WRONG key is refused by
    // the daemon: the handshake is not decoration.
    let wrong = GatewayLink::new(
        link.addr().to_string(),
        HubKeys::new(b"not-the-observe-key".to_vec(), Vec::new()),
    );
    assert!(GatewayClient::connect(&wrong, Scope::Read).is_err(), "a wrong key must not connect");

    let stopped_at = Instant::now();
    vike_boot::stop::request_stop(&stop);
    let exit = done_rx.recv_timeout(BOUND).expect("the daemon stopped within the bound");
    assert_eq!(exit, code(ExitCode::SUCCESS), "a clean stop is exit 0");
    assert!(stopped_at.elapsed() < BOUND, "stopped in {:?}", stopped_at.elapsed());
    daemon.join().expect("the daemon thread ended without a panic");
}

/// **The pair the store holds is the pair the daemon SERVES under**: the wire hook reads the node
/// keys through [`gateway_keys`] (the one read), and a client holding exactly those two values
/// authenticates at both scopes it was given a key for, while a client holding the datahub's own
/// values, or the gatewayhub's pair swapped, is refused. The other half of the datahub's
/// `client_keys` test: the same two names, read from the same table by the two ends.
#[test]
fn the_daemon_serves_under_the_keys_the_store_holds() {
    let [observe, control] = pair_names();
    let port = free_loopback_port();
    let addr = format!("127.0.0.1:{port}");
    let dir = planted(&[("config", "gatewayhub_addr", &format!("\"{addr}\""))]);
    let mut rows = other_services_keys();
    rows.push((observe, GW_OBSERVE_VALUE));
    rows.push((control, GW_CONTROL_VALUE));
    plant_hub_keys(dir.path(), &rows);
    let vars = vars_for(dir.path());

    let (flag_tx, flag_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let daemon = std::thread::Builder::new()
        .name("gwh-test-keyed".into())
        .spawn(move || {
            let exit = run_with(&vars, None, &[], move |ready| {
                flag_tx.send(ready.stop.flag()).expect("the test is waiting for the flag");
                let keys = gateway_keys(&ready.booted.store).expect("the store holds the pair");
                serve_ready(ready, keys, stub_lanes())
            });
            done_tx.send(code(exit)).expect("the test is waiting for the exit status");
        })
        .expect("spawn the daemon thread");

    let stop = flag_rx.recv_timeout(BOUND).expect("the boot reached the wire hook");
    let pair = |observe: &str, control: &str| {
        GatewayLink::new(
            addr.clone(),
            HubKeys::new(observe.as_bytes().to_vec(), control.as_bytes().to_vec()),
        )
    };
    let link = pair(GW_OBSERVE_VALUE, GW_CONTROL_VALUE);
    drop(connect_within(&link, BOUND));
    assert!(GatewayClient::connect(&link, Scope::Write).is_ok(), "the control key signs Write");
    let swapped = pair(GW_CONTROL_VALUE, GW_OBSERVE_VALUE);
    let datahubs_own = pair(OTHER_SERVICES_VALUE, OTHER_SERVICES_VALUE);
    for (what, wrong) in [("swapped", &swapped), ("another service's value", &datahubs_own)] {
        for scope in [Scope::Read, Scope::Write] {
            assert!(
                GatewayClient::connect(wrong, scope).is_err(),
                "{what} must not connect at {scope:?}"
            );
        }
    }

    vike_boot::stop::request_stop(&stop);
    let exit = done_rx.recv_timeout(BOUND).expect("the daemon stopped within the bound");
    assert_eq!(exit, code(ExitCode::SUCCESS), "a clean stop is exit 0");
    daemon.join().expect("the daemon thread ended without a panic");
}

/// The wire binds exactly the addresses the bind guard resolved and checked — it re-checks that SAME
/// set and never resolves the row's string again. Behaviour here; the text pin below holds the shape.
#[test]
fn the_wire_binds_the_checked_addresses_and_refuses_an_exposed_one() {
    let listener = bind_ready(&["127.0.0.1:0".parse().expect("a loopback address")])
        .expect("a checked loopback address binds");
    assert!(listener.local_addr().expect("bound").ip().is_loopback());
    for exposed in ["0.0.0.0:0", "<host>:7883"] {
        let refused = bind_ready(&[exposed.parse().expect("an address")])
            .expect_err("an exposed address is refused before any bind");
        assert_eq!(refused.kind(), std::io::ErrorKind::PermissionDenied, "{exposed}: {refused}");
    }
}

/// The wire's text: it binds `Ready::bind` and nothing re-resolves the address string there.
#[test]
fn the_wire_never_resolves_the_address_string_again() {
    let source = include_str!("gatewayhub_cli/wire.rs");
    let code: String =
        source.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
    assert!(code.contains("bind_ready(&ready.bind)"), "the listener is bound from `Ready::bind`");
    for resolver in ["bind_loopback(", "to_socket_addrs", "gatewayhub_addr"] {
        assert!(!code.contains(resolver), "the wire must not touch the row's string: {resolver}");
    }
}

// -- the status verb's budget rows --------------------------------------------------------------

/// The shadow ledger reaches the status report as two rows per line, one per pool, keyed by the
/// ledger's own venue, host and scope words; an empty ledger is no rows.
#[test]
fn the_budget_snapshot_becomes_two_counter_rows_per_line() {
    let t0 = Instant::now();
    let budget = crate::budget::ledger::Budget::new();
    assert!(budget_counters(&budget.snapshot(t0)).is_empty(), "nothing spent, nothing reported");

    // binance perp history: a soft limit of 2000 a minute, so a 20% reserve and an 80% pool.
    budget.spend("binance", Market::Perp, Axis::History, Class::Bulk, 100, t0);
    let rows = budget_counters(&budget.snapshot(t0));
    assert_eq!(rows.len(), 2, "{rows:?}");
    let (order, shared) = (&rows[0], &rows[1]);
    assert_eq!((order.class.as_str(), shared.class.as_str()), ("order", "shared"));
    for row in &rows {
        assert_eq!(
            (row.venue.as_str(), row.host.as_str(), row.scope.as_str()),
            ("binance", "perp", "ip")
        );
        assert_eq!(row.window_ms, 60_000);
    }
    assert_eq!((order.used, shared.used), (0, 100), "a bulk request draws the shared pool");
    assert_eq!(order.cap + shared.cap, 2_000, "the two pools are the whole admitted ceiling");
    assert_eq!((order.would_throttle, shared.would_throttle), (0, 0));

    // An over-spend is DISCLOSED, on the pool that was over, and still counted: shadow refuses nothing.
    budget.spend("binance", Market::Perp, Axis::History, Class::Bulk, 5_000, t0);
    let rows = budget_counters(&budget.snapshot(t0));
    assert_eq!(rows[0].would_throttle, 0);
    assert!(rows[1].would_throttle >= 1, "the over-spend rides the shared row: {rows:?}");
    assert_eq!(rows[1].used, 5_100, "…and was counted in full");
}

// -- the run order, in text ---------------------------------------------------------------------

/// `run_with`'s body with whole-line comments removed — the code the order test reads.
fn run_with_code() -> String {
    let source = include_str!("gatewayhub_cli.rs");
    let from = source.find("pub fn run_with(").expect("run_with is in this file");
    let body = &source[from..];
    let end = body.find("\n}\n").expect("run_with ends at a column-0 brace");
    body[..end].lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n")
}

/// THE ORDER, pinned in text so the wire PR that edits this file cannot reorder it unnoticed:
/// argv, then the stop handlers, then the boot, the log subscriber, the egress declaration, the
/// bind guard, and only then the wire. (`crates/vike-ops/tests/venues/polymarket_egress_declared_gate.rs`
/// is the cross-crate form of the egress half once the bridge is linked.)
#[test]
fn the_phases_run_in_the_documented_order() {
    let body = run_with_code();
    let phases = [
        "short_circuit(args)",
        "stop::install_handlers(",
        "boot_root(vars, cwd)",
        "vike_log::init(",
        "load_venue_settings",
        "declare_polymarket_egress(",
        "check_bind(&addr)",
        "wire(Ready",
    ];
    let mut at = 0;
    for phase in phases {
        let found = body[at..]
            .find(phase)
            .unwrap_or_else(|| panic!("`{phase}` is missing, or stands above the phase before it"));
        at += found + phase.len();
    }
    // The egress declaration is a STATEMENT of the root's own body, at depth zero — not under an
    // `if` or a `match` that runs on some paths only.
    let line = body
        .lines()
        .find(|l| l.contains("declare_polymarket_egress("))
        .expect("the declaration call");
    assert!(
        line.starts_with("    declare_polymarket_egress("),
        "the declaration must stand in the function's own body, unconditionally: {line:?}"
    );
    // …and it is compiled exactly where the bridge is linked: the line above it is the `#[cfg]`
    // naming the one feature that links it (the gate compares that against the manifest).
    let lines: Vec<&str> = body.lines().collect();
    let at = lines.iter().position(|l| *l == line).expect("the declaration line");
    assert_eq!(lines[at - 1].trim(), "#[cfg(feature = \"data\")]");
}

/// `run` plugs the real wire in with ONE argument and re-spells no phase.
#[test]
fn run_is_run_with_over_the_real_wire() {
    let source = include_str!("gatewayhub_cli.rs");
    let from = source.find("pub fn run(").expect("run is in this file");
    let body = &source[from..];
    let body = &body[..body.find("\n}\n").expect("run ends at a column-0 brace")];
    assert!(body.contains("run_with(vars, cwd, args, wire::serve_wire)"), "{body}");
    assert!(
        !body.contains("boot_root") && !body.contains("short_circuit"),
        "no phase is spelled in `run`: {body}"
    );
}
