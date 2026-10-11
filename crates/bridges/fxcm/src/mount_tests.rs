use super::*;
use std::assert_matches;
use vike_bridge_core::venue_mount_fixture::{MountFixture, found_tier_events};
use vike_log::capture::{CapturedEvent, captured};
use vike_model::accounts::account_keys::{AccountLabel, account_key};
use vike_model::{FillReport, OrderStatusReport, PositionStatusReport};

/// Both halves `load_fxcm_config_for_account` requires: the ForexConnect login and its password.
const KEYS: &[(&str, &str)] = &[("FXCM_DEMO_USER", "D251112911"), ("FXCM_DEMO_PASSWORD", "p")];

/// A login `FxcmSession::login` can never hand to the SDK: its interior NUL is refused by
/// `CString::new` at the FFI boundary, before any FFI call. So a test that SPAWNS a session with it
/// opens no socket on any box — including a box whose `target/debug/deps/` holds a stale, loadable
/// `libfcshim.so`, which `crates/bridges/fxcm/tests/fxcm_login_failure.rs` (`UNREACHABLE_LOGIN`,
/// the same device) measured on a the CI box lane. That is what lets the tests below that spawn a client
/// ASSERT on such a box instead of skipping there.
const UNREACHABLE_LOGIN: &str = "not-a-real-login\0with-an-interior-nul";

/// `KEYS` with the login swapped for [`UNREACHABLE_LOGIN`].
const UNREACHABLE_KEYS: &[(&str, &str)] =
    &[("FXCM_DEMO_USER", UNREACHABLE_LOGIN), ("FXCM_DEMO_PASSWORD", "p")];

/// The one resolution that arms: a loadable shim AND a DEMO login.
const ARMED: Resolution =
    Resolution::Armed { tier: Tier::Demo, held_below_live: Some(HeldBelowLive::DemoOnlyArm) };

fn alt() -> AccountLabel {
    AccountLabel::parse("ALT").expect("a legal label")
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
/// reordered with the rule the arming screen and the log now share: a stand-alone LIVE login is
/// named first, then the shim, then the demo login. The pure decision under BOTH shim answers — the
/// `true` one is unreachable on every CI runner, which is why the shim is a parameter here.
#[test]
fn the_pure_decision_names_a_stand_alone_live_login_then_the_shim_then_the_demo_login() {
    let creds = keyed(&AccountLabel::Default);
    let empty = MountFixture::new(&[]);
    for live in [false, true] {
        assert_eq!(resolution_for(true, &creds.inputs(live)), ARMED, "shim + login arm");
        assert_eq!(
            resolution_for(false, &creds.inputs(live)),
            Resolution::Paper(PaperCause::SdkAbsent),
            "no shim: the mount refuses, whatever the store says"
        );
        assert_eq!(
            resolution_for(true, &empty.inputs(live)),
            Resolution::Paper(PaperCause::NoCredentials),
            "a shim with no login is the ordinary unconfigured state"
        );
        assert_eq!(
            resolution_for(false, &empty.inputs(live)),
            Resolution::Paper(PaperCause::SdkAbsent),
            "an unconfigured store on a box without the shim is told the shim: only a stand-alone \
             LIVE login is named ahead of it, and the log stays silent (absent is silent)"
        );
        for half in [&KEYS[..1], &KEYS[1..]] {
            assert_eq!(
                resolution_for(true, &MountFixture::new(half).inputs(live)),
                Resolution::Paper(PaperCause::NoCredentials),
                "half a login is no login"
            );
        }
        // …and a stand-alone LIVE login is named FIRST, with the shim and without it.
        let (live_user, live_password) = crate::fxcm_env_var_names(Environment::Live);
        let live_only =
            MountFixture::new(&[(live_user.as_str(), "U"), (live_password.as_str(), "P")]);
        for sdk in [false, true] {
            assert_eq!(
                resolution_for(sdk, &live_only.inputs(live)),
                Resolution::Paper(PaperCause::LiveTierNotWired),
                "a stand-alone live login is named before the shim question (shim={sdk})"
            );
        }
    }
}

/// THE WIRED DECISION, pinned to its ANSWERS rather than to the formula it is built from: on a box
/// whose shim does not load — every CI runner — `resolve` refuses a configured account `SdkAbsent`,
/// and refuses an unconfigured one for the shim too (only a stand-alone live login is named ahead of it), so no store can arm the venue there. On
/// a box whose shim loads (never a CI runner) the same two stores must resolve armed and
/// unconfigured. The shim is a process fact no test can plant, so this is the only place the
/// wiring to it is exercised; both answers of the decision itself are pinned unconditionally in
/// `the_pure_decision_names_a_stand_alone_live_login_then_the_shim_then_the_demo_login`.
#[test]
fn resolve_refuses_a_configured_account_on_a_box_without_the_shim() {
    let (creds, empty) = (keyed(&AccountLabel::Default), MountFixture::new(&[]));
    let shim_loads = crate::sdk_available();
    for live in [false, true] {
        let configured = FxcmVenueMount.resolve(&creds.inputs(live));
        let unconfigured = FxcmVenueMount.resolve(&empty.inputs(live));
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
                "an unconfigured store on a box without the shim is told the shim (only a stand-alone live login is named ahead of it)"
            );
        }
    }
}

/// Review Focus 2: the mount resolves the DEMO tier only, so neither a ceiling nor a live-named
/// login makes it resolve `Live` — even with a loadable shim assumed.
#[test]
fn a_ceiling_below_live_never_resolves_live() {
    let mut fx = keyed(&AccountLabel::Default);
    // The live tier's login, composed by the crate's own naming function.
    let (live_user, live_password) = crate::fxcm_env_var_names(Environment::Live);
    fx.vars.insert(live_user.clone(), "U".to_string());
    fx.vars.insert(live_password.clone(), "P".to_string());
    for live in [false, true] {
        for sdk in [false, true] {
            assert!(!matches!(
                resolution_for(sdk, &fx.inputs(live)),
                Resolution::Armed { tier: Tier::Live, .. }
            ));
        }
    }
    // …and a store holding ONLY a live-tier login arms nothing at all, even with the shim: the
    // mount reads the DEMO login and nothing else.
    let live_only = MountFixture::new(&[(live_user.as_str(), "U"), (live_password.as_str(), "P")]);
    for live in [false, true] {
        assert_eq!(
            resolution_for(true, &live_only.inputs(live)),
            Resolution::Paper(PaperCause::LiveTierNotWired),
            "a live-tier login alone is not the DEMO login this mount reads — and the cause says \
             that, rather than that the store holds nothing"
        );
        assert_eq!(
            resolution_for(false, &live_only.inputs(live)),
            Resolution::Paper(PaperCause::LiveTierNotWired),
            "a stand-alone live login is named FIRST, on a box without the shim too: it is a fact \
             about the store that no shim can change, and the log says the same thing \
             (`the_screen_and_the_log_follow_one_rule`)"
        );
    }
}

/// Only a COMPLETE live-tier login is one: a user without its password is what
/// `load_fxcm_config_for_account` calls absent, so the original cause stands.
#[test]
fn half_a_live_login_is_still_no_credentials() {
    let (live_user, live_password) = crate::fxcm_env_var_names(Environment::Live);
    for half in [
        MountFixture::new(&[(live_user.as_str(), "U")]),
        MountFixture::new(&[(live_password.as_str(), "P")]),
    ] {
        for live in [false, true] {
            assert_eq!(
                resolution_for(true, &half.inputs(live)),
                Resolution::Paper(PaperCause::NoCredentials),
                "live={live}"
            );
        }
    }
}

/// The cause is scoped to the ACCOUNT asking, in both directions (the shim assumed present).
#[test]
fn the_live_tier_cause_is_scoped_to_the_account_that_holds_the_login() {
    let (live_user, live_password) = crate::fxcm_env_var_names(Environment::Live);
    let mut default_live =
        MountFixture::new(&[(live_user.as_str(), "U"), (live_password.as_str(), "P")]);
    let mut alt_live = MountFixture::new(&[]);
    alt_live.vars.insert(account_key(&live_user, &alt()), "U".to_string());
    alt_live.vars.insert(account_key(&live_password, &alt()), "P".to_string());
    alt_live.account = alt();
    assert_eq!(
        resolution_for(true, &default_live.inputs(true)),
        Resolution::Paper(PaperCause::LiveTierNotWired)
    );
    default_live.account = alt();
    assert_eq!(
        resolution_for(true, &default_live.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials),
        "ALT asks, and only the DEFAULT account holds a live login"
    );
    assert_eq!(
        resolution_for(true, &alt_live.inputs(true)),
        Resolution::Paper(PaperCause::LiveTierNotWired)
    );
    alt_live.account = AccountLabel::Default;
    assert_eq!(
        resolution_for(true, &alt_live.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials),
        "the DEFAULT account asks, and only ALT holds a live login"
    );
}

/// The stay-paper half at the MOUNT, with the shim assumed loadable and then not: a live-tier login
/// alone mounts PAPER and asks no reconcile factory, whichever way the shim answers — the cause and
/// the loudness changed, the outcome did not.
#[test]
fn a_live_login_alone_mounts_paper_and_asks_no_factory() {
    let (live_user, live_password) = crate::fxcm_env_var_names(Environment::Live);
    let fx = MountFixture::new(&[(live_user.as_str(), "U"), (live_password.as_str(), "P")]);
    let (tx, _rx) = vike_exec::event_channel(8);
    for sdk in [false, true] {
        let mut req = fx.request(true, "EURUSD", &tx);
        req.recon_enabled = true;
        let out = mount_with(req, sdk, |_, _| panic!("no demo login: no reconcile session"));
        assert!(matches!(out.exec, ExecOutcome::Paper), "sdk={sdk}");
        assert!(out.recon.is_none() && out.identity.is_none(), "sdk={sdk}");
    }
}

/// The account 2×2 — which account is asked for × which account's keys the store holds — through
/// the pure decision with the shim assumed present, the only way the credential half is reachable
/// on a box without the SDK.
#[test]
fn a_labelled_account_reads_only_its_own_keys() {
    assert_eq!(resolution_for(true, &keyed(&AccountLabel::Default).inputs(true)), ARMED);
    let mut alt_asks_default_keys = keyed(&AccountLabel::Default);
    alt_asks_default_keys.account = alt();
    assert_eq!(
        resolution_for(true, &alt_asks_default_keys.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials),
        "ALT must not arm off the default account's login"
    );
    assert_eq!(resolution_for(true, &keyed(&alt()).inputs(true)), ARMED);
    let mut default_asks_alt_keys = keyed(&alt());
    default_asks_alt_keys.account = AccountLabel::Default;
    assert_eq!(
        resolution_for(true, &default_asks_alt_keys.inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials)
    );
}

#[test]
fn with_no_credentials_the_mount_is_paper_and_offline() {
    let fx = MountFixture::new(&[]);
    let (tx, _rx) = vike_exec::event_channel(8);
    let out = FxcmVenueMount.mount(fx.request(true, "EURUSD", &tx));
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
    let mut req = fx.request(false, "EURUSD", &tx);
    // Reconciliation ON, as on a live-configured box, so the outcome is the one such a box gets.
    req.recon_enabled = true;
    let out = FxcmVenueMount.mount(req);
    if crate::sdk_available() {
        assert!(
            matches!(out.exec, ExecOutcome::Live(LiveExec { bound_tier: Tier::Demo, .. })),
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
    let fx = keyed(&AccountLabel::Default);
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut req = fx.request(false, "EURUSD", &tx);
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
        let mut req = fx.request(false, "EURUSD", &tx);
        req.recon_enabled = recon_enabled;
        let mut asked = None;
        let out = mount_with(req, true, |cfg, symbol| {
            asked = Some((cfg.user.clone(), symbol.to_string()));
            Some(Box::new(NoReports))
        });
        let ExecOutcome::Live(live) = out.exec else {
            panic!("a loadable shim and a DEMO login mount LIVE");
        };
        assert_eq!(live.bound_tier, Tier::Demo, "the credentials authenticate the DEMO tier");
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

// ── what the arm says about a LIVE tier it will not use ─────────────────────────────────────────
//
// Every account label below is unique to its test: the unused-beside-demo `warn!` is said once per
// process per `(venue, account)`, so two tests sharing a label would pass or fail by scheduling order.

/// A distinctive stand-in for a secret, so "no value was logged" is an assertion that can fail.
const SECRET: &str = "SECRET-VALUE-do-not-log";

/// The LIVE tier's `(user, password)` for `label`, composed by this crate's own naming function.
fn live_names(label: &AccountLabel) -> (String, String) {
    let (user, password) = crate::fxcm_env_var_names(Environment::Live);
    (account_key(&user, label), account_key(&password, label))
}

/// A complete LIVE login for `label` (every value is [`SECRET`]) and nothing else.
fn live_only(label: &AccountLabel) -> MountFixture {
    let (user, password) = live_names(label);
    let mut fx = MountFixture::new(&[]);
    fx.vars.insert(user, SECRET.to_string());
    fx.vars.insert(password, SECRET.to_string());
    fx.account = label.clone();
    fx
}

/// The DEMO login (the [`UNREACHABLE_LOGIN`] one, so a spawned session opens nothing on any box)
/// AND a complete live login, both for `label`.
fn both_tiers(label: &AccountLabel) -> MountFixture {
    let mut fx = live_only(label);
    for (k, v) in UNREACHABLE_KEYS {
        fx.vars.insert(account_key(k, label), (*v).to_string());
    }
    fx
}

/// A live login BESIDE the demo one used to mount the demo tier in silence. With a loadable shim it
/// still mounts the demo tier — bound at DEMO, nothing else moves — and now says, once, that the live
/// login is unused.
#[test]
fn a_live_login_beside_the_demo_one_is_named_unused_and_the_mount_is_unchanged() {
    let beside = AccountLabel::parse("BESIDE").expect("a legal label");
    let fx = both_tiers(&beside);
    let (tx, _rx) = vike_exec::event_channel(8);
    let (out, events) = captured(|| {
        mount_with(fx.request(true, "EURUSD", &tx), true, |_, _| Some(Box::new(NoReports)))
    });
    let ExecOutcome::Live(exec) = out.exec else {
        panic!("a loadable shim and a DEMO login mount")
    };
    assert_eq!(exec.bound_tier, Tier::Demo, "the live login must not move the bound tier");
    let unused = found_tier_events(&events);
    assert_eq!(unused.len(), 1, "exactly one line about the live login: {events:?}");
    let e = unused[0];
    assert_eq!(e.level, tracing::Level::WARN, "{e:?}");
    assert_eq!((e.field("venue"), e.field("account")), (Some("fxcm"), Some("BESIDE")), "{e:?}");
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

/// A live login with its password missing is a typo or a half-finished edit, and the loader calls it
/// absent — so it printed `NoCredentials` under the words of an empty store. The mount now names the
/// key it lacks (label-composed, names only) and still lands on paper, shim or no shim.
#[test]
fn a_half_written_live_login_names_the_missing_key_and_the_mount_is_still_paper() {
    let half = AccountLabel::parse("HALF").expect("a legal label");
    let (user, password) = live_names(&half);
    let mut fx = MountFixture::new(&[]);
    fx.account = half.clone();
    fx.vars.insert(user.clone(), SECRET.to_string());
    for sdk in [false, true] {
        let (tx, _rx) = vike_exec::event_channel(8);
        let (out, events) = captured(|| {
            mount_with(fx.request(true, "EURUSD", &tx), sdk, |_, _| {
                panic!("no demo login, so no reconcile session")
            })
        });
        assert!(matches!(out.exec, ExecOutcome::Paper), "sdk={sdk}");
        // The SAME store is read again for each shim answer, so the dedup of the unused-beside-demo
        // warning (which this case never reaches) cannot hide a repeat of THIS line.
        let said = found_tier_events(&events);
        assert_eq!(said.len(), 1, "sdk={sdk}: {events:?}");
        let e = said[0];
        assert_eq!(e.level, tracing::Level::ERROR, "{e:?}");
        assert_eq!((e.field("venue"), e.field("account")), (Some("fxcm"), Some("HALF")), "{e:?}");
        assert!(e.message.contains(&password), "names the missing key `{password}`: {}", e.message);
        assert!(!e.message.contains(&user), "names only what is MISSING: {}", e.message);
        assert!(!format!("{e:?}").contains(SECRET), "no credential VALUE: {e:?}");
    }
    assert_eq!(
        resolution_for(true, &fx.inputs(true)),
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
    let fx = live_only(&unmounted);
    let (_, events) = captured(|| FxcmVenueMount.report_unmounted_account(&fx.inputs(true)));
    let said = found_tier_events(&events);
    assert_eq!(said.len(), 1, "{events:?}");
    assert_eq!(said[0].level, tracing::Level::ERROR, "{events:?}");
    assert_eq!(said[0].field("account"), Some("UNMNT"), "{events:?}");

    let (_, events) = captured(|| {
        FxcmVenueMount.report_unmounted_account(&MountFixture::new(&[]).inputs(true));
    });
    assert!(events.is_empty(), "an empty store is the ordinary state and is silent: {events:?}");
}

/// **ONE RULE, TWO SURFACES.** The arming screen (`resolve`) and the log (`mount`) used to order
/// their questions differently — `resolve` asked the shim first, `mount` asked the credentials
/// first — so a box without the ForexConnect shim holding only a LIVE login printed `SdkAbsent` on
/// the screen and `LiveTierNotWired` in the log. Both were true, and an operator reading one then
/// the other had two answers to one question.
///
/// The rule both now follow: **a stand-alone LIVE login is named first** (it is a fact about the
/// STORE that no box can change), **then the shim** (a fact about the BOX), **then the demo login**.
/// Nothing in the table below changes what mounts — every cell that is paper stays paper, and the
/// one cell that arms is the cell that armed.
///
/// Each row is `(shim loaded, store)` → the cause `resolve` answers and what `mount` says. An
/// unconfigured store is SILENT in the log whatever the cause on the screen says: absent credentials
/// are the ordinary state, and the screen is where a missing shim is told.
#[test]
fn the_screen_and_the_log_follow_one_rule() {
    enum Says {
        /// Nothing about this venue at `error!`.
        Nothing,
        /// The shim refusal: an `error!` with a `reason`.
        TheShim,
        /// The stand-alone live login: an `error!` with `found_tier`.
        TheLiveLogin,
    }
    use Says::{Nothing, TheLiveLogin, TheShim};
    let paper = Resolution::Paper;
    let cells: [(bool, &str, Resolution, Says); 8] = [
        (false, "empty", paper(PaperCause::SdkAbsent), Nothing),
        (false, "demo", paper(PaperCause::SdkAbsent), TheShim),
        (false, "live", paper(PaperCause::LiveTierNotWired), TheLiveLogin),
        (false, "both", paper(PaperCause::SdkAbsent), TheShim),
        (true, "empty", paper(PaperCause::NoCredentials), Nothing),
        (true, "demo", ARMED, Nothing),
        (true, "live", paper(PaperCause::LiveTierNotWired), TheLiveLogin),
        (true, "both", ARMED, Nothing),
    ];
    for (i, (sdk, store, cause, says)) in cells.into_iter().enumerate() {
        // A label per row, so the unused-beside-demo warning's dedup cannot make one row's log
        // depend on another's.
        let label = AccountLabel::parse(&format!("AGREE{i}")).expect("a legal label");
        let fx = match store {
            "empty" => MountFixture::new(&[]),
            "demo" => keyed_unreachable(&label),
            "live" => live_only(&label),
            _ => both_tiers(&label),
        };
        let mut fx = fx;
        fx.account = label;
        assert_eq!(resolution_for(sdk, &fx.inputs(true)), cause, "shim={sdk} store={store}");
        let (tx, _rx) = vike_exec::event_channel(8);
        let (out, events) = captured(|| {
            mount_with(fx.request(true, "EURUSD", &tx), sdk, |_, _| Some(Box::new(NoReports)))
        });
        // The mount's outcome agrees with the screen's tier, cell by cell.
        assert_eq!(
            matches!(out.exec, ExecOutcome::Live(_)),
            matches!(cause, Resolution::Armed { .. }),
            "shim={sdk} store={store}: what mounts"
        );
        let errors: Vec<&CapturedEvent> =
            events.iter().filter(|e| e.level == tracing::Level::ERROR).collect();
        match says {
            Nothing => assert!(errors.is_empty(), "shim={sdk} store={store}: {events:?}"),
            TheShim => {
                assert_eq!(errors.len(), 1, "shim={sdk} store={store}: {events:?}");
                assert!(errors[0].field("reason").is_some(), "the shim refusal: {:?}", errors[0]);
                assert!(errors[0].field("found_tier").is_none(), "{:?}", errors[0]);
            }
            TheLiveLogin => {
                assert_eq!(errors.len(), 1, "shim={sdk} store={store}: {events:?}");
                assert_eq!(errors[0].field("found_tier"), Some("live"), "{:?}", errors[0]);
                assert!(errors[0].field("reason").is_none(), "no shim refusal: {:?}", errors[0]);
            }
        }
    }
}

/// The DEMO login ([`UNREACHABLE_LOGIN`], so nothing is opened) for `label`, and nothing else.
fn keyed_unreachable(label: &AccountLabel) -> MountFixture {
    let mut fx = MountFixture::new(&[]);
    for (k, v) in UNREACHABLE_KEYS {
        fx.vars.insert(account_key(k, label), (*v).to_string());
    }
    fx.account = label.clone();
    fx
}

/// On a box WITHOUT the shim a labelled account that holds the DEMO login is held back by the shim
/// (`SdkAbsent`), not by its keys — and still hears about its live login exactly as the default
/// account does: the hook asks the store, never the shim, and works out for itself that the demo
/// login loaded.
#[test]
fn an_account_held_back_by_the_shim_still_hears_about_an_unused_live_login() {
    let held = AccountLabel::parse("HELD").expect("a legal label");
    let fx = both_tiers(&held);
    let (_, events) = captured(|| FxcmVenueMount.report_unmounted_account(&fx.inputs(true)));
    let said = found_tier_events(&events);
    assert_eq!(said.len(), 1, "{events:?}");
    assert_eq!(said[0].level, tracing::Level::WARN, "the demo login loaded: {events:?}");
    assert_eq!(said[0].field("account"), Some("HELD"), "{events:?}");
    assert!(!format!("{events:?}").contains(SECRET), "no credential VALUE: {events:?}");
}
