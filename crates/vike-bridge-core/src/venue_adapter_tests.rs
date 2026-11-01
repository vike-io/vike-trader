use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

use vike_exec::recon::FakeReconClient;

use crate::venue_adapter_fixture::{AdapterFixture, PlantedAdapter};

/// A stub implementation, used only to prove the shape compiles as a registry row.
static NOTHING: PlantedAdapter =
    PlantedAdapter::new("binance", Resolution::Paper(PaperCause::NoLiveArm));

/// THE OBJECT-SAFETY PIN: a `'static` table of `&dyn VenueAdapter` rows is what `vike-tradehub`'s
/// registry is, so it must compile here first.
#[test]
fn the_contract_is_object_safe_and_a_static_table_of_rows_compiles() {
    static ROWS: &[&dyn VenueAdapter] = &[&NOTHING];
    assert_eq!(ROWS[0].venue(), "binance");
    let fx = AdapterFixture::new(&[]);
    assert_eq!(
        ROWS[0].resolve(&fx.inputs(AccountTier::Live)),
        Resolution::Paper(PaperCause::NoLiveArm)
    );
    assert!(
        ROWS[0].server_time_ms(&fx.inputs(AccountTier::Live), Duration::from_millis(1)).is_err()
    );
    assert!(ROWS[0].credential_probe(&fx.inputs(AccountTier::Live)).is_none());
}

#[test]
fn a_paper_outcome_carries_no_recon_and_no_identity() {
    let out = AdapterOutcome::paper();
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

/// The one-key-set probe binance, bybit and okx answer through (their copies' whole matrix): a set
/// arms the tier the network names, nothing held below live; none is paper, `LiveCredentialsAbsent`
/// on mainnet. Red on a swapped cause, an inverted `bound_tier` or a held-below-live reason.
#[test]
fn a_one_key_set_arms_the_tier_its_network_names() {
    assert_eq!(bound_tier(true), AccountTier::Live);
    assert_eq!(bound_tier(false), AccountTier::Demo);
    let armed = |tier| Resolution::Armed { tier, held_below_live: None };
    assert_eq!(resolve_one_key_set(true, true), armed(AccountTier::Live));
    assert_eq!(resolve_one_key_set(true, false), armed(AccountTier::Demo));
    assert_eq!(
        resolve_one_key_set(false, true),
        Resolution::Paper(PaperCause::LiveCredentialsAbsent)
    );
    assert_eq!(resolve_one_key_set(false, false), Resolution::Paper(PaperCause::NoCredentials));
}

/// A demo-only arm (alpaca's sandbox, ig's demo gateway, ctrader's and dukascopy's tier gate) over
/// every tier: `Paper` reads as `Demo`, `Live` alone is refused, and with no config the tier is never
/// asked. Red on a `Paper => false`, a `Live => true`, or a resolution arm swapped.
#[test]
fn a_demo_only_arm_refuses_only_the_live_tier() {
    assert!(demo_arm(AccountTier::Demo) && demo_arm(AccountTier::Paper));
    assert!(!demo_arm(AccountTier::Live));
    let demo = Resolution::Armed {
        tier: AccountTier::Demo,
        held_below_live: Some(HeldBelowLive::DemoOnlyArm),
    };
    for tier in [AccountTier::Paper, AccountTier::Demo] {
        assert!(matches!(DemoOnlyArm::of(tier, Some(7)), DemoOnlyArm::Demo(7)), "{tier}");
        assert_eq!(DemoOnlyArm::of(tier, Some(7)).resolution(), demo, "{tier}");
    }
    let live = DemoOnlyArm::of(AccountTier::Live, Some(7));
    assert!(matches!(live, DemoOnlyArm::LiveTierNotWired));
    assert_eq!(live.resolution(), Resolution::Paper(PaperCause::LiveTierNotWired));
    for tier in AccountTier::ALL {
        let none = DemoOnlyArm::<u8>::of(tier, None);
        assert!(matches!(none, DemoOnlyArm::Unconfigured), "{tier}");
        assert_eq!(none.resolution(), Resolution::Paper(PaperCause::NoCredentials), "{tier}");
    }
}

/// A bridge that implements no read answers "not offered by <venue>" for EVERY kind, in the one
/// sentence `vike_model::not_offered_words` spells, and asks nothing of the venue.
#[test]
fn a_bridge_that_writes_no_read_is_not_offered_in_words() {
    let fx = AdapterFixture::new(&[]);
    for kind in [
        vike_model::VenueReadKind::Balance,
        vike_model::VenueReadKind::OpenOrders,
        vike_model::VenueReadKind::Positions,
    ] {
        let why = NOTHING
            .venue_read(&fx.inputs(AccountTier::Live), kind)
            .expect_err("the default offers nothing");
        assert_eq!(why, vike_model::not_offered_words("binance", kind));
        assert!(why.starts_with("not offered by binance"), "{why}");
    }
}
