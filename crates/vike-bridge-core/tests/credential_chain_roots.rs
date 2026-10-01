//! **The composition-root credential seam** — the properties that must hold where the seven
//! binaries resolve credentials.
//!
//! This is the highest-risk seam in the workspace: every venue's live/paper decision is "did
//! credentials load", so a defect here either drops every venue to paper (live trading stops) or
//! loads the WRONG credentials. `vike_secrets::store`'s unit tests pin what opening ONE store does;
//! what is pinned HERE is the shape a ROOT actually calls —
//! `credentials::load_workspace_secrets_from_env`, whose input is the process-environment map.
//!
//! # One store
//!
//! `<project>/settings/secrets.env`, and nothing else. `VIKE_SETTINGS_DIR` names the directory
//! outright for a deployment; every other variable in the map is inert, which
//! [`the_home_directory_cannot_supply_credentials`] plants decoys to prove rather than assume.
//!
//! # Never printed
//!
//! Several assertions compare maps that, on a developer box, hold REAL live venue API secrets.
//! Every failure message here is written to name KEYS ONLY — key names are already public
//! knowledge (they are the `vike_ops::settings` registry's subject matter), values are the
//! credential. So no `assert_eq!` is ever applied to two whole credential maps, because its
//! panic message would print them.
//!
//! # Never touched
//!
//! No test here writes, moves or deletes any file outside a fresh temp directory of its own, and no
//! test mutates process state (which is also why they can run in parallel). Every path a test plants
//! is under that temp directory; nothing in this file can reach a real credential file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use vike_bridge_core::credentials::{
    Environment, load_credentials_from, load_workspace_dotenv, load_workspace_secrets_at,
    load_workspace_secrets_from_env,
};

/// A private temp directory. Fresh per test.
fn tmp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "vike-credroots-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Write a `KEY=value` store, creating parents.
fn put(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

/// A pair of demo credentials in the store's format.
fn demo_creds_body(key: &str, secret: &str) -> String {
    format!("# a comment\nBINANCE_DEMO_API_KEY={key}\nBINANCE_DEMO_API_SECRET={secret}\n")
}

/// A populated project store under `dir`, and the settings directory that names it.
fn project_store(dir: &Path, key: &str) -> PathBuf {
    let settings = dir.join("settings");
    put(&settings.join("secrets.env"), &demo_creds_body(key, "demo-secret"));
    settings
}

/// The process-environment map a root sweeps with `std::env::vars().collect()`, naming a settings
/// directory outright.
fn env_at(settings: &Path) -> HashMap<String, String> {
    HashMap::from([("VIKE_SETTINGS_DIR".to_string(), settings.display().to_string())])
}

/// Compare two credential maps WITHOUT ever printing a value — see the module doc.
fn assert_same_credentials(actual: &HashMap<String, String>, expected: &HashMap<String, String>) {
    let keys = |m: &HashMap<String, String>| {
        let mut v: Vec<String> = m.keys().cloned().collect();
        v.sort_unstable();
        v
    };
    assert_eq!(
        keys(actual),
        keys(expected),
        "the two calls disagree about WHICH credentials exist"
    );
    for k in keys(actual) {
        assert!(
            actual.get(&k) == expected.get(&k),
            "the two calls disagree about the VALUE of {k} (value withheld)"
        );
    }
}

// ── PROPERTY 1: the store is the project's, named or walked to ────────────────────────────────────

/// A named settings directory IS the store, and the credentials in it drive the live verdict.
#[test]
fn a_named_settings_directory_supplies_the_credentials() {
    let d = tmp_dir("named");
    let settings = project_store(&d, "demo-key-1234");

    let vars = load_workspace_secrets_from_env(&env_at(&settings));
    let creds = load_credentials_from("binance", Environment::Demo, &vars)
        .expect("a populated store must produce a LIVE verdict");
    assert_eq!(creds.api_key, "demo-key-1234");

    // …and the store is not rewritten by having been read.
    assert!(
        std::fs::read_to_string(settings.join("secrets.env"))
            .unwrap()
            .contains("BINANCE_DEMO_API_KEY=demo-key-1234")
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// With nothing named, the three entry points a binary can reach are ONE resolution: the walk from
/// the working directory. Meaningful whether or not this checkout has a store — with one, all three
/// return its contents; without (a clean CI checkout), all three return an empty map. What is pinned
/// is that they AGREE.
#[test]
fn every_entry_point_resolves_the_same_store() {
    let scrubbed: HashMap<String, String> = HashMap::new();
    assert_same_credentials(&load_workspace_secrets_from_env(&scrubbed), &load_workspace_dotenv());
    assert_same_credentials(&load_workspace_secrets_at(None), &load_workspace_dotenv());
    // A blank value (a systemd unit whose interpolation produced nothing) is the same arm.
    let blank = HashMap::from([("VIKE_SETTINGS_DIR".to_string(), "   ".to_string())]);
    assert_same_credentials(&load_workspace_secrets_from_env(&blank), &load_workspace_dotenv());
}

// ── PROPERTY 2: nothing outside the project can supply credentials ────────────────────────────────

/// Paths under a home directory that must NEVER supply credentials — planted as decoys below.
///
/// Asserting the project store loads would pass under any resolution; planting a populated decoy
/// and asserting it is INERT is what makes the test capable of failing.
const HOME_DECOYS: &[&str] = &[".vike/secrets.env", ".vike/secrets.enc", "secrets.env"];

/// Variables that name a per-user directory. None of them may reach a credential.
const HOME_VARS: &[&str] = &["HOME", "USERPROFILE", "XDG_DATA_HOME", "LOCALAPPDATA"];

/// **A home directory cannot supply credentials, however it is named and whatever it holds.**
///
/// The decoys hold DIFFERENT credentials from the project store, so a resolution that consulted one
/// would not merely add keys — it would change the api_key a live order is signed with. Each
/// variable is tested on its own so no single one can be inert by accident of ordering.
#[test]
fn the_home_directory_cannot_supply_credentials() {
    let d = tmp_dir("home-decoys");
    let settings = project_store(&d, "from-the-project");

    let decoy_home = d.join("home");
    for leaf in HOME_DECOYS {
        put(&decoy_home.join(leaf), &demo_creds_body("from-a-home", "from-a-home"));
    }

    let expected = load_workspace_secrets_from_env(&env_at(&settings));
    assert_eq!(
        load_credentials_from("binance", Environment::Demo, &expected)
            .map(|c| c.api_key)
            .as_deref(),
        Some("from-the-project"),
        "precondition: the project store is what answers"
    );

    for name in HOME_VARS {
        let mut env = env_at(&settings);
        env.insert((*name).to_string(), decoy_home.display().to_string());
        let vars = load_workspace_secrets_from_env(&env);
        assert_same_credentials(&vars, &expected);
        assert_eq!(
            load_credentials_from("binance", Environment::Demo, &vars)
                .map(|c| c.api_key)
                .as_deref(),
            Some("from-the-project"),
            "{name} reached a credential outside the project"
        );
    }

    // Every decoy is still on disk, byte for byte: inert means unread, not consumed.
    for leaf in HOME_DECOYS {
        assert!(std::fs::read_to_string(decoy_home.join(leaf)).unwrap().contains("from-a-home"));
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// The helper reads the PROCESS environment and nothing else — in particular it must not be
/// confusable with the credential map it returns. Feeding it a map shaped like a store (venue keys,
/// no settings directory) resolves nothing from the argument; the venue keys in the INPUT are never
/// mistaken for the ANSWER.
#[test]
fn credential_shaped_input_is_not_mistaken_for_the_answer() {
    let looks_like_the_store = HashMap::from([
        ("BINANCE_DEMO_API_KEY".to_string(), "from-the-argument".to_string()),
        ("BINANCE_DEMO_API_SECRET".to_string(), "from-the-argument".to_string()),
    ]);
    let vars = load_workspace_secrets_from_env(&looks_like_the_store);
    assert!(
        vars.get("BINANCE_DEMO_API_KEY").map(String::as_str) != Some("from-the-argument"),
        "the process-environment argument must never be echoed back as the credential store"
    );
    assert_same_credentials(&vars, &load_workspace_dotenv());
}

// ── PROPERTY 3: absent credentials ARE the live gate ──────────────────────────────────────────────

/// A store that names a DIFFERENT venue still leaves binance on paper — the gate is per-venue key
/// presence, not "did a store load".
#[test]
fn absent_credentials_still_mean_paper_when_a_store_exists() {
    let d = tmp_dir("gate-paper");
    let settings = d.join("settings");
    // All THREE okx credentials: this venue's signer sends the passphrase on every request, so
    // key+secret alone would not be a configured venue at all — see the half-credential test below.
    put(
        &settings.join("secrets.env"),
        "OKX_DEMO_API_KEY=k\nOKX_DEMO_API_SECRET=s\nOKX_DEMO_API_PASSPHRASE=p\n",
    );

    let vars = load_workspace_secrets_from_env(&env_at(&settings));
    assert!(
        load_credentials_from("binance", Environment::Demo, &vars).is_none(),
        "a store with no BINANCE_DEMO_* keys must leave binance on PAPER"
    );
    assert!(
        load_credentials_from("okx", Environment::Demo, &vars).is_some(),
        "…while the venue the store DOES name goes live"
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// **A HALF-configured venue is an ABSENT one** — the live gate reads the venue's REQUIRED
/// credential set, not "did some key for this venue load".
///
/// This is the store-level twin of the unit gate in `credentials.rs`, and it is here because this
/// file is where the live-gate properties live: a real store on disk, read through the real root
/// entry point, holding two of OKX's three credentials. Before the per-venue passphrase table this
/// resolved `Some`, the venue mounted LIVE, and every signed request was then rejected by the venue
/// (`OK-ACCESS-PASSPHRASE cannot be empty`) — the gate admitting the exact state it exists to
/// prevent. The missing credential is reported BY NAME, so the silent paper mount is not silent.
#[test]
fn a_half_configured_venue_is_an_absent_one() {
    let d = tmp_dir("gate-half");
    let settings = d.join("settings");
    put(&settings.join("secrets.env"), "OKX_DEMO_API_KEY=k\nOKX_DEMO_API_SECRET=s\n");

    let vars = load_workspace_secrets_from_env(&env_at(&settings));
    assert_eq!(vars.len(), 2, "the store itself loaded — this is not an empty-map artifact");
    assert!(
        load_credentials_from("okx", Environment::Demo, &vars).is_none(),
        "okx key+secret with no passphrase are UNUSABLE credentials → the venue stays PAPER"
    );
    assert_eq!(
        vike_bridge_core::credentials::missing_required_passphrase("okx", Environment::Demo, &vars)
            .as_deref(),
        Some("OKX_DEMO_API_PASSPHRASE"),
        "…and the operator is told WHICH credential is missing, by name"
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// **A store that EXISTS and cannot be read is loud-and-EMPTY at the root call site**, never a
/// partial or guessed answer.
///
/// Empty is the safe verdict — it means "cannot authenticate", every venue stays paper, and the
/// failure surfaces at the venue where an operator sees it. What must never happen is credentials
/// coming back from somewhere else because this file failed. A DIRECTORY where the store should be
/// is the portable stand-in for an unreadable file (a `chmod 000` proves nothing when the test runs
/// as root, which CI does).
#[test]
fn an_unreadable_store_is_loud_and_empty_at_the_root_call_site() {
    let d = tmp_dir("unreadable");
    let settings = d.join("settings");
    std::fs::create_dir_all(settings.join("secrets.env")).unwrap();

    let vars = load_workspace_secrets_from_env(&env_at(&settings));
    assert!(vars.is_empty(), "an unopenable store must yield an EMPTY map");
    assert!(
        load_credentials_from("binance", Environment::Demo, &vars).is_none(),
        "every venue must stay PAPER"
    );
    let _ = std::fs::remove_dir_all(&d);
}
