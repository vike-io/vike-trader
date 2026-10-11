use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

use vike_exec::recon::FakeReconClient;

use crate::venue_mount_fixture::{MountFixture, PlantedMount};

/// A stub implementation, used only to prove the shape compiles as a registry row.
static NOTHING: PlantedMount =
    PlantedMount::new("binance", Resolution::Paper(PaperCause::NoLiveArm));

/// THE OBJECT-SAFETY PIN: a `'static` table of `&dyn VenueMount` rows is what `vike-tradehub`'s
/// registry is, so it must compile here first.
#[test]
fn the_contract_is_object_safe_and_a_static_table_of_rows_compiles() {
    static ROWS: &[&dyn VenueMount] = &[&NOTHING];
    assert_eq!(ROWS[0].venue(), "binance");
    let fx = MountFixture::new(&[]);
    assert_eq!(ROWS[0].resolve(&fx.inputs(true)), Resolution::Paper(PaperCause::NoLiveArm));
    assert!(ROWS[0].server_time_ms(&fx.inputs(true), Duration::from_millis(1)).is_err());
    assert!(ROWS[0].credential_probe(&fx.inputs(true)).is_none());
}

#[test]
fn a_paper_outcome_carries_no_recon_and_no_identity() {
    let out = MountOutcome::paper();
    assert!(matches!(out.exec, ExecOutcome::Paper));
    assert!(out.recon.is_none() && out.identity.is_none());
}

fn counting_build(count: &AtomicUsize) -> Option<Box<dyn ReconClient>> {
    count.fetch_add(1, Ordering::SeqCst);
    None
}

/// `recon_if_enabled`'s whole property: the factory is never CALLED while reconciliation is off.
#[test]
fn recon_if_enabled_is_lazy_when_disabled() {
    let count = AtomicUsize::new(0);
    assert!(recon_if_enabled(false, || counting_build(&count)).is_none());
    assert_eq!(count.load(Ordering::SeqCst), 0, "a disabled gate must not call the factory");
    let _ = recon_if_enabled(true, || counting_build(&count));
    assert_eq!(count.load(Ordering::SeqCst), 1, "an enabled gate calls it exactly once");
}

#[test]
fn the_missing_field_message_names_the_field() {
    assert_eq!(missing_time_field("time"), "time missing from the server-time response");
}

/// The workspace's existing offline `ReconClient` double — never fetched from here; these tests
/// assert on WHETHER a client was constructed, never on what it returns.
fn stub() -> Box<dyn ReconClient> {
    Box::new(FakeReconClient::default())
}

/// GATE ON — the other direction, the one that keeps laziness from becoming a silent
/// reconciliation outage: the factory runs EXACTLY once and its result is passed through
/// untouched.
#[test]
fn recon_if_enabled_builds_exactly_once_when_enabled() {
    let calls = AtomicUsize::new(0);
    let out = recon_if_enabled(true, || {
        calls.fetch_add(1, Ordering::SeqCst);
        Some(stub())
    });
    assert_eq!(calls.load(Ordering::SeqCst), 1, "enabled ⇒ built once, not zero and not twice");
    assert!(out.is_some(), "enabled ⇒ the factory's client is returned verbatim");
}

/// …and an ENABLED gate does not paper over a factory that fails: a venue whose handshake
/// returns `None` (unreachable / bad creds) still resolves `None` — the gate short-circuits, it
/// never substitutes.
#[test]
fn recon_if_enabled_passes_through_a_failed_build() {
    let calls = AtomicUsize::new(0);
    let out = recon_if_enabled(true, || {
        calls.fetch_add(1, Ordering::SeqCst);
        None
    });
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(out.is_none(), "a failed handshake stays reconcile-inert, exec unaffected");
}
