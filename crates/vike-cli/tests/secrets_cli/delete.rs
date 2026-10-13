//! `secrets delete KEY --confirm KEY` over the SHIPPED binary: the key reads as absent afterwards,
//! the ceremony and the refusals change nothing, no token is ever quoted back, and the act is
//! journalled by NAME (decision 0144) — under WHOSE the name is by the store's filing, never by a
//! reading of the name: an account's key carries its account ROW's venue and tier, a name no account
//! owns is `untiered` (decision 0156 item 6).

use super::support::{PlantedAccount, SetCase, canon, exit_code, rung};
use super::{stderr, stdout};

/// The account whose key the cases delete: binance at `live`, unlabelled, created with its tier and
/// its key filed against it — the one way an account's credential reaches a store (decision 0156
/// item 6). The NAME a delete addresses is the row's (`vike_secrets::account_credential_name`).
const ACCOUNT: PlantedAccount<'static> =
    ("binance", "live", None, &[("API_KEY", "live-key-never-printed")]);

/// A LABELLED account beside it: the key a wrong `--confirm` names, and the echo's label clause.
const LABELLED: PlantedAccount<'static> =
    ("okx", "demo", Some("HEDGE"), &[("API_PASSPHRASE", "demo-pass")]);

/// Names no account owns, planted BY NAME — DEPLOYMENT rows: a data provider's key and an
/// attribution code.
const UNOWNED: &str = "TARDIS_API_KEY=tardis-key-never-printed\n\
                       BINANCE_BROKER_CODE=broker-one\n";

/// A case seeded with [`ACCOUNT`], [`LABELLED`] and [`UNOWNED`], and the names it deletes.
struct Seeded {
    c: SetCase,
    /// [`ACCOUNT`]'s row id.
    id: i64,
    /// [`ACCOUNT`]'s `API_KEY`, by the name the store filed it under.
    key: String,
    /// [`LABELLED`]'s row id.
    labelled_id: i64,
    /// [`LABELLED`]'s `API_PASSPHRASE`, by its row name.
    labelled_key: String,
    /// The store as seeded — what every "nothing was changed" compares against.
    before: String,
}

fn seeded(tag: &str) -> Seeded {
    let c = SetCase::new(tag);
    let ids = c.seed_accounts(&[ACCOUNT, LABELLED]);
    c.store_rows(UNOWNED);
    let key = vike_secrets::account_credential_name("binance", ids[0], "API_KEY");
    let labelled_key = vike_secrets::account_credential_name("okx", ids[1], "API_PASSPHRASE");
    let before = c.read_store();
    // The fixture's own check: the store holds each key under the name these cases address, so a
    // case below fails on the property it names rather than on a fixture that drifted.
    assert_eq!(
        before,
        canon(&format!("{key}=live-key-never-printed\n{labelled_key}=demo-pass\n{UNOWNED}")),
        "the seeded store"
    );
    Seeded { c, id: ids[0], key, labelled_id: ids[1], labelled_key, before }
}

/// One `"cell":"value"` pair of a ledger line, as the journal's compact JSON spells it.
fn cell(name: &str, value: &str) -> String {
    format!("\"{name}\":\"{value}\"")
}

/// **The delete retires the account's key: every other row stays, the act is journalled by NAME
/// under its own kind — with the account ROW's venue and tier — and the value reaches no stream and
/// no record.** `set --account` is the way back.
#[test]
fn delete_with_the_confirm_takes_the_key_out_and_journals_its_name() {
    let s = seeded("ok");
    let key = s.key.as_str();
    let out = s.c.run(&["delete", key, "--confirm", key], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));

    let rest = format!("{}=demo-pass\n{UNOWNED}", s.labelled_key);
    assert_eq!(s.c.read_store(), canon(&rest), "only the key went");
    let text = stdout(&out);
    assert!(text.contains(key) && text.contains("RETIRED"), "{text}");
    assert!(
        text.contains(&format!("`vike-cli secrets set --account {} <FIELD>`", s.id)),
        "the way back is the ACCOUNT's: {text}"
    );
    assert!(!text.contains("live-key-never-printed"), "a VALUE reached stdout: {text}");
    assert!(!stderr(&out).contains("live-key-never-printed"), "a VALUE reached stderr");

    let lines = s.c.journal_lines();
    let retires: Vec<&String> = lines.iter().filter(|l| l.contains("credential_retire")).collect();
    assert_eq!(retires.len(), 1, "ONE record for ONE delete: {lines:?}");
    assert!(retires[0].contains(key), "the row's NAME: {}", retires[0]);
    assert!(retires[0].contains(&cell("venue", "binance")), "the ROW's venue: {}", retires[0]);
    assert!(retires[0].contains(&cell("tier", "live")), "the ROW's tier: {}", retires[0]);
    assert!(!lines.iter().any(|l| l.contains("live-key-never-printed")), "a VALUE was journalled");
    assert!(!lines.iter().any(|l| l.contains("credential_write")), "a delete is not a write");

    // …and `set --account` adds it back.
    let id = s.id.to_string();
    let back = s.c.run(&["set", "--account", &id, "API_KEY"], Some("re-added-value\n"), &[]);
    assert_eq!(exit_code(&back), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&back));
    assert_eq!(
        s.c.read_store(),
        canon(&format!("{key}=re-added-value\n{rest}")),
        "the re-added value answers, never the retired one"
    );
}

/// **A name no account owns is retired the same way and recorded as no account's**: the ledger's
/// venue cell is the deployment and its tier cell `untiered` — no tier is read out of a name — and
/// `set NAME` is the way back.
#[test]
fn deleting_a_name_no_account_owns_journals_it_untiered() {
    let s = seeded("unowned");
    let name = "TARDIS_API_KEY";
    let out = s.c.run(&["delete", name, "--confirm", name], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let (text, err) = (stdout(&out), stderr(&out));
    assert!(text.contains("belongs: deployment (no account)"), "{text}");
    assert!(
        text.contains(&format!("`vike-cli secrets set {name}`")),
        "the way back is the NAME: {text}"
    );
    assert!(!text.contains("tardis-key-never-printed"), "a VALUE reached stdout: {text}");
    assert!(!err.contains("tardis-key-never-printed"), "a VALUE reached stderr: {err}");

    let rest = format!(
        "{}=live-key-never-printed\n{}=demo-pass\nBINANCE_BROKER_CODE=broker-one\n",
        s.key, s.labelled_key
    );
    assert_eq!(s.c.read_store(), canon(&rest), "only the name went");

    let lines = s.c.journal_lines();
    assert_eq!(lines.len(), 1, "ONE record for ONE delete: {lines:?}");
    let line = &lines[0];
    assert!(line.contains("credential_retire") && line.contains(name), "{line}");
    assert!(line.contains(&cell("venue", "deployment")), "the deployment's: {line}");
    assert!(
        line.contains(&cell("tier", vike_change_journal::TIER_UNTIERED)),
        "no account, so no tier: {line}"
    );
    assert!(!line.contains("tardis-key-never-printed"), "a VALUE was journalled: {line}");

    // …and `set NAME` adds it back.
    let back = s.c.run(&["set", name], Some("re-added-value\n"), &[]);
    assert_eq!(exit_code(&back), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&back));
    assert_eq!(
        s.c.read_store(),
        canon(&format!("{rest}{name}=re-added-value\n")),
        "the re-added value answers, never the retired one"
    );
}

/// **No confirm, or the wrong one: refused on the USAGE rung, nothing changed, nothing journalled,
/// and the typed token is not quoted back.**
#[test]
fn delete_without_the_exact_confirm_changes_nothing() {
    let s = seeded("confirm");
    let key = s.key.as_str();
    for args in [
        vec!["delete", key],
        vec!["delete", key, "--confirm", "sk-typed-into-the-wrong-box"],
        vec!["delete", key, "--confirm", s.labelled_key.as_str()],
    ] {
        let out = s.c.run(&args, None, &[]);
        assert_eq!(
            exit_code(&out),
            rung(vike_cli::exit::Exit::Usage),
            "{args:?}: {}",
            stderr(&out)
        );
        assert!(stderr(&out).contains("--confirm"), "{}", stderr(&out));
        assert!(!stderr(&out).contains("sk-typed-into-the-wrong-box"), "a typed token was quoted");
        assert_eq!(s.c.read_store(), s.before, "{args:?}: nothing was changed");
    }
    assert!(s.c.journal_lines().is_empty(), "a refused delete records nothing");
}

/// **`--dry-run` echoes the store, the key and WHOSE it is — the store's filing, never a reading of
/// the name — asks for the confirm, and changes nothing.** One name of every owner: an unlabelled
/// account's key, a labelled account's, a venue's application row, a deployment name and an
/// attribution code.
#[test]
fn delete_dry_run_echoes_whose_the_key_is_and_changes_nothing() {
    let s = seeded("dry");
    vike_secrets::save_owned_credentials_unjournalled(
        &s.c.settings(),
        &vike_secrets::CredentialOwner::Venue("ctrader".to_string()),
        &[("CLIENT_ID".to_string(), "client-id-never-printed".to_string())],
        vike_secrets::UpsertIntent::Add,
    )
    .expect("plant ctrader's venue-wide CLIENT_ID");
    let before = s.c.read_store();

    for (name, belongs) in [
        (s.key.clone(), format!("binance / live (account {})", s.id)),
        (s.labelled_key.clone(), format!("okx / demo (account {}, label HEDGE)", s.labelled_id)),
        (
            vike_secrets::venue_credential_name("ctrader", "CLIENT_ID"),
            "ctrader (venue-wide: every ctrader account)".to_string(),
        ),
        ("TARDIS_API_KEY".to_string(), "deployment (no account)".to_string()),
        ("BINANCE_BROKER_CODE".to_string(), "binance (no account)".to_string()),
    ] {
        let out = s.c.run(&["delete", &name, "--dry-run", "--confirm", &name], None, &[]);
        assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{name}: {}", stderr(&out));
        let (text, err) = (stdout(&out), stderr(&out));
        assert!(text.contains("DRY RUN"), "{name}: {text}");
        assert!(text.contains(&format!("belongs: {belongs}\n")), "{name}: {text}");
        assert!(text.contains(&s.c.store().display().to_string()), "the store is named: {text}");
        for value in ["never-printed", "demo-pass"] {
            assert!(!text.contains(value) && !err.contains(value), "a VALUE reached a stream");
        }
    }
    assert_eq!(s.c.read_store(), before);
    assert!(s.c.journal_lines().is_empty(), "a rehearsal records nothing");
}

/// **A key the store does not hold, and a NODE key, are refused BY NAME, and nothing changes.** The
/// absent one is the account's key in the retired `{VENUE}_{TIER}_{FIELD}` spelling: the store files
/// an account's key under the row's own name, so the old spelling addresses nothing.
#[test]
fn delete_refuses_an_absent_key_and_a_node_key() {
    const RETIRED_SPELLING: &str = "BINANCE_LIVE_API_KEY";
    let s = seeded("refused");
    let absent = s.c.run(&["delete", RETIRED_SPELLING, "--confirm", RETIRED_SPELLING], None, &[]);
    assert_eq!(exit_code(&absent), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&absent));
    assert!(stderr(&absent).contains(RETIRED_SPELLING), "{}", stderr(&absent));

    let node = s.c.run(
        &["delete", "VIKE_TRADEHUB_OBSERVE_KEY", "--confirm", "VIKE_TRADEHUB_OBSERVE_KEY"],
        None,
        &[],
    );
    assert_eq!(exit_code(&node), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&node));
    assert!(
        stderr(&node).contains("backend setup"),
        "it names the minting verb: {}",
        stderr(&node)
    );
    assert_eq!(s.c.read_store(), s.before);
    assert!(s.c.journal_lines().is_empty());
}

/// **A VALUE on the command line is refused quoting NOTHING** — a second positional, an inline
/// `=`, and a dash-leading token (a PEM armour line) alike. `delete` takes no value in any form.
#[test]
fn a_value_on_the_delete_command_line_is_refused_and_never_echoed() {
    let s = seeded("argv");
    let key = s.key.as_str();
    let inline = format!("{key}=sk-live-typed-out-of-habit");
    for args in [
        vec!["delete", key, "sk-live-typed-out-of-habit"],
        vec!["delete", inline.as_str()],
        vec!["delete", key, "-----BEGIN sk-live-typed-out-of-habit"],
    ] {
        let out = s.c.run(&args, None, &[]);
        assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{args:?}");
        assert!(!stderr(&out).contains("sk-live-typed-out-of-habit"), "{args:?}: echoed a token");
        assert!(!stdout(&out).contains("sk-live-typed-out-of-habit"), "{args:?}: echoed a token");
        assert_eq!(s.c.read_store(), s.before);
    }
}

/// **No store: refused, and none is created.**
#[test]
fn delete_with_no_store_is_refused_and_creates_none() {
    let c = SetCase::new("absent");
    let out = c.run(&["delete", "TARDIS_API_KEY", "--confirm", "TARDIS_API_KEY"], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Failed), "{}", stderr(&out));
    assert!(!c.store().exists(), "a delete must never create a store");
}
