//! `secrets delete KEY --confirm KEY` over the SHIPPED binary: the key reads as absent afterwards,
//! the ceremony and the refusals change nothing, no token is ever quoted back, and the act is
//! journalled by NAME (decision 0144).

use super::support::{SetCase, canon, exit_code, rung};
use super::{stderr, stdout};

const STORE: &str = "BINANCE_LIVE_API_KEY=live-key-never-printed\n\
                     OKX_DEMO_API_PASSPHRASE=demo-pass\n";
const KEY: &str = "BINANCE_LIVE_API_KEY";

fn seeded(tag: &str) -> SetCase {
    let c = SetCase::new(tag);
    c.store_rows(STORE);
    c
}

/// **The delete retires the key: every other row stays, the act is journalled by NAME under its
/// own kind, and the value reaches no stream and no record.**
#[test]
fn delete_with_the_confirm_takes_the_key_out_and_journals_its_name() {
    let c = seeded("ok");
    let out = c.run(&["delete", KEY, "--confirm", KEY], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));

    assert_eq!(c.read_store(), canon("OKX_DEMO_API_PASSPHRASE=demo-pass\n"), "only the key went");
    let text = stdout(&out);
    assert!(text.contains(KEY) && text.contains("RETIRED"), "{text}");
    assert!(!text.contains("live-key-never-printed"), "a VALUE reached stdout: {text}");
    assert!(!stderr(&out).contains("live-key-never-printed"), "a VALUE reached stderr");

    let lines = c.journal_lines();
    let retires: Vec<&String> = lines.iter().filter(|l| l.contains("credential_retire")).collect();
    assert_eq!(retires.len(), 1, "ONE record for ONE delete: {lines:?}");
    assert!(retires[0].contains(KEY), "{}", retires[0]);
    assert!(!lines.iter().any(|l| l.contains("live-key-never-printed")), "a VALUE was journalled");
    assert!(!lines.iter().any(|l| l.contains("credential_write")), "a delete is not a write");

    // …and `set` adds it back.
    let back = c.run(&["set", KEY], Some("re-added-value\n"), &[]);
    assert_eq!(exit_code(&back), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&back));
    assert_eq!(
        c.read_store(),
        canon(&format!("OKX_DEMO_API_PASSPHRASE=demo-pass\n{KEY}=re-added-value\n")),
        "the re-added value answers, never the retired one"
    );
}

/// **No confirm, or the wrong one: refused on the USAGE rung, nothing changed, nothing journalled,
/// and the typed token is not quoted back.**
#[test]
fn delete_without_the_exact_confirm_changes_nothing() {
    let c = seeded("confirm");
    for args in [
        vec!["delete", KEY],
        vec!["delete", KEY, "--confirm", "sk-typed-into-the-wrong-box"],
        vec!["delete", KEY, "--confirm", "OKX_DEMO_API_PASSPHRASE"],
    ] {
        let out = c.run(&args, None, &[]);
        assert_eq!(
            exit_code(&out),
            rung(vike_cli::exit::Exit::Usage),
            "{args:?}: {}",
            stderr(&out)
        );
        assert!(stderr(&out).contains("--confirm"), "{}", stderr(&out));
        assert!(!stderr(&out).contains("sk-typed-into-the-wrong-box"), "a typed token was quoted");
        assert_eq!(c.read_store(), canon(STORE), "{args:?}: nothing was changed");
    }
    assert!(c.journal_lines().is_empty(), "a refused delete records nothing");
}

/// **`--dry-run` echoes the store, the key and its account, asks for the confirm, and changes
/// nothing.**
#[test]
fn delete_dry_run_changes_nothing() {
    let c = seeded("dry");
    let out = c.run(&["delete", KEY, "--dry-run", "--confirm", KEY], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Ok), "{}", stderr(&out));
    let text = stdout(&out);
    assert!(text.contains("DRY RUN") && text.contains("binance"), "{text}");
    assert!(text.contains(&c.store().display().to_string()), "the store is named: {text}");
    assert_eq!(c.read_store(), canon(STORE));
    assert!(c.journal_lines().is_empty(), "a rehearsal records nothing");
}

/// **A key the store does not hold, and a NODE key, are refused BY NAME, and nothing changes.**
#[test]
fn delete_refuses_an_absent_key_and_a_node_key() {
    let c = seeded("refused");
    let absent =
        c.run(&["delete", "BYBIT_DEMO_API_KEY", "--confirm", "BYBIT_DEMO_API_KEY"], None, &[]);
    assert_eq!(exit_code(&absent), rung(vike_cli::exit::Exit::Usage), "{}", stderr(&absent));
    assert!(stderr(&absent).contains("BYBIT_DEMO_API_KEY"), "{}", stderr(&absent));

    let node = c.run(
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
    assert_eq!(c.read_store(), canon(STORE));
    assert!(c.journal_lines().is_empty());
}

/// **A VALUE on the command line is refused quoting NOTHING** — a second positional, an inline
/// `=`, and a dash-leading token (a PEM armour line) alike. `delete` takes no value in any form.
#[test]
fn a_value_on_the_delete_command_line_is_refused_and_never_echoed() {
    let c = seeded("argv");
    for args in [
        vec!["delete", KEY, "sk-live-typed-out-of-habit"],
        vec!["delete", "BINANCE_LIVE_API_KEY=sk-live-typed-out-of-habit"],
        vec!["delete", KEY, "-----BEGIN sk-live-typed-out-of-habit"],
    ] {
        let out = c.run(&args, None, &[]);
        assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Usage), "{args:?}");
        assert!(!stderr(&out).contains("sk-live-typed-out-of-habit"), "{args:?}: echoed a token");
        assert!(!stdout(&out).contains("sk-live-typed-out-of-habit"), "{args:?}: echoed a token");
        assert_eq!(c.read_store(), canon(STORE));
    }
}

/// **No store: refused, and none is created.**
#[test]
fn delete_with_no_store_is_refused_and_creates_none() {
    let c = SetCase::new("absent");
    let out = c.run(&["delete", KEY, "--confirm", KEY], None, &[]);
    assert_eq!(exit_code(&out), rung(vike_cli::exit::Exit::Failed), "{}", stderr(&out));
    assert!(!c.store().exists(), "a delete must never create a store");
}
