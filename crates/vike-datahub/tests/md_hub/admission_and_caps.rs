//! T6c, T6b, T10 and the resident/symbol checks: the lane label, the serving gate, the resident set and its caps, and symbol validation.

use super::*;
use std::assert_matches;
use vike_datahub::md::MD_LINGER;

// ------------------------------------------------------------------------------------------------
// T6c — the lane LABEL, which admission gates cannot see
// ------------------------------------------------------------------------------------------------

/// **A `Depth` subscription's frames decode as `MdFrame::Depth`, NEVER `MdFrame::Book`** — and the
/// polymarket inverse.
///
/// ⚠ This is the test a reviewer is most likely to call redundant with the caps gate, and it is the
/// only one that catches the mutation that matters. `Depth` and `Book` are separate VARIANTS over
/// the SAME payload struct, so nothing in the type system stops the publisher putting a
/// Depth-sourced snapshot into an `MdFrame::Book`. Swap the variant constructor in the publisher's
/// Depth arm: the admission gates stay green, this goes red alone. Without it §7.5's claim is
/// untested on the only path that actually carries data — and letting a conflating lane wear the
/// book's name is what lets a maker-fill backtest report fills it could never have got.
#[test]
fn a_conflating_lane_never_wears_the_lossless_lanes_name() {
    let log = Log::default();
    let h = hub(&log);
    let sink = h.sink();

    let depth = spec("binance", "BTCUSDT.P", MdLane::Depth);
    let mut g = h.open_session().unwrap();
    h.acquire(g.id(), &depth).unwrap();
    h.reconcile(now());
    sink.l2_snapshot(
        "binance",
        "BTCUSDT.P",
        0.1,
        vec![BookLevel::new(1.0, 1.0)],
        vec![BookLevel::new(2.0, 1.0)],
        1,
    );
    h.publish_tick();
    let frames = drain(g.mailbox());
    assert!(!frames.is_empty(), "the guard: the depth lane must have produced something");
    assert!(
        frames.iter().all(|f| !matches!(f, MdFrame::Book(_))),
        "a DEPTH subscription must never yield a Book frame: {frames:?}"
    );
    assert!(frames.iter().any(|f| matches!(f, MdFrame::Depth(_))), "{frames:?}");
    g.release_at(now());

    // ...and the inverse, on the one venue that serves the lossless lane.
    let book = spec("polymarket", "12345", MdLane::Book);
    let mut p = h.open_session().unwrap();
    h.acquire(p.id(), &book).expect("polymarket serves the Book lane");
    h.reconcile(now());
    sink.book("polymarket", "12345", Arc::new(vike_model::L2Book::new(0.001)));
    h.publish_tick();
    let frames = drain(p.mailbox());
    assert!(!frames.is_empty(), "the guard: the book lane must have produced something");
    assert!(
        frames.iter().all(|f| !matches!(f, MdFrame::Depth(_))),
        "a BOOK subscription must never yield a Depth frame: {frames:?}"
    );
    p.release_at(now());
}

// ------------------------------------------------------------------------------------------------
// T6b — the SERVING gate, and it must refuse BEFORE it subscribes
// ------------------------------------------------------------------------------------------------

/// A lane the venue's declared caps do not serve is refused with `require_live_verb`'s OWN string,
/// **and the double records ZERO subscribe calls** — a hub that refused AFTER subscribing has
/// already opened the socket and spent venue budget.
///
/// String EQUALITY, not `contains("book")`: equality is what makes the wire's refusal set
/// structurally unable to drift from `crates/vike-model/src/venues/venue_caps.rs`'s declared rows.
#[test]
fn an_unsupported_lane_is_refused_with_the_matrixs_own_words_and_no_venue_call() {
    use vike_datahub_client::market::MdRefusal;
    let log = Log::default();
    let h = hub(&log);
    let mut g = h.open_session().unwrap();

    let bad = spec("binance", "BTCUSDT.P", MdLane::Book);
    let err = h.acquire(g.id(), &bad).expect_err("binance declares no lossless book lane");
    let want = vike_data::require_live_verb("binance", vike_model::LiveVerb::Book)
        .expect_err("the matrix must refuse it")
        .to_string();
    assert_eq!(err, MdRefusal::LaneUnsupported(want));
    assert!(err.is_permanent(), "a capability refusal is permanent — the client must not retry");

    // The inverse, on the venue whose partition runs the other way.
    let bad2 = spec("polymarket", "12345", MdLane::Depth);
    assert_matches!(
        h.acquire(g.id(), &bad2).expect_err("polymarket declares no depth lane"),
        MdRefusal::LaneUnsupported(_)
    );

    h.reconcile(now());
    assert_eq!(
        log.all(),
        Vec::new(),
        "a refused spec must open NO venue client and make NO subscribe call: {:?}",
        log.all()
    );
    g.release_at(now());
}

/// An UNKNOWN venue and a venue this BUILD does not serve get DIFFERENT refusals, in that order of
/// precedence — cheapest and most permanent first.
#[test]
fn an_unknown_venue_and_an_unserved_one_are_different_refusals() {
    use vike_datahub_client::market::MdRefusal;
    let log = Log::default();
    let h = hub(&log);
    let mut g = h.open_session().unwrap();
    assert_eq!(
        h.acquire(g.id(), &spec("kalshi", "X", MdLane::Trades)).unwrap_err(),
        MdRefusal::UnknownVenue
    );
    // `okx` IS a roster venue and IS servable by the caps matrix, but this hub was not built for it.
    assert_matches!(
        h.acquire(g.id(), &spec("okx", "BTC-USDT-SWAP", MdLane::Depth)).unwrap_err(),
        MdRefusal::VenueNotServed(_)
    );
    g.release_at(now());
}

// ------------------------------------------------------------------------------------------------
// T10 — the resident set
// ------------------------------------------------------------------------------------------------

/// **A RESIDENT key is never reaped through client release**, while a NON-resident key at zero IS
/// reaped in the SAME `reconcile` call.
///
/// ⚠ The second half is the non-vacuity floor: without it, "nothing was reaped" passes for the wrong
/// reason. And the failure it catches is the one tier R exists to prevent — a janitor that treats
/// residents like on-demand keys silently retires the operator's declared set the moment the last
/// desktop closes.
#[test]
fn a_resident_key_outlives_every_client_while_an_on_demand_one_is_reaped() {
    let log = Log::default();
    let h = hub(&log);
    let res = spec("binance", "BTCUSDT.P", MdLane::Depth);
    let dem = spec("binance", "ETHUSDT.P", MdLane::Depth);
    let at = now();

    h.add_resident(&res).expect("a served venue on a supported lane");
    let mut g = h.open_session().unwrap();
    h.acquire(g.id(), &dem).unwrap();
    h.reconcile(at);
    assert_eq!(log.count(|c| matches!(c, Call::Depth(..))), 2);

    g.release_at(at);
    let r = h.reconcile(at + MD_LINGER.as_millis() as i64 + 1);
    assert_eq!(r.stopped, 1, "exactly the ON-DEMAND key: {r:?}");
    let unsubs: Vec<Call> =
        log.all().into_iter().filter(|c| matches!(c, Call::Unsubscribe(..))).collect();
    assert_eq!(unsubs.len(), 1, "and only one: {unsubs:?}");
    assert_eq!(
        log.count(|c| matches!(c, Call::Shutdown(_))),
        0,
        "the resident key keeps the venue client alive"
    );
    // ...and the resident key is STILL there and still serving.
    h.sink().l2_snapshot(
        "binance",
        "BTCUSDT.P",
        0.1,
        vec![BookLevel::new(1.0, 1.0)],
        vec![BookLevel::new(2.0, 1.0)],
        9,
    );
    let mut g2 = h.open_session().unwrap();
    h.acquire(g2.id(), &res).unwrap();
    h.reconcile(at + MD_LINGER.as_millis() as i64 + 2);
    assert_eq!(
        log.count(|c| matches!(c, Call::Depth(..))),
        2,
        "a resident key is never re-subscribed — it was never stopped"
    );
    g2.release_at(at);
}

// ------------------------------------------------------------------------------------------------
// The resident set is CHECKED and CAPPED
// ------------------------------------------------------------------------------------------------

/// **A resident row that cannot be served is refused AT DECLARATION, not retried forever.**
///
/// `md::parse_resident_set` validates only the three-field shape and the lane word, so
/// `notavenue:X:depth` and a real venue on a lane its declared caps do not serve both PARSE. Before
/// this, `add_resident` inserted them anyway — and a resident entry is permanently `wanted`, so
/// `reconcile` phase 1 retried it every pass and the `md-reconcile` loop logged the failure every
/// `MD_REAP_INTERVAL` for the life of the process, with no line naming the row.
///
/// The fourth leg is the non-vacuity floor: a GOOD row on the same hub is still accepted.
#[test]
fn a_resident_row_that_cannot_be_served_is_refused_by_name() {
    let log = Log::default();
    let h = hub(&log);

    let bad_venue = h.add_resident(&spec("notavenue", "X", MdLane::Depth));
    assert!(bad_venue.is_err(), "an unknown venue must be refused");

    let unserved = h.add_resident(&spec("okx", "BTCUSDT.P", MdLane::Depth));
    assert!(unserved.is_err(), "a real venue this build does not link must be refused");

    let bad_lane = h.add_resident(&spec("binance", "BTCUSDT.P", MdLane::Book));
    let why = bad_lane.expect_err("binance declares book: false — the matrix must refuse it");
    assert!(why.contains("book") || why.contains("Book"), "the refusal names the lane: {why}");

    h.add_resident(&spec("binance", "BTCUSDT.P", MdLane::Depth))
        .expect("floor: a GOOD row on the same hub is still accepted");
    // ...and nothing unservable is in the registry to be retried: the ONE reconcile pass subscribes
    // exactly the good row.
    let r = h.reconcile(now());
    assert!(r.failed.is_empty(), "a refused row must not become an endless retry: {r:?}");
    assert_eq!(r.started, 1, "exactly the good row: {r:?}");
}

/// **The resident set is BOUNDED — per venue and in total.**
///
/// `MD_MAX_KEYS_PER_VENUE`'s own doc says "RESIDENT INCLUDED" and only `acquire` kept it, so a fat
/// `VIKE_DATAHUB_LIVE_RESIDENT` armed N venue subscriptions with N unbounded: memory (§12.6's hub
/// term, the LARGER of the two in the 32 MB claim) and venue budget (200 resident binance depth keys
/// is ~2,000 weight/min of re-seed against a 2,400/min IP budget shared with the order-signing
/// daemon).
#[test]
fn the_resident_set_is_capped_per_venue_and_in_total() {
    use vike_datahub::md::{MD_MAX_KEYS_PER_VENUE, MD_MAX_KEYS_RESIDENT};
    let log = Log::default();
    let h = hub(&log);

    // Fill ONE venue to its cap on a single lane, then ask for one more.
    let mut pinned = 0u32;
    for i in 0..MD_MAX_KEYS_PER_VENUE {
        h.add_resident(&spec("binance", &format!("SYM{i}"), MdLane::Depth))
            .unwrap_or_else(|e| panic!("row {i} must fit inside the cap: {e}"));
        pinned += 1;
    }
    assert_eq!(pinned, MD_MAX_KEYS_PER_VENUE, "floor: the cap was actually reached");
    let why = h
        .add_resident(&spec("binance", "ONE_TOO_MANY", MdLane::Depth))
        .expect_err("the per-venue cap must refuse the next one");
    assert!(why.contains("MD_MAX_KEYS_PER_VENUE"), "the refusal names its number: {why}");

    // ...and the TOTAL cap bites on a DIFFERENT venue, which the per-venue cap cannot see. The rows
    // above already spent the process-wide budget (the two constants are equal today, so one full
    // venue is one full process — the assertion is on WHICH cap answers, not on the arithmetic).
    let why = h
        .add_resident(&spec("polymarket", "TOKEN", MdLane::Book))
        .expect_err("the total resident cap must refuse a row on an EMPTY venue");
    assert!(why.contains("MD_MAX_KEYS_RESIDENT"), "the refusal names its number: {why}");
    assert!(pinned >= MD_MAX_KEYS_RESIDENT, "floor: the total budget was actually spent");
}

// ------------------------------------------------------------------------------------------------
// The SYMBOL is validated — the fifth field, which nothing looked at
// ------------------------------------------------------------------------------------------------

/// **An OVER-LENGTH symbol is refused at the door and makes no venue call.**
///
/// Before this, `MdHub::acquire` validated the venue, whether this build serves it, the lane and
/// three caps — and never the symbol. A client subscribing with a 10,000-character symbol got an
/// `Ok`: the key entered the registry, `poke()` woke the reconciler, and the next pass called
/// `subscribe_depth(venue, <10 KB>)` on the real venue. The ctrl frame that key then produces is
/// far over `vike_datahub::md::MD_CTRL_FRAME_CEILING_BYTES`, which is a TERM in `MD_MAILBOX_BYTES`'
/// compile-time assertion — so one wire field invalidated the 32 MB ceiling
/// `docs/decisions/0052-a-market-data-subscription-is-an-observe-verb.md` rests on.
///
/// `log.all() == Vec::new()` after a `reconcile` is this suite's existing proof that a refusal
/// happened BEFORE any venue call — the shape
/// `an_unsupported_lane_is_refused_with_the_matrixs_own_words_and_no_venue_call` already uses.
#[test]
fn an_over_length_symbol_is_refused_at_the_door_and_makes_no_venue_call() {
    use vike_datahub_client::market::{MD_MAX_SYMBOL_BYTES, MdRefusal};
    let log = Log::default();
    let h = hub(&log);
    let mut g = h.open_session().unwrap();

    let huge = "A".repeat(10_000);
    let err = h
        .acquire(g.id(), &spec("binance", &huge, MdLane::Depth))
        .expect_err("an unbounded symbol is a per-request cost the CLIENT names");
    let MdRefusal::SymbolRejected(why) = &err else {
        panic!("expected MdRefusal::SymbolRejected, got {err:?}");
    };
    assert!(why.contains("10000"), "the refusal names the length it was given: {why}");
    assert!(why.contains(&MD_MAX_SYMBOL_BYTES.to_string()), "...and the cap it exceeded: {why}");
    assert!(
        !why.contains(&huge),
        "...and does NOT echo the symbol back — a refusal that quotes an unbounded field is the \
         same unbounded cost wearing a log line: {} chars",
        why.len()
    );
    assert!(
        err.is_permanent(),
        "a symbol this long can never become legal — the client must not hold it in a desired set \
         and retry it forever"
    );

    // ...and the bound is a BOUND, not a magnitude check: one byte over is refused too.
    let over_by_one = "A".repeat(MD_MAX_SYMBOL_BYTES + 1);
    assert_matches!(
        h.acquire(g.id(), &spec("binance", &over_by_one, MdLane::Depth)),
        Err(MdRefusal::SymbolRejected(_)),
        "MD_MAX_SYMBOL_BYTES + 1 must be refused"
    );

    h.reconcile(now());
    assert_eq!(
        log.all(),
        Vec::new(),
        "a refused spec must open NO venue client and make NO subscribe call: {:?}",
        log.all()
    );
    g.release_at(now());
}

/// **A BLANK symbol is refused** — the sibling of the blank `--produced-by` this PR closes on the
/// delete verb, wearing the other hat. `acquire` accepted `symbol: ""`, the key was admitted, and
/// `reconcile` phase 1 called `subscribe_depth("")` on the real venue.
///
/// ⚠ It is also an ASYMMETRY this removes: `vike_datahub::md::parse_resident_set` has always
/// refused an empty symbol in the operator's OWN declaration, while the network client's request
/// was not checked at all — the daemon judged itself more strictly than it judged a stranger.
#[test]
fn a_blank_symbol_is_refused() {
    use vike_datahub_client::market::MdRefusal;
    let log = Log::default();
    let h = hub(&log);
    let mut g = h.open_session().unwrap();

    for blank in ["", "   ", "\t"] {
        match h.acquire(g.id(), &spec("binance", blank, MdLane::Depth)) {
            Err(MdRefusal::SymbolRejected(why)) => {
                assert!(why.contains("BLANK"), "the refusal names what was wrong: {why}");
            }
            other => panic!("blank {blank:?} must be refused, got {other:?}"),
        }
    }

    h.reconcile(now());
    assert_eq!(log.all(), Vec::new(), "{:?}", log.all());
    g.release_at(now());
}

/// **An ASCII CONTROL BYTE in a symbol is refused**, and that rule is part of the BOUND rather than
/// hygiene: serde_json escapes a byte below `0x20` as a six-byte `\u00XX`, so without this rule the
/// worst-case expansion factor `MD_MAX_SYMBOL_BYTES` was derived against is 6 rather than 2 and the
/// derivation does not hold. The rule is deliberately the NARROWEST one that makes it hold — space,
/// the quote character, the backslash and every non-ASCII byte stay legal, because a venue this
/// plane does not yet serve may spell a real instrument with them (an IBKR OSI local symbol carries
/// padding spaces).
#[test]
fn a_control_byte_in_a_symbol_is_refused() {
    use vike_datahub_client::market::MdRefusal;
    let log = Log::default();
    let h = hub(&log);
    let mut g = h.open_session().unwrap();

    let err = h
        .acquire(g.id(), &spec("binance", "BTC\u{1}USDT", MdLane::Depth))
        .expect_err("a control byte breaks the escape factor the bound was derived against");
    assert_matches!(err, MdRefusal::SymbolRejected(_), "{err:?}");

    // ...and the NON-vacuity floor for the narrowness: a space is legal.
    h.acquire(g.id(), &spec("binance", "BTC USDT", MdLane::Depth))
        .expect("a space is NOT a control byte — refusing it would refuse a real OSI symbol");

    g.release_at(now());
}

/// **THE FLOOR THE BOUND MUST CLEAR** — the longest symbol this wire can actually carry is still
/// ACCEPTED, and still produces a real venue subscription.
///
/// A polymarket CLOB token id is a `uint256` spelled in decimal, and `2^256 - 1` is exactly 78
/// digits — so 78 is a MAXIMUM by construction rather than a measurement, and §12.3 of
/// `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` could not have measured a
/// 79. This is the leg that would catch a bound chosen under it, which is the way a length limit
/// goes wrong in the direction nobody tests.
#[test]
fn a_polymarket_token_id_at_the_hard_maximum_is_still_accepted() {
    let log = Log::default();
    let h = hub(&log);
    let mut g = h.open_session().unwrap();

    let token = "7".repeat(78);
    h.acquire(g.id(), &spec("polymarket", &token, MdLane::Book))
        .expect("78 digits is the widest a uint256 token id can be — it must be SERVED");
    let r = h.reconcile(now());
    assert_eq!(r.started, 1, "...and it really subscribes: {r:?}");
    assert!(
        log.all().contains(&Call::Book("polymarket".into(), token.clone())),
        "the venue was asked for the symbol VERBATIM: {:?}",
        log.all()
    );
    g.release_at(now());
}

/// **The operator's OWN resident declaration faces the same validator**, so the daemon cannot admit
/// through `VIKE_DATAHUB_LIVE_RESIDENT` what it refuses on the wire. The refusal stays a `String`
/// there for the reason `MdHub::add_resident`'s own doc gives: a resident row never crosses the
/// wire, and a wire variant nothing can see would widen a client-facing classification.
#[test]
fn a_resident_row_faces_the_same_symbol_validator() {
    use vike_datahub_client::market::MD_MAX_SYMBOL_BYTES;
    let log = Log::default();
    let h = hub(&log);

    let why = h
        .add_resident(&spec("binance", &"A".repeat(MD_MAX_SYMBOL_BYTES + 1), MdLane::Depth))
        .expect_err("a resident row is a subscription like any other");
    assert!(why.contains(&MD_MAX_SYMBOL_BYTES.to_string()), "{why}");

    // The floor: a GOOD row on the same hub is still accepted, and nothing unservable is left to
    // be retried forever.
    h.add_resident(&spec("binance", "BTCUSDT.P", MdLane::Depth)).expect("a good row");
    let r = h.reconcile(now());
    assert!(r.failed.is_empty(), "{r:?}");
    assert_eq!(r.started, 1, "{r:?}");
}
