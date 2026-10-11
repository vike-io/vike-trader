//! **Every roster venue x every account tier x every key state: what the box does, pinned cell by cell.**
//!
//! The question this answers is the one the final review of the venue programs asked of the DEMO-pinned
//! arms (alpaca, ig, deribit, ibkr, ctrader, fxcm and oanda): *"on a store that holds only LIVE-tier
//! keys, does the venue still stay paper?"* — and, more generally, *"did any change to a cause or a
//! log line move a venue from paper to live, or the reverse?"* A cause name is cheap to change and a
//! tier is not, so this table pins BOTH halves of every cell and a change that touches one can be seen
//! not to have touched the other:
//!
//! * the **effective mode** — `Paper`, `Demo` or `Live`, i.e. *does this mount reach a venue at all, and
//!   which one* — which is what the money-moving answer is, and
//! * the **block** — the cause `vike-backend venues` and the journal print for it.
//!
//! The four key states are the four a store can be in for a venue with two tiers: no keys, DEMO keys
//! only, LIVE keys only, and both. The account tiers are `demo` and `live`; a `paper` tier is checked
//! once per venue as `(Paper, PaperTier)` whatever the store holds, because `vike-mount` answers it
//! above every bridge and no key can change it.
//!
//! # Where the answers come from
//!
//! The projection under test is `vike_mount::venue_arming_under` — the SAME function the mount's
//! pre-connect probe and `vike-backend venues` read, and the one every bridge's
//! `VenueMount::resolve` is the inside of. Cells whose effective mode is `Paper` are then ALSO run
//! through a REAL `vike_mount::make_engine`, which stays offline for exactly those cells
//! (a bridge that declines opens no socket), and must mark no venue live and build no reconcile
//! handle. A cell whose effective mode is armed is NOT mounted here — it would dial the venue — so for
//! those the projection is the pin, and each bridge's own `mount_tests.rs` pins its `mount`.
//!
//! # BEFORE and AFTER
//!
//! The effective-mode column was derived from the code as it stood on `main` before the
//! `LiveTierNotWired` change, by running this table against that code, and it has not moved: that
//! change renames a CAUSE, never a tier. [`RENAMED`] lists the only cells whose block differs between
//! the two trees, with the block `main` printed there — and [`the_renamed_cells_are_exactly_the_ones_that_changed`]
//! holds the list against the table in both directions, so a cell cannot change its block without
//! being named here.
//!
//! # The second round, and the one rule behind fxcm's row
//!
//! A later change (the residuals of the live-tier cause) added log lines and one ordering rule, and
//! changed NO cell of this table: a half-written live set prints `NoCredentials` here exactly as it
//! always did (the missing key names are in the daemon's log, as okx's missing passphrase is), a live
//! set beside a demo one still arms the demo tier, and a labelled account that is never mounted is
//! spoken for in the log without moving its cell.
//!
//! The ordering rule is fxcm's, and fxcm is a `FeatureAbsent` row in the default build, so its cells
//! live in `crates/vike-tradehub/tests/fxcm_mount.rs` (feature `fxcm`), whose own `RENAMED` table
//! lists the cells that changed in both directions. **The rule, one for the projection and the
//! log alike: a stand-alone LIVE login is named first (`LiveTierNotWired`, on every box), then the
//! shim (`SdkAbsent`), then the demo login.** Before it, `resolve` asked the shim first and `mount`
//! the credentials first, so a box without the ForexConnect shim holding only a live login printed
//! `SdkAbsent` here and `LiveTierNotWired` in the log. Both were true; now there is one answer. The
//! tier never moves: every cell that changed is paper before and after.
//!
//! # The third round: an account trades at exactly its own tier (decision 0119)
//!
//! The ceiling became the account row's tier, and the owner ruled that a `live` account must never
//! trade demo. This round DID move tiers, and only toward paper: every cell that armed the DEMO
//! tier under `live` — the demo-pinned arms' held demo session (`DemoOnlyArm`) and aster's testnet
//! fallback (`LiveCredentialsAbsent` at `Demo`) — is now [`LCA`], `(Paper, LiveCredentialsAbsent)`.
//! [`no_cell_arms_at_a_tier_its_row_did_not_name`] is the rule as a property: a cell is paper or
//! exactly its tier.
//!
//! ⚠ **Default build only**, like every module of this binary (`crates/vike-tradehub/tests/mount_roster.rs`).
//! ibkr, fxcm and polymarket are `FeatureAbsent` rows here, which is itself pinned (a store cannot
//! arm them). Their feature-on halves are `ibkr_mount.rs`, `fxcm_mount.rs` and `polymarket_mount.rs`.

use std::collections::HashMap;

use vike_config::{ArmingBlock as B, VenueMode as M};
use vike_tradehub::registry::REGISTRY;

use crate::support::{armed_policy, vars};

/// One cell: what the projection answers.
type Cell = (M, B);

/// A fixture's `(key, value)` pairs.
type Pairs = &'static [(&'static str, &'static str)];

/// The four states a two-tier store can be in for one venue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Keys {
    None,
    DemoOnly,
    LiveOnly,
    Both,
}

/// In the order a row's arrays list them.
const STATES: [Keys; 4] = [Keys::None, Keys::DemoOnly, Keys::LiveOnly, Keys::Both];

/// One roster venue: its key fixtures per tier and the 8 cells (4 key states x the two account
/// tiers that reach a bridge).
struct Row {
    venue: &'static str,
    /// Keys that belong to no tier (cTrader's app registration pair); present whenever ANY tier is.
    shared: Pairs,
    demo: Pairs,
    live: Pairs,
    /// `[none, demo only, live only, both]` for a `demo` account.
    at_demo: [Cell; 4],
    /// …and for a `live` account.
    at_live: [Cell; 4],
}

// ---- the vocabulary of cells -------------------------------------------------------------------

/// No usable key set for the tier the arm reaches — the original live gate.
const NC: Cell = (M::Paper, B::NoCredentials);
/// A `live` account the arm cannot honour at `live` — no LIVE-tier key set, or an arm with no live
/// tier at all. PAPER, never the demo tier: a live account never trades demo (decision 0119).
const LCA: Cell = (M::Paper, B::LiveCredentialsAbsent);
/// The demo tier armed, with nothing refused (a `demo` account asked for exactly this).
const DEMO: Cell = (M::Demo, B::None);
/// The live tier armed.
const LIVE: Cell = (M::Live, B::None);
/// A LIVE-tier key set the arm holds but will not use: stays paper, and says why.
const LTNW: Cell = (M::Paper, B::LiveTierNotWired);
/// A build without the venue's bridge.
const ABSENT: Cell = (M::Paper, B::FeatureAbsent);
/// What `just new-venue` scaffolds: a mount whose `resolve` answers "no live arm" until it is written.
/// Referenced only by the scaffold's row template (the marker at the end of [`rows`]).
#[expect(dead_code)]
const NO_LIVE_ARM: Cell = (M::Paper, B::NoLiveArm);

/// The generic crypto shape (binance, bybit, okx, hyperliquid): the account's tier picks the
/// network, and each tier reads only its own keys.
const CEILING_PICKS_THE_TIER: ([Cell; 4], [Cell; 4]) =
    ([NC, DEMO, NC, DEMO], [LCA, LCA, LIVE, LIVE]);

/// A DEMO-pinned arm (deribit, ctrader, alpaca, ig): it has no live tier, so a `live` account is
/// PAPER whatever the store holds (its held demo session is refused, decision 0119), a LIVE-tier key
/// set is never SELECTED — and one stored alone is named for what it is.
const DEMO_PINNED: ([Cell; 4], [Cell; 4]) = ([NC, DEMO, LTNW, DEMO], [NC, LCA, LTNW, LCA]);

/// A DEMO-pinned arm whose key vocabulary has no live tier to find (dukascopy): the same shape with
/// nothing to name.
const DEMO_PINNED_NO_LIVE_TIER: ([Cell; 4], [Cell; 4]) = ([NC, DEMO, NC, DEMO], [NC, LCA, NC, LCA]);

/// The roster, in the order of `vike_model::VENUES`.
fn rows() -> Vec<Row> {
    vec![
        Row {
            venue: "binance",
            shared: &[],
            demo: &[("BINANCE_DEMO_API_KEY", "k"), ("BINANCE_DEMO_API_SECRET", "s")],
            live: &[("BINANCE_LIVE_API_KEY", "k"), ("BINANCE_LIVE_API_SECRET", "s")],
            at_demo: CEILING_PICKS_THE_TIER.0,
            at_live: CEILING_PICKS_THE_TIER.1,
        },
        Row {
            venue: "bybit",
            shared: &[],
            demo: &[("BYBIT_DEMO_API_KEY", "k"), ("BYBIT_DEMO_API_SECRET", "s")],
            live: &[("BYBIT_LIVE_API_KEY", "k"), ("BYBIT_LIVE_API_SECRET", "s")],
            at_demo: CEILING_PICKS_THE_TIER.0,
            at_live: CEILING_PICKS_THE_TIER.1,
        },
        Row {
            venue: "okx",
            shared: &[],
            demo: &[
                ("OKX_DEMO_API_KEY", "k"),
                ("OKX_DEMO_API_SECRET", "s"),
                ("OKX_DEMO_API_PASSPHRASE", "p"),
            ],
            live: &[
                ("OKX_LIVE_API_KEY", "k"),
                ("OKX_LIVE_API_SECRET", "s"),
                ("OKX_LIVE_API_PASSPHRASE", "p"),
            ],
            at_demo: CEILING_PICKS_THE_TIER.0,
            at_live: CEILING_PICKS_THE_TIER.1,
        },
        Row {
            venue: "deribit",
            shared: &[],
            demo: &[("DERIBIT_DEMO_API_KEY", "k"), ("DERIBIT_DEMO_API_SECRET", "s")],
            live: &[("DERIBIT_LIVE_API_KEY", "k"), ("DERIBIT_LIVE_API_SECRET", "s")],
            at_demo: DEMO_PINNED.0,
            at_live: DEMO_PINNED.1,
        },
        Row {
            venue: "oanda",
            shared: &[],
            demo: &[("OANDA_DEMO_API_KEY", "k"), ("OANDA_DEMO_ACCOUNT_ID", "101-004-1-001")],
            live: &[("OANDA_LIVE_API_KEY", "k"), ("OANDA_LIVE_ACCOUNT_ID", "001-001-1-001")],
            // The one arm that REFUSES a store holding a live-named key set outright, the practice
            // set beside it included: "both" stays paper, for the same named reason.
            at_demo: [NC, DEMO, LTNW, LTNW],
            at_live: [NC, LCA, LTNW, LTNW],
        },
        Row {
            venue: "ig",
            shared: &[],
            demo: &[
                ("IG_DEMO_API_KEY", "k"),
                ("IG_DEMO_IDENTIFIER", "u"),
                ("IG_DEMO_PASSWORD", "p"),
            ],
            live: &[
                ("IG_LIVE_API_KEY", "k"),
                ("IG_LIVE_IDENTIFIER", "u"),
                ("IG_LIVE_PASSWORD", "p"),
            ],
            at_demo: DEMO_PINNED.0,
            at_live: DEMO_PINNED.1,
        },
        Row {
            venue: "fxcm",
            shared: &[],
            demo: &[("FXCM_DEMO_USER", "u"), ("FXCM_DEMO_PASSWORD", "p")],
            live: &[("FXCM_LIVE_USER", "u"), ("FXCM_LIVE_PASSWORD", "p")],
            at_demo: [ABSENT; 4],
            at_live: [ABSENT; 4],
        },
        Row {
            venue: "dukascopy",
            shared: &[],
            demo: &[("DUKASCOPY_DEMO1_LOGIN", "u"), ("DUKASCOPY_DEMO1_PASSWORD", "p")],
            // JForex accounts are keyed by broker account (`DEMO1`, `DEMO2`), not by tier: no
            // live-tier key exists in this venue's vocabulary, so "live only" is "no keys".
            live: &[],
            at_demo: DEMO_PINNED_NO_LIVE_TIER.0,
            at_live: DEMO_PINNED_NO_LIVE_TIER.1,
        },
        Row {
            venue: "polymarket",
            shared: &[],
            // Polymarket runs no testnet: its one key set is the mainnet one.
            demo: &[],
            live: &[("POLY_PRIVATE_KEY", "0xkey")],
            at_demo: [ABSENT; 4],
            at_live: [ABSENT; 4],
        },
        Row {
            venue: "ibkr",
            shared: &[],
            demo: &[("IBKR_DEMO_ACCOUNT", "DU1234567")],
            live: &[("IBKR_LIVE_ACCOUNT", "U1234567")],
            at_demo: [ABSENT; 4],
            at_live: [ABSENT; 4],
        },
        Row {
            venue: "ctrader",
            shared: &[("CTRADER_CLIENT_ID", "cid"), ("CTRADER_CLIENT_SECRET", "csecret")],
            demo: &[("CTRADER_DEMO_ACCESS_TOKEN", "at"), ("CTRADER_DEMO_REFRESH_TOKEN", "rt")],
            live: &[("CTRADER_LIVE_ACCESS_TOKEN", "lat"), ("CTRADER_LIVE_REFRESH_TOKEN", "lrt")],
            at_demo: DEMO_PINNED.0,
            at_live: DEMO_PINNED.1,
        },
        Row {
            venue: "alpaca",
            shared: &[],
            demo: &[
                ("ALPACA_SANDBOX_CLIENT_ID", "cid"),
                ("ALPACA_SANDBOX_CLIENT_SECRET", "cs"),
                ("ALPACA_SANDBOX_ACCOUNT_ID", "acct"),
            ],
            live: &[
                ("ALPACA_LIVE_CLIENT_ID", "cid"),
                ("ALPACA_LIVE_CLIENT_SECRET", "cs"),
                ("ALPACA_LIVE_ACCOUNT_ID", "acct"),
            ],
            at_demo: DEMO_PINNED.0,
            at_live: DEMO_PINNED.1,
        },
        Row {
            venue: "aster",
            shared: &[],
            demo: &[("ASTER_TESTNET_USER", "0xUser"), ("ASTER_TESTNET_PRIVATE_KEY", "0xkey")],
            live: &[("ASTER_LIVE_USER", "0xUser"), ("ASTER_LIVE_PRIVATE_KEY", "0xkey")],
            // No switch: the tier is WHICH key set exists, tried live-first; a `demo` account deletes
            // the live attempt and a `live` one never falls back to testnet (decision 0119).
            at_demo: [NC, DEMO, NC, DEMO],
            at_live: [NC, LCA, LIVE, LIVE],
        },
        Row {
            venue: "hyperliquid",
            shared: &[],
            demo: &[("HYPERLIQUID_DEMO_PRIVATE_KEY", "0xkey")],
            live: &[("HYPERLIQUID_LIVE_PRIVATE_KEY", "0xkey")],
            at_demo: CEILING_PICKS_THE_TIER.0,
            at_live: CEILING_PICKS_THE_TIER.1,
        },
        // vike:new-venue:row // TODO(new-venue: {venue}): a scaffolded venue answers `NoLiveArm` at every tier and key state,
        // vike:new-venue:row // which is what this row pins. Replace the cells (and the fixtures) with the arm's real ones once its `resolve` is written.
        // vike:new-venue:row Row {
        // vike:new-venue:row     venue: "{venue}",
        // vike:new-venue:row     shared: &[],
        // vike:new-venue:row     demo: &[],
        // vike:new-venue:row     live: &[],
        // vike:new-venue:row     at_demo: [NO_LIVE_ARM; 4],
        // vike:new-venue:row     at_live: [NO_LIVE_ARM; 4],
        // vike:new-venue:row },
    ]
}

/// The store a cell plants: the shared keys whenever any tier is present, then each tier's.
fn store(row: &Row, state: Keys) -> HashMap<String, String> {
    let (demo, live) = match state {
        Keys::None => (false, false),
        Keys::DemoOnly => (true, false),
        Keys::LiveOnly => (false, true),
        Keys::Both => (true, true),
    };
    let mut pairs: Vec<(&str, &str)> = Vec::new();
    if demo || live {
        pairs.extend_from_slice(row.shared);
    }
    if demo {
        pairs.extend_from_slice(row.demo);
    }
    if live {
        pairs.extend_from_slice(row.live);
    }
    vars(&pairs)
}

/// The cells whose BLOCK the `LiveTierNotWired` change renamed, as `(venue, tier, key state, the
/// block `main` printed there)` — every other cell of the matrix equals `main`'s, block included.
///
/// No venue list is written here, because the set is DERIVED: it is exactly the cells the table
/// above gives [`LTNW`] — the `LiveOnly` state of every row built from [`DEMO_PINNED`] under both
/// tiers that reach a bridge, plus oanda's `Both` state too, because its arm refuses a live-named
/// set even with its practice set beside it. [`the_renamed_cells_are_exactly_the_ones_that_changed`]
/// holds this list against those cells in both directions, so a venue joins or leaves it only by its
/// table row changing. ibkr and fxcm are feature-on, hence not rows of this default-build table; they
/// carry the same two cells each in `ibkr_mount.rs` and `fxcm_mount.rs`. No cell here changes its
/// effective mode: a cause moved, a tier did not.
const RENAMED: &[(&str, M, Keys, B)] = &[
    ("deribit", M::Demo, Keys::LiveOnly, B::NoCredentials),
    ("deribit", M::Live, Keys::LiveOnly, B::NoCredentials),
    ("ig", M::Demo, Keys::LiveOnly, B::NoCredentials),
    ("ig", M::Live, Keys::LiveOnly, B::NoCredentials),
    ("ctrader", M::Demo, Keys::LiveOnly, B::NoCredentials),
    ("ctrader", M::Live, Keys::LiveOnly, B::NoCredentials),
    ("alpaca", M::Demo, Keys::LiveOnly, B::NoCredentials),
    ("alpaca", M::Live, Keys::LiveOnly, B::NoCredentials),
    ("oanda", M::Demo, Keys::LiveOnly, B::NoCredentials),
    ("oanda", M::Demo, Keys::Both, B::NoCredentials),
    ("oanda", M::Live, Keys::LiveOnly, B::NoCredentials),
    ("oanda", M::Live, Keys::Both, B::NoCredentials),
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
            let map = store(row, state);
            assert_eq!(
                vike_mount::venue_arming_under(REGISTRY, row.venue, &map, M::Paper),
                (M::Paper, B::PaperTier),
                "{} {state:?}: a `paper` tier answers above every bridge and no key moves it",
                row.venue
            );
        }
    }
    for (venue, tier, state, expected) in cells(&rows) {
        let row = rows.iter().find(|r| r.venue == venue).expect("a row");
        let got = vike_mount::venue_arming_under(REGISTRY, venue, &store(row, state), tier);
        if got != expected {
            wrong.push(format!(
                "{venue:<12} tier={tier:<5} keys={state:?}: expected {expected:?}, got {got:?}"
            ));
        }
    }
    assert!(wrong.is_empty(), "the matrix disagrees with the projection:\n{}", wrong.join("\n"));
}

/// **A stay-paper cell stays paper at the MOUNT, not only in the projection.** A cell whose
/// effective mode is `Paper` is run through the real `make_engine`: it marks no venue live and
/// builds no reconcile handle, and it does so offline — a bridge that declines opens no socket.
/// (Armed cells are not mounted: they would dial the venue. The projection is their pin.)
#[test]
fn every_stay_paper_cell_mounts_paper_offline() {
    // The symbol is irrelevant on the paper path (`all_roster_venues_absent_creds_stay_paper_and_inert`).
    const SYMBOL: &str = "BTCUSDT";
    let rows = rows();
    let (tx, _rx) = vike_exec::event_channel(16);
    let mut mounted = 0usize;
    for (venue, tier, state, (effective, _)) in cells(&rows) {
        if effective != M::Paper {
            continue;
        }
        let row = rows.iter().find(|r| r.venue == venue).expect("a row");
        let mut live = std::collections::HashSet::new();
        let creds = store(row, state);
        let policy = armed_policy(venue, tier);
        let mut env = vike_mount::MountEnv::new(REGISTRY, &creds, &tx, &mut live);
        env.recon_enabled = true;
        env.policy = Some(&policy);
        let (_engine, recon) =
            vike_mount::make_engine(&mut env, venue, SYMBOL).unwrap_or_else(|e| {
                panic!("{venue} {tier} {state:?}: a paper mount must not refuse: {e}")
            });
        assert!(
            live.is_empty(),
            "{venue} {tier} {state:?}: the cell says paper, the mount marked it live"
        );
        assert!(recon.is_none(), "{venue} {tier} {state:?}: a paper venue never reconciles");
        mounted += 1;
    }
    assert!(mounted >= 40, "only {mounted} stay-paper cells were mounted — the walk is broken");
}

/// **No cell arms at a tier its row did not name** — PAPER or exactly the account's tier: never
/// above it, and never the demo fallback below `live` (decision 0119, the owner's no-downgrade
/// rule). The property the whole table exists to protect, asserted on the table itself so a typo
/// in a row cannot make a pin that blesses an escalation or a downgrade.
#[test]
fn no_cell_arms_at_a_tier_its_row_did_not_name() {
    for (venue, tier, state, (effective, _)) in cells(&rows()) {
        assert!(
            effective == M::Paper || effective == tier,
            "{venue} {state:?}: effective {effective:?} for a `{tier}` account"
        );
    }
}

/// [`RENAMED`] is exactly the set of cells whose block `main` printed differently, and each names a
/// cell that is still paper — a rename of a cause that also changed the tier would not be a rename.
#[test]
fn the_renamed_cells_are_exactly_the_ones_that_changed() {
    let rows = rows();
    for (venue, tier, state, before) in RENAMED {
        let (_, _, _, (effective, block)) = cells(&rows)
            .into_iter()
            .find(|(v, c, s, _)| v == venue && c == tier && s == state)
            .unwrap_or_else(|| panic!("{venue} {tier} {state:?}: not a cell of the matrix"));
        assert_eq!(effective, M::Paper, "{venue} {tier} {state:?}: a renamed cause stays paper");
        assert_ne!(block, *before, "{venue} {tier} {state:?}: listed as renamed, but unchanged");
        assert_eq!(block, B::LiveTierNotWired, "{venue} {tier} {state:?}: the new cause");
        assert_eq!(*before, B::NoCredentials, "{venue} {tier} {state:?}: what `main` printed");
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
