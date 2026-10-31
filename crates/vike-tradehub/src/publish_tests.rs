use super::*;
use std::assert_matches;
use vike_marketdata::test_support::flat_bar_unit_volume;
// `Response`/`write_frame`/`project` come in via `super::*`; only `read_frame` is extra here.
use vike_tradehub_client::proto::read_frame;

/// The identity block is threaded through the projection verbatim (split-plane B3), and its
/// absence stays absent — an identity-less node publishes exactly the old shape.
#[test]
fn project_threads_the_identity_through() {
    let snap = CoreSnapshot::empty("binance", "BTCUSDT");
    let id = WireHubIdentity {
        name: "n".into(),
        strategy: "s".into(),
        params: "p".into(),
        live: false,
        build: "b".into(),
        advertise_addr: String::new(),
    };
    let wire = project(&snap, Some(&id), &[]);
    assert_eq!(wire.identity.as_ref().unwrap().strategy, "s");
    assert!(project(&snap, None, &[]).identity.is_none());
}

/// The armed core-held stops cross the projection whole (the row's `route_key` is the wire's `account`), and a
/// node that holds none writes no key at all: its frame is byte-identical to one from before the field.
#[test]
fn project_lists_the_armed_core_held_stops_and_writes_no_key_for_none() {
    let mut snap = CoreSnapshot::empty("polymarket", "TOK");
    let none = serde_json::to_string(&project(&snap, None, &[])).expect("serializes");
    assert!(!none.contains("core_held_stops"), "{none}");

    snap.core_held_stops = vec![vike_exec::CoreHeldStopView {
        arm_id: "s0a3".into(),
        venue: "polymarket".into(),
        symbol: "TOK".into(),
        route_key: "polymarket#ALT".into(),
        side: -1,
        qty: 4.0,
        trigger_price: 0.35,
        held: true,
        rearms: 0,
    }];
    let wire = project(&snap, None, &[]);
    assert_eq!(
        wire.core_held_stops,
        vec![WireCoreHeldStop {
            arm_id: "s0a3".into(),
            venue: "polymarket".into(),
            symbol: "TOK".into(),
            account: "polymarket#ALT".into(),
            side: -1,
            qty: 4.0,
            trigger_price: 0.35,
            held: true,
            rearms: 0,
        }]
    );
    let json = serde_json::to_string(&wire).expect("serializes");
    assert!(json.contains("\"held\":true") && !json.contains("rearms"), "{json}");
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
        data_delay: None,
        client_order_id: "c-1".into(),
        venue: "polymarket".into(),
        account: None,
        symbol: "TOK".into(),
        side: 1,
        qty: 20.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(0.4),
        trigger_price: None,
        status: vike_model::OrderStatus::Accepted,
        venue_order_id: Some("v-9".into()),
        filled_qty: 0.0,
        avg_fill_px: 0.0,
        reason: None,
        pending_price: None,
    });

    let wire = project(&snap, None, &[]);
    assert_eq!(wire.seq, 7);
    assert_eq!(wire.venue, "polymarket");
    assert_eq!(wire.symbol, "TOK");
    assert_eq!(wire.trading_state, WireTradingState::Reducing);
    assert_eq!(wire.equity_total.to_bits(), 1234.5_f64.to_bits());
    assert_eq!(wire.orders.len(), 1);
    assert_eq!(wire.orders[0].client_order_id, "c-1");
    assert_eq!(wire.orders[0].status, "ACCEPTED", "status is rendered to its FSM word (`as_str`)");
}

/// A framed snapshot decodes back into a `Response::SnapshotFrame` whose wire matches the direct
/// projection — the "serialize once" bytes are a valid frame.
#[test]
fn frame_snapshot_round_trips_as_a_snapshot_frame() {
    let mut snap = CoreSnapshot::empty("sim", "BTCUSDT");
    snap.seq = 3;
    let bytes = frame_snapshot(&snap, None, &[], &[], &NodeFacts::default());
    let mut cur = std::io::Cursor::new(bytes.as_slice());
    match read_frame::<_, Response>(&mut cur).expect("decode frame") {
        Response::SnapshotFrame(w) => {
            assert_eq!(w.seq, 3);
            // A frame is the projection plus what the publisher reads off the node: the uptime is
            // always stamped, and a publisher never handed a sentinel path says nothing about it.
            assert!(w.uptime_ms.is_some());
            assert_eq!(w.halt_sentinel, None);
            let mut expected = project(&snap, None, &[]);
            expected.uptime_ms = w.uptime_ms;
            assert_eq!(*w, expected);
        }
        other => panic!("expected SnapshotFrame, got {other:?}"),
    }
}

/// The key-changed list is stamped into the frame beside `not_running` (route keys only), and a frame
/// without any carries none.
#[test]
fn frame_snapshot_stamps_the_key_changed_route_keys() {
    let snap = CoreSnapshot::empty("sim", "BTCUSDT");
    let framed = |keys: &[String]| match read_frame::<_, Response>(&mut std::io::Cursor::new(
        frame_snapshot(&snap, None, &[], keys, &NodeFacts::default()).as_slice().to_vec(),
    ))
    .expect("decode frame")
    {
        Response::SnapshotFrame(w) => w.key_changed,
        other => panic!("expected SnapshotFrame, got {other:?}"),
    };
    assert!(framed(&[]).is_empty());
    assert_eq!(framed(&["binance#ALT".to_string()]), vec!["binance#ALT".to_string()]);
}

/// **A frame lists the accounts whose venue balance the node holds back, for the engines that RUN**: the lease
/// registry's gates report them (`LeaseFacts::set_balance_held`), the publisher stamps them beside the rest of its
/// own readings, a hold whose engine has no block in the frame is history and not listed, a publisher with no lease
/// registry lists none, and a frame that lists none carries no key (byte-identical to an older node's).
#[test]
fn frame_snapshot_stamps_the_balance_held_of_the_running_accounts_only() {
    use crate::remote_backend::facts::LeaseFacts;
    let mut snap = CoreSnapshot::empty("oanda", "EURUSD");
    snap.portfolio.venues = vec![vike_exec::VenueBlock {
        venue: "oanda".into(),
        route_key: "oanda".into(),
        ..Default::default()
    }];
    let source = Arc::new(LeaseFacts::default());
    let facts = NodeFacts::default();
    assert!(facts.held_source.set(source.clone()).is_ok());

    let framed = |facts: &NodeFacts| {
        let bytes = frame_snapshot(&snap, None, &[], &[], facts);
        let json = String::from_utf8_lossy(&bytes).into_owned();
        match read_frame::<_, Response>(&mut std::io::Cursor::new(bytes.as_slice().to_vec()))
            .expect("decode frame")
        {
            Response::SnapshotFrame(w) => (w.balance_held, json),
            other => panic!("expected SnapshotFrame, got {other:?}"),
        }
    };
    let (none, json) = framed(&facts);
    assert!(none.is_empty() && !json.contains("balance_held"), "nothing held: no key: {json}");

    source.set_balance_held("oanda", Some("assumed USD"));
    source.set_balance_held("fxcm", Some("stated EUR not anchored"));
    let (held, json) = framed(&facts);
    assert_eq!(
        held,
        vec![WireBalanceHeld { route_key: "oanda".into(), reason: "assumed USD".into() }],
        "fxcm has no engine block in this frame: its hold is not listed"
    );
    assert!(json.contains("\"balance_held\""), "{json}");

    let (none, _) = framed(&NodeFacts::default());
    assert!(none.is_empty(), "a publisher with no lease registry lists none");
}

/// **A frame lists the venue paths that are down, for the engines that RUN**: the lease worker's poll reports them
/// (`LeaseFacts::set_venue_link_down`), the publisher stamps them beside `balance_held`, an outage whose engine has
/// no block in the frame is history and not listed, an outage that ended is not listed, a publisher with no lease
/// registry lists none, and a frame that lists none carries no key (byte-identical to an older node's).
#[test]
fn frame_snapshot_stamps_the_down_venue_paths_of_the_running_accounts_only() {
    use crate::remote_backend::facts::LeaseFacts;
    use vike_tradehub_client::wire::WireVenueLinkDown;
    let mut snap = CoreSnapshot::empty("dukascopy", "EURUSD");
    snap.portfolio.venues = vec![vike_exec::VenueBlock {
        venue: "dukascopy".into(),
        route_key: "dukascopy".into(),
        ..Default::default()
    }];
    let source = Arc::new(LeaseFacts::default());
    let facts = NodeFacts::default();
    assert!(facts.held_source.set(source.clone()).is_ok());
    let down = |key: &str| WireVenueLinkDown {
        route_key: key.into(),
        why: "its output ended and the process exited".into(),
        failed_restarts: 0,
        retry_in_ms: 5_000,
    };

    let framed = |facts: &NodeFacts| {
        let bytes = frame_snapshot(&snap, None, &[], &[], facts);
        let json = String::from_utf8_lossy(&bytes).into_owned();
        match read_frame::<_, Response>(&mut std::io::Cursor::new(bytes.as_slice().to_vec()))
            .expect("decode frame")
        {
            Response::SnapshotFrame(w) => (w.venue_link_down, json),
            other => panic!("expected SnapshotFrame, got {other:?}"),
        }
    };
    let (none, json) = framed(&facts);
    assert!(none.is_empty() && !json.contains("venue_link_down"), "all up: no key: {json}");

    source.set_venue_link_down("dukascopy", Some(down("dukascopy")));
    source.set_venue_link_down("fxcm", Some(down("fxcm")));
    let (listed, json) = framed(&facts);
    assert_eq!(
        listed,
        vec![down("dukascopy")],
        "fxcm has no engine block in this frame: its outage is not listed"
    );
    assert!(json.contains("\"venue_link_down\""), "{json}");

    source.set_venue_link_down("dukascopy", None);
    let (back, json) = framed(&facts);
    assert!(back.is_empty() && !json.contains("venue_link_down"), "an ended outage is not listed");

    source.set_venue_link_down("dukascopy", Some(down("dukascopy")));
    let (none, _) = framed(&NodeFacts::default());
    assert!(none.is_empty(), "a publisher with no lease registry lists none");
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
    assert_matches!(mb.recv_timeout(Duration::from_millis(0)), Recv::Timeout);
    mb.close();
    assert_matches!(mb.recv_timeout(Duration::from_millis(0)), Recv::Closed);
}

// -----------------------------------------------------------------------------------------
// The bar lane (`project_bar_series`). ⚠ Until these landed, NOTHING in this workspace built a
// `CoreSnapshot` carrying bars and asserted what the projection emits: the only other
// `WireBarSeries` sites are client-side hand-built round-trips, which never reach this filter.
// That is exactly how a structurally empty bar lane shipped with every test green.
// -----------------------------------------------------------------------------------------

fn series(closed: Vec<Bar>, forming: Option<Bar>) -> vike_exec::BarSeries {
    vike_exec::BarSeries { closed: Arc::new(closed), forming }
}

fn mount_row(venue: &str, symbol: &str, interval: &str) -> vike_exec::MountView {
    vike_exec::MountView {
        kind: vike_exec::MountRowKind::Mount,
        mount_id: format!("{venue}__{symbol}__{interval}"),
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
        ccy: Default::default(),
    }
}

/// ⚠ **THE LIVE CONFIGURATION, and the case nothing covered.** The daemon mounts bybit;
/// `vike_tradehub_core::assemble_runtime` makes its hardcoded binance paper engine the PRIMARY, so
/// `CoreSnapshot::venue` reads `"binance"` while the only series the core's bar cache holds is
/// the bybit feed's. The old projection compared the two and published `bars: []` — every
/// frame, forever, on a daemon that was otherwise healthy and publishing ~38 snapshots/second.
#[test]
fn a_bybit_mount_under_a_binance_primary_publishes_its_bars() {
    // `empty`'s arguments ARE the primary engine's (venue, symbol) — the untraded binance one.
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.mounts = vec![mount_row("bybit", "BTCUSDT", "1m")].into();
    snap.bars.insert(
        ("bybit".into(), "BTCUSDT".into(), "1m".into()),
        series(
            vec![flat_bar_unit_volume(1, 100.0), flat_bar_unit_volume(2, 101.0)],
            Some(flat_bar_unit_volume(3, 102.0)),
        ),
    );

    let wire = project(&snap, None, &[]);
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
    snap.mounts =
        vec![mount_row("bybit", "BTCUSDT", "1m"), mount_row("okx", "BTC-USDT", "5m")].into();
    snap.bars.insert(
        ("bybit".into(), "BTCUSDT".into(), "1m".into()),
        series(vec![flat_bar_unit_volume(1, 1.0)], None),
    );
    snap.bars.insert(
        ("okx".into(), "BTC-USDT".into(), "5m".into()),
        series(vec![flat_bar_unit_volume(1, 2.0)], None),
    );

    let wire = project(&snap, None, &[]);
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
    snap.bars.insert(
        ("bybit".into(), "BTCUSDT".into(), "1m".into()),
        series(vec![flat_bar_unit_volume(1, 1.0)], None),
    );
    assert!(snap.mounts.is_empty(), "no mount rows at all");
    assert_eq!(project(&snap, None, &[]).bars.len(), 1);
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
            series(vec![flat_bar_unit_volume(1, 1.0)], None),
        );
    }
    snap.mounts = vec![mount_row("bybit", "BTCUSDT", "1m")].into();
    snap.bars.insert(
        ("bybit".into(), "BTCUSDT".into(), "1m".into()),
        series(vec![flat_bar_unit_volume(1, 9.0)], None),
    );

    let wire = project(&snap, None, &[]);
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
    let bars: Vec<Bar> =
        (0..(CHART_BARS_CAP as i64 + 10)).map(|i| flat_bar_unit_volume(i, i as f64)).collect();
    snap.bars.insert(("bybit".into(), "BTCUSDT".into(), "1m".into()), series(bars, None));

    let wire = project(&snap, None, &[]);
    assert_eq!(wire.bars[0].closed.len(), CHART_BARS_CAP);
    assert_eq!(wire.bars[0].closed[0].ts, 10, "the TAIL survives, not the head");
}

/// **The published block NAMES its account** — §6.2's snapshot mirror, and the successor to a
/// fence that stood here.
///
/// ⚠ **What this replaced, and why it is a restatement rather than a deletion.** Stage 1 gave
/// `vike_exec::VenueBlock` its `account` and `route_key`
/// (`docs/superpowers/specs/2026-09-13-the-wire-names-an-account-from-the-misroute.md` §7.1)
/// and this projection forwarded NEITHER. A test called
/// `the_published_venue_block_carries_no_account_field_yet` pinned that absence, and its doc
/// said what would license the change: *"it fails the day somebody adds either field to
/// `WireVenueBlock` **without doing §6.2's consumer half**, which is exactly the shape of
/// change that would let a client believe it can address an account it cannot."*
///
/// §6.2's consumer half is done — `crates/vike-cli/src/cmd/mcp/venue_gate.rs`'s `mounted_accounts` reads
/// `venues[].route_key` and `venue_verdict` refuses an ambiguous venue before a
/// `preview_token` is minted — so the fence has been paid rather than climbed over, and the
/// assertion inverts. The fence FIRED on this change, which is the evidence it was worth
/// having: nothing else in the tree would have asked whether the consumer half existed.
#[test]
fn the_published_venue_block_names_its_account_and_route_key() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![vike_exec::VenueBlock {
        venue: "binance".into(),
        account: vike_model::accounts::account_keys::AccountLabel::parse("ALT").ok(),
        route_key: "binance#ALT".into(),
        ..Default::default()
    }];
    let json = serde_json::to_string(&project(&snap, None, &[])).expect("the wire serializes");
    assert!(json.contains("\"account\":\"ALT\""), "the LABEL reaches the wire: {json}");
    assert!(json.contains("\"route_key\":\"binance#ALT\""), "…and the ROUTE KEY: {json}");
    assert!(json.contains("\"binance\""), "…and the venue is still published: {json}");
}

/// **The engine's conversion-rate table reaches the wire** (decision 0183, phase 1): one row per quote
/// currency with its rate, age and stale flag, and the currency the rates are in; an account with no symbol
/// quoted in another currency publishes neither key.
#[test]
fn the_published_venue_block_carries_the_conversion_rates() {
    let row = |ccy: &str, rate: f64, as_of_ms: i64, stale: bool| vike_exec::QuoteRateView {
        ccy: vike_exec::Interned::from(ccy),
        rate,
        as_of_ms,
        stale,
        held: stale,
    };
    let mut snap = CoreSnapshot::empty("oanda", "EURUSD");
    snap.portfolio.venues = vec![vike_exec::VenueBlock {
        venue: "oanda".into(),
        route_key: "oanda".into(),
        quote_rates: vec![
            row("GBP", 1.35, 1_700_000_000_000, false),
            row("JPY", 1.0 / 150.0, 5, true),
        ],
        quote_rates_in: vike_exec::Interned::from("USD"),
        ..Default::default()
    }];
    let wire = project(&snap, None, &[]);
    let block = &wire.venues[0];
    assert_eq!(block.quote_rates_in, "USD");
    let got: Vec<(&str, f64, i64, bool)> =
        block.quote_rates.iter().map(|r| (r.ccy.as_str(), r.rate, r.as_of_ms, r.stale)).collect();
    assert_eq!(got, [("GBP", 1.35, 1_700_000_000_000, false), ("JPY", 1.0 / 150.0, 5, true)]);
    // a stale row that values an open position says so on the wire, a fresh one never does
    assert_eq!(block.quote_rates.iter().map(|r| r.held).collect::<Vec<_>>(), [false, true]);

    snap.portfolio.venues[0].quote_rates.clear();
    snap.portfolio.venues[0].quote_rates_in = vike_exec::Interned::default();
    let json = serde_json::to_string(&project(&snap, None, &[])).expect("the wire serializes");
    assert!(!json.contains("quote_rates"), "an empty table writes no key: {json}");
}

/// ⚠ **The DEFAULT account publishes no `account` key and a route key equal to its venue** —
/// which is every node in production today, and the property that makes the two new fields
/// invisible to every existing consumer. Without this, the test above is satisfied by a
/// projection that stamps a label on blocks that have none.
#[test]
fn a_default_account_publishes_no_label_and_a_bare_route_key() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![vike_exec::VenueBlock {
        venue: "binance".into(),
        account: None,
        route_key: "binance".into(),
        ..Default::default()
    }];
    let json = serde_json::to_string(&project(&snap, None, &[])).expect("the wire serializes");
    assert!(!json.contains("\"account\""), "no label key at all for the default: {json}");
    assert!(json.contains("\"route_key\":\"binance\""), "the key IS the venue: {json}");
}

/// ⚠ **THE MEASUREMENT the account gate is built on, pinned rather than argued.**
/// [`PublisherHandle::engine_venues`] answers `["binance", "binance"]` on a two-account node —
/// evidence that cannot tell two books apart, and correctly so for the question it answers —
/// while [`PublisherHandle::engine_route_keys`] answers `["binance", "binance#ALT"]` and can.
/// `crate::server::refusal::account_refusal` reads the second; `crate::server::refusal::venue_refusal` keeps
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
        vike_exec::VenueBlock {
            venue: "binance".into(),
            account: None,
            route_key: "binance".into(),
            ..Default::default()
        },
        vike_exec::VenueBlock {
            venue: "binance".into(),
            account: vike_model::accounts::account_keys::AccountLabel::parse("ALT").ok(),
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
    // The third projection, the bracket gate's: the same blocks, in the same order, so its engine
    // is the one the route-key roster names.
    assert_eq!(
        handle.engine_blocks().iter().map(|b| b.route_key.as_str()).collect::<Vec<_>>(),
        vec!["binance", "binance#ALT"],
        "the engine blocks are the cell's own, one per engine"
    );

    // The pre-publish shape: a cell holding no venue block answers EMPTY on ALL THREE, which the
    // two gates read as UNKNOWN and the bracket's reads as nothing to judge.
    let bare = Arc::new(ArcSwap::from_pointee(CoreSnapshot::empty("binance", "BTCUSDT")));
    let quiet = spawn(Arc::clone(&bare), None);
    assert!(quiet.engine_venues().is_empty(), "no venue roster before the first publish");
    assert!(quiet.engine_route_keys().is_empty(), "…and no route-key roster either");
    assert!(quiet.engine_blocks().is_empty(), "…and no engine block either");

    handle.shutdown();
    quiet.shutdown();
}

/// **The order a `Modify` names, and only that one.** `orders_named_by` answers the OPEN order
/// of the coid a `Modify` carries — the ceiling's only way to its instrument — and nothing for a
/// terminal order, an unknown coid, or any verb that names no order to resolve (a `Cancel` names a
/// coid too, but the ceiling never sizes it, so this reads it as nothing). It reads the same cell
/// the rosters do.
///
/// ⚠ KILL PROOF: drop the `is_terminal` test in `named_orders`' `Modify` arm and the `FILLED` row
/// answers one order; drop the `Modify` match and the `Cancel` row answers one.
#[test]
fn orders_named_by_answers_the_open_order_a_modify_names_and_nothing_else() {
    use vike_model::OrderStatus;
    use vike_tradehub_client::wire::WireCommand;
    let order = |coid: &str, status: OrderStatus| vike_exec::OrderView {
        data_delay: None,
        client_order_id: coid.into(),
        venue: "binance".into(),
        account: None,
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(1.0),
        trigger_price: None,
        status,
        venue_order_id: None,
        filled_qty: 0.0,
        avg_fill_px: 0.0,
        reason: None,
        pending_price: None,
    };
    let modify = |coid: &str| WireCommand::Modify {
        client_order_id: coid.into(),
        new_qty: Some(1.0),
        new_price: Some(1.0),
    };
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.orders = vec![
        order("open", OrderStatus::Accepted),
        order("part", OrderStatus::PartiallyFilled),
        order("done", OrderStatus::Filled),
    ];
    let handle = spawn(Arc::new(ArcSwap::from_pointee(snap)), None);

    let named = |cmd: &WireCommand| -> Vec<String> {
        handle.orders_named_by(cmd).into_iter().map(|o| o.client_order_id).collect()
    };
    assert_eq!(named(&modify("open")), vec!["open".to_string()]);
    assert_eq!(named(&modify("part")), vec!["part".to_string()], "partially filled is still open");
    assert!(named(&modify("done")).is_empty(), "a FILLED order is terminal: nothing to resolve");
    assert!(named(&modify("never-published")).is_empty(), "an unknown coid resolves to nothing");
    assert!(
        named(&WireCommand::Cancel("open".into())).is_empty(),
        "only a Modify and a Submit ask for an order to be resolved"
    );

    // A `Submit` is the other verb that carries an id of its own, and it is asked in ANY status:
    // a terminal order is exactly the case `coid_reuse_refusal` exists for.
    let submit = |coid: &str| {
        WireCommand::Submit(vike_tradehub_client::wire::WireOrderRequest {
            client_order_id: coid.into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            order_type: "limit".into(),
            price: Some(1.0),
            trigger_price: None,
            reduce_only: false,
            account: None,
            time_in_force: None,
            gtd_expiry: None,
        })
    };
    assert_eq!(named(&submit("open")), vec!["open".to_string()]);
    assert_eq!(
        named(&submit("done")),
        vec!["done".to_string()],
        "a FILLED order still owns its id"
    );
    assert!(named(&submit("never-published")).is_empty(), "a free id names nothing");

    handle.shutdown();
}

/// The projection publishes the engine's symbols (primary first) and its tier, as its word.
#[test]
fn the_published_venue_block_names_its_symbols_and_tier() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![vike_exec::VenueBlock {
        venue: "binance".into(),
        route_key: "binance".into(),
        symbol: "BTCUSDT".into(),
        extra_symbols: vec!["ETHUSDT".to_string()].into(),
        tier: Some(vike_model::AccountTier::Demo),
        ..Default::default()
    }];
    let wire = project(&snap, None, &[]);
    assert_eq!(wire.venues[0].symbols, vec!["BTCUSDT".to_string(), "ETHUSDT".to_string()]);
    assert_eq!(wire.venues[0].tier.as_deref(), Some("demo"));
}

/// ⚠ **Each of the three tiers reaches the wire as ITSELF**, in its one spelling
/// (`AccountTier::as_str`). The test above names one, so a swapped pair (a LIVE engine published as
/// demo) would pass it. A GUI states this word to a person who is about to send an order.
#[test]
fn every_engine_tier_is_published_as_itself() {
    for tier in vike_model::AccountTier::ALL {
        let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
        snap.portfolio.venues = vec![vike_exec::VenueBlock {
            venue: "binance".into(),
            route_key: "binance".into(),
            symbol: "BTCUSDT".into(),
            tier: Some(tier),
            ..Default::default()
        }];
        let wire = project(&snap, None, &[]);
        assert_eq!(wire.venues[0].tier.as_deref(), Some(tier.as_str()), "{tier:?}");
    }
}

/// Decision 0156: the accounts the node does NOT run ride every frame, each with its route key,
/// its row's tier WORD and the reason word, so a GUI says why one is not running instead of
/// drawing it as anything. The list is the publisher's (`vike_tradehub_core::AssembledRuntime::not_running`), not the
/// core's: the core publishes none. An empty list writes no key, so a node with every account
/// running publishes the bytes it always did.
#[test]
fn the_accounts_the_node_does_not_run_ride_the_frame_with_their_reason() {
    let snap = CoreSnapshot::empty("binance", "BTCUSDT");
    let json = serde_json::to_string(&project(&snap, None, &[])).expect("serializes");
    assert!(!json.contains("\"not_running\""), "CONTROL: none, no key: {json}");
    let sub = vike_model::accounts::account_keys::AccountLabel::parse("SUB").expect("a label");
    let not_running = [
        vike_exec::NotRunningAccount {
            venue: "okx".into(),
            account: None,
            route_key: "okx".into(),
            tier: Some(vike_model::AccountTier::Live),
            reason: "no credentials".into(),
        },
        vike_exec::NotRunningAccount {
            venue: "binance".into(),
            route_key: vike_model::accounts::account_keys::route_key_of("binance", &sub),
            account: Some(sub),
            tier: None,
            reason: "no account row".into(),
        },
    ];
    let wire = project(&snap, None, &not_running);
    let seen: Vec<_> = wire
        .not_running
        .iter()
        .map(|n| {
            (
                n.venue.as_str(),
                n.account.as_deref(),
                n.route_key.as_str(),
                n.tier.as_deref(),
                n.reason.as_str(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            ("okx", None, "okx", Some("live"), "no credentials"),
            ("binance", Some("SUB"), "binance#SUB", None, "no account row"),
        ]
    );
}

/// ⚠ **"Not said" stays "not said" on the wire.** A block that names no symbol and no tier (an
/// engine the snapshot could not describe) publishes an EMPTY list and no tier: never "trades
/// nothing", never a default of `Live`. Neither key reaches the frame either, so an older client
/// reads exactly the bytes it always read. `VenueBlock::trades` answers `false` for such a block,
/// which is the reader's trap: it has to test for the empty list first.
///
/// The second block pins the one case the wire cannot express: extras with NO primary. They go
/// with the missing primary, because a list whose first entry is not the primary would be read as
/// one.
#[test]
fn a_block_that_names_no_symbol_or_tier_publishes_neither() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![
        vike_exec::VenueBlock {
            venue: "binance".into(),
            route_key: "binance".into(),
            ..Default::default()
        },
        vike_exec::VenueBlock {
            venue: "okx".into(),
            route_key: "okx".into(),
            extra_symbols: vec!["ETH-USDT".to_string()].into(),
            ..Default::default()
        },
    ];
    let wire = project(&snap, None, &[]);
    // Both blocks are published, or the loop below would pass over nothing.
    assert_eq!(wire.venues.len(), 2);
    for block in &wire.venues {
        assert!(block.symbols.is_empty(), "{}: {:?}", block.venue, block.symbols);
        assert_eq!(block.tier, None, "{}", block.venue);
    }
    let json = serde_json::to_string(&wire).expect("serializes");
    assert!(!json.contains("\"symbols\""), "{json}");
    assert!(!json.contains("\"tier\""), "{json}");
}

/// An order on a labelled account publishes its label, and a default-account order publishes no
/// key at all (its bytes are what they always were).
#[test]
fn a_published_order_names_its_account_only_when_labelled() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    let order = |account| vike_exec::OrderView {
        data_delay: None,
        client_order_id: "c-1".into(),
        venue: "binance".into(),
        account,
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(99.0),
        trigger_price: None,
        status: vike_model::OrderStatus::Accepted,
        venue_order_id: None,
        filled_qty: 0.0,
        avg_fill_px: 0.0,
        reason: None,
        pending_price: None,
    };
    snap.orders = vec![order(vike_model::accounts::account_keys::AccountLabel::parse("SUB").ok())];
    let json = serde_json::to_string(&project(&snap, None, &[])).expect("serializes");
    assert!(json.contains("\"account\":\"SUB\""), "{json}");
    snap.orders = vec![order(None)];
    let json = serde_json::to_string(&project(&snap, None, &[])).expect("serializes");
    assert!(!json.contains("\"account\""), "{json}");
}

/// Decision 0140: an order's recorded delay label rides the wire as its WORD, and an unlabelled
/// order publishes no key at all (an old reader's bytes, and the golden below, are unchanged).
#[test]
fn a_published_order_carries_its_delay_label_as_a_word_and_an_unlabelled_one_no_key() {
    let mut snap = CoreSnapshot::empty("ibkr", "AAPL.SMART.USD");
    let order = |data_delay| vike_exec::OrderView {
        data_delay,
        client_order_id: "c-1".into(),
        venue: "ibkr".into(),
        account: None,
        symbol: "AAPL.SMART.USD".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(190.0),
        trigger_price: None,
        status: vike_model::OrderStatus::Accepted,
        venue_order_id: None,
        filled_qty: 0.0,
        avg_fill_px: 0.0,
        reason: None,
        pending_price: None,
    };
    for (label, word) in [
        (vike_model::DataDelay::DELAYED, "delayed"),
        (vike_model::DataDelay::Frozen, "frozen"),
        (vike_model::DataDelay::RealTime, "real-time"),
    ] {
        snap.orders = vec![order(Some(label))];
        let wire = project(&snap, None, &[]);
        assert_eq!(wire.orders[0].data_delay.as_deref(), Some(word));
        let json = serde_json::to_string(&wire).expect("serializes");
        assert!(json.contains(&format!("\"data_delay\":\"{word}\"")), "{json}");
    }
    snap.orders = vec![order(None)];
    let json = serde_json::to_string(&project(&snap, None, &[])).expect("serializes");
    assert!(!json.contains("data_delay"), "{json}");
}

// -----------------------------------------------------------------------------------------
// The wire golden. ⚠ `wire_tests.rs` (vike-tradehub-client) only ROUND-TRIPS a hand-built
// `WireSnapshot`, and the tests above assert one field at a time, so until this one NOTHING pinned
// the BYTES the projection emits for a fully populated `CoreSnapshot`. It stands in front of the
// read-model move (`CoreSnapshot` and its view types leaving vike-core): that move must not change
// one byte of what a node publishes, and a re-spelled field, a reordered struct or a leaked
// core-only field all show here as a diff of this literal.
// -----------------------------------------------------------------------------------------

/// A bar whose five numbers differ, so a swapped `o`/`h`/`l`/`c`/`v` mapping cannot hide.
fn ohlcv(ts: i64, o: f64, h: f64, l: f64, c: f64, v: f64) -> Bar {
    Bar { open: o, high: h, low: l, close: c, volume: v, ..flat_bar_unit_volume(ts, 0.0) }
}

/// A snapshot with EVERY projected field populated, each `Option` set where the wire carries it,
/// and the core-only fields (`marks`, `recon`, margin mode, mark source, ...) set to non-default
/// values so a projection that started leaking one would change the bytes.
fn fully_populated_snapshot() -> CoreSnapshot {
    use vike_exec::{MountRowKind, MountView};
    use vike_model::accounts::account_keys::AccountLabel;
    use vike_model::events::PositionSide;

    let position =
        |symbol: &str, side: PositionSide, size: f64, avg_px: f64| vike_exec::PositionView {
            venue: "binance".into(),
            symbol: symbol.into(),
            position_side: side,
            size,
            avg_px,
            unrealized: size * 1.5,
            mark_source: Some(vike_exec::PriceSource::Mark),
            leverage: 5.0,
            liq_price: avg_px - 20.0,
            margin_mode: vike_model::MarginMode::Isolated,
            isolated_margin: Some(55.5),
        };
    let block = |account: Option<AccountLabel>, route_key: &str, balance: f64, positions| {
        vike_exec::VenueBlock {
            venue: "binance".into(),
            account,
            route_key: route_key.into(),
            symbol: "BTCUSDT".into(),
            extra_symbols: vec!["ETHUSDT".to_string()].into(),
            tier: Some(vike_model::AccountTier::Paper),
            balance,
            realized_pnl: 12.25,
            fees_paid: 1.5,
            funding_paid: -0.75,
            equity: balance + 37.5,
            unrealized: 37.5,
            missing_prices: 2,
            margin_used: 300.0,
            free_bp: balance - 300.0,
            margin_ratio: 0.03,
            trading_state: TradingState::Active,
            contract_value_default: vike_model::ContractValue::linear(2.0),
            positions,
            ..Default::default()
        }
    };
    let order =
        |coid: &str, account, status, price, trigger, venue_order_id| vike_exec::OrderView {
            data_delay: None,
            client_order_id: coid.into(),
            venue: "binance".into(),
            account,
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 2.0,
            order_type: vike_model::OrderType::StopLimit,
            price,
            trigger_price: trigger,
            status,
            venue_order_id,
            filled_qty: 0.5,
            avg_fill_px: 101.25,
            reason: None,
            pending_price: None,
        };
    let held = |coid: &str, price, trigger, parent: Option<&str>| HeldOrderView {
        client_order_id: coid.into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: -1,
        qty: 2.0,
        order_type: vike_model::OrderType::StopLimit,
        price,
        trigger_price: trigger,
        parent_order_id: parent.map(Into::into),
    };

    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.seq = 4242;
    snap.trading_state = TradingState::Reducing;
    snap.balance = 9876.5;
    snap.portfolio.equity_total = 20123.25;
    let mut alt = block(
        AccountLabel::parse("ALT").ok(),
        "binance#ALT",
        5000.25,
        vec![position("BTCUSDT", PositionSide::Short, -1.25, 101.0)],
    );
    alt.tier = Some(vike_model::AccountTier::Live);
    alt.trading_state = TradingState::Halted;
    snap.portfolio.venues = vec![
        block(
            None,
            "binance",
            10000.5,
            vec![
                position("BTCUSDT", PositionSide::Both, 0.75, 100.5),
                position("ETHUSDT", PositionSide::Long, 3.0, 2500.0),
            ],
        ),
        alt,
    ];
    snap.positions = vec![
        position("BTCUSDT", PositionSide::Both, 0.75, 100.5),
        position("ETHUSDT", PositionSide::Long, 3.0, 2500.0),
    ];
    snap.marks = vec![("binance".into(), "BTCUSDT".into(), 101.5)];
    snap.orders = vec![
        order(
            "c-1",
            AccountLabel::parse("SUB").ok(),
            vike_model::OrderStatus::PartiallyFilled,
            Some(99.5),
            Some(100.0),
            Some("v-77".into()),
        ),
        order("c-2", None, vike_model::OrderStatus::Canceled, None, None, None),
    ];
    snap.held_exits = vec![
        held("c-1-sl", Some(98.5), Some(99.0), Some("c-1")),
        held("c-1-tp", Some(110.0), None, None),
    ];
    // Two mounted series and one unmounted one, the unmounted one FIRST in the cache: the frame
    // must lead with the mounted pair, and the residual row (no venue, no symbol) mounts nothing.
    snap.bars.insert(
        ("okx".into(), "BTC-USDT".into(), "5m".into()),
        series(vec![ohlcv(60, 1.0, 2.0, 0.5, 1.5, 7.0)], None),
    );
    snap.bars.insert(
        ("bybit".into(), "BTCUSDT".into(), "1m".into()),
        series(
            vec![
                ohlcv(1, 100.0, 103.5, 99.25, 102.0, 12.5),
                ohlcv(2, 102.0, 104.0, 101.5, 103.25, 8.75),
            ],
            Some(ohlcv(3, 103.25, 105.0, 103.0, 104.5, 3.5)),
        ),
    );
    snap.bars.insert(
        ("binance".into(), "ETHUSDT".into(), "1h".into()),
        series(vec![ohlcv(3600, 2500.0, 2510.0, 2490.0, 2505.0, 410.0)], None),
    );
    let mount = |venue: &str, symbol: &str, interval: &str| MountView {
        budget: Some(vike_exec::MountBudget {
            max_loss: Some(50.0),
            max_notional: Some(1000.0),
            flatten_on_breach: true,
        }),
        position: 0.75,
        realized_pnl: 4.5,
        unrealized_pnl: 1.25,
        notional: 75.0,
        latched: true,
        ..mount_row(venue, symbol, interval)
    };
    snap.mounts = vec![
        mount("bybit", "BTCUSDT", "1m"),
        mount("binance", "ETHUSDT", "1h"),
        MountView {
            kind: MountRowKind::Residual,
            mount_id: String::new(),
            ..mount_row("", "", "")
        },
    ]
    .into();
    snap.recent_events = vec!["fill c-1 0.5@101.25".into(), "cancel c-2".into()];
    snap.fault = Some("handler panicked: boom".into());
    snap.conflated_market_drops = 9;
    snap.rejected_commands = 3;
    snap.recon.last_pass_ts = 1_700_000_000_000;
    snap.accounts_epoch = 0x0123_4567_89AB_CDEF;
    snap
}

/// **The JSON a node publishes for a fully populated snapshot, byte for byte.** Two venue blocks
/// (one labelled `ALT`), positions, two orders (one with every `Option` set, one with none), two
/// held exits, a closed-plus-forming bar series with mounted-first series order, a fault, recent
/// events, an accounts epoch and an identity. A change to the wire schema, to a field's spelling or
/// order, or to what the projection copies changes this literal and must be reviewed as a
/// wire-contract change; a refactor that does not mean to change the wire leaves it untouched.
#[test]
fn a_fully_populated_snapshot_projects_to_this_exact_json() {
    let id = WireHubIdentity {
        name: "the build runner".into(),
        strategy: "spread_maker".into(),
        params: "qty=0.5".into(),
        live: true,
        build: "v0.1.51 abc1234".into(),
        advertise_addr: "<host>:7879".into(),
    };
    let json = serde_json::to_string(&project(&fully_populated_snapshot(), Some(&id), &[]))
        .expect("the wire serializes");
    assert_eq!(json, WIRE_SNAPSHOT_GOLDEN);
}

/// The literal `a_fully_populated_snapshot_projects_to_this_exact_json` compares against: what
/// `serde_json::to_string` printed for `fully_populated_snapshot` on the first run, read by eye
/// afterwards. The mount rows do not appear in it by design (`project` reads them only to put the
/// MOUNTED bar series first, which is why `bybit` and `binance` lead and `okx` trails), nor do
/// `marks`, `recon`, `margin_ratio`, `mark_source` or `margin_mode`, which stay core-side.
const WIRE_SNAPSHOT_GOLDEN: &str = r##"{"seq":4242,"venue":"binance","symbol":"BTCUSDT","trading_state":"Reducing","balance":9876.5,"equity_total":20123.25,"venues":[{"venue":"binance","balance":10000.5,"realized_pnl":12.25,"fees_paid":1.5,"funding_paid":-0.75,"equity":10038.0,"unrealized":37.5,"missing_prices":2,"margin_used":300.0,"free_bp":9700.5,"trading_state":"Active","positions":[{"venue":"binance","symbol":"BTCUSDT","position_side":"BOTH","size":0.75,"avg_px":100.5,"unrealized":1.125,"leverage":5.0,"liq_price":80.5},{"venue":"binance","symbol":"ETHUSDT","position_side":"LONG","size":3.0,"avg_px":2500.0,"unrealized":4.5,"leverage":5.0,"liq_price":2480.0}],"route_key":"binance","symbols":["BTCUSDT","ETHUSDT"],"tier":"paper"},{"venue":"binance","balance":5000.25,"realized_pnl":12.25,"fees_paid":1.5,"funding_paid":-0.75,"equity":5037.75,"unrealized":37.5,"missing_prices":2,"margin_used":300.0,"free_bp":4700.25,"trading_state":"Halted","positions":[{"venue":"binance","symbol":"BTCUSDT","position_side":"SHORT","size":-1.25,"avg_px":101.0,"unrealized":-1.875,"leverage":5.0,"liq_price":81.0}],"account":"ALT","route_key":"binance#ALT","symbols":["BTCUSDT","ETHUSDT"],"tier":"live"}],"orders":[{"client_order_id":"c-1","venue":"binance","account":"SUB","symbol":"BTCUSDT","side":-1,"qty":2.0,"order_type":"stop_limit","price":99.5,"trigger_price":100.0,"status":"PARTIALLY_FILLED","venue_order_id":"v-77","filled_qty":0.5,"avg_fill_px":101.25},{"client_order_id":"c-2","venue":"binance","symbol":"BTCUSDT","side":-1,"qty":2.0,"order_type":"stop_limit","price":null,"trigger_price":null,"status":"CANCELED","venue_order_id":null,"filled_qty":0.5,"avg_fill_px":101.25}],"positions":[{"venue":"binance","symbol":"BTCUSDT","position_side":"BOTH","size":0.75,"avg_px":100.5,"unrealized":1.125,"leverage":5.0,"liq_price":80.5},{"venue":"binance","symbol":"ETHUSDT","position_side":"LONG","size":3.0,"avg_px":2500.0,"unrealized":4.5,"leverage":5.0,"liq_price":2480.0}],"held_exits":[{"client_order_id":"c-1-sl","venue":"binance","symbol":"BTCUSDT","side":-1,"qty":2.0,"order_type":"stop_limit","price":98.5,"trigger_price":99.0,"parent_order_id":"c-1"},{"client_order_id":"c-1-tp","venue":"binance","symbol":"BTCUSDT","side":-1,"qty":2.0,"order_type":"stop_limit","price":110.0,"trigger_price":null,"parent_order_id":null}],"recent_events":["fill c-1 0.5@101.25","cancel c-2"],"fault":"handler panicked: boom","bars":[{"venue":"bybit","symbol":"BTCUSDT","interval":"1m","closed":[{"ts":1,"o":100.0,"h":103.5,"l":99.25,"c":102.0,"v":12.5},{"ts":2,"o":102.0,"h":104.0,"l":101.5,"c":103.25,"v":8.75}],"forming":{"ts":3,"o":103.25,"h":105.0,"l":103.0,"c":104.5,"v":3.5}},{"venue":"binance","symbol":"ETHUSDT","interval":"1h","closed":[{"ts":3600,"o":2500.0,"h":2510.0,"l":2490.0,"c":2505.0,"v":410.0}],"forming":null},{"venue":"okx","symbol":"BTC-USDT","interval":"5m","closed":[{"ts":60,"o":1.0,"h":2.0,"l":0.5,"c":1.5,"v":7.0}],"forming":null}],"identity":{"name":"the build runner","strategy":"spread_maker","params":"qty=0.5","live":true,"build":"v0.1.51 abc1234","advertise_addr":"<host>:7879"},"accounts_epoch":81985529216486895}"##;

/// **The mount rows the `StrategyStatus` overlay reads carry the budget latch** (decision 0239): each REAL mount row
/// of the published cell hands out its `latched` flag, the residual row is left out, and the rows an un-latch is
/// judged against are the same real ones, for that verb alone.
#[test]
fn the_live_mount_rows_carry_the_budget_latch() {
    use vike_tradehub_client::wire::WireCommand;
    let snap = fully_populated_snapshot();
    let handle = spawn(Arc::new(ArcSwap::from_pointee(snap)), None);
    let live = handle.live_mount_params();
    assert_eq!(live.len(), 2, "the residual row is no mount: {live:?}");
    assert!(live.iter().all(|m| m.latched), "both fixture mounts are latched: {live:?}");
    assert_eq!(live[0].mount_id, "bybit__BTCUSDT__1m");

    let rows = handle
        .mounts_named_by(&WireCommand::UnlatchMount { mount_id: "bybit__BTCUSDT__1m".into() });
    assert_eq!(rows.len(), 2, "the real mount rows, for that verb");
    assert!(rows.iter().all(|m| m.latched && m.kind == vike_exec::MountRowKind::Mount));
    assert!(
        handle.mounts_named_by(&WireCommand::Cancel("c".into())).is_empty(),
        "any other verb clones nothing"
    );
    handle.shutdown();
}

/// A refused order's reason crosses the wire (`WireOrderView::reason`), and an order without one
/// puts nothing there.
#[test]
fn project_carries_a_refused_orders_reason() {
    let mut snap = CoreSnapshot::empty("ctrader", "EURUSD");
    let order = |coid: &str, reason: Option<&str>| OrderView {
        data_delay: None,
        client_order_id: coid.into(),
        venue: "ctrader".into(),
        account: None,
        symbol: "EURUSD".into(),
        side: 1,
        qty: 1000.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(1.1),
        trigger_price: None,
        status: vike_model::OrderStatus::Rejected,
        venue_order_id: None,
        filled_qty: 0.0,
        avg_fill_px: 0.0,
        reason: reason.map(str::to_owned),
        pending_price: None,
    };
    snap.orders.push(order("c-1", Some("INVALID_REQUEST: more digits than symbol allows")));
    snap.orders.push(order("c-2", None));

    let wire = project(&snap, None, &[]);
    assert_eq!(
        wire.orders[0].reason.as_deref(),
        Some("INVALID_REQUEST: more digits than symbol allows")
    );
    assert_eq!(wire.orders[1].reason, None);
}

/// An account that stopped and started again has two blocks under one route key in the core's list (the list only
/// grows): the frame carries the newest, in its place, so a consumer that takes the first block of a key reads the
/// running engine and not the stopped one. A stopped account that did not start again keeps its block, and blocks
/// that name no route key are never merged.
#[test]
fn a_restarted_accounts_stale_block_is_not_published_beside_its_new_one() {
    let block = |venue: &str, key: &str, equity: f64| vike_exec::VenueBlock {
        venue: venue.into(),
        route_key: key.into(),
        equity,
        ..Default::default()
    };
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![
        block("binance", "binance", 1.0),
        block("binance", "binance#ALT", 2.0), // stopped, then started again below
        block("okx", "okx", 3.0),             // stopped for good: still shown
        block("binance", "binance#ALT", 4.0),
        block("bybit", "", 5.0),
        block("bybit", "", 6.0),
    ];
    let wire = project(&snap, None, &[]);
    let seen: Vec<(String, f64)> =
        wire.venues.iter().map(|v| (v.route_key.clone(), v.equity)).collect();
    assert_eq!(
        seen,
        vec![
            ("binance".to_string(), 1.0),
            ("okx".to_string(), 3.0),
            ("binance#ALT".to_string(), 4.0),
            (String::new(), 5.0),
            (String::new(), 6.0),
        ]
    );
}

/// **The published `equity_total` is the sum of the rows beside it** (drill `n078-B1`, the CI box, 2026-10-19): the
/// core sums every engine it ever held, so one deactivate and activate of binance's demo account added the
/// stopped spot engine's wallet (4984.80) to the new engine's, and the REPL's total no longer matched its rows.
/// The frame sums the newest block of each route key, as its rows are; a stopped account that did not start
/// again keeps its block, and a node with nothing superseded publishes the core's own figure bit for bit.
#[test]
fn a_restarted_accounts_stopped_engine_is_not_added_to_the_published_total() {
    let block = |venue: &str, key: &str, equity: f64| vike_exec::VenueBlock {
        venue: venue.into(),
        route_key: key.into(),
        equity,
        ..Default::default()
    };
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![
        block("binance", "binance", 4984.80324635), // stopped by the deactivate, then started again below
        block("okx", "okx", 4999.625219769523),
        block("binance", "binance", 4984.80324635),
        block("binance", "binance#PERP", 4996.00829587), // the lane twin, then stopped for good: still shown
    ];
    snap.portfolio.equity_total =
        vike_model::py_sum(snap.portfolio.venues.iter().map(|v| v.equity));
    let wire = project(&snap, None, &[]);
    assert_eq!(wire.venues.len(), 3, "the stale block is not a row");
    let rows = vike_model::py_sum(wire.venues.iter().map(|v| v.equity));
    assert_eq!(wire.equity_total.to_bits(), rows.to_bits(), "the total is the rows' sum");
    assert!(
        wire.equity_total < snap.portfolio.equity_total - 4984.0,
        "the stopped spot engine's wallet is not counted again"
    );

    // CONTROL: nothing superseded, so the core's figure goes out untouched (here deliberately not the rows' sum).
    snap.portfolio.venues.remove(0);
    snap.portfolio.equity_total = 1234.5;
    assert_eq!(project(&snap, None, &[]).equity_total.to_bits(), 1234.5_f64.to_bits());
}

/// ⚠ **The published `equity_total` is one currency, the primary's, also when a block was superseded**
/// (decision 0183, item 13): the frame's total re-sums the newest block of each key, and a block in another
/// currency is not in it. The block's group rides the wire as `ledger_ccy`, EMPTY (and so absent from the
/// frame) for the dollar group.
#[test]
fn the_published_total_is_the_primary_currency_alone_and_each_block_names_its_group() {
    let block = |venue: &str, key: &str, ccy: &str, equity: f64| vike_exec::VenueBlock {
        venue: venue.into(),
        route_key: key.into(),
        ledger_ccy: ccy.into(),
        equity,
        ..Default::default()
    };
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![
        block("binance", "binance", "", 100.0), // stopped, started again below: a superseded block
        block("oanda", "oanda", "EUR", 7_000.0),
        block("binance", "binance", "", 110.0),
    ];
    snap.portfolio.equity_total = 100.0 + 110.0; // as `build` folds it: the primary group, every engine
    let wire = project(&snap, None, &[]);
    assert_eq!(wire.venues.len(), 2);
    assert_eq!(
        wire.equity_total.to_bits(),
        110.0_f64.to_bits(),
        "the dollar block alone, not +7000"
    );
    assert_eq!(wire.venues[0].ledger_ccy, "EUR");
    assert_eq!(wire.venues[1].ledger_ccy, "", "the dollar group is the absent key");
    let json = serde_json::to_string(&wire.venues[1]).expect("serializes");
    assert!(!json.contains("ledger_ccy"), "{json}");

    // CONTROL: nothing superseded, so the core's own figure goes out untouched.
    snap.portfolio.venues.remove(0);
    snap.portfolio.equity_total = 110.0;
    assert_eq!(project(&snap, None, &[]).equity_total.to_bits(), 110.0_f64.to_bits());
}

/// **The block says when its ledger currency is only an assumed USD** (decision 0189, A52): the engine's
/// `VenueBlock::ccy_assumed` rides the frame as `ccy_assumed`, absent while false, per block (a stated account
/// beside an assumed one stays stated).
#[test]
fn a_block_whose_currency_is_only_assumed_says_so_on_the_wire_and_others_write_no_key() {
    let block = |venue: &str, assumed: bool| vike_exec::VenueBlock {
        venue: venue.into(),
        route_key: venue.into(),
        ccy_assumed: assumed,
        ..Default::default()
    };
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![block("oanda", true), block("binance", false)];
    let wire = project(&snap, None, &[]);
    assert!(wire.venues[0].ccy_assumed && !wire.venues[1].ccy_assumed);
    let on = serde_json::to_string(&wire.venues[0]).expect("serializes");
    let off = serde_json::to_string(&wire.venues[1]).expect("serializes");
    assert!(on.contains(r#""ccy_assumed":true"#), "{on}");
    assert!(!off.contains("ccy_assumed"), "{off}");
}

/// ⚠ **The frame STATES the primary group; a reader never derives it from the first block.** The node's
/// primary is the core's first engine, the stopped primary's tombstone included, but the frame carries only
/// the newest block of each account: `[binance USDT (primary), oanda EUR, bybit USDT]` after binance stopped
/// and started again publishes `[oanda, bybit, binance']`, whose first block is in EUR while `equity_total`
/// is the dollar group's. The dollar group is the absent key; a EUR primary is stated.
#[test]
fn the_frame_states_the_primary_group_after_a_primary_restart() {
    let block = |venue: &str, key: &str, ccy: &str, equity: f64| vike_exec::VenueBlock {
        venue: venue.into(),
        route_key: key.into(),
        ledger_ccy: ccy.into(),
        equity,
        ..Default::default()
    };
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![
        block("binance", "binance", "", 100.0), // the PRIMARY, stopped; its successor is last
        block("oanda", "oanda", "EUR", 7_000.0),
        block("bybit", "bybit", "", 50.0),
        block("binance", "binance", "", 110.0),
    ];
    snap.portfolio.equity_total = 100.0 + 50.0 + 110.0;
    let wire = project(&snap, None, &[]);
    assert_eq!(
        wire.venues.iter().map(|v| v.route_key.as_str()).collect::<Vec<_>>(),
        ["oanda", "bybit", "binance"],
        "PREMISE: the first block of the frame is the EUR one"
    );
    assert_eq!(wire.venues[0].ledger_ccy, "EUR");
    assert_eq!(wire.primary_ccy, "", "the node's primary is the dollar group");
    assert!(!serde_json::to_string(&wire).expect("serializes").contains("primary_ccy"));
    assert_eq!(wire.equity_total.to_bits(), (50.0_f64 + 110.0).to_bits(), "the dollar blocks");

    // A EUR primary is stated, whichever block leads the frame.
    snap.portfolio.venues = vec![
        block("oanda", "oanda", "EUR", 7_000.0), // the PRIMARY, stopped
        block("binance", "binance", "", 100.0),
        block("oanda", "oanda", "EUR", 7_100.0),
    ];
    snap.portfolio.equity_total = 7_000.0 + 7_100.0;
    let wire = project(&snap, None, &[]);
    assert_eq!(
        wire.venues[0].ledger_ccy, "",
        "PREMISE: the first block of the frame is the dollar one"
    );
    assert_eq!(wire.primary_ccy, "EUR");
    assert!(serde_json::to_string(&wire).expect("serializes").contains(r#""primary_ccy":"EUR""#));
    assert_eq!(wire.equity_total.to_bits(), 7_100.0_f64.to_bits(), "the EUR block alone");
}

/// The door reads [`PublisherHandle::engine_blocks`], not the frame, and takes the first block of a route key: after
/// a restart that block must be the NEW engine's, with its own `extra_symbols`, not the stopped one's.
#[test]
fn engine_blocks_carry_the_newest_block_of_a_restarted_account() {
    let block = |key: &str, equity: f64, extra: &[&str]| vike_exec::VenueBlock {
        venue: "hyperliquid".into(),
        route_key: key.into(),
        equity,
        extra_symbols: extra.iter().map(|s| s.to_string()).collect::<Vec<_>>().into(),
        ..Default::default()
    };
    let mut snap = CoreSnapshot::empty("hyperliquid", "BTC");
    snap.portfolio.venues = vec![
        block("hyperliquid#LIVE", 1.0, &["SOL"]), // stopped, then started again below
        block("okx", 2.0, &[]),                   // stopped for good: still there
        block("hyperliquid#LIVE", 3.0, &[]),
    ];
    let handle = spawn(Arc::new(ArcSwap::from_pointee(snap)), None);
    let seen: Vec<_> = handle
        .engine_blocks()
        .into_iter()
        .map(|b| (b.route_key, b.equity, b.extra_symbols.len()))
        .collect();
    assert_eq!(seen, vec![("okx".to_string(), 2.0, 0), ("hyperliquid#LIVE".to_string(), 3.0, 0)]);
    handle.shutdown();
}

// -----------------------------------------------------------------------------------------
// What the publisher stamps onto a frame (`NodeFacts`): uptime, the HALT sentinel, order ages.
// -----------------------------------------------------------------------------------------

/// The uptime is counted from the PROCESS's start (`process_started`), as of the instant the frame
/// is built: 90 s after it reads 90 000 ms, not a figure measured from the publisher's spawn.
#[test]
fn a_frame_carries_the_uptime_counted_from_the_process_start() {
    let snap = CoreSnapshot::empty("sim", "BTCUSDT");
    let mut wire = project(&snap, None, &[]);
    assert_eq!(wire.uptime_ms, None, "`project` is pure: the stamp is the publisher's");
    NodeFacts::default().stamp(&mut wire, &snap, process_started() + Duration::from_secs(90));
    assert_eq!(wire.uptime_ms, Some(90_000));
}

/// The sentinel is `None` ("the node did not say") until a path is handed over, then follows the
/// file in both directions: `touch` shows on the next frame and `rm` clears it, nothing latches.
#[test]
fn the_halt_sentinel_is_unsaid_without_a_path_and_follows_the_file_with_one() {
    let snap = CoreSnapshot::empty("sim", "BTCUSDT");
    let stamped = |facts: &NodeFacts| {
        let mut wire = project(&snap, None, &[]);
        facts.stamp(&mut wire, &snap, Instant::now());
        wire.halt_sentinel
    };
    assert_eq!(stamped(&NodeFacts::default()), None, "no path handed: silence, never `false`");

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("HALT");
    let facts = NodeFacts::default();
    facts.halt_path.set(path.clone()).expect("the first path is taken");
    assert_eq!(stamped(&facts), Some(false));
    std::fs::write(&path, b"").expect("touch the sentinel");
    assert_eq!(stamped(&facts), Some(true));
    std::fs::remove_file(&path).expect("rm the sentinel");
    assert_eq!(stamped(&facts), Some(false));
}

/// An order's age counts from the instant the node FIRST saw it, grows with later frames, belongs
/// only to an order that is not terminal, and its bookkeeping goes when the order does.
#[test]
fn an_orders_age_counts_from_the_first_sight_and_ends_with_the_order() {
    use vike_model::OrderStatus;
    let order = |coid: &str, status: OrderStatus| vike_exec::OrderView {
        data_delay: None,
        client_order_id: coid.into(),
        venue: "binance".into(),
        account: None,
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(1.0),
        trigger_price: None,
        status,
        venue_order_id: None,
        filled_qty: 0.0,
        avg_fill_px: 0.0,
        reason: None,
        pending_price: None,
    };
    let facts = NodeFacts::default();
    let ages = |snap: &CoreSnapshot, at: Instant| -> Vec<Option<u64>> {
        let mut wire = project(snap, None, &[]);
        facts.stamp(&mut wire, snap, at);
        wire.orders.iter().map(|o| o.age_ms).collect()
    };
    let t0 = Instant::now();
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.orders = vec![order("c-1", OrderStatus::Accepted), order("c-2", OrderStatus::Filled)];

    // First sight: the resting order is new, the terminal one carries no age at all.
    assert_eq!(ages(&snap, t0), vec![Some(0), None]);
    // A later frame counts from the FIRST sight, not from itself.
    let hour = t0 + Duration::from_secs(3600);
    assert_eq!(ages(&snap, hour), vec![Some(3_600_000), None]);
    // A second order that appears now starts at zero while the first keeps growing.
    snap.orders.push(order("c-3", OrderStatus::PartiallyFilled));
    assert_eq!(ages(&snap, hour), vec![Some(3_600_000), None, Some(0)]);
    assert_eq!(
        ages(&snap, hour + Duration::from_secs(60)),
        vec![Some(3_660_000), None, Some(60_000)]
    );
    // An order that ends loses its age and its entry (the registry itself is never pruned).
    snap.orders[0].status = OrderStatus::Filled;
    assert_eq!(ages(&snap, hour + Duration::from_secs(61)), vec![None, None, Some(61_000)]);
    let kept: Vec<String> = facts.first_seen.lock().unwrap().keys().cloned().collect();
    assert_eq!(kept, vec!["c-3".to_string()]);
}

/// Through a REAL publisher: a handle handed a sentinel path answers every `Request::Snapshot` with
/// the file's state at that moment, and one never handed a path says nothing.
#[test]
fn a_publisher_frames_the_sentinel_it_was_handed_and_stays_silent_otherwise() {
    let framed = |handle: &PublisherHandle| match read_frame::<_, Response>(
        &mut std::io::Cursor::new(handle.snapshot_frame_now().as_slice().to_vec()),
    )
    .expect("decode frame")
    {
        Response::SnapshotFrame(w) => *w,
        other => panic!("expected SnapshotFrame, got {other:?}"),
    };
    let cell = || Arc::new(ArcSwap::from_pointee(CoreSnapshot::empty("sim", "BTCUSDT")));

    let silent = spawn(cell(), None);
    let frame = framed(&silent);
    assert_eq!((frame.halt_sentinel, frame.uptime_ms.is_some()), (None, true));
    silent.shutdown();

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("HALT");
    let watching = spawn(cell(), None).with_halt_sentinel_path(path.clone());
    assert_eq!(framed(&watching).halt_sentinel, Some(false));
    std::fs::write(&path, b"").expect("touch the sentinel");
    assert_eq!(framed(&watching).halt_sentinel, Some(true));
    watching.shutdown();
}

/// **The route-key roster is one key per engine that RUNS** (v0.1.80 review b). The core's list only grows and a
/// stopped account keeps its block, but the core counts only live engines when it asks whether a venue has
/// several, so the bracket gate must not count a restarted account's old block (a duplicate key) or a stopped
/// lane twin's. A raw list refused every bracket of the venue for as long as either was published.
#[test]
fn the_route_key_roster_is_one_key_per_engine_that_runs() {
    use vike_exec::NotRunningAccount;
    let block = |key: &str| vike_exec::VenueBlock {
        venue: "binance".into(),
        route_key: key.into(),
        ..Default::default()
    };
    let bracket = vike_tradehub_client::wire::WireCommand::Bracket(
        vike_tradehub_client::wire::WireBracketSpec {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            entry_price: Some(1.0),
            stop_loss: 0.9,
            take_profit: 1.1,
            account: None,
        },
    );
    // The default account was restarted (its old block is first), and the lane twin was stopped (flat, row gone).
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.portfolio.venues = vec![block("binance"), block("binance#PERP"), block("binance")];
    let stopped_twin = NotRunningAccount {
        venue: "binance".into(),
        account: vike_model::accounts::account_keys::AccountLabel::parse("PERP").ok(),
        route_key: "binance#PERP".into(),
        tier: None,
        reason:
            "the perp lane is not running: state a leverage on the binance demo account to start it"
                .into(),
    };

    // Before the twin is listed as stopped it runs: two accounts, one key each, and the bracket is refused.
    let handle = spawn(Arc::new(ArcSwap::from_pointee(snap)), None);
    assert_eq!(handle.engine_route_keys(), vec!["binance#PERP".to_string(), "binance".to_string()]);
    assert!(
        crate::server::refusal::account_refusal(&bracket, &handle.engine_route_keys()).is_some()
    );

    // Listed as stopped it is no account of the exchange (as the core counts), and the bracket goes to the
    // venue's one account.
    handle.set_not_running(vec![stopped_twin]);
    assert_eq!(handle.engine_route_keys(), vec!["binance".to_string()]);
    assert_eq!(
        crate::server::refusal::account_refusal(&bracket, &handle.engine_route_keys()),
        None
    );
    // The blocks themselves are untouched: a stopped account's position is still shown.
    assert_eq!(handle.engine_blocks().len(), 2);
    handle.shutdown();
}
