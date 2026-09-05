//! The `[[mounts]]` daemon profile (split-plane I10, the Pattern-A centerpiece): N strategies /
//! N venues in ONE daemon process — the STATIC, profile-declared half (runtime mount/unmount is
//! B5, a separate concurrent workstream).
//!
//! What is proven, and against which seam:
//!
//! - **Profile shape** (`DaemonProfile::from_toml_str`): the single-mount spelling parses
//!   byte-compatibly beside the new array; both-spellings-set is refused naming the offending
//!   keys; a row that fails the existing per-mount refusals fails naming its ROW; two rows
//!   deriving one mount id are refused at LOAD naming both rows (never `assemble_core`'s
//!   duplicate-id panic).
//! - **Which ACCOUNT a row trades on** (`account = "ALT"`): the headline SPREAD — two rows, one
//!   venue, one symbol, one strategy, two accounts — LOADS, each row keeps its own label through
//!   `mount_rows`/`to_mount_spec`, and the two derive DIFFERENT mount ids while the default
//!   account's id stays byte-identical. This is the operator's only entry point to the account
//!   field, and it sits above `vike-core`/`vike-run`, so their account tests cannot reach it.
//! - **The mount itself** (`vike_run::build_paper_multi_strategy_core_with`, the exact call
//!   `main.rs`'s multi paper arm composes): a two-venue profile mounts BOTH strategies on one
//!   core and each trades into its OWN venue's paper book (the attribution a shared process must
//!   keep straight); a one-venue two-symbol profile routes through
//!   `MultiPaperExecutionClient` — each book holds exactly its own mount's fill.
//! - **Bounded teardown**: the two-mount core shuts down `Graceful` inside the profile deadline
//!   through the same `run_with_deadline` primitive `main.rs` uses.
//! - **`StrategyStatus` over the wire** (split-plane B4's `mounts: Vec<WireMountRow>` — designed
//!   for exactly this): a publisher spawned with N mount rows answers N rows; the mount-less
//!   `publish::spawn` keeps the identity-derived single row byte-identically
//!   (`observe_roundtrip.rs` pins that half).
//!
//! No network, no creds, no feature flags: paper books + hand-fed bars + a loopback observe
//! server, all in the default CI lane.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_exec::BarUpdate;
use vike_model::Bar;
use vike_ops::shutdown::{ShutdownOutcome, run_with_deadline};
use vike_run::{
    MultiStrategyMount, PaperFill, PaperHalt, PaperMountOpts, StrategyMountSpec,
    build_paper_multi_strategy_core_with,
};
use vike_tradehub::config::DaemonProfile;
use vike_tradehub::{publish, server};
use vike_tradehub_client::NodeKeys;
use vike_tradehub_client::auth;
use vike_tradehub_client::proto::{
    NODE_PROTO_VERSION, Request, Response, Scope, read_frame, write_frame,
};
use vike_tradehub_client::wire::{WireMountRow, WireNodeIdentity};

/// A HALT sentinel path THIS FILE owns and never creates — the same pinning
/// `any_strategy_mount.rs` documents: a paper mount is HALT-armed by design, so a test expecting
/// fills must not inherit the operator's kill switch off whatever box runs it.
///
/// ⚠ Returns the owning `TempDir` ALONGSIDE the path, and the caller must BIND it for as long as
/// the mount lives — the paper client consults the pinned path on every opening order. This is
/// `crates/vike-core/src/scratch.rs`'s `Scratch::reserved` shape: the ROOT exists and is owned,
/// the `HALT` inside it does not, so `opts_pinned_to`'s "must NOT exist" assertion holds by
/// construction rather than by assuming nothing on the box ever wrote to a pid-keyed name.
///
/// It used to be `env::temp_dir().join(format!("…-{pid}-{name}")).join("HALT")`. Nothing here
/// creates that directory, so this file leaked none — but it is the same idiom as the family that
/// leaked 44,840 directories under the CI box's `/tmp` (measured 2026-08-25) and flakes on pid reuse
/// across the box's two test users; the argument and the numbers are in
/// `crates/vike-tradehub/src/config.rs`'s `own_script`.
fn own_sentinel(name: &str) -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::Builder::new()
        .prefix(&format!("vike-tradehub-multi-mount-{name}-"))
        .tempdir()
        .expect("temp sentinel root");
    let path = root.path().join("HALT");
    (root, path)
}

fn opts_pinned_to(sentinel: &Path) -> PaperMountOpts {
    assert!(
        !sentinel.exists(),
        "the pinned sentinel must NOT exist, or the mount refuses opening orders: {}",
        sentinel.display()
    );
    PaperMountOpts { halt: PaperHalt::Pinned(sentinel.to_path_buf()), ..Default::default() }
}

fn bar(ts: i64, px: f64) -> Bar {
    Bar {
        ts,
        open: px,
        high: px,
        low: px,
        close: px,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

fn wait_until(secs: u64, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Resolve a validated profile's rows into the `StrategyMountSpec`s `main.rs`'s multi paper arm
/// builds — the same three library calls, in `main`'s order (rows → lowerings → derived
/// controller ids → resolve).
fn resolve_mounts(profile: &DaemonProfile) -> Vec<StrategyMountSpec> {
    profile
        .mount_rows()
        .into_iter()
        .map(|row| {
            let cfg = row.to_mount_config();
            let mut spec = row.to_mount_spec();
            spec.controller_id = Some(row.derived_controller_id());
            let strategy = row.resolve_strategy(&cfg).expect("a validated row resolves");
            StrategyMountSpec { strategy, spec }
        })
        .collect()
}

fn book<'a>(
    mount: &'a MultiStrategyMount,
    venue: &str,
    symbol: &str,
) -> &'a Arc<Mutex<Vec<PaperFill>>> {
    mount.fills.iter().find(|((v, s), _)| v == venue && s == symbol).map(|(_, f)| f).unwrap_or_else(
        || {
            panic!(
                "no paper book for ({venue}, {symbol}); books: {:?}",
                mount.fills.iter().map(|(k, _)| k).collect::<Vec<_>>()
            )
        },
    )
}

fn drive_two_bars(mount: &MultiStrategyMount, venue: &str, symbol: &str) {
    let bars = mount.handle.bar_sender();
    for (i, px) in [0.40_f64, 0.50].into_iter().enumerate() {
        bars.close(BarUpdate {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            interval: "1m".to_string(),
            bar: bar(60_000 * (i as i64 + 1), px),
        })
        .expect("the core is alive");
    }
}

// ---------------------------------------------------------------------------------------------
// Profile shape
// ---------------------------------------------------------------------------------------------

/// BYTE-COMPAT: a pre-I10 single-mount profile parses unchanged, reports an EMPTY `mounts` array,
/// and its one `mount_rows()` row IS the profile (same lowering, controller id left to the
/// runtime's legacy derivation).
#[test]
fn a_single_mount_profile_stays_byte_compatible() {
    let profile = DaemonProfile::from_toml_str(
        r#"
venue = "polymarket"
symbol = "MM_SINGLE_TOK"
interval = "1m"
interval_ms = 60000

[strategy]
name = "buy_hold"

[strategy.params]
size = 3.0
"#,
    )
    .expect("the historical single-mount spelling still parses");
    assert!(profile.mounts.is_empty(), "no [[mounts]] array ⇒ empty");
    let rows = profile.mount_rows();
    assert_eq!(rows.len(), 1, "a single-mount profile is ONE row");
    let spec = rows[0].to_mount_spec();
    assert_eq!(spec.venue, "polymarket");
    assert_eq!(spec.symbol, "MM_SINGLE_TOK");
    assert_eq!(
        spec.controller_id, None,
        "the single-mount path keeps the runtime's legacy {{venue}}__{{symbol}}__{{interval}} \
         derivation — an existing deployment's state sidecar names must not change"
    );
}

/// **THE HEADLINE SPREAD, at the only door an operator actually writes to.** Two `[[mounts]]` rows
/// on ONE venue and ONE symbol differing only by `account` — long BTC on the default account,
/// short BTC on a labelled one — must LOAD, and each row must keep its own account through
/// `mount_rows()`.
///
/// It did neither. `DaemonProfile::derived_controller_id` forgot the account, so both rows derived
/// the SAME mount id and `validate`'s duplicate-id check refused the profile at load — telling the
/// operator to "make one row distinct — a different interval, symbol or strategy", i.e. to stop
/// running a spread. The feature was unreachable through its own entry point.
///
/// Nothing could see it, because every account-aware test in this workspace builds its mounts
/// BELOW this seam: `vike-core`'s `two_mounts_on_one_symbol_and_two_accounts_each_reach_their_own_engine`
/// constructs `StrategyMount`s directly and `vike-run`'s lowering test starts at a `MountSpec`.
/// Both crates sit under this one, so neither can reach the profile parse.
///
/// The two rows carry the SAME strategy on purpose: a spread whose legs differ by strategy already
/// derived distinct ids and would have passed while the defect stood.
#[test]
fn a_spread_of_two_accounts_on_one_symbol_loads_and_each_row_keeps_its_account() {
    let profile = DaemonProfile::from_toml_str(
        r#"
[[mounts]]
venue = "binance"
symbol = "MM_SPREAD_BTC"
interval = "1m"
interval_ms = 60000

[mounts.strategy]
name = "buy_hold"

[mounts.strategy.params]
size = 1.0

[[mounts]]
venue = "binance"
symbol = "MM_SPREAD_BTC"
account = "ALT"
interval = "1m"
interval_ms = 60000

[mounts.strategy]
name = "buy_hold"

[mounts.strategy.params]
size = 1.0
"#,
    )
    .expect(
        "two accounts on one instrument is an ORDINARY SPREAD, not a duplicate mount — it must \
         load",
    );

    let rows = profile.mount_rows();
    assert_eq!(rows.len(), 2, "two rows in, two rows out");

    // (1) THE LOWERING. `mount_rows` copies every field by hand into a single-mount profile — a
    // FOURTH hand-written copy of `account`, after `vike_run`'s two `fold_strategy_mount` builders
    // and `StrategyMountSpec::into_mount`. It is the copy nearest the operator and the only one
    // above `vike-run`, so `the_account_survives_every_lowering_from_a_mount_spec_into_the_core`
    // cannot reach it. Dropping it here is the silent catastrophe the field exists to prevent: the
    // mount runs on the venue's DEFAULT account and `refuse_unarmed_mount_accounts` finds nothing
    // to refuse, because a `None` account is never checked.
    assert_eq!(rows[0].account, None, "an account-less row is the DEFAULT account");
    assert_eq!(
        rows[1].account.as_ref().and_then(|l| l.text()),
        Some("ALT"),
        "the labelled row's account must survive `mount_rows`"
    );

    // (2) …and through `to_mount_spec`, which is what `main.rs` hands to the core.
    let specs: Vec<_> = rows.iter().map(DaemonProfile::to_mount_spec).collect();
    assert_eq!(specs[0].account, None);
    assert_eq!(specs[1].account.as_ref().and_then(|l| l.text()), Some("ALT"));
    assert_eq!(specs[0].symbol, specs[1].symbol, "one instrument — that is what makes it a spread");

    // (3) THE IDENTITIES DIVERGE, which is what lets both rows exist. Two accounts are two BOOKS,
    // so they must not share a durable-state sidecar or a journal attribution key.
    let ids: Vec<String> = rows.iter().map(DaemonProfile::derived_controller_id).collect();
    assert_ne!(ids[0], ids[1], "two accounts are two mounts: {ids:?}");
    assert_eq!(
        ids[0], "binance__MM_SPREAD_BTC__1m__buy_hold",
        "the DEFAULT account's id is the legacy quadruple, byte for byte — no shipped \
         deployment's sidecar may move"
    );
    assert_eq!(
        ids[1], "binance__MM_SPREAD_BTC__1m__buy_hold__ALT",
        "a labelled account appends its label, and nothing else changes"
    );
}

/// The NEGATIVE CONTROL for the test above: making the account part of the mount id must not have
/// disarmed the duplicate-id refusal it lives beside. Two rows equal in every field — account
/// included — are still refused at load, naming both rows.
#[test]
fn two_rows_equal_in_every_field_including_the_account_are_still_refused() {
    let err = DaemonProfile::from_toml_str(
        r#"
[[mounts]]
venue = "binance"
symbol = "MM_DUP_BTC"
account = "ALT"
interval = "1m"
interval_ms = 60000

[mounts.strategy]
name = "buy_hold"

[mounts.strategy.params]
size = 1.0

[[mounts]]
venue = "binance"
symbol = "MM_DUP_BTC"
account = "ALT"
interval = "1m"
interval_ms = 60000

[mounts.strategy]
name = "buy_hold"

[mounts.strategy.params]
size = 1.0
"#,
    )
    .expect_err("two rows on ONE account, equal in every field, are one mount written twice");
    assert!(err.contains("SAME mount id"), "the refusal is the duplicate-id one: {err}");
    assert!(
        err.contains("mounts[0]") && err.contains("mounts[1]"),
        "…and it names BOTH rows, which is the whole reason it is a load-time check: {err}"
    );
}

/// A row naming the RESERVED default spelling is refused by `AccountLabel::parse`, through the
/// hand-written serde that exists so a label reaching this type from a FILE faces the same
/// refusals a `parse` call does. `DEFAULT` means the account the operator already has, whose
/// ceiling is the venue's own `[venues]` line — accepting it would create a second, silent name
/// for one account.
#[test]
fn a_mounts_row_may_not_spell_the_reserved_default_account() {
    let err = DaemonProfile::from_toml_str(
        r#"
[[mounts]]
venue = "binance"
symbol = "MM_RESERVED_BTC"
account = "DEFAULT"
interval = "1m"
interval_ms = 60000

[mounts.strategy]
name = "buy_hold"

[mounts.strategy.params]
size = 1.0
"#,
    )
    .expect_err("`DEFAULT` is the reserved spelling of the unlabelled account");
    assert!(
        err.to_uppercase().contains("DEFAULT"),
        "the refusal names the spelling it rejected: {err}"
    );
}

/// The two spellings are mutually exclusive, and the refusal NAMES the offending top-level keys.
#[test]
fn both_spellings_set_is_refused_naming_the_keys() {
    let err = DaemonProfile::from_toml_str(
        r#"
symbol = "TOP_LEVEL_TOK"
qty = 5.0

[[mounts]]
symbol = "ROW_TOK"
"#,
    )
    .expect_err("a top-level mount field beside [[mounts]] must refuse");
    assert!(err.contains("BOTH spellings"), "the refusal says BOTH spellings were set: {err}");
    assert!(err.contains("symbol") && err.contains("qty"), "…and names the keys: {err}");
}

/// …including the venue key, which used to be indistinguishable from its default.
#[test]
fn a_top_level_venue_beside_mounts_is_refused() {
    let err = DaemonProfile::from_toml_str(
        r#"
venue = "binance"

[[mounts]]
symbol = "ROW_TOK"
"#,
    )
    .expect_err("a top-level venue beside [[mounts]] configures nothing and must refuse");
    assert!(err.contains("venue"), "the refusal names the venue key: {err}");
}

/// A row that fails the EXISTING per-mount refusals fails at load, and the error names the row —
/// the same `unknown_params` gate a single-mount profile hits, prefixed `mounts[i]`.
#[test]
fn a_mounts_row_failure_names_the_offending_row() {
    let err = DaemonProfile::from_toml_str(
        r#"
[[mounts]]
symbol = "GOOD_TOK"

[mounts.strategy]
name = "buy_hold"

[mounts.strategy.params]
size = 3.0

[[mounts]]
symbol = "BAD_TOK"

[mounts.strategy]
name = "buy_hold"

[mounts.strategy.params]
sizee = 3.0
"#,
    )
    .expect_err("a row with an unread params key must refuse, exactly as a single mount would");
    assert!(err.starts_with("mounts[1]"), "the refusal names the offending ROW: {err}");
    assert!(err.contains("sizee"), "…and the offending key: {err}");
}

/// Two rows deriving one mount id — venue, symbol, interval AND strategy all equal — are refused
/// at LOAD naming both rows, never left to `assemble_core`'s duplicate-controller panic.
#[test]
fn duplicate_derived_mount_ids_are_refused_naming_both_rows() {
    let err = DaemonProfile::from_toml_str(
        r#"
[[mounts]]
symbol = "DUP_TOK"

[mounts.strategy]
name = "buy_hold"

[mounts.strategy.params]
size = 3.0

[[mounts]]
symbol = "DUP_TOK"

[mounts.strategy]
name = "buy_hold"

[mounts.strategy.params]
size = 5.0
"#,
    )
    .expect_err("two rows on one (venue, symbol, interval, strategy) must refuse at load");
    assert!(
        err.contains("mounts[0]") && err.contains("mounts[1]"),
        "the refusal names BOTH rows: {err}"
    );
    assert!(err.contains("polymarket__DUP_TOK__1m__buy_hold"), "…and the derived id: {err}");
}

/// The id derivation is venue/symbol/interval + STRATEGY identity, so two DIFFERENT strategies on
/// one series are two mounts, not a duplicate.
#[test]
fn two_strategies_on_one_series_derive_distinct_ids() {
    let profile = DaemonProfile::from_toml_str(
        r#"
[[mounts]]
symbol = "SHARED_TOK"

[mounts.strategy]
name = "buy_hold"

[mounts.strategy.params]
size = 3.0

[[mounts]]
symbol = "SHARED_TOK"
"#,
    )
    .expect("two different strategies on one series are two legitimate mounts");
    let ids: Vec<String> = profile.mount_rows().iter().map(|r| r.derived_controller_id()).collect();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1], "the strategy identity keeps the ids distinct: {ids:?}");
}

// ---------------------------------------------------------------------------------------------
// The mount: N strategies actually trade, each into its own book
// ---------------------------------------------------------------------------------------------

/// THE headline proof: a two-mount / two-VENUE profile mounts both strategies on ONE core, and
/// each trades into its OWN venue's paper book with its own size — the per-mount attribution a
/// shared process must keep straight (`coid_mount` routing underneath; asserted here at the book
/// seam, where a misroute would land the fill under the wrong venue).
#[test]
fn a_two_venue_profile_mounts_both_and_each_trades_in_its_own_book() {
    vike_log::test_init();
    let profile = DaemonProfile::from_toml_str(
        r#"
[[mounts]]
venue = "polymarket"
symbol = "MM_TWO_VENUE_TOK"
interval = "1m"
interval_ms = 60000

[mounts.strategy]
name = "buy_hold"

[mounts.strategy.params]
size = 3.0

[[mounts]]
venue = "binance"
symbol = "MM_TWO_VENUE_BTC"
interval = "1m"
interval_ms = 60000

[mounts.strategy]
name = "buy_hold"

[mounts.strategy.params]
size = 5.0
"#,
    )
    .expect("a two-venue [[mounts]] profile parses and validates");

    let (_halt_dir, sentinel) = own_sentinel("two-venue");
    let mount =
        build_paper_multi_strategy_core_with(resolve_mounts(&profile), opts_pinned_to(&sentinel));

    // Two books, one per (venue, symbol).
    assert_eq!(mount.fills.len(), 2, "one paper book per (venue, symbol)");

    // Drive two CLOSED bars on EACH mount's own series: buy_hold submits on the first bar's
    // on_bar; the paper book fills the market order at the next bar's open.
    drive_two_bars(&mount, "polymarket", "MM_TWO_VENUE_TOK");
    drive_two_bars(&mount, "binance", "MM_TWO_VENUE_BTC");

    let poly = Arc::clone(book(&mount, "polymarket", "MM_TWO_VENUE_TOK"));
    let cex = Arc::clone(book(&mount, "binance", "MM_TWO_VENUE_BTC"));
    assert!(
        wait_until(10, || {
            !poly.lock().expect("fills").is_empty() && !cex.lock().expect("fills").is_empty()
        }),
        "both mounts must trade: poly fills = {}, binance fills = {}",
        poly.lock().expect("fills").len(),
        cex.lock().expect("fills").len()
    );
    {
        let f = poly.lock().expect("fills");
        assert_eq!(f.len(), 1, "buy_hold buys once");
        assert_eq!(f[0].qty.to_bits(), 3.0_f64.to_bits(), "the polymarket mount's OWN size");
    }
    {
        let f = cex.lock().expect("fills");
        assert_eq!(f.len(), 1, "buy_hold buys once");
        assert_eq!(f[0].qty.to_bits(), 5.0_f64.to_bits(), "the binance mount's OWN size");
    }

    mount.handle.shutdown_and_join();
}

/// ⚠ **THE I10 REHEARSAL OBSERVATION, against a REAL two-mount core**
/// (`docs/ops/i10-rehearsal-2026-08-19.md`): the rehearsal's summary printed
/// `equity_book: 100000.0` for two mounts seeded at 10k each, because `build_node` seeds EVERY
/// default-build venue engine and the book figure sums them all. Correct, and misread.
///
/// This is the fix's evidence from a real core rather than a hand-built snapshot: mount TWO
/// venues, publish, and assert that
/// [`vike_tradehub::summary::mounted_book_equity`] — the scoping `main.rs`'s `summary_line`
/// renders as `equity_book_mounted` / `mounted_venues` — names exactly the two MOUNTED venues
/// and sums only their blocks, while `Portfolio::equity_book_total` (the unscoped
/// `equity_book`, unchanged by this program) stays wider.
///
/// The seeds here are the paper mount's own, not the ten-engine live shape — a paper multi-mount
/// core registers one engine per mounted venue — so the arithmetic is asserted as a RELATION
/// (`mounted ⊆ book`, names exact) rather than against the rehearsal's literal 20000/100000.
#[test]
fn the_mounted_set_scoping_names_exactly_the_mounted_venues_of_a_real_two_mount_core() {
    vike_log::test_init();
    let profile = DaemonProfile::from_toml_str(
        r#"
[[mounts]]
venue = "polymarket"
symbol = "MM_SCOPE_TOK"
interval = "1m"
interval_ms = 60000

[mounts.strategy]
name = "buy_hold"

[mounts.strategy.params]
size = 3.0

[[mounts]]
venue = "binance"
symbol = "MM_SCOPE_BTC"
interval = "1m"
interval_ms = 60000

[mounts.strategy]
name = "buy_hold"

[mounts.strategy.params]
size = 5.0
"#,
    )
    .expect("a two-venue [[mounts]] profile parses and validates");

    let (_halt_dir, sentinel) = own_sentinel("scope");
    let mount =
        build_paper_multi_strategy_core_with(resolve_mounts(&profile), opts_pinned_to(&sentinel));
    // Drive each mount so the core publishes a snapshot carrying its mount rows.
    drive_two_bars(&mount, "polymarket", "MM_SCOPE_TOK");
    drive_two_bars(&mount, "binance", "MM_SCOPE_BTC");

    let cell = mount.handle.snapshot_cell();
    assert!(
        wait_until(10, || {
            !vike_tradehub::summary::mounted_book_equity(&cell.load_full()).venues.is_empty()
        }),
        "the core must publish a snapshot carrying its mount rows"
    );

    let snap = cell.load_full();
    let scoped = vike_tradehub::summary::mounted_book_equity(&snap);
    let mut named = scoped.venues.clone();
    named.sort();
    assert_eq!(
        named,
        ["binance", "polymarket"],
        "the scoping names exactly the venues this daemon MOUNTED — the residual row (whose \
         venue is the empty string) is not one of them"
    );
    assert!(
        !scoped.venues.iter().any(|v| v.is_empty()),
        "and never an empty venue: {:?}",
        scoped.venues
    );
    // The scoped figure is a SUBSET of the whole book: every mounted venue's Delta block is in
    // both, and no un-mounted engine's seed can reach the scoped one.
    let whole = snap.portfolio.equity_book_total();
    assert!(
        scoped.total <= whole + 1e-9,
        "the mounted-set figure can never exceed the whole book: {} vs {whole}",
        scoped.total
    );
    // …and it is the sum of exactly the mounted venues' own Delta blocks, computed from the
    // per-venue rows the same snapshot carries (the relation the summary line renders).
    let expected: f64 = vike_model::py_sum(
        snap.portfolio
            .venues
            .iter()
            .filter(|v| v.balance_mode == vike_exec::BalanceMode::Delta)
            .filter(|v| scoped.venues.iter().any(|m| m == &v.venue))
            .map(|v| v.equity),
    );
    assert_eq!(scoped.total.to_bits(), expected.to_bits(), "same fold law, same order");

    mount.handle.shutdown_and_join();
}

/// The one-venue / two-SYMBOL shape: the venue's engine takes a `MultiPaperExecutionClient` (one
/// single-symbol book per symbol) and each mount's order lands in ITS symbol's book — the exact
/// misrouting the single-book tripwire exists to catch, proven routed here.
#[test]
fn two_symbols_on_one_venue_route_into_their_own_books() {
    vike_log::test_init();
    let profile = DaemonProfile::from_toml_str(
        r#"
[[mounts]]
symbol = "MM_MULTI_SYM_A"

[mounts.strategy]
name = "buy_hold"

[mounts.strategy.params]
size = 2.0

[[mounts]]
symbol = "MM_MULTI_SYM_B"

[mounts.strategy]
name = "buy_hold"

[mounts.strategy.params]
size = 7.0
"#,
    )
    .expect("a one-venue two-symbol [[mounts]] profile parses and validates");

    let (_halt_dir, sentinel) = own_sentinel("two-symbol");
    let mount =
        build_paper_multi_strategy_core_with(resolve_mounts(&profile), opts_pinned_to(&sentinel));
    assert_eq!(mount.fills.len(), 2, "one book per symbol behind the ONE venue engine");

    drive_two_bars(&mount, "polymarket", "MM_MULTI_SYM_A");
    drive_two_bars(&mount, "polymarket", "MM_MULTI_SYM_B");

    let a = Arc::clone(book(&mount, "polymarket", "MM_MULTI_SYM_A"));
    let b = Arc::clone(book(&mount, "polymarket", "MM_MULTI_SYM_B"));
    assert!(
        wait_until(10, || {
            !a.lock().expect("fills").is_empty() && !b.lock().expect("fills").is_empty()
        }),
        "both symbol mounts must trade: A fills = {}, B fills = {}",
        a.lock().expect("fills").len(),
        b.lock().expect("fills").len()
    );
    {
        let f = a.lock().expect("fills");
        assert_eq!(f.len(), 1, "exactly A's own fill in A's book");
        assert_eq!(f[0].qty.to_bits(), 2.0_f64.to_bits());
    }
    {
        let f = b.lock().expect("fills");
        assert_eq!(f.len(), 1, "exactly B's own fill in B's book");
        assert_eq!(f[0].qty.to_bits(), 7.0_f64.to_bits());
    }

    mount.handle.shutdown_and_join();
}

/// **The aster profile (split-plane I9, the daemon's fourth CEX venue) passes the LIVE gate and
/// mounts on the PAPER seam — no network, no credentials.** Two halves, deliberately in one test:
///
/// 1. `validate_for_live` ACCEPTS the runbook pair (`aster`, `BTCUSDT.P`) — the same pure gate
///    `live_mount` runs before any core spawns, proving the new `LIVE_WIRED_VENUES` row and
///    `vike_run::WIRED_MARKETS`' aster row agree on the symbol. (The shipped runbook file is
///    separately gated by `docs_profiles_parse.rs`; this asserts the pair at the source, so the
///    proof survives a runbook rename.)
/// 2. The same profile then MOUNTS on the paper seam (`build_paper_multi_strategy_core_with`, the
///    exact call `main.rs`'s paper arm composes), trades into aster's OWN `(venue, symbol)` book
///    and joins cleanly — the mount-reaches-ready proof, network-free by construction (paper book
///    + hand-fed bars; a venue `Feeds` is never constructed on the paper arm).
///
/// ⚠ CAPABILITY ONLY. A real aster mount is REAL MONEY in practice (credential-resolved
/// MAINNET-FIRST exec — root CLAUDE.md's aster paragraph), which is exactly why this proof runs on
/// the paper seam: this test existing must never be read as a validation run against the venue.
#[test]
fn an_aster_profile_passes_the_live_gate_and_mounts_on_the_paper_seam() {
    vike_log::test_init();
    let profile = DaemonProfile::from_toml_str(
        r#"
venue = "aster"
symbol = "BTCUSDT.P"
interval = "1m"
interval_ms = 60000
tick_size = 0.1

[strategy]
name = "buy_hold"

[strategy.params]
size = 2.0
"#,
    )
    .expect("the aster single-mount profile parses");
    profile
        .validate_for_live()
        .expect("(aster, BTCUSDT.P) is WIRED_MARKETS' aster pair and aster is live-wired");

    let (_halt_dir, sentinel) = own_sentinel("aster-paper");
    let mount =
        build_paper_multi_strategy_core_with(resolve_mounts(&profile), opts_pinned_to(&sentinel));
    assert_eq!(mount.fills.len(), 1, "one paper book — aster's own");

    drive_two_bars(&mount, "aster", "BTCUSDT.P");
    let fills = Arc::clone(book(&mount, "aster", "BTCUSDT.P"));
    assert!(
        wait_until(10, || !fills.lock().expect("fills").is_empty()),
        "the aster mount must trade on the paper seam"
    );
    {
        let f = fills.lock().expect("fills");
        assert_eq!(f.len(), 1, "buy_hold buys once");
        assert_eq!(f[0].qty.to_bits(), 2.0_f64.to_bits(), "the mount's OWN size");
    }
    mount.handle.shutdown_and_join();
}

/// **The alpaca profile (split-plane I9, the daemon's first CREDENTIALED-DATA venue) passes the
/// LIVE gate and mounts on the PAPER seam — no network, no credentials.** The same two halves as
/// the aster proof above: `validate_for_live` accepts the runbook pair (`alpaca`, `AAPL`) — the
/// new `LIVE_WIRED_VENUES` row agreeing with `vike_run::WIRED_MARKETS`' alpaca row — and the same
/// profile then trades into alpaca's OWN paper book and joins cleanly.
///
/// ⚠ Deliberately CREDENTIAL-FREE: the live path's credential threading (the SANDBOX trio the
/// DATA connection needs, and the refusal when it is absent) is `alpaca_plan`'s job, unit-tested
/// in `main.rs`'s own test module — the PAPER seam never builds a venue feed, which is exactly
/// what keeps this proof runnable on the credential-free CI runners.
#[test]
fn an_alpaca_profile_passes_the_live_gate_and_mounts_on_the_paper_seam() {
    vike_log::test_init();
    let profile = DaemonProfile::from_toml_str(
        r#"
venue = "alpaca"
symbol = "AAPL"
interval = "1m"
interval_ms = 60000
tick_size = 0.01

[strategy]
name = "buy_hold"

[strategy.params]
size = 2.0
"#,
    )
    .expect("the alpaca single-mount profile parses");
    profile
        .validate_for_live()
        .expect("(alpaca, AAPL) is WIRED_MARKETS' alpaca pair and alpaca is live-wired");

    let (_halt_dir, sentinel) = own_sentinel("alpaca-paper");
    let mount =
        build_paper_multi_strategy_core_with(resolve_mounts(&profile), opts_pinned_to(&sentinel));
    assert_eq!(mount.fills.len(), 1, "one paper book — alpaca's own");

    drive_two_bars(&mount, "alpaca", "AAPL");
    let fills = Arc::clone(book(&mount, "alpaca", "AAPL"));
    assert!(
        wait_until(10, || !fills.lock().expect("fills").is_empty()),
        "the alpaca mount must trade on the paper seam"
    );
    {
        let f = fills.lock().expect("fills");
        assert_eq!(f.len(), 1, "buy_hold buys once");
        assert_eq!(f[0].qty.to_bits(), 2.0_f64.to_bits(), "the mount's OWN size");
    }
    mount.handle.shutdown_and_join();
}

/// **The ctrader profile (split-plane I9, the second credentialed-data venue) passes the LIVE
/// gate and mounts on the PAPER seam — no network, no credentials.** `validate_for_live` accepts
/// the runbook pair (`ctrader`, `EURUSD`); the paper mount trades into ctrader's own book.
///
/// ⚠ Same credential-free framing as the alpaca proof above — and doubly load-bearing here: the
/// live ctrader feed's `connect_and_auth` handshake is SYNCHRONOUS, so no test that reaches the
/// real `wire_venue_feeds` arm can ever be network-free. The plan gate's credential threading and
/// refusals are `ctrader_plan`'s unit tests in `main.rs`; this proves the mount SHAPE the live
/// path shares (venue routing, paper book attribution, bounded join).
#[test]
fn a_ctrader_profile_passes_the_live_gate_and_mounts_on_the_paper_seam() {
    vike_log::test_init();
    let profile = DaemonProfile::from_toml_str(
        r#"
venue = "ctrader"
symbol = "EURUSD"
interval = "1m"
interval_ms = 60000
tick_size = 0.00001

[strategy]
name = "buy_hold"

[strategy.params]
size = 1000.0
"#,
    )
    .expect("the ctrader single-mount profile parses");
    profile
        .validate_for_live()
        .expect("(ctrader, EURUSD) is WIRED_MARKETS' ctrader pair and ctrader is live-wired");

    let (_halt_dir, sentinel) = own_sentinel("ctrader-paper");
    let mount =
        build_paper_multi_strategy_core_with(resolve_mounts(&profile), opts_pinned_to(&sentinel));
    assert_eq!(mount.fills.len(), 1, "one paper book — ctrader's own");

    drive_two_bars(&mount, "ctrader", "EURUSD");
    let fills = Arc::clone(book(&mount, "ctrader", "EURUSD"));
    assert!(
        wait_until(10, || !fills.lock().expect("fills").is_empty()),
        "the ctrader mount must trade on the paper seam"
    );
    {
        let f = fills.lock().expect("fills");
        assert_eq!(f.len(), 1, "buy_hold buys once");
        assert_eq!(f[0].qty.to_bits(), 1000.0_f64.to_bits(), "the mount's OWN size");
    }
    mount.handle.shutdown_and_join();
}

/// **The oanda profile (split-plane I9, the third credentialed-data venue) passes the LIVE gate
/// and mounts on the PAPER seam — no network, no credentials.** `validate_for_live` accepts the
/// runbook pair (`oanda`, `EURUSD`) — the new `LIVE_WIRED_VENUES` row agreeing with
/// `vike_run::WIRED_MARKETS`' oanda row — and the same profile then trades into oanda's OWN paper
/// book and joins cleanly.
///
/// ⚠ Same credential-free framing as the two proofs above. The live oanda feed's own gates — the
/// PRACTICE token/account pair both lanes authenticate with, the refusal when it is absent, and
/// the granularity-derived interval refusal this venue alone carries — are `oanda_plan`'s job and
/// are unit-tested in `main.rs`'s own test module; the PAPER seam builds no venue feed at all,
/// which is exactly what keeps this proof runnable on the credential-free CI runners.
///
/// The interval is `1m` here only because [`drive_two_bars`] publishes on that series and the bar
/// lane dispatches on the FULL `(venue, symbol, interval)` triple — it is NOT a claim that oanda
/// serves one bar width. It serves many (its bars are polled candles keyed by a venue granularity
/// code), which is the property that makes its interval refusal derived rather than a literal;
/// that half is proven where it lives, in `main.rs`'s `oanda_plan` unit tests.
#[test]
fn an_oanda_profile_passes_the_live_gate_and_mounts_on_the_paper_seam() {
    vike_log::test_init();
    let profile = DaemonProfile::from_toml_str(
        r#"
venue = "oanda"
symbol = "EURUSD"
interval = "1m"
interval_ms = 60000
tick_size = 0.00001

[strategy]
name = "buy_hold"

[strategy.params]
size = 1000.0
"#,
    )
    .expect("the oanda single-mount profile parses");
    profile
        .validate_for_live()
        .expect("(oanda, EURUSD) is WIRED_MARKETS' oanda pair and oanda is live-wired");

    let (_halt_dir, sentinel) = own_sentinel("oanda-paper");
    let mount =
        build_paper_multi_strategy_core_with(resolve_mounts(&profile), opts_pinned_to(&sentinel));
    assert_eq!(mount.fills.len(), 1, "one paper book — oanda's own");

    drive_two_bars(&mount, "oanda", "EURUSD");
    let fills = Arc::clone(book(&mount, "oanda", "EURUSD"));
    assert!(
        wait_until(10, || !fills.lock().expect("fills").is_empty()),
        "the oanda mount must trade on the paper seam"
    );
    {
        let f = fills.lock().expect("fills");
        assert_eq!(f.len(), 1, "buy_hold buys once");
        assert_eq!(f[0].qty.to_bits(), 1000.0_f64.to_bits(), "the mount's OWN size");
    }
    mount.handle.shutdown_and_join();
}

/// **The deribit profile (split-plane I9) passes the LIVE gate and mounts on the PAPER seam.**
/// `validate_for_live` accepts the runbook pair (`deribit`, `BTC-PERPETUAL`) — the new
/// `LIVE_WIRED_VENUES` row agreeing with `vike_run::WIRED_MARKETS`' deribit row — and the same
/// profile then trades into deribit's OWN paper book and joins cleanly.
///
/// ⚠ This venue is the one live-wired arm with NO credential gate on its feed (all four lanes are
/// keyless public MAINNET reads), so unlike the three credentialed-data proofs above there is no
/// "absent creds refuse the mount" half to leave elsewhere — `deribit_plan`'s unit tests in
/// `main.rs` prove the opposite property, that an EMPTY store still plans. What is left to this
/// file is the mount SHAPE the live path shares: venue routing, paper book attribution, bounded
/// join. Still credential-free and network-free: the PAPER seam builds no venue feed at all.
///
/// The interval is `1m` here only because [`drive_two_bars`] publishes on that series and the bar
/// lane dispatches on the FULL `(venue, symbol, interval)` triple — it is NOT a claim that deribit
/// serves one bar width. It serves a dozen resolutions, with GAPS (no 4h), which is what makes its
/// interval refusal derived from `vike_deribit::data::resolution_code` rather than a literal; that
/// half is proven where it lives, in `main.rs`'s `deribit_plan` unit tests.
#[test]
fn a_deribit_profile_passes_the_live_gate_and_mounts_on_the_paper_seam() {
    vike_log::test_init();
    let profile = DaemonProfile::from_toml_str(
        r#"
venue = "deribit"
symbol = "BTC-PERPETUAL"
interval = "1m"
interval_ms = 60000
tick_size = 0.5

[strategy]
name = "buy_hold"

[strategy.params]
size = 10.0
"#,
    )
    .expect("the deribit single-mount profile parses");
    profile.validate_for_live().expect(
        "(deribit, BTC-PERPETUAL) is WIRED_MARKETS' deribit pair and deribit is live-wired",
    );

    let (_halt_dir, sentinel) = own_sentinel("deribit-paper");
    let mount =
        build_paper_multi_strategy_core_with(resolve_mounts(&profile), opts_pinned_to(&sentinel));
    assert_eq!(mount.fills.len(), 1, "one paper book — deribit's own");

    drive_two_bars(&mount, "deribit", "BTC-PERPETUAL");
    let fills = Arc::clone(book(&mount, "deribit", "BTC-PERPETUAL"));
    assert!(
        wait_until(10, || !fills.lock().expect("fills").is_empty()),
        "the deribit mount must trade on the paper seam"
    );
    {
        let f = fills.lock().expect("fills");
        assert_eq!(f.len(), 1, "buy_hold buys once");
        assert_eq!(f[0].qty.to_bits(), 10.0_f64.to_bits(), "the mount's OWN size");
    }
    mount.handle.shutdown_and_join();
}

/// **The IG profile (split-plane I9, the fourth credentialed-data venue) passes the LIVE gate and
/// mounts on the PAPER seam — no network, no credentials.** `validate_for_live` accepts the
/// runbook pair (`ig`, `CS.D.EURUSD.MINI.IP`) — the new `LIVE_WIRED_VENUES` row agreeing with
/// `vike_run::WIRED_MARKETS`' ig row — and the same profile then trades into ig's OWN paper book
/// and joins cleanly.
///
/// The symbol is worth reading twice: IG's mounted string is an EPIC, dotted and nothing like a
/// ticker, and it travels verbatim through every layer this test drives — the profile, the mount
/// spec, the paper book key and the `(venue, symbol, interval)` bar-lane dispatch. A layer that
/// tried to normalize or split it would surface here as a missing book.
///
/// ⚠ Same credential-free framing as the three proofs above. The live IG feed's own gates — the
/// DEMO trio every Lightstreamer subscription logs in with, the refusal when it is absent, and the
/// `ig_scale`-derived interval refusal — are `ig_plan`'s job and are unit-tested in `main.rs`'s own
/// test module; the PAPER seam builds no venue feed at all, which is exactly what keeps this proof
/// runnable on the credential-free CI runners.
///
/// The interval is `1m` here only because [`drive_two_bars`] publishes on that series and the bar
/// lane dispatches on the FULL triple — it is NOT a claim that IG streams one bar width. It streams
/// several scales, which is the property that makes its interval refusal derived rather than a
/// literal.
#[test]
fn an_ig_profile_passes_the_live_gate_and_mounts_on_the_paper_seam() {
    vike_log::test_init();
    let profile = DaemonProfile::from_toml_str(
        r#"
venue = "ig"
symbol = "CS.D.EURUSD.MINI.IP"
interval = "1m"
interval_ms = 60000
tick_size = 0.0001

[strategy]
name = "buy_hold"

[strategy.params]
size = 1.0
"#,
    )
    .expect("the ig single-mount profile parses");
    profile
        .validate_for_live()
        .expect("(ig, CS.D.EURUSD.MINI.IP) is WIRED_MARKETS' ig pair and ig is live-wired");

    let (_halt_dir, sentinel) = own_sentinel("ig-paper");
    let mount =
        build_paper_multi_strategy_core_with(resolve_mounts(&profile), opts_pinned_to(&sentinel));
    assert_eq!(mount.fills.len(), 1, "one paper book — ig's own");

    drive_two_bars(&mount, "ig", "CS.D.EURUSD.MINI.IP");
    let fills = Arc::clone(book(&mount, "ig", "CS.D.EURUSD.MINI.IP"));
    assert!(
        wait_until(10, || !fills.lock().expect("fills").is_empty()),
        "the ig mount must trade on the paper seam"
    );
    {
        let f = fills.lock().expect("fills");
        assert_eq!(f.len(), 1, "buy_hold buys once");
        assert_eq!(f[0].qty.to_bits(), 1.0_f64.to_bits(), "the mount's OWN size");
    }
    mount.handle.shutdown_and_join();
}

/// The bounded teardown holds for a multi-mount core: the same `run_with_deadline` primitive
/// `main.rs` wraps `shutdown_and_join` in completes `Graceful` inside the profile's (default)
/// deadline with TWO venues' engines mounted.
#[test]
fn a_two_venue_core_shuts_down_gracefully_inside_the_deadline() {
    vike_log::test_init();
    let profile = DaemonProfile::from_toml_str(
        r#"
[[mounts]]
venue = "polymarket"
symbol = "MM_TEARDOWN_TOK"

[[mounts]]
venue = "binance"
symbol = "MM_TEARDOWN_BTC"
tick_size = 1.0
"#,
    )
    .expect("a two-venue maker-default [[mounts]] profile parses");
    let deadline = profile.shutdown_deadline();
    assert_eq!(deadline, Duration::from_millis(5_000), "the default deadline");

    let (_halt_dir, sentinel) = own_sentinel("teardown");
    let mount =
        build_paper_multi_strategy_core_with(resolve_mounts(&profile), opts_pinned_to(&sentinel));
    let handle = mount.handle;
    let outcome =
        run_with_deadline(Vec::new(), Box::new(move || handle.shutdown_and_join()), deadline);
    assert!(
        matches!(outcome, ShutdownOutcome::Graceful),
        "a two-venue multi-mount core must quiesce inside the deadline, got {outcome:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// StrategyStatus: N rows over the wire
// ---------------------------------------------------------------------------------------------

/// The observe key this test's server and client share.
const OBSERVE_KEY: &[u8] = b"multi-mount-observe-key";

/// A `StrategyStatus` answered from a publisher spawned WITH mount rows carries all N rows
/// verbatim — the `mounts: Vec<WireMountRow>` B4 shipped for exactly this — while
/// `effective_params`/identity keep the daemon-level singular shape. (The mount-less `spawn`
/// fallback — one identity-derived row — is pinned by `observe_roundtrip.rs`.)
#[test]
fn strategy_status_returns_one_row_per_mount() {
    vike_log::test_init();
    // A real (single-mount) paper core supplies the snapshot cell; the ROWS under test are the
    // publisher's process-static block, exactly as `main.rs` passes them.
    let cfg = vike_run::MakerMountConfig::polymarket("MM_STATUS_TOK", Some(3_000_000_000));
    let mount = vike_run::build_paper_maker_core(&cfg);
    let identity = WireNodeIdentity {
        name: "multi-mount-status".into(),
        strategy: "buy_hold+grid".into(),
        params: "joined".into(),
        live: false,
        build: "test-build".into(),
    };
    let rows = vec![
        WireMountRow {
            strategy: "buy_hold".into(),
            params: "venue=polymarket symbol=TOK_A interval=1m :: size=3".into(),
            live: false,
        },
        WireMountRow {
            strategy: "grid".into(),
            params: "venue=binance symbol=BTCUSDT interval=1m :: size=1 rungs=4".into(),
            live: false,
        },
    ];
    let publisher =
        publish::spawn_with_mounts(mount.handle.snapshot_cell(), Some(identity), rows.clone());
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let keys = NodeKeys::new(OBSERVE_KEY.to_vec(), Vec::new());
    let server_publisher = publisher.clone();
    std::thread::spawn(move || {
        let _ = server::serve(
            listener,
            server_publisher,
            keys,
            None,
            server::ControlLimitsConfig::default(),
            // No SettingsShow source — this test's server answers StrategyStatus only.
            None,
            None,
        );
    });

    let mut stream = authed_observe_stream(addr, OBSERVE_KEY);
    write_frame(&mut stream, &Request::StrategyStatus).expect("status request");
    match read_frame::<_, Response>(&mut stream).expect("status response") {
        Response::StrategyStatus(status) => {
            assert_eq!(status.identity.name, "multi-mount-status");
            assert_eq!(status.mounts, rows, "one row per mount, verbatim, in mount order");
            assert_eq!(
                status.effective_params, "joined",
                "the daemon-level params line stays the identity's"
            );
        }
        other => panic!("StrategyStatus must answer, got {other:?}"),
    }

    publisher.shutdown();
    mount.handle.shutdown_and_join();
}

/// Hello → Welcome → Auth(Observe) over a raw stream — the same low-level handshake helper
/// `observe_roundtrip.rs` uses (module-private there; the daemon test binary compiles each member
/// as its own module).
fn authed_observe_stream(addr: SocketAddr, key: &[u8]) -> TcpStream {
    let mut stream = TcpStream::connect(addr).expect("connect");
    write_frame(&mut stream, &Request::Hello { proto_version: NODE_PROTO_VERSION }).expect("hello");
    let nonce = match read_frame::<_, Response>(&mut stream).expect("welcome") {
        Response::Welcome { nonce, .. } => nonce,
        other => panic!("expected Welcome, got {other:?}"),
    };
    let mac = auth::sign(key, &nonce, NODE_PROTO_VERSION, Scope::Observe);
    write_frame(&mut stream, &Request::Auth { scope: Scope::Observe, mac }).expect("auth");
    match read_frame::<_, Response>(&mut stream).expect("authok") {
        Response::AuthOk { scope: Scope::Observe } => {}
        other => panic!("expected AuthOk(Observe), got {other:?}"),
    }
    stream
}
