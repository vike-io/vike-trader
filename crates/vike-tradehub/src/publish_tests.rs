use super::*;
// `Response`/`write_frame`/`project` come in via `super::*`; only `read_frame` is extra here.
use vike_tradehub_client::proto::read_frame;

/// The identity block is threaded through the projection verbatim (split-plane B3), and its
/// absence stays absent — an identity-less node publishes exactly the old shape.
#[test]
fn project_threads_the_identity_through() {
    let snap = CoreSnapshot::empty("binance", "BTCUSDT");
    let id = WireNodeIdentity {
        name: "n".into(),
        strategy: "s".into(),
        params: "p".into(),
        live: false,
        build: "b".into(),
        advertise_addr: String::new(),
    };
    let wire = project(&snap, Some(&id));
    assert_eq!(wire.identity.as_ref().unwrap().strategy, "s");
    assert!(project(&snap, None).identity.is_none());
}

/// The projection carries the rendered fields the GUI needs: seq, venue/symbol, trading state,
/// equity, and each order (with its status rendered to a string).
#[test]
fn project_maps_the_rendered_fields() {
    let mut snap = CoreSnapshot::empty("polymarket", "TOK");
    snap.seq = 7;
    snap.trading_state = TradingState::Reducing;
    snap.portfolio.equity_total = 1234.5;
    snap.orders.push(OrderView {
        client_order_id: "c-1".into(),
        venue: "polymarket".into(),
        account: None,
        symbol: "TOK".into(),
        side: 1,
        qty: 20.0,
        order_type: "limit".into(),
        price: Some(0.4),
        trigger_price: None,
        status: vike_exec::OrderStatus::Accepted,
        venue_order_id: Some("v-9".into()),
        filled_qty: 0.0,
        avg_fill_px: 0.0,
    });

    let wire = project(&snap, None);
    assert_eq!(wire.seq, 7);
    assert_eq!(wire.venue, "polymarket");
    assert_eq!(wire.symbol, "TOK");
    assert_eq!(wire.trading_state, WireTradingState::Reducing);
    assert_eq!(wire.equity_total.to_bits(), 1234.5_f64.to_bits());
    assert_eq!(wire.orders.len(), 1);
    assert_eq!(wire.orders[0].client_order_id, "c-1");
    assert_eq!(wire.orders[0].status, "Accepted", "status is rendered to its {{:?}} string");
}

/// A framed snapshot decodes back into a `Response::SnapshotFrame` whose wire matches the direct
/// projection — the "serialize once" bytes are a valid frame.
#[test]
fn frame_snapshot_round_trips_as_a_snapshot_frame() {
    let mut snap = CoreSnapshot::empty("sim", "BTCUSDT");
    snap.seq = 3;
    let bytes = frame_snapshot(&snap, None);
    let mut cur = std::io::Cursor::new(bytes.as_slice());
    match read_frame::<_, Response>(&mut cur).expect("decode frame") {
        Response::SnapshotFrame(w) => {
            assert_eq!(w.seq, 3);
            assert_eq!(*w, project(&snap, None));
        }
        other => panic!("expected SnapshotFrame, got {other:?}"),
    }
}

/// The mailbox is bounded and drops the OLDEST frame when full — never the newest, never blocking.
#[test]
fn mailbox_drops_oldest_when_full() {
    let mb = Mailbox::new(2);
    let f = |n: u8| Arc::new(vec![n]);
    mb.push(f(1));
    mb.push(f(2));
    mb.push(f(3)); // drops the oldest (1)
    match mb.recv_timeout(Duration::from_millis(0)) {
        Recv::Frame(b) => assert_eq!(*b, vec![2], "oldest (1) was dropped, 2 is next"),
        _ => panic!("expected a frame"),
    }
    match mb.recv_timeout(Duration::from_millis(0)) {
        Recv::Frame(b) => assert_eq!(*b, vec![3]),
        _ => panic!("expected a frame"),
    }
    // empty now -> a zero timeout reports Timeout, not Closed.
    assert!(matches!(mb.recv_timeout(Duration::from_millis(0)), Recv::Timeout));
    mb.close();
    assert!(matches!(mb.recv_timeout(Duration::from_millis(0)), Recv::Closed));
}

// -----------------------------------------------------------------------------------------
// The bar lane (`project_bar_series`). ⚠ Until these landed, NOTHING in this workspace built a
// `CoreSnapshot` carrying bars and asserted what the projection emits: the only other
// `WireBarSeries` sites are client-side hand-built round-trips, which never reach this filter.
// That is exactly how a structurally empty bar lane shipped with every test green.
// -----------------------------------------------------------------------------------------

fn bar(ts: i64, close: f64) -> Bar {
    Bar {
        ts,
        open: close,
        high: close,
        low: close,
        close,
        volume: 1.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

fn series(closed: Vec<Bar>, forming: Option<Bar>) -> vike_exec::BarSeries {
    vike_exec::BarSeries { closed: Arc::new(closed), forming }
}

fn mount_row(venue: &str, symbol: &str, interval: &str) -> vike_core::MountView {
    vike_core::MountView {
        kind: vike_core::MountRowKind::Mount,
        venue: venue.into(),
        symbol: symbol.into(),
        interval: interval.into(),
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

/// ⚠ **THE LIVE CONFIGURATION, and the case nothing covered.** The daemon mounts bybit;
/// `vike_mount::build_node` makes its hardcoded binance paper engine the PRIMARY, so
/// `CoreSnapshot::venue` reads `"binance"` while the only series the core's bar cache holds is
/// the bybit feed's. The old projection compared the two and published `bars: []` — every
/// frame, forever, on a daemon that was otherwise healthy and publishing ~38 snapshots/second.
#[test]
fn a_bybit_mount_under_a_binance_primary_publishes_its_bars() {
    // `empty`'s arguments ARE the primary engine's (venue, symbol) — the untraded binance one.
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.mounts.push(mount_row("bybit", "BTCUSDT", "1m"));
    snap.bars.insert(
        ("bybit".into(), "BTCUSDT".into(), "1m".into()),
        series(vec![bar(1, 100.0), bar(2, 101.0)], Some(bar(3, 102.0))),
    );

    let wire = project(&snap, None);
    assert_eq!(wire.venue, "binance", "the primary-mirroring scalars are untouched by the fix");
    assert_eq!(
        wire.bars.len(),
        1,
        "the MOUNTED series must be published — this is the whole defect: a bybit-mounted, \
             binance-primary daemon published an empty bar lane"
    );
    let s = &wire.bars[0];
    assert_eq!(
        (s.venue.as_str(), s.symbol.as_str(), s.interval.as_str()),
        ("bybit", "BTCUSDT", "1m")
    );
    assert_eq!(s.closed.len(), 2);
    assert_eq!(
        s.forming.as_ref().expect("the forming candle rides along").c.to_bits(),
        102.0_f64.to_bits()
    );
}

/// A MULTI-mount daemon publishes EVERY series it holds, not one. This is the half that a
/// "filter to `mounts[0]`" repair would have left broken — it would have made the live box work
/// and kept the defect one mount later.
#[test]
fn every_mounted_series_is_published_not_just_one() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.mounts.push(mount_row("bybit", "BTCUSDT", "1m"));
    snap.mounts.push(mount_row("okx", "BTC-USDT", "5m"));
    snap.bars
        .insert(("bybit".into(), "BTCUSDT".into(), "1m".into()), series(vec![bar(1, 1.0)], None));
    snap.bars
        .insert(("okx".into(), "BTC-USDT".into(), "5m".into()), series(vec![bar(1, 2.0)], None));

    let wire = project(&snap, None);
    let mut got: Vec<(&str, &str)> =
        wire.bars.iter().map(|s| (s.venue.as_str(), s.symbol.as_str())).collect();
    got.sort_unstable();
    assert_eq!(got, vec![("bybit", "BTCUSDT"), ("okx", "BTC-USDT")]);
}

/// `mounts` ORDERS the frame, it never FILTERS it: a core with feeds and no strategy mount
/// still publishes its bars. A mount-gated projection would be the same empty chart wearing a
/// new argument.
#[test]
fn a_mount_less_core_still_publishes_its_bars() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.bars
        .insert(("bybit".into(), "BTCUSDT".into(), "1m".into()), series(vec![bar(1, 1.0)], None));
    assert!(snap.mounts.is_empty(), "no mount rows at all");
    assert_eq!(project(&snap, None).bars.len(), 1);
}

/// The frame is bounded in BOTH dimensions, and a truncated one keeps the MOUNTED series. The
/// cap replaces the bounding the removed venue filter was doing by accident; the mounted-first
/// ordering is what makes a truncated frame still the useful one. Every unmounted series is
/// inserted FIRST here, so insertion order alone would have evicted the traded one.
#[test]
fn the_series_cap_bounds_the_frame_and_keeps_the_mounted_series_first() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    for i in 0..(MAX_BAR_SERIES + 4) {
        snap.bars.insert(
            ("binance".into(), format!("ALT{i}"), "1m".into()),
            series(vec![bar(1, 1.0)], None),
        );
    }
    snap.mounts.push(mount_row("bybit", "BTCUSDT", "1m"));
    snap.bars
        .insert(("bybit".into(), "BTCUSDT".into(), "1m".into()), series(vec![bar(1, 9.0)], None));

    let wire = project(&snap, None);
    assert_eq!(wire.bars.len(), MAX_BAR_SERIES, "the frame carries at most the series cap");
    assert_eq!(
        (wire.bars[0].venue.as_str(), wire.bars[0].symbol.as_str()),
        ("bybit", "BTCUSDT"),
        "the mounted series sorts FIRST, so a capped frame still carries what is traded"
    );
}

/// Each series is still tail-sliced to [`CHART_BARS_CAP`] — the per-series bound is unchanged
/// by the filter's removal, and it keeps the TAIL (the recent candles), not the head.
#[test]
fn each_series_is_tail_sliced_to_the_chart_bars_cap() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    let bars: Vec<Bar> = (0..(CHART_BARS_CAP as i64 + 10)).map(|i| bar(i, i as f64)).collect();
    snap.bars.insert(("bybit".into(), "BTCUSDT".into(), "1m".into()), series(bars, None));

    let wire = project(&snap, None);
    assert_eq!(wire.bars[0].closed.len(), CHART_BARS_CAP);
    assert_eq!(wire.bars[0].closed[0].ts, 10, "the TAIL survives, not the head");
}

/// **The published block NAMES its account** — §6.2's snapshot mirror, and the successor to a
/// fence that stood here.
///
/// ⚠ **What this replaced, and why it is a restatement rather than a deletion.** Stage 1 gave
/// `vike_core::snapshot::VenueBlock` its `account` and `route_key`
/// (`docs/superpowers/specs/2026-09-13-the-wire-names-an-account-from-the-misroute.md` §7.1)
/// and this projection forwarded NEITHER. A test called
/// `the_published_venue_block_carries_no_account_field_yet` pinned that absence, and its doc
/// said what would license the change: *"it fails the day somebody adds either field to
/// `WireVenueBlock` **without doing §6.2's consumer half**, which is exactly the shape of
/// change that would let a client believe it can address an account it cannot."*
///
/// §6.2's consumer half is done — `crates/vike-cli/src/cmd/mcp.rs`'s `mounted_accounts` reads
/// `venues[].route_key` and `venue_verdict` refuses an ambiguous venue before a
/// `preview_token` is minted — so the fence has been paid rather than climbed over, and the
/// assertion inverts. The fence FIRED on this change, which is the evidence it was worth
/// having: nothing else in the tree would have asked whether the consumer half existed.
#[test]
fn the_published_venue_block_names_its_account_and_route_key() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![vike_core::VenueBlock {
        venue: "binance".into(),
        account: vike_model::account_keys::AccountLabel::parse("ALT").ok(),
        route_key: "binance#ALT".into(),
        ..Default::default()
    }];
    let json = serde_json::to_string(&project(&snap, None)).expect("the wire serializes");
    assert!(json.contains("\"account\":\"ALT\""), "the LABEL reaches the wire: {json}");
    assert!(json.contains("\"route_key\":\"binance#ALT\""), "…and the ROUTE KEY: {json}");
    assert!(json.contains("\"binance\""), "…and the venue is still published: {json}");
}

/// ⚠ **The DEFAULT account publishes no `account` key and a route key equal to its venue** —
/// which is every node in production today, and the property that makes the two new fields
/// invisible to every existing consumer. Without this, the test above is satisfied by a
/// projection that stamps a label on blocks that have none.
#[test]
fn a_default_account_publishes_no_label_and_a_bare_route_key() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![vike_core::VenueBlock {
        venue: "binance".into(),
        account: None,
        route_key: "binance".into(),
        ..Default::default()
    }];
    let json = serde_json::to_string(&project(&snap, None)).expect("the wire serializes");
    assert!(!json.contains("\"account\""), "no label key at all for the default: {json}");
    assert!(json.contains("\"route_key\":\"binance\""), "the key IS the venue: {json}");
}

/// ⚠ **THE MEASUREMENT the account gate is built on, pinned rather than argued.**
/// [`PublisherHandle::engine_venues`] answers `["binance", "binance"]` on a two-account node —
/// evidence that cannot tell two books apart, and correctly so for the question it answers —
/// while [`PublisherHandle::engine_route_keys`] answers `["binance", "binance#ALT"]` and can.
/// `crate::server::account_refusal` reads the second; `crate::server::venue_refusal` keeps
/// reading the first, because re-pointing it would change what an existing refusal MEANS.
///
/// The third assertion is the one a reader should not skip: both rosters are EMPTY on a cell
/// that has not published, and empty TOGETHER — which is what makes the account gate's
/// inherited "empty means UNKNOWN, never refuse" rule structural rather than a coincidence of
/// two separate reads.
#[test]
fn the_two_rosters_read_the_same_cell_and_differ_only_where_the_account_lives() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![
        vike_core::VenueBlock {
            venue: "binance".into(),
            account: None,
            route_key: "binance".into(),
            ..Default::default()
        },
        vike_core::VenueBlock {
            venue: "binance".into(),
            account: vike_model::account_keys::AccountLabel::parse("ALT").ok(),
            route_key: "binance#ALT".into(),
            ..Default::default()
        },
    ];
    let cell = Arc::new(ArcSwap::from_pointee(snap));
    let handle = spawn(Arc::clone(&cell), None);
    assert_eq!(
        handle.engine_venues(),
        vec!["binance".to_string(), "binance".to_string()],
        "one entry per ENGINE, and the two are the SAME STRING — the defect, not a bug in this \
             accessor"
    );
    assert_eq!(
        handle.engine_route_keys(),
        vec!["binance".to_string(), "binance#ALT".to_string()],
        "…and the route keys tell them apart, which is the whole of what the gate needs"
    );

    // The pre-publish shape: a cell holding no venue block answers EMPTY on BOTH, which both
    // gates read as UNKNOWN.
    let bare = Arc::new(ArcSwap::from_pointee(CoreSnapshot::empty("binance", "BTCUSDT")));
    let quiet = spawn(Arc::clone(&bare), None);
    assert!(quiet.engine_venues().is_empty(), "no venue roster before the first publish");
    assert!(quiet.engine_route_keys().is_empty(), "…and no route-key roster either");

    handle.shutdown();
    quiet.shutdown();
}

/// The projection publishes the engine's symbols (primary first) and its mode.
#[test]
fn the_published_venue_block_names_its_symbols_and_mode() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![vike_core::VenueBlock {
        venue: "binance".into(),
        route_key: "binance".into(),
        symbol: "BTCUSDT".into(),
        extra_symbols: vec!["ETHUSDT".into()],
        mode: Some(vike_exec::EngineMode::Demo),
        ..Default::default()
    }];
    let wire = project(&snap, None);
    assert_eq!(wire.venues[0].symbols, vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()]);
    assert_eq!(wire.venues[0].mode, Some(vike_tradehub_client::wire::WireEngineMode::Demo));
}

/// ⚠ **Each of the three modes reaches the wire as ITSELF.** The test above names one, and this
/// field's whole projection is a three-arm translation, so a swapped pair (a LIVE engine published
/// as demo) would pass it. A GUI states this word to a person who is about to send an order.
#[test]
fn every_engine_mode_is_published_as_itself() {
    for (engine_mode, wire_mode) in [
        (vike_exec::EngineMode::Paper, vike_tradehub_client::wire::WireEngineMode::Paper),
        (vike_exec::EngineMode::Demo, vike_tradehub_client::wire::WireEngineMode::Demo),
        (vike_exec::EngineMode::Live, vike_tradehub_client::wire::WireEngineMode::Live),
    ] {
        let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
        snap.portfolio.venues = vec![vike_core::VenueBlock {
            venue: "binance".into(),
            route_key: "binance".into(),
            symbol: "BTCUSDT".into(),
            mode: Some(engine_mode),
            ..Default::default()
        }];
        assert_eq!(project(&snap, None).venues[0].mode, Some(wire_mode), "{engine_mode:?}");
    }
}

/// ⚠ **"Not said" stays "not said" on the wire.** A block that names no symbol and no mode (an
/// engine the snapshot could not describe) publishes an EMPTY list and no mode: never "trades
/// nothing", never a default of `Live`. Neither key reaches the frame either, so an older client
/// reads exactly the bytes it always read. `VenueBlock::trades` answers `false` for such a block,
/// which is the reader's trap: it has to test for the empty list first.
///
/// The second block pins the one case the wire cannot express: extras with NO primary. They go
/// with the missing primary, because a list whose first entry is not the primary would be read as
/// one.
#[test]
fn a_block_that_names_no_symbol_or_mode_publishes_neither() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![
        vike_core::VenueBlock {
            venue: "binance".into(),
            route_key: "binance".into(),
            ..Default::default()
        },
        vike_core::VenueBlock {
            venue: "okx".into(),
            route_key: "okx".into(),
            extra_symbols: vec!["ETH-USDT".into()],
            ..Default::default()
        },
    ];
    let wire = project(&snap, None);
    // Both blocks are published, or the loop below would pass over nothing.
    assert_eq!(wire.venues.len(), 2);
    for block in &wire.venues {
        assert!(block.symbols.is_empty(), "{}: {:?}", block.venue, block.symbols);
        assert_eq!(block.mode, None, "{}", block.venue);
    }
    let json = serde_json::to_string(&wire).expect("serializes");
    assert!(!json.contains("\"symbols\""), "{json}");
    assert!(!json.contains("\"mode\""), "{json}");
}

/// An order on a labelled account publishes its label, and a default-account order publishes no
/// key at all (its bytes are what they always were).
#[test]
fn a_published_order_names_its_account_only_when_labelled() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    let order = |account| vike_core::snapshot::OrderView {
        client_order_id: "c-1".into(),
        venue: "binance".into(),
        account,
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(99.0),
        trigger_price: None,
        status: vike_exec::OrderStatus::Accepted,
        venue_order_id: None,
        filled_qty: 0.0,
        avg_fill_px: 0.0,
    };
    snap.orders = vec![order(vike_model::account_keys::AccountLabel::parse("SUB").ok())];
    let json = serde_json::to_string(&project(&snap, None)).expect("serializes");
    assert!(json.contains("\"account\":\"SUB\""), "{json}");
    snap.orders = vec![order(None)];
    let json = serde_json::to_string(&project(&snap, None)).expect("serializes");
    assert!(!json.contains("\"account\""), "{json}");
}
