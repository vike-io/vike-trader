use super::*;
use std::assert_matches;
use vike_bridge_core::venue_mount_fixture::{MountFixture, found_tier_events};
use vike_log::capture::{CapturedEvent, captured};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::{FillReport, OrderStatusReport, PositionStatusReport};

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

/// The one resolution that arms: a loadable shim AND an account below `live` holding its login.
const ARMED: Resolution = Resolution::Armed {
    tier: AccountTier::Demo,
    held_below_live: Some(HeldBelowLive::DemoOnlyArm),
};

/// A reconcile client that reports nothing — what a planted factory hands back, so a test can tell
/// "the factory's client was kept" from "no factory was asked" on a box where the real factory
/// could never log in.
struct NoReports;

impl ReconClient for NoReports {
    fn fetch_order_status_reports(&self, _since: i64) -> Result<Vec<OrderStatusReport>, String> {
        Ok(Vec::new())
    }
    fn fetch_fill_reports(&self, _since: i64) -> Result<Vec<FillReport>, String> {
        Ok(Vec::new())
    }
    fn fetch_position_status_reports(&self) -> Result<Vec<PositionStatusReport>, String> {
        Ok(Vec::new())
    }
}

#[test]
fn the_declaration_is_the_rows_the_mount_crate_carried() {
    let d = FxcmVenueMount.declaration();
    assert_eq!(FxcmVenueMount.venue(), "fxcm");
    assert!(d.addresses_accounts && d.process_exclusive.is_none() && !d.takes_recon_trigger);
    assert_eq!(d.grid_source, DeclaredGridSource::NoGrid);
    assert_matches!(d.book_identity, BookIdentity::Undeterminable { .. });
    assert_matches!(d.clock, ClockDecl::NotWired { unmeasured_risk: None, .. });
}

/// THE ONE TEXT CHANGE this port makes on purpose (the venue mount contract spec's Finding 3): the
/// clock row said this venue had "no make_engine arm" while it had one. The reason must still be a
/// sentence an operator can act on.
#[test]
fn the_clock_reason_says_why_and_no_longer_denies_the_arm() {
    let ClockDecl::NotWired { reason, .. } = FxcmVenueMount.declaration().clock else {
        panic!("fxcm reads no clock");
    };
    assert!(reason.len() >= 60, "too short to be a reason: {reason}");
    assert!(!reason.contains("make_engine") && !reason.contains("no arm"), "stale: {reason}");
}

/// MOVED from `vike-mount` (`fxcm_live_intent`'s tests and the arming row built on them), and
/// ordered with the rule the arming screen and the log share: a LIVE account's login is named
/// first, then the shim, then the demo login. The pure decision under BOTH shim answers — the
/// `true` one is unreachable on every CI runner, which is why the shim is a parameter here.
#[test]
fn the_pure_decision_names_a_stand_alone_live_login_then_the_shim_then_the_demo_login() {
    let creds = MountFixture::new(KEYS);
    let empty = MountFixture::new(&[]);
    for tier in [AccountTier::Paper, AccountTier::Demo] {
        assert_eq!(resolution_for(true, &creds.inputs(tier)), ARMED, "shim + login arm");
        assert_eq!(
            resolution_for(false, &creds.inputs(tier)),
            Resolution::Paper(PaperCause::SdkAbsent),
            "no shim: the mount refuses, whatever the account holds"
        );
    }
    for tier in AccountTier::ALL {
        assert_eq!(
            resolution_for(true, &empty.inputs(tier)),
            Resolution::Paper(PaperCause::NoCredentials),
            "a shim with no login is the ordinary unconfigured state ({tier})"
        );
        assert_eq!(
            resolution_for(false, &empty.inputs(tier)),
            Resolution::Paper(PaperCause::SdkAbsent),
            "an unconfigured account on a box without the shim is told the shim: only a LIVE \
             account's login is named ahead of it, and the log stays silent (absent is silent)"
        );
        for half in [&KEYS[..1], &KEYS[1..]] {
            assert_eq!(
                resolution_for(true, &MountFixture::new(half).inputs(tier)),
                Resolution::Paper(PaperCause::NoCredentials),
                "half a login is no login ({tier})"
            );
        }
    }
    // …and a LIVE account's login is named FIRST, with the shim and without it.
    for sdk in [false, true] {
        assert_eq!(
            resolution_for(sdk, &creds.inputs(AccountTier::Live)),
            Resolution::Paper(PaperCause::LiveTierNotWired),
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
    let (creds, empty) = (MountFixture::new(KEYS), MountFixture::new(&[]));
    let shim_loads = crate::sdk_available();
    for tier in [AccountTier::Paper, AccountTier::Demo] {
        let configured = FxcmVenueMount.resolve(&creds.inputs(tier));
        let unconfigured = FxcmVenueMount.resolve(&empty.inputs(tier));
        if shim_loads {
            assert_eq!(configured, ARMED, "a box with the shim arms a configured account");
            assert_eq!(unconfigured, Resolution::Paper(PaperCause::NoCredentials));
        } else {
            assert_eq!(
                configured,
                Resolution::Paper(PaperCause::SdkAbsent),
                "a login on a box without the shim must be refused, never armed"
            );
            assert_eq!(
                unconfigured,
                Resolution::Paper(PaperCause::SdkAbsent),
                "an unconfigured account on a box without the shim is told the shim (only a live \
                 account's login is named ahead of it)"
            );
        }
    }
    assert_eq!(
        FxcmVenueMount.resolve(&creds.inputs(AccountTier::Live)),
        Resolution::Paper(PaperCause::LiveTierNotWired),
        "a live account's login is named on every box"
    );
}

/// Review Focus 2: the mount resolves the DEMO login only, so no tier makes it resolve `Live` —
/// even with a loadable shim assumed.
#[test]
fn no_tier_ever_resolves_live() {
    let fx = MountFixture::new(KEYS);
    for tier in AccountTier::ALL {
        for sdk in [false, true] {
            assert!(
                !matches!(
                    resolution_for(sdk, &fx.inputs(tier)),
                    Resolution::Armed { tier: AccountTier::Live, .. }
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
    for half in [MountFixture::new(&KEYS[..1]), MountFixture::new(&KEYS[1..])] {
        assert_eq!(
            resolution_for(true, &half.inputs(AccountTier::Live)),
            Resolution::Paper(PaperCause::NoCredentials)
        );
    }
}

/// The stay-paper half at the MOUNT, with the shim assumed loadable and then not: a live account's
/// login does NOT RUN and asks no reconcile factory, whichever way the shim answers.
#[test]
fn a_live_accounts_login_mounts_paper_and_asks_no_factory() {
    let fx = MountFixture::new(KEYS);
    let (tx, _rx) = vike_exec::event_channel(8);
    for sdk in [false, true] {
        let mut req = fx.request(AccountTier::Live, "EURUSD", &tx);
        req.recon_enabled = true;
        let out = mount_with(req, sdk, |_, _| panic!("a live account: no reconcile session"));
        assert!(matches!(out.exec, ExecOutcome::Paper), "sdk={sdk}");
        assert!(out.recon.is_none() && out.identity.is_none(), "sdk={sdk}");
    }
}

#[test]
fn with_no_credentials_the_mount_is_paper_and_offline() {
    let fx = MountFixture::new(&[]);
    let (tx, _rx) = vike_exec::event_channel(8);
    let out = FxcmVenueMount.mount(fx.request(AccountTier::Demo, "EURUSD", &tx));
    assert!(matches!(out.exec, ExecOutcome::Paper));
    assert!(out.recon.is_none() && out.identity.is_none());
}

/// THE REFUSAL, at the bridge, through `mount` itself and so at THIS box's shim (the vike-mount twin
/// moves to `vike-tradehub`): credentials PRESENT, shim NOT loadable ⇒ paper. The EXEC half is what
/// this proves — a `mount` that stopped asking the shim would come back LIVE here. That nothing
/// reconciles is asserted too, but on this box it would hold whatever the ORDER of the steps, since
/// a reconcile login fails without the shim exactly as the exec one does; the order is pinned
/// through the seam, by `the_refusal_comes_before_the_reconcile_factory`.
///
/// On a box whose shim DOES load, the same call must mount LIVE, bound to the DEMO tier — so this
/// asserts on both kinds of box rather than skipping on one. The login is [`UNREACHABLE_LOGIN`], so
/// there both the exec session and the real reconcile factory fail their logins at the FFI boundary
/// and nothing reaches the network; nothing reconciles on that box either.
#[test]
fn credentials_without_a_loadable_shim_refuse_the_live_mount() {
    let fx = MountFixture::new(UNREACHABLE_KEYS);
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut req = fx.request(AccountTier::Demo, "EURUSD", &tx);
    // Reconciliation ON, as on a live-configured box, so the outcome is the one such a box gets.
    req.recon_enabled = true;
    let out = FxcmVenueMount.mount(req);
    if crate::sdk_available() {
        assert!(
            matches!(out.exec, ExecOutcome::Live(LiveExec { bound_tier: AccountTier::Demo, .. })),
            "a loadable shim and a login mount LIVE, on the DEMO tier"
        );
    } else {
        assert!(matches!(out.exec, ExecOutcome::Paper), "no shim must never mount live");
    }
    assert!(out.recon.is_none() && out.identity.is_none());
}

/// THE ORDER, through the seam and so on every box: with a login present and the shim refused, the
/// reconcile factory is never ASKED — reconciliation on — so a refused mount opens no second
/// ForexConnect session and logs no `fxcm reconcile:` failure beside its refusal.
#[test]
fn the_refusal_comes_before_the_reconcile_factory() {
    let fx = MountFixture::new(KEYS);
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut req = fx.request(AccountTier::Demo, "EURUSD", &tx);
    req.recon_enabled = true;
    let mut asked = false;
    let out = mount_with(req, false, |_, _| {
        asked = true;
        Some(Box::new(NoReports))
    });
    assert!(!asked, "the refusal must be decided before any reconcile session is opened");
    assert!(matches!(out.exec, ExecOutcome::Paper), "a refused mount is paper");
    assert!(out.recon.is_none() && out.identity.is_none());
}

/// THE LIVE BRANCH, through the seam with the shim assumed loadable — the branch no CI runner reaches
/// through `mount`. Its outcome is what `vike-mount` folds: a client bound to the DEMO tier, no grid,
/// no contract size, no margin mode, no leg grids and no identity; and the reconcile factory asked
/// exactly when reconciliation is on, with the resolved login and the mounted symbol, its client
/// kept. Offline on EVERY box: the spawned client's session thread fails its login — at the shim
/// lookup where there is no shim, at the FFI boundary on [`UNREACHABLE_LOGIN`] where there is one —
/// and waits in `refuse_every_command` until the client drops.
#[test]
fn a_loadable_shim_mounts_a_demo_bound_client_and_reconciles_only_when_asked() {
    let fx = MountFixture::new(UNREACHABLE_KEYS);
    let (tx, _rx) = vike_exec::event_channel(8);
    for recon_enabled in [false, true] {
        let mut req = fx.request(AccountTier::Demo, "EURUSD", &tx);
        req.recon_enabled = recon_enabled;
        let mut asked = None;
        let out = mount_with(req, true, |cfg, symbol| {
            asked = Some((cfg.user.clone(), symbol.to_string()));
            Some(Box::new(NoReports))
        });
        let ExecOutcome::Live(live) = out.exec else {
            panic!("a loadable shim and a DEMO login mount LIVE");
        };
        assert_eq!(
            live.bound_tier,
            AccountTier::Demo,
            "the credentials authenticate the DEMO tier"
        );
        assert!(live.grid.is_none(), "no symbol-properties endpoint: the permissive grid");
        assert!(live.contract_size.is_none() && live.margin_mode.is_none());
        assert!(live.leg_grids.is_empty());
        assert!(out.identity.is_none());
        if recon_enabled {
            assert_eq!(asked, Some((UNREACHABLE_LOGIN.to_string(), "EURUSD".to_string())));
            assert!(out.recon.is_some(), "the factory's reconcile client is the one kept");
        } else {
            assert_eq!(asked, None, "reconciliation off: the second login is never attempted");
            assert!(out.recon.is_none());
        }
    }
}

// ── what the arm says about a LIVE account it will not use ──────────────────────────────────────

/// A distinctive stand-in for a secret, so "no value was logged" is an assertion that can fail.
const SECRET: &str = "SECRET-VALUE-do-not-log";

/// A complete login for `label` (every value is [`SECRET`]) and nothing else.
fn secret_login(label: &str) -> MountFixture {
    let mut fx = MountFixture::new(&[("USER", SECRET), ("PASSWORD", SECRET)]);
    fx.account = AccountLabel::parse(label).expect("a legal label");
    fx
}

/// The DEMO login (the [`UNREACHABLE_LOGIN`] one, so a spawned session opens nothing on any box)
/// for `label`, and nothing else.
fn unreachable_login(label: &str) -> MountFixture {
    let mut fx = MountFixture::new(UNREACHABLE_KEYS);
    fx.account = AccountLabel::parse(label).expect("a legal label");
    fx
}

/// A `live` account holding its whole login used to read as an empty store. It does NOT RUN, shim
/// or no shim, and says so once, at `error!`, naming the account and never a value.
#[test]
fn a_live_accounts_login_is_said_once_and_the_mount_is_paper() {
    let fx = secret_login("LIVEACCT");
    for sdk in [false, true] {
        let (tx, _rx) = vike_exec::event_channel(8);
        let (out, events) = captured(|| {
            mount_with(fx.request(AccountTier::Live, "EURUSD", &tx), sdk, |_, _| {
                panic!("a live account mounts no demo login, so no reconcile session")
            })
        });
        assert!(matches!(out.exec, ExecOutcome::Paper), "sdk={sdk}");
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
    let mut fx = MountFixture::new(&[("USER", SECRET)]);
    fx.account = AccountLabel::parse("HALF").expect("a legal label");
    for sdk in [false, true] {
        let (tx, _rx) = vike_exec::event_channel(8);
        let (out, events) = captured(|| {
            mount_with(fx.request(AccountTier::Live, "EURUSD", &tx), sdk, |_, _| {
                panic!("no complete login, so no reconcile session")
            })
        });
        assert!(matches!(out.exec, ExecOutcome::Paper), "sdk={sdk}");
        assert!(found_tier_events(&events).is_empty(), "sdk={sdk}: {events:?}");
        assert!(!format!("{events:?}").contains(SECRET), "no credential VALUE: {events:?}");
    }
    assert_eq!(
        resolution_for(true, &fx.inputs(AccountTier::Live)),
        Resolution::Paper(PaperCause::NoCredentials),
        "the cause the arming screen prints"
    );
}

/// A LABELLED account is mounted only when it armed, so a `live` one never reaches `mount` and said
/// nothing at all — `vike-mount` asks the arm to speak for it instead, and the arm says exactly
/// what `mount` says for the default account. The hook asks the account, never the shim. An empty
/// and a demo account are silent.
#[test]
fn an_account_that_is_never_mounted_is_spoken_for_in_the_same_words() {
    let live = secret_login("UNMNT");
    let (_, events) =
        captured(|| FxcmVenueMount.report_unmounted_account(&live.inputs(AccountTier::Live)));
    let said = found_tier_events(&events);
    assert_eq!(said.len(), 1, "{events:?}");
    assert_eq!(said[0].level, tracing::Level::ERROR, "{events:?}");
    assert_eq!(said[0].field("account"), Some("UNMNT"), "{events:?}");
    assert!(!format!("{events:?}").contains(SECRET), "no credential VALUE: {events:?}");

    for (quiet, tier) in [(&MountFixture::new(&[]), AccountTier::Live), (&live, AccountTier::Demo)]
    {
        let (_, events) = captured(|| FxcmVenueMount.report_unmounted_account(&quiet.inputs(tier)));
        assert!(events.is_empty(), "silent ({tier}): {events:?}");
    }
}

/// **ONE RULE, TWO SURFACES.** The arming screen (`resolve`) and the log (`mount`) used to order
/// their questions differently — `resolve` asked the shim first, `mount` asked the credentials
/// first — so a box without the ForexConnect shim holding only a LIVE login printed `SdkAbsent` on
/// the screen and `LiveTierNotWired` in the log. Both were true, and an operator reading one then
/// the other had two answers to one question.
///
/// The rule both now follow: **a LIVE account's login is named first** (it is a fact about the
/// ACCOUNT that no box can change), **then the shim** (a fact about the BOX), **then the demo
/// login**.
///
/// Each row is `(shim loaded, account)` → the cause `resolve` answers and what `mount` says. An
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
    let paper = Resolution::Paper;
    let cells: [(bool, &str, Resolution, Says); 6] = [
        (false, "empty", paper(PaperCause::SdkAbsent), Nothing),
        (false, "demo", paper(PaperCause::SdkAbsent), TheShim),
        (false, "live", paper(PaperCause::LiveTierNotWired), TheLiveLogin),
        (true, "empty", paper(PaperCause::NoCredentials), Nothing),
        (true, "demo", ARMED, Nothing),
        (true, "live", paper(PaperCause::LiveTierNotWired), TheLiveLogin),
    ];
    for (i, (sdk, account, cause, says)) in cells.into_iter().enumerate() {
        let label = format!("AGREE{i}");
        let (fx, tier) = match account {
            "empty" => (MountFixture::new(&[]), AccountTier::Demo),
            "demo" => (unreachable_login(&label), AccountTier::Demo),
            _ => (unreachable_login(&label), AccountTier::Live),
        };
        assert_eq!(resolution_for(sdk, &fx.inputs(tier)), cause, "shim={sdk} account={account}");
        let (tx, _rx) = vike_exec::event_channel(8);
        let (out, events) = captured(|| {
            mount_with(fx.request(tier, "EURUSD", &tx), sdk, |_, _| Some(Box::new(NoReports)))
        });
        // The mount's outcome agrees with the screen's tier, cell by cell.
        assert_eq!(
            matches!(out.exec, ExecOutcome::Live(_)),
            matches!(cause, Resolution::Armed { .. }),
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
