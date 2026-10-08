//! `summary_line`: the field set, the per-venue totals and the mounted-set scoping.

use super::*;

#[test]
fn summary_line_is_valid_json_with_expected_keys() {
    let snap = CoreSnapshot::empty("polymarket", "TOK");
    let line = summary_line(&snap, "TOK");
    let v: serde_json::Value =
        serde_json::from_str(&line).expect("the summary line must be valid JSON");
    assert_eq!(v["kind"], "summary");
    assert_eq!(v["orders"], 0);
    assert_eq!(v["working"], 0);
    assert_eq!(v["positions"], 0);
    assert_eq!(v["net_pos"], 0.0);
    assert!(v["fault"].is_null(), "no fault on a fresh snapshot");
    assert!(v["trading_state"].is_string());
}

/// The FIELD SET of the summary line is a STDOUT PROTOCOL surface — pinned verbatim, as a set,
/// so the scope fix below cannot quietly add or drop a key that something downstream parses.
/// A key added on purpose is one edited row here; a key added by accident is a red test.
///
/// ⚠ The set CHANGED, deliberately, on 2026-08-17: the single `equity` was replaced by
/// `equity_book`/`equity_wallet`/`wallet_venues`. `equity` was `Portfolio::equity_total`, the
/// sum of the daemon's own book-keeping and a venue's whole-account wallet — see
/// [`summary_line`]'s doc for the 62647.10600813 measured on the CI box. Dropping the NAME rather
/// than redefining it is the point: a consumer keyed on `.equity` gets `null` and breaks
/// loudly instead of silently reading a figure that means nothing.
///
/// ⚠ …and AGAIN on 2026-08-19 (the I10 rehearsal follow-up): `equity_book_mounted` +
/// `mounted_venues` were ADDED. Additive on purpose — every existing key keeps its name, its
/// meaning and its VALUE, so no `jq`/alerting consumer reads a number that changed under it;
/// the mounted-set scoping arrives as its own labelled pair instead
/// (`crate::summary::mounted_book_equity` carries the argument).
#[test]
fn the_summary_line_field_set_is_exactly_these_fifteen_keys() {
    let line = summary_line(&CoreSnapshot::empty("bybit", "BTCUSDT"), "BTCUSDT");
    let v: serde_json::Value = serde_json::from_str(&line).expect("valid JSON");
    let mut keys: Vec<&str> =
        v.as_object().expect("a JSON object").keys().map(|k| k.as_str()).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "equity_book",
            "equity_book_mounted",
            "equity_wallet",
            "fault",
            "fees",
            "kind",
            "mounted_venues",
            "net_pos",
            "orders",
            "positions",
            "realized_pnl",
            "seq",
            "trading_state",
            "wallet_venues",
            "working",
        ],
        "the summary line's field set is a protocol surface — changing it is a deliberate edit"
    );
    assert!(
        !v.as_object().expect("a JSON object").contains_key("equity"),
        "there must be no bare `equity` key: a venue wallet and a book-kept equity are \
             different quantities, and the name that used to carry their sum is retired rather \
             than redefined so a stale consumer fails loudly"
    );
}

/// One venue block, spelled out so the scope test below reads as data rather than a builder.
fn venue_block(
    venue: &str,
    realized: f64,
    fees: f64,
    positions: Vec<vike_core::PositionView>,
) -> vike_core::VenueBlock {
    vike_core::VenueBlock {
        venue: venue.to_string(),
        account: None,
        route_key: venue.to_string(),
        symbol: String::new(),
        extra_symbols: Vec::new(),
        mode: None,
        balance: 0.0,
        realized_pnl: realized,
        fees_paid: fees,
        funding_paid: 0.0,
        balance_mode: vike_exec::BalanceMode::Delta,
        equity: 0.0,
        unrealized: 0.0,
        missing_prices: 0,
        margin_used: 0.0,
        free_bp: 0.0,
        margin_ratio: 0.0,
        fee_schedule: None,
        trading_state: vike_exec::TradingState::Active,
        multipliers: Default::default(),
        multiplier_default: 1.0,
        positions,
    }
}

fn position(venue: &str, symbol: &str, size: f64) -> vike_core::PositionView {
    vike_core::PositionView {
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        position_side: "BOTH".to_string(),
        size,
        avg_px: 100.0,
        unrealized: 0.0,
        mark_source: None,
        leverage: 0.0,
        liq_price: 0.0,
        margin_mode: vike_model::MarginMode::Cross,
        isolated_margin: None,
    }
}

/// ⚠ **The the CI box shape, and the bug this test exists for.** `crate::wired_markets::WIRED_MARKETS` lists
/// binance first, so on a CEX node the PRIMARY engine is the binance PAPER engine — which has
/// traded nothing — while the mount that actually trades is bybit, a NON-primary engine.
/// `CoreSnapshot::build` binds `let acc = &engine.account` (the primary) into the scalar
/// `Portfolio::realized_pnl`/`fees_paid` and `CoreSnapshot::positions`, so a summary line that
/// read those four fields reported binance's silence: measured on the CI box as
/// `fees: 0.0, realized_pnl: 0.0, net_pos: 0.0, positions: 0` on the very minute the bybit mount
/// booked ten maker fills — while `equity` in the SAME line tracked those fills to eight decimal
/// places, which is how we know the engine saw everything and only the REPORT was wrong.
///
/// So this snapshot is built the way `build` builds one on the CI box: primary venue block EMPTY and
/// the primary-mirroring scalars left at 0.0, every traded number living in the SECOND venue
/// block. Every assertion below fails on the pre-fix code.
#[test]
fn the_summary_reports_a_non_primary_venues_fills_not_the_untraded_primarys_silence() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![
        venue_block("binance", 0.0, 0.0, vec![]),
        venue_block("bybit", 12.5, 0.75, vec![position("bybit", "BTCUSDT", -0.25)]),
    ];
    // The primary-mirroring scalars stay exactly as `build` leaves them for an untraded
    // primary — the point is that the line must NOT be reading them.
    assert_eq!(snap.portfolio.realized_pnl, 0.0);
    assert_eq!(snap.portfolio.fees_paid, 0.0);
    assert!(snap.positions.is_empty());

    let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT"))
        .expect("the summary line must be valid JSON");
    assert_eq!(
        v["realized_pnl"], 12.5,
        "realized_pnl must be the CROSS-VENUE total; the primary engine traded nothing"
    );
    assert_eq!(v["fees"], 0.75, "fees must be the CROSS-VENUE total; the primary engine paid none");
    assert_eq!(
        v["positions"], 1,
        "positions must count every venue's rows, not just the primary's"
    );
    assert_eq!(
        v["net_pos"], -0.25,
        "net_pos must net the mount symbol across every venue, not read the primary's leg"
    );
}

/// The other half of "cross-venue": the four widened fields must SUM, not merely find the one
/// venue that happens to be non-empty. A per-venue-block fix that returned the first non-zero
/// row would pass the test above and fail this one.
#[test]
fn the_summary_totals_every_venue_rather_than_picking_one() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![
        venue_block("binance", 4.0, 0.25, vec![position("binance", "BTCUSDT", 2.0)]),
        venue_block("bybit", -1.5, 0.75, vec![position("bybit", "BTCUSDT", -0.5)]),
        venue_block("okx", 0.5, 0.5, vec![position("okx", "ETHUSDT", 3.0)]),
    ];
    let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT"))
        .expect("the summary line must be valid JSON");
    assert_eq!(v["realized_pnl"], 3.0, "4.0 + -1.5 + 0.5");
    assert_eq!(v["fees"], 1.5, "0.25 + 0.75 + 0.5");
    assert_eq!(v["positions"], 3, "one row per venue, ETHUSDT included");
    assert_eq!(
        v["net_pos"], 1.5,
        "BTCUSDT nets +2.0 against -0.5 across venues; the ETHUSDT leg is a different symbol"
    );
}

/// ⚠ **The the CI box shape of 2026-08-17, and the bug this test exists for.** `VIKE_RECONCILE=1`
/// made bybit authoritative; `CoreThread::reconcile_reports` adopted the account's USDT
/// `walletBalance` — 53647, from a SHARED demo account carrying settlements on
/// AUCTIONUSDT/ONDOUSDT/ETHUSDT/WLDUSDT that this daemon never traded — while nine paper mounts
/// still contributed 1000 seed each. The headline `equity` jumped from ~10000 to
/// **62647.10600813**: a venue wallet the daemon does not own, plus paper seed cash, added
/// together and printed as one number.
///
/// The mount's OWN accounting on bybit is the 0.75 of realized PnL and the quarter-coin
/// position beside it — four orders of magnitude away from the wallet. So the report must
/// distinguish the two, and a reader must be able to tell WHICH is which from the key alone.
/// Every assertion below fails on the pre-fix line, which carried neither key.
#[test]
fn an_adopted_venue_wallet_is_never_added_to_book_kept_paper_seed() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    let mut venues =
        vec![vike_core::VenueBlock { equity: 1_000.0, ..venue_block("binance", 0.0, 0.0, vec![]) }];
    for v in ["okx", "hyperliquid", "aster", "deribit", "alpaca", "ctrader", "ig", "oanda"] {
        venues.push(vike_core::VenueBlock { equity: 1_000.0, ..venue_block(v, 0.0, 0.0, vec![]) });
    }
    // The one live mount: its cash is the venue's whole-account wallet, adopted verbatim.
    venues.push(vike_core::VenueBlock {
        balance: 53_647.10600813,
        balance_mode: vike_exec::BalanceMode::Authoritative,
        equity: 53_647.10600813,
        ..venue_block("bybit", 0.75, 0.25, vec![position("bybit", "BTCUSDT", -0.25)])
    });
    snap.portfolio.venues = venues;
    // The conflated figure the old line printed, kept here as the thing NOT to report.
    snap.portfolio.equity_total =
        vike_model::py_sum(snap.portfolio.venues.iter().map(|v| v.equity));
    assert_eq!(snap.portfolio.equity_total, 62_647.10600813, "the measured the CI box headline");

    let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT"))
        .expect("the summary line must be valid JSON");

    assert_eq!(
        v["equity_book"], 9_000.0,
        "the book-kept half is the nine paper mounts' seed and nothing else"
    );
    assert_eq!(
        v["equity_wallet"], 53_647.10600813,
        "the venue-attested half is bybit's whole-account wallet, reported as its own quantity"
    );
    assert_eq!(
        v["wallet_venues"], "bybit",
        "and the line NAMES whose wallet it quoted, so the reader need not open a log"
    );
    // The whole point: no field on this line is the sum of the two.
    for (key, val) in v.as_object().expect("a JSON object") {
        if let Some(f) = val.as_f64() {
            assert_ne!(
                f, 62_647.10600813,
                "`{key}` is the conflated total — a venue wallet and a book-kept equity are \
                     different quantities and no field may add them"
            );
        }
    }
    // The mount's own accounting is still reported, unwidened and unswallowed by the wallet.
    assert_eq!(v["realized_pnl"], 0.75);
    assert_eq!(v["net_pos"], -0.25);
}

/// The other half: on a node where NOTHING has ever attested a balance, the wallet fields must
/// read empty rather than mirroring the book — otherwise a paper daemon reports its seed cash
/// twice, once under each name, and the split says nothing.
#[test]
fn a_pure_paper_node_reports_a_zero_wallet_and_names_no_venue() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![
        vike_core::VenueBlock { equity: 1_000.0, ..venue_block("binance", 0.0, 0.0, vec![]) },
        vike_core::VenueBlock { equity: 2_500.0, ..venue_block("okx", 0.0, 0.0, vec![]) },
    ];
    let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT"))
        .expect("the summary line must be valid JSON");
    assert_eq!(v["equity_book"], 3_500.0);
    assert_eq!(v["equity_wallet"], 0.0, "no venue has attested a balance");
    assert_eq!(v["wallet_venues"], "", "so there is no wallet to name");
}

/// A `MountRowKind::Mount` row for `venue`/`symbol` — the snapshot half of "what this daemon
/// runs", which is what the mounted-set scoping reads.
fn mount_row(venue: &str, symbol: &str) -> vike_core::MountView {
    vike_core::MountView {
        kind: vike_core::MountRowKind::Mount,
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        interval: "1m".to_string(),
        ready: true,
        position: 0.0,
        realized_pnl: 0.0,
        unrealized_pnl: 0.0,
        notional: 0.0,
        budget: None,
        latched: false,
        params: None,
    }
}

/// ⚠ **The I10 REHEARSAL shape, and the observation this pair of keys exists for**
/// (`docs/ops/i10-rehearsal-2026-08-19.md`): two mounts seeded at 10k, ten default-build venue
/// engines each carrying that same seed, and a summary line reading `equity_book: 100000.0`.
///
/// Both halves are asserted, because the fix is that BOTH are reported: `equity_book` keeps
/// its whole-book value (a consumer that already reads it sees no change, and the
/// book/wallet partition still holds), while `equity_book_mounted` answers the question the
/// operator was actually asking — 20000.0 — and `mounted_venues` names the two it scoped to.
#[test]
fn the_summary_scopes_a_mounted_set_figure_beside_the_whole_book() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = [
        "binance",
        "bybit",
        "okx",
        "hyperliquid",
        "aster",
        "deribit",
        "alpaca",
        "ctrader",
        "ig",
        "oanda",
    ]
    .into_iter()
    .map(|v| vike_core::VenueBlock { equity: 10_000.0, ..venue_block(v, 0.0, 0.0, vec![]) })
    .collect();
    snap.mounts = vec![mount_row("binance", "BTCUSDT"), mount_row("bybit", "BTCUSDT")];

    let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT"))
        .expect("the summary line must be valid JSON");
    assert_eq!(
        v["equity_book"], 100_000.0,
        "the whole-book figure is CORRECT and unchanged — ten engines seeded at 10k each"
    );
    assert_eq!(
        v["equity_book_mounted"], 20_000.0,
        "…and the scoped companion is the two MOUNTED venues' seeds, which is the number the \
             rehearsal's operator was reaching for"
    );
    assert_eq!(
        v["mounted_venues"], "binance,bybit",
        "the line NAMES what it scoped to, so the reader need not open a profile"
    );
}

/// The RESIDUAL row is not a mount. `CoreThread::mount_views` appends one
/// `MountRowKind::Residual` row (venue: the empty string) whenever any mount exists, so a
/// scoping that filtered on "has a venue row" rather than on KIND would silently widen the
/// set the day a residual row carried a venue — and would name an empty venue today.
#[test]
fn the_residual_row_is_not_treated_as_a_mount() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![
        vike_core::VenueBlock { equity: 1_000.0, ..venue_block("binance", 0.0, 0.0, vec![]) },
        vike_core::VenueBlock { equity: 2_500.0, ..venue_block("okx", 0.0, 0.0, vec![]) },
    ];
    snap.mounts = vec![
        mount_row("binance", "BTCUSDT"),
        vike_core::MountView { kind: vike_core::MountRowKind::Residual, ..mount_row("", "") },
    ];
    let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT"))
        .expect("the summary line must be valid JSON");
    assert_eq!(v["equity_book_mounted"], 1_000.0, "only the binance mount is scoped in");
    assert_eq!(v["mounted_venues"], "binance", "the residual row names no venue");
}

/// A core that has mounted NOTHING scopes to nothing: `0.0` with an EMPTY name list, which is
/// how a reader tells "scoped to nothing" from "nothing to scope". `equity_book` still
/// reports the seed, so no capital goes unreported by the line as a whole.
#[test]
fn a_mountless_snapshot_scopes_to_zero_and_names_nobody() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues =
        vec![vike_core::VenueBlock { equity: 1_000.0, ..venue_block("binance", 0.0, 0.0, vec![]) }];
    let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT"))
        .expect("the summary line must be valid JSON");
    assert_eq!(v["equity_book"], 1_000.0, "the whole book is still reported");
    assert_eq!(v["equity_book_mounted"], 0.0);
    assert_eq!(v["mounted_venues"], "");
}

/// A MOUNTED venue that has flipped `Authoritative` (reconcile adopted its wallet) is NAMED
/// but contributes NOTHING to the mounted BOOK figure — its equity lives in `equity_wallet`.
/// The scoped figure obeys the same partition `equity_book` does; a mounted-set number that
/// quietly pulled an adopted wallet back into a "book" key would re-commit the exact
/// conflation the 62647.10600813 split exists to prevent.
#[test]
fn a_mounted_venue_on_an_adopted_wallet_is_named_but_not_booked() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![
        vike_core::VenueBlock { equity: 10_000.0, ..venue_block("binance", 0.0, 0.0, vec![]) },
        vike_core::VenueBlock {
            balance: 53_647.10600813,
            balance_mode: vike_exec::BalanceMode::Authoritative,
            equity: 53_647.10600813,
            ..venue_block("bybit", 0.0, 0.0, vec![])
        },
    ];
    snap.mounts = vec![mount_row("binance", "BTCUSDT"), mount_row("bybit", "BTCUSDT")];
    let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT"))
        .expect("the summary line must be valid JSON");
    assert_eq!(
        v["equity_book_mounted"], 10_000.0,
        "bybit's adopted wallet is NOT book-kept equity, mounted or otherwise"
    );
    assert_eq!(v["mounted_venues"], "binance,bybit", "but it IS a mount, and is named as one");
    assert_eq!(v["equity_wallet"], 53_647.10600813, "its equity is reported under the wallet");
    assert_eq!(v["wallet_venues"], "bybit");
}
