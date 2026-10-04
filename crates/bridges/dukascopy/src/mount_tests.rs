use super::*;
use std::collections::BTreeMap;
use std::path::PathBuf;

use vike_bridge_core::credentials::{Account, AccountKeys, Accounts};
use vike_bridge_core::venue_mount::ProcessFacts;
use vike_bridge_core::venue_mount_fixture::MountFixture;

use crate::config::{
    BRIDGE_JAR_FILE, DukascopyAccount, JFOREX_BRIDGE_JAR_ENV, JFOREX_HOME_SUBDIR, JFOREX_TOOL_DIR,
};

fn row(id: i64, book: Option<&str>, label: Option<&str>, active: bool) -> Account {
    Account {
        id,
        venue: VENUE.to_string(),
        tier: "demo".to_string(),
        label: label.map(str::to_string),
        venue_account_id: book.map(str::to_string),
        parent_id: None,
        active,
        last_verified_at: None,
        armed: false,
    }
}

const SWISS_KEYS: &str = DukascopyAccount::Demo1.key_prefix();
const EU_KEYS: &str = DukascopyAccount::Demo2.key_prefix();

fn keys(rows: &[(i64, &[&str])]) -> BTreeMap<i64, AccountKeys> {
    rows.iter()
        .map(|(id, prefixes)| {
            (
                *id,
                AccountKeys {
                    prefixes: prefixes.iter().map(|p| (*p).to_string()).collect(),
                    names: Vec::new(),
                },
            )
        })
        .collect()
}

/// The live shape: two unlabelled demo rows, distinguishable only by their key prefixes, with
/// the books an operator wrote against them.
fn live_store() -> (Accounts, BTreeMap<i64, AccountKeys>) {
    (
        Accounts::Known(vec![
            row(7, Some("3709890"), None, true),
            row(8, Some("3716974"), None, true),
        ]),
        keys(&[(7, &[SWISS_KEYS]), (8, &[EU_KEYS])]),
    )
}

fn label(text: &str) -> AccountLabel {
    AccountLabel::parse(text).expect("a legal label")
}

/// Both halves `load_dukascopy_config_from` requires, for the Swiss (DEMO1) key family…
const SWISS_LOGIN: &[(&str, &str)] =
    &[("DUKASCOPY_DEMO1_LOGIN", "fake-login-1"), ("DUKASCOPY_DEMO1_PASSWORD", "fake-pass-1")];
/// …and for the EU (DEMO2) one.
const EU_LOGIN: &[(&str, &str)] =
    &[("DUKASCOPY_DEMO2_LOGIN", "fake-login-2"), ("DUKASCOPY_DEMO2_PASSWORD", "fake-pass-2")];

/// A box whose store the root READ — both rows, both key families — asking for `label`.
fn two_account_box(label: AccountLabel, pairs: &[(&str, &str)]) -> MountFixture {
    let (accounts, keys) = live_store();
    let mut fx = MountFixture::new(pairs);
    fx.accounts = AccountDirectory::from_rows(accounts, Some(keys));
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

/// The arming-probe row as it was: a labelled account on a box that read no store is refused BY
/// NAME (`AccountNotInStore`); a resolved account with no login is `NoCredentials`; with one, a
/// DEMO session, held below `live` because no live tier is wired.
#[test]
fn resolve_is_the_arms_own_gate() {
    let armed =
        Resolution::Armed { tier: Tier::Demo, held_below_live: Some(HeldBelowLive::DemoOnlyArm) };
    for live in [false, true] {
        assert_eq!(
            DukascopyVenueMount.resolve(&MountFixture::new(&[]).inputs(live)),
            Resolution::Paper(PaperCause::NoCredentials),
            "the DEFAULT account resolves without the store, and has no login here"
        );
        let swiss = MountFixture::new(SWISS_LOGIN);
        assert_eq!(DukascopyVenueMount.resolve(&swiss.inputs(live)), armed);
        let mut unknown = MountFixture::new(SWISS_LOGIN);
        unknown.account = label("3716974");
        assert_eq!(
            DukascopyVenueMount.resolve(&unknown.inputs(live)),
            Resolution::Paper(PaperCause::AccountNotInStore),
            "an unread store cannot say which broker a labelled account is — refused, not coerced"
        );
    }
}

/// Review Focus 2 at this venue: no ceiling makes this arm resolve `Live` — it has no live tier.
#[test]
fn a_ceiling_below_live_never_resolves_live() {
    let fx = two_account_box(AccountLabel::Default, SWISS_LOGIN);
    for live in [false, true] {
        assert!(!matches!(
            DukascopyVenueMount.resolve(&fx.inputs(live)),
            Resolution::Armed { tier: Tier::Live, .. }
        ));
    }
}

/// A labelled account arms only from the key family ITS row owns: the EU book's row owns DEMO2,
/// so the Swiss login alone never arms it, and the EU login does.
#[test]
fn a_labelled_account_arms_only_from_its_own_rows_key_family() {
    let eu = label("3716974");
    assert_eq!(
        DukascopyVenueMount.resolve(&two_account_box(eu.clone(), SWISS_LOGIN).inputs(true)),
        Resolution::Paper(PaperCause::NoCredentials),
        "the EU account must not arm off the Swiss bank's login"
    );
    assert!(matches!(
        DukascopyVenueMount.resolve(&two_account_box(eu, EU_LOGIN).inputs(true)),
        Resolution::Armed { tier: Tier::Demo, .. }
    ));
}

/// Absent credentials: paper, nothing to reconcile, no identity — and nothing started. Driven
/// through `mount_with`, whose start double records whether it was reached.
#[test]
fn with_no_credentials_the_mount_is_paper_and_offline() {
    let fx = MountFixture::new(&[]);
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut req = fx.request(true, "EURUSD", &tx);
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

/// An account the store cannot identify is refused by name and never reaches the sidecar START,
/// even with the Swiss bank's login present — a refusal is a refusal, never a fallback onto the
/// Swiss bank. It matters twice over: `vike-mount` asks a row whose probe answered paper to mount
/// WITHOUT taking the one-sidecar claim, so a start from here would also sit outside the
/// backstop. Driven through `mount_with`: the real start's jar check would turn a wrongly started
/// mount paper on a test box too, so only a double can tell the two apart.
#[test]
fn an_account_the_store_cannot_identify_mounts_paper_without_a_spawn() {
    let mut fx = MountFixture::new(SWISS_LOGIN);
    fx.account = label("9999999");
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut started = false;
    let out = mount_with(fx.request(true, "EURUSD", &tx), |_, _, _| {
        started = true;
        Err(DukascopyError::Unavailable)
    });
    assert!(!started, "an account the store cannot identify must never reach the sidecar start");
    assert!(matches!(out.exec, ExecOutcome::Paper));
    assert!(out.recon.is_none() && out.identity.is_none());
}

/// …and the other half: an account the store DOES identify reaches the start with ITS row's
/// login — the EU book's row owns DEMO2, so DEMO2's login is what the sidecar is started with,
/// though the Swiss bank's login sits beside it in the store — and with its tools resolved from
/// the two directories `vike-mount` handed it, each in its own place: the jar under the BIN
/// directory and the JVM's home under the STATE directory. Swapped, the jar would be looked for
/// under `settings/state` and the home would land under `bin/`.
#[test]
fn an_identified_account_starts_with_its_own_rows_login_and_the_handed_directories() {
    let both_logins: Vec<(&str, &str)> = SWISS_LOGIN.iter().chain(EU_LOGIN).copied().collect();
    let mut fx = two_account_box(label("3716974"), &both_logins);
    let bin = PathBuf::from("/nonexistent/vike/project/bin");
    let state = PathBuf::from("/nonexistent/vike/project/settings/state");
    fx.process = ProcessFacts {
        state_dir: Some(state.clone()),
        bin_dir: Some(bin.clone()),
        ..Default::default()
    };
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut started = None;
    mount_with(fx.request(true, "EURUSD", &tx), |cfg, tools, _| {
        started = Some((cfg.login, tools.clone()));
        Err(DukascopyError::Unavailable)
    });
    let (login, tools) = started.expect("an identified account with its login reaches the start");
    assert_eq!(login, "fake-login-2", "the EU book's row owns DEMO2, so DEMO2's login starts it");
    assert_eq!(tools.bridge_jar, bin.join(JFOREX_TOOL_DIR).join(BRIDGE_JAR_FILE));
    assert_eq!(tools.jforex_home, Some(state.join(JFOREX_HOME_SUBDIR)));
}

/// A start that cannot find its jar is PAPER and reconciles nothing, even with reconciliation on
/// — `crate::exec`'s `spawn` refuses before it forks, so this stays JVM-free.
#[test]
fn a_start_that_cannot_find_its_jar_is_paper_and_reconciles_nothing() {
    let mut pairs = SWISS_LOGIN.to_vec();
    pairs.push((JFOREX_BRIDGE_JAR_ENV, "/nonexistent/vike/jforex-bridge.jar"));
    let fx = MountFixture::new(&pairs);
    let (tx, _rx) = vike_exec::event_channel(8);
    let mut req = fx.request(true, "EURUSD", &tx);
    req.recon_enabled = true;
    let out = DukascopyVenueMount.mount(req);
    assert!(matches!(out.exec, ExecOutcome::Paper), "a sidecar that never started is paper");
    assert!(out.recon.is_none(), "…and a recon client derives from a RUNNING sidecar only");
}

/// The confirmation parks into the state directory the mount is HANDED (`MountInputs::process`)
/// — this crate reads no process-global path — and `None` parks nothing and does not panic.
#[test]
fn a_confirmation_parks_into_the_state_dir_it_is_handed() {
    let mount = DukascopyMount {
        account: DukascopyAccount::Demo1,
        row: Some(7),
        book: Some("3709890".to_string()),
    };
    record_confirmation(&mount, "3709890", None);
    let dir = tempfile::tempdir().expect("a scratch state directory");
    record_confirmation(&mount, "3709890", Some(dir.path()));
    let parked =
        vike_model::accounts::account_confirmation::read(Some(dir.path())).expect("readable");
    assert_eq!(parked.len(), 1, "exactly one confirmation parked");
    assert_eq!(parked[0].venue, "dukascopy");
    assert_eq!(parked[0].key_prefix, DukascopyAccount::Demo1.key_prefix());
    assert_eq!(parked[0].handshake_account_id, "3709890");
}

/// MOVED from `vike-mount`'s dukascopy tests. A store failure is reported as a STORE FAILURE — for
/// BOTH halves of the read, and never as a row that names no broker.
#[test]
fn a_store_that_will_not_open_is_not_reported_as_a_bad_row() {
    let unreadable = AccountDirectory::read(
        Err::<Accounts, _>("disk I/O error"),
        Err::<Option<BTreeMap<i64, AccountKeys>>, _>("disk I/O error"),
    );
    let err = resolve_in(&label("3716974"), &unreadable).expect_err("refused");
    assert!(matches!(err, DukascopyRefusal::StoreUnreadable { .. }), "{err}");
    assert!(err.to_string().contains("disk I/O error"), "{err}");
    // …and the DEFAULT account still mounts, because it never needed the table.
    assert_eq!(
        resolve_in(&AccountLabel::Default, &unreadable).expect("default").account,
        DukascopyAccount::Demo1
    );

    // ⚠ THE HALF THAT WAS SWALLOWED: rows fine, key NAMES unreadable.
    let (accounts, _) = live_store();
    let keys_failed = AccountDirectory::read(
        Ok::<_, &str>(accounts),
        Err::<Option<BTreeMap<i64, AccountKeys>>, _>("database is locked"),
    );
    let err = resolve_in(&label("3716974"), &keys_failed).expect_err("refused");
    assert!(
        matches!(err, DukascopyRefusal::StoreUnreadable { .. }),
        "a key-read failure must be a store failure, not a bad row: {err}"
    );
    let text = err.to_string();
    assert!(text.contains("database is locked"), "{text}");
    assert!(text.contains("STORE FAILURE"), "{text}");
}

/// MOVED. A process that read NO store behaves exactly like a `Backend::Files` box: the default
/// account mounts the Swiss bank, every labelled one is refused.
#[test]
fn an_unread_store_is_a_files_box() {
    let unread = AccountDirectory::unread();
    let mount = resolve_in(&AccountLabel::Default, &unread).expect("default");
    assert_eq!(mount.account, DukascopyAccount::Demo1);
    assert_eq!(mount.row, None);
    let err = resolve_in(&label("3716974"), &unread).expect_err("refused");
    assert!(matches!(err, DukascopyRefusal::NoAccountTable { .. }), "{err}");
}

/// MOVED. …and a directory a caller DID read resolves both accounts, through the same entry point
/// the mount uses.
#[test]
fn a_read_directory_resolves_each_account_through_the_mount_entry_point() {
    let (accounts, keys) = live_store();
    let dir = AccountDirectory::from_rows(accounts, Some(keys));
    assert_eq!(
        resolve_in(&label("3716974"), &dir).expect("row 8").account,
        DukascopyAccount::Demo2
    );
    assert_eq!(
        resolve_in(&AccountLabel::Default, &dir).expect("default").account,
        DukascopyAccount::Demo1
    );
}
