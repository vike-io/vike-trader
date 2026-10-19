//! The one-shot QUOTE session of the multi-currency rate (`conn::fetch_conversion_quote`, decision 0183, phase 1)
//! and the deposit currency it rides on, driven end to end against an in-process scripted venue (plaintext, no
//! TLS): handshake -> the conversion pair chosen from the account's own list -> `SubscribeSpots` -> the
//! quote -> a rate in the engine's orientation.
//!
//! ⚠ DOCS-SHAPED, not measured: the scripted venue follows Spotware's Open API as documented
//! (<https://help.ctrader.com/open-api/messages/> for the spot events and the subscription,
//! <https://help.ctrader.com/open-api/model-messages/> for `ProtoOATrader.depositAssetId` and the light
//! symbol's base and quote assets); nothing was captured from a demo account. Prices are in 1/100000 of a unit.
//!
//! This binary has its OWN small venue instead of `tests/common`'s `FakeCtrader`: that one scripts a single
//! EURUSD and no deposit asset, and every consumer of it stays its own binary.

use std::io::Write;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use prost::Message;

use vike_ctrader::conn::{
    ConnConfig, fetch_conversion_quote, fetch_conversion_quote_timed,
    fetch_conversion_quote_within, fetch_symbol_lists, fetch_symbol_lists_within,
};
use vike_ctrader::framing::{self, FrameReader};
use vike_ctrader::proto::{
    ProtoOaAccountAuthRes, ProtoOaApplicationAuthRes, ProtoOaAsset, ProtoOaAssetClass,
    ProtoOaAssetClassListRes, ProtoOaAssetListRes, ProtoOaClosePositionDetail,
    ProtoOaCtidTraderAccount, ProtoOaDeal, ProtoOaDealListRes, ProtoOaErrorRes,
    ProtoOaGetAccountListByAccessTokenRes, ProtoOaGetPositionUnrealizedPnLRes, ProtoOaInterval,
    ProtoOaLightSymbol, ProtoOaPosition, ProtoOaPositionUnrealizedPnL, ProtoOaReconcileRes,
    ProtoOaSpotEvent, ProtoOaSubscribeSpotsReq, ProtoOaSubscribeSpotsRes, ProtoOaSymbol,
    ProtoOaSymbolByIdRes, ProtoOaSymbolCategory, ProtoOaSymbolCategoryListRes,
    ProtoOaSymbolsListRes, ProtoOaTradeData, ProtoOaTrader, ProtoOaTraderRes, ProtoOaTradingMode,
    pt,
};
use vike_ctrader::quote_rate::{MAX_SPOT_AGE_MS, MIN_SPOT_MS_EPOCH, RateError};
use vike_ctrader::symbols::SymbolMap;
use vike_model::now_ms;

const HOUR_MS: i64 = 3_600_000;

/// The asset ids of the scripted broker.
const EUR: i64 = 5;
const JPY: i64 = 7;
const GBP: i64 = 8;
const CHF: i64 = 9;
const USD: i64 = 16;

/// What the scripted venue does after a `SubscribeSpots`.
#[derive(Clone)]
enum Spot {
    /// An unrelated symbol's event first, then these `(bid, ask, timestamp)` events for the subscribed symbol.
    Events(Vec<(Option<u64>, Option<u64>, Option<i64>)>),
    /// The response and nothing else, for ever.
    Silent,
    /// An `ERROR_RES` instead of the response.
    Refuse,
}

#[derive(Clone)]
struct Venue {
    /// `ProtoOATrader.depositAssetId`.
    deposit_asset: i64,
    spot: Spot,
    /// What a `ProtoOADealListReq` is answered with, and whether the reply says `hasMore`.
    deals: Vec<ProtoOaDeal>,
    deals_more: bool,
    /// What `SymbolById` says about USDJPY beyond its digits: `(schedule_time_zone, schedule, trading_mode)`.
    usdjpy: (Option<String>, Vec<(u32, u32)>, Option<i32>),
    /// `ProtoOATrader.moneyDigits` as the trader record states it (`None`: the field is omitted).
    money_digits: Option<u32>,
    /// The open positions a reconcile lists: `(position id, symbol id, trade side, volume in centi-units)`.
    open: Vec<(i64, i64, i32, i64)>,
    /// The unrealized PnL rows: `(position id, gross, net)`, scaled by 2 digits.
    unrealized: Vec<(i64, i64, i64)>,
}

impl Venue {
    fn new(deposit_asset: i64, spot: Spot) -> Self {
        Venue {
            deposit_asset,
            spot,
            deals: Vec::new(),
            deals_more: false,
            usdjpy: (None, Vec::new(), None),
            money_digits: Some(2),
            open: Vec::new(),
            unrealized: Vec::new(),
        }
    }

    /// This venue with USDJPY's schedule (UTC, these `(start, end)` seconds since Sunday 00:00).
    fn with_utc_schedule(self, schedule: &[(u32, u32)]) -> Self {
        self.with_schedule("UTC", schedule)
    }

    /// This venue with USDJPY's schedule in time zone `zone` (these `(start, end)` seconds since Sunday 00:00).
    fn with_schedule(mut self, zone: &str, schedule: &[(u32, u32)]) -> Self {
        self.usdjpy.0 = Some(zone.to_string());
        self.usdjpy.1 = schedule.to_vec();
        self
    }
}

/// The seconds of a week that say OPEN at any instant: the whole week.
const ALWAYS_OPEN: [(u32, u32); 1] = [(0, 604_800)];
/// A single second at Sunday 00:00 UTC: CLOSED at any other instant.
const ALWAYS_CLOSED: [(u32, u32); 1] = [(0, 1)];

/// A running scripted venue: the address, every `SubscribeSpotsReq` it received and the payload type of every
/// frame it received, in order.
struct Served {
    addr: SocketAddr,
    subscriptions: Arc<Mutex<Vec<ProtoOaSubscribeSpotsReq>>>,
    requests: Arc<Mutex<Vec<u32>>>,
}

fn serve(venue: Venue) -> Served {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let subscriptions = Arc::new(Mutex::new(Vec::new()));
    let kept = Arc::clone(&subscriptions);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&requests);
    thread::Builder::new()
        .name("quote-rate-venue".into())
        .spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                serve_one(stream, &venue, &kept, &seen);
            }
        })
        .expect("spawn");
    Served { addr, subscriptions, requests }
}

fn send(writer: &mut TcpStream, payload_type: u32, body: &impl Message) -> bool {
    let frame = framing::encode(payload_type, &body.encode_to_vec(), "srv");
    writer.write_all(&frame).and_then(|()| writer.flush()).is_ok()
}

fn serve_one(
    stream: TcpStream,
    venue: &Venue,
    subscriptions: &Mutex<Vec<ProtoOaSubscribeSpotsReq>>,
    requests: &Mutex<Vec<u32>>,
) {
    stream.set_read_timeout(Some(Duration::from_millis(200))).ok();
    let mut writer = stream.try_clone().expect("clone");
    let mut reader = FrameReader::new(stream);
    let light = |symbol_id, name: &str, base, quote| ProtoOaLightSymbol {
        symbol_id,
        symbol_name: Some(name.to_string()),
        symbol_category_id: Some(1),
        base_asset_id: Some(base),
        quote_asset_id: Some(quote),
        ..Default::default()
    };
    let symbols = [
        light(1, "EURUSD", EUR, USD),
        light(2, "USDJPY", USD, JPY),
        light(3, "GBPUSD", GBP, USD),
        light(4, "USDCHF", USD, CHF),
    ];
    let mut idle = 0;
    while idle < 100 {
        let Ok(frame) = reader.next_frame() else { break };
        let Some(msg) = frame else {
            idle += 1;
            continue;
        };
        idle = 0;
        requests.lock().expect("requests").push(msg.payload_type);
        let payload = msg.payload.as_deref().unwrap_or(&[]);
        let ok = match msg.payload_type {
            pt::APPLICATION_AUTH_REQ => {
                send(&mut writer, pt::APPLICATION_AUTH_RES, &ProtoOaApplicationAuthRes::default())
            }
            pt::GET_ACCOUNTS_BY_ACCESS_TOKEN_REQ => send(
                &mut writer,
                pt::GET_ACCOUNTS_BY_ACCESS_TOKEN_RES,
                &ProtoOaGetAccountListByAccessTokenRes {
                    access_token: "token".to_string(),
                    ctid_trader_account: vec![ProtoOaCtidTraderAccount {
                        ctid_trader_account_id: 99,
                        is_live: Some(false),
                        trader_login: Some(1234),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            ),
            pt::ACCOUNT_AUTH_REQ => send(
                &mut writer,
                pt::ACCOUNT_AUTH_RES,
                &ProtoOaAccountAuthRes { ctid_trader_account_id: 99, ..Default::default() },
            ),
            pt::TRADER_REQ => send(
                &mut writer,
                pt::TRADER_RES,
                &ProtoOaTraderRes {
                    ctid_trader_account_id: 99,
                    trader: ProtoOaTrader {
                        ctid_trader_account_id: 99,
                        money_digits: venue.money_digits,
                        deposit_asset_id: venue.deposit_asset,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            ),
            pt::SYMBOL_CATEGORY_REQ => send(
                &mut writer,
                pt::SYMBOL_CATEGORY_RES,
                &ProtoOaSymbolCategoryListRes {
                    ctid_trader_account_id: 99,
                    symbol_category: vec![ProtoOaSymbolCategory {
                        id: 1,
                        asset_class_id: 1,
                        name: "Default Category".to_string(),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            ),
            pt::ASSET_CLASS_LIST_REQ => send(
                &mut writer,
                pt::ASSET_CLASS_LIST_RES,
                &ProtoOaAssetClassListRes {
                    ctid_trader_account_id: 99,
                    asset_class: vec![ProtoOaAssetClass {
                        id: Some(1),
                        name: Some("Forex".to_string()),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            ),
            pt::ASSET_LIST_REQ => {
                let asset = |asset_id, name: &str| ProtoOaAsset {
                    asset_id,
                    name: name.to_string(),
                    ..Default::default()
                };
                send(
                    &mut writer,
                    pt::ASSET_LIST_RES,
                    &ProtoOaAssetListRes {
                        ctid_trader_account_id: 99,
                        asset: vec![
                            asset(EUR, "EUR"),
                            asset(JPY, "JPY"),
                            asset(GBP, "GBP"),
                            asset(CHF, "CHF"),
                            asset(USD, "USD"),
                        ],
                        ..Default::default()
                    },
                )
            }
            pt::SYMBOLS_LIST_REQ => send(
                &mut writer,
                pt::SYMBOLS_LIST_RES,
                &ProtoOaSymbolsListRes {
                    ctid_trader_account_id: 99,
                    symbol: symbols.to_vec(),
                    ..Default::default()
                },
            ),
            pt::SYMBOL_BY_ID_REQ => send(
                &mut writer,
                pt::SYMBOL_BY_ID_RES,
                &ProtoOaSymbolByIdRes {
                    ctid_trader_account_id: 99,
                    symbol: symbols
                        .iter()
                        .map(|s| {
                            let mut full = ProtoOaSymbol {
                                symbol_id: s.symbol_id,
                                digits: if s.symbol_id == 2 { 3 } else { 5 },
                                pip_position: 4,
                                ..Default::default()
                            };
                            if s.symbol_id == 2 {
                                let (zone, schedule, mode) = venue.usdjpy.clone();
                                full.schedule_time_zone = zone;
                                full.trading_mode = mode;
                                full.schedule = schedule
                                    .iter()
                                    .map(|(start_second, end_second)| ProtoOaInterval {
                                        start_second: *start_second,
                                        end_second: *end_second,
                                    })
                                    .collect();
                            }
                            full
                        })
                        .collect(),
                    ..Default::default()
                },
            ),
            pt::SUBSCRIBE_SPOTS_REQ => {
                let req = ProtoOaSubscribeSpotsReq::decode(payload).expect("a subscribe request");
                let subscribed = req.symbol_id.first().copied().unwrap_or_default();
                subscriptions.lock().expect("subs").push(req);
                match &venue.spot {
                    Spot::Refuse => send(
                        &mut writer,
                        pt::ERROR_RES,
                        &ProtoOaErrorRes {
                            error_code: "SYMBOL_NOT_FOUND".to_string(),
                            description: Some("the scripted venue refuses".to_string()),
                            ..Default::default()
                        },
                    ),
                    Spot::Silent => send(
                        &mut writer,
                        pt::SUBSCRIBE_SPOTS_RES,
                        &ProtoOaSubscribeSpotsRes {
                            ctid_trader_account_id: 99,
                            ..Default::default()
                        },
                    ),
                    Spot::Events(events) => {
                        let spot = |symbol_id, bid, ask, timestamp| ProtoOaSpotEvent {
                            ctid_trader_account_id: 99,
                            symbol_id,
                            bid,
                            ask,
                            timestamp,
                            ..Default::default()
                        };
                        let mut ok = send(
                            &mut writer,
                            pt::SUBSCRIBE_SPOTS_RES,
                            &ProtoOaSubscribeSpotsRes {
                                ctid_trader_account_id: 99,
                                ..Default::default()
                            },
                        );
                        // An event of ANOTHER symbol (USDCHF, or EURUSD when that is the one asked) that
                        // must change nothing.
                        let other = if subscribed == 4 { 1 } else { 4 };
                        ok &= send(
                            &mut writer,
                            pt::SPOT_EVENT,
                            &spot(other, Some(113_911), Some(113_912), Some(now_ms())),
                        );
                        for (bid, ask, ts) in events {
                            ok &= send(
                                &mut writer,
                                pt::SPOT_EVENT,
                                &spot(subscribed, *bid, *ask, *ts),
                            );
                        }
                        ok
                    }
                }
            }
            pt::RECONCILE_REQ => send(
                &mut writer,
                pt::RECONCILE_RES,
                &ProtoOaReconcileRes {
                    ctid_trader_account_id: 99,
                    position: venue
                        .open
                        .iter()
                        .map(|(position_id, symbol_id, trade_side, volume)| ProtoOaPosition {
                            position_id: *position_id,
                            trade_data: ProtoOaTradeData {
                                symbol_id: *symbol_id,
                                volume: *volume,
                                trade_side: *trade_side,
                                ..Default::default()
                            },
                            ..Default::default()
                        })
                        .collect(),
                    ..Default::default()
                },
            ),
            pt::GET_POSITION_UNREALIZED_PNL_REQ => send(
                &mut writer,
                pt::GET_POSITION_UNREALIZED_PNL_RES,
                &ProtoOaGetPositionUnrealizedPnLRes {
                    ctid_trader_account_id: 99,
                    position_unrealized_pn_l: venue
                        .unrealized
                        .iter()
                        .map(|(position_id, gross, net)| ProtoOaPositionUnrealizedPnL {
                            position_id: *position_id,
                            gross_unrealized_pn_l: *gross,
                            net_unrealized_pn_l: *net,
                        })
                        .collect(),
                    money_digits: 2,
                    ..Default::default()
                },
            ),
            pt::DEAL_LIST_REQ => send(
                &mut writer,
                pt::DEAL_LIST_RES,
                &ProtoOaDealListRes {
                    ctid_trader_account_id: 99,
                    deal: venue.deals.clone(),
                    has_more: venue.deals_more,
                    ..Default::default()
                },
            ),
            _ => true, // heartbeats and anything else: no reply
        };
        if !ok {
            break;
        }
    }
}

fn cfg(served: &Served) -> ConnConfig {
    ConnConfig::for_test(served.addr, "cid", "secret", "token")
}

fn usd_account(spot: Spot) -> Venue {
    Venue::new(USD, spot)
}

/// **A yen quote on a USD account**: the rate comes from USDJPY (the account is the BASE of the pair), the
/// subscription names THAT symbol and asks for the timestamp, the event of another symbol and the split
/// first-event (a bid alone, then the ask) change nothing, and the result is a FRACTION of a dollar per yen
/// (`1/150.10`, never `150.10`): 100,000 units closed 0.10 above entry realize 66.62 dollars, not 10,000.
#[test]
fn a_yen_quote_on_a_usd_account_is_read_from_usdjpy_and_is_a_fraction_of_a_dollar() {
    let t0 = now_ms();
    let venue = usd_account(Spot::Events(vec![
        (Some(15_009_000), None, Some(t0)),
        (None, Some(15_011_000), Some(t0)),
    ]));
    let served = serve(venue);
    let rate = fetch_conversion_quote(cfg(&served), "jpy").expect("a priced yen");
    assert_eq!(rate.quote_ccy, "JPY");
    assert_eq!(rate.account_ccy, "USD", "the deposit currency THIS session read rides on the rate");
    let r = rate.account_per_quote.get();
    assert!(r < 0.01 && r > 0.006, "{r} is yen per dollar, the flipped rate");
    assert!((r - 1.0 / 150.10).abs() < 1e-12, "{r}");
    assert!((rate.account_per_quote.to_account(100_000.0 * 0.10) - 66.62).abs() < 0.01);
    assert!(rate.as_of_ms >= t0 && rate.as_of_ms <= now_ms(), "stamped at the ask");

    let subs = served.subscriptions.lock().expect("subs");
    assert_eq!(subs.len(), 1, "one subscription");
    assert_eq!(subs[0].symbol_id, vec![2], "USDJPY, by its assets and not its name");
    assert_eq!(subs[0].ctid_trader_account_id, 99);
    assert_eq!(subs[0].subscribe_to_spot_timestamp, Some(true));
}

/// **A pound quote**: the rate comes from GBPUSD (the quote currency is the BASE), and a pound is MORE than a
/// dollar: EURGBP at 0.85, 100,000 units, is 85,000 pounds = 114,750 dollars.
#[test]
fn a_pound_quote_on_a_usd_account_is_read_from_gbpusd_and_is_more_than_one_dollar() {
    let served =
        serve(usd_account(Spot::Events(vec![(Some(134_999), Some(135_001), Some(now_ms()))])));
    let rate = fetch_conversion_quote(cfg(&served), "GBP").expect("a priced pound");
    assert_eq!(rate.quote_ccy, "GBP");
    assert!(rate.account_per_quote.get() > 1.0);
    assert!((rate.account_per_quote.to_account(85_000.0) - 114_750.0).abs() < 1e-6);
    assert_eq!(served.subscriptions.lock().expect("subs")[0].symbol_id, vec![3], "GBPUSD");
}

/// The account's own currency needs no pair and no quote: par, with no subscription sent.
#[test]
fn the_deposit_currency_is_par_and_subscribes_to_nothing() {
    let served = serve(usd_account(Spot::Silent));
    let rate = fetch_conversion_quote(cfg(&served), "USD").expect("par");
    assert_eq!((rate.quote_ccy.as_str(), rate.account_per_quote.get()), ("USD", 1.0));
    assert_eq!(rate.account_ccy, "USD");
    assert!(served.subscriptions.lock().expect("subs").is_empty());
}

/// The deposit currency decides the pair: on a EUR account a USD quote converts through EURUSD, with the
/// EURUSD price read as dollars per euro, so a dollar is `1 / mid` euros (the account is the BASE).
#[test]
fn the_pair_follows_the_accounts_deposit_currency() {
    let served =
        serve(Venue::new(EUR, Spot::Events(vec![(Some(108_000), Some(108_002), Some(now_ms()))])));
    let rate = fetch_conversion_quote(cfg(&served), "USD").expect("dollars on a euro account");
    assert!((rate.account_per_quote.get() - 1.0 / 1.08001).abs() < 1e-12);
    assert_eq!(rate.account_ccy, "EUR", "the same quote on another account names that account");
    assert_eq!(served.subscriptions.lock().expect("subs")[0].symbol_id, vec![1], "EURUSD");
}

/// No pair in the account's list for the currency: refused in words, and nothing is subscribed.
#[test]
fn a_currency_with_no_pair_is_refused_and_subscribes_to_nothing() {
    let served = serve(usd_account(Spot::Silent));
    let why = fetch_conversion_quote(cfg(&served), "NZD").expect_err("no NZD pair");
    assert!(matches!(&why, RateError::NoPair(_)), "{why}");
    assert!(served.subscriptions.lock().expect("subs").is_empty());
}

/// A venue that never completes the quote, one that refuses the subscription, a crossed quote, a zero side and
/// a price the venue itself stamped days ago are each a refusal in words; none is a rate of 1.
#[test]
fn a_quote_that_is_missing_refused_crossed_zero_or_old_is_no_rate() {
    let now = now_ms();
    // only one side ever arrives, then silence. The ask's ceiling is generous (a loaded machine slows the
    // handshake, and that must not turn the asserted refusal into a timeout); only the wait for the quote
    // is short.
    let served = serve(usd_account(Spot::Events(vec![(Some(15_009_000), None, Some(now))])));
    let why = quote_timed(&served, "one side only");
    assert!(matches!(&why, RateError::Unavailable(w) if w.contains("no two-sided quote")), "{why}");

    let served = serve(usd_account(Spot::Silent));
    let why = quote_timed(&served, "silence");
    assert!(matches!(&why, RateError::Unavailable(w) if w.contains("USDJPY")), "{why}");

    let served = serve(usd_account(Spot::Refuse));
    let why = fetch_conversion_quote(cfg(&served), "JPY").expect_err("refused");
    assert!(
        matches!(&why, RateError::Unavailable(w) if w.contains("SYMBOL_NOT_FOUND")),
        "the venue's own code is named: {why}"
    );

    for (bid, ask, what) in [
        (15_011_000, 15_009_000, "crossed"),
        (0, 15_011_000, "a zero bid"),
        (15_009_000, 0, "a zero ask"),
    ] {
        let served = serve(usd_account(Spot::Events(vec![(Some(bid), Some(ask), Some(now))])));
        let why = fetch_conversion_quote(cfg(&served), "JPY").expect_err(what);
        assert!(matches!(&why, RateError::BadQuote(_)), "{what}: {why}");
    }

    // a weekend-old price is a rate when the pair's own schedule says the market is CLOSED; one stamped past
    // the bound is not, whatever the schedule says
    let weekend = now - 48 * HOUR_MS;
    let served = serve(
        usd_account(Spot::Events(vec![(Some(15_009_000), Some(15_011_000), Some(weekend))]))
            .with_utc_schedule(&ALWAYS_CLOSED),
    );
    fetch_conversion_quote(cfg(&served), "JPY").expect("a closed market's last price");
    let frozen = now - MAX_SPOT_AGE_MS - HOUR_MS;
    let served = serve(
        usd_account(Spot::Events(vec![(Some(15_009_000), Some(15_011_000), Some(frozen))]))
            .with_utc_schedule(&ALWAYS_CLOSED),
    );
    let why = fetch_conversion_quote(cfg(&served), "JPY").expect_err("a frozen price");
    assert!(matches!(&why, RateError::BadQuote(w) if w.contains("h old")), "{why}");
}

/// `fetch_conversion_quote_timed` for a JPY quote with a generous 30 s ceiling for the whole ask and a 1.5 s
/// wait for the quote: the refusal asserted is the venue's, never a loaded machine's timeout.
fn quote_timed(served: &Served, what: &str) -> RateError {
    fetch_conversion_quote_timed(
        cfg(served),
        "JPY",
        Duration::from_secs(30),
        Duration::from_millis(1500),
    )
    .expect_err(what)
}

/// **A quote with no timestamp is no rate**: the request asks for one, but the page says the field is optional,
/// and a price of unknown age (a suspended pair's weeks-old last price) must not be stamped fresh at the ask.
/// Refused in words when no event carries a stamp within the wait; a LATER event that does carry one completes
/// the quote (the first snapshot may omit it).
#[test]
fn an_untimed_quote_is_refused_and_a_later_stamp_completes_it() {
    let served = serve(usd_account(Spot::Events(vec![(Some(15_009_000), Some(15_011_000), None)])));
    let why = quote_timed(&served, "two sides and no stamp");
    assert!(matches!(&why, RateError::BadQuote(w) if w.contains("no timestamp")), "{why}");

    let served = serve(usd_account(Spot::Events(vec![
        (Some(15_009_000), Some(15_011_000), None),
        (None, None, Some(now_ms())),
    ])));
    let rate = fetch_conversion_quote(cfg(&served), "JPY").expect("the next event is stamped");
    assert!((rate.account_per_quote.get() - 1.0 / 150.10).abs() < 1e-12);
}

/// **The unit of the stamp is ASSUMED milliseconds**: a quote stamped in SECONDS (a current one is about 1.7e9)
/// is refused loudly, never read as an age and never as a fresh quote.
#[test]
fn a_seconds_valued_stamp_is_refused_loudly() {
    let seconds = now_ms() / 1_000;
    assert!(seconds < MIN_SPOT_MS_EPOCH);
    let served =
        serve(usd_account(Spot::Events(vec![(Some(15_009_000), Some(15_011_000), Some(seconds))])));
    let why = fetch_conversion_quote(cfg(&served), "JPY").expect_err("seconds");
    assert!(
        matches!(&why, RateError::BadQuote(w) if w.contains("not a millisecond epoch")),
        "{why}"
    );
}

/// **An open market allows the engine's window, not 72 hours**: the pair's own schedule (UTC) says open, and a
/// price stamped ten minutes ago is refused; the same price is a rate when the schedule says CLOSED. A schedule
/// this crate cannot read, or none, is held to the strict window too (the weekend bound would pass a frozen
/// feed on an open market, with the rate stamped fresh at the ask).
#[test]
fn a_stale_price_is_refused_while_the_pairs_own_schedule_says_open() {
    let ten_minutes_ago = now_ms() - 10 * 60_000;
    let events = || Spot::Events(vec![(Some(15_009_000), Some(15_011_000), Some(ten_minutes_ago))]);

    let served = serve(usd_account(events()).with_utc_schedule(&ALWAYS_OPEN));
    let why = fetch_conversion_quote(cfg(&served), "JPY").expect_err("open and stale");
    assert!(
        matches!(&why, RateError::BadQuote(w) if w.contains(" s old (more than 120 s with the market Open)")),
        "{why}"
    );

    // Fresh enough for an open market: a rate.
    let served = serve(
        usd_account(Spot::Events(vec![(
            Some(15_009_000),
            Some(15_011_000),
            Some(now_ms() - 30_000),
        )]))
        .with_utc_schedule(&ALWAYS_OPEN),
    );
    fetch_conversion_quote(cfg(&served), "JPY").expect("a 30 s old price in an open market");

    let served = serve(usd_account(events()).with_utc_schedule(&ALWAYS_CLOSED));
    fetch_conversion_quote(cfg(&served), "JPY").expect("closed: the last price stands");
    let served = serve(usd_account(events()));
    let why =
        fetch_conversion_quote(cfg(&served), "JPY").expect_err("no schedule: the strict window");
    assert!(
        matches!(&why, RateError::BadQuote(w) if w.contains("market schedule is not stated")),
        "{why}"
    );
}

/// **A schedule zone this crate cannot read does not relax the bound to 72 hours**: USDJPY frozen 20 h on a
/// broker whose schedule zone is `Europe/London` is refused in words naming the zone (where the 72 h bound let
/// its rate through, stamped fresh at the ask: a USD account's yen positions valued at yesterday's 1/150.10
/// while the market is at 1/147), and a quote inside the strict window is still a rate. A UTC schedule is
/// unchanged: open refuses at the window, closed allows the weekend.
#[test]
fn an_unreadable_schedule_zone_is_held_to_the_strict_window_not_72_hours() {
    let twenty_hours_ago = now_ms() - 20 * HOUR_MS;
    let frozen =
        || Spot::Events(vec![(Some(15_009_000), Some(15_011_000), Some(twenty_hours_ago))]);

    for schedule in [&ALWAYS_OPEN, &ALWAYS_CLOSED] {
        let served = serve(usd_account(frozen()).with_schedule("Europe/London", schedule));
        let why = fetch_conversion_quote(cfg(&served), "JPY").expect_err("a frozen price");
        assert!(
            matches!(&why, RateError::BadQuote(w) if w.contains("20 h old")
            && w.contains(
                "the market schedule zone Europe/London is not understood; refusing a quote \
                 older than 120 s"
            )),
            "{why}"
        );
    }
    let served = serve(
        usd_account(Spot::Events(vec![(
            Some(15_009_000),
            Some(15_011_000),
            Some(now_ms() - 30_000),
        )]))
        .with_schedule("Europe/London", &ALWAYS_OPEN),
    );
    fetch_conversion_quote(cfg(&served), "JPY")
        .expect("a 30 s old price is inside the strict window");

    // The UTC behaviour is what it was: closed allows 20 h, open refuses it.
    let served = serve(usd_account(frozen()).with_utc_schedule(&ALWAYS_CLOSED));
    fetch_conversion_quote(cfg(&served), "JPY").expect("closed UTC market: the last price stands");
    let served = serve(usd_account(frozen()).with_utc_schedule(&ALWAYS_OPEN));
    assert!(fetch_conversion_quote(cfg(&served), "JPY").is_err(), "open UTC market");
}

/// **The unit guard is two-sided**: a quote stamped in MICROseconds or NANOseconds (a number 10^3 or 10^6 times
/// a millisecond epoch) would give a negative age and pass as fresh; it is refused in words, on an open, a
/// closed and a schedule-less market alike, and no rate comes of it.
#[test]
fn a_microsecond_or_nanosecond_stamp_is_refused_not_taken_as_fresh() {
    let now = now_ms();
    for (stamp, unit) in [(now * 1_000, "microseconds"), (now * 1_000_000, "nanoseconds")] {
        for schedule in [Some(&ALWAYS_OPEN), Some(&ALWAYS_CLOSED), None] {
            let mut venue =
                usd_account(Spot::Events(vec![(Some(15_009_000), Some(15_011_000), Some(stamp))]));
            if let Some(schedule) = schedule {
                venue = venue.with_utc_schedule(schedule);
            }
            let served = serve(venue);
            let why = fetch_conversion_quote(cfg(&served), "JPY").expect_err(unit);
            assert!(
                matches!(&why, RateError::BadQuote(w)
                    if w.contains("after the ask") && w.contains("assumed to be milliseconds")),
                "{unit}: {why}"
            );
        }
    }
}

/// A pair the broker has SUSPENDED (`tradingMode` disabled) stays listed with its last price: refused by name,
/// before anything is subscribed.
#[test]
fn a_suspended_pair_is_refused_before_it_is_subscribed() {
    let mut venue =
        usd_account(Spot::Events(vec![(Some(15_009_000), Some(15_011_000), Some(now_ms()))]));
    venue.usdjpy.2 = Some(ProtoOaTradingMode::DisabledWithoutPendingsExecution as i32);
    let served = serve(venue);
    let why = fetch_conversion_quote(cfg(&served), "JPY").expect_err("suspended");
    assert!(
        matches!(&why, RateError::BadQuote(w) if w.contains("USDJPY is not enabled for trading")),
        "{why}"
    );
    assert!(served.subscriptions.lock().expect("subs").is_empty());
}

/// A venue that accepts the connection and never answers: held open, silent. Counts what it accepted.
struct Mute {
    addr: SocketAddr,
    accepted: Arc<std::sync::atomic::AtomicUsize>,
}

fn serve_mute() -> Mute {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = Arc::clone(&accepted);
    thread::Builder::new()
        .name("quote-rate-mute".into())
        .spawn(move || {
            let mut held = Vec::new();
            while let Ok((stream, _)) = listener.accept() {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                held.push(stream); // never read, never written, never closed
            }
        })
        .expect("spawn");
    Mute { addr, accepted }
}

/// **One deadline for the whole ask**: a venue that takes the connection and never answers holds the asking
/// thread for the deadline (and the one read in flight when it passes), not for ten handshake steps of 15 s;
/// the thread is RELEASED (it joins), the ask dials ONCE (no retry of its own), and the failure is worded as the
/// session's.
#[test]
fn a_mute_venue_costs_the_ask_its_deadline_and_releases_the_thread() {
    let mute = serve_mute();
    let config = ConnConfig::for_test(mute.addr, "cid", "secret", "token");
    let started = std::time::Instant::now();
    let asking = thread::spawn(move || {
        fetch_conversion_quote_within(config, "JPY", Duration::from_millis(1500))
    });
    let why = asking.join().expect("the asking thread ends").expect_err("no answer ever");
    let took = started.elapsed();
    assert!(took >= Duration::from_millis(1400), "the deadline was waited for: {took:?}");
    assert!(took < Duration::from_secs(4), "one deadline, not a step's 15 s: {took:?}");
    assert!(
        matches!(&why, RateError::Unavailable(w) if w.contains("timed out")),
        "worded as the session's failure: {why}"
    );
    assert_eq!(mute.accepted.load(std::sync::atomic::Ordering::SeqCst), 1, "one dial, no retry");

    // The account-currency ask (it runs inside an adapter start, adapter lock held) is bounded the same way.
    let config = ConnConfig::for_test(mute.addr, "cid", "secret", "token");
    let started = std::time::Instant::now();
    let asking = thread::spawn(move || {
        vike_ctrader::conn::fetch_account_currency_within(config, Duration::from_millis(1500))
    });
    let why = asking.join().expect("the asking thread ends").expect_err("no answer ever");
    let took = started.elapsed();
    assert!(took >= Duration::from_millis(1400) && took < Duration::from_secs(4), "{took:?}");
    assert!(why.to_string().contains("timed out"), "{why}");
}

/// The two deadlines are the numbers the gatewayhub's lookup threads are sized by: a whole rate ask is 12 s, and
/// the account-currency ask, which runs under an adapter lock, is five or less.
#[test]
fn the_deadlines_are_the_pinned_numbers() {
    assert_eq!(vike_ctrader::conn::RATE_ASK_DEADLINE, Duration::from_secs(12));
    assert!(vike_ctrader::conn::ACCOUNT_CURRENCY_DEADLINE <= Duration::from_secs(5));
}

/// A deadline that has already passed connects to nothing: the ask fails at once and the venue sees no
/// connection.
#[test]
fn a_deadline_already_passed_dials_nothing() {
    let mute = serve_mute();
    let config = ConnConfig::for_test(mute.addr, "cid", "secret", "token");
    let why = fetch_conversion_quote_within(config, "JPY", Duration::ZERO).expect_err("no time");
    assert!(matches!(&why, RateError::Unavailable(w) if w.contains("deadline")), "{why}");
    thread::sleep(Duration::from_millis(100));
    assert_eq!(mute.accepted.load(std::sync::atomic::Ordering::SeqCst), 0, "nothing was dialled");
}

/// A broker whose asset list does not name the account's deposit asset has NO currency to convert into: the
/// session refuses before it picks a pair (an unknown currency is never "any currency will do").
#[test]
fn an_unnamed_deposit_asset_is_refused_before_any_pair_is_chosen() {
    let served = serve(Venue::new(4242, Spot::Silent));
    let why = fetch_conversion_quote(cfg(&served), "JPY").expect_err("no deposit currency");
    assert!(matches!(&why, RateError::Unavailable(w) if w.contains("deposit currency")), "{why}");
    assert!(served.subscriptions.lock().expect("subs").is_empty());
}

/// The deposit currency crosses the wire: `ProtoOATrader.depositAssetId` named through the asset list, and
/// `None` (never a default) when the list does not name it.
#[test]
fn the_deposit_currency_is_read_from_the_trader_record_over_the_wire() {
    let served = serve(usd_account(Spot::Silent));
    let lists = fetch_symbol_lists(cfg(&served)).expect("a handshake");
    assert_eq!(SymbolMap::from_lists(&lists).deposit_currency(), Some("USD"));

    let served = serve(Venue::new(EUR, Spot::Silent));
    let lists = fetch_symbol_lists(cfg(&served)).expect("a handshake");
    assert_eq!(SymbolMap::from_lists(&lists).deposit_currency(), Some("EUR"));

    let served = serve(Venue::new(4242, Spot::Silent));
    let lists = fetch_symbol_lists(cfg(&served)).expect("a handshake");
    assert_eq!(SymbolMap::from_lists(&lists).deposit_currency(), None);
}

/// **The account-currency ask stops after the asset list**: the deposit currency is known there, so the
/// categories, the asset classes, the symbol list and the full symbols (the slowest answers of a broker with a
/// large list) are never asked, and the answer is the same currency the full handshake names.
#[test]
fn the_account_currency_ask_stops_the_handshake_after_the_asset_list() {
    use vike_ctrader::conn::fetch_account_currency_within;
    for (deposit, expected) in [(USD, Some("USD")), (EUR, Some("EUR")), (4242, None)] {
        let served = serve(Venue::new(deposit, Spot::Silent));
        let got = fetch_account_currency_within(cfg(&served), Duration::from_secs(30))
            .expect("a handshake");
        assert_eq!(got.as_deref(), expected, "deposit asset {deposit}");
        // The venue records a frame before it answers, so by the time the ask returned all are in.
        let seen = served.requests.lock().expect("requests").clone();
        assert!(seen.contains(&pt::ASSET_LIST_REQ), "{seen:?}");
        for never in [
            pt::SYMBOLS_LIST_REQ,
            pt::SYMBOL_BY_ID_REQ,
            pt::SYMBOL_CATEGORY_REQ,
            pt::ASSET_CLASS_LIST_REQ,
            pt::SUBSCRIBE_SPOTS_REQ,
        ] {
            assert!(!seen.contains(&never), "payload type {never} was asked: {seen:?}");
        }
    }
}

/// **The account listing's bound**: `fetch_symbol_lists_within` (what `CtraderCatalog::account_listing` runs
/// under) returns the same lists as the unbounded fetch from a healthy venue, and a venue that never answers
/// costs it its deadline and releases the thread.
#[test]
fn a_bounded_listing_matches_the_unbounded_one_and_a_mute_venue_costs_its_deadline() {
    let served = serve(usd_account(Spot::Silent));
    let lists =
        fetch_symbol_lists_within(cfg(&served), Duration::from_secs(30)).expect("a handshake");
    assert_eq!(SymbolMap::from_lists(&lists).deposit_currency(), Some("USD"));
    assert_eq!(lists.light.len(), 4);

    let mute = serve_mute();
    let config = ConnConfig::for_test(mute.addr, "cid", "secret", "token");
    let started = std::time::Instant::now();
    let asking =
        thread::spawn(move || fetch_symbol_lists_within(config, Duration::from_millis(1500)));
    let why = asking.join().expect("the asking thread ends").expect_err("no answer ever");
    let took = started.elapsed();
    assert!(took >= Duration::from_millis(1400) && took < Duration::from_secs(4), "{took:?}");
    assert!(why.to_string().contains("timed out"), "{why}");
}

/// **An absent `moneyDigits` is a refusal where money is scaled, and costs nothing where it is not**: the trader
/// record omits the field (the vendor documents no default, and a true 2 read as 0 is a 100x error on every
/// commission, conversion fee and balance). The session that scales deals (the reconcile client's handshake)
/// is refused; the asks that scale nothing (the listing, the deposit currency, a rate) still answer.
#[test]
fn an_absent_money_digits_refuses_the_session_that_scales_money_only() {
    let omitted = || {
        let mut venue =
            usd_account(Spot::Events(vec![(Some(15_009_000), Some(15_011_000), Some(now_ms()))]));
        venue.money_digits = None;
        venue
    };
    // the sanity half: the same venue WITH the field builds the client
    let served = serve(usd_account(Spot::Silent));
    assert!(vike_ctrader::CtraderReconClient::connect(&cfg(&served), "EURUSD").is_some());

    let served = serve(omitted());
    assert!(
        vike_ctrader::CtraderReconClient::connect(&cfg(&served), "EURUSD").is_none(),
        "no scale, no session that scales money"
    );
    let served = serve(omitted());
    fetch_symbol_lists(cfg(&served)).expect("the listing scales nothing");
    let served = serve(omitted());
    let got = vike_ctrader::conn::fetch_account_currency(cfg(&served)).expect("a handshake");
    assert_eq!(got.as_deref(), Some("USD"));
    let served = serve(omitted());
    fetch_conversion_quote(cfg(&served), "JPY").expect("a rate scales no money integer");
}

/// A USDJPY SELL deal of 1,000 units (100,000 centi-units) at 150.12, closing a position opened at 150.02:
/// 100 yen = 0.67 dollars gross on an account whose `moneyDigits` is 2 (`67` on the wire), the booked
/// balance after it, and the venue's raw `quoteToDepositConversionRate`. DOCS-SHAPED values.
fn closing_deal(deal_id: i64, ts: i64) -> ProtoOaDeal {
    ProtoOaDeal {
        deal_id,
        order_id: 1,
        position_id: 1,
        volume: 100_000,
        filled_volume: 100_000,
        symbol_id: 2,
        create_timestamp: ts,
        execution_timestamp: ts,
        execution_price: Some(150.12),
        trade_side: 2,
        deal_status: 2,
        close_position_detail: Some(ProtoOaClosePositionDetail {
            entry_price: 150.02,
            gross_profit: 67,
            swap: 0,
            commission: -3,
            balance: 1_000_064,
            quote_to_deposit_conversion_rate: Some(0.006_66),
            closed_volume: Some(100_000),
            money_digits: Some(2),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// **The venue's own figure for a close, over the wire**: the deal list's closing deals come back descaled with
/// the venue's raw rate beside them and the opening deal is left out; this is what the demo drill sets the
/// engine's realized PnL beside (nothing in the engine reads it).
#[test]
fn the_deal_list_yields_what_the_venue_booked_on_each_closing_deal() {
    let mut opening = closing_deal(8, 1_000);
    opening.close_position_detail = None;
    let mut venue = usd_account(Spot::Silent);
    venue.deals = vec![closing_deal(9, 2_000), opening];
    let served = serve(venue);
    let client = vike_ctrader::CtraderReconClient::connect(&cfg(&served), "EURUSD")
        .expect("a dedicated socket");
    let got = client.close_details(0).expect("the deal list");
    assert_eq!(got.len(), 1, "the opening deal has no close detail: {got:?}");
    let d = &got[0];
    assert_eq!((d.deal_id, d.symbol.as_str(), d.side, d.filled_units), (9, "USDJPY", -1, 1000.0));
    assert_eq!((d.entry_price, d.execution_price), (150.02, Some(150.12)));
    assert_eq!((d.gross_profit, d.commission, d.balance), (0.67, -0.03, 10_000.64));
    assert_eq!(d.quote_to_deposit_rate, Some(0.006_66), "read raw, never interpreted");
    let line = d.line();
    assert!(line.starts_with(
        "CTDEAL id=9 sym=USDJPY side=-1 units=1000 px=150.12 entry=150.02 gross=0.67"
    ));
}

/// **The venue's own unrealized PnL of an open 10,000-unit position, over the wire**: the drill holds a larger
/// position than its 1,000-unit round trip (whole cents cannot tell orientation, units and rate effects apart)
/// and reads this beside the engine's mark. The row is joined to its position (symbol, side, size) and
/// descaled by the reply's own `moneyDigits`.
#[test]
fn the_unrealized_pnl_of_an_open_position_is_read_with_its_symbol_and_size() {
    let mut venue = usd_account(Spot::Silent);
    venue.open = vec![(7, 2, 1, 1_000_000)]; // USDJPY, long, 10,000 units
    venue.unrealized = vec![(7, 6662, 6600)];
    let served = serve(venue);
    let client = vike_ctrader::CtraderReconClient::connect(&cfg(&served), "EURUSD")
        .expect("a dedicated socket");
    let got = client.unrealized_pnls().expect("the unrealized pnl");
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(
        got[0].line(),
        "CTPNL position=7 sym=USDJPY side=1 units=10000 gross=66.62 net=66 digits=2"
    );
}

/// A reply that says it has more rows than it carries is an error, never a quietly short list.
#[test]
fn a_truncated_deal_list_is_an_error_not_a_short_list() {
    let mut venue = usd_account(Spot::Silent);
    venue.deals = vec![closing_deal(9, 2_000)];
    venue.deals_more = true;
    let served = serve(venue);
    let client = vike_ctrader::CtraderReconClient::connect(&cfg(&served), "EURUSD")
        .expect("a dedicated socket");
    let why = client.close_details(0).expect_err("truncated");
    assert!(why.contains("has_more"), "{why}");
}
