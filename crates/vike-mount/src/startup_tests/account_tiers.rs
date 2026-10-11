//! The account's tier reaches the credential probe: the tier each account hands a bridge.

use super::*;

// ---- THE ACCOUNT'S TIER reaches the preflight: tested where the registry is ----
//
// The tests that drive ONE credential map arming a real spread of the roster and vary ONLY the
// tier are `crates/vike-tradehub/tests/mount_roster/startup.rs`'s, over its `credentialled`.
// The tier tests here drive PLANTED rows over an empty map.

/// `(venue, key set)` for every probe a [`tier_following_probe`] row built.
static HANDED_KEYS: Mutex<Vec<(&'static str, &'static str)>> = Mutex::new(Vec::new());

/// What [`HANDED_KEYS`] recorded for `venue` — each test plants its own venue name.
fn handed_keys_for(venue: &str) -> Vec<&'static str> {
    HANDED_KEYS
        .lock()
        .expect("the keys")
        .iter()
        .filter(|(v, _)| *v == venue)
        .map(|(_, k)| *k)
        .collect()
}

/// A probe binding the tier it is HANDED, like every CEX bridge's `credential_probe`:
/// `MountInputs::live_permitted` picks LIVE keys and `Tier::Live`, else DEMO. Each call records
/// its key set in [`HANDED_KEYS`], so a test reads what branch 4 HANDED the bridge.
fn tier_following_probe(
    venue: &'static str,
    reads: &'static AtomicUsize,
    inputs: &MountInputs<'_>,
) -> Option<vike_bridge_core::venue_mount::CredentialProbe> {
    let (keys, tier) =
        if inputs.live_permitted { ("LIVE", Tier::Live) } else { ("DEMO", Tier::Demo) };
    HANDED_KEYS.lock().expect("the keys").push((venue, keys));
    Some(vike_bridge_core::venue_mount::CredentialProbe::RecordsIdentity {
        client: Box::new(BalanceOnly { balance: Ok(Some(1.0)), reads }),
        bound_tier: tier,
    })
}

static DEMO_TIER_READS: AtomicUsize = AtomicUsize::new(0);
static DEMO_TIER: PlantedMount = PlantedMount {
    credential_probe: Some(|inputs| tier_following_probe("bybit", &DEMO_TIER_READS, inputs)),
    ..PlantedMount::new("bybit", ARMED_DEMO)
};
static DEMO_TIER_REG: [crate::VenueRow; 1] = [crate::VenueRow::Mount(&DEMO_TIER)];

/// Branch 4 for a `demo` account: a row ARMED below `live` is still probed, handed the DEMO tier,
/// recorded at DEMO. A branch 4 skipping rows below `live` would silently switch a demo box's
/// credential leg off (no probe = never checked = never demoted). Handing every row
/// `live_permitted = true` would send a demo box's signed balance read to the real-money account
/// with LIVE keys (the defect decision 0095 closed); the tier-following probe makes that visible.
///
/// Its `live` half (non-vacuity) is [`a_live_account_hands_the_probe_the_live_tier`]; the
/// PRESENCE half over the REAL registry is in `crates/vike-tradehub/tests/mount_roster/startup.rs`.
#[test]
fn an_armed_venue_is_still_probed_for_a_demo_account() {
    let demo = all_venues_at(VenueMode::Demo);
    assert!(
        crate::would_mount_live_under_policy(&DEMO_TIER_REG, "bybit", &HashMap::new(), Some(&demo)),
        "precondition: the planted row arms at DEMO for a `demo` account"
    );
    let probes = authed_read_probes_with(&DEMO_TIER_REG, &HashMap::new(), Some(&demo), capture);
    let probe = probes.get("bybit").expect("a row a `demo` account arms is probed");
    assert_eq!(
        handed_keys_for("bybit"),
        vec!["DEMO"],
        "a `demo` account must hand the bridge the DEMO tier, never the LIVE keys"
    );
    assert_eq!(probe(), Ok(()));
    assert_eq!(DEMO_TIER_READS.load(Ordering::SeqCst), 1, "the balance was read");
    assert_eq!(
        recorded_for("bybit"),
        vec![(
            "bybit".to_string(),
            vike_model::accounts::account_keys::AccountLabel::Default,
            VenueMode::Demo,
            true
        )],
        "recorded at the DEMO tier the credentials bind"
    );
}

static LIVE_TIER_READS: AtomicUsize = AtomicUsize::new(0);
static LIVE_TIER: PlantedMount = PlantedMount {
    credential_probe: Some(|inputs| tier_following_probe("binance", &LIVE_TIER_READS, inputs)),
    ..PlantedMount::new("binance", ARMED_LIVE)
};
static LIVE_TIER_REG: [crate::VenueRow; 1] = [crate::VenueRow::Mount(&LIVE_TIER)];

/// The other half of [`an_armed_venue_is_still_probed_for_a_demo_account`] (so a probe answering
/// DEMO whatever it is handed cannot pass it): for a `live` account the tier-following probe binds
/// and records LIVE.
#[test]
fn a_live_account_hands_the_probe_the_live_tier() {
    let probes =
        authed_read_probes_with(&LIVE_TIER_REG, &HashMap::new(), Some(&all_live()), capture);
    let probe = probes.get("binance").expect("a row a `live` account arms is probed");
    assert_eq!(handed_keys_for("binance"), vec!["LIVE"], "a `live` account hands the LIVE tier");
    assert_eq!(probe(), Ok(()));
    assert_eq!(LIVE_TIER_READS.load(Ordering::SeqCst), 1, "the balance was read");
    assert_eq!(
        recorded_for("binance"),
        vec![(
            "binance".to_string(),
            vike_model::accounts::account_keys::AccountLabel::Default,
            VenueMode::Live,
            true
        )],
        "recorded at the LIVE tier the credentials bind"
    );
}

static BOUND_LIVE_READS: AtomicUsize = AtomicUsize::new(0);
/// Arms below `live`, then offers a probe BOUND to LIVE whatever it is handed (a bridge ignoring
/// `MountInputs::live_permitted`).
static BOUND_LIVE: PlantedMount = PlantedMount {
    credential_probe: Some(|_| {
        Some(vike_bridge_core::venue_mount::CredentialProbe::RecordsIdentity {
            client: Box::new(BalanceOnly { balance: Ok(Some(1.0)), reads: &BOUND_LIVE_READS }),
            bound_tier: Tier::Live,
        })
    }),
    ..PlantedMount::new("oanda", ARMED_DEMO)
};
static BOUND_LIVE_REG: [crate::VenueRow; 1] = [crate::VenueRow::Mount(&BOUND_LIVE)];

/// The same LIVE-bound probe on a row that resolves LIVE — the control: a `live` account's probe.
static BOUND_LIVE_AT_LIVE_READS: AtomicUsize = AtomicUsize::new(0);
static BOUND_LIVE_AT_LIVE: PlantedMount = PlantedMount {
    credential_probe: Some(|_| {
        Some(vike_bridge_core::venue_mount::CredentialProbe::RecordsIdentity {
            client: Box::new(BalanceOnly {
                balance: Ok(Some(1.0)),
                reads: &BOUND_LIVE_AT_LIVE_READS,
            }),
            bound_tier: Tier::Live,
        })
    }),
    ..PlantedMount::new("ig", ARMED_LIVE)
};
static BOUND_LIVE_AT_LIVE_REG: [crate::VenueRow; 1] = [crate::VenueRow::Mount(&BOUND_LIVE_AT_LIVE)];

/// THE TIER INTERLOCK, startup-probe half: a LIVE-bound `RecordsIdentity` probe for a `demo`
/// account is never built — in the map it would be a signed read against the real-money account
/// the tier forbids. `resolve` answered DEMO, and the other halves watch `resolve` and `mount`,
/// neither reached by this leg. Nothing is mapped, read or recorded.
///
/// Control: the same probe on a row that resolves LIVE, for a `live` account, is probed, read
/// once, recorded at LIVE (the tier refused, not the probe's shape).
#[test]
fn a_probe_bound_to_the_live_tier_under_a_demo_tier_is_never_built() {
    let demo = all_venues_at(VenueMode::Demo);
    assert!(
        crate::would_mount_live_under_policy(
            &BOUND_LIVE_REG,
            "oanda",
            &HashMap::new(),
            Some(&demo)
        ),
        "precondition: the planted row arms below `live`, so the tier gate lets it through"
    );
    let probes = authed_read_probes_with(&BOUND_LIVE_REG, &HashMap::new(), Some(&demo), capture);
    assert!(!probes.contains_key("oanda"), "a LIVE-bound probe for a demo account is refused");
    assert_eq!(BOUND_LIVE_READS.load(Ordering::SeqCst), 0, "no balance was read");
    assert!(recorded_for("oanda").is_empty(), "nothing was recorded");

    let probes = authed_read_probes_with(
        &BOUND_LIVE_AT_LIVE_REG,
        &HashMap::new(),
        Some(&all_live()),
        capture,
    );
    let probe = probes.get("ig").expect("for a `live` account the same probe is built");
    assert_eq!(probe(), Ok(()));
    assert_eq!(BOUND_LIVE_AT_LIVE_READS.load(Ordering::SeqCst), 1, "the balance was read");
    assert_eq!(
        recorded_for("ig"),
        vec![(
            "ig".to_string(),
            vike_model::accounts::account_keys::AccountLabel::Default,
            VenueMode::Live,
            true
        )],
        "recorded at the LIVE tier the credentials bind"
    );
}

static BOUND_DEMO_AT_LIVE_READS: AtomicUsize = AtomicUsize::new(0);
/// Resolves LIVE, then offers a probe BOUND to DEMO whatever it is handed — the no-downgrade
/// rule's startup shape (a bridge whose probe falls back to its demo keys for a `live` account).
static BOUND_DEMO_AT_LIVE: PlantedMount = PlantedMount {
    credential_probe: Some(|_| {
        Some(vike_bridge_core::venue_mount::CredentialProbe::RecordsIdentity {
            client: Box::new(BalanceOnly {
                balance: Ok(Some(1.0)),
                reads: &BOUND_DEMO_AT_LIVE_READS,
            }),
            bound_tier: Tier::Demo,
        })
    }),
    ..PlantedMount::new("deribit", ARMED_LIVE)
};
static BOUND_DEMO_AT_LIVE_REG: [crate::VenueRow; 1] = [crate::VenueRow::Mount(&BOUND_DEMO_AT_LIVE)];

/// THE NO-DOWNGRADE RULE, startup-probe half: a DEMO-bound probe for a `live` account is never
/// built — a live account never signs, or records an identity, at the demo tier.
#[test]
fn a_probe_bound_to_the_demo_tier_for_a_live_account_is_never_built() {
    let probes = authed_read_probes_with(
        &BOUND_DEMO_AT_LIVE_REG,
        &HashMap::new(),
        Some(&all_live()),
        capture,
    );
    assert!(!probes.contains_key("deribit"), "a DEMO-bound probe for a live account is refused");
    assert_eq!(BOUND_DEMO_AT_LIVE_READS.load(Ordering::SeqCst), 0, "no balance was read");
    assert!(recorded_for("deribit").is_empty(), "nothing was recorded");
}
