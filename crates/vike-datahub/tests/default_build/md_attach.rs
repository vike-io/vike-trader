//! The market-data plane's quote lane over a credentialed venue (decision 0193, verdicts 8 and 9): the
//! REAL broker and the REAL hub, wired the way `start_market_data_plane` wires them, over a scripted
//! client that logs every verb and counts every build. What these prove is what an Observe subscription
//! costs the venue and what it does to the strategy's and the recorder's streams: nothing and nothing.
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use vike_data::delivery::Delivery;
use vike_data::{DataClient, LiveDataError, LiveDataSink, NoopSink, StreamLabel, SubscriptionId};
use vike_datahub::feeds::venues::sharing_for;
use vike_datahub::feeds::{ClientBuilder, FeedBroker, Holder};
use vike_datahub::md::venues::{attach_gate, market_builder};
use vike_datahub::md::{Attach, MdHub};
use vike_datahub::test_support::drain_md;
use vike_datahub_client::market::{MdFrame, MdLane, MdRefusal, MdSpec, QuoteSnapshot};
use vike_marketdata::QuoteTick;
use vike_model::{DataDelay, StreamStatus};

type Log = Arc<Mutex<Vec<String>>>;

/// A client that logs each verb the broker asks of it. A log line is a thing the venue would see.
struct Scripted {
    log: Log,
    next: u64,
}

impl Scripted {
    fn verb(&mut self, line: String) -> Result<SubscriptionId, LiveDataError> {
        self.next += 1;
        self.log.lock().unwrap().push(line);
        Ok(SubscriptionId(self.next))
    }
}

impl DataClient for Scripted {
    fn subscribe_bars(&mut self, s: &str, i: &str) -> Result<SubscriptionId, LiveDataError> {
        self.verb(format!("bars:{i} {s}"))
    }
    fn subscribe_quotes(&mut self, s: &str) -> Result<SubscriptionId, LiveDataError> {
        self.verb(format!("quotes {s}"))
    }
    fn subscribe_trades(&mut self, s: &str) -> Result<SubscriptionId, LiveDataError> {
        self.verb(format!("trades {s}"))
    }
    fn subscribe_book(&mut self, s: &str) -> Result<SubscriptionId, LiveDataError> {
        self.verb(format!("book {s}"))
    }
    fn subscribe_depth(&mut self, s: &str) -> Result<SubscriptionId, LiveDataError> {
        self.verb(format!("depth {s}"))
    }
    fn unsubscribe(&mut self, id: SubscriptionId) {
        self.log.lock().unwrap().push(format!("unsubscribe {}", id.0));
    }
    fn begin_shutdown(&mut self) {
        self.log.lock().unwrap().push("begin_shutdown".to_string());
    }
    fn set_delivery(
        &mut self,
        s: &str,
        lane: StreamLabel,
        class: Delivery,
    ) -> Result<(), LiveDataError> {
        self.log.lock().unwrap().push(format!("set_delivery {s} {} {class:?}", lane.as_str()));
        Ok(())
    }
    fn is_dead(&self, id: SubscriptionId) -> bool {
        self.log.lock().unwrap().contains(&format!("dead {}", id.0))
    }
    fn shutdown(&mut self) {
        self.log.lock().unwrap().push("shutdown".to_string());
    }
}

/// The broker over the real sharing table, the hub over the real gate and builder, and every sink a
/// holder needs. `builds` counts clients: on a credentialed venue one client is one login.
struct Rig {
    broker: Arc<FeedBroker>,
    hub: Arc<MdHub>,
    log: Log,
    builds: Arc<AtomicUsize>,
    sinks: Arc<Mutex<Vec<Arc<dyn LiveDataSink>>>>,
}

fn rig() -> Rig {
    let (log, builds): (Log, _) = (Log::default(), Arc::new(AtomicUsize::new(0)));
    let sinks: Arc<Mutex<Vec<Arc<dyn LiveDataSink>>>> = Arc::default();
    let (l, b, s) = (Arc::clone(&log), Arc::clone(&builds), Arc::clone(&sinks));
    let table: ClientBuilder = Box::new(
        move |_venue: &str,
              sink: Arc<dyn LiveDataSink>|
              -> Result<Box<dyn DataClient + Send>, String> {
            b.fetch_add(1, Ordering::SeqCst);
            s.lock().unwrap().push(sink);
            Ok(Box::new(Scripted { log: Arc::clone(&l), next: 0 }))
        },
    );
    let broker = FeedBroker::new(table, sharing_for);
    broker.attach_sink(Holder::Strat, Arc::new(NoopSink));
    broker.attach_sink(Holder::Rec, Arc::new(NoopSink));
    let hub = MdHub::with_gate(
        market_builder(Arc::clone(&broker), vec!["oanda", "ibkr", "fxcm"]),
        vec!["oanda".into(), "ibkr".into(), "fxcm".into()],
        attach_gate(Arc::clone(&broker)),
    );
    Rig { broker, hub, log, builds, sinks }
}

impl Rig {
    fn verbs(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
    fn venue_says(&self, symbol: &str, bid: f64) {
        let sink = Arc::clone(&self.sinks.lock().unwrap()[0]);
        let tick = QuoteTick {
            ts: 1,
            local_ts: 0,
            bid,
            ask: bid + 1.0,
            bid_size: 0.0,
            ask_size: 0.0,
            symbol: symbol.to_string(),
        };
        sink.quote("oanda", symbol, tick);
    }
}

fn quotes(venue: &str, symbol: &str) -> MdSpec {
    MdSpec { venue: venue.into(), symbol: symbol.into(), lane: MdLane::Quotes, depth_levels: None }
}

fn only_quotes(frames: Vec<MdFrame>) -> Vec<QuoteSnapshot> {
    frames
        .into_iter()
        .filter_map(|f| match f {
            MdFrame::Quote(q) => Some(q),
            _ => None,
        })
        .collect()
}

fn now() -> i64 {
    vike_model::now_ms()
}

/// **An Observe subscription on a key nobody holds costs the venue nothing**: no client is built, no verb
/// is sent, and the refusal is the non-permanent one. On a credentialed venue one client is one login, so
/// `builds` is the login counter. Reddens if the hub's gate stops refusing, and the reconciler pass proves
/// a key that was refused left nothing behind to ask for later.
#[test]
fn an_observe_subscription_on_an_unheld_key_costs_the_venue_nothing() {
    let r = rig();
    let g = r.hub.open_session().unwrap();
    assert_eq!(r.hub.acquire(g.id(), &quotes("oanda", "EUR_USD")), Err(MdRefusal::NotHeld));
    assert!(!MdRefusal::NotHeld.is_permanent());
    r.hub.reconcile(now());
    assert_eq!(r.builds.load(Ordering::SeqCst), 0, "a client built is a login at the venue");
    assert_eq!(r.verbs(), Vec::<String>::new(), "the venue was asked for something");
    assert_eq!(r.hub.quote_keys(), 0, "a refused key holds no slot");
}

/// **The same over a real socket, as the client sees it**: the handshake names the venues the row opened
/// and the lane's word, a key nobody holds comes back `NotHeld` beside a venue the row left out, which is
/// the permanent refusal it always was, and the venue was still asked for nothing. The hub's own suite
/// never crosses the wire, and an Observe client does.
#[test]
fn over_the_wire_a_key_nobody_holds_is_refused_and_the_venue_hears_nothing() {
    use vike_data::{HistStore, MemHistStore};
    use vike_datahub_client::proto::FEATURE_MD_LANE_QUOTES;
    use vike_datahub_client::{DatahubClient, advertised_md_venues};

    let r = rig();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("the assigned port");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    let hub = Arc::clone(&r.hub);
    std::thread::spawn(move || {
        let _ = vike_datahub::serve_authed(listener, store, None, None, Some(hub), None, None);
    });

    let client = DatahubClient::connect(addr).expect("connect");
    assert_eq!(advertised_md_venues(client.features()), ["oanda", "ibkr", "fxcm"]);
    assert!(client.features().iter().any(|f| f == FEATURE_MD_LANE_QUOTES));
    let (info, _stream) = client
        .md_subscribe(vec![quotes("oanda", "EUR_USD"), quotes("ig", "CS.D.EURUSD.CFD.IP")])
        .map_err(|(_, why)| why)
        .expect("a subscribe answers per spec");
    assert!(info.accepted.is_empty(), "{:?}", info.accepted);
    assert_eq!(info.refused[0].1, MdRefusal::NotHeld);
    assert!(
        matches!(&info.refused[1].1, MdRefusal::VenueNotServed(served) if !served.contains("ig")),
        "the row left ig out: {:?}",
        info.refused[1]
    );
    assert_eq!(r.builds.load(Ordering::SeqCst), 0);
    assert_eq!(r.verbs(), Vec::<String>::new());
}

/// **The broker refuses on its own, whatever the hub above it does** (the second fence): a market-data
/// handle asking for an unheld key builds no client and sends no verb. Reddens if `acquire` loses the
/// attach path or `attach_md` reaches for `state`, which builds a client.
#[test]
fn the_brokers_own_door_refuses_a_market_data_handle_with_no_client_built() {
    let r = rig();
    r.broker.attach_sink(Holder::Md, r.hub.sink());
    let mut md = r.broker.handle(Holder::Md, "oanda");
    assert!(md.subscribe_quotes("EUR_USD").is_err());
    assert_eq!(r.builds.load(Ordering::SeqCst), 0);

    // With a client up for ANOTHER key the same holds: the existing client is asked for nothing.
    let mut strat = r.broker.handle(Holder::Strat, "oanda");
    strat.subscribe_quotes("GBP_USD").unwrap();
    let before = r.verbs();
    assert!(md.subscribe_quotes("EUR_USD").is_err());
    assert_eq!(r.verbs(), before, "a window asked the venue for a key nobody holds");
}

/// **Attaching and detaching never disturb the strategy's or the recorder's stream**: from the moment the
/// key is held, a whole window's life sends the venue nothing at all, the real subscription keeps its
/// one id, and both owners stay on it. Reddens on an attach that re-subscribes, on a detach that
/// unsubscribes, and on a window's hold that changes the key's delivery class.
#[test]
fn attaching_and_detaching_never_disturb_the_strategy_or_the_recorder() {
    let r = rig();
    let (mut strat, mut rec) =
        (r.broker.handle(Holder::Strat, "oanda"), r.broker.handle(Holder::Rec, "oanda"));
    strat.subscribe_quotes("EUR_USD").unwrap();
    rec.subscribe_quotes("EUR_USD").unwrap();
    let held = r.verbs();
    assert_eq!(held, ["quotes EUR_USD"], "the floor: one real subscription for the two owners");

    let mut g = r.hub.open_session().unwrap();
    r.hub.acquire(g.id(), &quotes("oanda", "EUR_USD")).expect("a held key is joined");
    assert_eq!(r.hub.reconcile(now()).started, 1, "the window attached");
    assert_eq!(r.verbs(), held, "the attach reached the venue");

    r.venue_says("EUR_USD", 1.5);
    r.hub.publish_tick();
    let seen = only_quotes(drain_md(g.mailbox()));
    assert_eq!(seen.len(), 1, "the window reads the stream: {seen:?}");
    assert_eq!((seen[0].bid, seen[0].ask), (1.5, 2.5));

    // The linger is passed in the clock, so the reap really runs.
    let closed = now();
    g.release_at(closed);
    let report = r.hub.reconcile(closed + 2 * vike_datahub::md::MD_LINGER.as_millis() as i64);
    assert_eq!(report.stopped, 1, "the window's hold was released: {report:?}");
    assert_eq!(r.verbs(), held, "the detach reached the venue");
    assert_eq!(r.broker.real_subscriptions(), 1, "the owners' stream is still there");
    assert_eq!(r.hub.quote_keys(), 0, "a feed thread's quote costs one atomic again");
}

/// **The last owner closes the stream under an attached window, and the window hears a gap, not silence**.
/// Its hold survives, and it reads the stream again when an owner reopens the key. Reddens if an attached
/// window keeps a stream open on its own, which would be an Observe client spending the venue's stream
/// allowance on the operator's account.
#[test]
fn a_window_holds_no_stream_open_and_reads_it_again_when_an_owner_returns() {
    let r = rig();
    let mut strat = r.broker.handle(Holder::Strat, "oanda");
    let id = strat.subscribe_quotes("EUR_USD").unwrap();
    let g = r.hub.open_session().unwrap();
    r.hub.acquire(g.id(), &quotes("oanda", "EUR_USD")).unwrap();
    r.hub.reconcile(now());
    r.hub.publish_tick();
    drain_md(g.mailbox());

    strat.unsubscribe(id);
    assert!(r.verbs().contains(&"unsubscribe 1".to_string()), "still open: {:?}", r.verbs());
    assert_eq!(r.broker.real_subscriptions(), 0);
    r.hub.publish_tick();
    let frames = drain_md(g.mailbox());
    assert!(
        frames.iter().any(|f| matches!(
            f,
            MdFrame::Status { lane: MdLane::Quotes, status: StreamStatus::GapStart { .. }, .. }
        )),
        "the window must be told its stream is gone: {frames:?}"
    );

    strat.subscribe_quotes("EUR_USD").unwrap();
    assert_eq!(r.verbs().iter().filter(|v| v.starts_with("quotes")).count(), 2, "reopened");
    r.venue_says("EUR_USD", 3.0);
    r.hub.publish_tick();
    let frames = drain_md(g.mailbox());
    assert!(
        frames.iter().any(|f| matches!(
            f,
            MdFrame::Status { lane: MdLane::Quotes, status: StreamStatus::Live { .. }, .. }
        )),
        "and told it is back: {frames:?}"
    );
    assert_eq!(only_quotes(frames).len(), 1, "without asking again");
}

/// **A key accepted while it was held and let go before the reconciler came is simply not joined yet**:
/// the pass asks no client and counts no failure, which would be one log line every reap for as long as
/// the window stays open, and a later pass joins it once an owner holds the key again.
#[test]
fn a_key_let_go_before_the_reconciler_came_is_joined_when_an_owner_returns() {
    let r = rig();
    let mut strat = r.broker.handle(Holder::Strat, "oanda");
    let id = strat.subscribe_quotes("EUR_USD").unwrap();
    let g = r.hub.open_session().unwrap();
    r.hub.acquire(g.id(), &quotes("oanda", "EUR_USD")).unwrap();
    strat.unsubscribe(id);

    let report = r.hub.reconcile(now());
    assert_eq!((report.started, report.failed.len()), (0, 0), "{report:?}");

    strat.subscribe_quotes("EUR_USD").unwrap();
    assert_eq!(r.hub.reconcile(now()).started, 1, "joined on the pass after the owner returned");
    r.venue_says("EUR_USD", 1.5);
    r.hub.publish_tick();
    assert_eq!(only_quotes(drain_md(g.mailbox())).len(), 1);
}

/// **A window does not cost a strategy its first price** (rule eight beside rule nine): a strategy that
/// joins a quiet key only the recorder holds is given a fresh stream whether or not a window is attached.
/// Reddens if md's hold counts in rule eight, which would leave the strategy with no price on a quiet pair
/// for as long as somebody on a desktop happens to be looking at it.
#[test]
fn a_window_does_not_cost_a_strategy_its_fresh_stream() {
    let r = rig();
    let mut rec = r.broker.handle(Holder::Rec, "oanda");
    rec.subscribe_quotes("EUR_USD").unwrap();
    let g = r.hub.open_session().unwrap();
    r.hub.acquire(g.id(), &quotes("oanda", "EUR_USD")).unwrap();
    r.hub.reconcile(now());

    let mut strat = r.broker.handle(Holder::Strat, "oanda");
    strat.subscribe_quotes("EUR_USD").unwrap();
    assert_eq!(
        r.verbs(),
        ["quotes EUR_USD", "unsubscribe 1", "quotes EUR_USD"],
        "a key with no quote yet is quiet, and the joiner gets a fresh stream"
    );
    assert_eq!(r.broker.real_subscriptions(), 1);
}

/// **A strategy alone with a window still shuts its client down the fast way**: a window's hold makes the
/// strategy no less the sole owner. Reddens if md's hold counts in `begin_shutdown_if_sole`, which would
/// turn the raise-all stop into a join per key because somebody was looking.
#[test]
fn a_window_does_not_make_its_owner_less_sole_at_shutdown() {
    let r = rig();
    let mut strat = r.broker.handle(Holder::Strat, "oanda");
    strat.subscribe_quotes("EUR_USD").unwrap();
    let _window = {
        let g = r.hub.open_session().unwrap();
        r.hub.acquire(g.id(), &quotes("oanda", "EUR_USD")).unwrap();
        g
    };
    r.hub.reconcile(now());
    strat.begin_shutdown();
    assert!(r.verbs().contains(&"begin_shutdown".to_string()), "{:?}", r.verbs());
}

/// **A stream its client reports dead is not a window's to replace**: rule five's "whoever asks next"
/// re-subscribes, and a window is not allowed to ask. Reddens if `attach_md` joins a dead stream or
/// falls through to the path that subscribes afresh; the strategy's next ask is what repairs it.
#[test]
fn a_window_does_not_repair_a_dead_stream() {
    let r = rig();
    let mut strat = r.broker.handle(Holder::Strat, "oanda");
    strat.subscribe_quotes("EUR_USD").unwrap();
    r.log.lock().unwrap().push("dead 1".to_string());
    let before = r.verbs();
    r.broker.attach_sink(Holder::Md, r.hub.sink());
    let mut md = r.broker.handle(Holder::Md, "oanda");
    assert!(md.subscribe_quotes("EUR_USD").is_err());
    assert_eq!(r.verbs(), before, "a window asked the venue for a stream");
}

/// **A resident row is the operator's own hold, and the one thing on this plane that opens a key**
/// (verdict 8 names `config.datahub_live_resident` among the operator-owned holders). It opens a client,
/// a window then joins it with no further verb, and it survives the window. Reddens if a pin stops
/// counting, in either direction: a pinned key that cannot open, or an unpinned one that can.
#[test]
fn a_resident_row_opens_the_stream_and_a_window_joins_it() {
    let r = rig();
    assert_eq!(
        attach_gate(Arc::clone(&r.broker))("oanda", "EUR_USD", MdLane::Quotes),
        Attach::NotHeld,
        "the floor: unpinned, the key is nobody's"
    );
    r.hub.add_resident(&quotes("oanda", "EUR_USD")).expect("the operator named it");
    r.broker.pin_md_key("oanda", "EUR_USD", StreamLabel::Quotes);
    r.hub.reconcile(now());
    assert_eq!(r.builds.load(Ordering::SeqCst), 1, "the operator's own login");
    assert_eq!(r.verbs(), ["quotes EUR_USD"]);

    let mut g = r.hub.open_session().unwrap();
    r.hub.acquire(g.id(), &quotes("oanda", "EUR_USD")).expect("joined");
    g.release_at(now());
    r.hub.reconcile(now() + 2 * vike_datahub::md::MD_LINGER.as_millis() as i64);
    assert_eq!(r.verbs(), ["quotes EUR_USD"], "the window cost nothing and took nothing with it");
    assert_eq!(r.broker.real_subscriptions(), 1);
}

/// **The delay label reaches the window as delayed**, also when the stream said it before the window
/// came (a label is sent when it is first known and when it changes, never again for a holder that joins
/// later), and a price already on screen is re-sent under a new label. Reddens on `restate_delay_to_md`
/// going, and on the sink dropping `data_delay` as it did when this wire had no quote frame.
#[test]
fn the_delay_label_reaches_the_window_as_delayed() {
    let r = rig();
    let mut strat = r.broker.handle(Holder::Strat, "ibkr");
    strat.subscribe_quotes("AAPL").unwrap();
    let sink = Arc::clone(&r.sinks.lock().unwrap()[0]);
    sink.data_delay("ibkr", "AAPL", "quotes", DataDelay::DELAYED);

    let g = r.hub.open_session().unwrap();
    r.hub.acquire(g.id(), &quotes("ibkr", "AAPL")).unwrap();
    r.hub.reconcile(now());
    sink.quote(
        "ibkr",
        "AAPL",
        QuoteTick {
            ts: 1,
            local_ts: 0,
            bid: 190.1,
            ask: 190.2,
            bid_size: 1.0,
            ask_size: 2.0,
            symbol: "AAPL".into(),
        },
    );
    r.hub.publish_tick();
    let seen = only_quotes(drain_md(g.mailbox()));
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert_eq!(seen[0].delay, Some(DataDelay::DELAYED), "the label said before the attach");

    sink.data_delay("ibkr", "AAPL", "quotes", DataDelay::Frozen);
    r.hub.publish_tick();
    let seen = only_quotes(drain_md(g.mailbox()));
    assert_eq!(seen.len(), 1, "a change of label re-sends the price: {seen:?}");
    assert_eq!(seen[0].delay, Some(DataDelay::Frozen));
}

/// **The caps table does not decide a credentialed quote key, and it still decides everything else**:
/// FXCM's row declares no live data although the gatewayhub streams it a price, so a key a strategy holds
/// is joined, and a lane the venue declares nothing for is the permanent refusal it always was.
#[test]
fn a_held_quote_key_is_joined_whatever_the_caps_table_says_of_the_venue() {
    let r = rig();
    assert!(vike_data::require_live_verb("fxcm", MdLane::Quotes.live_verb()).is_err(), "the floor");
    let g = r.hub.open_session().unwrap();
    assert_eq!(r.hub.acquire(g.id(), &quotes("fxcm", "EUR/USD")), Err(MdRefusal::NotHeld));

    let mut strat = r.broker.handle(Holder::Strat, "fxcm");
    strat.subscribe_quotes("EUR/USD").unwrap();
    r.hub.acquire(g.id(), &quotes("fxcm", "EUR/USD")).expect("a held key is joined");

    let depth = MdSpec {
        venue: "oanda".into(),
        symbol: "X".into(),
        lane: MdLane::Depth,
        depth_levels: None,
    };
    assert!(
        matches!(r.hub.acquire(g.id(), &depth), Err(MdRefusal::LaneUnsupported(_))),
        "oanda declares no depth, held or not, and that one is permanent"
    );
}

/// **A gap on the quote lane stays a gap, in the order the hub really sends it**: a `GapStart` or a `Stale` is the last
/// frame of its tick, with no `Quote` of the price it cut off behind it, whether that price was published before or is
/// still waiting in the same tick. The client forgets the price on the status and takes any `Quote` as a stream that
/// flows, so that frame would show a frozen bid and ask as live. A fresh price is handed on, and a window that attaches
/// to the recovered key is handed no price from before the gap. Reddens on `store_status` not emptying the slot, for
/// either word; a `Live` emptying it too is `a_quote_handed_to_a_new_subscriber_says_how_old_it_is`.
#[test]
fn a_gap_on_the_quote_lane_is_not_undone_by_the_price_it_cut_off() {
    let words = [
        StreamStatus::GapStart { at_ts_ms: 1 },
        StreamStatus::Stale { newest_data_ts_ms: 1, now_ms: 2 },
    ];
    for word in words {
        let r = rig();
        let mut strat = r.broker.handle(Holder::Strat, "oanda");
        strat.subscribe_quotes("EUR_USD").unwrap();
        let g = r.hub.open_session().unwrap();
        r.hub.acquire(g.id(), &quotes("oanda", "EUR_USD")).unwrap();
        r.hub.reconcile(now());
        let sink = Arc::clone(&r.sinks.lock().unwrap()[0]);
        r.venue_says("EUR_USD", 1.5);
        r.hub.publish_tick();
        assert_eq!(
            only_quotes(drain_md(g.mailbox())).len(),
            1,
            "the floor: the price is on screen"
        );

        // Across two ticks: the price went out earlier, and the status marks the key dirty.
        sink.stream_status("oanda", "EUR_USD", "quotes", word);
        r.hub.publish_tick();
        let frames = drain_md(g.mailbox());
        assert_eq!(frames.len(), 1, "{word:?}: the status and nothing behind it: {frames:?}");
        assert!(
            matches!(frames[0], MdFrame::Status { status, .. } if status == word),
            "{frames:?}"
        );

        // In one tick: a price, then the word that cuts it off.
        r.venue_says("EUR_USD", 2.5);
        sink.stream_status("oanda", "EUR_USD", "quotes", word);
        r.hub.publish_tick();
        assert_eq!(only_quotes(drain_md(g.mailbox())), Vec::new(), "{word:?}");

        // The stream comes back: no price of before the gap, for a window here or one that attaches now.
        sink.stream_status(
            "oanda",
            "EUR_USD",
            "quotes",
            StreamStatus::Live { gap_started_ts_ms: Some(1) },
        );
        r.hub.publish_tick();
        assert_eq!(only_quotes(drain_md(g.mailbox())), Vec::new(), "{word:?}");
        let key = vike_datahub::md::MdKey {
            venue: "oanda".into(),
            symbol: "EUR_USD".into(),
            lane: MdLane::Quotes,
        };
        assert_eq!(only_quotes(r.hub.attach_frames(&key, now())), Vec::new(), "{word:?}");

        // A price that really is new goes out.
        r.venue_says("EUR_USD", 3.5);
        r.hub.publish_tick();
        let seen = only_quotes(drain_md(g.mailbox()));
        assert_eq!(seen.iter().map(|q| q.bid).collect::<Vec<_>>(), [3.5], "{word:?}");
    }
}

/// **The last owner letting go under a window (rule nine) is that same gap, and it is the path the finding walked**: the
/// real broker says `GapStart` to md, and the price of the stream that just closed does not follow it.
#[test]
fn the_last_owner_letting_go_leaves_a_window_a_gap_and_no_price() {
    let r = rig();
    let mut strat = r.broker.handle(Holder::Strat, "oanda");
    let id = strat.subscribe_quotes("EUR_USD").unwrap();
    let g = r.hub.open_session().unwrap();
    r.hub.acquire(g.id(), &quotes("oanda", "EUR_USD")).unwrap();
    r.hub.reconcile(now());
    r.venue_says("EUR_USD", 1.5);
    r.hub.publish_tick();
    assert_eq!(only_quotes(drain_md(g.mailbox())).len(), 1, "the floor: the price is on screen");

    strat.unsubscribe(id);
    r.hub.publish_tick();
    let frames = drain_md(g.mailbox());
    assert!(
        frames.iter().any(|f| matches!(
            f,
            MdFrame::Status { lane: MdLane::Quotes, status: StreamStatus::GapStart { .. }, .. }
        )),
        "the window is told: {frames:?}"
    );
    assert_eq!(only_quotes(frames), Vec::new(), "the closed stream's price came after its gap");
}

/// **A key nobody holds is refused in a word an older desktop can read, unless the lane is the one only a new client
/// asks for**: `NotHeld` is a variant an older `MdRefusal` does not have, so it fails the decode of the whole reply and
/// every key of that desktop retries. Reddens on the lane rule going, either way: `NotHeld` on the trade key, or a quote
/// key turned permanent. A held key is still joined, and the venue is still asked for nothing.
#[test]
fn an_unheld_key_of_an_older_desktops_lane_is_refused_in_a_word_it_can_read() {
    let r = rig();
    let g = r.hub.open_session().unwrap();
    let trades = MdSpec {
        venue: "ibkr".into(),
        symbol: "AAPL".into(),
        lane: MdLane::Trades,
        depth_levels: None,
    };
    assert!(vike_data::require_live_verb("ibkr", MdLane::Trades.live_verb()).is_ok(), "the floor");

    assert!(
        matches!(r.hub.acquire(g.id(), &trades), Err(MdRefusal::LaneUnsupported(_))),
        "an older desktop cannot decode NotHeld"
    );
    assert_eq!(r.hub.acquire(g.id(), &quotes("ibkr", "AAPL")), Err(MdRefusal::NotHeld));
    assert_eq!(r.builds.load(Ordering::SeqCst), 0, "a client built is a login at the venue");

    let mut strat = r.broker.handle(Holder::Strat, "ibkr");
    strat.subscribe_trades("AAPL").unwrap();
    assert!(r.hub.acquire(g.id(), &trades).is_ok(), "a key a strategy holds is joined");
}

/// **A quote handed to a new subscriber says how old it is**: its age is the hub's clock against the
/// hub's own receipt, so a second desktop on a quiet pair is not shown a price as if it had just arrived.
/// The `Live` status is what lets `attach_frames` hand one out at all.
#[test]
fn a_quote_handed_to_a_new_subscriber_says_how_old_it_is() {
    let r = rig();
    let mut strat = r.broker.handle(Holder::Strat, "oanda");
    strat.subscribe_quotes("EUR_USD").unwrap();
    let g = r.hub.open_session().unwrap();
    r.hub.acquire(g.id(), &quotes("oanda", "EUR_USD")).unwrap();
    r.hub.reconcile(now());
    r.venue_says("EUR_USD", 1.5);
    let sink = Arc::clone(&r.sinks.lock().unwrap()[0]);
    sink.stream_status(
        "oanda",
        "EUR_USD",
        "quotes",
        StreamStatus::Live { gap_started_ts_ms: None },
    );
    let key = vike_datahub::md::MdKey {
        venue: "oanda".into(),
        symbol: "EUR_USD".into(),
        lane: MdLane::Quotes,
    };
    let attach = r.hub.attach_frames(&key, now() + 5_000);
    let handed = only_quotes(attach);
    assert_eq!(handed.len(), 1, "a Live key's quote is handed out");
    assert!(handed[0].age_ms >= 5_000, "as old as it is: {}", handed[0].age_ms);
}
