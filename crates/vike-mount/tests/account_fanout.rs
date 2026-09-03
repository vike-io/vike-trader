//! **The per-account mount fan-out**, and above all the one property the whole change is judged on:
//! *a box with no `[accounts]` table behaves EXACTLY as it did before it existed.*
//!
//! That claim is not argued here, it is measured — engine COUNT, the engine's `route_key` (which is
//! the `LIVE-<route_key>.lock` sentinel filename and the `live_venues` key), and the arming rows the
//! Data Manager renders — over the WHOLE roster rather than over a chosen venue, so a venue that
//! behaves differently cannot hide behind one that does not.
//!
//! # What these tests can and cannot reach
//!
//! Every case runs with a `paper` ceiling or with no credentials, which is what keeps them
//! network-free: `make_engine_accounts` mounts a live client the moment a ceiling and a credential
//! set agree, and that would dial a venue. The ARMING half — which account is active, which is
//! refused and why — is pure (`venue_account_arming` opens no socket), so the selection logic is
//! tested at full strength and only the client construction is left to the live smokes.
//!
//! ⚠ The shared-BOOK rule's own unit tests live in `crates/vike-config/tests/venue_accounts_table.rs`
//! (the predicate) and in `crates/vike-mount/src/lib.rs`'s own test module (the report over real
//! arming rows). What is here is the FAN-OUT: how many engines come out, named how.

use std::collections::{HashMap, HashSet};

use vike_config::{ArmingBlock, VenueMode, VenuePolicy};
use vike_model::account_keys::AccountLabel;
use vike_model::VENUES;

fn label(text: &str) -> AccountLabel {
    AccountLabel::parse(text).unwrap_or_else(|e| panic!("{text} must be a legal label: {e}"))
}

/// A mount with no credentials and a policy that names nothing — the state of a fresh install, and
/// the one every assertion about "unchanged" is made against.
fn mount(
    venue: &str,
    symbol: &str,
    vars: &HashMap<String, String>,
    policy: &VenuePolicy,
    live_venues: &mut HashSet<String>,
) -> Vec<(AccountLabel, vike_mount::EngineAndRecon)> {
    let (events, _rx) = vike_exec::event_channel(16);
    let mount_policy = vike_mount::MountPolicy { venues: policy.clone(), ..Default::default() };
    vike_mount::make_engine_accounts(
        venue,
        // The one-entry map — byte-identically the single `symbol` parameter this used to take.
        &[(AccountLabel::Default, symbol.to_string())],
        &[],
        vars,
        &events,
        live_venues,
        false,
        None,
        None,
        None,
        Some(&mount_policy),
    )
    .expect("a paper mount refuses nothing")
}

/// **THE unchanged-box property, over the whole roster.** One account, one engine, and the engine is
/// addressed by the bare venue id.
///
/// `route_key` is the load-bearing half: `vike_ops::live_lock::LiveLock::acquire` builds
/// `LIVE-<route_key>.lock` from it, so any deviation here renames the sentinel file of every
/// deployment that already holds one — a running process would stop excluding its own double-launch.
#[test]
fn a_default_account_box_mounts_one_engine_per_venue_addressed_by_the_bare_venue_id() {
    let vars = HashMap::new();
    let policy = VenuePolicy::default();
    for venue in VENUES {
        let mut live = HashSet::new();
        let mounted = mount(venue, "BTCUSDT", &vars, &policy, &mut live);
        assert_eq!(mounted.len(), 1, "{venue}: a box with one account mounts one engine");
        let (label, (engine, recon)) = &mounted[0];
        assert!(label.is_default(), "{venue}: and it is the DEFAULT account");
        assert_eq!(engine.route_key, *venue, "{venue}: the sentinel filename must not move");
        assert_eq!(engine.route_key, engine.venue, "{venue}: route key and venue stay equal");
        assert!(recon.is_none(), "{venue}: a paper mount builds no reconcile client");
        assert!(live.is_empty(), "{venue}: a paper mount records no live arm");
    }
}

/// A venue `vike_model::VENUES` does not carry — a sim or test id — still mounts exactly one engine.
///
/// It is a separate path (`venue_account_arming` can build no row for a non-roster venue, because
/// `VenueArming::venue` is a `&'static str` taken from the roster itself), and it is the path every
/// in-workspace test venue takes, so it must not become a zero-engine mount.
#[test]
fn a_non_roster_venue_still_mounts_its_one_account() {
    let mut live = HashSet::new();
    let mounted = mount("sim", "BTCUSDT", &HashMap::new(), &VenuePolicy::default(), &mut live);
    assert_eq!(mounted.len(), 1);
    assert!(mounted[0].0.is_default());
    assert_eq!(mounted[0].1 .0.route_key, "sim");
    assert!(live.is_empty());
}

/// **An `[accounts]` table naming an account the STORE does not hold changes nothing.** The extra
/// row appears on the screen (with the cause), and the MOUNT is still one engine.
///
/// This is the shape of an operator's first attempt — the ceiling written before the credential —
/// and it must not produce a half-mounted second book.
#[test]
fn an_armed_account_with_no_credentials_adds_a_row_and_no_engine() {
    let alt = label("ALT");
    let policy = VenuePolicy::default().declare("bybit", VenueMode::Demo).declare_account(
        "bybit",
        &alt,
        VenueMode::Demo,
    );
    let vars = HashMap::new();

    let rows = vike_mount::venue_account_arming("bybit", &vars, Some(&policy));
    assert_eq!(rows.len(), 2, "the table names an account, so it gets a row: {rows:?}");
    assert_eq!(rows[1].label, alt);
    assert_eq!(rows[1].effective, VenueMode::Paper);
    assert_eq!(rows[1].block, ArmingBlock::NoCredentials, "the cause is the absent key set");

    let mut live = HashSet::new();
    let mounted = mount("bybit", "BTCUSDT", &vars, &policy, &mut live);
    assert_eq!(mounted.len(), 1, "an unarmed second account produces NO engine");
    assert!(mounted[0].0.is_default());
    assert!(live.is_empty());
}

/// **A labelled credential set with no `[accounts]` line arms nothing** — the asymmetry the ceiling
/// exists for. The venue's own `live` line was written when it had one account.
#[test]
fn a_labelled_credential_without_a_line_of_its_own_arms_nothing() {
    let policy = VenuePolicy::default().declare("bybit", VenueMode::Demo);
    let vars: HashMap<String, String> =
        [("BYBIT_DEMO_API_KEY__ALT", "k"), ("BYBIT_DEMO_API_SECRET__ALT", "s")]
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();

    let rows = vike_mount::venue_account_arming("bybit", &vars, Some(&policy));
    assert_eq!(rows.len(), 2, "the STORE names an account, so it gets a row too: {rows:?}");
    let alt = &rows[1];
    assert_eq!(alt.label, label("ALT"));
    assert_eq!(alt.effective, VenueMode::Paper);
    assert_eq!(alt.block, ArmingBlock::AccountNotNamed);
    assert!(alt.why().contains("ALT"), "{}", alt.why());
    assert_eq!(alt.key(), "policy.accounts.bybit.ALT", "the row names the line to write");

    // …and the DEFAULT account is untouched by its neighbour's existence: no credentials of its
    // own, so it is exactly the `NoCredentials` row a store like this has always produced.
    assert_eq!(rows[0].block, ArmingBlock::NoCredentials);
    assert_eq!(rows[0].route_key(), "bybit");
}

/// **`known_accounts` unions the two sources**, and the DEFAULT account is always first.
///
/// Both halves matter: an account with keys and no line must be listed (that is the row whose block
/// tells the operator how to arm it), and an account named in the file with no keys must be listed
/// (or a typo'd label vanishes instead of showing up as `NoCredentials`).
#[test]
fn known_accounts_unions_the_store_and_the_table_default_first() {
    let policy =
        VenuePolicy::default().declare_account("bybit", &label("FILEONLY"), VenueMode::Demo);
    let vars: HashMap<String, String> = [("BYBIT_DEMO_API_KEY__STOREONLY", "k")]
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();

    let found = vike_mount::known_accounts("bybit", &vars, Some(&policy));
    assert_eq!(found.len(), 3, "{found:?}");
    assert!(found[0].is_default(), "the default account sorts FIRST, always");
    let names: Vec<&str> = found.iter().filter_map(AccountLabel::text).collect();
    assert_eq!(names, vec!["FILEONLY", "STOREONLY"], "sorted, deduplicated, both sources");

    // …and a venue neither source mentions has exactly the one account it always had.
    assert_eq!(
        vike_mount::known_accounts("okx", &vars, Some(&policy)),
        vec![AccountLabel::Default]
    );
}

/// The ROUTE KEY rendering, pinned at both ends — this string is a lock FILENAME and a
/// `live_venues` key, so it is not free to change.
#[test]
fn the_route_key_is_the_bare_venue_for_the_default_account_and_suffixed_otherwise() {
    for venue in VENUES {
        let rows = vike_mount::venue_account_arming(venue, &HashMap::new(), None);
        assert_eq!(rows.len(), 1, "{venue}: no policy and no store names one account");
        assert_eq!(rows[0].route_key(), *venue);
        // …and the suffix form, built from the same renderer the row uses.
        let alt = vike_config::VenueArming { label: label("ALT"), ..rows[0].clone() };
        assert_eq!(alt.route_key(), format!("{venue}#ALT"));
        // A route key becomes a filename component: no separator may reach it.
        assert!(!alt.route_key().contains('/') && !alt.route_key().contains('\\'));
    }
}

/// **Every engine this mount returns carries its ACCOUNT's route key — on the paper path too.**
///
/// ⚠ This test exists because a mutation survived without it. `make_engine_for_account` returns
/// early for a `paper` ceiling, through `paper_engine` → `ExecutionEngine::new`, which seeds
/// `route_key` equal to `venue`. That is the right answer for the DEFAULT account and the WRONG one
/// for any other: two engines sharing a route key make the second unreachable for every venue-tagged
/// payload, which is the two-books-in-one bug `ExecutionEngine::route_key` exists to prevent.
///
/// Nothing reaches that path with a labelled account through `make_engine_accounts` today (it mounts
/// no paper second account), so the hole was latent rather than live — which is exactly the kind a
/// gate has to hold shut, because the thing that opens it is a future caller, not this one.
#[test]
fn every_returned_engine_carries_its_own_route_key_including_on_the_paper_path() {
    let (events, _rx) = vike_exec::event_channel(16);
    let mut live = HashSet::new();
    let alt = label("ALT");
    let (engine, _recon) = vike_mount::make_engine_for_account(
        "bybit",
        "BTCUSDT",
        &alt,
        &[],
        &HashMap::new(),
        &events,
        &mut live,
        false,
        None,
        None,
        None,
        // `paper` for every venue — the ceiling that takes the early return.
        Some(&vike_mount::MountPolicy::default()),
    )
    .expect("a paper mount refuses nothing");

    assert_eq!(engine.route_key, "bybit#ALT", "the paper path must not fall back to the venue id");
    assert_eq!(engine.venue, "bybit", "…while `venue` stays the roster id every caps table needs");
    assert!(live.is_empty(), "a paper mount records no live arm");
}

/// **A labelled account is REFUSED on every venue whose mount arm cannot address one** — and that
/// is now exactly ONE venue.
///
/// A venue qualifies when its arm threads the account label into a loader that reads THAT account's
/// key names. An arm that still read UNLABELLED names would build a SECOND live client on the
/// DEFAULT account's keys — two engines on one venue account, the exact accident `route_key` and
/// `vike_ops::live_lock` exist to prevent.
///
/// Asserted over the WHOLE roster, in both directions, so the supported set cannot silently grow
/// *or shrink*. ⚠ The list below is the REFUSED half rather than the supported half, deliberately:
/// it is the short one, so an added venue that forgets its loader shows up as a test that has to be
/// edited, and the edit is the place the argument gets made.
const REFUSED: &[&str] = &[
    // No live arm in `make_engine_for_account` AT ALL (every build reaches the paper `_` arm), and
    // separately the one venue whose `DUKASCOPY_DEMO1_*` / `DEMO2` shape already carries an account
    // INDEX inside the tier token — `vike_model::account_keys` pins it as non-conforming ON
    // PURPOSE, and reconciling two multi-account spellings is a design question, not a port.
    "dukascopy",
];

#[test]
fn a_labelled_account_is_refused_on_every_venue_whose_arm_cannot_address_one() {
    let alt = label("ALT");
    for venue in VENUES {
        let policy = VenuePolicy::default().declare(venue, VenueMode::Demo).declare_account(
            venue,
            &alt,
            VenueMode::Demo,
        );
        let rows = vike_mount::venue_account_arming(venue, &HashMap::new(), Some(&policy));
        let row = rows.iter().find(|r| r.label == alt).expect("the table names ALT");
        assert_eq!(
            row.effective,
            VenueMode::Paper,
            "{venue}: no store, so nothing arms either way"
        );
        if REFUSED.contains(venue) {
            assert_eq!(
                row.block,
                ArmingBlock::NoAccountSupport,
                "{venue}'s arm reads unlabelled keys — a second account there would trade the \
                 FIRST account's credentials"
            );
            assert!(row.why().contains("ALT"), "{venue}: {}", row.why());
        } else {
            assert_ne!(
                row.block,
                ArmingBlock::NoAccountSupport,
                "{venue} threads the account label into its own loader, so a second account is \
                 expressible"
            );
        }
    }
}

/// …and the refusal is not merely a row: such an account is NOT MOUNTED, so no second engine is
/// built on the first account's keys.
#[test]
fn an_unsupported_venues_labelled_account_produces_no_engine() {
    let alt = label("ALT");
    let policy = VenuePolicy::default().declare("dukascopy", VenueMode::Demo).declare_account(
        "dukascopy",
        &alt,
        VenueMode::Demo,
    );
    let mut live = HashSet::new();
    let mounted = mount("dukascopy", "EURUSD", &HashMap::new(), &policy, &mut live);
    assert_eq!(mounted.len(), 1, "the default account and nothing else");
    assert!(mounted[0].0.is_default());
    assert!(live.is_empty());
}
