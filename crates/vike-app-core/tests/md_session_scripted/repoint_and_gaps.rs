//! Address repoint, the held advertisement and gapped lanes.

use super::*;

// ── 11. the bracket, the blip and the poisoned advertisement ─────────────────────────────────────

/// **§7.3 RULE 4'S BRACKET ON AN ADDRESS CHANGE — the previous server's ladder must be FORGOTTEN.**
///
/// `MdSession::set_addr`'s own doc promises it ("a CHANGE tears the stream down and applies §7.3
/// rule 4's bracket"), and it did not run: the reconciler cleared `served` three statements after
/// `begin_stream_teardown()`, while `open_gap_bracket` — which the READER runs, and whose entire
/// input is `served` — needs a kernel wake-up first. The reconciler won essentially always, `held`
/// was empty, and the bracket removed no book, bumped no epoch and logged nothing. Nothing else in
/// this binary clears `BookStore`, so the old server's ladder stayed painted for ever (flagged STALE
/// after `DOM_STALE_MS`, i.e. readable).
#[test]
fn an_address_change_drops_the_previous_servers_books() {
    let a = spawn_fake(
        md_features(),
        Box::new(|specs| {
            ScriptedAnswer::accept_all(specs, vec![book_frame("binance", "BTCUSDT", 1)])
        }),
    );
    // B accepts and pushes NOTHING, so any book present after the re-point came from A.
    let b =
        spawn_fake(md_features(), Box::new(|specs| ScriptedAnswer::accept_all(specs, Vec::new())));
    let mut h = Harness::new();
    h.attach(&a);
    h.feed("binance").subscribe_depth("BTCUSDT").expect("an advertised venue");
    until("A's book to land", || h.books.get("binance", "BTCUSDT").is_some());

    h.mount.session.set_addr(Some(&b.addr.to_string()));
    until("the re-point to forget A's book", || h.books.get("binance", "BTCUSDT").is_none());
    until("the session to reconnect to B", || h.status("binance").contains(&b.addr.to_string()));
    assert!(
        h.books.get("binance", "BTCUSDT").is_none(),
        "B pushed no book, so a present one is A's — carried across a re-point"
    );
}

/// **A `None` ADDRESS IS NOT A RE-POINT.** `observe_bridge` clears its `advertised_datahub` cell the
/// moment the tradehub link drops and restores it only after the next handshake, a `DIAL_BACKOFF`
/// later — and on a box with no explicit `config.datahub_addr` that advertisement is the WHOLE
/// address ladder. So every tradehub blip arrived here as `set_addr(None)`, which the address-change
/// arm treated as a different server: stream torn down, `Caps` thrown away, every permanent refusal
/// re-opened, every venue's line rewritten to name a misconfiguration that does not exist. The
/// datahub is a different PROCESS and did not go anywhere.
#[test]
fn a_cleared_advertisement_does_not_tear_the_stream_down() {
    let fake = spawn_fake(
        md_features(),
        Box::new(|specs| {
            ScriptedAnswer::accept_all(specs, vec![book_frame("binance", "BTCUSDT", 1)])
        }),
    );
    let mut h = Harness::new();
    h.attach(&fake);
    h.feed("binance").subscribe_depth("BTCUSDT").expect("an advertised venue");
    until("the book to land", || h.books.get("binance", "BTCUSDT").is_some());
    let dials = fake.accepted.load(Ordering::SeqCst);

    // The blip: the bridge drops its advertisement for a moment and puts it back.
    h.mount.session.set_addr(None);
    thread::sleep(PUSH_GAP);
    h.mount.session.set_addr(Some(&fake.addr.to_string()));
    thread::sleep(PUSH_GAP);

    assert!(
        h.books.get("binance", "BTCUSDT").is_some(),
        "a tradehub blip dropped the DOM ladder of a datahub that never went anywhere"
    );
    assert_eq!(
        fake.accepted.load(Ordering::SeqCst),
        dials,
        "a blip re-dialled the datahub — the stream was torn down and rebuilt for nothing"
    );
    assert!(
        !h.status("binance").contains("no datahub configured"),
        "a blip rewrote the status line to name a misconfiguration that does not exist: {:?}",
        h.status("binance")
    );
    // ...and the held advertisement kept the feeds mounted: a blip must not look like a server that
    // advertises nothing (or like one nobody has heard from, which would re-probe it).
    h.sync();
    assert_eq!(h.mounted().len(), 6, "a blip unmounted the feeds");
}

/// **A PLANE-LESS SERVER MUST NOT POISON THE SESSION FOR EVERY LATER ADDRESS.**
///
/// `dial` caches the advertisement BEFORE it refuses — deliberately, so `want` can refuse locally
/// rather than re-attempting for ever — but the clear was keyed on `connected_addr`, which a refused
/// dial never sets. So `Caps { market_data: false }` survived an address change, `want`'s rung 2
/// answered `Unsupported` against the NEW server too, and `FeedRetries` records that as
/// `RetryState::Refused` and never re-asks: every DOM window opened in that window was dead for the
/// life of the process. `Caps`' own doc asserts the opposite ("It IS cleared when the address
/// changes"), which was false in exactly the case where the cached value was harmful.
///
/// Since the feeds are mounted FROM the advertisement, the poison would now be a plane-less
/// server's EMPTY advertisement outliving its address: no feed, for the life of the process, on a
/// perfectly good server. The re-point must therefore re-probe and mount.
#[test]
fn a_plane_less_servers_advertisement_does_not_survive_a_repoint() {
    let none = spawn_fake(
        vec!["load_bars".to_string()],
        Box::new(|specs| ScriptedAnswer::accept_all(specs, Vec::new())),
    );
    let good =
        spawn_fake(md_features(), Box::new(|specs| ScriptedAnswer::accept_all(specs, Vec::new())));
    let mut h = Harness::new();
    h.attach(&none);
    assert!(h.mount.feeds.is_empty(), "the plane-less server mounted a feed");

    h.mount.session.set_addr(Some(&good.addr.to_string()));
    until("the re-point to re-probe and mount the SECOND server's venues", || {
        h.sync();
        h.mounted().len() == 6
    });
    assert!(
        h.feed("okx").subscribe_depth("BTC-USDT-SWAP").is_ok(),
        "the plane-less server's advertisement outlived the address that produced it — every \
         subscribe made from here is a `FeedRetries::Refused` that is never re-asked"
    );
}

/// **A `Status::GapStart` EMPTIES THE LADDER, SO THE STATUS LINE MUST STOP SAYING `live`.**
///
/// `served` is the server's ACCEPTED set and a gap does not change it, so the last arm rendered
/// `1/1 stream(s) live` — Connected — over a ladder this client had just removed. An operator
/// staring at a blank DOM was told by the Connections tool that the venue was fine.
#[test]
fn a_gapped_book_lane_stops_reading_connected() {
    use vike_model::feed_status::{ConnectionState, parse_feed_status};

    let fake = spawn_fake(
        md_features(),
        Box::new(|specs| {
            ScriptedAnswer::accept_all(
                specs,
                vec![
                    book_frame("binance", "BTCUSDT", 1),
                    MdFrame::Status {
                        venue: "binance".to_string(),
                        symbol: "BTCUSDT".to_string(),
                        lane: MdLane::Depth,
                        status: StreamStatus::GapStart { at_ts_ms: 5 },
                    },
                ],
            )
        }),
    );
    let mut h = Harness::new();
    h.attach(&fake);
    h.feed("binance").subscribe_depth("BTCUSDT").expect("an advertised venue");

    until("the book to land and then be dropped by the gap", || {
        h.books.get("binance", "BTCUSDT").is_none() && h.status("binance").contains("0/1")
    });
    let line = h.status("binance");
    assert_ne!(
        parse_feed_status(&line),
        ConnectionState::Connected,
        "a venue whose ladder was just emptied still read Connected: {line:?}"
    );
}

/// **A `Status::GapStart` ON THE TRADES LANE MARKS A TAPE GAP.** Nothing else discloses a
/// VENUE-side hole in a tape: it increments no `entry.dropped`, so the server emits no `TapeGap`;
/// the wire `seq` counts published frames rather than venue ticks, so §7.2's backstop sees nothing.
/// Folding across it leaves `OrderflowAgg` — no per-trade dedup, no gap concept — understating that
/// bar's cells and every CVD value after it, permanently and silently.
///
/// ⚠ This WIDENS §7.3 rule 1, which names the book lanes only. It is deliberate and is flagged in
/// the report; the alternative reading, that a venue-side tape hole is undisclosed by design,
/// contradicts §6.2's "Loss here must be impossible to hide".
#[test]
fn a_gap_on_the_trades_lane_bumps_the_tape_epoch() {
    let fake = spawn_fake(
        md_features(),
        Box::new(|specs| {
            ScriptedAnswer::accept_all(
                specs,
                vec![MdFrame::Status {
                    venue: "binance".to_string(),
                    symbol: "BTCUSDT".to_string(),
                    lane: MdLane::Trades,
                    status: StreamStatus::GapStart { at_ts_ms: 5 },
                }],
            )
        }),
    );
    let mut h = Harness::new();
    h.attach(&fake);
    h.feed("binance").subscribe_trades("BTCUSDT").expect("an advertised venue");

    // The subscribe itself bumps once (§7.3 rule 4's closing half for the tape), so the assertion
    // is that the STATUS moved it again rather than that it is non-zero.
    until("the subscribe's own bump", || h.mount.session.tape_gap_epoch("binance", "BTCUSDT") >= 1);
    until("the venue-side gap to be disclosed to the aggregators", || {
        h.mount.session.tape_gap_epoch("binance", "BTCUSDT") >= 2
    });
}
