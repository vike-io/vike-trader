//! The ready banner names the arming record, not the venue the profile mounts.

use super::oanda::{ScriptedOandaCtors, fake_oanda_store, fill_sized_budget, oanda_armed_policy};
use super::*;

/// **THE READY BANNER IS THE MOUNT'S RECORD, NOT THE PROFILE'S — over the REAL mount path.**
///
/// [`super::ready_mode_line`]'s own unit tests prove the RENDERING (a nine-venue record renders
/// nine venues, sorted). What they cannot reach is the value it is rendered FROM, and that is where
/// the defect lived: the banner was built from the daemon's `mount_venues`, the DISTINCT venues the
/// profile mounts a strategy on, while `vike_mount::build_node` arms a real exec client wherever the
/// CREDENTIAL STORE answers — a set the profile does not decide. MEASURED on the CI box, one startup:
/// `live_venues={…nine venues…}` under `"mode":"LIVE (venue=bybit)"`.
///
/// So this drives the REAL [`super::live_mount_with`] — the same call the daemon makes — with a
/// profile naming exactly ONE venue, and asserts the banner rendered from the returned ARMING
/// RECORD does not name it. The two sets genuinely differ here: the profile's is `{oanda}` and the
/// record is EMPTY (the `data_only` declaration withheld the store's keys from exec), so a banner
/// built from the profile says `LIVE (venue=oanda)` and a banner built from the record says
/// `LIVE (venue=none)`. Under the pre-fix binary this test reads the first.
///
/// ⚠ **Why the record is empty rather than the several-venue shape the CI box measured, and why that is
/// not a weakening.** The direction under test is "the banner follows the record, whatever the
/// profile says", and any DIFFERENCE between the two sets tests it. A several-venue record cannot
/// be produced hermetically: every arming branch in `vike_mount::make_engine_with_legs` performs
/// venue I/O on the way to its `live_venues.insert` — a blocking `SymbolProperties` pre-fetch
/// (bybit/okx/binance/alpaca/ibkr), a synchronous authed handshake (ctrader/ibkr/deribit), or an
/// exec actor that dials on spawn (alpaca/ig/oanda) — so "arm several venues" and "touch no network
/// with fake keys" are mutually exclusive in this process. The N-venue half is carried by
/// `the_banner_names_every_armed_venue_not_the_one_the_profile_mounts`, over the the CI box record
/// verbatim; the two together cover both halves of the claim.
#[test]
fn the_ready_banner_names_the_arming_record_not_the_venue_the_profile_mounts() {
    // The PRECONDITION, keyed on the profile — not on the renderer, and not on the record the
    // assertions below are about. If a future edit made this profile mount some other venue (or no
    // venue), the test's whole premise would be gone and it must FAIL saying so, never pass quietly
    // because the string it was hunting for happened to be absent.
    let mount = super::buy_hold_mount("oanda", true);
    let cfg = &mount.cfg;
    assert_eq!(cfg.venue, "oanda", "this test's premise is a profile that mounts exactly oanda");
    assert!(
        vike_model::VENUES.contains(&cfg.venue.as_str()),
        "the profile's venue must be a real roster id, or `LIVE (venue=oanda)` was never a shape \
         the pre-fix banner could print and this test proves nothing"
    );
    let profile_venue = cfg.venue.clone();
    let mounts = vec![mount];

    // FAKE keys in a plain map — never real ones, never the real store. Present so the venue's plan
    // gate passes and the mount reaches `build_node` at all; the `data_only` declaration is what
    // then keeps them from exec.
    let vars = fake_oanda_store("fake-banner-token");

    let subs: Arc<Mutex<Vec<(&'static str, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let got_creds: Arc<Mutex<Option<(String, String)>>> = Arc::new(Mutex::new(None));
    let ctors = ScriptedOandaCtors { subs, got_creds };

    let lock_dir = tempfile::tempdir().expect("a throwaway state dir for the B11 lock claims");
    let (handle, teardown, live_venues, _live_locks) = super::seam_mount(
        mounts,
        Some(fill_sized_budget()),
        &oanda_armed_policy(),
        vars,
        lock_dir.path(),
        &ctors,
    )
    .expect("the live mount stands up — no real store, no network");

    // The two sets DIFFER — the whole premise. Asserted before the banner so a build in which they
    // happened to coincide fails here, naming the reason, instead of passing the banner assert for
    // the wrong reason.
    assert!(
        !live_venues.contains(&profile_venue),
        "premise gone: build_node armed the profile's own venue ({profile_venue}), so this mount \
         can no longer tell a banner built from the record apart from one built from the profile"
    );

    // THE BANNER, rendered exactly as `main` renders it, from the mount's own record.
    let mode = super::ready_mode_line(
        true,
        &live_venues,
        // The store is irrelevant to THIS test — it is about the arming record — so the arm an
        // absent store lands in, which renders exactly the pre-parameter string.
        &vike_bridge_core::credentials::StoreHealth::Readable,
    );
    assert!(
        !mode.contains(&profile_venue),
        "the ready banner named the venue the PROFILE mounts rather than what ARMED: {mode:?} \
         (build_node's arming record was {live_venues:?})"
    );
    assert_eq!(
        mode, "LIVE (venue=none)",
        "nothing armed, so the banner must say so — record {live_venues:?}"
    );

    super::tear_down(handle, teardown);
}
