//! The ONE way a live FXCM smoke finds the demo account's login, declared ONCE by the `fxcm_smokes` group binary
//! (`main.rs`) for every smoke and for `fxcm_smoke_login.rs`, so it compiles once.
//!
//! A smoke opens a SECOND session of the demo login, and FXCM keeps one session per login, so the lane run
//! deactivates the daemon's own account first (`vike-cli secrets account deactivate --id <N>`; reactivate it after).
//! An INACTIVE row has no `credentials_for` answer (`vike_secrets::account_directory::AccountDirectory::account_row`
//! reads active rows only), so a smoke that asked only that SELF-SKIPPED ("skip: no demo fxcm account credentials")
//! in exactly the run built for it, and the v0.1.93 independent read of the EUR wallet came back inconclusive.
//! [`demo_config`] answers from the active default row when there is one and from THE one fxcm demo row otherwise.

use std::path::PathBuf;
use std::time::Duration;

use vike_bridge_core::exec_actor::ExecutionClientActor;
use vike_execution::{EventSender, Ingest};
use vike_fxcm::{FxcmConfig, load_fxcm_config_from, spawn_execution_client};
use vike_model::AccountTier;
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_secrets::account_directory::AccountDirectory;
use vike_secrets::venue_setting::VenueSettings;

/// The venue's settings and `venue_url` address rows, read from the settings database in the directory the credential
/// read resolves. A box with no database or no `venue_url` table, and no settings directory at all, read the release's
/// addresses.
///
/// LOUD, never a quiet default: a database that will not read would otherwise hand a smoke the release's host instead
/// of the row the node dials.
pub fn fxcm_settings() -> VenueSettings {
    let settings_dir = std::env::var("VIKE_SETTINGS_DIR").ok();
    vike_paths::state_path::project_settings_dir_from(
        settings_dir.as_deref(),
        &std::env::current_dir().expect("a working directory"),
    )
    .map(|dir| {
        vike_secrets::venue_setting::load_venue_settings(&dir)
            .unwrap_or_else(|e| panic!("the settings database could not be read: {e}"))
    })
    .and_then(|mut all| all.remove("fxcm"))
    .unwrap_or_else(|| {
        VenueSettings::default().with_urls(vike_venues::venue_urls::VenueUrls::seed("fxcm"))
    })
}

/// The demo fxcm account's login from the smoke store: the active default row when there is one, else the one fxcm
/// demo row there is, active or not. `None` (the smoke skips) when there is no such login, and when SEVERAL unlabelled
/// demo rows are inactive: picking one would be a guess, and a smoke that trades must not guess an account.
pub fn demo_config() -> Option<FxcmConfig> {
    demo_config_from(&vike_secrets::smoke_directory(), &fxcm_settings())
}

/// [`demo_config`] over a directory the caller holds (the test's seam).
pub fn demo_config_from(dir: &AccountDirectory, settings: &VenueSettings) -> Option<FxcmConfig> {
    // A whole login with no Hosts.jsp row is LOUD, never a skip.
    let loud = |config: Option<Result<FxcmConfig, vike_venues::venue_urls::UrlRefusal>>| {
        config.map(|c| c.unwrap_or_else(|why| panic!("{why}")))
    };
    let active = dir.credentials_for("fxcm", &AccountLabel::Default, AccountTier::Demo);
    if let Some(config) = loud(load_fxcm_config_from(AccountTier::Demo, &active, settings)) {
        return Some(config);
    }
    let id = {
        let rows = dir.rows()?.ok()?.known()?;
        let mut demo = rows
            .iter()
            .filter(|r| r.venue == "fxcm" && r.tier == AccountTier::Demo && r.label.is_none());
        let first = demo.next()?.id;
        demo.next().is_none().then_some(first)?
    };
    let book = dir.credentials()?.ok()?;
    loud(load_fxcm_config_from(AccountTier::Demo, &book.view("fxcm", Some(id)), settings))
}

/// The HALT sentinel every smoke's client watches: a path this binary owns and never creates, so the client is wired
/// the way a start wires it (no `HALT KILL SWITCH IS NOT WIRED` line, which stays for a real client built without one)
/// and a demo order never depends on a kill switch lying around on the box (the twin of
/// `crates/bridges/ctrader/tests/ctrader_live_mount_smoke.rs`'s never-engaged sentinel).
pub const SMOKE_HALT_SENTINEL: &str = "fxcm-smoke-never-engaged/HALT";

/// The exec client a smoke trades through: [`spawn_execution_client`] handed [`SMOKE_HALT_SENTINEL`].
pub fn spawn_client(config: FxcmConfig, events: EventSender) -> ExecutionClientActor {
    spawn_execution_client(config, events).with_halt_path(PathBuf::from(SMOKE_HALT_SENTINEL))
}

/// Next venue event off the ingest lane within `secs`, or `None` on timeout (venue round trips are
/// seconds, not ms). `None` is how the caller self-skips a shim-less box (no SDK staged) or a failed login.
pub fn next_event(rx: &mut tokio::sync::mpsc::Receiver<Ingest>, secs: u64) -> Option<Event> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    match rt.block_on(async { tokio::time::timeout(Duration::from_secs(secs), rx.recv()).await }) {
        Ok(Some(Ingest::Event(event))) => Some(event),
        Ok(Some(other)) => panic!("expected Ingest::Event, got {other:?}"),
        Ok(None) => panic!("ingest channel closed"),
        Err(_) => None, // timeout
    }
}
