use super::*;
use vike_bridge_core::venue_adapter::ArmingNotRunningCause;
use vike_bridge_core::venue_adapter_fixture::{
    AdapterFixture, assert_demo_only_resolve, assert_never_resolves_live, assert_quiet_demo_start,
    found_tier_events,
};
use vike_log::capture::{CapturedEvent, captured};
use vike_model::accounts::account_keys::AccountLabel;

/// All three fields `load_ig_config_from` requires.
const KEYS: &[(&str, &str)] = &[("API_KEY", "k"), ("IDENTIFIER", "user"), ("PASSWORD", "pw")];

/// The epic every start below is asked for.
const EPIC: &str = "CS.D.EURUSD.CFD.IP";

/// What the market-read double states one unit of [`EPIC`] is worth: a Mini's 10,000.
const MINI: ContractValue = ContractValue::linear(10_000.0);

/// A fixture holding `fields` and the release's ig address rows (the bare fixture holds none, so
/// every start would be refused for its missing row).
fn seeded(fields: &[(&str, &str)]) -> AdapterFixture {
    let mut fx = AdapterFixture::new(fields);
    fx.settings = vike_secrets::venue_setting::VenueSettings::default()
        .with_urls(vike_venues::venue_urls::VenueUrls::seed(VENUE));
    fx
}

#[test]
fn the_declaration_is_the_rows_the_tradehub_core_carried() {
    let d = VenueAdapterIg.declaration();
    assert_eq!(VenueAdapterIg.venue(), "ig");
    assert!(d.addresses_accounts && d.process_exclusive.is_none() && !d.takes_reconcile_trigger);
    assert_eq!(d.grid_source, GridSource::NoGrid);
    assert_eq!(
        d.book_identity,
        BookIdentityRule::Named { name_fields: &["IDENTIFIER"], evm_key_fields: &[] }
    );
    assert_eq!(
        d.clock,
        ClockDeclaration::Wired {
            endpoint: "GET /session/encryptionKey (X-IG-API-KEY only)",
            auth: ClockAuthentication::Credentialed,
            risk: ClockRisk::NoTimestamp,
        }
    );
}

/// The arming-probe row: the whole trio on an account below `live` arms a demo-only arm; a `live`
/// account's whole trio is `LiveTierNotWired`; anything less stays `NoCredentials` at every tier.
#[test]
fn resolve_is_the_arms_own_gate() {
    assert_demo_only_resolve(&VenueAdapterIg, KEYS, &[&KEYS[..2]]);
}

/// ig's arm has no live tier at all: no tier makes it resolve `Live`.
#[test]
fn no_tier_ever_resolves_live() {
    assert_never_resolves_live(&VenueAdapterIg, KEYS);
}

/// The stay-paper half at the START: through `start_with`, a live account's trio spawns no exec
/// client and attempts no reconcile login, reconciliation on or off.
#[test]
fn a_live_accounts_trio_spawns_nothing_and_logs_in_nowhere() {
    let fx = AdapterFixture::new(KEYS);
    let (tx, _rx) = vike_execution::event_channel(8);
    for recon_enabled in [false, true] {
        let mut req = fx.request(AccountTier::Live, EPIC, &tx);
        req.reconcile_enabled = recon_enabled;
        let out = start_with(
            req,
            |_, _| panic!("a live account must not read a market row"),
            |_, _| panic!("a live account must not spawn an exec client"),
            |_, _| panic!("a live account must not attempt a reconcile login"),
        );
        assert!(
            matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning),
            "recon_enabled={recon_enabled}"
        );
        assert!(out.reconcile.is_none() && out.identity.is_none(), "recon_enabled={recon_enabled}");
    }
}

/// ig offers no startup credential probe — `vike-tradehub-core`'s credential leg never read an ig balance —
/// and that holds for an ARMED ig, the only case in which a probe is asked for.
#[test]
fn an_armed_ig_offers_no_credential_probe() {
    for tier in [AccountTier::Paper, AccountTier::Demo] {
        assert!(VenueAdapterIg.credential_probe(&AdapterFixture::new(KEYS).inputs(tier)).is_none());
    }
}

/// Through the REAL `start` (the real spawn and the real reconcile factory), an account without the
/// trio starts PAPER with no reconcile handle and no identity, with reconciliation ON in the
/// request. `out.recon` being `None` cannot show by itself that no login was attempted — the
/// factory also answers `None` when a login fails — so the `start_with` test below counts the
/// attempts.
#[test]
fn with_no_credentials_the_start_is_paper_even_when_reconciling() {
    let fx = AdapterFixture::new(&[]);
    let (tx, _rx) = vike_execution::event_channel(8);
    let mut req = fx.request(AccountTier::Demo, EPIC, &tx);
    req.reconcile_enabled = true;
    let out = VenueAdapterIg.start(req);
    assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning));
    assert!(out.reconcile.is_none() && out.identity.is_none());
}

/// What an ARMED ig start hands `vike-tradehub-core`, read off `start_with` — `start`'s own body — with the
/// market read, the exec spawn and the reconcile login replaced by offline doubles, so it reads what THIS
/// body sets: a live client bound at the DEMO tier (the tier the demo account's trio authenticates, which
/// `vike-tradehub-core` records the account against), the contract value the market row states (an EUR/USD
/// Mini's 10,000), no grid, margin mode or leg grids, and ONE reconcile login exactly when reconciliation
/// is on, its handle carried through. Without the trio no network step is taken, reconciliation on or off.
#[test]
fn an_armed_start_binds_demo_and_logs_in_to_reconcile_only_when_asked() {
    let (tx, _rx) = vike_execution::event_channel(8);
    let fx = seeded(KEYS);
    for tier in [AccountTier::Paper, AccountTier::Demo] {
        for recon_enabled in [false, true] {
            let row = format!("tier={tier} recon_enabled={recon_enabled}");
            let mut req = fx.request(tier, EPIC, &tx);
            req.reconcile_enabled = recon_enabled;
            let (mut spawns, mut logins) = (0usize, 0usize);
            let out = start_with(
                req,
                |config, epic| {
                    assert_eq!((config.identifier.expose().as_str(), epic), ("user", EPIC));
                    Ok(MINI)
                },
                |config, _events| {
                    spawns += 1;
                    assert_eq!(
                        config.identifier.expose(),
                        "user",
                        "the spawn is handed the resolved trio"
                    );
                    Box::new(vike_execution::testing::RecordingClient::default())
                },
                |config, symbol| {
                    logins += 1;
                    assert_eq!((config.identifier.expose().as_str(), symbol), ("user", EPIC));
                    let handle: Box<dyn ReconcileClient> =
                        Box::new(vike_execution::recon::ReconcileClientFake::default());
                    Some(handle)
                },
            );
            let VenueAdapterExecutionOutcome::Running(exec) = out.exec else {
                panic!("the trio is present, so the mount goes live ({row})");
            };
            assert_eq!(exec.bound_tier, AccountTier::Demo, "the tier the mount BINDS ({row})");
            assert!(exec.grid.is_none(), "{row}");
            assert_eq!(exec.contract_value, Some(MINI), "the market row's unit ({row})");
            assert!(exec.margin_mode.is_none() && exec.leg_grids.is_empty(), "{row}");
            assert!(out.identity.is_none(), "{row}");
            assert_eq!(spawns, 1, "{row}");
            assert_eq!(logins, usize::from(recon_enabled), "the reconcile login is LAZY ({row})");
            assert_eq!(out.reconcile.is_some(), recon_enabled, "{row}");
        }
    }
    let no_trio = AdapterFixture::new(&[]);
    for recon_enabled in [false, true] {
        let mut req = no_trio.request(AccountTier::Demo, EPIC, &tx);
        req.reconcile_enabled = recon_enabled;
        let out = start_with(
            req,
            |_, _| panic!("no trio, so no market row may be read"),
            |_, _| panic!("no trio, so no exec client may be spawned"),
            |_, _| panic!("no trio, so no reconcile login may be attempted"),
        );
        assert!(
            matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning),
            "recon_enabled={recon_enabled}"
        );
        assert!(out.reconcile.is_none() && out.identity.is_none(), "recon_enabled={recon_enabled}");
    }
}

/// With no key in the account, the credentialed clock read fails naming the missing FIELDS and no
/// URL.
#[test]
fn a_credentialed_clock_read_reports_its_own_missing_key() {
    let fx = AdapterFixture::new(&[]);
    let e = VenueAdapterIg
        .server_time_ms(&fx.inputs(AccountTier::Demo), std::time::Duration::from_secs(3))
        .expect_err("no IG credentials in an empty account");
    assert!(e.contains("API_KEY"), "the message must name what is missing, not the URL: {e}");
    assert!(!e.contains("http"), "no URL in a report line: {e}");

    // Only what is MISSING is named.
    let half = AdapterFixture::new(&KEYS[..1]);
    let e = VenueAdapterIg
        .server_time_ms(&half.inputs(AccountTier::Demo), std::time::Duration::from_secs(3))
        .expect_err("half a trio reads no clock");
    assert!(e.contains("IDENTIFIER") && e.contains("PASSWORD"), "{e}");
    assert!(!e.contains("API_KEY"), "the API key is present: {e}");
}

// ── what the arm says about a LIVE account it will not use ──────────────────────────────────────

/// A distinctive stand-in for a secret, so "no value was logged" is an assertion that can fail.
const SECRET: &str = "SECRET-VALUE-do-not-log";

/// The whole trio for `label`, every value [`SECRET`].
fn secret_trio(label: &str) -> AdapterFixture {
    let mut fx = seeded(&[("API_KEY", SECRET), ("IDENTIFIER", SECRET), ("PASSWORD", SECRET)]);
    fx.account = AccountLabel::parse(label).expect("a legal label");
    fx
}

/// **A demo trio whose gateway row is refused does not run, and says why**: it still ARMS (the probe
/// judges credentials), and the start reads no market row, spawns nothing and logs in nowhere, with
/// one `error!` naming the missing `venue_url` row and never a value.
#[test]
fn a_demo_trio_with_no_gateway_row_is_not_running_and_names_the_row() {
    let mut fx = secret_trio("NOURL");
    fx.settings = vike_secrets::venue_setting::VenueSettings::default();
    assert!(matches!(
        VenueAdapterIg.resolve(&fx.inputs(AccountTier::Demo)),
        ArmingResolution::Armed { .. }
    ));
    let (tx, _rx) = vike_execution::event_channel(8);
    let (out, events) = captured(|| {
        start_with(
            fx.request(AccountTier::Demo, EPIC, &tx),
            |_, _| panic!("no gateway, so no market read"),
            |_, _| panic!("no gateway, so no exec client"),
            |_, _| panic!("no gateway, so no reconcile login"),
        )
    });
    assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning), "NOT RUNNING");
    let errors: Vec<&CapturedEvent> =
        events.iter().filter(|e| e.level == tracing::Level::ERROR).collect();
    assert_eq!(errors.len(), 1, "{events:?}");
    assert!(errors[0].message.contains("venue_url"), "names the row: {:?}", errors[0]);
    assert!(events.iter().all(|e| !format!("{e:?}").contains(SECRET)), "{events:?}");
}

/// `start_with` over offline doubles: a market row stating a Mini's unit, a recording exec client, no
/// reconcile login.
fn start_offline(fx: &AdapterFixture, tier: AccountTier) -> (AdapterOutcome, Vec<CapturedEvent>) {
    let (tx, _rx) = vike_execution::event_channel(8);
    captured(|| {
        start_with(
            fx.request(tier, EPIC, &tx),
            |_, _| Ok(MINI),
            |_, _| Box::new(vike_execution::testing::RecordingClient::default()),
            |_, _| None,
        )
    })
}

/// **A mount whose market row cannot be read does not run**, never booked at 1: no exec client is spawned,
/// no reconcile login is tried, and one `error!` line names the epic and the reason, never a value.
#[test]
fn a_mount_whose_value_cannot_be_read_does_not_run() {
    let fx = secret_trio("NOROW");
    let (tx, _rx) = vike_execution::event_channel(8);
    let mut req = fx.request(AccountTier::Demo, EPIC, &tx);
    req.reconcile_enabled = true;
    let (out, events) = captured(|| {
        start_with(
            req,
            |_, _| Err("ig: GET /markets/CS.D.EURUSD.CFD.IP answered 503".to_string()),
            |_, _| panic!("an unvalued mount spawns no exec client"),
            |_, _| panic!("an unvalued mount logs in to no reconcile session"),
        )
    });
    assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning), "NOT RUNNING");
    assert!(out.reconcile.is_none() && out.identity.is_none());
    let refusals: Vec<&CapturedEvent> =
        events.iter().filter(|e| e.level == tracing::Level::ERROR).collect();
    assert_eq!(refusals.len(), 1, "{events:?}");
    let line = format!("{:?}", refusals[0]);
    assert!(line.contains(EPIC) && line.contains("answered 503") && line.contains("NOT RUNNING"));
    assert!(events.iter().all(|e| !format!("{e:?}").contains(SECRET)), "{events:?}");
}

/// A demo account's trio — the ordinary case — starts the demo gateway, bound at DEMO, announces
/// the start at the tier it bound, and says nothing about a live tier.
#[test]
fn a_demo_account_starts_and_says_nothing_about_a_live_tier() {
    let (out, events) = start_offline(&secret_trio("ALONE"), AccountTier::Demo);
    assert_quiet_demo_start(out, &events, SECRET);
}

/// A `live` account holding the whole trio used to read as an empty store. It does NOT RUN and says
/// so once, at `error!`, naming the account and never a value.
#[test]
fn a_live_accounts_trio_is_said_once_and_the_start_is_paper() {
    let (out, events) = start_offline(&secret_trio("LIVEACCT"), AccountTier::Live);
    assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning));
    let said = found_tier_events(&events);
    assert_eq!(said.len(), 1, "exactly one line about the live account: {events:?}");
    let e = said[0];
    assert_eq!(e.level, tracing::Level::ERROR, "{e:?}");
    assert_eq!((e.field("venue"), e.field("account")), (Some("ig"), Some("LIVEACCT")), "{e:?}");
    assert_eq!(e.field("found_tier"), Some("live"), "{e:?}");
    assert!(e.field("tier").is_none(), "a diagnostic carries `found_tier`, never `tier`: {e:?}");
    assert!(
        events.iter().all(|e| !format!("{e:?}").contains(SECRET)),
        "no credential VALUE may reach a line: {events:?}"
    );
}

/// Half a trio on a `live` account is what the loader calls absent: paper, `NoCredentials`, and
/// silent.
#[test]
fn a_half_written_set_is_no_credentials_and_says_nothing() {
    let mut fx = AdapterFixture::new(&KEYS[..1]);
    fx.account = AccountLabel::parse("HALF").expect("a legal label");
    let (tx, _rx) = vike_execution::event_channel(8);
    let (out, events) = captured(|| {
        start_with(
            fx.request(AccountTier::Live, EPIC, &tx),
            |_, _| panic!("no complete trio, so no market read"),
            |_, _| panic!("no complete trio, so no exec client"),
            |_, _| panic!("no complete trio, so no reconcile login"),
        )
    });
    assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning));
    assert!(found_tier_events(&events).is_empty(), "{events:?}");
    assert_eq!(
        VenueAdapterIg.resolve(&fx.inputs(AccountTier::Live)),
        ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials)
    );
}

/// A LABELLED account is started only when it armed, so a `live` one never reaches `start` and said
/// nothing at all — `vike-tradehub-core` asks the arm to speak for it instead, and the arm says exactly
/// what `start` says for the default account. An empty and a demo account are silent.
#[test]
fn an_account_that_is_never_started_is_spoken_for_in_the_same_words() {
    let live = secret_trio("UNMNT");
    let (_, events) =
        captured(|| VenueAdapterIg.report_not_running_account(&live.inputs(AccountTier::Live)));
    let said = found_tier_events(&events);
    assert_eq!(said.len(), 1, "{events:?}");
    assert_eq!(said[0].level, tracing::Level::ERROR, "{events:?}");
    assert_eq!(said[0].field("account"), Some("UNMNT"), "{events:?}");

    for (quiet, tier) in
        [(&AdapterFixture::new(&[]), AccountTier::Live), (&live, AccountTier::Demo)]
    {
        let (_, events) =
            captured(|| VenueAdapterIg.report_not_running_account(&quiet.inputs(tier)));
        assert!(events.is_empty(), "silent ({tier}): {events:?}");
    }
}

/// The account row's `venue_account_id` is the BOOK the operator named: the start hands every login of this
/// account that book in the config (`IgConfig::account_pin`), trimmed, and a row with none, a blank one, or no
/// row at all hands none (the rule picks, `crate::account::pick`).
#[test]
fn the_rows_book_rides_the_config_every_login_of_the_start_is_handed() {
    fn row(book: Option<&str>) -> vike_secrets::AccountRow {
        vike_secrets::AccountRow {
            id: 1,
            venue: VENUE.to_string(),
            tier: AccountTier::Demo,
            label: None,
            venue_account_id: book.map(str::to_string),
            parent_id: None,
            active: true,
            last_verified_at: None,
            max_exposure: None,
            leverage: None,
            symbol: vike_secrets::fixture_account_symbol(VENUE).map(str::to_string),
            facts: Default::default(),
        }
    }
    let pin_of_start = |fx: &AdapterFixture| {
        let (tx, _rx) = vike_execution::event_channel(8);
        let mut seen = None;
        start_with(
            fx.request(AccountTier::Demo, EPIC, &tx),
            |_, _| Ok(MINI),
            |config, _| {
                seen = Some(config.account_pin);
                Box::new(vike_execution::testing::RecordingClient::default())
            },
            |_, _| None,
        );
        seen.expect("the start spawned its exec client")
    };
    let with_row = |book: Option<&str>| {
        let mut fx = seeded(KEYS);
        fx.accounts = vike_secrets::account_directory::AccountDirectory::from_rows(
            vike_secrets::AccountRows::Known(vec![row(book)]),
            None,
        );
        fx
    };
    assert_eq!(pin_of_start(&with_row(Some(" ABC12 "))), Some("ABC12".to_string()));
    assert_eq!(pin_of_start(&with_row(Some("  "))), None, "a blank book is no book");
    assert_eq!(pin_of_start(&with_row(None)), None);
    assert_eq!(pin_of_start(&seeded(KEYS)), None, "an unread table names no book");
}
