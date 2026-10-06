//! **`VIKE_CREDENTIAL_SCOPE` reaches the store through the sweep-taking loader, and can only
//! NARROW.**
//!
//! `vike_bridge_core::credentials::load_workspace_secrets_from_env` takes two facts out of a
//! process's environment sweep: the settings directory, and the credential scope. This drives the
//! three answers the scope has over ONE real migrated settings database holding demo AND live names:
//!
//! * unset — every name, byte-identical to before the scope existed (the control);
//! * `demo` — no `_LIVE_`/`_MAINNET_`/`ASTER_`/`POLY_` name, so a live request however it is
//!   spelled finds nothing, while the demo request beside it still loads;
//! * anything else — NO credential at all and an unreadable verdict, never a guess.
//!
//! The env maps here are built by hand: nothing reads or writes this process's real environment.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use vike_bridge_core::credentials::{
    Environment, StoreHealth, load_credentials_from, load_workspace_secrets_from_env,
    load_workspace_secrets_from_env_checked,
};

/// Every planted name; the values are obviously fake.
const NAMES: &[&str] = &[
    "BINANCE_DEMO_API_KEY",
    "BINANCE_DEMO_API_SECRET",
    "BINANCE_LIVE_API_KEY",
    "BINANCE_LIVE_API_SECRET",
    "HYPERLIQUID_DEMO_PRIVATE_KEY",
    "HYPERLIQUID_LIVE_PRIVATE_KEY",
    "HYPERLIQUID_MAINNET_PRIVATE_KEY",
    "ASTER_DEMO_API_KEY",
    "POLY_PRIVATE_KEY",
];

fn migrated_store(root: &Path) -> PathBuf {
    let settings = root.join("settings");
    std::fs::create_dir_all(&settings).expect("settings dir");
    let text: String = NAMES.iter().map(|n| format!("{n}=fake-{n}\n")).collect();
    std::fs::write(settings.join("secrets.env"), text).expect("write the credential file");
    let classify = |name: &str| vike_secrets::Classification::unrecognised(name);
    vike_secrets::migrate(settings.to_str(), |_| false, &classify).expect("migrate");
    assert!(settings.join("db").join("vike.db").is_file(), "the database must exist");
    settings
}

fn env(settings: &Path, scope: Option<&str>) -> HashMap<String, String> {
    let mut env =
        HashMap::from([("VIKE_SETTINGS_DIR".to_string(), settings.display().to_string())]);
    if let Some(s) = scope {
        env.insert("VIKE_CREDENTIAL_SCOPE".to_string(), s.to_string());
    }
    env
}

#[test]
fn unset_reads_every_name_and_demo_withholds_every_live_or_real_money_one() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let settings = migrated_store(tmp.path());

    // The CONTROL: no scope, every name — including the live ones the scope exists to keep out.
    let all = load_workspace_secrets_from_env(&env(&settings, None));
    assert_eq!(all.len(), NAMES.len(), "the unscoped read lost names: {:?}", all.keys());
    assert!(
        load_credentials_from("binance", Environment::Live, &all).is_some(),
        "the control must be able to load the live pair, or the scoped case proves nothing"
    );
    // A blank scope is the unset case, not an unknown value.
    assert_eq!(load_workspace_secrets_from_env(&env(&settings, Some("  "))).len(), NAMES.len());

    let (demo, health) = load_workspace_secrets_from_env_checked(&env(&settings, Some("demo")));
    assert_eq!(health, StoreHealth::Readable);
    let mut got: Vec<&str> = demo.keys().map(String::as_str).collect();
    got.sort_unstable();
    assert_eq!(
        got,
        ["BINANCE_DEMO_API_KEY", "BINANCE_DEMO_API_SECRET", "HYPERLIQUID_DEMO_PRIVATE_KEY"],
        "the demo-only scope returned a name it must withhold, or lost a demo one"
    );
    assert!(
        load_credentials_from("binance", Environment::Live, &demo).is_none(),
        "⚠ a LIVE request loaded a live pair under the demo-only scope"
    );
    assert!(
        load_credentials_from("binance", Environment::Demo, &demo).is_some(),
        "the DEMO request must still load under the scope"
    );
}

#[test]
fn an_unknown_scope_value_refuses_every_credential_rather_than_guessing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let settings = migrated_store(tmp.path());
    for bogus in ["Demo", "demo-only", "live", "all"] {
        let (map, health) = load_workspace_secrets_from_env_checked(&env(&settings, Some(bogus)));
        assert!(map.is_empty(), "VIKE_CREDENTIAL_SCOPE={bogus:?} returned {} name(s)", map.len());
        assert!(
            !health.is_readable(),
            "VIKE_CREDENTIAL_SCOPE={bogus:?} must report the empty map as NOT a measurement"
        );
    }
}
