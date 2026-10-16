//! **The per-account start fan-out**, and above all the one property the whole change is judged on:
//! *a box with no labelled account behaves EXACTLY as it did before labelled accounts existed* —
//! for an account that RUNS. Since decision 0156 an account runs at the tier its active row states
//! or not at all, so "the box before labelled accounts" is a box whose DEFAULT account's row states
//! its tier; one whose table names no account runs nothing, and says so.
//!
//! That claim is not argued here, it is measured — engine COUNT, the engine's `route_key` (which is
//! the `LIVE-<route_key>.lock` sentinel filename and the `live_venues` key), and the arming rows
//! `vike-backend venues` renders — over the WHOLE roster rather than over a chosen venue, so a venue
//! that behaves differently cannot hide behind one that does not.
//!
//! # What these tests can and cannot reach
//!
//! Every case runs with an account at `paper` or NOT RUNNING (no row, an inactive row, two
//! conflicting tiers, no credentials), which is what keeps them network-free:
//! `make_engine_accounts` starts a live client the moment an active row's tier and a credential set
//! agree, and that would dial a venue. The ARMING half — which account is active, which is refused
//! and why — is pure (`venue_account_arming` opens no socket), so the selection logic is tested at
//! full strength and only the client construction is left to the live smokes.
//!
//! ⚠ The shared-BOOK rule's own unit tests live in `crates/vike-config/tests/shared_book_table.rs`
//! (the predicate) and in `crates/vike-gatewayhub/tests/trade_roster/shared_book_report.rs` (the report over real
//! arming rows). What is here is the FAN-OUT: how many engines come out, named how, and which
//! accounts do not run, with why.
//!
//! Moved from `crates/vike-tradehub-core/tests/` when the venue-adapter contract finished
//! (docs/decisions/0096): it drives `vike-tradehub-core`'s public fold with real venue ids, which only a
//! crate holding the registry can — `vike-tradehub` since the 2026-09-29 amendment. Default build
//! only: `vike-tradehub-core`'s registry carried ibkr, fxcm and polymarket `FeatureAbsent` in every build
//! and these assertions were written against that; each venue's feature-on half is its own
//! `crates/vike-gatewayhub/tests/trade_roster/ibkr_adapter.rs`, `crates/vike-gatewayhub/tests/trade_roster/fxcm_adapter.rs` or
//! `crates/vike-gatewayhub/tests/trade_roster/polymarket_adapter.rs`. That crate-level `#![cfg]` also makes this its
//! own test binary rather than a `daemon` member.
//! (W15: this crate has no venue features any more; the scripted table fixes ibkr, fxcm and polymarket
//! absent, as the default gatewayhub build answers, so these assertions run in every build.)

#[path = "support/account_book.rs"]
mod account_book;

// The in-process backend the roster tests drive the fold through (wave 2, W9a).
#[path = "support/local_backend.rs"]
mod local;

use std::collections::{HashMap, HashSet};

use vike_config::ArmingBlock;
use vike_model::AccountTier;
use vike_model::VENUES;
use vike_model::accounts::account_keys::AccountLabel;
use vike_tradehub_core::AdapterPolicy;

use account_book::{book, with_book, with_keyed_account, with_keyed_row};

fn label(text: &str) -> AccountLabel {
    AccountLabel::parse(text).unwrap_or_else(|e| panic!("{text} must be a legal label: {e}"))
}

/// One INACTIVE `account` row of `venue`, labelled `text`, at `tier`, with the caller's id — the row
/// `AdapterPolicy::with_account` cannot state.
fn inactive_row(id: i64, venue: &str, text: &str, tier: AccountTier) -> vike_secrets::Account {
    vike_secrets::Account {
        id,
        venue: venue.to_string(),
        tier,
        label: Some(text.to_string()),
        venue_account_id: None,
        parent_id: None,
        active: false,
        last_verified_at: None,
        max_exposure: None,
    }
}

/// A bybit account's arming fields — every one a value the store holds against ONE account row.
const BYBIT_KEYS: &[(&str, &str)] = &[("API_KEY", "k"), ("API_SECRET", "s")];

/// A policy whose table holds ONE active row: `venue`'s DEFAULT account at `paper`.
fn paper_default(venue: &str) -> AdapterPolicy {
    AdapterPolicy::default().with_account(venue, &AccountLabel::Default, AccountTier::Paper)
}

/// One fan-out of `venue` on `symbol` under `policy` — the credentials are the ones `policy` files
/// against each account's row, and the deployment map `vars` carries none.
fn start_adapter(
    venue: &str,
    symbol: &str,
    vars: &HashMap<String, String>,
    policy: &AdapterPolicy,
    live_venues: &mut HashSet<String>,
) -> vike_tradehub_core::AccountEngines {
    let (events, _rx) = vike_exec::event_channel(16);

    let rig = crate::local::Rig::new(vars, Some(policy));
    let mut env = rig.env(&events, live_venues);
    env.policy = Some(policy);
    vike_tradehub_core::make_engine_accounts(
        &mut env,
        venue,
        // The one-entry map — byte-identically the single `symbol` parameter this used to take.
        &[(AccountLabel::Default, symbol.to_string())],
        &[],
    )
    .unwrap_or_else(|e| panic!("{venue}: an offline fan-out refuses nothing: {e}"))
}

/// The accounts a fan-out did NOT run, each with its reason.
fn stopped(mounted: &vike_tradehub_core::AccountEngines) -> Vec<(AccountLabel, ArmingBlock)> {
    mounted.not_running.iter().map(|nr| (nr.account.clone(), nr.block)).collect()
}

/// **THE unchanged-box property, over the whole roster.** One account at `paper`, one engine, and
/// the engine is addressed by the bare venue id.
///
/// `route_key` is the load-bearing half: `vike_boot::live_lock::LiveLock::acquire` builds
/// `LIVE-<route_key>.lock` from it, so any deviation here renames the sentinel file of every
/// deployment that already holds one — a running process would stop excluding its own double-launch.
///
/// And decision 0156's half: the SAME box with no row runs nothing on any venue — the default
/// account is NOT RUNNING, `NoAccountRow`, and no paper engine stands in for it.
#[test]
fn a_default_account_box_starts_one_engine_per_venue_addressed_by_the_bare_venue_id() {
    let vars = HashMap::new();
    for venue in VENUES {
        let mut live = HashSet::new();
        let mounted = start_adapter(venue, "BTCUSDT", &vars, &paper_default(venue), &mut live);
        assert_eq!(mounted.running.len(), 1, "{venue}: a box with one account mounts one engine");
        assert!(mounted.not_running.is_empty(), "{venue}: {:?}", stopped(&mounted));
        let (label, (engine, recon)) = &mounted.running[0];
        assert!(label.is_default(), "{venue}: and it is the DEFAULT account");
        assert_eq!(engine.route_key, *venue, "{venue}: the sentinel filename must not move");
        assert_eq!(engine.route_key, engine.venue, "{venue}: route key and venue stay equal");
        assert_eq!(engine.tier, AccountTier::Paper, "{venue}: at the tier its row states");
        assert!(recon.is_none(), "{venue}: a paper mount builds no reconcile client");
        assert!(live.is_empty(), "{venue}: a paper mount records no live arm");

        let mut live = HashSet::new();
        let none = start_adapter(venue, "BTCUSDT", &vars, &AdapterPolicy::default(), &mut live);
        assert!(none.running.is_empty(), "{venue}: no row, no engine");
        assert_eq!(
            stopped(&none),
            [(AccountLabel::Default, ArmingBlock::NoAccountRow)],
            "{venue}: the default account no row names does not run, and says why"
        );
        assert!(live.is_empty());
    }
}

/// A venue `vike_model::VENUES` does not carry — a sim or test id — starts its one account at the
/// tier its row states, and runs nothing without one.
///
/// It is a separate path (`venue_account_arming` can build no row for a non-roster venue, because
/// `VenueArming::venue` is a `&'static str` taken from the roster itself), and it is the path every
/// in-workspace test venue takes.
#[test]
fn a_non_roster_venue_starts_its_one_account_at_its_rows_tier() {
    let mut live = HashSet::new();
    let mounted =
        start_adapter("sim", "BTCUSDT", &HashMap::new(), &paper_default("sim"), &mut live);
    assert_eq!(mounted.running.len(), 1);
    assert!(mounted.running[0].0.is_default());
    assert_eq!(mounted.running[0].1.0.route_key, "sim");
    assert!(live.is_empty());

    let none =
        start_adapter("sim", "BTCUSDT", &HashMap::new(), &AdapterPolicy::default(), &mut live);
    assert!(none.running.is_empty(), "no row, no engine");
    assert_eq!(stopped(&none), [(AccountLabel::Default, ArmingBlock::NoAccountRow)]);
}

/// **An `account` row naming an account the STORE holds no keys for builds nothing.** The extra
/// row appears in the projection (with the cause), and NEITHER account runs: with no credentials,
/// the default account's `demo` row cannot arm either (decision 0156 — no paper engine for it).
///
/// This is the shape of an operator's first attempt — the row added before the credential — and it
/// must not produce a half-mounted second book.
#[test]
fn an_armed_account_with_no_credentials_adds_a_row_and_no_engine() {
    let alt = label("ALT");
    let policy = AdapterPolicy::default()
        .with_account("bybit", &AccountLabel::Default, AccountTier::Demo)
        .with_account("bybit", &alt, AccountTier::Demo);
    let vars = HashMap::new();

    let rows = crate::local::rows("bybit", &vars, Some(&policy));
    assert_eq!(rows.len(), 2, "the table names an account, so it gets a row: {rows:?}");
    assert_eq!(rows[1].label, alt);
    assert_eq!(rows[1].tier, Some(AccountTier::Demo), "the row states ALT's tier");
    assert_eq!(rows[1].effective, None, "and it does not run");
    assert_eq!(rows[1].block, ArmingBlock::NoCredentials, "the cause is the absent key set");
    assert_eq!(rows[1].account_ids.len(), 1, "the row names the account that stated the tier");

    let mut live = HashSet::new();
    let mounted = start_adapter("bybit", "BTCUSDT", &vars, &policy, &mut live);
    assert!(mounted.running.is_empty(), "no credentials: NO engine, for either account");
    assert_eq!(
        stopped(&mounted),
        [(AccountLabel::Default, ArmingBlock::NoCredentials), (alt, ArmingBlock::NoCredentials)]
    );
    assert!(live.is_empty());
}

/// **Credentials no `account` row owns arm nothing and name no account** — the asymmetry the
/// account table exists for, in the account model (decision 0156 item 6): a credential belongs to
/// its account ROW, so the default account's row reaches only the fields filed against IT, and a
/// set filed against no row of this table (here: against row 2, which the table does not carry)
/// is neither listed as an account nor read for the default one.
#[test]
fn credentials_no_account_row_owns_arm_nothing_and_name_no_account() {
    let policy = with_book(
        AdapterPolicy::default().with_account("bybit", &AccountLabel::Default, AccountTier::Demo),
        book("bybit", 2, BYBIT_KEYS),
    );
    let vars = HashMap::new();

    let rows = crate::local::rows("bybit", &vars, Some(&policy));
    assert_eq!(rows.len(), 1, "the account ROWS name the accounts, never a credential: {rows:?}");

    // …and the DEFAULT account is untouched by a set filed against another id: no credentials of
    // its own, so it is exactly the `NoCredentials` row an empty account has always produced.
    assert_eq!(rows[0].block, ArmingBlock::NoCredentials);
    assert_eq!(rows[0].route_key(), "bybit");

    let mut live = HashSet::new();
    let mounted = start_adapter("bybit", "BTCUSDT", &vars, &policy, &mut live);
    assert!(mounted.running.is_empty(), "a set no row owns arms nothing: NO engine");
    assert_eq!(stopped(&mounted), [(AccountLabel::Default, ArmingBlock::NoCredentials)]);
    assert!(live.is_empty(), "a set no row owns arms nothing");
}

/// **An INACTIVE row is the off switch for its account, and still lists it.** ALT holds its own
/// keys on an `account` row at `demo` — but the row is inactive, so the projection names the cause
/// (`AccountInactive`, not the `NoAccountRow` a missing row reads as) and the start builds no engine
/// for it.
#[test]
fn an_inactive_labelled_row_lists_the_account_and_arms_nothing() {
    let policy = with_keyed_row(
        AdapterPolicy::default().with_account("bybit", &AccountLabel::Default, AccountTier::Demo),
        inactive_row(9_001, "bybit", "ALT", AccountTier::Demo),
        BYBIT_KEYS,
    );
    let vars = HashMap::new();

    let rows = crate::local::rows("bybit", &vars, Some(&policy));
    assert_eq!(rows.len(), 2, "{rows:?}");
    let alt = &rows[1];
    assert_eq!(alt.label, label("ALT"));
    assert_eq!((alt.effective, alt.block), (None, ArmingBlock::AccountInactive));

    let mut live = HashSet::new();
    let mounted = start_adapter("bybit", "BTCUSDT", &vars, &policy, &mut live);
    assert!(mounted.running.is_empty(), "an inactive account produces NO engine");
    assert_eq!(
        stopped(&mounted),
        [
            (AccountLabel::Default, ArmingBlock::NoCredentials),
            (label("ALT"), ArmingBlock::AccountInactive),
        ],
        "the default account has no keys of its own, and ALT is switched off"
    );
    assert!(live.is_empty());
}

/// **Two ACTIVE non-paper tiers of one account do NOT RUN, with `TierConflict`** (decision 0119's
/// rule, the owner's D4; decision 0156): `demo` and `live` rows both active for one
/// `(venue, label)` are refused rather than resolved — there is no automatic pick, no paper engine
/// in its place, and the operator deactivates one.
///
/// Every one of the four rows holds its own arming set, so ONLY the conflict can stop either
/// account — and it does so before any credential is read, which is also what keeps this test
/// offline.
#[test]
fn two_active_tiers_of_one_label_do_not_run_with_tier_conflict() {
    let alt = label("ALT");
    let policy = [
        (AccountLabel::Default, AccountTier::Demo),
        (AccountLabel::Default, AccountTier::Live),
        (alt.clone(), AccountTier::Demo),
        (alt.clone(), AccountTier::Live),
    ]
    .into_iter()
    .fold(AdapterPolicy::default(), |p, (label, tier)| {
        with_keyed_account(p, "bybit", &label, tier, BYBIT_KEYS)
    });
    let vars = HashMap::new();
    // CONTROL: the key set alone arms bybit at either tier, so the stop below is the conflict's
    // doing and not a fixture that never armed anything.
    for tier in [AccountTier::Demo, AccountTier::Live] {
        let alone = with_keyed_account(
            AdapterPolicy::default(),
            "bybit",
            &AccountLabel::Default,
            tier,
            BYBIT_KEYS,
        );
        let armed = crate::local::rows("bybit", &vars, Some(&alone));
        assert_eq!(armed.len(), 1, "{armed:?}");
        assert_eq!(armed[0].effective, Some(tier), "{tier}");
    }

    let rows = crate::local::rows("bybit", &vars, Some(&policy));
    assert_eq!(rows.len(), 2, "{rows:?}");
    for row in &rows {
        assert_eq!(
            (row.effective, row.block),
            (None, ArmingBlock::TierConflict),
            "{}: two active tiers are refused, never picked between",
            row.subject()
        );
    }

    let mut live = HashSet::new();
    let mounted = start_adapter("bybit", "BTCUSDT", &vars, &policy, &mut live);
    assert!(mounted.running.is_empty(), "neither account runs, and no paper engine stands in");
    assert_eq!(
        stopped(&mounted),
        [(AccountLabel::Default, ArmingBlock::TierConflict), (alt, ArmingBlock::TierConflict)]
    );
    assert!(live.is_empty(), "a tier conflict arms NOTHING, on either network");
}

/// **A VENUE-scoped tier conflicts over EVERY label** (decision 0140): ibkr rides ONE session per
/// box, so an unlabelled ibkr row at `demo` and a labelled one at `live` stop BOTH accounts with
/// `TierConflict`, and each row names both ids. CONTROL: the same shape on bybit — an
/// account-scoped venue — still arms both accounts, each at its own tier (decision 0119).
///
/// ibkr is `FeatureAbsent` in this default build, so without the venue scope its two rows would
/// read `FeatureAbsent` (each at its own non-paper tier); `TierConflict` here can only be the
/// venue-wide read, which answers before any bridge is asked.
#[test]
fn a_venue_scoped_tier_conflicts_across_labels_and_an_account_scoped_one_does_not() {
    let alt = label("ALT");
    let ibkr = AdapterPolicy::default()
        .with_account("ibkr", &AccountLabel::Default, AccountTier::Demo)
        .with_account("ibkr", &alt, AccountTier::Live);
    let rows = crate::local::rows("ibkr", &HashMap::new(), Some(&ibkr));
    assert_eq!(rows.len(), 2, "{rows:?}");
    for row in &rows {
        assert_eq!(
            (row.tier, row.effective, row.block),
            (None, None, ArmingBlock::TierConflict),
            "{}: demo under one label and live under another is ONE ibkr session's conflict",
            row.subject()
        );
        assert_eq!(row.account_ids, vec![1, 2], "{}: the remedy names both rows", row.subject());
        assert!(row.why().contains("of any label"), "{}", row.why());
    }
    let mut live = HashSet::new();
    let mounted = start_adapter("ibkr", "AAPL.SMART.USD", &HashMap::new(), &ibkr, &mut live);
    assert!(mounted.running.is_empty(), "neither account runs");
    assert_eq!(
        stopped(&mounted),
        [
            (AccountLabel::Default, ArmingBlock::TierConflict),
            (alt.clone(), ArmingBlock::TierConflict),
        ]
    );
    assert!(live.is_empty(), "a venue-wide conflict arms NOTHING");

    // CONTROL — the same shape on an account-scoped venue arms each account at its own tier, each
    // on the key set filed against its own row.
    let bybit = with_keyed_account(
        with_keyed_account(
            AdapterPolicy::default(),
            "bybit",
            &AccountLabel::Default,
            AccountTier::Demo,
            BYBIT_KEYS,
        ),
        "bybit",
        &alt,
        AccountTier::Live,
        &[("API_KEY", "k2"), ("API_SECRET", "s2")],
    );
    let rows = crate::local::rows("bybit", &HashMap::new(), Some(&bybit));
    assert_eq!(rows.len(), 2, "{rows:?}");
    let got: Vec<_> =
        rows.iter().map(|r| (r.label.clone(), r.tier, r.effective, r.block)).collect();
    let (demo, live) = (Some(AccountTier::Demo), Some(AccountTier::Live));
    assert_eq!(
        got,
        vec![
            (AccountLabel::Default, demo, demo, ArmingBlock::None),
            (alt, live, live, ArmingBlock::None),
        ],
        "bybit judges each label alone: demo and live under two labels both arm"
    );
}

/// **`known_accounts` lists every labelled ROW of the venue**, active or not, and the DEFAULT
/// account is always first.
///
/// The account ROWS are the one source (decision 0156 item 6: a credential belongs to its row, so
/// no account is read out of a credential), and every row must be listed — ACTIVE OR NOT — or a
/// typo'd label vanishes instead of showing up as `NoCredentials`, and a deactivated account
/// vanishes instead of showing up as `AccountInactive`. A credential set filed against an id no
/// row of the table carries names nothing.
#[test]
fn known_accounts_lists_every_row_of_the_venue_default_first() {
    let policy = with_book(
        AdapterPolicy::default()
            .with_account("bybit", &label("ROWONLY"), AccountTier::Demo)
            .with_account_row(inactive_row(9_001, "bybit", "INACTIVEONLY", AccountTier::Live)),
        book("bybit", 4_242, BYBIT_KEYS),
    );

    let found = vike_config::account_arming::known_accounts("bybit", Some(&policy.accounts));
    assert_eq!(found.len(), 3, "{found:?}");
    assert!(found[0].is_default(), "the default account sorts FIRST, always");
    let names: Vec<&str> = found.iter().filter_map(AccountLabel::text).collect();
    assert_eq!(
        names,
        vec!["INACTIVEONLY", "ROWONLY"],
        "sorted, deduplicated, inactive rows included, and no account out of a credential"
    );

    // …and a venue no row mentions has exactly the one account it always had.
    assert_eq!(
        vike_config::account_arming::known_accounts("okx", Some(&policy.accounts)),
        vec![AccountLabel::Default]
    );
}

/// The ROUTE KEY rendering, pinned at both ends — this string is a lock FILENAME and a
/// `live_venues` key, so it is not free to change.
#[test]
fn the_route_key_is_the_bare_venue_for_the_default_account_and_suffixed_otherwise() {
    for venue in VENUES {
        let rows = crate::local::rows(venue, &HashMap::new(), None);
        assert_eq!(rows.len(), 1, "{venue}: no policy and no store names one account");
        assert_eq!(rows[0].route_key(), *venue);
        // …and the suffix form, built from the same renderer the row uses.
        let alt = vike_config::VenueArming { label: label("ALT"), ..rows[0].clone() };
        assert_eq!(alt.route_key(), format!("{venue}#ALT"));
        // A route key becomes a filename component: no separator may reach it.
        assert!(!alt.route_key().contains('/') && !alt.route_key().contains('\\'));
    }
}

/// **Every engine this start returns carries its ACCOUNT's route key — on the paper path too.**
///
/// ⚠ This test exists because a mutation survived without it. `make_engine_for_account` returns
/// early for a `paper`-tier account, through `paper_engine` → `ExecutionEngine::new`, which seeds
/// `route_key` equal to `venue`. That is the right answer for the DEFAULT account and the WRONG one
/// for any other: two engines sharing a route key make the second unreachable for every venue-tagged
/// payload, which is the two-books-in-one bug `ExecutionEngine::route_key` exists to prevent.
#[test]
fn every_returned_engine_carries_its_own_route_key_including_on_the_paper_path() {
    let (events, _rx) = vike_exec::event_channel(16);
    let mut live = HashSet::new();
    let alt = label("ALT");
    let vars = HashMap::new();
    // ALT's own row at `paper` — the early return.
    let policy = AdapterPolicy::default().with_account("bybit", &alt, AccountTier::Paper);
    let rig = crate::local::Rig::new(&vars, Some(&policy));
    let mut env = rig.env(&events, &mut live);
    env.policy = Some(&policy);
    let (engine, _recon) =
        vike_tradehub_core::make_engine_for_account(&mut env, "bybit", "BTCUSDT", &alt, &[])
            .unwrap_or_else(|e| panic!("a paper-tier account runs: {e}"));

    assert_eq!(engine.route_key, "bybit#ALT", "the paper path must not fall back to the venue id");
    assert_eq!(engine.venue, "bybit", "…while `venue` stays the roster id every caps table needs");
    assert_eq!(engine.tier, AccountTier::Paper);
    assert!(live.is_empty(), "a paper mount records no live arm");
}

/// …and a refusal is not merely a row: an account the store cannot single out is NOT ARMED, so no
/// engine is built on a login that might be another account's.
///
/// ⚠ **Driven over bybit.** Two ACTIVE unlabelled rows of one `(venue, tier)` answer ONE address, and
/// picking either would be a guess about which account signs — so the account does not run
/// (`NoCredentials`: no single row holds credentials for it) though each row holds a key set.
/// dukascopy is the one venue that does NOT decline this shape: its owner's two demo books resolve
/// to the lowest account id (`vike_secrets::account_directory::SEVERAL_BOOKS`;
/// `crates/vike-gatewayhub/tests/trade_roster/dukascopy_sidecar_holder.rs`'s
/// `two_unlabelled_demo_rows_run_the_lowest_id_with_its_own_login`).
#[test]
fn an_account_the_store_cannot_single_out_produces_no_live_engine() {
    let keys: &[(&str, &str)] = &[("API_KEY", "fake-key"), ("API_SECRET", "fake-secret")];
    let policy = with_keyed_account(
        with_keyed_account(
            AdapterPolicy::default(),
            "bybit",
            &AccountLabel::Default,
            AccountTier::Demo,
            keys,
        ),
        "bybit",
        &AccountLabel::Default,
        AccountTier::Demo,
        keys,
    );
    let rows = crate::local::rows("bybit", &HashMap::new(), Some(&policy));
    assert_eq!(rows.len(), 1, "two unlabelled rows are ONE account's address: {rows:?}");
    let row = &rows[0];
    assert_eq!(
        (row.effective, row.block),
        (None, ArmingBlock::NoCredentials),
        "two unlabelled `demo` rows cannot say which account the DEFAULT account is: {}",
        row.why()
    );
    let mut live = HashSet::new();
    let mounted = start_adapter("bybit", "BTCUSDT", &HashMap::new(), &policy, &mut live);
    assert!(mounted.running.is_empty(), "no engine, and none on either row's keys");
    assert_eq!(
        stopped(&mounted),
        [(AccountLabel::Default, ArmingBlock::NoCredentials)],
        "the default account does not run, and says why"
    );
    assert!(live.is_empty(), "an account the store cannot single out arms nothing");
}
