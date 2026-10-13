//! The B11 live-account lock covers the ARMED set, claimed before `build_node`.

use super::*;

// ===============================================================================================
// THE B11 LIVE-ACCOUNT LOCK COVERS THE ARMED SET (fix/locks-cover-the-armed-set)
//
// The defect these two tests are the gate for, measured on the CI box: the daemon claimed one
// `LIVE-<venue>.lock` per RUN-PROFILE mount while `vike_mount::build_node` armed nine authenticated
// exec sessions from the credential store. A second process with a different profile locked a
// venue the first never locked, saw no conflict, and both traded one account.
//
// Both drive the REAL [`super::live_mount_with`] — every venue's plan gate, the `data_only`
// withhold, the claims, `build_node` — and both stay OFFLINE, by the same device the
// arming-tier suite in `vike-mount` uses: NO operator risk budget is supplied, so the first
// venue the mount classifies as live-intent is refused by `vike_mount::require_live_risk_budget`
// BEFORE its exec client is constructed. That refusal is not incidental scaffolding here — it is
// the ORDERING DISCRIMINATOR the second test reads (see its doc).
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
fn deribit_only_mount() -> Vec<super::ResolvedMount> {
    vec![super::buy_hold_mount("deribit", false)]
}

/// **THE SET: the locks cover what ARMS, not what the profile mounts.**
///
/// A profile naming exactly ONE venue (deribit, keyless — it arms nothing) beside a credential
/// store that could arm THREE, of which the `account` table arms TWO. The claims must be `ig` and
/// `oanda` and nothing else:
///
/// * NOT `deribit` — the profile's own venue, and the ONLY venue the pre-fix loop over
///   `mount_venues` would have locked. Its absence is what makes this a regression test rather
///   than a coincidence: the two sets are disjoint by construction.
/// * NOT `binance` — credentials, but no account row behind them. Credentials alone never arm an
///   account, and a venue that cannot arm must not hold an account lock.
///
/// Asserted on the sentinel FILES, which is the only trace a claim leaves, and on the mount's own
/// refusal so the file assertion cannot be read against a run that never reached the claims.
#[test]
fn the_live_account_locks_cover_the_armed_set_not_the_profiles_mount_set() {
    // The DEPLOYMENT map carries no account's credential: those ride the policy's `account` rows.
    let vars = HashMap::new();
    let policy = ig_and_oanda_armed_policy();

    // ANTI-VACUITY, and it must FAIL rather than skip: without this the assertions below would
    // pass unchanged against a fixture whose credentials arm nothing at all.
    let armed = vike_mount::armed_live_venues(
        crate::registry::REGISTRY,
        crate::wired_markets::WIRED_MARKETS,
        &vars,
        &policy.mount_policy(),
    );
    assert_eq!(
        armed,
        vec!["ig".to_string(), "oanda".to_string()],
        "the fixture must arm exactly ig+oanda from these account rows, or this test proves nothing"
    );
    // ⚠ `armed` is ROUTE KEYS now, one per ACCOUNT. With one account per venue a route key IS the
    // venue id, which is why the two literals above still read as venue names.
    assert!(
        !armed.iter().any(|k| k == "deribit"),
        "the profile's own venue must arm NOTHING, or the two sets are not disjoint and the \
         pre-fix loop would have passed this test too"
    );

    let lock_dir = tempfile::tempdir().expect("a throwaway state dir for the B11 lock claims");
    // NO risk budget on purpose — see the section header. The mount refuses at the first
    // live-intent venue, before any exec client is constructed, so this stays offline.
    let err = super::seam_mount(
        deribit_only_mount(),
        None,
        &policy,
        vars,
        lock_dir.path(),
        &super::ProdFeedCtors,
    )
    .err()
    .unwrap_or_else(|| {
        panic!(
            "no operator risk budget ⇒ the first live-intent venue must refuse the mount; a \
             green mount here means nothing armed, and the sentinel assertions below would \
             prove nothing"
        )
    });
    assert!(
        err.contains("max_notional_per_order") || err.contains("max_total_exposure"),
        "the mount must have reached `build_node`'s live arms and refused there — got: {err}"
    );

    assert_eq!(
        sentinels(lock_dir.path()),
        vec!["LIVE-ig.lock".to_string(), "LIVE-oanda.lock".to_string()],
        "one sentinel per ARMED venue: not the profile's deribit, and not the row-less binance"
    );
}

/// **THE ORDER: the claims are made before any exec client exists — read off the ERROR, not a
/// comment.**
///
/// A second process already holds `LIVE-oanda.lock`. This mount arms ig+oanda and supplies NO risk
/// budget, so the two refusals it can produce are distinguishable and they are produced at
/// different points in the sequence:
///
/// * the LOCK refusal fires from [`super::live_mount_with`]'s safety-gate-#6 block, before
///   `vike_mount::build_node` is called at all;
/// * the RISK-BUDGET refusal fires from inside `vike_mount::make_engine_with_legs`, i.e. once the
///   mount is already walking its venue arms.
///
/// So the error's IDENTITY is the ordering assertion: move the claims below `build_node` — which
/// is exactly where `vike-app` had them, and the reason a refused lock there was no longer
/// side-effect-free (bybit/okx/aster each post `set_leverage` at startup) — and this test goes red
/// with the risk-budget message instead. A comment could not have caught that; this does.
#[test]
fn the_account_locks_are_claimed_before_build_node_walks_a_single_venue_arm() {
    let vars = HashMap::new();
    let policy = ig_and_oanda_armed_policy();
    let lock_dir = tempfile::tempdir().expect("a throwaway state dir for the B11 lock claims");

    // The OTHER live process, standing in for the accident this lock exists to refuse: a stale
    // unit, a second terminal, a fat GUI beside the daemon.
    let _held = vike_boot::live_lock::LiveLock::acquire(lock_dir.path(), "oanda")
        .expect("the first claim on a fresh dir wins");

    let err = super::seam_mount(
        deribit_only_mount(),
        None,
        &policy,
        vars,
        lock_dir.path(),
        &super::ProdFeedCtors,
    )
    .err()
    .unwrap_or_else(|| panic!("a held account lock must refuse the whole live mount"));

    assert!(
        err.contains("already trades the oanda account"),
        "the refusal must be the LOCK's, raised before `build_node` reached a venue arm — got: \
         {err}"
    );
    assert!(
        !err.contains("max_notional_per_order"),
        "a risk-budget refusal here means the claims were made AFTER the mount walked its venue \
         arms, i.e. after exec clients existed — got: {err}"
    );
}
