use super::*;
use std::assert_matches;
use vike_bridge_core::venue_adapter::ArmingHeldBelowLive;
use vike_bridge_core::venue_adapter_fixture::{AdapterFixture, found_tier_events};
use vike_execution::recon::ReconcileClientFake;
use vike_log::capture::{CapturedEvent, captured};
use vike_model::accounts::account_keys::AccountLabel;

/// Both fields `load_fxcm_config_from` requires: the ForexConnect login and its password.
const KEYS: &[(&str, &str)] = &[("USER", "D251112911"), ("PASSWORD", "p")];

/// A login `FxcmSession::login` can never hand to the SDK: its interior NUL is refused by
/// `CString::new` at the FFI boundary, before any FFI call. So a test that SPAWNS a session with it
/// opens no socket on any box — including a box whose `target/debug/deps/` holds a stale, loadable
/// `libfcshim.so`, which `crates/bridges/fxcm/tests/fxcm_login_failure.rs` (`UNREACHABLE_LOGIN`,
/// the same device) measured on a the CI box lane. That is what lets the tests below that spawn a client
/// ASSERT on such a box instead of skipping there.
const UNREACHABLE_LOGIN: &str = "not-a-real-login\0with-an-interior-nul";

/// `KEYS` with the login swapped for [`UNREACHABLE_LOGIN`].
const UNREACHABLE_KEYS: &[(&str, &str)] = &[("USER", UNREACHABLE_LOGIN), ("PASSWORD", "p")];

/// The [`UNREACHABLE_KEYS`] login on a box holding the release's addresses.
fn unreachable_on_seed() -> AdapterFixture {
    let mut fx = AdapterFixture::new(UNREACHABLE_KEYS);
    fx.settings = vike_secrets::venue_setting::VenueSettings::default()
        .with_urls(vike_venues::venue_urls::VenueUrls::seed(VENUE));
    fx
}

/// The one resolution that arms: a loadable shim AND an account below `live` holding its login.
const ARMED: ArmingResolution = ArmingResolution::Armed {
    tier: AccountTier::Demo,
    held_below_live: Some(ArmingHeldBelowLive::DemoOnlyArm),
};

#[test]
fn the_declaration_is_the_rows_the_tradehub_core_carried() {
    let d = VenueAdapterFxcm.declaration();
    assert_eq!(VenueAdapterFxcm.venue(), "fxcm");
    assert!(d.addresses_accounts && d.process_exclusive.is_none() && !d.takes_reconcile_trigger);
    assert_eq!(d.grid_source, GridSource::NoGrid);
    assert_matches!(d.book_identity, BookIdentityRule::Undeterminable { .. });
    assert_matches!(d.clock, ClockDeclaration::NotWired { unmeasured_risk: None, .. });
}

/// THE ONE TEXT CHANGE this port makes on purpose (the venue-adapter contract spec's Finding 3): the
/// clock row said this venue had "no make_engine arm" while it had one. The reason must still be a
/// sentence an operator can act on.
#[test]
fn the_clock_reason_says_why_and_no_longer_denies_the_arm() {
    let ClockDeclaration::NotWired { reason, .. } = VenueAdapterFxcm.declaration().clock else {
        panic!("fxcm reads no clock");
    };
    assert!(reason.len() >= 60, "too short to be a reason: {reason}");
    assert!(!reason.contains("make_engine") && !reason.contains("no arm"), "stale: {reason}");
}

/// MOVED from `vike-tradehub-core` (`fxcm_live_intent`'s tests and the arming row built on them), and
/// ordered with the rule the arming screen and the log share: a LIVE account's login is named
/// first, then the shim, then the demo login. The pure decision under BOTH shim answers — the
/// `true` one is unreachable on every CI runner, which is why the shim is a parameter here.
#[test]
fn the_pure_decision_names_a_stand_alone_live_login_then_the_shim_then_the_demo_login() {
    let creds = AdapterFixture::new(KEYS);
    let empty = AdapterFixture::new(&[]);
    for tier in [AccountTier::Paper, AccountTier::Demo] {
        assert_eq!(resolution_for(true, &creds.inputs(tier)), ARMED, "shim + login arm");
        assert_eq!(
            resolution_for(false, &creds.inputs(tier)),
            ArmingResolution::NotRunning(ArmingNotRunningCause::SdkAbsent),
            "no shim: the mount refuses, whatever the account holds"
        );
    }
    for tier in AccountTier::ALL {
        assert_eq!(
            resolution_for(true, &empty.inputs(tier)),
            ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials),
            "a shim with no login is the ordinary unconfigured state ({tier})"
        );
        assert_eq!(
            resolution_for(false, &empty.inputs(tier)),
            ArmingResolution::NotRunning(ArmingNotRunningCause::SdkAbsent),
            "an unconfigured account on a box without the shim is told the shim: only a LIVE \
             account's login is named ahead of it, and the log stays silent (absent is silent)"
        );
        for half in [&KEYS[..1], &KEYS[1..]] {
            assert_eq!(
                resolution_for(true, &AdapterFixture::new(half).inputs(tier)),
                ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials),
                "half a login is no login ({tier})"
            );
        }
    }
    // …and a LIVE account's login is named FIRST, with the shim and without it.
    for sdk in [false, true] {
        assert_eq!(
            resolution_for(sdk, &creds.inputs(AccountTier::Live)),
            ArmingResolution::NotRunning(ArmingNotRunningCause::LiveTierNotWired),
            "a live account's login is named before the shim question (shim={sdk})"
        );
    }
}

/// THE WIRED DECISION, pinned to its ANSWERS rather than to the formula it is built from: on a box
/// whose shim does not load — every CI runner — `resolve` refuses a configured demo account
/// `SdkAbsent`, and refuses an unconfigured one for the shim too (only a live account's login is
/// named ahead of it), so no account can arm the venue there. On a box whose shim loads (never a CI
/// runner) the same two accounts must resolve armed and unconfigured. The shim is a process fact no
/// test can plant, so this is the only place the wiring to it is exercised; both answers of the
/// decision itself are pinned unconditionally in
/// `the_pure_decision_names_a_stand_alone_live_login_then_the_shim_then_the_demo_login`.
#[test]
fn resolve_refuses_a_configured_account_on_a_box_without_the_shim() {
    let (creds, empty) = (AdapterFixture::new(KEYS), AdapterFixture::new(&[]));
    let shim_loads = crate::sdk_available();
    for tier in [AccountTier::Paper, AccountTier::Demo] {
        let configured = VenueAdapterFxcm.resolve(&creds.inputs(tier));
        let unconfigured = VenueAdapterFxcm.resolve(&empty.inputs(tier));
        if shim_loads {
            assert_eq!(configured, ARMED, "a box with the shim arms a configured account");
            assert_eq!(
                unconfigured,
                ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials)
            );
        } else {
            assert_eq!(
                configured,
                ArmingResolution::NotRunning(ArmingNotRunningCause::SdkAbsent),
                "a login on a box without the shim must be refused, never armed"
            );
            assert_eq!(
                unconfigured,
                ArmingResolution::NotRunning(ArmingNotRunningCause::SdkAbsent),
                "an unconfigured account on a box without the shim is told the shim (only a live \
                 account's login is named ahead of it)"
            );
        }
    }
    assert_eq!(
        VenueAdapterFxcm.resolve(&creds.inputs(AccountTier::Live)),
        ArmingResolution::NotRunning(ArmingNotRunningCause::LiveTierNotWired),
        "a live account's login is named on every box"
    );
}

/// Review Focus 2: the start resolves the DEMO login only, so no tier makes it resolve `Live` —
/// even with a loadable shim assumed.
#[test]
fn no_tier_ever_resolves_live() {
    let fx = AdapterFixture::new(KEYS);
    for tier in AccountTier::ALL {
        for sdk in [false, true] {
            assert!(
                !matches!(
                    resolution_for(sdk, &fx.inputs(tier)),
                    ArmingResolution::Armed { tier: AccountTier::Live, .. }
                ),
                "{tier} shim={sdk}"
            );
        }
    }
}

/// Only a COMPLETE login on a `live` account is one: a user without its password is what
/// `load_fxcm_config_from` calls absent, so the original cause stands.
#[test]
fn half_a_live_login_is_still_no_credentials() {
    for half in [AdapterFixture::new(&KEYS[..1]), AdapterFixture::new(&KEYS[1..])] {
        assert_eq!(
            resolution_for(true, &half.inputs(AccountTier::Live)),
            ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials)
        );
    }
}

/// The stay-paper half at the START, with the shim assumed loadable and then not: a live account's
/// login does NOT RUN and asks no reconcile factory, whichever way the shim answers.
#[test]
fn a_live_accounts_login_starts_paper_and_asks_no_factory() {
    let fx = AdapterFixture::new(KEYS);
    let (tx, _rx) = vike_execution::event_channel(8);
    for sdk in [false, true] {
        let mut req = fx.request(AccountTier::Live, "EURUSD", &tx);
        req.reconcile_enabled = true;
        let out = start_with(req, sdk, |_, _| panic!("a live account: no reconcile session"));
        assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning), "sdk={sdk}");
        assert!(out.reconcile.is_none() && out.identity.is_none(), "sdk={sdk}");
    }
}

#[test]
fn with_no_credentials_the_start_is_paper_and_offline() {
    let fx = AdapterFixture::new(&[]);
    let (tx, _rx) = vike_execution::event_channel(8);
    let out = VenueAdapterFxcm.start(fx.request(AccountTier::Demo, "EURUSD", &tx));
    assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning));
    assert!(out.reconcile.is_none() && out.identity.is_none());
}

/// THE REFUSAL, at the bridge, through `start` itself and so through the shim question it asks (the
/// vike-tradehub-core twin moves to `vike-tradehub`): credentials PRESENT, shim NOT loadable ⇒ paper. The EXEC half is
/// what this proves — a `start` that stopped asking the shim would come back LIVE here. That nothing reconciles is
/// asserted too; the order of the steps is pinned through the seam, by `the_refusal_comes_before_the_reconcile_factory`.
///
/// ⚠ **The test states its own precondition** (`crate::transport::loader::test_seam`): this thread finds no shim, on every box. It
/// used to ask the box, and on the CI box, where the SDK is staged and the shim loads, the start went LIVE and its reconcile
/// client (`reconcile_client_redialing_for`, which keeps retrying a failed login) was kept, so `out.recon.is_none()` was red
/// while CI, which has no SDK, stayed green. The login is still [`UNREACHABLE_LOGIN`], so a start that stopped asking
/// would fail its logins at the FFI boundary and reach no network.
#[test]
fn credentials_without_a_loadable_shim_refuse_the_live_start() {
    crate::transport::loader::test_seam::refuse_on_this_thread();
    assert!(!crate::sdk_available(), "the seam answers for this thread");
    let fx = AdapterFixture::new(UNREACHABLE_KEYS);
    let (tx, _rx) = vike_execution::event_channel(8);
    let mut req = fx.request(AccountTier::Demo, "EURUSD", &tx);
    // Reconciliation ON, as on a live-configured box, so the outcome is the one such a box gets.
    req.reconcile_enabled = true;
    let out = VenueAdapterFxcm.start(req);
    assert!(
        matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning),
        "no shim must never mount live"
    );
    assert!(out.reconcile.is_none() && out.identity.is_none());
}

/// THE ORDER, through the seam and so on every box: with a login present and the shim refused, the
/// reconcile factory is never ASKED — reconciliation on — so a refused start opens no second
/// ForexConnect session and logs no `fxcm reconcile:` failure beside its refusal.
#[test]
fn the_refusal_comes_before_the_reconcile_factory() {
    let fx = AdapterFixture::new(KEYS);
    let (tx, _rx) = vike_execution::event_channel(8);
    let mut req = fx.request(AccountTier::Demo, "EURUSD", &tx);
    req.reconcile_enabled = true;
    let mut asked = false;
    let out = start_with(req, false, |_, _| {
        asked = true;
        Some(Box::new(ReconcileClientFake::default()))
    });
    assert!(!asked, "the refusal must be decided before any reconcile session is opened");
    assert!(
        matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning),
        "a refused mount is paper"
    );
    assert!(out.reconcile.is_none() && out.identity.is_none());
}

/// THE LIVE BRANCH, through the seam with the shim assumed loadable — the branch no CI runner reaches
/// through `start`. Its outcome is what `vike-tradehub-core` folds: a client bound to the DEMO tier, no grid,
/// no contract size, no margin mode, no leg grids and no identity; and the reconcile factory asked
/// exactly when reconciliation is on, with the resolved login and the engine symbol, its client
/// kept. Offline on EVERY box: the spawned client's session thread fails its login — at the shim
/// lookup where there is no shim, at the FFI boundary on [`UNREACHABLE_LOGIN`] where there is one —
/// and waits in `refuse_every_command` until the client drops.
#[test]
fn a_loadable_shim_starts_a_demo_bound_client_and_reconciles_only_when_asked() {
    let fx = unreachable_on_seed();
    let (tx, _rx) = vike_execution::event_channel(8);
    for recon_enabled in [false, true] {
        let mut req = fx.request(AccountTier::Demo, "EURUSD", &tx);
        req.reconcile_enabled = recon_enabled;
        let mut asked = None;
        let out = start_with(req, true, |config, symbol| {
            asked = Some((config.user.clone(), symbol.to_string()));
            Some(Box::new(ReconcileClientFake::default()))
        });
        let VenueAdapterExecutionOutcome::Running(live) = out.exec else {
            panic!("a loadable shim and a DEMO login mount LIVE");
        };
        assert_eq!(
            live.bound_tier,
            AccountTier::Demo,
            "the credentials authenticate the DEMO tier"
        );
        assert!(live.grid.is_none(), "no symbol-properties endpoint: the permissive grid");
        assert!(live.contract_value.is_none() && live.margin_mode.is_none());
        assert!(live.leg_grids.is_empty());
        assert!(out.identity.is_none());
        if recon_enabled {
            assert_eq!(asked, Some((UNREACHABLE_LOGIN.to_string(), "EURUSD".to_string())));
            assert!(out.reconcile.is_some(), "the factory's reconcile client is the one kept");
        } else {
            assert_eq!(asked, None, "reconciliation off: the second login is never attempted");
            assert!(out.reconcile.is_none());
        }
    }
}

/// **A whole demo login with no `fxcm demo login rest Demo` row still ARMS** (the login decides the
/// probe), and its start, past the shim, refuses BY NAME: no client, no reconcile session, and an
/// `error!` naming the missing row — never a built-in host.
#[test]
fn a_login_with_no_hosts_row_arms_and_its_start_refuses_by_name() {
    let fx = AdapterFixture::new(UNREACHABLE_KEYS);
    assert_eq!(resolution_for(true, &fx.inputs(AccountTier::Demo)), ARMED);
    let (tx, _rx) = vike_execution::event_channel(8);
    let mut req = fx.request(AccountTier::Demo, "EURUSD", &tx);
    req.reconcile_enabled = true;
    let (out, events) = captured(|| {
        start_with(req, true, |_, _| panic!("no address row: no reconcile session either"))
    });
    assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning));
    assert!(
        events.iter().any(|e| e.level == tracing::Level::ERROR
            && e.message.contains("demo login rest product=Demo")),
        "the start names the missing row: {events:?}"
    );
}

/// The login's URL and connection are the `fxcm demo login rest Demo` row of the release's seed:
/// what `FxcmSession::login` is handed, byte for byte.
#[test]
fn the_demo_login_dials_the_seeds_hosts_row_on_the_demo_connection() {
    let fx = unreachable_on_seed();
    let ArmingDemoOnly::Demo(Ok(config)) = login(&fx.inputs(AccountTier::Demo)) else {
        panic!("a whole demo login on the seed");
    };
    use vike_venues::venue_urls::{UrlChannel, UrlProtocol, UrlType, seed_url};
    let seed = seed_url(VENUE, UrlType::Demo, UrlChannel::Login, UrlProtocol::Rest, "Demo")
        .expect("the seed's demo login row");
    assert_eq!((config.url.as_str(), config.connection.as_str()), (seed, "Demo"));
}

// ── what the arm says about a LIVE account it will not use ──────────────────────────────────────

/// A distinctive stand-in for a secret, so "no value was logged" is an assertion that can fail.
const SECRET: &str = "SECRET-VALUE-do-not-log";

/// A complete login for `label` (every value is [`SECRET`]) and nothing else.
fn secret_login(label: &str) -> AdapterFixture {
    let mut fx = AdapterFixture::new(&[("USER", SECRET), ("PASSWORD", SECRET)]);
    fx.account = AccountLabel::parse(label).expect("a legal label");
    fx
}

/// The DEMO login (the [`UNREACHABLE_LOGIN`] one, so a spawned session opens nothing on any box)
/// for `label`, and nothing else.
fn unreachable_login(label: &str) -> AdapterFixture {
    let mut fx = unreachable_on_seed();
    fx.account = AccountLabel::parse(label).expect("a legal label");
    fx
}

/// A `live` account holding its whole login used to read as an empty store. It does NOT RUN, shim
/// or no shim, and says so once, at `error!`, naming the account and never a value.
#[test]
fn a_live_accounts_login_is_said_once_and_the_start_is_paper() {
    let fx = secret_login("LIVEACCT");
    for sdk in [false, true] {
        let (tx, _rx) = vike_execution::event_channel(8);
        let (out, events) = captured(|| {
            start_with(fx.request(AccountTier::Live, "EURUSD", &tx), sdk, |_, _| {
                panic!("a live account mounts no demo login, so no reconcile session")
            })
        });
        assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning), "sdk={sdk}");
        let said = found_tier_events(&events);
        assert_eq!(said.len(), 1, "sdk={sdk}: {events:?}");
        let e = said[0];
        assert_eq!(e.level, tracing::Level::ERROR, "{e:?}");
        assert_eq!(e.field("venue"), Some("fxcm"), "{e:?}");
        assert_eq!(e.field("account"), Some("LIVEACCT"), "{e:?}");
        assert_eq!(e.field("found_tier"), Some("live"), "{e:?}");
        assert!(e.field("tier").is_none(), "`found_tier`, never `tier`: {e:?}");
        assert!(
            events.iter().all(|e| !format!("{e:?}").contains(SECRET)),
            "no credential VALUE may reach a line: {events:?}"
        );
    }
}

/// A live account's login with its password missing is what the loader calls absent: paper,
/// `NoCredentials`, and silent — shim or no shim.
#[test]
fn a_half_written_live_login_is_no_credentials_and_says_nothing() {
    let mut fx = AdapterFixture::new(&[("USER", SECRET)]);
    fx.account = AccountLabel::parse("HALF").expect("a legal label");
    for sdk in [false, true] {
        let (tx, _rx) = vike_execution::event_channel(8);
        let (out, events) = captured(|| {
            start_with(fx.request(AccountTier::Live, "EURUSD", &tx), sdk, |_, _| {
                panic!("no complete login, so no reconcile session")
            })
        });
        assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning), "sdk={sdk}");
        assert!(found_tier_events(&events).is_empty(), "sdk={sdk}: {events:?}");
        assert!(!format!("{events:?}").contains(SECRET), "no credential VALUE: {events:?}");
    }
    assert_eq!(
        resolution_for(true, &fx.inputs(AccountTier::Live)),
        ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials),
        "the cause the arming screen prints"
    );
}

/// A LABELLED account is started only when it armed, so a `live` one never reaches `start` and said
/// nothing at all — `vike-tradehub-core` asks the arm to speak for it instead, and the arm says exactly
/// what `start` says for the default account. The hook asks the account, never the shim. An empty
/// and a demo account are silent.
#[test]
fn an_account_that_is_never_started_is_spoken_for_in_the_same_words() {
    let live = secret_login("UNMNT");
    let (_, events) =
        captured(|| VenueAdapterFxcm.report_not_running_account(&live.inputs(AccountTier::Live)));
    let said = found_tier_events(&events);
    assert_eq!(said.len(), 1, "{events:?}");
    assert_eq!(said[0].level, tracing::Level::ERROR, "{events:?}");
    assert_eq!(said[0].field("account"), Some("UNMNT"), "{events:?}");
    assert!(!format!("{events:?}").contains(SECRET), "no credential VALUE: {events:?}");

    for (quiet, tier) in
        [(&AdapterFixture::new(&[]), AccountTier::Live), (&live, AccountTier::Demo)]
    {
        let (_, events) =
            captured(|| VenueAdapterFxcm.report_not_running_account(&quiet.inputs(tier)));
        assert!(events.is_empty(), "silent ({tier}): {events:?}");
    }
}

/// **ONE RULE, TWO SURFACES.** The arming screen (`resolve`) and the log (`start`) used to order
/// their questions differently — `resolve` asked the shim first, `start` asked the credentials
/// first — so a box without the ForexConnect shim holding only a LIVE login printed `SdkAbsent` on
/// the screen and `LiveTierNotWired` in the log. Both were true, and an operator reading one then
/// the other had two answers to one question.
///
/// The rule both now follow: **a LIVE account's login is named first** (it is a fact about the
/// ACCOUNT that no box can change), **then the shim** (a fact about the BOX), **then the demo
/// login**.
///
/// Each row is `(shim loaded, account)` → the cause `resolve` answers and what `start` says. An
/// unconfigured account is SILENT in the log whatever the cause on the screen says: absent
/// credentials are the ordinary state, and the screen is where a missing shim is told.
#[test]
fn the_screen_and_the_log_follow_one_rule() {
    enum Says {
        /// Nothing about this venue at `error!`.
        Nothing,
        /// The shim refusal: an `error!` with a `reason`.
        TheShim,
        /// The live account's login: an `error!` with `found_tier`.
        TheLiveLogin,
    }
    use Says::{Nothing, TheLiveLogin, TheShim};
    let paper = ArmingResolution::NotRunning;
    let cells: [(bool, &str, ArmingResolution, Says); 6] = [
        (false, "empty", paper(ArmingNotRunningCause::SdkAbsent), Nothing),
        (false, "demo", paper(ArmingNotRunningCause::SdkAbsent), TheShim),
        (false, "live", paper(ArmingNotRunningCause::LiveTierNotWired), TheLiveLogin),
        (true, "empty", paper(ArmingNotRunningCause::NoCredentials), Nothing),
        (true, "demo", ARMED, Nothing),
        (true, "live", paper(ArmingNotRunningCause::LiveTierNotWired), TheLiveLogin),
    ];
    for (i, (sdk, account, cause, says)) in cells.into_iter().enumerate() {
        let label = format!("AGREE{i}");
        let (fx, tier) = match account {
            "empty" => (AdapterFixture::new(&[]), AccountTier::Demo),
            "demo" => (unreachable_login(&label), AccountTier::Demo),
            _ => (unreachable_login(&label), AccountTier::Live),
        };
        assert_eq!(resolution_for(sdk, &fx.inputs(tier)), cause, "shim={sdk} account={account}");
        let (tx, _rx) = vike_execution::event_channel(8);
        let (out, events) = captured(|| {
            start_with(fx.request(tier, "EURUSD", &tx), sdk, |_, _| {
                Some(Box::new(ReconcileClientFake::default()))
            })
        });
        // The start's outcome agrees with the screen's tier, cell by cell.
        assert_eq!(
            matches!(out.exec, VenueAdapterExecutionOutcome::Running(_)),
            matches!(cause, ArmingResolution::Armed { .. }),
            "shim={sdk} account={account}: what mounts"
        );
        let errors: Vec<&CapturedEvent> =
            events.iter().filter(|e| e.level == tracing::Level::ERROR).collect();
        match says {
            Nothing => assert!(errors.is_empty(), "shim={sdk} account={account}: {events:?}"),
            TheShim => {
                assert_eq!(errors.len(), 1, "shim={sdk} account={account}: {events:?}");
                assert!(errors[0].field("reason").is_some(), "the shim refusal: {:?}", errors[0]);
                assert!(errors[0].field("found_tier").is_none(), "{:?}", errors[0]);
            }
            TheLiveLogin => {
                assert_eq!(errors.len(), 1, "shim={sdk} account={account}: {events:?}");
                assert_eq!(errors[0].field("found_tier"), Some("live"), "{:?}", errors[0]);
                assert!(errors[0].field("reason").is_none(), "no shim refusal: {:?}", errors[0]);
            }
        }
    }
}

// ── what one unit of the MOUNTED symbol stands for ─────────────────────────────────────────────────

/// A reconcile session whose Offers table is the RECORDED one: the 449 class rows the EUR demo answered
/// (`tests/fixtures/offers_classes.json`), as the shim's `forex_connect_offers` array.
struct RecordedOffers;

impl crate::account::reads::SessionReads for RecordedOffers {
    fn system_properties(&self) -> crate::account::quote_rate::PropertiesRead {
        crate::account::quote_rate::PropertiesRead::Json("{}".to_string())
    }
    fn offers(&self) -> Result<String, String> {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/offers_classes.json"))
                .expect("the recorded table is JSON");
        Ok(fixture["offers"].to_string())
    }
}

/// Start `symbol` LIVE for `label` (the shim assumed loadable), the reconcile factory registering the recorded
/// session for the account exactly as the real one does when its login opens, when `register` is set.
fn start_mounted(label: &str, symbol: &str, recon_enabled: bool, register: bool) -> AdapterOutcome {
    let fx = unreachable_login(label);
    let (tx, _rx) = vike_execution::event_channel(8);
    let mut req = fx.request(AccountTier::Demo, symbol, &tx);
    req.reconcile_enabled = recon_enabled;
    let mut entry = None;
    let out = start_with(req, true, |_, _| {
        if register {
            entry =
                Some(crate::account::reads::register(label, std::sync::Arc::new(RecordedOffers)));
        }
        Some(Box::new(ReconcileClientFake::default()))
    });
    if let Some(entry) = entry {
        crate::account::reads::unregister(label, &entry);
    }
    out
}

/// **A CFD mount books its row's multiplier**, the value an admitted CFD is answered by: Copper 100 and US30 0.1
/// off the recorded rows, and spot gold, whose row states 1, the default. RED on the base: every mount stated none.
#[test]
fn a_cfd_mount_books_its_recorded_rows_multiplier() {
    for (symbol, want) in [
        ("COPPER", Some(vike_model::ContractValue::linear(100.0))),
        ("US30", Some(vike_model::ContractValue::linear(0.1))),
        ("XAUUSD", None),
    ] {
        let VenueAdapterExecutionOutcome::Running(live) =
            start_mounted("CFDMOUNT", symbol, true, true).exec
        else {
            panic!("{symbol}: a readable row mounts LIVE");
        };
        assert_eq!(live.contract_value, want, "{symbol}");
    }
}

/// **A CFD whose row cannot be read does not run**, never booked at 1: reconciliation off (no session to read
/// over), a reconcile login that did not open (nothing registered), and a symbol the table does not list each
/// leave the account NOT RUNNING with one `error!` line naming the symbol. A currency pair needs no read and
/// mounts with reconciliation off. RED on the base: every one of them mounted.
#[test]
fn a_cfd_mount_whose_row_cannot_be_read_does_not_run_and_a_pair_needs_no_read() {
    for (symbol, recon_enabled, register, why) in [
        ("COPPER", false, false, "no reconcile session"),
        ("COPPER", true, false, "no reconcile session"),
        ("NOSUCHCFD", true, true, "not in the venue's Offers table"),
    ] {
        let (out, events) = captured(|| start_mounted("NOROWCFD", symbol, recon_enabled, register));
        assert!(
            matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning),
            "{symbol}: NOT RUNNING"
        );
        assert!(out.reconcile.is_none(), "{symbol}: the reconcile client went with the outcome");
        let refusals: Vec<String> = events
            .iter()
            .filter(|e| e.level == tracing::Level::ERROR)
            .map(|e| format!("{e:?}"))
            .collect();
        assert_eq!(refusals.len(), 1, "{symbol}: {events:?}");
        assert!(
            refusals[0].contains(symbol) && refusals[0].contains(why),
            "{symbol}: {refusals:?}"
        );
    }
    let VenueAdapterExecutionOutcome::Running(pair) =
        start_mounted("PAIRMOUNT", "EURUSD", false, false).exec
    else {
        panic!("a currency pair mounts with no read");
    };
    assert_eq!(pair.contract_value, None, "a pair is linear 1.0");
}
