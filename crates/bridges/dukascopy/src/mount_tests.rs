use super::*;
use std::assert_matches;
use std::collections::BTreeMap;
use std::path::PathBuf;

use vike_bridge_core::venue_mount::ProcessFacts;
use vike_bridge_core::venue_mount_fixture::{MountFixture, found_tier_events};
use vike_log::capture::captured;
use vike_secrets::account_credentials::CredentialBook;
use vike_secrets::{Account, AccountKeys, Accounts};

use crate::config::{BRIDGE_JAR_FILE, JFOREX_BRIDGE_JAR_ENV, JFOREX_HOME_SUBDIR, JFOREX_TOOL_DIR};

fn row_at(id: i64, tier: AccountTier, book: Option<&str>, label: Option<&str>) -> Account {
    Account {
        id,
        venue: VENUE.to_string(),
        tier,
        label: label.map(str::to_string),
        venue_account_id: book.map(str::to_string),
        parent_id: None,
        active: true,
        last_verified_at: None,
        max_exposure: None,
    }
}

/// The owner's shape: two demo rows with two LABELS, and the books an operator wrote against them.
fn live_store() -> Accounts {
    Accounts::Known(vec![
        row_at(7, AccountTier::Demo, Some("3709890"), Some("SWISS")),
        row_at(8, AccountTier::Demo, Some("3716974"), Some("EU")),
    ])
}

/// A box with ONE unlabelled row at `tier` — the default account's.
fn default_store(tier: AccountTier) -> Accounts {
    Accounts::Known(vec![row_at(1, tier, None, None)])
}

fn label(text: &str) -> AccountLabel {
    AccountLabel::parse(text).expect("a legal label")
}

/// The two fields `load_dukascopy_config_from` requires of the account.
const LOGIN: &[(&str, &str)] = &[("LOGIN", "fake-login"), ("PASSWORD", "fake-pass")];

/// A box whose store the root READ (`accounts`), asking for `label`, whose account holds `fields`
/// as `vike-mount` hands them (`MountInputs::credentials`).
fn on_box(accounts: Accounts, label: AccountLabel, fields: &[(&str, &str)]) -> MountFixture {
    let mut fx = MountFixture::new(fields);
    fx.accounts = AccountDirectory::from_rows(accounts, None);
    fx.account = label;
    fx
}

/// The rows `vike-mount`'s tables carried for dukascopy, pinned — with the ONE deliberate text
/// change the spec's "Findings" 3 names: the clock reason no longer claims there is no arm.
#[test]
fn the_declaration_is_the_rows_the_mount_crate_carried() {
    let d = DukascopyVenueMount.declaration();
    assert_eq!(DukascopyVenueMount.venue(), "dukascopy");
    assert!(d.addresses_accounts);
    assert!(!d.takes_recon_trigger);
    assert_eq!(d.grid_source, DeclaredGridSource::NoGrid);
    let exclusive = d.process_exclusive.expect("the ONE-sidecar rule is this venue's declaration");
    assert_eq!(exclusive.resource, "JForex sidecar");
    // Both texts in FULL: once this venue's rows leave `vike-mount`'s `BOOK_IDENTITY` and
    // `CLOCK_SOURCES`, this is the only thing that pins them.
    let BookIdentity::Undeterminable { why } = d.book_identity else {
        panic!("dukascopy's store names no book offline: {:?}", d.book_identity);
    };
    assert_eq!(
        why,
        "the store holds a JForex LOGIN, not an account number, so nothing in it names the book; \
         the venue answers at the sidecar's ready handshake instead, and \
         `account.venue_account_id` is where that answer is kept",
        "BOOK_IDENTITY's dukascopy row, byte for byte"
    );
    let ClockDecl::NotWired { reason, unmeasured_risk: None } = d.clock else {
        panic!("dukascopy declares no clock leg and no order-path risk");
    };
    assert_eq!(
        reason,
        "execution is a JForex Java sidecar over stdio with no REST API, and the sidecar starts \
         only at mount — after this step — so there is no server clock this preflight can read",
        "CLOCK_SOURCES' dukascopy row with its stale 'make_engine has no arm for it' replaced"
    );
}

/// Every refusal renders a sentence that says PAPER, names the account, the hazard (one shared
/// platform cache) and the way out (the second account's own project folder), and the
/// held-by-another one names the holder too. Moved with the texts from `vike-mount`'s dukascopy
/// tests, including the two assertions Task 1 folded into this test.
#[test]
fn every_sidecar_refusal_says_it_stays_paper() {
    let exclusive = DukascopyVenueMount.declaration().process_exclusive.expect("declared");
    let held = (exclusive.held_by_another)("3716974", "3709890");
    let claimed = (exclusive.already_claimed)("3716974");
    for text in [&held, &claimed] {
        assert!(text.contains("PAPER"), "{text}");
        assert!(text.contains("3716974"), "the refusal must name the account: {text}");
        assert!(text.len() > 60, "{text}");
        assert!(text.contains("platform cache"), "the hazard: {text}");
        assert!(text.contains("own project folder"), "the way out: {text}");
    }
    assert!(held.contains("`3709890` holds it"), "…and the holder: {held}");
}

/// The arming-probe row: an account resolved to its row with no login is `NoCredentials`; with
/// one, a DEMO session held below `live` (a `Paper` tier reads as demo); a `live` account holding
/// its login is the NAMED `LiveTierNotWired`; and an account no row can be resolved for — the
/// default one included, on a box that read no store — is refused BY NAME (`AccountNotInStore`).
#[test]
fn resolve_is_the_arms_own_gate() {
    let armed = Resolution::Armed {
        tier: AccountTier::Demo,
        held_below_live: Some(HeldBelowLive::DemoOnlyArm),
    };
    for tier in [AccountTier::Demo, AccountTier::Paper] {
        let empty = on_box(default_store(AccountTier::Demo), AccountLabel::Default, &[]);
        assert_eq!(
            DukascopyVenueMount.resolve(&empty.inputs(tier)),
            Resolution::Paper(PaperCause::NoCredentials)
        );
        let login = on_box(default_store(AccountTier::Demo), AccountLabel::Default, LOGIN);
        assert_eq!(DukascopyVenueMount.resolve(&login.inputs(tier)), armed, "{tier}");
    }

    let live = on_box(default_store(AccountTier::Live), AccountLabel::Default, LOGIN);
    assert_eq!(
        DukascopyVenueMount.resolve(&live.inputs(AccountTier::Live)),
        Resolution::Paper(PaperCause::LiveTierNotWired)
    );
    let live_empty = on_box(default_store(AccountTier::Live), AccountLabel::Default, &[]);
    assert_eq!(
        DukascopyVenueMount.resolve(&live_empty.inputs(AccountTier::Live)),
        Resolution::Paper(PaperCause::NoCredentials)
    );

    for asked in [AccountLabel::Default, label("EU")] {
        let mut unread = MountFixture::new(LOGIN);
        unread.account = asked;
        assert_eq!(
            DukascopyVenueMount.resolve(&unread.inputs(AccountTier::Demo)),
            Resolution::Paper(PaperCause::AccountNotInStore),
            "an unread store cannot say which row an account is — refused, not coerced"
        );
    }
}

/// Review Focus 2 at this venue: no account tier makes this arm resolve `Live` — it has no live
/// tier.
#[test]
fn a_ceiling_below_live_never_resolves_live() {
    for tier in [AccountTier::Paper, AccountTier::Demo, AccountTier::Live] {
        let fx = on_box(default_store(row_tier(tier)), AccountLabel::Default, LOGIN);
        assert!(!matches!(
            DukascopyVenueMount.resolve(&fx.inputs(tier)),
            Resolution::Armed { tier: AccountTier::Live, .. }
        ));
    }
}

/// A labelled account resolves its OWN row and arms from its login; a label no row carries is
/// refused BY NAME, even with a login in hand.
#[test]
fn a_labelled_account_arms_from_its_own_row() {
    assert_matches!(
        DukascopyVenueMount
            .resolve(&on_box(live_store(), label("EU"), LOGIN).inputs(AccountTier::Demo)),
        Resolution::Armed { tier: AccountTier::Demo, .. }
    );
    assert_eq!(
        DukascopyVenueMount
            .resolve(&on_box(live_store(), label("NOPE"), LOGIN).inputs(AccountTier::Demo)),
        Resolution::Paper(PaperCause::AccountNotInStore)
    );
}

/// Absent credentials: paper, nothing to reconcile, no identity — and nothing started. Driven
/// through `mount_with`, whose start double records whether it was reached.
#[test]
fn with_no_credentials_the_mount_is_paper_and_offline() {
    let fx = on_box(default_store(AccountTier::Demo), AccountLabel::Default, &[]);
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut req = fx.request(AccountTier::Demo, "EURUSD", &tx);
    req.recon_enabled = true;
    let mut started = false;
    let out = mount_with(req, |_, _, _| {
        started = true;
        Err(DukascopyError::Unavailable)
    });
    assert!(!started, "a mount with no credentials must never reach the sidecar start");
    assert!(matches!(out.exec, ExecOutcome::Paper));
    assert!(out.recon.is_none() && out.identity.is_none());
}

/// An account the store cannot resolve is refused by name and never reaches the sidecar START,
/// even with a login in hand — a refusal is a refusal, never a fallback onto another account. It
/// matters twice over: `vike-mount` asks a row whose probe answered paper to mount WITHOUT taking
/// the one-sidecar claim, so a start from here would also sit outside the backstop. Driven through
/// `mount_with`: the real start's jar check would turn a wrongly started mount paper on a test box
/// too, so only a double can tell the two apart.
#[test]
fn an_account_the_store_cannot_identify_mounts_paper_without_a_spawn() {
    let fx = on_box(live_store(), label("NOPE"), LOGIN);
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut started = false;
    let out = mount_with(fx.request(AccountTier::Demo, "EURUSD", &tx), |_, _, _| {
        started = true;
        Err(DukascopyError::Unavailable)
    });
    assert!(!started, "an account the store cannot identify must never reach the sidecar start");
    assert!(matches!(out.exec, ExecOutcome::Paper));
    assert!(out.recon.is_none() && out.identity.is_none());
}

/// …and the other half: an account the store DOES resolve reaches the start with ITS login — the
/// credentials it was handed — and with its tools resolved from the two directories `vike-mount`
/// handed it, each in its own place: the jar under the BIN directory and the JVM's home under the
/// STATE directory. Swapped, the jar would be looked for under `settings/state` and the home would
/// land under `bin/`.
#[test]
fn an_identified_account_starts_with_its_own_login_and_the_handed_directories() {
    const EU_LOGIN: &[(&str, &str)] = &[("LOGIN", "fake-login-2"), ("PASSWORD", "fake-pass-2")];
    let mut fx = on_box(live_store(), label("EU"), EU_LOGIN);
    fx.account_id = Some(8);
    let bin = PathBuf::from("/nonexistent/vike/project/bin");
    let state = PathBuf::from("/nonexistent/vike/project/settings/state");
    fx.process = ProcessFacts {
        state_dir: Some(state.clone()),
        bin_dir: Some(bin.clone()),
        ..Default::default()
    };
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut started = None;
    mount_with(fx.request(AccountTier::Demo, "EURUSD", &tx), |cfg, tools, _| {
        started = Some((cfg.login, tools.clone()));
        Err(DukascopyError::Unavailable)
    });
    let (login, tools) = started.expect("an identified account with its login reaches the start");
    assert_eq!(login, "fake-login-2", "the account's own login starts the sidecar");
    assert_eq!(tools.bridge_jar, bin.join(JFOREX_TOOL_DIR).join(BRIDGE_JAR_FILE));
    assert_eq!(tools.jforex_home, Some(state.join(JFOREX_HOME_SUBDIR)));
}

/// **A `live` account holding its login is NAMED, once, and its sidecar is never started** — the
/// arm has no live tier. `found_tier`, never `tier`, and no credential value on any line; a labelled
/// live account that is never mounted is spoken for in the same words, and a demo account says
/// nothing about a live tier.
#[test]
fn a_live_account_holding_its_login_is_named_once_and_never_started() {
    const SECRET: &str = "SECRET-VALUE-do-not-log";
    let live_eu = Accounts::Known(vec![row_at(9, AccountTier::Live, None, Some("LIVEEU"))]);
    let fx = on_box(live_eu, label("LIVEEU"), &[("LOGIN", "l"), ("PASSWORD", SECRET)]);
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut started = false;
    let (out, events) = captured(|| {
        mount_with(fx.request(AccountTier::Live, "EURUSD", &tx), |_, _, _| {
            started = true;
            Err(DukascopyError::Unavailable)
        })
    });
    assert!(!started, "a live account's sidecar must never be started");
    assert!(matches!(out.exec, ExecOutcome::Paper));
    let said = found_tier_events(&events);
    assert_eq!(said.len(), 1, "exactly one line about the live account: {events:?}");
    let e = said[0];
    assert_eq!(e.level, tracing::Level::ERROR, "{e:?}");
    assert_eq!(
        (e.field("venue"), e.field("account")),
        (Some("dukascopy"), Some("LIVEEU")),
        "{e:?}"
    );
    assert_eq!(e.field("found_tier"), Some("live"), "{e:?}");
    assert!(e.field("tier").is_none(), "a diagnostic carries `found_tier`, never `tier`: {e:?}");
    assert!(
        events.iter().all(|e| !format!("{e:?}").contains(SECRET)),
        "no credential VALUE may reach a line: {events:?}"
    );

    let (_, events) =
        captured(|| DukascopyVenueMount.report_unmounted_account(&fx.inputs(AccountTier::Live)));
    assert_eq!(found_tier_events(&events).len(), 1, "the same words for an unmounted account");

    let demo = on_box(live_store(), label("EU"), LOGIN);
    let (_, events) =
        captured(|| DukascopyVenueMount.report_unmounted_account(&demo.inputs(AccountTier::Demo)));
    assert!(events.is_empty(), "a demo account has no live tier to name: {events:?}");
}

/// A start that cannot find its jar is PAPER and reconciles nothing, even with reconciliation on
/// — `crate::exec`'s `spawn` refuses before it forks, so this stays JVM-free. The jar's path is a
/// DEPLOYMENT name (`MountInputs::secrets`), never an account field.
#[test]
fn a_start_that_cannot_find_its_jar_is_paper_and_reconciles_nothing() {
    let fx = on_box(default_store(AccountTier::Demo), AccountLabel::Default, LOGIN)
        .with_vars(&[(JFOREX_BRIDGE_JAR_ENV, "/nonexistent/vike/jforex-bridge.jar")]);
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut req = fx.request(AccountTier::Demo, "EURUSD", &tx);
    req.recon_enabled = true;
    let out = DukascopyVenueMount.mount(req);
    assert!(matches!(out.exec, ExecOutcome::Paper), "a sidecar that never started is paper");
    assert!(out.recon.is_none(), "…and a recon client derives from a RUNNING sidecar only");
}

/// The confirmation parks into the state directory the mount is HANDED (`MountInputs::process`)
/// — this crate reads no process-global path — under the account's LABEL + TIER; `None` parks
/// nothing and does not panic, and an UNLABELLED account parks nothing either.
#[test]
fn a_confirmation_parks_into_the_state_dir_it_is_handed() {
    let mount = DukascopyMount {
        row: 7,
        label: Some("SWISS".to_string()),
        tier: AccountTier::Demo,
        book: Some("3709890".to_string()),
    };
    record_confirmation(&mount, "3709890", None);
    let dir = tempfile::tempdir().expect("a scratch state directory");
    record_confirmation(&mount, "3709890", Some(dir.path()));
    let parked = vike_secrets::account_confirmation::read(Some(dir.path())).expect("readable");
    assert_eq!(parked.len(), 1, "exactly one confirmation parked");
    assert_eq!(parked[0].venue, "dukascopy");
    assert_eq!(parked[0].key_prefix, "", "the labelled shape");
    assert_eq!(parked[0].label.as_deref(), Some("SWISS"));
    assert_eq!(parked[0].tier, Some(AccountTier::Demo));
    assert_eq!(parked[0].handshake_account_id, "3709890");

    let unlabelled = tempfile::tempdir().expect("a scratch state directory");
    record_confirmation(
        &DukascopyMount { label: None, ..mount },
        "3709890",
        Some(unlabelled.path()),
    );
    let parked = vike_secrets::account_confirmation::read(Some(unlabelled.path())).expect("reads");
    assert!(parked.is_empty(), "an unlabelled account has no address to park under: {parked:?}");
}

/// MOVED from `vike-mount`'s dukascopy tests. A store failure is reported as a STORE FAILURE — for
/// BOTH halves of the read (the account rows and the credentials), for every account, and never as
/// a missing row or a missing login.
#[test]
fn a_store_that_will_not_open_is_not_reported_as_a_bad_row() {
    let unreadable = AccountDirectory::read(
        Err::<Accounts, _>("disk I/O error"),
        Err::<Option<BTreeMap<i64, AccountKeys>>, _>("disk I/O error"),
        Err::<CredentialBook, _>("disk I/O error"),
    );
    for asked in [label("EU"), AccountLabel::Default] {
        let err = resolve_in(&asked, AccountTier::Demo, &unreadable).expect_err("refused");
        assert_matches!(err, DukascopyRefusal::StoreUnreadable { .. }, "{err}");
        assert!(err.to_string().contains("disk I/O error"), "{err}");
    }

    // ⚠ THE HALF THAT WOULD BE SWALLOWED: rows fine, the CREDENTIALS unreadable — the account's
    // login is then absent, and that must not read as "no login".
    let creds_failed = AccountDirectory::read(
        Ok::<_, &str>(live_store()),
        Ok::<Option<BTreeMap<i64, AccountKeys>>, &str>(None),
        Err::<CredentialBook, _>("database is locked"),
    );
    let err = resolve_in(&label("EU"), AccountTier::Demo, &creds_failed).expect_err("refused");
    assert_matches!(
        err,
        DukascopyRefusal::StoreUnreadable { .. },
        "a credential-read failure must be a store failure, not a missing login: {err}"
    );
    let text = err.to_string();
    assert!(text.contains("database is locked"), "{text}");
    assert!(text.contains("STORE FAILURE"), "{text}");
    assert!(text.contains("credentials"), "names WHICH read failed: {text}");
}

/// MOVED. A process that read NO store behaves exactly like a `Backend::Absent` box: every account
/// is refused — the default one included, which resolves through its row like any other.
#[test]
fn an_unread_store_refuses_every_account() {
    let unread = AccountDirectory::unread();
    for asked in [AccountLabel::Default, label("EU")] {
        let err = resolve_in(&asked, AccountTier::Demo, &unread).expect_err("refused");
        assert_matches!(err, DukascopyRefusal::NoAccountTable { .. }, "{err}");
    }
}

/// MOVED. …and a directory a caller DID read resolves each account to its own row, through the
/// same entry point the mount uses.
#[test]
fn a_read_directory_resolves_each_account_through_the_mount_entry_point() {
    let dir = AccountDirectory::from_rows(live_store(), None);
    assert_eq!(resolve_in(&label("EU"), AccountTier::Demo, &dir).expect("row 8").row, 8);
    assert_eq!(resolve_in(&label("SWISS"), AccountTier::Demo, &dir).expect("row 7").row, 7);
    assert_matches!(
        resolve_in(&AccountLabel::Default, AccountTier::Demo, &dir),
        Err(DukascopyRefusal::NoSuchAccount { .. }),
        "two labelled rows leave no unlabelled default account"
    );
}
