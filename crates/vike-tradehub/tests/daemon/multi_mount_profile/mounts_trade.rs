//! The mount: N strategies trade on one core, each into its own book, and quiesce in deadline.

use std::assert_matches;
use std::sync::Arc;
use std::time::Duration;

use vike_mount::build_paper_multi_strategy_core_with;
use vike_ops::shutdown::{ShutdownOutcome, run_with_deadline};
use vike_tradehub::config::DaemonProfile;

use super::*;

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

    let (_halt_dir, sentinel) = own_sentinel("multi-mount", "two-venue");
    let mount =
        build_paper_multi_strategy_core_with(resolve_mounts(&profile), opts_pinned_to(&sentinel));

    // Two books, one per (venue, symbol).
    assert_eq!(mount.fills.len(), 2, "one paper book per (venue, symbol)");

    // Drive two CLOSED bars on EACH mount's own series: buy_hold submits on the first bar's
    // on_bar; the paper book fills the market order at the next bar's open.
    drive_two_bars(&mount.handle, "polymarket", "MM_TWO_VENUE_TOK");
    drive_two_bars(&mount.handle, "binance", "MM_TWO_VENUE_BTC");

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
/// [`vike_tradehub::summary::mounted_book_equity`] — the scoping `summary_line` (same module)
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

    let (_halt_dir, sentinel) = own_sentinel("multi-mount", "scope");
    let mount =
        build_paper_multi_strategy_core_with(resolve_mounts(&profile), opts_pinned_to(&sentinel));
    // Drive each mount so the core publishes a snapshot carrying its mount rows.
    drive_two_bars(&mount.handle, "polymarket", "MM_SCOPE_TOK");
    drive_two_bars(&mount.handle, "binance", "MM_SCOPE_BTC");

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

    let (_halt_dir, sentinel) = own_sentinel("multi-mount", "two-symbol");
    let mount =
        build_paper_multi_strategy_core_with(resolve_mounts(&profile), opts_pinned_to(&sentinel));
    assert_eq!(mount.fills.len(), 2, "one book per symbol behind the ONE venue engine");

    drive_two_bars(&mount.handle, "polymarket", "MM_MULTI_SYM_A");
    drive_two_bars(&mount.handle, "polymarket", "MM_MULTI_SYM_B");

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

/// The one body of the per-venue `…_profile_passes_the_live_gate_and_mounts_on_the_paper_seam`
/// proofs below: each keeps its own name, its own doc and its own inline profile text, and hands
/// this the `(venue, symbol)` pair and the `size` that text declares. The two halves are the aster
/// proof's: `validate_for_live` accepts the pair, then the same profile trades `size` into the
/// venue's OWN paper book (exactly one book, exactly one fill) and joins cleanly. The sentinel is
/// `own_sentinel("multi-mount", "<venue>-paper")`.
fn passes_the_live_gate_and_mounts_on_the_paper_seam(
    venue: &str,
    symbol: &str,
    size: f64,
    profile_toml: &str,
) {
    vike_log::test_init();
    let profile = DaemonProfile::from_toml_str(profile_toml)
        .unwrap_or_else(|e| panic!("the {venue} single-mount profile parses: {e:?}"));
    profile.validate_for_live().unwrap_or_else(|e| {
        panic!(
            "({venue}, {symbol}) is WIRED_MARKETS' {venue} pair and {venue} is live-wired: {e:?}"
        )
    });

    let (_halt_dir, sentinel) = own_sentinel("multi-mount", &format!("{venue}-paper"));
    let mount =
        build_paper_multi_strategy_core_with(resolve_mounts(&profile), opts_pinned_to(&sentinel));
    assert_eq!(mount.fills.len(), 1, "one paper book — {venue}'s own");

    drive_two_bars(&mount.handle, venue, symbol);
    let fills = Arc::clone(book(&mount, venue, symbol));
    assert!(
        wait_until(10, || !fills.lock().expect("fills").is_empty()),
        "the {venue} mount must trade on the paper seam"
    );
    {
        let f = fills.lock().expect("fills");
        assert_eq!(f.len(), 1, "buy_hold buys once");
        assert_eq!(f[0].qty.to_bits(), size.to_bits(), "the mount's OWN size");
    }
    mount.handle.shutdown_and_join();
}

/// **The aster profile (split-plane I9, the daemon's fourth CEX venue) passes the LIVE gate and
/// mounts on the PAPER seam — no network, no credentials.** Two halves, deliberately in one test:
///
/// 1. `validate_for_live` ACCEPTS the runbook pair (`aster`, `BTCUSDT.P`) — the same pure gate
///    `live_mount` runs before any core spawns, proving the new `LIVE_WIRED_VENUES` row and
///    `vike_tradehub::wired_markets::WIRED_MARKETS`' aster row agree on the symbol. (The shipped runbook file is
///    separately gated by `docs_profiles_parse.rs`; this asserts the pair at the source, so the
///    proof survives a runbook rename.)
/// 2. The same profile then MOUNTS on the paper seam (`build_paper_multi_strategy_core_with`, the
///    exact call `main.rs`'s paper arm composes), trades into aster's OWN `(venue, symbol)` book
///    and joins cleanly — the mount-reaches-ready proof, network-free by construction (paper book
///    + hand-fed bars; a venue `Feeds` is never constructed on the paper arm).
///
/// ⚠ CAPABILITY ONLY. A real aster mount is REAL MONEY in practice (credential-resolved
/// MAINNET-FIRST exec — crates/bridges/aster/CLAUDE.md's network section), which is exactly why this proof runs on
/// the paper seam: this test existing must never be read as a validation run against the venue.
#[test]
fn an_aster_profile_passes_the_live_gate_and_mounts_on_the_paper_seam() {
    passes_the_live_gate_and_mounts_on_the_paper_seam(
        "aster",
        "BTCUSDT.P",
        2.0,
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
    );
}

/// **The alpaca profile (split-plane I9, the daemon's first CREDENTIALED-DATA venue) passes the
/// LIVE gate and mounts on the PAPER seam — no network, no credentials.** The same two halves as
/// the aster proof above: `validate_for_live` accepts the runbook pair (`alpaca`, `AAPL`) — the
/// new `LIVE_WIRED_VENUES` row agreeing with `vike_tradehub::wired_markets::WIRED_MARKETS`' alpaca row — and the same
/// profile then trades into alpaca's OWN paper book and joins cleanly.
///
/// ⚠ Deliberately CREDENTIAL-FREE: the live path's credential threading (the SANDBOX trio the
/// DATA connection needs, and the refusal when it is absent) is `alpaca_plan`'s job, unit-tested
/// in `main.rs`'s own test module — the PAPER seam never builds a venue feed, which is exactly
/// what keeps this proof runnable on the credential-free CI runners.
#[test]
fn an_alpaca_profile_passes_the_live_gate_and_mounts_on_the_paper_seam() {
    passes_the_live_gate_and_mounts_on_the_paper_seam(
        "alpaca",
        "AAPL",
        2.0,
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
    );
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
    passes_the_live_gate_and_mounts_on_the_paper_seam(
        "ctrader",
        "EURUSD",
        1000.0,
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
    );
}

/// **The oanda profile (split-plane I9, the third credentialed-data venue) passes the LIVE gate
/// and mounts on the PAPER seam — no network, no credentials.** `validate_for_live` accepts the
/// runbook pair (`oanda`, `EURUSD`) — the new `LIVE_WIRED_VENUES` row agreeing with
/// `vike_tradehub::wired_markets::WIRED_MARKETS`' oanda row — and the same profile then trades into oanda's OWN paper
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
    passes_the_live_gate_and_mounts_on_the_paper_seam(
        "oanda",
        "EURUSD",
        1000.0,
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
    );
}

/// **The deribit profile (split-plane I9) passes the LIVE gate and mounts on the PAPER seam.**
/// `validate_for_live` accepts the runbook pair (`deribit`, `BTC-PERPETUAL`) — the new
/// `LIVE_WIRED_VENUES` row agreeing with `vike_tradehub::wired_markets::WIRED_MARKETS`' deribit row — and the same
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
    passes_the_live_gate_and_mounts_on_the_paper_seam(
        "deribit",
        "BTC-PERPETUAL",
        10.0,
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
    );
}

/// **The IG profile (split-plane I9, the fourth credentialed-data venue) passes the LIVE gate and
/// mounts on the PAPER seam — no network, no credentials.** `validate_for_live` accepts the
/// runbook pair (`ig`, `CS.D.EURUSD.MINI.IP`) — the new `LIVE_WIRED_VENUES` row agreeing with
/// `vike_tradehub::wired_markets::WIRED_MARKETS`' ig row — and the same profile then trades into ig's OWN paper book
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
    passes_the_live_gate_and_mounts_on_the_paper_seam(
        "ig",
        "CS.D.EURUSD.MINI.IP",
        1.0,
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
    );
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

    let (_halt_dir, sentinel) = own_sentinel("multi-mount", "teardown");
    let mount =
        build_paper_multi_strategy_core_with(resolve_mounts(&profile), opts_pinned_to(&sentinel));
    let handle = mount.handle;
    let outcome =
        run_with_deadline(Vec::new(), Box::new(move || handle.shutdown_and_join()), deadline);
    assert_matches!(
        outcome,
        ShutdownOutcome::Graceful,
        "a two-venue multi-mount core must quiesce inside the deadline, got {outcome:?}"
    );
}
