//! Shared setup for the live bybit smokes: the demo credentials, or a loud skip.
use vike_secrets::credentials::{
    Credentials, Environment, load_credentials_from, load_workspace_secrets_at,
};

/// `BYBIT_DEMO_*` from the workspace store (`$VIKE_SETTINGS_DIR` when set), or `None` after the skip warning.
pub fn demo_creds() -> Option<Credentials> {
    let vars = load_workspace_secrets_at(std::env::var("VIKE_SETTINGS_DIR").ok().as_deref());
    let creds = load_credentials_from("bybit", Environment::Demo, &vars);
    if creds.is_none() {
        tracing::warn!(target: "vike_bybit", "SKIP: BYBIT_DEMO creds absent");
    }
    creds
}
