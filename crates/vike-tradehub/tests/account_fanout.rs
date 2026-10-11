//! **The per-account mount fan-out**, and above all the one property the whole change is judged on:
//! *a box with no labelled account behaves EXACTLY as it did before labelled accounts existed.*
//!
//! That claim is not argued here, it is measured — engine COUNT, the engine's `route_key` (which is
//! the `LIVE-<route_key>.lock` sentinel filename and the `live_venues` key), and the arming rows
//! `vike-backend venues` renders — over the WHOLE roster rather than over a chosen venue, so a venue
//! that behaves differently cannot hide behind one that does not.
//!
//! # What these tests can and cannot reach
//!
//! Every case runs with an account held at paper (no row, a `paper` row, an inactive row, two
//! conflicting tiers) or with no credentials, which is what keeps them network-free:
//! `make_engine_accounts` mounts a live client the moment an active row's tier and a credential set
//! agree, and that would dial a venue. The ARMING half — which account is active, which is refused
//! and why — is pure (`venue_account_arming` opens no socket), so the selection logic is tested at
//! full strength and only the client construction is left to the live smokes.
//!
//! ⚠ The shared-BOOK rule's own unit tests live in `crates/vike-config/tests/shared_book_table.rs`
//! (the predicate) and in `crates/vike-tradehub/tests/shared_book_report.rs` (the report over real
//! arming rows). What is here is the FAN-OUT: how many engines come out, named how.
//!
//! Moved from `crates/vike-mount/tests/` when the venue mount contract finished
//! (docs/decisions/0096): it drives `vike-mount`'s public fold with real venue ids, which only a
//! crate holding the registry can — `vike-tradehub` since the 2026-09-29 amendment. Default build
//! only: `vike-mount`'s registry carried ibkr, fxcm and polymarket `FeatureAbsent` in every build
//! and these assertions were written against that; each venue's feature-on half is its own
//! `crates/vike-tradehub/tests/ibkr_mount.rs`, `crates/vike-tradehub/tests/fxcm_mount.rs` or
//! `crates/vike-tradehub/tests/polymarket_mount.rs`. That crate-level `#![cfg]` also makes this its
//! own test binary rather than a `daemon` member.
#![cfg(not(any(feature = "ibkr", feature = "polymarket", feature = "fxcm")))]

use std::collections::{HashMap, HashSet};

use vike_config::{ArmingBlock, VenueMode};
use vike_model::VENUES;
use vike_model::accounts::account_keys::AccountLabel;
use vike_mount::MountPolicy;
use vike_tradehub::registry::REGISTRY;

fn label(text: &str) -> AccountLabel {
    AccountLabel::parse(text).unwrap_or_else(|e| panic!("{text} must be a legal label: {e}"))
}

/// One INACTIVE `account` row of `venue`, labelled `text`, at `tier`, with the caller's id — the row
/// `MountPolicy::with_account` cannot state.
fn inactive_row(id: i64, venue: &str, text: &str, tier: VenueMode) -> vike_secrets::Account {
    vike_secrets::Account {
        id,
        venue: venue.to_string(),
        tier: tier.as_str().to_string(),
        label: Some(text.to_string()),
        venue_account_id: None,
        parent_id: None,
        active: false,
        last_verified_at: None,
        max_exposure: None,
    }
}

/// A credential map from `(key, value)` pairs.
fn vars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
}

/// A mount with no credentials and a policy that names nothing — the state of a fresh install, and
/// the one every assertion about "unchanged" is made against.
fn mount(
    venue: &str,
    symbol: &str,
    vars: &HashMap<String, String>,
    policy: &MountPolicy,
    live_venues: &mut HashSet<String>,
) -> Vec<(AccountLabel, vike_mount::EngineAndRecon)> {
    let (events, _rx) = vike_exec::event_channel(16);

    let mut env = vike_mount::MountEnv::new(REGISTRY, vars, &events, live_venues);
    env.policy = Some(policy);
    vike_mount::make_engine_accounts(
        &mut env,
        venue,
        // The one-entry map — byte-identically the single `symbol` parameter this used to take.
        &[(AccountLabel::Default, symbol.to_string())],
        &[],
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
    let policy = MountPolicy::default();
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
    let mounted = mount("sim", "BTCUSDT", &HashMap::new(), &MountPolicy::default(), &mut live);
    assert_eq!(mounted.len(), 1);
    assert!(mounted[0].0.is_default());
    assert_eq!(mounted[0].1.0.route_key, "sim");
    assert!(live.is_empty());
}

/// **An `account` row naming an account the STORE holds no keys for changes nothing.** The extra
/// row appears in the projection (with the cause), and the MOUNT is still one engine.
///
/// This is the shape of an operator's first attempt — the row added before the credential — and it
/// must not produce a half-mounted second book.
#[test]
fn an_armed_account_with_no_credentials_adds_a_row_and_no_engine() {
    let alt = label("ALT");
    let policy = MountPolicy::default()
        .with_account("bybit", &AccountLabel::Default, VenueMode::Demo)
        .with_account("bybit", &alt, VenueMode::Demo);
    let vars = HashMap::new();

    let rows = vike_mount::venue_account_arming(REGISTRY, "bybit", &vars, Some(&policy));
    assert_eq!(rows.len(), 2, "the table names an account, so it gets a row: {rows:?}");
    assert_eq!(rows[1].label, alt);
    assert_eq!(rows[1].tier, VenueMode::Demo, "the row states ALT's tier");
    assert_eq!(rows[1].effective, VenueMode::Paper);
    assert_eq!(rows[1].block, ArmingBlock::NoCredentials, "the cause is the absent key set");
    assert_eq!(rows[1].account_ids.len(), 1, "the row names the account that stated the tier");

    let mut live = HashSet::new();
    let mounted = mount("bybit", "BTCUSDT", &vars, &policy, &mut live);
    assert_eq!(mounted.len(), 1, "an unarmed second account produces NO engine");
    assert!(mounted[0].0.is_default());
    assert!(live.is_empty());
}

/// **A labelled credential set with no `account` row of its own arms nothing** — the asymmetry the
/// account table exists for: the default account's row was written when it had one account, and it
/// says nothing about a second.
#[test]
fn a_labelled_credential_without_a_row_of_its_own_arms_nothing() {
    let policy =
        MountPolicy::default().with_account("bybit", &AccountLabel::Default, VenueMode::Demo);
    let vars = vars(&[("BYBIT_DEMO_API_KEY__ALT", "k"), ("BYBIT_DEMO_API_SECRET__ALT", "s")]);

    let rows = vike_mount::venue_account_arming(REGISTRY, "bybit", &vars, Some(&policy));
    assert_eq!(rows.len(), 2, "the STORE names an account, so it gets a row too: {rows:?}");
    let alt = &rows[1];
    assert_eq!(alt.label, label("ALT"));
    assert_eq!(alt.tier, VenueMode::Paper, "no row states a tier for ALT");
    assert_eq!(alt.effective, VenueMode::Paper);
    assert_eq!(alt.block, ArmingBlock::NoAccountRow);
    assert!(alt.account_ids.is_empty(), "no row, so no id to name");
    assert!(alt.why().contains("ALT"), "{}", alt.why());

    // …and the DEFAULT account is untouched by its neighbour's existence: no credentials of its
    // own, so it is exactly the `NoCredentials` row a store like this has always produced.
    assert_eq!(rows[0].block, ArmingBlock::NoCredentials);
    assert_eq!(rows[0].route_key(), "bybit");
}

/// **An INACTIVE row is the off switch for its account, and still lists it.** ALT holds its own
/// keys and an `account` row at `demo` — but the row is inactive, so the projection names the cause
/// (`AccountInactive`, not the `NoAccountRow` a missing row reads as) and the mount builds no engine
/// for it.
#[test]
fn an_inactive_labelled_row_lists_the_account_and_arms_nothing() {
    let policy = MountPolicy::default()
        .with_account("bybit", &AccountLabel::Default, VenueMode::Demo)
        .with_account_row(inactive_row(9_001, "bybit", "ALT", VenueMode::Demo));
    let vars = vars(&[("BYBIT_DEMO_API_KEY__ALT", "k"), ("BYBIT_DEMO_API_SECRET__ALT", "s")]);

    let rows = vike_mount::venue_account_arming(REGISTRY, "bybit", &vars, Some(&policy));
    assert_eq!(rows.len(), 2, "{rows:?}");
    let alt = &rows[1];
    assert_eq!(alt.label, label("ALT"));
    assert_eq!((alt.effective, alt.block), (VenueMode::Paper, ArmingBlock::AccountInactive));

    let mut live = HashSet::new();
    let mounted = mount("bybit", "BTCUSDT", &vars, &policy, &mut live);
    assert_eq!(mounted.len(), 1, "an inactive account produces NO engine");
    assert!(mounted[0].0.is_default());
    assert!(live.is_empty());
}

/// **Two ACTIVE non-paper tiers of one account mount PAPER, with `TierConflict`** (decision 0119's
/// rule, the owner's D4): `demo` and `live` rows both active for one `(venue, label)` are refused
/// rather than resolved — there is no automatic pick, and the operator deactivates one.
///
/// Both key sets are present for both accounts, so ONLY the conflict can hold either at paper — and
/// it does so before any credential is read, which is also what keeps this test offline.
#[test]
fn two_active_tiers_of_one_label_mount_paper_with_tier_conflict() {
    let alt = label("ALT");
    let policy = MountPolicy::default()
        .with_account("bybit", &AccountLabel::Default, VenueMode::Demo)
        .with_account("bybit", &AccountLabel::Default, VenueMode::Live)
        .with_account("bybit", &alt, VenueMode::Demo)
        .with_account("bybit", &alt, VenueMode::Live);
    let vars = vars(&[
        ("BYBIT_DEMO_API_KEY", "k"),
        ("BYBIT_DEMO_API_SECRET", "s"),
        ("BYBIT_LIVE_API_KEY", "k"),
        ("BYBIT_LIVE_API_SECRET", "s"),
        ("BYBIT_DEMO_API_KEY__ALT", "k"),
        ("BYBIT_DEMO_API_SECRET__ALT", "s"),
        ("BYBIT_LIVE_API_KEY__ALT", "k"),
        ("BYBIT_LIVE_API_SECRET__ALT", "s"),
    ]);
    // CONTROL: either key set alone arms bybit at its own tier, so the paper below is the
    // conflict's doing and not a fixture that never armed anything.
    for tier in [VenueMode::Demo, VenueMode::Live] {
        assert!(vike_mount::would_mount_live_under(REGISTRY, "bybit", &vars, tier), "{tier}");
    }

    let rows = vike_mount::venue_account_arming(REGISTRY, "bybit", &vars, Some(&policy));
    assert_eq!(rows.len(), 2, "{rows:?}");
    for row in &rows {
        assert_eq!(
            (row.effective, row.block),
            (VenueMode::Paper, ArmingBlock::TierConflict),
            "{}: two active tiers are refused, never picked between",
            row.subject()
        );
    }

    let mut live = HashSet::new();
    let mounted = mount("bybit", "BTCUSDT", &vars, &policy, &mut live);
    assert_eq!(mounted.len(), 1, "the DEFAULT account mounts PAPER, and ALT not at all");
    let (label, (engine, recon)) = &mounted[0];
    assert!(label.is_default());
    assert_eq!(engine.route_key, "bybit");
    assert!(recon.is_none(), "a paper mount builds no reconcile client");
    assert!(live.is_empty(), "a tier conflict arms NOTHING, on either network");
}

/// **`known_accounts` unions the two sources**, and the DEFAULT account is always first.
///
/// Both halves matter: an account with keys and no row must be listed (that is the row whose block
/// tells the operator how to arm it), and an account with an `account` row and no keys must be
/// listed — ACTIVE OR NOT — or a typo'd label vanishes instead of showing up as `NoCredentials`, and
/// a deactivated account vanishes instead of showing up as `AccountInactive`.
#[test]
fn known_accounts_unions_the_store_and_the_table_default_first() {
    let policy = MountPolicy::default()
        .with_account("bybit", &label("ROWONLY"), VenueMode::Demo)
        .with_account_row(inactive_row(9_001, "bybit", "INACTIVEONLY", VenueMode::Live));
    let vars = vars(&[("BYBIT_DEMO_API_KEY__STOREONLY", "k")]);

    let found = vike_mount::known_accounts("bybit", &vars, Some(&policy));
    assert_eq!(found.len(), 4, "{found:?}");
    assert!(found[0].is_default(), "the default account sorts FIRST, always");
    let names: Vec<&str> = found.iter().filter_map(AccountLabel::text).collect();
    assert_eq!(
        names,
        vec!["INACTIVEONLY", "ROWONLY", "STOREONLY"],
        "sorted, deduplicated, both sources, inactive rows included"
    );

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
        let rows = vike_mount::venue_account_arming(REGISTRY, venue, &HashMap::new(), None);
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
/// early for an account held at paper, through `paper_engine` → `ExecutionEngine::new`, which seeds
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
    let vars = HashMap::new();
    // No `account` row at all — every account paper, the early return.
    let policy = MountPolicy::default();
    let mut env = vike_mount::MountEnv::new(REGISTRY, &vars, &events, &mut live);
    env.policy = Some(&policy);
    let (engine, _recon) =
        vike_mount::make_engine_for_account(&mut env, "bybit", "BTCUSDT", &alt, &[])
            .expect("a paper mount refuses nothing");

    assert_eq!(engine.route_key, "bybit#ALT", "the paper path must not fall back to the venue id");
    assert_eq!(engine.venue, "bybit", "…while `venue` stays the roster id every caps table needs");
    assert!(live.is_empty(), "a paper mount records no live arm");
}

/// **A labelled account is REFUSED on every venue whose mount arm cannot address one** — and since
/// 2026-09-15 that is NO roster venue.
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
///
/// ⚠ **It is EMPTY, and an empty list is still the right shape.** `dukascopy` was its last entry
/// and left when its arm started addressing the settings database's `account` rows
/// (`vike_mount`'s `dukascopy` module then, `crates/bridges/dukascopy/src/mount.rs` since the
/// venue mount contract). A venue scaffolded by `just new-venue` declares
/// `addresses_accounts: false`, so the next venue to exist lands here and the test below fails
/// until somebody writes the argument — which is exactly what this list is for. Replacing it with
/// `assert!(no venue is refused)` would delete that.
const REFUSED: &[&str] = &[];

#[test]
fn a_labelled_account_is_refused_on_every_venue_whose_arm_cannot_address_one() {
    let alt = label("ALT");
    for venue in VENUES {
        let policy = MountPolicy::default()
            .with_account(venue, &AccountLabel::Default, VenueMode::Demo)
            .with_account(venue, &alt, VenueMode::Demo);
        let rows =
            vike_mount::venue_account_arming(REGISTRY, venue, &HashMap::new(), Some(&policy));
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

/// …and a refusal is not merely a row: a refused account is NOT MOUNTED, so no second engine is
/// built on the first account's keys.
///
/// ⚠ **Driven over dukascopy, whose refusal MOVED rather than went away.** This test used to assert
/// `ArmingBlock::NoAccountSupport`'s effect: the arm read one account's keys and a second account
/// could not be expressed at all. It now asserts `AccountNotInStore`'s: ALT has an active `demo` row
/// of its own, but the arm addresses dukascopy accounts through the row's credential-key OWNER
/// PREFIX, and this store holds no key names for any row — so nothing says WHICH Dukascopy broker
/// ALT is, and it is refused rather than coerced onto the default account's broker. The OUTCOME is
/// the one it always was: the default account mounts and nothing else does.
#[test]
fn a_labelled_account_of_a_store_that_cannot_name_it_produces_no_engine() {
    let alt = label("ALT");
    let policy = MountPolicy::default()
        .with_account("dukascopy", &AccountLabel::Default, VenueMode::Demo)
        .with_account("dukascopy", &alt, VenueMode::Demo);
    let rows =
        vike_mount::venue_account_arming(REGISTRY, "dukascopy", &HashMap::new(), Some(&policy));
    let row = rows.iter().find(|r| r.label == alt).expect("the table names ALT");
    assert_eq!(
        row.block,
        ArmingBlock::AccountNotInStore,
        "no key names say which broker ALT's row is, so the account cannot be identified: {}",
        row.why()
    );
    let mut live = HashSet::new();
    let mounted = mount("dukascopy", "EURUSD", &HashMap::new(), &policy, &mut live);
    assert_eq!(mounted.len(), 1, "the default account and nothing else");
    assert!(mounted[0].0.is_default());
    assert!(live.is_empty());
}
