//! **Every roster venue x every account tier x every key state: what the box does, pinned cell by cell.**
//!
//! The question this answers is the one the final review of the venue programs asked of the DEMO-pinned
//! arms (alpaca, ig, deribit, ibkr, ctrader, fxcm and oanda): *"does the venue still do not run where
//! it cannot honour the account's tier?"* — and, more generally, *"did any change to a cause or a log
//! line move a venue from paper to live, or the reverse?"* A cause name is cheap to change and a tier
//! is not, so this table pins BOTH halves of every cell and a change that touches one can be seen not
//! to have touched the other:
//!
//! * the **effective mode** — `Paper`, `Demo` or `Live`, i.e. *does this start reach a venue at all, and
//!   which one* — which is what the money-moving answer is, and
//! * the **block** — the cause `vike-backend venues` and the journal print for it.
//!
//! The three key states are the three an ACCOUNT can be in (decision 0156 item 6: a credential belongs
//! to its account row, and the row's tier is the account's only tier): it holds no fields, a HALF-written
//! set (a required field missing), or its FULL set. The account tiers are `demo` and `live`; a `paper`
//! tier is checked once per venue as `(Paper, PaperTier)` whatever the account holds, because
//! `vike-tradehub-core` answers it above every bridge and no key can change it.
//!
//! # Where the answers come from
//!
//! The projection under test is `vike_tradehub_core::venue_account_arming` over a policy whose one row is the
//! venue's DEFAULT account at the cell's tier holding the cell's fields — the SAME projection the start
//! selects with and `vike-backend venues` renders, and the one every bridge's `VenueAdapter::resolve` is
//! the inside of. (Over the policy, not the policy-less `venue_arming_under`: dukascopy resolves its
//! account ROW out of the `account` table, which only the policy carries.) Cells whose effective mode is
//! `Paper` are then ALSO run through a REAL `vike_tradehub_core::make_engine` under the same policy, which stays
//! offline for exactly those cells (a bridge that declines opens no socket), and must mark no venue live
//! and build no reconcile handle. A cell whose effective mode is armed is NOT started here — it would
//! dial the venue — so for those the projection is the pin, and each bridge's own `adapter_tests.rs` pins
//! its `start`.
//!
//! # The rounds, and what each moved
//!
//! 1. **`LiveTierNotWired`** renamed a CAUSE, never a tier: a live key set a demo-pinned arm holds but
//!    will not use stopped printing `NoCredentials`. [`RENAMED`] lists every cell that carries that
//!    cause, with the block it printed before it did, and
//!    [`the_renamed_cells_are_exactly_the_ones_that_changed`] holds the list against the table in both
//!    directions, so a cell cannot change its block without being named here.
//! 2. **The residuals of the live-tier cause** added log lines and one ordering rule (fxcm's: a live
//!    login is named first, `LiveTierNotWired` on every box, then the shim, then the demo login), and
//!    changed NO cell. fxcm is a `FeatureAbsent` row in the default build, so its cells live in
//!    `crates/vike-gatewayhub/tests/trade_roster/fxcm_adapter.rs` (feature `fxcm`), with its own `RENAMED` table.
//! 3. **An account trades at exactly its own tier (decision 0119).** The owner ruled that a `live`
//!    account must never trade demo, so every cell that armed the DEMO tier under `live` became paper.
//!    [`no_cell_arms_at_a_tier_its_row_did_not_name`] is the rule as a property: a cell is paper or
//!    exactly its tier.
//! 4. **A credential belongs to its account (decision 0156 item 6).** The key states stopped being
//!    TIER-named sets (`DEMO`-only, `LIVE`-only, both) and became the account's own set at the
//!    account's own tier, so a `live` account of a demo-only arm holding its set is the cell that says
//!    `LiveTierNotWired`, and the tier-named cells are gone with the names. Two blocks moved, both on
//!    paper cells: aster's `live` account holding nothing is `LiveCredentialsAbsent` (its network is its
//!    tier, as binance's, with no lower-tier fallback), and dukascopy's `live` account holding its login is
//!    `LiveTierNotWired` — listed in [`RENAMED`] with what it printed before. No tier moved.
//!
//! ⚠ **Default build only**, like every module of this binary (`crates/vike-gatewayhub/tests/trade_roster/mount_roster.rs`).
//! ibkr, fxcm and polymarket are `FeatureAbsent` rows here, which is itself pinned (a store cannot
//! arm them). Their feature-on halves are `ibkr_mount.rs`, `fxcm_mount.rs` and `polymarket_mount.rs`.

use std::collections::HashMap;

use vike_config::ArmingBlock as B;
use vike_gatewayhub::registry::trade::REGISTRY;
use vike_model::AccountTier as M;

use crate::account_book::keyed_policy;

/// One cell: what the projection answers.
type Cell = (Option<M>, B);

/// A fixture's `(field, value)` pairs.
type Pairs = &'static [(&'static str, &'static str)];

/// The three states one account can be in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Keys {
    /// No field at all — the ordinary unconfigured state.
    None,
    /// A set with a REQUIRED field missing (a typo'd or forgotten secret).
    Half,
    /// The venue loader's whole required set.
    Full,
}

/// In the order a row's arrays list them.
const STATES: [Keys; 3] = [Keys::None, Keys::Half, Keys::Full];

/// One roster venue: its account's fields per key state and the 6 cells (3 key states x the two
/// account tiers that reach a bridge).
struct Row {
    venue: &'static str,
    /// Every field the venue's loader requires — and, for cTrader, the VENUE's application pair,
    /// which `crate::account_book` files against the venue.
    full: Pairs,
    /// [`Self::full`] with a required field missing.
    half: Pairs,
    /// `[none, half, full]` for a `demo` account.
    at_demo: [Cell; 3],
    /// …and for a `live` account.
    at_live: [Cell; 3],
}

// ---- the vocabulary of cells -------------------------------------------------------------------

/// No usable key set — the original live gate.
const NC: Cell = (None, B::NoCredentials);
/// A `live` account the arm cannot honour at `live` for want of a usable set of its own. PAPER, never
/// a demo tier: a live account never trades demo (decision 0119).
const LCA: Cell = (None, B::LiveCredentialsAbsent);
/// The demo tier armed, with nothing refused (a `demo` account asked for exactly this).
const DEMO: Cell = (Some(M::Demo), B::None);
/// The live tier armed.
const LIVE: Cell = (Some(M::Live), B::None);
/// A `live` account holding its set on an arm that binds only its demo tier: does not run, and says
/// why.
const LTNW: Cell = (None, B::LiveTierNotWired);
/// A build without the venue's bridge.
const ABSENT: Cell = (None, B::FeatureAbsent);
/// What `just new-venue` scaffolds: a start whose `resolve` answers "no live arm" until it is written.
/// Referenced only by the scaffold's row template (the marker at the end of [`rows`]).
#[expect(dead_code)]
const NO_LIVE_ARM: Cell = (None, B::NoLiveArm);

/// A venue whose NETWORK is its account's tier (binance, bybit, okx, hyperliquid — decision 0095 —
/// and aster): the account's set arms whichever tier its row states, and a `live` account without a
/// usable set names the live tier it could not reach.
const NETWORK_IS_THE_TIER: ([Cell; 3], [Cell; 3]) = ([NC, NC, DEMO], [LCA, LCA, LIVE]);

/// A DEMO-pinned arm (deribit, oanda, ig, ctrader, alpaca, dukascopy): it has no live tier, so a `live`
/// account holding its set is named for what it is and does NOT RUN, and anything less is the ordinary
/// unconfigured state at either tier.
const DEMO_ONLY: ([Cell; 3], [Cell; 3]) = ([NC, NC, DEMO], [NC, NC, LTNW]);

/// The roster, in the order of `vike_model::VENUES`.
fn rows() -> Vec<Row> {
    vec![
        Row {
            venue: "binance",
            full: &[("API_KEY", "k"), ("API_SECRET", "s")],
            half: &[("API_KEY", "k")],
            at_demo: NETWORK_IS_THE_TIER.0,
            at_live: NETWORK_IS_THE_TIER.1,
        },
        Row {
            venue: "bybit",
            full: &[("API_KEY", "k"), ("API_SECRET", "s")],
            half: &[("API_KEY", "k")],
            at_demo: NETWORK_IS_THE_TIER.0,
            at_live: NETWORK_IS_THE_TIER.1,
        },
        Row {
            venue: "okx",
            full: &[("API_KEY", "k"), ("API_SECRET", "s"), ("API_PASSPHRASE", "p")],
            // The passphrase okx's signer requires on every request — the half-credential gate.
            half: &[("API_KEY", "k"), ("API_SECRET", "s")],
            at_demo: NETWORK_IS_THE_TIER.0,
            at_live: NETWORK_IS_THE_TIER.1,
        },
        Row {
            venue: "deribit",
            full: &[("API_KEY", "k"), ("API_SECRET", "s")],
            half: &[("API_KEY", "k")],
            at_demo: DEMO_ONLY.0,
            at_live: DEMO_ONLY.1,
        },
        Row {
            venue: "oanda",
            full: &[("API_KEY", "k"), ("ACCOUNT_ID", "101-004-1-001")],
            half: &[("API_KEY", "k")],
            at_demo: DEMO_ONLY.0,
            at_live: DEMO_ONLY.1,
        },
        Row {
            venue: "ig",
            full: &[("API_KEY", "k"), ("IDENTIFIER", "u"), ("PASSWORD", "p")],
            half: &[("API_KEY", "k")],
            at_demo: DEMO_ONLY.0,
            at_live: DEMO_ONLY.1,
        },
        Row {
            venue: "fxcm",
            full: &[("USER", "u"), ("PASSWORD", "p")],
            half: &[("USER", "u")],
            at_demo: [ABSENT; 3],
            at_live: [ABSENT; 3],
        },
        Row {
            venue: "dukascopy",
            full: &[("LOGIN", "u"), ("PASSWORD", "p")],
            half: &[("LOGIN", "u")],
            at_demo: DEMO_ONLY.0,
            at_live: DEMO_ONLY.1,
        },
        Row {
            venue: "polymarket",
            // Polymarket runs no testnet: its one key is the mainnet wallet's.
            full: &[("PRIVATE_KEY", "0xkey")],
            half: &[("ADDRESS", "0xfunder")],
            at_demo: [ABSENT; 3],
            at_live: [ABSENT; 3],
        },
        Row {
            venue: "ibkr",
            full: &[("ACCOUNT", "DU1234567")],
            half: &[("CLIENT_ID", "7")],
            at_demo: [ABSENT; 3],
            at_live: [ABSENT; 3],
        },
        Row {
            venue: "ctrader",
            // The app registration pair is the VENUE's, filed once for every account; the grant is
            // the account's.
            full: &[
                ("CLIENT_ID", "cid"),
                ("CLIENT_SECRET", "csecret"),
                ("ACCESS_TOKEN", "at"),
                ("REFRESH_TOKEN", "rt"),
            ],
            half: &[("CLIENT_ID", "cid"), ("CLIENT_SECRET", "csecret"), ("ACCESS_TOKEN", "at")],
            at_demo: DEMO_ONLY.0,
            at_live: DEMO_ONLY.1,
        },
        Row {
            venue: "alpaca",
            full: &[("CLIENT_ID", "cid"), ("CLIENT_SECRET", "cs"), ("ACCOUNT_ID", "acct")],
            half: &[("CLIENT_ID", "cid"), ("CLIENT_SECRET", "cs")],
            at_demo: DEMO_ONLY.0,
            at_live: DEMO_ONLY.1,
        },
        Row {
            venue: "aster",
            full: &[("USER", "0xUser"), ("PRIVATE_KEY", "0xkey")],
            half: &[("USER", "0xUser")],
            // No switch and no fallback: the account's tier is the network, a `demo` account binds
            // testnet and a `live` one mainnet, each with its own pair (decision 0119).
            at_demo: NETWORK_IS_THE_TIER.0,
            at_live: NETWORK_IS_THE_TIER.1,
        },
        Row {
            venue: "hyperliquid",
            full: &[("PRIVATE_KEY", "0xkey")],
            // The master address alone (agent-wallet mode's optional field): no key to sign with.
            half: &[("ACCOUNT_ADDRESS", "0xmaster")],
            at_demo: NETWORK_IS_THE_TIER.0,
            at_live: NETWORK_IS_THE_TIER.1,
        },
        // vike:new-venue:row // TODO(new-venue: {venue}): a scaffolded venue answers `NoLiveArm` at every tier and key state,
        // vike:new-venue:row // which is what this row pins. Replace the cells (and the fixtures) with the arm's real ones once its `resolve` is written.
        // vike:new-venue:row Row {
        // vike:new-venue:row     venue: "{venue}",
        // vike:new-venue:row     full: &[],
        // vike:new-venue:row     half: &[],
        // vike:new-venue:row     at_demo: [NO_LIVE_ARM; 3],
        // vike:new-venue:row     at_live: [NO_LIVE_ARM; 3],
        // vike:new-venue:row },
    ]
}

/// The fields a cell's account holds.
fn store(row: &Row, state: Keys) -> Pairs {
    match state {
        Keys::None => &[],
        Keys::Half => row.half,
        Keys::Full => row.full,
    }
}

/// The DEFAULT account's `(effective, block)` from the projection, for an account of `venue` at
/// `tier` holding `fields`.
fn projected(venue: &str, tier: M, fields: &[(&str, &str)]) -> Cell {
    let policy = keyed_policy(venue, tier, fields);
    let rows = crate::local::rows(REGISTRY, venue, &HashMap::new(), Some(&policy));
    let row = rows.iter().find(|r| r.is_default_account()).expect("the default account's row");
    (row.effective, row.block)
}

/// Every cell that carries `LiveTierNotWired`, as `(venue, tier, key state, the block it printed
/// before it carried that cause)` — every other cell's block is unchanged by that cause.
///
/// No venue list is written here, because the set is DERIVED: it is exactly the cells the table
/// above gives [`LTNW`] — the `Full` state of every row built from [`DEMO_ONLY`] under a `live`
/// account. [`the_renamed_cells_are_exactly_the_ones_that_changed`] holds this list against those
/// cells in both directions, so a venue joins or leaves it only by its table row changing. ibkr and
/// fxcm are feature-on, hence not rows of this default-build table; they carry their own in
/// `ibkr_mount.rs` and `fxcm_mount.rs`. No cell here changes its effective mode: a cause moved, a
/// tier did not.
///
/// The five original demo-pinned arms printed `NoCredentials` there before the cause existed;
/// dukascopy, whose accounts carried no tier-named key, printed `LiveCredentialsAbsent` for a `live`
/// account holding its login until decision 0156 item 6 made the login the account's own.
const RENAMED: &[(&str, M, Keys, B)] = &[
    ("deribit", M::Live, Keys::Full, B::NoCredentials),
    ("oanda", M::Live, Keys::Full, B::NoCredentials),
    ("ig", M::Live, Keys::Full, B::NoCredentials),
    ("dukascopy", M::Live, Keys::Full, B::LiveCredentialsAbsent),
    ("ctrader", M::Live, Keys::Full, B::NoCredentials),
    ("alpaca", M::Live, Keys::Full, B::NoCredentials),
];

/// Every cell of every row, as `(venue, tier, state, expected)`.
fn cells(rows: &[Row]) -> Vec<(&'static str, M, Keys, Cell)> {
    let mut out = Vec::new();
    for row in rows {
        for (i, state) in STATES.iter().enumerate() {
            out.push((row.venue, M::Demo, *state, row.at_demo[i]));
            out.push((row.venue, M::Live, *state, row.at_live[i]));
        }
    }
    out
}

/// The matrix has exactly one row per roster venue — a venue joining `vike_model::VENUES` reddens
/// this until its cells are written down.
#[test]
fn every_roster_venue_has_exactly_one_row() {
    let rows = rows();
    for v in vike_model::VENUES {
        assert_eq!(
            rows.iter().filter(|r| r.venue == *v).count(),
            1,
            "{v}: the stay-paper matrix needs exactly one row for every roster venue"
        );
    }
    for r in &rows {
        assert!(vike_model::VENUES.contains(&r.venue), "{}: not a roster venue", r.venue);
    }
}

/// **THE MATRIX.** Every cell's `(effective, block)` against the real projection over the real
/// registry. All mismatches are collected so one run prints the whole table's disagreement.
#[test]
fn the_projection_answers_every_cell_as_pinned() {
    let rows = rows();
    let mut wrong = Vec::new();
    for row in &rows {
        for state in STATES {
            assert_eq!(
                projected(row.venue, M::Paper, store(row, state)),
                (Some(M::Paper), B::PaperTier),
                "{} {state:?}: a `paper` tier answers above every bridge and no key moves it",
                row.venue
            );
        }
    }
    for (venue, tier, state, expected) in cells(&rows) {
        let row = rows.iter().find(|r| r.venue == venue).expect("a row");
        let got = projected(venue, tier, store(row, state));
        if got != expected {
            wrong.push(format!(
                "{venue:<12} tier={tier:<5} keys={state:?}: expected {expected:?}, got {got:?}"
            ));
        }
    }
    assert!(wrong.is_empty(), "the matrix disagrees with the projection:\n{}", wrong.join("\n"));
}

/// **A cell that does not run does not run at the START, not only in the projection.** A cell whose
/// effective tier is `None` is run through the real `make_engine`: it is NOT RUNNING with the
/// cell's own block (decision 0156 — no paper engine stands in), marks no venue live, and does so
/// offline — a bridge that declines opens no socket. (Armed cells are not started: they would dial
/// the venue. The projection is their pin.)
#[test]
fn every_cell_that_does_not_run_starts_nothing_offline() {
    // The symbol is irrelevant on a path that dials nothing.
    const SYMBOL: &str = "BTCUSDT";
    let rows = rows();
    let (tx, _rx) = vike_exec::event_channel(16);
    let no_vars = HashMap::new();
    let mut mounted = 0usize;
    for (venue, tier, state, (effective, block)) in cells(&rows) {
        if effective.is_some() {
            continue;
        }
        let row = rows.iter().find(|r| r.venue == venue).expect("a row");
        let mut live = std::collections::HashSet::new();
        let policy = keyed_policy(venue, tier, store(row, state));
        let rig = crate::local::Rig::new(REGISTRY, &no_vars, Some(&policy));
        let mut env = rig.env(&tx, &mut live);
        env.recon_enabled = true;
        env.policy = Some(&policy);
        match vike_tradehub_core::make_engine(&mut env, venue, SYMBOL) {
            Err(vike_tradehub_core::EngineError::NotRunning(stopped)) => assert_eq!(
                (stopped.tier, stopped.block),
                (Some(tier), block),
                "{venue} {tier} {state:?}: the mount's reason is the cell's"
            ),
            Err(other) => panic!("{venue} {tier} {state:?}: expected NOT RUNNING, got: {other}"),
            Ok((engine, _)) => panic!(
                "{venue} {tier} {state:?}: the cell does not run, the mount built an engine at {}",
                engine.tier
            ),
        }
        assert!(
            live.is_empty(),
            "{venue} {tier} {state:?}: the cell does not run, the mount marked it live"
        );
        mounted += 1;
    }
    assert!(mounted >= 40, "only {mounted} stopped cells were mounted — the walk is broken");
}

/// **No cell arms at a tier its row did not name** — it does not run, or runs at exactly the
/// account's tier: never above it, and never the demo fallback below `live` (decision 0119, the owner's no-downgrade
/// rule). The property the whole table exists to protect, asserted on the table itself so a typo
/// in a row cannot make a pin that blesses an escalation or a downgrade.
#[test]
fn no_cell_arms_at_a_tier_its_row_did_not_name() {
    for (venue, tier, state, (effective, _)) in cells(&rows()) {
        assert!(
            effective.is_none() || effective == Some(tier),
            "{venue} {state:?}: effective {effective:?} for a `{tier}` account"
        );
    }
}

/// [`RENAMED`] is exactly the set of cells that carry `LiveTierNotWired`, and each names a cell that
/// is still paper and printed another PAPER cause before — a rename of a cause that also changed the
/// tier would not be a rename.
#[test]
fn the_renamed_cells_are_exactly_the_ones_that_changed() {
    let rows = rows();
    for (venue, tier, state, before) in RENAMED {
        let (_, _, _, (effective, block)) = cells(&rows)
            .into_iter()
            .find(|(v, c, s, _)| v == venue && c == tier && s == state)
            .unwrap_or_else(|| panic!("{venue} {tier} {state:?}: not a cell of the matrix"));
        assert_eq!(effective, None, "{venue} {tier} {state:?}: a renamed cause still does not run");
        assert_ne!(block, *before, "{venue} {tier} {state:?}: listed as renamed, but unchanged");
        assert_eq!(block, B::LiveTierNotWired, "{venue} {tier} {state:?}: the new cause");
        assert!(
            matches!(before, B::NoCredentials | B::LiveCredentialsAbsent),
            "{venue} {tier} {state:?}: what it printed before must be a paper cause, got {before:?}"
        );
    }
    // …and the other direction: every cell that carries the new cause is listed, so a cell cannot
    // adopt it without a line here that says what it used to print.
    for (venue, tier, state, (_, block)) in cells(&rows) {
        if block == B::LiveTierNotWired {
            assert!(
                RENAMED.iter().any(|(v, c, s, _)| *v == venue && *c == tier && *s == state),
                "{venue} {tier} {state:?}: prints the new cause but is not listed in RENAMED"
            );
        }
    }
}
