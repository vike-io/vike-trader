//! `summary_line`: the field set, the per-venue totals and the mounted-set scoping.

use super::*;

#[test]
fn summary_line_is_valid_json_with_expected_keys() {
    let snap = CoreSnapshot::empty("polymarket", "TOK");
    let line = summary_line(&snap, "TOK", &[]);
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
fn the_summary_line_field_set_is_exactly_these_seventeen_keys() {
    let line = summary_line(&CoreSnapshot::empty("bybit", "BTCUSDT"), "BTCUSDT", &[]);
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
            "halted_venues",
            "kind",
            "mounted_venues",
            "net_pos",
            "orders",
            "positions",
            "realized_pnl",
            "reducing_venues",
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
    positions: Vec<vike_exec::PositionView>,
) -> vike_exec::VenueBlock {
    vike_exec::VenueBlock {
        venue: venue.to_string(),
        account: None,
        route_key: venue.to_string(),
        symbol: String::new(),
        extra_symbols: Default::default(),
        tier: None,
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
        trading_state: vike_model::TradingState::Active,
        contract_values: Default::default(),
        contract_value_default: vike_model::ContractValue::default(),
        admits_symbols: false,
        quote_rates: Vec::new(),
        quote_rates_in: Default::default(),
        ledger_ccy: Default::default(),
        ccy_assumed: false,
        colliding_fills_refused: 0,
        fees_unconverted: 0,
        fees_at_stale_rate: 0,
        positions,
    }
}

fn position(venue: &str, symbol: &str, size: f64) -> vike_exec::PositionView {
    vike_exec::PositionView {
        venue: venue.into(),
        symbol: symbol.into(),
        position_side: vike_model::events::PositionSide::Both,
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

/// ⚠ **The the CI box shape, and the bug this test exists for.** `vike_tradehub_core::wired_markets` lists
/// binance first, so on a CEX node the PRIMARY engine is the binance PAPER engine — which has
/// traded nothing — while the mount that actually trades is bybit, a NON-primary engine.
/// `vike_core::snapshot::build` binds `let acc = &engine.account` (the primary) into the scalar
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

    let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT", &[]))
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

/// ⚠ **Decision 0180's blind spot.** A venue-scoped link dead-man halt halts ONE venue's engines, and
/// `trading_state` is the primary's, so with a healthy primary the line said `Active` while bybit
/// refused every opening order. The two `*_venues` strings name the engines whose own block says
/// otherwise; and the reverse (a dead PRIMARY venue) names it while the others stay out of the list.
#[test]
fn a_venue_scoped_halt_is_named_beside_the_primarys_state() {
    use vike_model::TradingState;
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![
        venue_block("binance", 0.0, 0.0, vec![]),
        vike_exec::VenueBlock {
            trading_state: TradingState::Halted,
            route_key: "bybit#ALT".to_string(),
            ..venue_block("bybit", 0.0, 0.0, vec![])
        },
        vike_exec::VenueBlock {
            trading_state: TradingState::Reducing,
            ..venue_block("okx", 0.0, 0.0, vec![])
        },
    ];
    let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT", &[])).unwrap();
    assert_eq!(v["trading_state"], "Active", "the primary's own state is untouched");
    assert_eq!(v["halted_venues"], "bybit#ALT");
    assert_eq!(v["reducing_venues"], "okx");

    snap.portfolio.venues[0].trading_state = TradingState::Halted;
    let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT", &[])).unwrap();
    assert_eq!(v["halted_venues"], "binance,bybit#ALT");
}

/// An engine superseded by a newer block of its route key (stopped, then started again) is not
/// reported: the frame carries the NEWEST block of each key, and so does this line.
#[test]
fn a_superseded_halted_block_is_not_named() {
    use vike_model::TradingState;
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![
        vike_exec::VenueBlock {
            trading_state: TradingState::Halted,
            ..venue_block("bybit", 0.0, 0.0, vec![])
        },
        venue_block("bybit", 0.0, 0.0, vec![]),
    ];
    let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT", &[])).unwrap();
    assert_eq!(v["halted_venues"], "");
}

/// **A STOPPED account is not a halted venue.** A stopped account that was not started again keeps its last
/// block, and a `Halted` one would read as a halt for ever; the node's NOT RUNNING list (a stop adds the
/// account before the engine goes) names it, and the line skips it, `reducing_venues` included.
#[test]
fn a_stopped_account_is_not_named_halted_or_reducing() {
    use vike_model::TradingState;
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![
        vike_exec::VenueBlock {
            trading_state: TradingState::Halted,
            route_key: "bybit#ALT".to_string(),
            ..venue_block("bybit", 0.0, 0.0, vec![])
        },
        vike_exec::VenueBlock {
            trading_state: TradingState::Halted,
            ..venue_block("ibkr", 0.0, 0.0, vec![])
        },
        vike_exec::VenueBlock {
            trading_state: TradingState::Reducing,
            route_key: "okx#OLD".to_string(),
            ..venue_block("okx", 0.0, 0.0, vec![])
        },
    ];
    let stopped = |venue: &str, label: &str| vike_exec::NotRunningAccount {
        venue: venue.to_string(),
        account: None,
        route_key: format!("{venue}#{label}"),
        tier: None,
        reason: "account.active = 0".to_string(),
    };
    let stopped = [stopped("bybit", "ALT"), stopped("okx", "OLD")];
    let v: serde_json::Value =
        serde_json::from_str(&summary_line(&snap, "BTCUSDT", &stopped)).unwrap();
    assert_eq!(v["halted_venues"], "ibkr", "only the engine that still runs");
    assert_eq!(v["reducing_venues"], "");
    let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT", &[])).unwrap();
    assert_eq!(v["halted_venues"], "bybit#ALT,ibkr", "with no list, as before");
    assert_eq!(v["reducing_venues"], "okx#OLD");
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
    let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT", &[]))
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
        vec![vike_exec::VenueBlock { equity: 1_000.0, ..venue_block("binance", 0.0, 0.0, vec![]) }];
    for v in ["okx", "hyperliquid", "aster", "deribit", "alpaca", "ctrader", "ig", "oanda"] {
        venues.push(vike_exec::VenueBlock { equity: 1_000.0, ..venue_block(v, 0.0, 0.0, vec![]) });
    }
    // The one live mount: its cash is the venue's whole-account wallet, adopted verbatim.
    venues.push(vike_exec::VenueBlock {
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

    let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT", &[]))
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
        vike_exec::VenueBlock { equity: 1_000.0, ..venue_block("binance", 0.0, 0.0, vec![]) },
        vike_exec::VenueBlock { equity: 2_500.0, ..venue_block("okx", 0.0, 0.0, vec![]) },
    ];
    let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT", &[]))
        .expect("the summary line must be valid JSON");
    assert_eq!(v["equity_book"], 3_500.0);
    assert_eq!(v["equity_wallet"], 0.0, "no venue has attested a balance");
    assert_eq!(v["wallet_venues"], "", "so there is no wallet to name");
}

/// **A wallet is counted once, however often its account was restarted** (drill `n078-B1`, the CI box, 2026-10-19:
/// one deactivate and activate of binance's demo account, then its lane twin). The core's list of engines only
/// grows, so the stopped spot engine's block stays beside the new one's, and both adopted the one venue wallet:
/// `wallet_venues` read `binance` three times and `equity_wallet` was 4984.80 too high. Only the NEWEST block of
/// each route key counts, as in the rows the wire carries; a stopped account that never started again keeps
/// its block (its position is still valued, decision 0169 item 5), and a node that restarted nothing prints
/// the figure the core's own halves give, bit for bit.
#[test]
fn a_restarted_accounts_stopped_engine_does_not_count_its_wallet_twice() {
    let wallet = |venue: &str, key: &str, equity: f64| vike_exec::VenueBlock {
        route_key: key.to_string(),
        balance: equity,
        equity,
        balance_mode: vike_exec::BalanceMode::Authoritative,
        ..venue_block(venue, 0.0, 0.0, vec![])
    };
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![
        wallet("binance", "binance", 4_984.80324635), // stopped by the deactivate, never removed
        wallet("okx", "okx", 4_999.625219769523),
        wallet("binance", "binance", 4_984.80324635), // the same account started again: the same wallet
        wallet("binance", "binance#PERP", 4_996.00829587), // the lane twin: the futures wallet
    ];
    let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT", &[]))
        .expect("the summary line must be valid JSON");
    assert_eq!(v["wallet_venues"], "okx,binance,binance", "the stopped spot engine is not named");
    assert_eq!(
        v["equity_wallet"],
        vike_model::py_sum([4_999.625219769523, 4_984.80324635, 4_996.00829587]),
        "each wallet once"
    );

    // CONTROL: nothing superseded, so the line is the core's own halves, bit for bit.
    snap.portfolio.venues.remove(0);
    let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT", &[]))
        .expect("the summary line must be valid JSON");
    assert_eq!(
        v["equity_wallet"].as_f64().map(f64::to_bits),
        Some(snap.portfolio.equity_wallet_total().to_bits())
    );
    assert_eq!(v["wallet_venues"], snap.portfolio.wallet_venues().join(","));
}

/// A `MountRowKind::Mount` row for `venue`/`symbol` — the snapshot half of "what this daemon
/// runs", which is what the mounted-set scoping reads.
fn mount_row(venue: &str, symbol: &str) -> vike_exec::MountView {
    vike_exec::MountView {
        kind: vike_exec::MountRowKind::Mount,
        mount_id: format!("{venue}__{symbol}__1m"),
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
        ccy: Default::default(),
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
    .map(|v| vike_exec::VenueBlock { equity: 10_000.0, ..venue_block(v, 0.0, 0.0, vec![]) })
    .collect();
    snap.mounts = vec![mount_row("binance", "BTCUSDT"), mount_row("bybit", "BTCUSDT")].into();

    let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT", &[]))
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
        vike_exec::VenueBlock { equity: 1_000.0, ..venue_block("binance", 0.0, 0.0, vec![]) },
        vike_exec::VenueBlock { equity: 2_500.0, ..venue_block("okx", 0.0, 0.0, vec![]) },
    ];
    snap.mounts = vec![
        mount_row("binance", "BTCUSDT"),
        vike_exec::MountView {
            kind: vike_exec::MountRowKind::Residual,
            mount_id: String::new(),
            ..mount_row("", "")
        },
    ]
    .into();
    let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT", &[]))
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
        vec![vike_exec::VenueBlock { equity: 1_000.0, ..venue_block("binance", 0.0, 0.0, vec![]) }];
    let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT", &[]))
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
        vike_exec::VenueBlock { equity: 10_000.0, ..venue_block("binance", 0.0, 0.0, vec![]) },
        vike_exec::VenueBlock {
            balance: 53_647.10600813,
            balance_mode: vike_exec::BalanceMode::Authoritative,
            equity: 53_647.10600813,
            ..venue_block("bybit", 0.0, 0.0, vec![])
        },
    ];
    snap.mounts = vec![mount_row("binance", "BTCUSDT"), mount_row("bybit", "BTCUSDT")].into();
    let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT", &[]))
        .expect("the summary line must be valid JSON");
    assert_eq!(
        v["equity_book_mounted"], 10_000.0,
        "bybit's adopted wallet is NOT book-kept equity, mounted or otherwise"
    );
    assert_eq!(v["mounted_venues"], "binance,bybit", "but it IS a mount, and is named as one");
    assert_eq!(v["equity_wallet"], 53_647.10600813, "its equity is reported under the wallet");
    assert_eq!(v["wallet_venues"], "bybit");
}

/// A block of `venue` in the currency group `ccy` (`""` = the dollar group) with these figures.
fn ccy_block(
    venue: &str,
    ccy: &str,
    equity: f64,
    realized: f64,
    fees: f64,
) -> vike_exec::VenueBlock {
    vike_exec::VenueBlock {
        ledger_ccy: ccy.into(),
        equity,
        ..venue_block(venue, realized, fees, vec![])
    }
}

/// ⚠ **A EUR engine's figures are never added to the dollar line** (decision 0183, item 13). Every
/// money key is the PRIMARY block's group alone, and the EUR group rides beside it in `other_ledgers`.
#[test]
fn a_eur_engine_is_listed_beside_the_dollar_line_never_added_to_it() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![
        ccy_block("binance", "", 1_000.5, 12.5, 0.75),
        ccy_block("oanda", "EUR", 7_000.25, -300.0, 4.5),
    ];
    let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT", &[]))
        .expect("the summary line must be valid JSON");
    assert_eq!(v["realized_pnl"], 12.5, "the dollar group's realized, without the EUR -300");
    assert_eq!(v["fees"], 0.75);
    assert_eq!(v["equity_book"], 1_000.5, "without the EUR 7000.25");
    assert_eq!(v["ledger_ccy"], "USD");
    let others = v["other_ledgers"].as_array().expect("the other groups are listed");
    assert_eq!(others.len(), 1);
    assert_eq!(others[0]["ccy"], "EUR");
    assert_eq!(others[0]["realized_pnl"], -300.0);
    assert_eq!(others[0]["fees"], 4.5);
    assert_eq!(others[0]["equity_book"], 7_000.25);
}

/// A node of ONE currency writes neither key, however its dollars are spelled: its line is the one it
/// always was (the seventeen-key set above is the protocol).
#[test]
fn one_currency_writes_no_ledger_keys() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues =
        vec![ccy_block("binance", "", 1_000.5, 12.5, 0.75), ccy_block("bybit", "", 10.0, 1.0, 0.0)];
    let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT", &[]))
        .expect("the summary line must be valid JSON");
    let keys = v.as_object().expect("an object");
    assert!(!keys.contains_key("ledger_ccy") && !keys.contains_key("other_ledgers"), "{v}");
    assert!(!keys.contains_key("capital_base") && !keys.contains_key("drawdown_curve"), "{v}");
    assert_eq!(v["realized_pnl"], 13.5, "both dollar blocks");
}

/// ⚠ **A stopped engine is not counted twice in a listed group** (the account stopped and started again, so the
/// core holds its tombstone AND its new engine under one route key): the other-group figures are over the
/// CURRENT blocks, as the primary group's are, so the EUR group reads the new block alone.
#[test]
fn a_restarted_eur_account_is_listed_once() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![
        ccy_block("binance", "", 1_000.5, 12.5, 0.75),
        ccy_block("oanda", "EUR", 7_000.0, -100.0, 1.5), // stopped
        ccy_block("oanda", "EUR", 6_900.0, -300.0, 4.5), // started again: the one that counts
    ];
    let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT", &[]))
        .expect("the summary line must be valid JSON");
    let others = v["other_ledgers"].as_array().expect("the EUR group is listed");
    assert_eq!(others.len(), 1);
    assert_eq!(others[0]["realized_pnl"], -300.0, "the new engine alone, not -400");
    assert_eq!(others[0]["fees"], 4.5, "not 6.0");
    assert_eq!(others[0]["equity_book"], 6_900.0, "not 13900");
}

/// An account that started again in ANOTHER currency leaves its EUR tombstone as the group's only block, which
/// is superseded: nothing current is in EUR, so the line lists no EUR group (the core's latch still judges it).
#[test]
fn a_group_with_only_a_superseded_block_is_not_listed() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![
        ccy_block("binance", "", 1_000.5, 12.5, 0.75),
        ccy_block("oanda", "EUR", 7_000.0, -100.0, 1.5), // stopped
        ccy_block("oanda", "", 90.0, 0.0, 0.0),          // started again, now a dollar account
    ];
    let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "BTCUSDT", &[]))
        .expect("the summary line must be valid JSON");
    let keys = v.as_object().expect("an object");
    assert!(!keys.contains_key("other_ledgers"), "{v}");
    assert_eq!(v["realized_pnl"], 12.5, "the dollar group over the current blocks");
}

/// A EUR primary makes the line EUR, and the dollar engine is the one listed beside it.
#[test]
fn a_eur_primary_makes_the_line_eur() {
    let mut snap = CoreSnapshot::empty("oanda", "EURUSD");
    snap.portfolio.venues = vec![
        ccy_block("oanda", "EUR", 7_000.25, -300.0, 4.5),
        ccy_block("binance", "", 1_000.5, 12.5, 0.75),
    ];
    let v: serde_json::Value = serde_json::from_str(&summary_line(&snap, "EURUSD", &[]))
        .expect("the summary line must be valid JSON");
    assert_eq!(v["ledger_ccy"], "EUR");
    assert_eq!(v["realized_pnl"], -300.0);
    assert_eq!(v["other_ledgers"][0]["ccy"], "USD");
    assert_eq!(v["other_ledgers"][0]["realized_pnl"], 12.5);
}

/// What a reader written BEFORE the latch figures existed knows of one `other_ledgers` object: the six keys it
/// always carried, and nothing else (no `deny_unknown_fields`, as a `jq` filter has none).
#[derive(serde::Deserialize)]
struct OtherLedgerBeforeTheLatchFigures {
    ccy: String,
    realized_pnl: f64,
    fees: f64,
    equity_book: f64,
    equity_wallet: f64,
    wallet_venues: String,
}

/// A node of a dollar primary (16,000 of configured seeds, 12.5 won) and FXCM's EUR demo whose wallet of
/// 49,986 EUR the latch adopted as its base, once the sweep has measured both groups (`measured`).
fn latch_snapshot(measured: bool) -> CoreSnapshot {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![
        ccy_block("binance", "", 1_000.5, 12.5, 0.0),
        vike_exec::VenueBlock {
            balance_mode: vike_exec::BalanceMode::Authoritative,
            ..ccy_block("fxcm", "EUR", 49_985.25, -0.5, 0.25)
        },
    ];
    snap.portfolio.capital_base = 16_000.0;
    if measured {
        // The latch's last sweep: base 49,986 + own PnL (-0.5 - 0.25) = 49,985.25, peak seeded at the base.
        snap.portfolio.group_curves = vec![vike_exec::GroupCurve {
            ccy: "EUR".into(),
            base: 49_986.0,
            curve: 49_985.25,
            peak: 49_986.0,
        }];
    }
    snap
}

/// ⚠ **The latch's per-group base, peak and curve ride the summary line** so the adopted-wallet base of the
/// v0.1.95 fix can be read live with no trade and no restart: a EUR group of a 49,986 EUR wallet reads about
/// 49,986, not the 1,000 of one `seed_cash`. RED before the keys existed (every one read `null`).
#[test]
fn the_eur_group_shows_the_base_peak_and_curve_the_latch_measures() {
    let v: serde_json::Value =
        serde_json::from_str(&summary_line(&latch_snapshot(true), "BTCUSDT", &[]))
            .expect("the summary line must be valid JSON");
    let eur = &v["other_ledgers"][0];
    assert_eq!(eur["ccy"], "EUR");
    assert_eq!(eur["base"], 49_986.0, "the adopted wallet, not the 1000 seed: {v}");
    assert_eq!(eur["peak"], 49_986.0);
    assert_eq!(eur["curve"], 49_985.25);
    // The primary (dollar) group: its real base, the sum of its configured seeds, and its curve.
    assert_eq!(v["capital_base"], 16_000.0);
    assert_eq!(v["drawdown_curve"], 16_012.5, "capital base + the dollar group's own PnL");
    assert!(v.get("peak").is_none(), "no top-level `peak`: the primary's is `drawdown_peak`: {v}");
    assert!(v.get("drawdown_peak").is_none(), "the sweep published no row for the primary: {v}");
}

/// ⚠ **The printed `base` is the one the latch FOLDED, not one derived from the published blocks.** The line used
/// to print the curve less the group's own PnL (`Portfolio::pnl_in`), which is right only while the group's PnL
/// has not moved since the 30 s sweep: here a 125 EUR open loss appeared after the sweep, and the derived base
/// read 50,110 for a wallet of 49,986. RED before `GroupCurve::base` existed (the derived figure was printed).
#[test]
fn the_printed_base_is_the_latchs_own_even_when_the_group_has_open_pnl() {
    let mut snap = latch_snapshot(true);
    snap.portfolio.venues[1].unrealized = -125.0; // booked after the sweep measured the curve
    let v: serde_json::Value =
        serde_json::from_str(&summary_line(&snap, "BTCUSDT", &[])).expect("valid JSON");
    let eur = &v["other_ledgers"][0];
    assert_eq!(eur["base"], 49_986.0, "the base the latch folded, not curve less the PnL: {v}");
    assert_eq!(eur["curve"], 49_985.25, "the curve stays the sweep's");
}

/// ⚠ **The primary group shows its high-water mark too**: `drawdown_peak` beside `capital_base` and
/// `drawdown_curve`, the latch's own, once the sweep has judged the group; it is the dollar row of
/// `Portfolio::group_curves` (the EUR row is still `other_ledgers[0].peak`). RED before `state_groups` published
/// the primary (no such key).
#[test]
fn the_primary_group_shows_its_peak_once_the_latch_has_judged_it() {
    let mut snap = latch_snapshot(true);
    snap.portfolio.group_curves.insert(
        0,
        vike_exec::GroupCurve {
            ccy: vike_exec::Interned::default(),
            base: 16_000.0,
            curve: 16_012.5,
            peak: 16_020.0,
        },
    );
    let v: serde_json::Value =
        serde_json::from_str(&summary_line(&snap, "BTCUSDT", &[])).expect("valid JSON");
    assert_eq!(v["drawdown_peak"], 16_020.0, "{v}");
    assert_eq!(v["capital_base"], 16_000.0);
    assert_eq!(v["other_ledgers"][0]["peak"], 49_986.0, "the EUR row is still its own: {v}");
    assert_eq!(
        v["other_ledgers"].as_array().expect("array").len(),
        1,
        "the primary is not listed: {v}"
    );
}

/// A group the latch has not measured (its first sweep is 30 s away, or `max_drawdown` is off) carries NONE of
/// the three keys, never zero, and a reader of the older keys parses the line, measured or not.
#[test]
fn an_unmeasured_group_has_no_latch_keys_and_an_older_reader_parses_either_line() {
    for measured in [false, true] {
        let line = summary_line(&latch_snapshot(measured), "BTCUSDT", &[]);
        let v: serde_json::Value = serde_json::from_str(&line).expect("valid JSON");
        let eur = v["other_ledgers"][0].as_object().expect("an object");
        for key in ["base", "peak", "curve"] {
            assert_eq!(eur.contains_key(key), measured, "{key} with measured={measured}: {v}");
        }
        let old: Vec<OtherLedgerBeforeTheLatchFigures> =
            serde_json::from_value(v["other_ledgers"].clone()).expect("the older shape parses");
        assert_eq!(old.len(), 1);
        let eur = &old[0];
        assert_eq!((eur.ccy.as_str(), eur.realized_pnl, eur.fees), ("EUR", -0.5, 0.25));
        assert_eq!((eur.equity_book, eur.equity_wallet), (0.0, 49_985.25));
        assert_eq!(eur.wallet_venues, "fxcm");
    }
}

/// A curve the latch measured for a group the line does not list (a superseded tombstone, or the primary)
/// is not printed under another group.
#[test]
fn a_curve_of_an_unlisted_group_is_not_printed() {
    let mut snap = latch_snapshot(true);
    snap.portfolio.group_curves.push(vike_exec::GroupCurve {
        ccy: "GBP".into(),
        base: 4.0,
        curve: 5.0,
        peak: 6.0,
    });
    let v: serde_json::Value =
        serde_json::from_str(&summary_line(&snap, "BTCUSDT", &[])).expect("valid JSON");
    assert_eq!(v["other_ledgers"].as_array().expect("array").len(), 1);
    assert_eq!(v["other_ledgers"][0]["peak"], 49_986.0, "the EUR group's own: {v}");
}
