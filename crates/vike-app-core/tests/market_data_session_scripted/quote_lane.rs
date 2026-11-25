//! The market-data plane's quote lane, as the desktop receives it (decision 0193, verdicts 8 and 9).

use super::*;
use std::sync::atomic::AtomicBool;
use vike_datahub_client::market::MarketDataFrameQuote;
use vike_datahub_client::proto::FEATURE_MARKET_DATA_LANE_QUOTES;
use vike_model::{StreamDelay, StreamLabel};

/// A datahub that serves the quote lane on `venues`.
fn quote_features(venues: &[&str]) -> Vec<String> {
    let mut f = features_for(venues);
    f.push(FEATURE_MARKET_DATA_LANE_QUOTES.to_string());
    f
}

fn quote_frame(
    venue: &str,
    symbol: &str,
    age_ms: u64,
    delay: Option<StreamDelay>,
) -> MarketDataFrame {
    MarketDataFrame::Quote(MarketDataFrameQuote {
        venue: venue.into(),
        symbol: symbol.into(),
        bid: 190.1,
        ask: 190.25,
        bid_size: 0.0,
        ask_size: 0.0,
        venue_timestamp: 1,
        age_ms,
        delay,
        sequence: 1,
    })
}

/// **A quote reaches the store with its age on THIS desktop's clock and its label as the server gave
/// it**, so the screen can say `DELAYED`. Reddens on the arm going, on a wire stamp used for the age,
/// and on the label dropped between the frame and the store. FXCM is the venue on purpose: its caps row
/// declares no live data, so a `want` still routed through that table would refuse it.
#[test]
fn a_quote_frame_lands_with_its_age_and_its_label() {
    let fake = spawn_fake(
        quote_features(&["fxcm"]),
        Box::new(|specs| {
            ScriptedAnswer::accept_all(
                specs,
                vec![quote_frame("fxcm", "EUR/USD", 5_000, Some(StreamDelay::DELAYED))],
            )
        }),
    );
    let mut h = Harness::new();
    h.attach(&fake);
    h.feed("fxcm").subscribe_quotes("EUR/USD").expect("the server's word, not the caps table's");
    let trades = h.trades.clone();
    until("the quote to land", || trades.quote("fxcm", "EUR/USD").is_some());
    let q = h.trades.quote("fxcm", "EUR/USD").expect("landed");
    assert_eq!((q.bid, q.ask), (190.1, 190.25));
    assert_eq!(q.delay, Some(StreamDelay::DELAYED));
    let age = vike_model::now_ms() - q.at_ms;
    assert!(
        (5_000..15_000).contains(&age),
        "the hub's age counts, the wire's stamp does not: {age}"
    );
    assert_eq!(q.tick, 0.01, "the step of the prices as spelled");
}

/// **A server without the word is sent no quote spec, and the keys it can serve survive that**: the
/// refusal is local and permanent, and it costs the other lane's dial nothing. Reddens on the local
/// check going, which would send a server a frame it cannot decode, and lose every spec beside it.
#[test]
fn an_older_server_is_asked_for_no_quote_lane_and_keeps_serving_the_rest() {
    let fake = spawn_fake(
        market_data_features(),
        Box::new(|specs| ScriptedAnswer::accept_all(specs, Vec::new())),
    );
    let mut h = Harness::new();
    h.attach(&fake);
    assert_matches!(
        h.feed("binance").subscribe_quotes("BTCUSDT"),
        Err(DataClientError::Unsupported(_))
    );
    h.feed("binance").subscribe_depth("BTCUSDT").expect("depth is unaffected");
    until("the depth key to be served", || h.status("binance").text.contains("1/1 stream(s) live"));
}

/// **A quote key wanted before the handshake landed, on a server that turns out to have no quote lane,
/// must not cost the others their stream.** After a re-point the caps are unknown, so `want` accepts
/// optimistically, and the client's own refusal fails that one dial with nothing sent. Reddens on
/// `refuse_quote_keys` going, where every later dial then fails the same way, for ever.
#[test]
fn a_quote_key_wanted_before_the_handshake_does_not_cost_an_older_server_the_dial() {
    let new = spawn_fake(
        quote_features(&["binance"]),
        Box::new(|specs| ScriptedAnswer::accept_all(specs, Vec::new())),
    );
    let old = spawn_fake(
        market_data_features(),
        Box::new(|specs| ScriptedAnswer::accept_all(specs, Vec::new())),
    );
    let mut h = Harness::new();
    h.attach(&new);
    let id = h.feed("binance").subscribe_quotes("BTCUSDT").expect("served here");
    h.feed("binance").subscribe_depth("ETHUSDT").expect("served everywhere");
    let session = h.mount.session.clone();
    until("the new server to serve both", || h.status("binance").text.contains("2/2"));

    session.set_address(Some(&old.address.to_string()));
    until("the older server to serve the depth key", || h.status("binance").text.contains("1/1"));
    assert_matches!(
        h.feed("binance").subscribe_quotes("BTCUSDT"),
        Err(DataClientError::Unsupported(_)),
        "and the quote key is refused for good there"
    );
    h.feed("binance").unsubscribe(id);
}

/// **`NotHeld` is not a refusal for good**: the key stays wanted, the line says why in words that read
/// neither Connected nor Connecting, and the window attaches on its own when a strategy takes the key
/// up. Reddens on the variant turning permanent, and on the arm in `refresh_statuses` going, which would
/// tell an operator the ladder fills any moment now, for ever.
#[test]
fn a_key_nobody_holds_stays_wanted_says_so_and_attaches_when_someone_does() {
    use vike_model::feed_status::DataClientConnectionState;

    let held = Arc::new(AtomicBool::new(false));
    let flag = held.clone();
    let fake = spawn_fake(
        quote_features(&["oanda"]),
        Box::new(move |specs| {
            if flag.load(Ordering::SeqCst) {
                ScriptedAnswer::accept_all(specs, Vec::new())
            } else {
                ScriptedAnswer {
                    accepted: Vec::new(),
                    refused: specs
                        .iter()
                        .map(|s| (s.clone(), MarketDataRefusal::NotHeld))
                        .collect(),
                    push: Vec::new(),
                }
            }
        }),
    );
    let mut h = Harness::new();
    h.attach(&fake);
    h.feed("oanda").subscribe_quotes("EUR_USD").expect("accepted locally: the server decides");
    until("the line to name the holders", || h.status("oanda").text.contains("resident row"));
    let line = h.status("oanda");
    assert!(line.text.contains("EUR_USD"), "{line:?}");
    assert_eq!(line.state, DataClientConnectionState::Unknown, "{line:?}");

    held.store(true, Ordering::SeqCst);
    until("the key to attach", || h.status("oanda").text.contains("1/1 stream(s) live"));
}

/// **A gap on the lane forgets the price**: a bid and an ask known to be cut off are worse on screen than
/// none, the rule a book's gap keeps. Reddens on the lane's arm in the status match going quiet. The order here is the
/// hub's: the quote of an earlier tick, then the status, and no quote behind it, which is the hub's half
/// (`crates/vike-datahub/tests/default_build/market_data_attach.rs`'s `a_gap_on_the_quote_lane_is_not_undone_by_the_price_it_cut_off`,
/// because this client takes any `Quote` as a stream that flows).
#[test]
fn a_gap_on_the_quote_lane_forgets_the_price() {
    let fake = spawn_fake(
        quote_features(&["oanda"]),
        Box::new(|specs| {
            ScriptedAnswer::accept_all(
                specs,
                vec![
                    quote_frame("oanda", "EUR_USD", 0, None),
                    MarketDataFrame::Status {
                        venue: "oanda".into(),
                        symbol: "EUR_USD".into(),
                        lane: StreamLabel::Quotes,
                        status: StreamStatus::GapStart { at_timestamp_ms: 1 },
                    },
                ],
            )
        }),
    );
    let mut h = Harness::new();
    h.attach(&fake);
    h.feed("oanda").subscribe_quotes("EUR_USD").unwrap();
    let trades = h.trades.clone();
    until("the quote to land", || trades.quote("oanda", "EUR_USD").is_some());
    until("the gap to forget it", || trades.quote("oanda", "EUR_USD").is_none());
    until("the line to stop saying the stream flows", || {
        !h.status("oanda").text.contains("1/1 stream(s) live")
    });
}
