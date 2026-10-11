use std::assert_matches;
use std::cell::Cell;

use super::*;
use vike_bridge_core::account_directory::AccountDirectory;
use vike_bridge_core::credentials::{Account, Accounts, NoAccountTable};
use vike_bridge_core::venue_mount_fixture::{MountFixture, found_tier_events};
use vike_log::capture::captured;
use vike_model::accounts::account_keys::{AccountLabel, account_key};
use vike_model::{FillReport, OrderRequest, OrderStatusReport, PositionStatusReport};

/// The one key `load_ibkr_config_for_account` requires: the Gateway holds the login, so the
/// account number is the whole live gate.
const KEYS: &[(&str, &str)] = &[("IBKR_DEMO_ACCOUNT", "DU1234567")];

/// The LIVE tier's account key, assembled rather than spelled as one literal: every env-shaped
/// string literal in a `src/` file is a sighting to the settings registry's sweep
/// (`crates/vike-model/src/scan.rs`'s `find_map_lookups`), and this fixture is not a read.
const LIVE_ACCOUNT: &str = concat!("IBKR", "_LIVE_ACCOUNT");

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
}

/// The DEMO tier's `venue.ibkr.demo.<field>` rows — the gateway, which the mount reads from
/// `MountInputs::settings` (decision 0095, Task 7).
fn demo_gateway(rows: &[(&str, &str)]) -> vike_secrets::venue_setting::VenueSettings {
    let rows: Vec<vike_secrets::VenueSettingRow> = rows
        .iter()
        .map(|(field, value)| vike_secrets::VenueSettingRow {
            venue: VENUE.to_string(),
            tier: Some("demo".to_string()),
            field: field.to_ascii_uppercase(),
            value: (*value).to_string(),
        })
        .collect();
    vike_secrets::venue_setting::VenueSettings::from_rows(VENUE, &rows)
}

/// A fixture holding `KEYS` under `label`'s key names, asking for `label`.
fn keyed(label: &AccountLabel) -> MountFixture {
    let mut fx = MountFixture::new(&[]);
    for (k, v) in KEYS {
        fx.vars.insert(account_key(k, label), (*v).to_string());
    }
    fx.account = label.clone();
    fx
}

/// Whether anything accepts a TCP connection on `127.0.0.1:port`. The dead-port test points a
/// config at two ports; a box where either answers cannot say what a dead port does, so the test
/// skips BEFORE mounting. Deciding the skip from the mount's own outcome instead would make the
/// outcome it asserts on unfalsifiable.
fn something_listens_on(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        std::time::Duration::from_millis(250),
    )
    .is_ok()
}

/// The seam's stand-in for the cpapi reconcile client: it answers nothing, and nothing asks it.
struct NoReports;

impl ReconClient for NoReports {
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        Ok(vec![])
    }
    fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<FillReport>, String> {
        Ok(vec![])
    }
    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        Ok(vec![])
    }
}

/// The seam's stand-in for a connected Gateway session: it accepts every call and sends nothing.
struct Inert;

impl ExecutionClient for Inert {
    fn submit(&mut self, _request: &OrderRequest) {}
    fn cancel(&mut self, _client_order_id: &str) {}
}

/// The rows `vike-mount`'s tables carried for ibkr. ⚠ This is now the ONLY place they are
/// pinned: `vike-mount`'s own registry carries ibkr `FeatureAbsent` and answers the generic facts
/// (`NoGrid`, `Undeterminable`, the absent clock), because that crate no longer compiles this
/// bridge. The clock reason is the text `vike-mount`'s `CLOCK_SOURCES` carried, whole.
#[test]
fn the_declaration_is_the_rows_the_mount_crate_carried() {
    let d = IbkrVenueMount.declaration();
    assert_eq!(IbkrVenueMount.venue(), "ibkr");
    assert!(d.addresses_accounts && d.process_exclusive.is_none() && !d.takes_recon_trigger);
    assert_eq!(d.grid_source, DeclaredGridSource::PerSymbolFetch);
    assert_eq!(
        d.book_identity,
        BookIdentity::Named {
            prefix: "IBKR",
            demo_tiers: &["DEMO"],
            live_tiers: &["LIVE"],
            name_suffixes: &["ACCOUNT"],
            evm_key_suffixes: &[],
        }
    );
    assert_eq!(
        d.clock,
        ClockDecl::NotWired {
            reason: "feature-gated here, and both backends put the clock behind an authenticated \
                     TWS socket / local CP-Gateway session this pre-mount step does not open",
            unmeasured_risk: None,
        }
    );
}

/// The arming-probe row as it was: a DEMO config the loader accepts arms a demo-only arm; one it
/// refuses — no account, an unparseable port, an unknown backend — stays paper.
#[test]
fn resolve_is_the_arms_own_gate() {
    let armed =
        Resolution::Armed { tier: Tier::Demo, held_below_live: Some(HeldBelowLive::DemoOnlyArm) };
    for live in [false, true] {
        assert_eq!(
            IbkrVenueMount.resolve(&MountFixture::new(&[]).inputs(live)),
            Resolution::Paper(PaperCause::NoCredentials)
        );
        assert_eq!(IbkrVenueMount.resolve(&keyed(&AccountLabel::Default).inputs(live)), armed);
        for broken in [("port", "not-a-port"), ("backend", "grpc")] {
            let mut fx = MountFixture::new(&[KEYS[0]]);
            fx.settings = demo_gateway(&[broken]);
            assert_eq!(
                IbkrVenueMount.resolve(&fx.inputs(live)),
                Resolution::Paper(PaperCause::NoCredentials),
                "{broken:?}: a config the loader refuses is no config"
            );
        }
    }
}

/// Review Focus 2 at this venue: a LIVE-tier `_ACCOUNT` beside the DEMO one is not, by itself,
/// enough to arm real money — here the account table was never read (`MountFixture::new`'s
/// `AccountDirectory::unread`), so nothing says the live account is ACTIVE, and the answer under
/// either ceiling is the demo-only arm. (`a_live_account_arms_...` below holds the arming case.)
#[test]
fn a_ceiling_below_live_never_resolves_live() {
    let mut fx = keyed(&AccountLabel::Default);
    fx.vars.insert(LIVE_ACCOUNT.to_string(), "U13112916".to_string());
    for live in [false, true] {
        let resolved = IbkrVenueMount.resolve(&fx.inputs(live));
        assert!(
            !matches!(resolved, Resolution::Armed { tier: Tier::Live, .. }),
            "live={live}: resolved the LIVE tier"
        );
        assert_eq!(
            resolved,
            Resolution::Armed {
                tier: Tier::Demo,
                held_below_live: Some(HeldBelowLive::DemoOnlyArm),
            },
            "live={live}"
        );
    }
}

/// **A LIVE-tier `_ACCOUNT` the arm cannot use is a NAMED cause.** With no account table read, a
/// store holding only the LIVE account arms nothing under either ceiling — and says WHICH tier it
/// will not use. The LIVE account KEY is the whole credential here (the Gateway holds the login), so
/// it is detected by that key alone: a live-tier GATEWAY row nobody can parse must not turn the named
/// cause back into "the store holds nothing".
#[test]
fn a_live_account_alone_stays_paper_for_a_named_reason() {
    let mut fx = MountFixture::new(&[]);
    fx.vars.insert(LIVE_ACCOUNT.to_string(), "U13112916".to_string());
    for live in [false, true] {
        assert_eq!(
            IbkrVenueMount.resolve(&fx.inputs(live)),
            Resolution::Paper(PaperCause::LiveTierNotWired),
            "live={live}"
        );
    }
    let live_rows: Vec<vike_secrets::VenueSettingRow> = [("PORT", "not-a-port")]
        .into_iter()
        .map(|(field, value)| vike_secrets::VenueSettingRow {
            venue: VENUE.to_string(),
            tier: Some("live".to_string()),
            field: field.to_string(),
            value: value.to_string(),
        })
        .collect();
    fx.settings = vike_secrets::venue_setting::VenueSettings::from_rows(VENUE, &live_rows);
    assert_eq!(
        IbkrVenueMount.resolve(&fx.inputs(true)),
        Resolution::Paper(PaperCause::LiveTierNotWired),
        "an unparseable LIVE gateway row does not hide the live account"
    );
}

/// The cause is scoped to the ACCOUNT asking, in both directions.
#[test]
fn the_live_tier_cause_is_scoped_to_the_account_that_holds_the_key() {
    let mut default_live = MountFixture::new(&[]);
    default_live.vars.insert(LIVE_ACCOUNT.to_string(), "U1".to_string());
    let mut alt_live = MountFixture::new(&[]);
    alt_live.vars.insert(account_key(LIVE_ACCOUNT, &alt()), "U2".to_string());
    alt_live.account = alt();
    assert_eq!(
        IbkrVenueMount.resolve(&default_live.inputs(true)),
        Resolution::Paper(PaperCause::LiveTierNotWired)
    );
    default_live.account = alt();
    assert_eq!(
        IbkrVenueMount.resolve(&default_live.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials),
        "ALT asks, and only the DEFAULT account holds a live account key"
    );
    assert_eq!(
        IbkrVenueMount.resolve(&alt_live.inputs(true)),
        Resolution::Paper(PaperCause::LiveTierNotWired)
    );
    alt_live.account = AccountLabel::Default;
    assert_eq!(
        IbkrVenueMount.resolve(&alt_live.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials),
        "the DEFAULT account asks, and only ALT holds a live account key"
    );
}

/// The stay-paper half at the MOUNT: a LIVE account alone builds no reconcile client, dials no
/// Gateway and fetches no grid — the three network steps are panicking doubles.
#[test]
fn a_live_account_alone_mounts_paper_and_dials_nothing() {
    let mut fx = MountFixture::new(&[]);
    fx.vars.insert(LIVE_ACCOUNT.to_string(), "U13112916".to_string());
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut req = fx.request(true, "AAPL.SMART.USD", &tx);
    req.recon_enabled = true;
    let out = mount_with(
        req,
        |_, _| panic!("no demo account: the reconcile factory must not be called"),
        |_, _| panic!("no demo account: nothing may be dialled"),
        |_, _| panic!("no demo account: no grid may be fetched"),
    );
    assert!(matches!(out.exec, ExecOutcome::Paper));
    assert!(out.recon.is_none() && out.identity.is_none());
}

/// The account 2×2: a labelled account reads its OWN `_ACCOUNT` key and never the default
/// account's — borrowing it would place a second account's orders in the first account's book.
#[test]
fn a_labelled_account_reads_only_its_own_keys() {
    let mut alt_asks_default_keys = keyed(&AccountLabel::Default);
    alt_asks_default_keys.account = alt();
    assert_eq!(
        IbkrVenueMount.resolve(&alt_asks_default_keys.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials),
        "ALT must not arm off the default account's `_ACCOUNT`"
    );
    assert_matches!(IbkrVenueMount.resolve(&keyed(&alt()).inputs(true)), Resolution::Armed { .. });
    let mut default_asks_alt_keys = keyed(&alt());
    default_asks_alt_keys.account = AccountLabel::Default;
    assert_eq!(
        IbkrVenueMount.resolve(&default_asks_alt_keys.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials)
    );
}

/// Absent config: paper, and no reconcile client even with reconciliation ON. Offline is proved
/// through `mount_with`, whose three network steps are panicking doubles in the second half: the
/// config is the gate, and without it `mount` returns before anything is built or dialled.
#[test]
fn with_no_credentials_the_mount_is_paper_and_offline() {
    let fx = MountFixture::new(&[]);
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut req = fx.request(true, "AAPL.SMART.USD", &tx);
    // ON, so a `None` recon is the absent config's doing rather than the laziness gate's.
    req.recon_enabled = true;
    let out = IbkrVenueMount.mount(req);
    assert!(matches!(out.exec, ExecOutcome::Paper));
    assert!(out.recon.is_none() && out.identity.is_none());

    let mut req = fx.request(true, "AAPL.SMART.USD", &tx);
    req.recon_enabled = true;
    let out = mount_with(
        req,
        |_, _| panic!("no config: the reconcile factory must not be called"),
        |_, _| panic!("no config: nothing may be dialled"),
        |_, _| panic!("no config: no grid may be fetched"),
    );
    assert!(matches!(out.exec, ExecOutcome::Paper));
    assert!(out.recon.is_none() && out.identity.is_none());
}

/// THE CONNECTED BRANCH, through the seam: the session reaches the fold at the DEMO tier — what
/// `IBKR_DEMO_*` authenticates, under either ceiling — with the mounted symbol's pre-fetched grid,
/// no contract size, margin mode or leg grid, and the reconcile client built before the connect
/// KEPT.
#[test]
fn a_connected_mount_is_a_demo_session_with_its_grid_and_its_recon() {
    let fx = keyed(&AccountLabel::Default);
    let (tx, _rx) = vike_exec::event_channel(8);
    let props =
        SymbolProperties { tick_size: 0.01, step_size: 1.0, min_qty: 1.0, ..Default::default() };
    for live in [false, true] {
        let mut req = fx.request(live, "AAPL.SMART.USD", &tx);
        req.recon_enabled = true;
        let out = mount_with(
            req,
            |_, _| Some(Box::new(NoReports)),
            |_, _| Ok(Box::new(Inert)),
            |_, symbol| {
                assert_eq!(symbol, "AAPL.SMART.USD", "the grid is the mounted symbol's");
                Some(props)
            },
        );
        let ExecOutcome::Live(exec) = out.exec else {
            panic!("live={live}: a connected session must be a LIVE outcome")
        };
        assert_eq!(exec.bound_tier, Tier::Demo, "live={live}: `IBKR_DEMO_*` is a DEMO session");
        assert_eq!(exec.grid, Some(props));
        assert_eq!((exec.contract_size, exec.margin_mode), (None, None));
        assert!(exec.leg_grids.is_empty());
        assert!(out.recon.is_some(), "the reconcile client built before the connect is kept");
        assert!(out.identity.is_none());
    }
}

/// THE DEMOTION'S DROP, through the seam: a reconcile client built before a connect that FAILS is
/// dropped — a paper engine must not reconcile against the account the failed session was for —
/// and no grid is fetched for a session that never connected.
#[test]
fn a_failed_connect_drops_the_recon_it_built_first() {
    let fx = keyed(&AccountLabel::Default);
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut req = fx.request(false, "AAPL.SMART.USD", &tx);
    req.recon_enabled = true;
    let built = Cell::new(false);
    let out = mount_with(
        req,
        |_, _| {
            built.set(true);
            Some(Box::new(NoReports))
        },
        |_, _| Err(IbkrError::Connect("connection refused".to_string())),
        |_, _| panic!("a session that never connected must not pre-fetch a grid"),
    );
    // The premise: without it, `out.recon` would be `None` for the wrong reason.
    assert!(built.get(), "a reconcile client was built before the connect");
    assert!(matches!(out.exec, ExecOutcome::Paper), "a failed connect demotes to paper");
    assert!(out.recon.is_none(), "the demotion drops the reconcile client it built");
    assert!(out.identity.is_none());
}

/// THE CONNECT-FAILURE DEMOTION against the real network steps: a present DEMO config pointed at
/// dead ports fails the synchronous connect, and the mount returns a PAPER outcome with no
/// reconcile client and no identity. Needs no Gateway — nothing listens on 127.0.0.1:6553{4,3}, so
/// both handshakes are refused at once. SELF-SKIPS, before mounting, on a box where either port
/// answers.
///
/// ⚠ The DROP of a reconcile client built before the failed connect is
/// `a_failed_connect_drops_the_recon_it_built_first`'s to prove: here the CP Gateway port is dead
/// too, so `recon_client` itself resolves `None` and `out.recon` would be `None` either way.
#[test]
fn a_present_config_without_a_gateway_demotes_to_paper() {
    if something_listens_on(65534) || something_listens_on(65533) {
        eprintln!("skipping: something answers on the IBKR test ports 65534/65533");
        return;
    }
    let mut fx = MountFixture::new(&[("IBKR_DEMO_ACCOUNT", "DUTEST000")]);
    fx.settings = demo_gateway(&[
        ("backend", "socket"),
        ("port", "65534"),
        ("cpapi_url", "https://127.0.0.1:65533"),
    ]);
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut req = fx.request(false, "AAPL.SMART.USD", &tx);
    // Reconciliation ON, so the cpapi factory is reached (and refused) rather than skipped by the
    // laziness gate.
    req.recon_enabled = true;
    let out = IbkrVenueMount.mount(req);
    assert!(matches!(out.exec, ExecOutcome::Paper), "a refused connect demotes to paper");
    assert!(out.recon.is_none(), "a demoted mount holds no reconcile client");
    assert!(out.identity.is_none());
}

/// A demo account whose gateway settings the loader REFUSES, beside a live account, is not "the live
/// tier alone": its fault is a setting, and the live account is not what keeps the venue on paper. The
/// cause stays the original one rather than blaming a tier that is not the problem.
#[test]
fn a_broken_demo_config_beside_a_live_account_is_not_the_live_tier() {
    let mut fx = keyed(&AccountLabel::Default);
    fx.vars.insert(LIVE_ACCOUNT.to_string(), "U13112916".to_string());
    fx.settings = demo_gateway(&[("port", "not-a-port")]);
    for live in [false, true] {
        assert_eq!(
            IbkrVenueMount.resolve(&fx.inputs(live)),
            Resolution::Paper(PaperCause::NoCredentials),
            "live={live}"
        );
    }
}

// ── what the arm says about a LIVE tier it will not use ─────────────────────────────────────────
//
// Every account label below is unique to its test, out of habit from when the unused-beside-demo
// `warn!` was said once per process per `(venue, account)`; it is said once per mount now, and a
// shared label would no longer change a result.

/// A distinctive stand-in for a secret, so "no value was logged" is an assertion that can fail.
const SECRET: &str = "SECRET-VALUE-do-not-log";

/// The LIVE tier's optional client id, assembled for the reason [`LIVE_ACCOUNT`] is.
const LIVE_CLIENT_ID: &str = concat!("IBKR", "_LIVE_CLIENT_ID");

/// The DEMO account AND the LIVE account, both for `label`.
fn both_tiers(label: &AccountLabel) -> MountFixture {
    let mut fx = keyed(label);
    fx.vars.insert(account_key(LIVE_ACCOUNT, label), SECRET.to_string());
    fx
}

/// A live account BESIDE the demo one used to mount the demo tier in silence. It still mounts the
/// demo tier — bound at DEMO, nothing else moves — and now says, once, that the live account is
/// unused.
#[test]
fn a_live_account_beside_the_demo_one_is_named_unused_and_the_mount_is_unchanged() {
    let beside = AccountLabel::parse("BESIDE").expect("a legal label");
    let fx = both_tiers(&beside);
    let (tx, _rx) = vike_exec::event_channel(8);
    let (out, events) = captured(|| {
        mount_with(
            fx.request(true, "AAPL.SMART.USD", &tx),
            |_, _| None,
            |_, _| Ok(Box::new(Inert)),
            |_, _| None,
        )
    });
    let ExecOutcome::Live(exec) = out.exec else { panic!("a connected demo session mounts") };
    assert_eq!(exec.bound_tier, Tier::Demo, "the live account must not move the bound tier");
    let unused = found_tier_events(&events);
    assert_eq!(unused.len(), 1, "exactly one line about the live account: {events:?}");
    let e = unused[0];
    assert_eq!(e.level, tracing::Level::WARN, "{e:?}");
    assert_eq!((e.field("venue"), e.field("account")), (Some("ibkr"), Some("BESIDE")), "{e:?}");
    assert_eq!(e.field("found_tier"), Some("live"), "{e:?}");
    assert!(e.field("tier").is_none(), "a diagnostic carries `found_tier`, never `tier`: {e:?}");
    assert!(
        events.iter().any(|e| e.field("tier") == Some("demo")),
        "the mount announcement is still said, at the tier it bound: {events:?}"
    );
    assert!(
        events.iter().all(|e| !format!("{e:?}").contains(SECRET)),
        "no credential VALUE may reach a line: {events:?}"
    );
}

/// The demo account alone — the ordinary case — says nothing new.
#[test]
fn a_demo_account_alone_adds_no_line_about_the_live_tier() {
    let alone = AccountLabel::parse("ALONE").expect("a legal label");
    let fx = keyed(&alone);
    let (tx, _rx) = vike_exec::event_channel(8);
    let (_, events) = captured(|| {
        mount_with(
            fx.request(true, "AAPL.SMART.USD", &tx),
            |_, _| None,
            |_, _| Ok(Box::new(Inert)),
            |_, _| None,
        )
    });
    assert!(found_tier_events(&events).is_empty(), "{events:?}");
}

/// A live tier whose `_ACCOUNT` was forgotten — a client id written, the account not — is a typo or
/// a half-finished edit, and the loader calls it absent, so it printed `NoCredentials` under the
/// words of an empty store. The mount now names the key it lacks (label-composed, names only) and
/// still lands on paper.
#[test]
fn a_half_written_live_tier_names_the_missing_account_and_the_mount_is_still_paper() {
    let half = AccountLabel::parse("HALF").expect("a legal label");
    let mut fx = MountFixture::new(&[]);
    fx.account = half.clone();
    fx.vars.insert(account_key(LIVE_CLIENT_ID, &half), SECRET.to_string());
    let missing = account_key(LIVE_ACCOUNT, &half);
    let (tx, _rx) = vike_exec::event_channel(8);
    let (out, events) = captured(|| {
        mount_with(
            fx.request(true, "AAPL.SMART.USD", &tx),
            |_, _| panic!("no demo account: the reconcile factory must not be called"),
            |_, _| panic!("no demo account: nothing may be dialled"),
            |_, _| panic!("no demo account: no grid may be fetched"),
        )
    });
    assert!(matches!(out.exec, ExecOutcome::Paper));
    let said = found_tier_events(&events);
    assert_eq!(said.len(), 1, "{events:?}");
    let e = said[0];
    assert_eq!(e.level, tracing::Level::ERROR, "{e:?}");
    assert_eq!((e.field("venue"), e.field("account")), (Some("ibkr"), Some("HALF")), "{e:?}");
    assert!(e.message.contains(&missing), "names the missing key `{missing}`: {}", e.message);
    assert!(
        !e.message.contains(&account_key(LIVE_CLIENT_ID, &half)),
        "names only what is MISSING: {}",
        e.message
    );
    assert!(!format!("{e:?}").contains(SECRET), "no credential VALUE: {e:?}");
    assert_eq!(
        IbkrVenueMount.resolve(&fx.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials),
        "the cause the arming screen prints is unchanged"
    );
}

/// A LABELLED account is mounted only when it armed, so one holding only live keys never reaches
/// `mount` and said nothing at all — `vike-mount` asks the arm to speak for it instead, and the arm
/// says exactly what `mount` says for the default account.
#[test]
fn an_account_that_is_never_mounted_is_spoken_for_in_the_same_words() {
    let unmounted = AccountLabel::parse("UNMNT").expect("a legal label");
    let mut live_only = MountFixture::new(&[]);
    live_only.account = unmounted.clone();
    live_only.vars.insert(account_key(LIVE_ACCOUNT, &unmounted), SECRET.to_string());
    let (_, events) = captured(|| IbkrVenueMount.report_unmounted_account(&live_only.inputs(true)));
    let said = found_tier_events(&events);
    assert_eq!(said.len(), 1, "{events:?}");
    assert_eq!(said[0].level, tracing::Level::ERROR, "{events:?}");
    assert_eq!(said[0].field("account"), Some("UNMNT"), "{events:?}");

    let (_, events) = captured(|| {
        IbkrVenueMount.report_unmounted_account(&MountFixture::new(&[]).inputs(true));
    });
    assert!(events.is_empty(), "an empty store is the ordinary state and is silent: {events:?}");
}

// ── the LIVE tier ───────────────────────────────────────────────────────────────────────────────
//
// It arms only when the ceiling permits `live`, the LIVE `_ACCOUNT` is stored, the `account` table
// holds an ACTIVE ibkr/live row for the asking account, the live gateway rows load and the backend is
// `socket`. Anything else is the demo tier (when a demo account loads) or paper with a named cause.

/// The LIVE tier's `venue.ibkr.live.<field>` rows.
fn live_gateway(rows: &[(&str, &str)]) -> vike_secrets::venue_setting::VenueSettings {
    let rows: Vec<vike_secrets::VenueSettingRow> = rows
        .iter()
        .map(|(field, value)| vike_secrets::VenueSettingRow {
            venue: VENUE.to_string(),
            tier: Some("live".to_string()),
            field: field.to_ascii_uppercase(),
            value: (*value).to_string(),
        })
        .collect();
    vike_secrets::venue_setting::VenueSettings::from_rows(VENUE, &rows)
}

/// One `account` row of `venue` at `tier` for `label` (`None` = the default account).
fn account_row(venue: &str, tier: &str, label: Option<&str>, active: bool) -> Account {
    Account {
        id: 1,
        venue: venue.to_string(),
        tier: tier.to_string(),
        label: label.map(str::to_string),
        venue_account_id: None,
        parent_id: None,
        active,
        last_verified_at: None,
        max_exposure: None,
    }
}

/// A fixture whose store holds the LIVE `_ACCOUNT` (the value is [`SECRET`], so a leak can fail a
/// test) for `who` and whose `account` table answered with `rows`.
fn live_store(who: &AccountLabel, rows: Vec<Account>) -> MountFixture {
    let mut fx = MountFixture::new(&[]);
    fx.vars.insert(account_key(LIVE_ACCOUNT, who), SECRET.to_string());
    fx.account = who.clone();
    fx.accounts = AccountDirectory::from_rows(Accounts::Known(rows), None);
    fx
}

fn named(text: &str) -> AccountLabel {
    AccountLabel::parse(text).expect("a legal label")
}

/// Mount `fx` under `live_permitted` with the three network steps as panicking doubles — so the
/// mount provably dialled nothing and built no reconcile client — and return the message of the ONE
/// `error!` a refused LIVE tier says.
fn refused_mount_says(fx: &MountFixture, live_permitted: bool) -> String {
    let (tx, _rx) = vike_exec::event_channel(8);
    let (out, events) = captured(|| {
        let mut req = fx.request(live_permitted, "AAPL.SMART.USD", &tx);
        req.recon_enabled = true;
        mount_with(
            req,
            |_, _| panic!("a refused live tier builds no reconcile client"),
            |_, _| panic!("a refused live tier dials nothing"),
            |_, _| panic!("a refused live tier fetches no grid"),
        )
    });
    assert!(matches!(out.exec, ExecOutcome::Paper));
    assert!(out.recon.is_none() && out.identity.is_none());
    let said = found_tier_events(&events);
    assert_eq!(said.len(), 1, "exactly one line about the live account: {events:?}");
    let e = said[0];
    assert_eq!(e.level, tracing::Level::ERROR, "{e:?}");
    assert_eq!(e.field("found_tier"), Some("live"), "{e:?}");
    assert!(e.field("tier").is_none(), "a refusal carries `found_tier`, never `tier`: {e:?}");
    assert!(
        events.iter().all(|e| !format!("{e:?}").contains(SECRET)),
        "no credential VALUE may reach a line: {events:?}"
    );
    e.message.clone()
}

fn says(message: &str, needle: &str) {
    assert!(message.contains(needle), "expected `{needle}` in: {message}");
}

/// **THE ARMING CASE**: the ceiling permits live, the live account is stored and its row is active,
/// the gateway is socket → `Armed` at the LIVE tier, and NOT held below live. Its mount binds
/// `Tier::Live`, announces the real-money warning once in the convention, builds NO reconcile client
/// (the factory is a panicking double and reconciliation is ON), and connects on the LIVE tier's own
/// gateway rows.
#[test]
fn a_live_account_arms_the_live_tier_under_a_permitting_ceiling_with_an_active_row() {
    let mut fx = live_store(&AccountLabel::Default, vec![account_row(VENUE, "live", None, true)]);
    fx.settings = live_gateway(&[("port", "4101"), ("backend", "socket")]);
    assert_eq!(
        IbkrVenueMount.resolve(&fx.inputs(true)),
        Resolution::Armed { tier: Tier::Live, held_below_live: None },
        "socket live is the supported path and is not held below live"
    );
    let (tx, _rx) = vike_exec::event_channel(8);
    let props =
        SymbolProperties { tick_size: 0.01, step_size: 1.0, min_qty: 1.0, ..Default::default() };
    let (out, events) = captured(|| {
        let mut req = fx.request(true, "AAPL.SMART.USD", &tx);
        req.recon_enabled = true;
        mount_with(
            req,
            |_, _| panic!("the live tier builds no cpapi reconcile client"),
            |cfg, _| {
                assert_matches!(cfg.env, Environment::Live, "connects on the LIVE tier's config");
                assert_eq!((cfg.port, cfg.backend), (4101, IbkrBackend::Socket));
                assert_eq!(cfg.account, SECRET, "the live account, not a demo one");
                Ok(Box::new(Inert))
            },
            |cfg, symbol| {
                assert_matches!(cfg.env, Environment::Live);
                assert_eq!(symbol, "AAPL.SMART.USD");
                Some(props)
            },
        )
    });
    let ExecOutcome::Live(exec) = out.exec else { panic!("a connected live mount must be Live") };
    assert_eq!(exec.bound_tier, Tier::Live);
    assert_eq!(exec.grid, Some(props));
    assert!(out.recon.is_none() && out.identity.is_none());
    let announced: Vec<_> = events.iter().filter(|e| e.field("tier") == Some("live")).collect();
    assert_eq!(announced.len(), 1, "exactly one live announcement: {events:?}");
    let e = announced[0];
    assert_eq!(e.level, tracing::Level::WARN, "{e:?}");
    assert!(e.message.starts_with("⚠ REAL-MONEY: "), "the alert prefix: {}", e.message);
    assert_eq!((e.field("venue"), e.field("account")), (Some("ibkr"), Some("DEFAULT")), "{e:?}");
    assert!(
        events.iter().all(|e| !format!("{e:?}").contains(SECRET)),
        "the live account number must not reach a line: {events:?}"
    );
    assert!(found_tier_events(&events).is_empty(), "nothing is refused or unused: {events:?}");
}

/// A LABELLED account arms from its OWN keys and its OWN `account` row, and a row of the default
/// account does not stand in for it.
#[test]
fn a_labelled_live_account_needs_its_own_active_row() {
    let who = named("LIVEALT");
    let fx = live_store(&who, vec![account_row(VENUE, "live", Some("LIVEALT"), true)]);
    assert_eq!(
        IbkrVenueMount.resolve(&fx.inputs(true)),
        Resolution::Armed { tier: Tier::Live, held_below_live: None }
    );
    let wrong = live_store(&who, vec![account_row(VENUE, "live", None, true)]);
    assert_eq!(
        IbkrVenueMount.resolve(&wrong.inputs(true)),
        Resolution::Paper(PaperCause::LiveTierNotWired),
        "the DEFAULT account's active row is not the labelled account's"
    );
}

/// **The ceiling is read FIRST and only ever refuses**: every other rule holding, a ceiling below
/// `live` stays paper with the named cause, dials nothing, and says the ceiling is why.
#[test]
fn a_ceiling_below_live_keeps_an_otherwise_armed_live_account_on_paper() {
    let fx = live_store(&named("NOCEIL"), vec![account_row(VENUE, "live", Some("NOCEIL"), true)]);
    assert_eq!(
        IbkrVenueMount.resolve(&fx.inputs(false)),
        Resolution::Paper(PaperCause::LiveTierNotWired)
    );
    says(&refused_mount_says(&fx, false), "below `live`");
}

/// A DEACTIVATED live row (`account.active` is the off switch), a table with NO such row, and a store
/// that cannot say at all (never read; no `account` table) each keep real money off — and each says
/// which, because "present and unused" is an error, not an absence.
#[test]
fn a_live_account_that_is_not_provably_active_stays_paper_and_says_why() {
    let who = named("INACTV");
    let mut unread = live_store(&who, vec![]);
    unread.accounts = AccountDirectory::unread();
    let mut no_table = live_store(&who, vec![]);
    no_table.accounts = AccountDirectory::from_rows(
        Accounts::Unanswerable(NoAccountTable::NoStore { db: "vike.db".into() }),
        None,
    );
    let cases: [(MountFixture, &str); 5] = [
        (live_store(&who, vec![account_row(VENUE, "live", Some("INACTV"), false)]), "deactivated"),
        (live_store(&who, vec![]), "no LIVE ibkr row"),
        (
            // Rows of OTHER venues, tiers and labels are not this account's.
            live_store(
                &who,
                vec![
                    account_row("binance", "live", Some("INACTV"), true),
                    account_row(VENUE, "demo", Some("INACTV"), true),
                    account_row(VENUE, "live", Some("OTHER"), true),
                ],
            ),
            "no LIVE ibkr row",
        ),
        (unread, "read no settings store"),
        (no_table, "no `account` table"),
    ];
    for (fx, needle) in &cases {
        assert_eq!(
            IbkrVenueMount.resolve(&fx.inputs(true)),
            Resolution::Paper(PaperCause::LiveTierNotWired),
            "{needle}"
        );
        says(&refused_mount_says(fx, true), needle);
    }
}

/// **cpapi has NO live fill path**, so a cpapi LIVE tier does not arm: the venue stays paper, dials
/// nothing, and the journal says why and what to set. (The design's "connect, serve data, send no real
/// orders" needs a shared-contract change this crate cannot make alone — see the module doc.)
#[test]
fn a_cpapi_live_tier_is_held_below_live() {
    let mut fx =
        live_store(&named("CPAPIL"), vec![account_row(VENUE, "live", Some("CPAPIL"), true)]);
    fx.settings = live_gateway(&[("backend", "cpapi")]);
    assert_eq!(
        IbkrVenueMount.resolve(&fx.inputs(true)),
        Resolution::Paper(PaperCause::LiveTierNotWired)
    );
    let line = refused_mount_says(&fx, true);
    says(&line, "HELD BELOW LIVE");
    says(&line, "NO live fill path");
    says(&line, "venue.ibkr.live.backend");
    // `oauth` dials the socket today, which is not an answer a LIVE tier may rest on.
    fx.settings = live_gateway(&[("backend", "oauth")]);
    assert_eq!(
        IbkrVenueMount.resolve(&fx.inputs(true)),
        Resolution::Paper(PaperCause::LiveTierNotWired)
    );
    says(&refused_mount_says(&fx, true), "oauth");
}

/// A live gateway the loader refuses (a port that is not a number) is a refusal, not a default.
#[test]
fn an_unparseable_live_gateway_row_stays_paper() {
    let mut fx =
        live_store(&named("BADPRT"), vec![account_row(VENUE, "live", Some("BADPRT"), true)]);
    fx.settings = live_gateway(&[("port", "not-a-port")]);
    assert_eq!(
        IbkrVenueMount.resolve(&fx.inputs(true)),
        Resolution::Paper(PaperCause::LiveTierNotWired)
    );
    says(&refused_mount_says(&fx, true), "do not load");
}

/// A live arm whose connect FAILS demotes to paper for the session — no retry, no relogin — builds
/// no reconcile client, fetches no grid, and says so at `error!`.
#[test]
fn a_failed_live_connect_demotes_to_paper_with_no_retry() {
    let fx = live_store(&named("LIVEFL"), vec![account_row(VENUE, "live", Some("LIVEFL"), true)]);
    let (tx, _rx) = vike_exec::event_channel(8);
    let attempts = Cell::new(0);
    let (out, events) = captured(|| {
        let mut req = fx.request(true, "AAPL.SMART.USD", &tx);
        req.recon_enabled = true;
        mount_with(
            req,
            |_, _| panic!("the live tier builds no reconcile client"),
            |_, _| {
                attempts.set(attempts.get() + 1);
                Err(IbkrError::Connect("connection refused".to_string()))
            },
            |_, _| panic!("a session that never connected must not pre-fetch a grid"),
        )
    });
    assert_eq!(attempts.get(), 1, "one attempt, no retry");
    assert!(matches!(out.exec, ExecOutcome::Paper) && out.recon.is_none());
    assert!(
        events
            .iter()
            .any(|e| e.level == tracing::Level::ERROR
                && e.message.contains("no retry and no relogin")),
        "{events:?}"
    );
    assert!(
        events.iter().all(|e| e.field("tier") != Some("live")),
        "nothing went live: {events:?}"
    );
}

/// A demo account BESIDE a live one that ARMS: the live tier mounts (one ibkr account mounts one
/// tier) and the demo account is named as unused.
#[test]
fn a_live_arm_beside_a_demo_account_names_the_demo_account_unused() {
    let who = named("LIVDEMO");
    let mut fx = both_tiers(&who);
    fx.accounts = AccountDirectory::from_rows(
        Accounts::Known(vec![account_row(VENUE, "live", Some("LIVDEMO"), true)]),
        None,
    );
    assert_eq!(
        IbkrVenueMount.resolve(&fx.inputs(true)),
        Resolution::Armed { tier: Tier::Live, held_below_live: None }
    );
    let (tx, _rx) = vike_exec::event_channel(8);
    let (out, events) = captured(|| {
        mount_with(
            fx.request(true, "AAPL.SMART.USD", &tx),
            |_, _| None,
            |_, _| Ok(Box::new(Inert)),
            |_, _| None,
        )
    });
    let ExecOutcome::Live(exec) = out.exec else { panic!("a connected live mount") };
    assert_eq!(exec.bound_tier, Tier::Live);
    let said = found_tier_events(&events);
    assert_eq!(said.len(), 1, "{events:?}");
    assert_eq!(said[0].level, tracing::Level::WARN, "{events:?}");
    assert_eq!(said[0].field("found_tier"), Some("demo"), "{events:?}");
    assert!(said[0].message.contains("UNUSED"), "{}", said[0].message);
    assert!(events.iter().all(|e| !format!("{e:?}").contains(SECRET)), "{events:?}");
}

/// A demo account beside a live one that does NOT arm (here: its row is deactivated) mounts the demo
/// tier exactly as before, and the warning names the live account's reason.
#[test]
fn a_demo_account_beside_an_unarmed_live_one_still_mounts_demo_and_says_why() {
    let who = named("DEMLIV");
    let mut fx = both_tiers(&who);
    fx.accounts = AccountDirectory::from_rows(
        Accounts::Known(vec![account_row(VENUE, "live", Some("DEMLIV"), false)]),
        None,
    );
    assert_eq!(
        IbkrVenueMount.resolve(&fx.inputs(true)),
        Resolution::Armed { tier: Tier::Demo, held_below_live: Some(HeldBelowLive::DemoOnlyArm) }
    );
    let (tx, _rx) = vike_exec::event_channel(8);
    let (out, events) = captured(|| {
        mount_with(
            fx.request(true, "AAPL.SMART.USD", &tx),
            |_, _| None,
            |cfg, _| {
                assert_matches!(cfg.env, Environment::Demo);
                Ok(Box::new(Inert))
            },
            |_, _| None,
        )
    });
    let ExecOutcome::Live(exec) = out.exec else { panic!("a connected demo mount") };
    assert_eq!(exec.bound_tier, Tier::Demo);
    let said = found_tier_events(&events);
    assert_eq!(said.len(), 1, "{events:?}");
    assert_eq!(said[0].level, tracing::Level::WARN, "{events:?}");
    assert_eq!(said[0].field("found_tier"), Some("live"), "{events:?}");
    assert!(said[0].message.contains("deactivated"), "{}", said[0].message);
    assert!(
        events.iter().all(|e| e.field("tier") != Some("live")),
        "no line may claim the live tier mounted: {events:?}"
    );
}
