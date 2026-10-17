//! Refusals (local and wire) and the frame routing: receipts, the re-stamp, tape gaps, stale books.

use super::*;

// ── 1. the local capability refusal ──────────────────────────────────────────────────────────────

/// **A server that does not advertise `market_data` mounts NO feed, is sent nothing past the
/// handshake, and the link line names the fix.**
///
/// The feed set is the server's advertisement, so a datahub built without the plane (or with
/// `flags.datahub_live` off) has nothing to mount a feed FOR: no window finds a feed to ask, and the
/// question "is the optimistic first subscribe recorded as a permanent `Refused`?" cannot arise —
/// there is no first subscribe. With no venue line to carry the reason, `MdSession::link_status` does.
#[test]
fn a_server_with_no_market_data_plane_mounts_no_feed_and_names_the_fix() {
    use vike_model::feed_status::{ConnectionState, parse_feed_status};

    // Advertises `load_bars` and nothing else — an older datahub, or one built without `live-feeds`.
    let fake = spawn_fake(
        vec!["load_bars".to_string()],
        Box::new(|specs| ScriptedAnswer::accept_all(specs, Vec::new())),
    );
    let mut h = Harness::new();
    h.attach(&fake);

    assert!(h.mount.feeds.is_empty(), "a plane-less server advertises nothing, so mounts nothing");
    assert!(h.mount.feed_statuses.is_empty(), "...and no status line either");
    until("the link line to name the missing plane", || {
        h.mount.session.link_status().contains("live-feeds")
    });
    let link = h.mount.session.link_status();
    assert_eq!(
        parse_feed_status(&link),
        ConnectionState::Error,
        "a datahub with NO plane must not read as healthy: {link:?}"
    );
    // ⚠ NOTHING was ever sent past the handshake: this server was never asked to subscribe.
    let seen = fake.seen.recv_timeout(SETTLE).expect("the fake reports what it read");
    assert_eq!(
        seen,
        vec!["Hello".to_string()],
        "a frame was sent to a server that cannot serve it"
    );
}

/// The venue half of the same rule: a server that serves market data but advertises no
/// `md_venue=polymarket` mounts no feed for that venue (so no window can ask it for anything), and
/// still serves the ones it advertises.
#[test]
fn a_venue_the_server_does_not_advertise_has_no_feed() {
    let fake = spawn_fake(
        features_for(&["binance"]),
        Box::new(|specs| ScriptedAnswer::accept_all(specs, Vec::new())),
    );
    let mut h = Harness::new();
    h.attach(&fake);

    assert_eq!(h.mounted(), vec!["binance"]);
    assert!(!h.mount.feeds.contains_key("polymarket"), "an unadvertised venue got a feed");
    assert!(
        !h.mount.feed_statuses.contains_key("polymarket"),
        "an unadvertised venue got a status line"
    );
    assert!(
        h.feed("binance").subscribe_trades("ETHUSDT").is_ok(),
        "...and an advertised one must still be served"
    );
}

/// **The STATIC matrix is consulted first, before the cached advertisement.** binance declares
/// `book: false` and polymarket declares `depth: false` (`vike_venues::venue_caps`), so
/// `require_live_verb` refuses each with no wire traffic — which is the whole reason `FeedRetries`
/// can classify these as `Refused` and never retry them.
#[test]
fn the_static_capability_matrix_refuses_before_any_stream_exists() {
    let fake = spawn_fake(
        features_for(&["binance", "polymarket"]),
        Box::new(|specs| ScriptedAnswer::accept_all(specs, Vec::new())),
    );
    let mut h = Harness::new();
    h.attach(&fake);
    assert_matches!(
        h.feed("binance").subscribe_book("BTCUSDT"),
        Err(LiveDataError::Unsupported(_)),
        "binance declares no lossless book lane"
    );
    assert_matches!(
        h.feed("polymarket").subscribe_depth("0xtoken"),
        Err(LiveDataError::Unsupported(_)),
        "polymarket declares no depth-snapshot lane"
    );
}

/// **`subscribe_bars`/`subscribe_quotes` are DECLARED refusals, not accidents — and they must NOT
/// be routed through the venue matrix.** `caps_for("binance").live_data.bars` is `true` and
/// hyperliquid declares `quotes: true`, so a `require_live_verb` route would answer `Ok` and open a
/// wire request for a lane `MdLane` has no variant for.
#[test]
fn the_bar_and_quote_lanes_are_declared_refusals_on_every_venue() {
    let fake =
        spawn_fake(md_features(), Box::new(|specs| ScriptedAnswer::accept_all(specs, Vec::new())));
    let mut h = Harness::new();
    h.attach(&fake);
    for venue in ["binance", "bybit", "okx", "aster", "hyperliquid", "polymarket"] {
        match h.feed(venue).subscribe_bars("X", "1m") {
            Err(LiveDataError::Unsupported(msg)) => {
                assert!(msg.contains("bar lane"), "{venue}: {msg:?}")
            }
            other => panic!("{venue}: subscribe_bars must be a declared refusal, got {other:?}"),
        }
        match h.feed(venue).subscribe_quotes("X") {
            Err(LiveDataError::Unsupported(msg)) => {
                assert!(msg.contains("quotes lane"), "{venue}: {msg:?}")
            }
            other => panic!("{venue}: subscribe_quotes must be a declared refusal, got {other:?}"),
        }
    }
}

// ── 2. the two refusal halves ────────────────────────────────────────────────────────────────────

/// **A PERMANENT wire refusal leaves the desired set; a CAP refusal stays in it.**
///
/// `MdRefusal::is_permanent` is the split, and it is what decides whether a later `subscribe_*` is
/// answered synchronously (permanent — so `FeedRetries` records `Refused`) or optimistically
/// accepted again (a cap frees up when another window closes).
#[test]
fn a_permanent_refusal_is_remembered_and_a_cap_refusal_is_not() {
    let fake = spawn_fake(
        md_features(),
        Box::new(|specs| ScriptedAnswer {
            accepted: Vec::new(),
            refused: specs
                .iter()
                .map(|s| {
                    let why = match s.lane {
                        MdLane::Trades => MdRefusal::KeyCapTotal { held: 64, cap: 64 },
                        _ => MdRefusal::VenueNotServed("binance".to_string()),
                    };
                    (s.clone(), why)
                })
                .collect(),
            push: Vec::new(),
        }),
    );
    let mut h = Harness::new();
    h.attach(&fake);
    h.feed("bybit").subscribe_depth("BTCUSDT").expect("an advertised venue");
    h.feed("bybit").subscribe_trades("BTCUSDT").expect("an advertised venue");

    until("the refusals to land", || h.status("bybit").contains("refused"));
    assert_matches!(
        h.feed("bybit").subscribe_depth("BTCUSDT"),
        Err(LiveDataError::Unsupported(_)),
        "a PERMANENT refusal must be answered synchronously from then on"
    );
    assert!(
        h.feed("bybit").subscribe_trades("BTCUSDT").is_ok(),
        "a CAP refusal must NOT be remembered as permanent — a cap frees up"
    );
}

// ── 3-5, 7. the frame routing ────────────────────────────────────────────────────────────────────

/// **A book frame lands in `BookStore` with a LOCAL receipt** — §7.4. Scripted with a `venue_ts`
/// far in the past, so a client that stamped the wire time would store a receipt the Trade window
/// reads as permanently stale, and this assertion is what tells the two apart.
#[test]
fn a_book_frame_lands_with_a_local_receipt_not_the_wire_timestamp() {
    const ANCIENT: i64 = 1_500_000_000_000; // 2017 — decades of DOM_STALE_MS ago
    let fake = spawn_fake(
        md_features(),
        Box::new(|specs| {
            ScriptedAnswer::accept_all(specs, vec![book_frame("binance", "BTCUSDT", ANCIENT)])
        }),
    );
    let mut h = Harness::new();
    h.attach(&fake);
    h.feed("binance").subscribe_depth("BTCUSDT").expect("an advertised venue");

    until("the book to arrive", || h.books.get("binance", "BTCUSDT").is_some());
    let (book, receipt) = h.books.get("binance", "BTCUSDT").expect("the book is there");
    assert!(book.best_bid().is_some(), "the ladder rebuilt from the wire's raw levels");
    assert!(
        receipt > ANCIENT + 1_000_000_000,
        "the receipt is the WIRE timestamp ({receipt}) — every ladder would read permanently stale"
    );
    assert!(h.wakes.load(Ordering::SeqCst) > 0, "the producer must wake the frame thread");
}

/// ⚠⚠ **THE KILL PROOF.** The hub STRIPS every tick's symbol (`md/hub.rs`'s `push_trade`) and the
/// envelope carries it once; `GuiFeedSink::trade` keys `TradeStore` by `trade.symbol`. A reader
/// that hands the tick through unchanged puts every print on every venue under `(venue, "")`, where
/// `sync_from_core`'s drain — which drains under each aggregator's real `(venue, symbol)` — finds
/// nothing. The tick/volume and orderflow charts then stay empty for ever, with no error, no log
/// and nothing else in this tree that would notice.
#[test]
fn trade_ticks_are_re_stamped_from_the_envelope_symbol() {
    let fake = spawn_fake(
        md_features(),
        Box::new(|specs| {
            ScriptedAnswer::accept_all(
                specs,
                vec![MdFrame::Trades {
                    venue: "binance".to_string(),
                    symbol: "BTCUSDT".to_string(),
                    ticks: vec![stripped_tick(1_000, 100.0, 1.0), stripped_tick(1_001, 101.0, 2.0)],
                    seq: 1,
                }],
            )
        }),
    );
    let mut h = Harness::new();
    h.attach(&fake);
    h.feed("binance").subscribe_trades("BTCUSDT").expect("an advertised venue");

    until("the prints to arrive", || !h.trades.drain("binance", "BTCUSDT").is_empty());
    assert!(
        h.trades.drain("binance", "").is_empty(),
        "prints landed under the EMPTY symbol — the re-stamp is missing and every tick/volume and \
         orderflow chart on this key would stay silently empty"
    );
}

/// **A `TapeGap` discards what is buffered AND bumps the epoch**, in that order — §7.3 rule 3. The
/// epoch is the only route the reader has to the aggregators, which live on the frame thread.
#[test]
fn a_tape_gap_discards_the_buffered_tape_and_bumps_the_epoch() {
    let fake = spawn_fake(
        md_features(),
        Box::new(|specs| {
            ScriptedAnswer::accept_all(
                specs,
                vec![
                    MdFrame::Trades {
                        venue: "binance".to_string(),
                        symbol: "BTCUSDT".to_string(),
                        ticks: vec![stripped_tick(1_000, 100.0, 1.0)],
                        seq: 1,
                    },
                    MdFrame::TapeGap {
                        venue: "binance".to_string(),
                        symbol: "BTCUSDT".to_string(),
                        dropped: 12,
                        from_seq: 1,
                        to_seq: 14,
                    },
                ],
            )
        }),
    );
    let mut h = Harness::new();
    h.attach(&fake);
    h.feed("binance").subscribe_trades("BTCUSDT").expect("an advertised venue");
    let key = ("binance".to_string(), "BTCUSDT".to_string());

    // ⚠ The subscribe itself bumps the epoch once (a reconnect is a hole — §7.3 rule 4), so the
    // assertion is that the GAP bumps it AGAIN, not that it reaches any particular number.
    until("the gap to be disclosed", || h.mount.session.tape_gap_epoch(&key.0, &key.1) >= 2);
    assert!(
        h.trades.drain("binance", "BTCUSDT").is_empty(),
        "the prints buffered before the hole must be DISCARDED, not folded"
    );
}

/// **A stale book key produces a status and NO book** — §7.3 rule 1. A ladder that is known to be
/// wrong is worse than an empty one, and `BookStore::remove` is what makes "one key" expressible.
#[test]
fn a_stale_status_removes_that_keys_book_and_leaves_its_neighbour_alone() {
    let fake = spawn_fake(
        md_features(),
        Box::new(|specs| {
            ScriptedAnswer::accept_all(
                specs,
                vec![
                    book_frame("binance", "BTCUSDT", 1_700_000_000_000),
                    book_frame("binance", "ETHUSDT", 1_700_000_000_000),
                    MdFrame::Status {
                        venue: "binance".to_string(),
                        symbol: "BTCUSDT".to_string(),
                        lane: MdLane::Depth,
                        status: StreamStatus::Stale {
                            newest_data_ts_ms: 1_700_000_000_000,
                            now_ms: 1_700_000_060_000,
                        },
                    },
                ],
            )
        }),
    );
    let mut h = Harness::new();
    h.attach(&fake);
    h.feed("binance").subscribe_depth("BTCUSDT").expect("an advertised venue");
    h.feed("binance").subscribe_depth("ETHUSDT").expect("an advertised venue");

    until("the stale key's book to be dropped", || {
        h.books.get("binance", "ETHUSDT").is_some() && h.books.get("binance", "BTCUSDT").is_none()
    });
    assert!(
        h.books.get("binance", "ETHUSDT").is_some(),
        "clearing one key's book must not blank every other ladder and cockpit"
    );
}
