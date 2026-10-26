//! LIVE FXCM demo smoke of CLOSING A TRADE BY ITS ID (2026-10-26): on the HEDGING demo account a reduce-only market
//! order closes the trades facing it instead of opening the opposite one (`crates/bridges/fxcm/src/close.rs`).
//!
//! ```text
//! export FCSDK_DIR=<repo>/vendor/fcsdk/linux LD_LIBRARY_PATH=$FCSDK_DIR/lib VIKE_SETTINGS_DIR=<project>/settings
//! cargo test -p vike-fxcm --features fxcm --test fxcm_close_smoke -- --ignored --nocapture
//! ```
//!
//! **PLACES REAL ORDERS on the demo account**: about a dozen EUR/USD market orders of at most 2000 units, never more
//! than 3000 open at once, and it ends FLAT (it asserts so, and closes what is left if a step fails). It needs the
//! market OPEN (Monday to Thursday, 12:30-15:30 UTC is the liquid window), a demo account that holds NO EUR/USD trade
//! and no resting order (it refuses to start otherwise: it touches only its own trades), and the daemon's own FXCM
//! account stopped (`vike-cli secrets account deactivate --id <N>`; reactivate it after): it opens TWO sessions of the
//! demo login, exec and reconcile, as the daemon does.
//!
//! What it proves, each step against the VENUE's own table read back through the reconcile client:
//!
//! 1. a plain BUY opens a trade (the order path as it always was);
//! 2. a reduce-only SELL of HALF that trade closes half of it BY ID: the closing fill carries its own trade id
//!    (`<trade id>-close-<closing order id>`, never the opening fill's), and the venue holds the other half;
//! 3. a reduce-only SELL of MORE than is held closes what is held and opens nothing;
//! 4. a reduce-only SELL with nothing to close is refused BY NAME, and nothing opens;
//! 5. a reduce-only SELL of two trades closes both, oldest first, as ONE order (a partial fill, then the fill);
//! 6. a plain opposite order still opens a HEDGE (the locked pair), and the reduce-only orders unwind it one trade at a
//!    time, each closing the trade that faces it and not its twin.
//!
//! Triple-gated like the other `*_smoke.rs`: `#[ignore]`, self-skips with no fxcm demo account in the store, and
//! self-skips on a box whose shim did not load (`no ForexConnect shim` is NOT a pass: look for `LOGIN OK`).

use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::time::Duration;

use vike_exec::recon::ReconClient;
use vike_exec::{ExecutionClient, Ingest, event_channel};
use vike_fxcm::{FxcmConfig, FxcmExecutionClient, load_fxcm_config_from, recon_client};
use vike_model::accounts::account_keys::AccountLabel;
use vike_model::events::Event;
use vike_model::{AccountTier, OrderRequest};

const SYMBOL: &str = "EURUSD";
/// ONE lot of EUR/USD on the demo is its base unit size, 1000 (measured).
const LOT: f64 = 1000.0;

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64
}

/// The demo fxcm account's login: the active default row when there is one, else the one fxcm demo row there is (this
/// smoke runs while the daemon's account is deactivated).
fn config() -> Option<FxcmConfig> {
    let settings_dir = std::env::var("VIKE_SETTINGS_DIR").ok();
    let dir = vike_secrets::smoke_directory();
    let settings = vike_paths::state_path::project_settings_dir_from(
        settings_dir.as_deref(),
        &std::env::current_dir().expect("a working directory"),
    )
    .map(|dir| {
        vike_secrets::venue_setting::load_venue_settings(&dir)
            .unwrap_or_else(|e| panic!("the settings database could not be read: {e}"))
    })
    .and_then(|mut all| all.remove("fxcm"))
    .unwrap_or_default();
    let active = dir.credentials_for("fxcm", &AccountLabel::Default, AccountTier::Demo);
    if let Some(cfg) = load_fxcm_config_from(AccountTier::Demo, &active, &settings) {
        return Some(cfg);
    }
    let id = {
        let rows = dir.rows()?.ok()?.known()?;
        let mut demo = rows
            .iter()
            .filter(|r| r.venue == "fxcm" && r.tier == AccountTier::Demo && r.label.is_none());
        let first = demo.next()?.id;
        demo.next().is_none().then_some(first)?
    };
    let book = dir.credentials()?.ok()?;
    load_fxcm_config_from(AccountTier::Demo, &book.view("fxcm", Some(id)), &settings)
}

fn market(coid: &str, side: i32, qty: f64, reduce_only: bool) -> OrderRequest {
    OrderRequest {
        client_order_id: coid.into(),
        venue: "fxcm".into(),
        symbol: SYMBOL.into(),
        side,
        qty,
        order_type: vike_model::OrderType::Market,
        reduce_only,
        ts: now_ms(),
        ..Default::default()
    }
}

/// An open trade as the venue's own table reads it back: `(trade id, side, units)`.
type Trade = (String, i32, f64);

/// The exec client, the ingest lane it answers on, and a reconcile session to read the venue's Trades table back with.
struct Venue {
    client: FxcmExecutionClient,
    rx: tokio::sync::mpsc::Receiver<Ingest>,
    recon: Box<dyn ReconClient>,
    seq: u32,
}

impl Venue {
    /// Submit one market order and collect its events until it ends (filled, cancelled or rejected).
    fn order(&mut self, side: i32, qty: f64, reduce_only: bool) -> Vec<Event> {
        self.seq += 1;
        let coid = format!("fxcl{}x{}", now_ms(), self.seq);
        self.client.submit(&market(&coid, side, qty, reduce_only));
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        let mut seen = Vec::new();
        loop {
            let next = rt.block_on(async {
                tokio::time::timeout(Duration::from_secs(60), self.rx.recv()).await
            });
            let ev = match next {
                Ok(Some(Ingest::Event(ev))) => ev,
                Ok(Some(other)) => panic!("expected Ingest::Event, got {other:?}"),
                Ok(None) => panic!("ingest channel closed"),
                Err(_) => panic!("no end to the order {coid} within 60 s; saw {:?}", names(&seen)),
            };
            let ended = matches!(
                ev,
                Event::OrderFilled(_) | Event::OrderCanceled(_) | Event::OrderRejected(_)
            );
            seen.push(ev);
            if ended {
                // The bare fills of the legs arrive before the wrap; nothing follows a terminal.
                return seen;
            }
        }
    }

    /// The open EUR/USD trades, read back from the venue.
    fn trades(&self) -> Vec<Trade> {
        let mut t: Vec<Trade> = self
            .recon
            .fetch_fill_reports(0)
            .expect("the reconcile read of the open trades")
            .into_iter()
            .map(|f| (f.trade_id.to_string(), f.side, f.last_qty))
            .collect();
        t.sort_by_key(|(id, _, _)| id.parse::<u64>().unwrap_or(u64::MAX));
        t
    }

    /// Close whatever EUR/USD trade is open, by reduce-only orders (the path under test, used as the cleanup too).
    fn flatten(&mut self) {
        for _ in 0..8 {
            let open = self.trades();
            let long: f64 = open.iter().filter(|t| t.1 > 0).map(|t| t.2).sum();
            let short: f64 = open.iter().filter(|t| t.1 < 0).map(|t| t.2).sum();
            if long == 0.0 && short == 0.0 {
                return;
            }
            if long > 0.0 {
                self.order(-1, long, true);
            }
            if short > 0.0 {
                self.order(1, short, true);
            }
        }
    }
}

fn names(evs: &[Event]) -> Vec<&'static str> {
    evs.iter()
        .map(|e| match e {
            Event::OrderSubmitted(_) => "OrderSubmitted",
            Event::OrderAccepted(_) => "OrderAccepted",
            Event::Fill(_) => "Fill",
            Event::OrderPartiallyFilled(_) => "OrderPartiallyFilled",
            Event::OrderFilled(_) => "OrderFilled",
            Event::OrderCanceled(_) => "OrderCanceled",
            Event::OrderRejected(_) => "OrderRejected",
            _ => "other",
        })
        .collect()
}

/// The bare fills among `evs`: `(trade id, side, units)`.
fn fills(evs: &[Event]) -> Vec<(String, i32, f64)> {
    evs.iter()
        .filter_map(|e| match e {
            Event::Fill(f) => Some((f.trade_id.to_string(), f.side, f.last_qty)),
            _ => None,
        })
        .collect()
}

fn the_steps(v: &mut Venue) {
    // 1. a plain BUY opens a trade.
    let evs = v.order(1, 2.0 * LOT, false);
    assert_eq!(names(&evs), ["OrderSubmitted", "OrderAccepted", "Fill", "OrderFilled"], "{evs:?}");
    let open_a = fills(&evs)[0].0.clone();
    assert_eq!(v.trades(), vec![(open_a.clone(), 1, 2.0 * LOT)]);
    println!("S1 plain BUY 2000: trade {open_a} open");

    // 2. a reduce-only SELL of half closes half of it, BY ID, under a closing fill id of its own.
    let evs = v.order(-1, LOT, true);
    assert_eq!(names(&evs), ["OrderSubmitted", "OrderAccepted", "Fill", "OrderFilled"], "{evs:?}");
    let closing = fills(&evs);
    assert_eq!((closing[0].1, closing[0].2), (-1, LOT), "{closing:?}");
    assert!(
        closing[0].0.starts_with(&format!("{open_a}-close-")),
        "the closing fill's id: {closing:?}"
    );
    let rest = v.trades();
    assert_eq!(rest.iter().map(|t| t.1 as f64 * t.2).sum::<f64>(), LOT, "half is left: {rest:?}");
    assert!(rest.iter().all(|t| t.1 > 0), "no opposite trade was opened: {rest:?}");
    println!("S2 reduce-only SELL 1000: closing fill {} ; the venue holds {rest:?}", closing[0].0);

    // 3. more than is held closes what is held and opens nothing.
    let evs = v.order(-1, 5.0 * LOT, true);
    assert_eq!(names(&evs), ["OrderSubmitted", "OrderAccepted", "Fill", "OrderFilled"], "{evs:?}");
    assert_eq!(fills(&evs)[0].2, LOT, "only what was held: {evs:?}");
    assert_eq!(v.trades(), vec![], "flat, and nothing opposite opened");
    println!("S3 reduce-only SELL 5000 of 1000 held: closed 1000, the venue is flat");

    // 4. nothing to close: refused by name.
    let evs = v.order(-1, LOT, true);
    let Some(Event::OrderRejected(r)) = evs.last() else { panic!("expected a rejection: {evs:?}") };
    assert!(r.reason.contains("no open long trade on EUR/USD"), "{}", r.reason);
    assert_eq!(v.trades(), vec![], "a refused reduce-only order opens nothing");
    println!("S4 reduce-only SELL on a flat account: {}", r.reason);

    // 5. two trades, one reduce-only order: both closed, oldest first, as ONE order.
    let a = fills(&v.order(1, LOT, false))[0].0.clone();
    let b = fills(&v.order(1, LOT, false))[0].0.clone();
    let evs = v.order(-1, 2.0 * LOT, true);
    assert_eq!(
        names(&evs),
        ["OrderSubmitted", "OrderAccepted", "Fill", "OrderPartiallyFilled", "Fill", "OrderFilled"],
        "{evs:?}"
    );
    let closing = fills(&evs);
    assert!(
        closing[0].0.starts_with(&format!("{a}-close-")),
        "the oldest trade first: {closing:?}"
    );
    assert!(closing[1].0.starts_with(&format!("{b}-close-")), "then the next: {closing:?}");
    assert_eq!(v.trades(), vec![]);
    println!(
        "S5 reduce-only SELL 2000 over trades {a} and {b}: {:?}",
        closing.iter().map(|c| &c.0).collect::<Vec<_>>()
    );

    // 6. a plain opposite order still opens a HEDGE; reduce-only orders unwind the pair one trade each.
    let long = fills(&v.order(1, LOT, false))[0].0.clone();
    let short = fills(&v.order(-1, LOT, false))[0].0.clone();
    let pair = v.trades();
    assert_eq!(
        pair.len(),
        2,
        "a plain opposite order opens a second trade on this account: {pair:?}"
    );
    println!("S6 plain BUY then plain SELL: the locked pair {pair:?}");
    let evs = v.order(-1, LOT, true);
    assert!(
        fills(&evs)[0].0.starts_with(&format!("{long}-close-")),
        "the SELL closes the LONG: {evs:?}"
    );
    assert_eq!(v.trades(), vec![(short.clone(), -1, LOT)], "the short is untouched");
    let evs = v.order(1, LOT, true);
    assert!(
        fills(&evs)[0].0.starts_with(&format!("{short}-close-")),
        "the BUY closes the SHORT: {evs:?}"
    );
    assert_eq!(v.trades(), vec![]);
    println!("S6 unwound: the long {long} and the short {short} closed one at a time");
}

#[test]
#[ignore = "live demo; PLACES REAL ORDERS on the demo account (about a dozen EUR/USD market orders); opens two sessions of the demo login"]
fn fxcm_reduce_only_market_closes_trades_by_id_on_the_hedging_demo() {
    vike_log::test_init();
    let Some(cfg) = config() else {
        eprintln!("skip: no fxcm demo account credentials");
        return;
    };
    if !vike_fxcm::sdk_available() {
        eprintln!("skip: no ForexConnect shim on this box (NOT a pass)");
        return;
    }
    let recon = recon_client(&cfg, SYMBOL)
        .expect("the reconcile login failed (re-run with --nocapture: the session thread logs FXCM's words)");
    println!("LOGIN OK (reconcile session)");
    assert_eq!(
        recon.fetch_order_status_reports(0).expect("orders").len(),
        0,
        "the demo account holds a resting {SYMBOL} order: this smoke touches only its own"
    );
    let before = recon.fetch_fill_reports(0).expect("trades");
    assert!(before.is_empty(), "the demo account holds {SYMBOL} trades already: {before:?}");
    println!("balance before: {:?}", recon.fetch_balance());

    let (events, rx) = event_channel(256);
    let client = FxcmExecutionClient::spawn(cfg, events);
    let mut venue = Venue { client, rx, recon, seq: 0 };
    let outcome = catch_unwind(AssertUnwindSafe(|| the_steps(&mut venue)));
    // Whatever happened above, leave the account flat by the path under test.
    venue.flatten();
    let left = venue.trades();
    println!("balance after: {:?}", venue.recon.fetch_balance());
    let orders = venue.recon.fetch_order_status_reports(0).expect("orders").len();
    venue.client.detach();
    if let Err(panic) = outcome {
        resume_unwind(panic);
    }
    assert_eq!((left.len(), orders), (0, 0), "the account must end flat: {left:?}");
    println!("FXCM CLOSE SMOKE DONE: flat, no resting order");
}

/// **The refusal, on a flat account, with whichever shim is installed - and NO order placed.** A reduce-only market order
/// with nothing to close must never open a trade: with the shim of this release the planner refuses it ("no open long
/// trade ..."); with an OLDER shim (no `fc_open_trades` / `fc_close_trade`) the bridge refuses it BEFORE the wire, naming
/// the missing export and the remedy. Run it from a directory whose `../lib/libfcshim.so` is the old shim to see the second
/// (the loader's first rung); the account is checked flat before and after, so a regression to the silent opening order
/// shows as a trade.
#[test]
#[ignore = "live demo; needs a FLAT account; places NO order when it passes (the order is refused before the wire)"]
fn fxcm_a_reduce_only_close_with_nothing_to_close_is_refused_by_name_and_opens_nothing() {
    vike_log::test_init();
    let Some(cfg) = config() else {
        eprintln!("skip: no fxcm demo account credentials");
        return;
    };
    if !vike_fxcm::sdk_available() {
        eprintln!("skip: no ForexConnect shim on this box (NOT a pass)");
        return;
    }
    println!("shim: {:?} abi {:?}", vike_fxcm::sdk_shim_path(), vike_fxcm::sdk_abi_version());
    let recon = recon_client(&cfg, SYMBOL).expect("the reconcile login failed");
    println!("LOGIN OK (reconcile session)");
    let held = recon.fetch_fill_reports(0).expect("trades");
    assert!(held.is_empty(), "the demo account holds {SYMBOL} trades already: {held:?}");

    let (events, rx) = event_channel(64);
    let client = FxcmExecutionClient::spawn(cfg, events);
    let mut venue = Venue { client, rx, recon, seq: 0 };
    let evs = venue.order(-1, LOT, true);
    let Some(Event::OrderRejected(r)) = evs.last() else { panic!("expected a refusal: {evs:?}") };
    assert!(
        !evs.iter().any(|e| matches!(e, Event::OrderAccepted(_))),
        "nothing may be accepted: {evs:?}"
    );
    println!("REFUSED: {}", r.reason);
    assert!(
        r.reason.contains("no open long trade on EUR/USD") || r.reason.contains("exports no `fc_"),
        "a refusal that names neither the missing trade nor the missing export: {}",
        r.reason
    );
    let after = venue.trades();
    venue.client.detach();
    assert_eq!(after, vec![], "a refused reduce-only order opened a trade");
    println!("FXCM REFUSAL SMOKE DONE: no trade opened");
}
