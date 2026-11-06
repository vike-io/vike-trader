//! The live mount over the venue backend: the ARMED set still decides, no lock is claimed here, and a lease another client holds refuses the whole mount.

use super::*;

// ===============================================================================================
// THE ARMED SET, AND WHO CLAIMS THE LOCK FOR IT (fix/locks-cover-the-armed-set; wave 2, W11)
//
// The defect the first test is the gate for, measured on the CI box: the daemon claimed one
// `LIVE-<venue>.lock` per RUN-PROFILE mount while `vike_tradehub_core::assemble_runtime` armed nine
// authenticated exec sessions from the credential store. A second process with a different profile
// locked a venue the first never locked, saw no conflict, and both traded one account. The claim has
// since moved to the process that owns the venue clients (the gatewayhub, at the trade lease), so
// what survives HERE is the armed set (it decides the reconcile gate) and the refusal of a lease
// another client holds.
//
// Both drive the REAL [`super::live_runtime_with`] — every venue's plan gate, the `data_only`
// withhold, `assemble_runtime` — and both stay OFFLINE, by the same device the arming-tier suite in
// `vike-tradehub-core` uses: NO operator risk budget is supplied to the first, so the first venue
// the mount classifies as live-intent is refused by `vike_tradehub_core::require_live_risk_budget`
// BEFORE its exec client is constructed.
// ===============================================================================================

/// The credentials of THREE venues none of which the profile below mounts, by FIELD on the
/// ACCOUNT ROW id they belong to (decision 0156 item 6): ig on row 1 and oanda on row 2 (the rows
/// [`ig_and_oanda_armed_policy`] plants, in that order) each hold a complete demo set, and
/// binance's pair sits under row 3, which NO row carries. Fake values throughout — never real
/// keys, never the real store.
fn three_armed_venues_creds() -> vike_secrets::account_credentials::CredentialBook {
    let mut book = vike_secrets::account_credentials::CredentialBook::new();
    for (field, value) in
        [("API_KEY", "fake-ig-key"), ("IDENTIFIER", "fake-ig-user"), ("PASSWORD", "fake-ig-pass")]
    {
        book.insert_account(1, field, value);
    }
    for (field, value) in [("API_KEY", "fake-oanda-token"), ("ACCOUNT_ID", "101-004-0000000-001")] {
        book.insert_account(2, field, value);
    }
    for (field, value) in [("API_KEY", "fake-binance-key"), ("API_SECRET", "fake-binance-secret")] {
        book.insert_account(3, field, value);
    }
    book
}

/// The `account` table for both tests: ig and oanda ARMED at `demo` (one ACTIVE row each), binance
/// with NO row — which mounts it `paper` (`NoAccountRow`), as every venue without an active
/// non-paper row is (decision 0119) — and [`three_armed_venues_creds`] as every account's fields.
///
/// Binance is the load-bearing venue, and it is the one with no row: its fields sit in the store
/// and it must still claim NO lock, because credentials with no active non-paper account row arm
/// nothing and claiming its account sentinel would refuse a legitimate second process over a venue
/// this deployment deliberately does not trade.
fn ig_and_oanda_armed_policy() -> super::SeamPolicy {
    super::SeamPolicy::armed(&[
        ("ig", vike_model::AccountTier::Demo),
        ("oanda", vike_model::AccountTier::Demo),
        // The profile's own venue runs on a `paper` row (decision 0156: a strategy mount on an account
        // no row names refuses the node) — which arms nothing live, so the sets stay disjoint.
        ("deribit", vike_model::AccountTier::Paper),
    ])
    .holding(three_armed_venues_creds())
}

/// A single-mount deribit profile — the KEYLESS venue, so the profile's own venue arms nothing and
/// the "profile set" and the "armed set" are DISJOINT, which is the premise both tests rest on.
fn deribit_only_mount() -> Vec<super::ResolvedAdapter> {
    vec![super::buy_hold_mount("deribit", false)]
}

/// **THE ARMED SET still decides, and THIS process claims no lock for it.**
///
/// A profile naming exactly ONE venue (deribit, keyless — it arms nothing) beside a credential
/// store that could arm THREE, of which the `account` table arms TWO. The armed set the mount
/// computes (it decides the reconcile gate, and is the list the gatewayhub claims its sentinels
/// from) must be `ig` and `oanda` and nothing else:
///
/// * NOT `deribit` — the profile's own venue; the two sets are disjoint by construction, which is
///   what makes this a regression test for "the set is the ARMED one, not the profile's".
/// * NOT `binance` — credentials, but no account row behind them.
///
/// And **no `LIVE-<venue>.lock` exists afterwards**: since wave 2 the gatewayhub claims the
/// account's sentinel when it answers the `TradeOpen` (`RemoteBackend::start_adapter` opens the
/// lease before it asks for the adapter), so this root writes none. Asserted on the sentinel FILES,
/// a claim's only trace, and on the mount's own refusal so the file assertion cannot be read against
/// a run that never reached the arms.
#[test]
fn the_armed_set_is_the_accounts_that_arm_and_this_process_claims_no_lock_for_it() {
    // The DEPLOYMENT map carries no account's credential: those ride the policy's `account` rows.
    let vars = HashMap::new();
    let policy = ig_and_oanda_armed_policy();

    // ANTI-VACUITY, and it must FAIL rather than skip: without this the assertions below would
    // pass unchanged against a fixture whose credentials arm nothing at all.
    let armed = crate::scripted_arming_tests::armed_live_venues(&vars, &policy.adapter_policy());
    assert_eq!(
        armed,
        vec!["ig".to_string(), "oanda".to_string()],
        "the fixture must arm exactly ig+oanda from these account rows, or this test proves nothing"
    );
    // ⚠ `armed` is ROUTE KEYS now, one per ACCOUNT. With one account per venue a route key IS the
    // venue id, which is why the two literals above still read as venue names.
    assert!(
        !armed.iter().any(|k| k == "deribit"),
        "the profile's own venue must arm NOTHING, or the two sets are not disjoint"
    );

    let lock_dir = tempfile::tempdir().expect("a throwaway state dir");
    // NO risk budget on purpose: the mount refuses at the first live-intent venue, before any exec
    // client is constructed, so this stays offline.
    let err = super::seam_live_runtime(
        deribit_only_mount(),
        None,
        &policy,
        vars,
        lock_dir.path(),
        &super::StubCtors { refusal: None },
    )
    .err()
    .unwrap_or_else(|| {
        panic!(
            "no operator risk budget ⇒ the first live-intent venue must refuse the mount; a \
             green mount here means nothing armed, and the sentinel assertion below would prove \
             nothing"
        )
    });
    assert!(
        err.contains("max_notional_per_order") || err.contains("max_total_exposure"),
        "the mount must have reached `assemble_runtime`'s live arms and refused there — got: {err}"
    );
    assert_eq!(
        sentinels(lock_dir.path()),
        Vec::<String>::new(),
        "the gatewayhub claims the account locks now; this root writes no sentinel"
    );
}

/// A backend source whose gatewayhub reports a lease another client holds: the in-process backend
/// for the mount itself, plus the refusal a `RemoteCtor` collects from `TradeOpenError::LeaseHeld`.
struct LeaseHeldCtor;

impl super::super::super::BackendCtor for LeaseHeldCtor {
    fn backend(
        &self,
        vars: &HashMap<String, String>,
        policy: &vike_tradehub_core::AdapterPolicy,
        data_only: &std::collections::HashSet<String>,
    ) -> std::sync::Arc<dyn vike_tradehub_core::VenueBackend> {
        super::super::super::BackendCtor::backend(&super::LocalCtor, vars, policy, data_only)
    }

    fn lease_refusals(&self) -> Vec<String> {
        vec![
            "oanda: the gatewayhub's trade lease for this account is held by nk-abc since 1".into(),
        ]
    }
}

/// **A lease another trade client holds refuses the WHOLE live mount**, as the live-account lock's
/// `AddrInUse` always did: two live processes on one venue account is the accident the lock exists
/// to refuse, so the refusal is the node's and not one account's. The mount here assembles (a paper
/// deribit account, nothing armed), so the refusal is the post-assembly check's, and the error names
/// the lease.
#[test]
fn a_lease_another_client_holds_refuses_the_whole_live_mount() {
    let policy = super::SeamPolicy::armed(&[("deribit", vike_model::AccountTier::Paper)]);
    let lock_dir = tempfile::tempdir().expect("a throwaway state dir");
    let err = super::seam_live_runtime_over(
        deribit_only_mount(),
        None,
        &policy,
        HashMap::new(),
        lock_dir.path(),
        &super::StubCtors { refusal: None },
        &LeaseHeldCtor,
    )
    .err()
    .unwrap_or_else(|| panic!("a held lease must refuse the whole live mount"));
    assert!(err.starts_with("refusing LIVE mount: "), "{err}");
    assert!(err.contains("held by nk-abc"), "the refusal names the holder: {err}");
}
