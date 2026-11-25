use super::*;
use std::assert_matches;
use std::collections::BTreeMap;
use std::path::PathBuf;

use vike_bridge_core::transport::ErrorKind;
use vike_bridge_core::venue_adapter::{ArmingHeldBelowLive, ProcessPaths};
use vike_bridge_core::venue_adapter_fixture::{AdapterFixture, found_tier_events};
use vike_log::capture::captured;
use vike_secrets::account_credentials::CredentialRegistry;
use vike_secrets::{AccountKeys, AccountRow, AccountRows};

use crate::config::{BRIDGE_JAR_FILE, JFOREX_BRIDGE_JAR_ENV, JFOREX_HOME_SUBDIR, JFOREX_TOOL_DIR};
use crate::execution::fatal_kind;

fn row_at(id: i64, tier: AccountTier, book: Option<&str>, label: Option<&str>) -> AccountRow {
    AccountRow {
        id,
        venue: VENUE.to_string(),
        tier,
        label: label.map(str::to_string),
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

/// The owner's shape: two demo rows with two LABELS, and the books an operator wrote against them.
fn live_store() -> AccountRows {
    AccountRows::Known(vec![
        row_at(7, AccountTier::Demo, Some("3709890"), Some("SWISS")),
        row_at(8, AccountTier::Demo, Some("3716974"), Some("EU")),
    ])
}

/// A box with ONE unlabelled row at `tier` — the default account's.
fn default_store(tier: AccountTier) -> AccountRows {
    AccountRows::Known(vec![row_at(1, tier, None, None)])
}

fn label(text: &str) -> AccountLabel {
    AccountLabel::parse(text).expect("a legal label")
}

/// The two fields `load_dukascopy_config_from` requires of the account.
const LOGIN: &[(&str, &str)] = &[("LOGIN", "fake-login"), ("PASSWORD", "fake-pass")];

/// A box whose store the root READ (`accounts`), asking for `label`, whose account holds `fields`
/// as `vike-tradehub-core` hands them (`AdapterInputs::credentials`), with the release's addresses.
fn on_box(accounts: AccountRows, label: AccountLabel, fields: &[(&str, &str)]) -> AdapterFixture {
    let mut fx = AdapterFixture::new(fields);
    fx.accounts = AccountDirectory::from_rows(accounts, None);
    fx.account = label;
    fx.settings = vike_secrets::venue_setting::VenueSettings::default()
        .with_urls(vike_venues::venue_urls::VenueUrls::seed(VENUE));
    fx
}

/// The rows `vike-tradehub-core`'s tables carried for dukascopy, pinned — with the ONE deliberate text
/// change the spec's "Findings" 3 names: the clock reason no longer claims there is no arm.
#[test]
fn the_declaration_is_the_rows_the_tradehub_core_carried() {
    let d = VenueAdapterDukascopy.declaration();
    assert_eq!(VenueAdapterDukascopy.venue(), "dukascopy");
    assert!(d.addresses_accounts);
    assert_eq!(d.grid_source, GridSource::NoGrid);
    let exclusive = d.process_exclusive.expect("the ONE-sidecar rule is this venue's declaration");
    assert_eq!(exclusive.resource, "JForex sidecar");
    // Both texts in FULL: once this venue's rows leave `vike-tradehub-core`'s `BOOK_IDENTITY` and
    // `CLOCK_SOURCES`, this is the only thing that pins them.
    let BookIdentityRule::Undeterminable { why } = d.book_identity else {
        panic!("dukascopy's store names no book offline: {:?}", d.book_identity);
    };
    assert_eq!(
        why,
        "the store holds a JForex LOGIN, not an account number, so nothing in it names the book; \
         the venue answers at the sidecar's ready handshake instead, and \
         `account.venue_account_id` is where that answer is kept",
        "BOOK_IDENTITY's dukascopy row, byte for byte"
    );
    let ClockDeclaration::NotWired { reason, unmeasured_risk: None } = d.clock else {
        panic!("dukascopy declares no clock leg and no order-path risk");
    };
    assert_eq!(
        reason,
        "execution is a JForex Java sidecar over stdio with no REST API, and the sidecar starts \
         only at mount — after this step — so there is no server clock this preflight can read",
        "CLOCK_SOURCES' dukascopy row with its stale 'make_engine has no arm for it' replaced"
    );
}

/// The declared one-sidecar sentences are the two start refusals, naming the account and (the held-by-another
/// one) the holder; their words (NOT RUNNING, the shared platform cache, the second account's own project folder)
/// are the refusals' one text in `crate::refusals`.
#[test]
fn every_sidecar_refusal_says_it_stays_paper() {
    use crate::refusals::DukascopyStartRefusal;
    let exclusive = VenueAdapterDukascopy.declaration().process_exclusive.expect("declared");
    let held = DukascopyStartRefusal::SidecarHeldByAnother {
        label: "3716974".to_string(),
        holder: "3709890".to_string(),
    };
    assert_eq!((exclusive.held_by_another)("3716974", "3709890"), held.to_string());
    let claimed = DukascopyStartRefusal::SidecarAlreadyClaimed { label: "3716974".to_string() };
    assert_eq!((exclusive.already_claimed)("3716974"), claimed.to_string());
}

/// The arming-probe row: an account resolved to its row with no login is `NoCredentials`; with
/// one, a DEMO session held below `live` (a `Paper` tier reads as demo); a `live` account holding
/// its login is the NAMED `LiveTierNotWired`; and an account no row can be resolved for — the
/// default one included, on a box that read no store — is refused BY NAME (`AccountNotInStore`).
#[test]
fn resolve_is_the_arms_own_gate() {
    let armed = ArmingResolution::Armed {
        tier: AccountTier::Demo,
        held_below_live: Some(ArmingHeldBelowLive::DemoOnlyArm),
    };
    for tier in [AccountTier::Demo, AccountTier::Paper] {
        let empty = on_box(default_store(AccountTier::Demo), AccountLabel::Default, &[]);
        assert_eq!(
            VenueAdapterDukascopy.resolve(&empty.inputs(tier)),
            ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials)
        );
        let login = on_box(default_store(AccountTier::Demo), AccountLabel::Default, LOGIN);
        assert_eq!(VenueAdapterDukascopy.resolve(&login.inputs(tier)), armed, "{tier}");
    }

    let live = on_box(default_store(AccountTier::Live), AccountLabel::Default, LOGIN);
    assert_eq!(
        VenueAdapterDukascopy.resolve(&live.inputs(AccountTier::Live)),
        ArmingResolution::NotRunning(ArmingNotRunningCause::LiveTierNotWired)
    );
    let live_empty = on_box(default_store(AccountTier::Live), AccountLabel::Default, &[]);
    assert_eq!(
        VenueAdapterDukascopy.resolve(&live_empty.inputs(AccountTier::Live)),
        ArmingResolution::NotRunning(ArmingNotRunningCause::NoCredentials)
    );

    for asked in [AccountLabel::Default, label("EU")] {
        let mut unread = AdapterFixture::new(LOGIN);
        unread.account = asked;
        assert_eq!(
            VenueAdapterDukascopy.resolve(&unread.inputs(AccountTier::Demo)),
            ArmingResolution::NotRunning(ArmingNotRunningCause::AccountNotInStore),
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
            VenueAdapterDukascopy.resolve(&fx.inputs(tier)),
            ArmingResolution::Armed { tier: AccountTier::Live, .. }
        ));
    }
}

/// A labelled account resolves its OWN row and arms from its login; a label no row carries is
/// refused BY NAME, even with a login in hand.
#[test]
fn a_labelled_account_arms_from_its_own_row() {
    assert_matches!(
        VenueAdapterDukascopy
            .resolve(&on_box(live_store(), label("EU"), LOGIN).inputs(AccountTier::Demo)),
        ArmingResolution::Armed { tier: AccountTier::Demo, .. }
    );
    assert_eq!(
        VenueAdapterDukascopy
            .resolve(&on_box(live_store(), label("NOPE"), LOGIN).inputs(AccountTier::Demo)),
        ArmingResolution::NotRunning(ArmingNotRunningCause::AccountNotInStore)
    );
}

/// Absent credentials: paper, nothing to reconcile, no identity — and nothing started. Driven
/// through `start_with`, whose start double records whether it was reached.
#[test]
fn with_no_credentials_the_start_is_paper_and_offline() {
    let fx = on_box(default_store(AccountTier::Demo), AccountLabel::Default, &[]);
    let (tx, _rx) = vike_execution::event_channel(8);
    let mut req = fx.request(AccountTier::Demo, "EURUSD", &tx);
    req.reconcile_enabled = true;
    let mut started = false;
    let out = start_with(req, |_, _, _| {
        started = true;
        Err(DukascopyError::Unavailable(ErrorKind::Unknown))
    });
    assert!(!started, "a mount with no credentials must never reach the sidecar start");
    assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning));
    assert!(out.reconcile.is_none() && out.identity.is_none());
}

/// An account the store cannot resolve is refused by name and never reaches the sidecar START,
/// even with a login in hand — a refusal is a refusal, never a fallback onto another account. It
/// matters twice over: `vike-tradehub-core` asks a row whose probe answered paper to start WITHOUT taking
/// the one-sidecar claim, so a start from here would also sit outside the backstop. Driven through
/// `start_with`: the real start's jar check would turn a wrongly started start paper on a test box
/// too, so only a double can tell the two apart.
#[test]
fn an_account_the_store_cannot_identify_starts_paper_without_a_spawn() {
    let fx = on_box(live_store(), label("NOPE"), LOGIN);
    let (tx, _rx) = vike_execution::event_channel(8);
    let mut started = false;
    let (out, events) = captured(|| {
        start_with(fx.request(AccountTier::Demo, "EURUSD", &tx), |_, _, _| {
            started = true;
            Err(DukascopyError::Unavailable(ErrorKind::Unknown))
        })
    });
    assert!(!started, "an account the store cannot identify must never reach the sidecar start");
    assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning));
    assert!(out.reconcile.is_none() && out.identity.is_none());
    // The refusal line carries the `account` field, so a log search by account finds it (A-D4).
    let refusal = events
        .iter()
        .find(|e| e.level == tracing::Level::ERROR)
        .unwrap_or_else(|| panic!("the refusal is an error line: {events:?}"));
    assert_eq!(refusal.field("venue"), Some(VENUE));
    assert_eq!(refusal.field("account"), Some("NOPE"));
}

/// …and the other half: an account the store DOES resolve reaches the start with ITS login — the
/// credentials it was handed — and with its tools resolved from the two directories `vike-tradehub-core`
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
    fx.process = ProcessPaths {
        state_dir: Some(state.clone()),
        bin_dir: Some(bin.clone()),
        ..Default::default()
    };
    let (tx, _rx) = vike_execution::event_channel(8);
    let mut started = None;
    start_with(fx.request(AccountTier::Demo, "EURUSD", &tx), |config, tools, _| {
        started = Some((config.login, config.server, config.ledger_mount, tools.clone()));
        Err(DukascopyError::Unavailable(ErrorKind::Unknown))
    });
    let (login, jnlp, mount, tools) =
        started.expect("an identified account with its login reaches the start");
    assert_eq!(login, "fake-login-2", "the account's own login starts the sidecar");
    let demo_jnlp = fx.settings.urls().url(
        vike_venues::venue_urls::UrlType::Demo,
        vike_venues::venue_urls::UrlChannel::Login,
        vike_venues::venue_urls::UrlProtocol::Rest,
        "jnlp",
    );
    assert_eq!(Ok(jnlp.as_str()), demo_jnlp, "the demo login rest jnlp row is the JNLP");
    assert_eq!(
        mount.as_deref(),
        Some("EURUSD"),
        "the start's symbol is the engine's mount: it decides whether the ready balance is sent"
    );
    assert_eq!(tools.bridge_jar, bin.join(JFOREX_TOOL_DIR).join(BRIDGE_JAR_FILE));
    assert_eq!(tools.jforex_home, Some(state.join(JFOREX_HOME_SUBDIR)));
}

/// A start of the identified `EU` account whose sidecar answers the `fatal` `reason`, read as the real start reads
/// it (`crate::execution::fatal_kind`), and whether the start reached the sidecar.
fn start_whose_sidecar_said(reason: &str) -> (AdapterOutcome, bool) {
    let mut fx = on_box(live_store(), label("EU"), LOGIN);
    fx.account_id = Some(8);
    let (tx, _rx) = vike_execution::event_channel(8);
    let mut started = false;
    let out = start_with(fx.request(AccountTier::Demo, "EURUSD", &tx), |_, _, _| {
        started = true;
        Err(DukascopyError::Unavailable(fatal_kind(reason, None, None, None)))
    });
    (out, started)
}

/// **A sidecar that could not reach Dukascopy's servers is asked again** (owner, 2026-11-16: an FX venue comes up
/// by itself once the network is back): its `login failed:` in the JDK's words for a refused connect is
/// `Unreachable`, which the tradehub starts again, with nothing reconciled and nothing recorded.
#[test]
fn a_start_whose_sidecar_cannot_reach_dukascopy_is_unreachable() {
    let (out, started) = start_whose_sidecar_said("login failed: Connection refused");
    assert!(started, "the identified account reaches the sidecar start");
    assert!(matches!(out.exec, VenueAdapterExecutionOutcome::Unreachable));
    assert!(out.reconcile.is_none() && out.identity.is_none());
}

/// **A sidecar whose DNS lookup failed is asked again**: `java.net.UnknownHostException` says only the host it could
/// not resolve, so a `login failed:` that is a bare host name is a network failure and the start is `Unreachable`.
#[test]
fn a_start_whose_sidecar_could_not_resolve_the_host_is_unreachable() {
    let (out, started) = start_whose_sidecar_said("login failed: platform.dukascopy.com");
    assert!(started, "the identified account reaches the sidecar start");
    assert!(matches!(out.exec, VenueAdapterExecutionOutcome::Unreachable));
    assert!(out.reconcile.is_none() && out.identity.is_none());
}

/// **A login Dukascopy refused parks**: a wrong password only repeats on a later start, so the account is
/// `NotRunning`, which waits for the operator.
#[test]
fn a_start_whose_login_dukascopy_refused_is_not_running() {
    let (out, started) =
        start_whose_sidecar_said("login failed: Wrong login or password for user fake-login");
    assert!(started, "the identified account reaches the sidecar start");
    assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning));
    assert!(out.reconcile.is_none() && out.identity.is_none());
}

/// **A whole login with no usable JNLP still ARMS, and its start refuses by name without a spawn**:
/// the probe is the login's, and the start says which address row is missing. No stand-in JNLP.
#[test]
fn a_login_with_no_jnlp_row_arms_and_its_start_refuses_by_name() {
    let mut fx = on_box(default_store(AccountTier::Demo), AccountLabel::Default, LOGIN);
    fx.settings = vike_secrets::venue_setting::VenueSettings::default();
    assert_matches!(
        VenueAdapterDukascopy.resolve(&fx.inputs(AccountTier::Demo)),
        ArmingResolution::Armed { .. }
    );
    let (tx, _rx) = vike_execution::event_channel(8);
    let mut started = false;
    let (out, events) = captured(|| {
        start_with(fx.request(AccountTier::Demo, "EURUSD", &tx), |_, _, _| {
            started = true;
            Err(DukascopyError::Unavailable(ErrorKind::Unknown))
        })
    });
    assert!(!started, "no JNLP row: the sidecar is never started");
    assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning));
    assert!(
        events.iter().any(|e| e.level == tracing::Level::ERROR
            && e.message.contains("demo login rest product=jnlp")),
        "the start names the missing row: {events:?}"
    );
}

/// **A `live` account holding its login is NAMED, once, and its sidecar is never started** — the
/// arm has no live tier. `found_tier`, never `tier`, and no credential value on any line; a labelled
/// live account that is never started is spoken for in the same words, and a demo account says
/// nothing about a live tier.
#[test]
fn a_live_account_holding_its_login_is_named_once_and_never_started() {
    const SECRET: &str = "SECRET-VALUE-do-not-log";
    let live_eu = AccountRows::Known(vec![row_at(9, AccountTier::Live, None, Some("LIVEEU"))]);
    let fx = on_box(live_eu, label("LIVEEU"), &[("LOGIN", "l"), ("PASSWORD", SECRET)]);
    let (tx, _rx) = vike_execution::event_channel(8);
    let mut started = false;
    let (out, events) = captured(|| {
        start_with(fx.request(AccountTier::Live, "EURUSD", &tx), |_, _, _| {
            started = true;
            Err(DukascopyError::Unavailable(ErrorKind::Unknown))
        })
    });
    assert!(!started, "a live account's sidecar must never be started");
    assert!(matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning));
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
        // secret-absent: the planted credential value never reaches a line
        events.iter().all(|e| !format!("{e:?}").contains(SECRET)),
        "no credential VALUE may reach a line: {events:?}"
    );

    let (_, events) = captured(|| {
        VenueAdapterDukascopy.report_not_running_account(&fx.inputs(AccountTier::Live))
    });
    assert_eq!(found_tier_events(&events).len(), 1, "the same words for an unmounted account");

    let demo = on_box(live_store(), label("EU"), LOGIN);
    let (_, events) = captured(|| {
        VenueAdapterDukascopy.report_not_running_account(&demo.inputs(AccountTier::Demo))
    });
    assert!(events.is_empty(), "a demo account has no live tier to name: {events:?}");
}

/// A start that cannot find its jar is PAPER and reconciles nothing, even with reconciliation on
/// — `crate::execution`'s `spawn` refuses before it forks, so this stays JVM-free. The jar's path is a
/// DEPLOYMENT name (`AdapterInputs::secrets`), never an account field.
#[test]
fn a_start_that_cannot_find_its_jar_is_paper_and_reconciles_nothing() {
    let fx = on_box(default_store(AccountTier::Demo), AccountLabel::Default, LOGIN)
        .with_vars(&[(JFOREX_BRIDGE_JAR_ENV, "/nonexistent/vike/jforex-bridge.jar")]);
    let (tx, _rx) = vike_execution::event_channel(8);
    let mut req = fx.request(AccountTier::Demo, "EURUSD", &tx);
    req.reconcile_enabled = true;
    let out = VenueAdapterDukascopy.start(req);
    assert!(
        matches!(out.exec, VenueAdapterExecutionOutcome::NotRunning),
        "a sidecar that never started is paper"
    );
    assert!(out.reconcile.is_none(), "…and no recon client is handed out");
}

/// The confirmation parks into the state directory the start is HANDED (`AdapterInputs::process`)
/// — this crate reads no process-global path — under the account's LABEL + TIER; `None` parks
/// nothing and does not panic, and an UNLABELLED account parks nothing either.
#[test]
fn a_confirmation_parks_into_the_state_dir_it_is_handed() {
    let account_row = DukascopyAccount {
        row: 7,
        label: Some("SWISS".to_string()),
        tier: AccountTier::Demo,
        book: Some("3709890".to_string()),
    };
    record_confirmation(&account_row, "3709890", None);
    let dir = tempfile::tempdir().expect("a scratch state directory");
    record_confirmation(&account_row, "3709890", Some(dir.path()));
    let parked = vike_secrets::account_confirmation::read(Some(dir.path())).expect("readable");
    assert_eq!(parked.len(), 1, "exactly one confirmation parked");
    assert_eq!(parked[0].venue, "dukascopy");
    assert_eq!(parked[0].key_prefix, "", "the labelled shape");
    assert_eq!(parked[0].label.as_deref(), Some("SWISS"));
    assert_eq!(parked[0].tier, Some(AccountTier::Demo));
    assert_eq!(parked[0].handshake_account_id, "3709890");

    let unlabelled = tempfile::tempdir().expect("a scratch state directory");
    record_confirmation(
        &DukascopyAccount { label: None, ..account_row },
        "3709890",
        Some(unlabelled.path()),
    );
    let parked = vike_secrets::account_confirmation::read(Some(unlabelled.path())).expect("reads");
    assert!(parked.is_empty(), "an unlabelled account has no address to park under: {parked:?}");
}

/// MOVED from `vike-tradehub-core`'s dukascopy tests. A store failure is reported as a STORE FAILURE — for
/// BOTH halves of the read (the account rows and the credentials), for every account, and never as
/// a missing row or a missing login.
#[test]
fn a_store_that_will_not_open_is_not_reported_as_a_bad_row() {
    let unreadable = AccountDirectory::read(
        Err::<AccountRows, _>("disk I/O error"),
        Err::<Option<BTreeMap<i64, AccountKeys>>, _>("disk I/O error"),
        Err::<CredentialRegistry, _>("disk I/O error"),
    );
    for asked in [label("EU"), AccountLabel::Default] {
        let err = resolve_in(&asked, AccountTier::Demo, &unreadable).expect_err("refused");
        let DukascopyRefusal::StoreUnreadable { error, .. } = &err else { panic!("{err}") };
        assert!(error.contains("disk I/O error"), "the store's own words are carried: {err}");
    }

    // ⚠ THE HALF THAT WOULD BE SWALLOWED: rows fine, the CREDENTIALS unreadable — the account's
    // login is then absent, and that must not read as "no login".
    let credentials_failed = AccountDirectory::read(
        Ok::<_, &str>(live_store()),
        Ok::<Option<BTreeMap<i64, AccountKeys>>, &str>(None),
        Err::<CredentialRegistry, _>("database is locked"),
    );
    let err =
        resolve_in(&label("EU"), AccountTier::Demo, &credentials_failed).expect_err("refused");
    let DukascopyRefusal::StoreUnreadable { what, error, .. } = &err else {
        panic!("a credential-read failure must be a store failure, not a missing login: {err}")
    };
    assert!(error.contains("database is locked"), "{err}");
    assert_eq!(
        *what, "the account's credentials — the LOGIN and PASSWORD the sidecar signs in with",
        "names WHICH read failed"
    );
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
/// same entry point the start uses.
#[test]
fn a_read_directory_resolves_each_account_through_the_start_entry_point() {
    let dir = AccountDirectory::from_rows(live_store(), None);
    assert_eq!(resolve_in(&label("EU"), AccountTier::Demo, &dir).expect("row 8").row, 8);
    assert_eq!(resolve_in(&label("SWISS"), AccountTier::Demo, &dir).expect("row 7").row, 7);
    assert_matches!(
        resolve_in(&AccountLabel::Default, AccountTier::Demo, &dir),
        Err(DukascopyRefusal::NoSuchAccount { .. }),
        "two labelled rows leave no unlabelled default account"
    );
}
