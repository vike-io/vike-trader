//! **Which Dukascopy account this process arms — over the REAL projection, on a real two-account
//! store.**
//!
//! # The defect this file exists to hold shut
//!
//! `vike-mount`'s dukascopy arm learned to address the account it is mounted for (#1845), keyed on
//! the settings database's `account` row, and the feature **could not be reached**. The fan-out
//! mounts the DEFAULT account first (`known_accounts` puts it first and `accounts_to_mount`
//! preserves row order); the one-sidecar claim was first-come; and `VenuePolicy::account` is
//! `min(venue line, account line)`, so raising `venues.dukascopy` to `demo` — the only way to get an
//! `[accounts.dukascopy]` line above paper — NECESSARILY arms the default account too. The default
//! therefore took the sidecar on every box, every time, and every labelled account hit the claim
//! refusal. On the owner's own box, holding both `DUKASCOPY_DEMO1_*` and `DUKASCOPY_DEMO2_*`, there
//! was **no policy that mounted the second account**.
//!
//! `vike_mount`'s `dukascopy` module now decides the holder from the POLICY, before anything is
//! mounted, and the DEFAULT account yields to a labelled account the operator named. Everything
//! below drives that through `venue_account_arming` — the projection the mount selects with and the
//! one `vike_run::refuse_unarmed_mount_accounts` judges a strategy mount against.
//!
//! # Why these tests can exist at all
//!
//! The `account` table is a PARAMETER now (`vike_mount::AccountDirectory`, carried on
//! `MountPolicy::accounts` and read by the composition root), where the arm used to open the store
//! itself at a path taken from a process global. So a two-account box is a value a test can write
//! down, no database on disk and no process state — which is also why the store read moved.
//!
//! Network-free and JVM-free by construction: the projection opens no socket and spawns nothing.

use std::collections::HashMap;

use vike_bridge_core::credentials::{Account, AccountKeys, Accounts};
use vike_config::{ArmingBlock, VenueMode, VenuePolicy};
use vike_dukascopy::DukascopyAccount;
use vike_model::account_keys::AccountLabel;

const SWISS_BOOK: &str = "3709890";
const EU_BOOK: &str = "3716974";

fn label(text: &str) -> AccountLabel {
    AccountLabel::parse(text).expect("a legal label")
}

fn row(id: i64, book: &str) -> Account {
    Account {
        id,
        venue: "dukascopy".to_string(),
        tier: "demo".to_string(),
        // ⚠ NULL on both, as the migration writes them: the owner refused the provisional
        // `DEMO1`/`DEMO2` label spellings, so the key family is the only thing that separates these
        // two rows — which is the whole reason the arm reads it.
        label: None,
        venue_account_id: Some(book.to_string()),
        parent_id: None,
        active: true,
        last_verified_at: None,
        // DERIVED from the store's `venue_arming` rows (`vike_secrets::Account::armed`) and
        // read by nothing on this path — these rows exercise BOOK identity.
        armed: false,
    }
}

/// The measured live shape: two unlabelled `(dukascopy, demo)` rows, each owning one credential
/// family, each carrying the book the venue itself gave it.
fn directory() -> vike_mount::AccountDirectory {
    let keys = |prefix: &str| AccountKeys { prefixes: vec![prefix.to_string()], names: Vec::new() };
    vike_mount::AccountDirectory::from_rows(
        Accounts::Known(vec![row(7, SWISS_BOOK), row(8, EU_BOOK)]),
        Some(
            [
                (7, keys(DukascopyAccount::Demo1.key_prefix())),
                (8, keys(DukascopyAccount::Demo2.key_prefix())),
            ]
            .into_iter()
            .collect(),
        ),
    )
}

/// Both credential families present — the owner's box, and the only configuration in which the
/// choice of holder decides anything.
fn both_key_families() -> HashMap<String, String> {
    [
        ("DUKASCOPY_DEMO1_LOGIN", "fake-login-1"),
        ("DUKASCOPY_DEMO1_PASSWORD", "fake-pass-1"),
        ("DUKASCOPY_DEMO2_LOGIN", "fake-login-2"),
        ("DUKASCOPY_DEMO2_PASSWORD", "fake-pass-2"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect()
}

/// `venues.dukascopy = "demo"`, plus one `[accounts.dukascopy]` line per `named` entry.
fn policy(named: &[&str]) -> vike_mount::MountPolicy {
    let mut venues = VenuePolicy::default().declare("dukascopy", VenueMode::Demo);
    for book in named {
        venues = venues.declare_account("dukascopy", &label(book), VenueMode::Demo);
    }
    vike_mount::MountPolicy { venues, accounts: directory(), ..Default::default() }
}

fn arming_rows(
    vars: &HashMap<String, String>,
    policy: &vike_mount::MountPolicy,
) -> Vec<vike_config::VenueArming> {
    vike_mount::venue_account_arming("dukascopy", vars, Some(policy))
}

fn find<'a>(
    rows: &'a [vike_config::VenueArming],
    label: &AccountLabel,
) -> &'a vike_config::VenueArming {
    rows.iter()
        .find(|r| r.label == *label)
        .unwrap_or_else(|| panic!("no row for {label}: {rows:?}"))
}

/// **THE HEADLINE: there is now a policy that mounts the SECOND account**, and it is one line.
#[test]
fn naming_the_second_account_arms_it_and_the_default_yields() {
    let vars = both_key_families();
    let rows = arming_rows(&vars, &policy(&[EU_BOOK]));

    let eu = find(&rows, &label(EU_BOOK));
    assert_ne!(eu.effective, VenueMode::Paper, "the named account arms: {}", eu.why());
    assert_eq!(eu.route_key(), format!("dukascopy#{EU_BOOK}"));

    let default = find(&rows, &AccountLabel::Default);
    assert_eq!(
        default.effective,
        VenueMode::Paper,
        "the DEFAULT account must yield — one sidecar per process, and the operator named the \
         other one: {}",
        default.why()
    );
    assert_eq!(
        default.block,
        ArmingBlock::SidecarHeldElsewhere,
        "…and the row must say WHY it is paper, or it reads as a missing credential"
    );
    assert!(
        default.why().contains("ONE") && default.why().contains("dukascopy"),
        "{}",
        default.why()
    );
}

/// **…and the mount SET follows the rows**, so `vike_run::refuse_unarmed_mount_accounts` — which
/// reads exactly this projection — cannot pass a strategy that names an account the mount papers.
///
/// The default account is still in the set (it is the engine every caller binds at `[0]`), and its
/// engine is the paper one its row describes.
#[test]
fn the_mount_set_carries_both_and_the_projection_says_which_one_arms() {
    let vars = both_key_families();
    let rows = arming_rows(&vars, &policy(&[EU_BOOK]));
    assert_eq!(
        vike_mount::accounts_to_mount(&rows),
        vec![AccountLabel::Default, label(EU_BOOK)],
        "the default account is always mounted; the named one is mounted because it ARMED"
    );
}

/// **A box that names no account is byte-identical**: the default account arms, exactly as it has
/// since the arm was written, and nothing is declined.
#[test]
fn a_box_that_names_no_account_still_mounts_the_swiss_bank() {
    let vars = both_key_families();
    let rows = arming_rows(&vars, &policy(&[]));
    assert_eq!(rows.len(), 1, "no second account is named anywhere: {rows:?}");
    let default = find(&rows, &AccountLabel::Default);
    assert_ne!(default.effective, VenueMode::Paper, "{}", default.why());
    assert_ne!(default.block, ArmingBlock::SidecarHeldElsewhere);
    assert_eq!(default.route_key(), "dukascopy", "the sentinel filename does not move");
}

/// **AT MOST ONE dukascopy account ever arms** — the closure of the two-engines-on-one-account
/// hazard `arm_addresses_accounts`' own marker comment forbids joining that list with.
///
/// The arm reads a ROW's credential family, and a labelled account can legitimately resolve to the
/// SAME family as the default account (address `3709890` and the row owning `DUKASCOPY_DEMO1_*` is
/// the same account, reached by its book). Two ARMED engines over one credential set would be two
/// engines on one venue account; one holder per process makes that unreachable rather than
/// unlikely — including in that exact case, asserted below.
#[test]
fn only_one_dukascopy_account_can_ever_arm() {
    let vars = both_key_families();
    for named in [&[][..], &[EU_BOOK][..], &[SWISS_BOOK][..], &[SWISS_BOOK, EU_BOOK][..]] {
        let rows = arming_rows(&vars, &policy(named));
        let armed: Vec<&vike_config::VenueArming> =
            rows.iter().filter(|r| r.effective != VenueMode::Paper).collect();
        assert!(
            armed.len() <= 1,
            "{named:?}: {} dukascopy accounts armed at once — two engines over one JForex sidecar, \
             and on a shared key family two engines over one ACCOUNT: {armed:?}",
            armed.len()
        );
    }
}

/// **The DEFAULT account's own credential family, addressed by its BOOK, is one account and not
/// two.** Naming `3709890` moves the engine's route key, not the broker — and the default account
/// yields, so nothing signs twice.
#[test]
fn addressing_the_default_accounts_own_row_does_not_produce_two_engines_on_it() {
    let vars = both_key_families();
    let rows = arming_rows(&vars, &policy(&[SWISS_BOOK]));
    let swiss = find(&rows, &label(SWISS_BOOK));
    assert_ne!(swiss.effective, VenueMode::Paper, "the named account arms: {}", swiss.why());
    assert_eq!(
        find(&rows, &AccountLabel::Default).effective,
        VenueMode::Paper,
        "…and the default account, which reads the SAME credential family, does not"
    );
}

/// **An account named but unarmable takes nothing from the default account.**
///
/// The holder is the account that WOULD arm on its own merits, not merely the one a line names. A
/// box mid-migration — a `policy.toml` written before `vike-cli secrets set-book`, a typo, a store
/// that cannot identify the row — keeps mounting the account it has always mounted instead of
/// losing it to an account that will not arm either.
#[test]
fn an_account_that_cannot_arm_does_not_take_the_sidecar() {
    let vars = both_key_families();

    // Named, but no such row in the store.
    let rows = arming_rows(&vars, &policy(&["9999999"]));
    assert_eq!(find(&rows, &label("9999999")).block, ArmingBlock::AccountNotInStore);
    assert_ne!(
        find(&rows, &AccountLabel::Default).effective,
        VenueMode::Paper,
        "an unresolvable account must not disarm the account that works"
    );

    // …and a real row whose credential family is absent from the store.
    let only_swiss: HashMap<String, String> = both_key_families()
        .into_iter()
        .filter(|(k, _)| !k.starts_with(DukascopyAccount::Demo2.key_prefix()))
        .collect();
    let rows = arming_rows(&only_swiss, &policy(&[EU_BOOK]));
    assert_eq!(find(&rows, &label(EU_BOOK)).block, ArmingBlock::NoCredentials);
    assert_ne!(find(&rows, &AccountLabel::Default).effective, VenueMode::Paper);
}

/// **A store that will not open is reported as a STORE failure, and never as a bad row** — the
/// projection's half of the refusal `vike_mount`'s `DukascopyRefusal::StoreUnreadable` renders.
///
/// Both halves of the read are carried separately, because the KEY-NAME half used to be swallowed:
/// `.ok().flatten()` turned "the store would not open" into "this row's key names name no broker",
/// sending an operator to fix a row that was never at fault.
///
/// ⚠ **The granularity this test CANNOT see is stated rather than implied.** An arming ROW carries
/// one block, and `AccountNotInStore` is the answer for every dukascopy refusal — so re-swallowing
/// the key-read error would leave this test GREEN (MEASURED, by mutating exactly that). What it
/// proves here is that a broken store does not ARM anything and does not take the default account
/// down with it. The distinction between a store failure and a bad row lives in the REFUSAL an
/// operator reads, and `crates/vike-mount/src/dukascopy.rs`'s
/// `a_store_that_will_not_open_is_not_reported_as_a_bad_row` is what fails when it is lost.
#[test]
fn a_store_that_will_not_open_never_reads_as_an_unmappable_row() {
    let vars = both_key_families();
    let broken = |accounts: vike_mount::AccountDirectory| {
        let p =
            vike_mount::MountPolicy {
                venues: VenuePolicy::default()
                    .declare("dukascopy", VenueMode::Demo)
                    .declare_account("dukascopy", &label(EU_BOOK), VenueMode::Demo),
                accounts,
                ..Default::default()
            };
        let rows = vike_mount::venue_account_arming("dukascopy", &vars, Some(&p));
        find(&rows, &label(EU_BOOK)).block
    };

    // The rows would not read…
    assert_eq!(
        broken(vike_mount::AccountDirectory::read(
            Err::<Accounts, _>("disk I/O error"),
            Ok::<_, &str>(None)
        )),
        ArmingBlock::AccountNotInStore
    );
    // …and the KEY NAMES would not read, which is the half that used to look like a bad row.
    assert_eq!(
        broken(vike_mount::AccountDirectory::read(
            Ok::<_, &str>(Accounts::Known(vec![row(8, EU_BOOK)])),
            Err::<Option<std::collections::BTreeMap<i64, AccountKeys>>, _>("database is locked"),
        )),
        ArmingBlock::AccountNotInStore
    );
    // …and in both cases the DEFAULT account still arms: it never needed the table.
    let p = vike_mount::MountPolicy {
        venues: VenuePolicy::default().declare("dukascopy", VenueMode::Demo),
        accounts: vike_mount::AccountDirectory::read(
            Err::<Accounts, _>("disk I/O error"),
            Err::<Option<std::collections::BTreeMap<i64, AccountKeys>>, _>("disk I/O error"),
        ),
        ..Default::default()
    };
    let rows = vike_mount::venue_account_arming("dukascopy", &vars, Some(&p));
    assert_ne!(find(&rows, &AccountLabel::Default).effective, VenueMode::Paper);
}

/// **A process that read no store is exactly a `Backend::Files` box** — the default account mounts
/// `DUKASCOPY_DEMO1_*`, every labelled account is refused by name, and nothing is coerced.
#[test]
fn an_unread_directory_refuses_every_labelled_account_and_arms_the_default() {
    let vars = both_key_families();
    let p = vike_mount::MountPolicy {
        venues: VenuePolicy::default().declare("dukascopy", VenueMode::Demo).declare_account(
            "dukascopy",
            &label(EU_BOOK),
            VenueMode::Demo,
        ),
        // UNREAD — the default, and what every caller that threads no store gets.
        ..Default::default()
    };
    let rows = vike_mount::venue_account_arming("dukascopy", &vars, Some(&p));
    assert_eq!(find(&rows, &label(EU_BOOK)).block, ArmingBlock::AccountNotInStore);
    assert_ne!(
        find(&rows, &AccountLabel::Default).effective,
        VenueMode::Paper,
        "an unmigrated box keeps the account it has always traded"
    );
}
