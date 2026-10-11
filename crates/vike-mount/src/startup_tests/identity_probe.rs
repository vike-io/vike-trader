//! The identity-recording probe and branch 4's two contract-row probe shapes.

use super::*;

// ---- the identity-recording probe, and branch 4's two probe shapes ------------------------------

/// The probe records AFTER an answered balance read only (the balance alone is the verdict), at
/// the tier it was given, for the default account, with the client that answered.
#[test]
fn the_identity_probe_records_after_an_answered_balance_and_only_then() {
    static READS: AtomicUsize = AtomicUsize::new(0);
    let answered = identity_recording_probe(
        "identity-probe-answered".to_string(),
        Box::new(BalanceOnly { balance: Ok(Some(1.0)), reads: &READS }),
        VenueMode::Demo,
        vike_bridge_core::account_directory::AccountDirectory::default(),
        capture,
    );
    assert_eq!(answered(), Ok(()));
    assert_eq!(READS.load(Ordering::SeqCst), 1, "one balance read");
    assert_eq!(
        recorded_for("identity-probe-answered"),
        vec![(
            "identity-probe-answered".to_string(),
            vike_model::accounts::account_keys::AccountLabel::Default,
            VenueMode::Demo,
            true
        )]
    );

    static REFUSED_READS: AtomicUsize = AtomicUsize::new(0);
    let refused = identity_recording_probe(
        "identity-probe-refused".to_string(),
        Box::new(BalanceOnly { balance: Err("401 refused"), reads: &REFUSED_READS }),
        VenueMode::Demo,
        vike_bridge_core::account_directory::AccountDirectory::default(),
        capture,
    );
    assert_eq!(refused(), Err("401 refused".to_string()), "the balance read is the verdict");
    assert!(recorded_for("identity-probe-refused").is_empty(), "a refused key records nothing");
}

static BOUND_DEMO_READS: AtomicUsize = AtomicUsize::new(0);
static BOUND_DEMO: PlantedMount = PlantedMount {
    credential_probe: Some(|_| {
        Some(vike_bridge_core::venue_mount::CredentialProbe::RecordsIdentity {
            client: Box::new(BalanceOnly { balance: Ok(Some(1.0)), reads: &BOUND_DEMO_READS }),
            bound_tier: Tier::Demo,
        })
    }),
    ..PlantedMount::new("okx", ARMED_DEMO)
};
static BOUND_DEMO_REG: [crate::VenueRow; 1] = [crate::VenueRow::Mount(&BOUND_DEMO)];

/// Branch 4, `RecordsIdentity`: records at the tier the credentials BIND. The same DEMO row for a
/// `live` account is never probed at all: it resolves PAPER (the no-downgrade rule), so no record
/// can land on the account's LIVE row.
#[test]
fn a_contract_rows_identity_probe_records_at_its_bound_tier() {
    let probes =
        authed_read_probes_with(&BOUND_DEMO_REG, &HashMap::new(), Some(&all_live()), capture);
    assert!(!probes.contains_key("okx"), "a demo-only row is never probed for a live account");
    assert_eq!(BOUND_DEMO_READS.load(Ordering::SeqCst), 0, "no balance was read");

    let demo = all_venues_at(VenueMode::Demo);
    let probes = authed_read_probes_with(&BOUND_DEMO_REG, &HashMap::new(), Some(&demo), capture);
    let probe = probes.get("okx").expect("an armed contract row's probe is in the map");
    assert_eq!(probe(), Ok(()));
    assert_eq!(BOUND_DEMO_READS.load(Ordering::SeqCst), 1, "the balance was read");
    assert_eq!(
        recorded_for("okx"),
        vec![(
            "okx".to_string(),
            vike_model::accounts::account_keys::AccountLabel::Default,
            VenueMode::Demo,
            true
        )],
        "recorded at the BOUND tier"
    );
}

static READ_ONLY_CALLS: AtomicUsize = AtomicUsize::new(0);
static READ_ONLY: PlantedMount = PlantedMount {
    credential_probe: Some(|_| {
        Some(vike_bridge_core::venue_mount::CredentialProbe::ReadOnly(Arc::new(|| {
            READ_ONLY_CALLS.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })))
    }),
    ..PlantedMount::new("alpaca", ARMED_DEMO)
};
static READ_ONLY_REG: [crate::VenueRow; 1] = [crate::VenueRow::Mount(&READ_ONLY)];

/// Branch 4, `ReadOnly`: the bridge's own read goes into the map as is, nothing recorded, behind
/// the same tier gate.
#[test]
fn a_contract_rows_read_only_probe_is_the_bridges_own_read() {
    let demo = all_venues_at(VenueMode::Demo);
    let probes = authed_read_probes_with(&READ_ONLY_REG, &HashMap::new(), Some(&demo), capture);
    assert_eq!(probes.get("alpaca").expect("the bridge's own read")(), Ok(()));
    assert_eq!(READ_ONLY_CALLS.load(Ordering::SeqCst), 1, "the map holds the bridge's closure");
    assert!(recorded_for("alpaca").is_empty(), "a read-only probe records nothing");

    let capped = all_venues_at_except(VenueMode::Demo, "alpaca");
    assert!(
        !authed_read_probes(&READ_ONLY_REG, &HashMap::new(), Some(&capped)).contains_key("alpaca"),
        "a `paper` account yields no probe for a contract row either"
    );
}
