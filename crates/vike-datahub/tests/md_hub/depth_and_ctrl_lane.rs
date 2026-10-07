//! The depth fold (up and back down) and the CTRL lane with its attach burst and frame ceiling.

use super::*;
use vike_datahub::md::hub::MdKey;

// ------------------------------------------------------------------------------------------------
// The depth fold
// ------------------------------------------------------------------------------------------------

/// A key's effective depth is the MAX over its live subscribers, clamped to the ceiling — the
/// property §6.1's one-encode-per-key rule forces, and the consequence
/// `crate::md::MD_MAILBOX_BYTES` exists to absorb.
#[test]
fn a_keys_depth_is_the_max_over_its_subscribers_and_is_clamped() {
    use vike_datahub_client::market::MD_DEPTH_LEVELS_CEILING;
    let log = Log::default();
    let h = hub(&log);
    let shallow = MdSpec { depth_levels: Some(5), ..spec("binance", "BTCUSDT.P", MdLane::Depth) };
    let deep = MdSpec { depth_levels: Some(9_999), ..shallow.clone() };

    let mut a = h.open_session().unwrap();
    let acc = h.acquire(a.id(), &shallow).unwrap();
    assert_eq!(acc.depth_levels, Some(5), "the echo is AUTHORITATIVE, not the request");
    let mut b = h.open_session().unwrap();
    let acc = h.acquire(b.id(), &deep).unwrap();
    assert_eq!(
        acc.depth_levels,
        Some(MD_DEPTH_LEVELS_CEILING),
        "a request above the ceiling is CLAMPED and the client LEARNS the number"
    );
    h.reconcile(now());

    // 300 levels a side in, ceiling out: the frame is cut by the server, not by the venue.
    let levels: Vec<BookLevel> = (0..300).map(|i| BookLevel::new(100.0 - i as f64, 1.0)).collect();
    let asks: Vec<BookLevel> = (0..300).map(|i| BookLevel::new(200.0 + i as f64, 1.0)).collect();
    h.sink().l2_snapshot("binance", "BTCUSDT.P", 0.1, levels, asks, 1);
    h.publish_tick();
    for f in drain(a.mailbox()) {
        if let MdFrame::Depth(bk) = f {
            assert_eq!(bk.bids.len(), MD_DEPTH_LEVELS_CEILING as usize);
            assert!(bk.bids[0].price > bk.bids[1].price, "bids DESCEND, best first");
            assert!(bk.asks[0].price < bk.asks[1].price, "asks ASCEND, best first");
        }
    }
    a.release_at(now());
    b.release_at(now());
}

// ------------------------------------------------------------------------------------------------
// The CTRL lane and the attach burst
// ------------------------------------------------------------------------------------------------

/// **A MULTI-KEY ATTACH MUST NOT CLOSE THE CONNECTION IT IS ATTACHING.**
///
/// `crate::server`'s `run_market_writer` pushes one `attach_frames` `Status` per accepted key BEFORE
/// it enters its drain loop, and it is itself the mailbox's only consumer — so nothing drains during
/// that burst and the CTRL lane sees `accepted.len()` frames back to back. At `MD_MAILBOX_CTRL = 16`
/// that was a DETERMINISTIC kill of every session holding 17 or more specs: push 17 set `must_close`
/// and the writer's first statement is `if mailbox.must_close()`, so the peer got a `Bye` before one
/// data frame — well under the advertised `MD_MAX_SPECS_PER_SESSION` of 64.
///
/// ⚠ Two non-vacuity floors. The key count must EXCEED the old constant (16) or the test passes on
/// the bug, and every `Status` must come back out — a lane that silently dropped them would satisfy
/// "not closed" for the wrong reason.
#[test]
fn a_multi_key_attach_never_overflows_the_control_lane() {
    use vike_datahub::md::hub::push_attach_frame;
    use vike_datahub::md::{MD_MAILBOX_CTRL, MD_MAX_KEYS_PER_VENUE};

    let log = Log::default();
    let h = hub(&log);
    let mut g = h.open_session().unwrap();
    // The most keys this harness can reach: 8 symbols x 2 servable lanes on each of the three
    // venues, which is MD_MAX_KEYS_PER_VENUE on every one of them.
    let mut accepted = Vec::new();
    for (venue, lanes) in [
        ("binance", [MdLane::Depth, MdLane::Trades]),
        ("bybit", [MdLane::Depth, MdLane::Trades]),
        ("polymarket", [MdLane::Book, MdLane::Trades]),
    ] {
        for i in 0..8 {
            for lane in lanes {
                let s = spec(venue, &format!("SYM{i}"), lane);
                accepted.push(h.acquire(g.id(), &s).expect("a servable lane inside every cap"));
            }
        }
    }
    assert_eq!(
        accepted.len(),
        3 * MD_MAX_KEYS_PER_VENUE as usize,
        "the harness must reach a MULTI-key session or the burst below proves nothing"
    );
    assert!(
        accepted.len() > 16,
        "floor: the burst must EXCEED the old MD_MAILBOX_CTRL of 16, or this passes on the bug"
    );
    assert!(
        accepted.len() <= MD_MAILBOX_CTRL,
        "...and stay inside the relation the constant holds"
    );

    // Exactly what `run_market_writer` does, in the same order, with NO drain in between.
    let at = now();
    for s in &accepted {
        let key = MdKey::of(s);
        for frame in h.attach_frames(&key, at) {
            push_attach_frame(g.mailbox(), &key, frame);
        }
    }
    assert!(
        !g.mailbox().must_close(),
        "the attach burst CLOSED the connection it was attaching — the peer would get a Bye before \
         one data frame"
    );
    let frames = drain(g.mailbox());
    let statuses = frames.iter().filter(|f| matches!(f, MdFrame::Status { .. })).count();
    assert_eq!(statuses, accepted.len(), "every accepted key's Status survived: {frames:?}");
    g.release_at(now());
}

/// The byte price of the CTRL lane is a MEASURED fact, not an assumption.
///
/// `vike_datahub::md::MD_CTRL_FRAME_CEILING_BYTES` is a term in `MD_MAILBOX_BYTES`' compile-time
/// assertion — `MD_MAX_SPECS_PER_SESSION * MD_FRAME_CEILING_BYTES + MD_MAILBOX_CTRL * this <= the
/// byte bound` — so a `Status` frame wider than it silently falsifies that assertion and, through
/// it, the 32 MB plane ceiling
/// `docs/decisions/0052-a-market-data-subscription-is-an-observe-verb.md` rests on.
///
/// # ⚠ It used to measure ONE example and compare it to nothing else
///
/// The original body framed polymarket's 78-character token id and asserted `<= 384`. That is a
/// witness, not a bound: NOTHING made any other symbol fit, and the wire had no symbol length
/// limit at all — so a 10,000-character symbol produced a ctrl frame twenty-six times the declared
/// ceiling and the assertion above it became false from the wire. The ceiling is now an
/// ARITHMETIC consequence of two independently-pinned terms, and this test pins both:
///
/// 1. **the ENVELOPE** — everything a `Status` frame costs with an EMPTY symbol, at the widest
///    venue slug, lane word and status spelling the roster can produce. Pinned by EQUALITY against
///    `vike_datahub::md::MD_STATUS_ENVELOPE_CEILING_BYTES`, not `<=`, so a wire change that moves
///    the envelope in EITHER direction is caught rather than silently eating the slack the symbol
///    bound was derived from.
/// 2. **the SYMBOL** — a symbol at exactly `MD_MAX_SYMBOL_BYTES` made entirely of `"`, i.e.
///    serde_json's worst expansion of a control-free string. That exercises the `2x` factor
///    `vike_datahub::md::MD_STATUS_ENVELOPE_CEILING_BYTES`' sibling assertion asserts in prose.
///
/// Leg 3 keeps the original measurement as a REGRESSION witness: the real polymarket token id at
/// its hard maximum still frames where it always did.
#[test]
fn a_status_frame_fits_the_declared_ctrl_ceiling() {
    use vike_datahub::md::{MD_CTRL_FRAME_CEILING_BYTES, MD_STATUS_ENVELOPE_CEILING_BYTES};
    use vike_datahub_client::market::{MD_MAX_SYMBOL_BYTES, WireStreamStatus};
    use vike_datahub_client::proto::write_frame;

    fn framed(venue: &str, symbol: String, lane: MdLane, status: WireStreamStatus) -> usize {
        let frame = MdFrame::Status { venue: venue.into(), symbol, lane, status };
        let mut buf = Vec::new();
        write_frame(&mut buf, &Response::Md(Box::new(frame))).expect("frame a Status");
        buf.len()
    }

    // 1. THE ENVELOPE, at every extreme this plane can reach: the longest `vike_model::VENUES`
    //    slug, the longest lane word, and the two-field `Stale` status with both stamps spelled at
    //    their widest (`i64::MIN` is 20 characters).
    let widest_venue =
        vike_model::VENUES.iter().max_by_key(|v| v.len()).expect("the roster is non-empty");
    let envelope = framed(
        widest_venue,
        String::new(),
        MdLane::Trades,
        WireStreamStatus::Stale { newest_data_ts_ms: i64::MIN, now_ms: i64::MIN },
    );
    assert_eq!(
        envelope, MD_STATUS_ENVELOPE_CEILING_BYTES,
        "the empty-symbol Status envelope is {envelope} B and the declared ceiling is \
         {MD_STATUS_ENVELOPE_CEILING_BYTES} B. This is pinned by EQUALITY: the symbol bound was \
         DERIVED from `MD_CTRL_FRAME_CEILING_BYTES - this`, so moving it either way means \
         re-deriving MD_MAX_SYMBOL_BYTES rather than editing this number"
    );

    // 2. THE SYMBOL, at the bound and at serde_json's worst control-free expansion. Every `"`
    //    costs two bytes, which is the factor the assertion in `md/mod.rs` budgets for.
    let worst = framed(
        widest_venue,
        "\"".repeat(MD_MAX_SYMBOL_BYTES),
        MdLane::Trades,
        WireStreamStatus::Stale { newest_data_ts_ms: i64::MIN, now_ms: i64::MIN },
    );
    assert!(
        worst <= MD_CTRL_FRAME_CEILING_BYTES,
        "a symbol at MD_MAX_SYMBOL_BYTES made entirely of quote characters frames to {worst} B \
         against a declared ceiling of {MD_CTRL_FRAME_CEILING_BYTES} B"
    );
    assert!(
        worst > envelope,
        "floor: the symbol must actually be ON the frame, or this leg measures the envelope twice"
    );

    // 3. The original witness — the longest symbol any roster venue can actually spell.
    let real = framed(
        "polymarket",
        "7".repeat(78),
        MdLane::Book,
        WireStreamStatus::Stale { newest_data_ts_ms: 1_757_500_000_000, now_ms: 1_757_500_060_000 },
    );
    assert!(
        real <= MD_CTRL_FRAME_CEILING_BYTES,
        "the widest REAL Status is {real} B against a declared ceiling of \
         {MD_CTRL_FRAME_CEILING_BYTES} B — re-derive MD_MAILBOX_BYTES' assertion before raising it"
    );
}

// ------------------------------------------------------------------------------------------------
// The depth fold comes back DOWN
// ------------------------------------------------------------------------------------------------

/// **A key's depth is refolded from the subscribers that REMAIN**, so one client's ceiling-depth
/// request does not retire the default for everyone else.
///
/// `StreamEntry::depth` was a `fetch_max` and nothing else. Since `publish_tick` serializes ONCE per
/// key at that depth, a single deep subscriber inflated the frame for EVERY subscriber of the key —
/// §12.4's measured 2.74 KB became 10.48 KB — and it never came back down. On a RESIDENT key, which
/// is never reaped, that was permanent for the life of the process.
///
/// ⚠ The resident leg is the one that matters most and the one a `fetch_max` cannot pass: the
/// operator's DECLARED depth must survive as a floor while the deep client's request does not.
#[test]
fn a_deep_subscriber_leaving_gives_the_key_its_shallow_depth_back() {
    let log = Log::default();
    let h = hub(&log);
    let res = MdSpec { depth_levels: Some(10), ..spec("binance", "BTCUSDT.P", MdLane::Depth) };
    h.add_resident(&res).expect("a served venue on a supported lane");

    let shallow = MdSpec { depth_levels: Some(20), ..res.clone() };
    let deep = MdSpec { depth_levels: Some(200), ..res.clone() };
    let mut a = h.open_session().unwrap();
    let mut b = h.open_session().unwrap();
    h.acquire(a.id(), &shallow).unwrap();
    h.acquire(b.id(), &deep).unwrap();
    h.reconcile(now());

    let bids: Vec<BookLevel> = (0..300).map(|i| BookLevel::new(100.0 - i as f64, 1.0)).collect();
    let asks: Vec<BookLevel> = (0..300).map(|i| BookLevel::new(200.0 + i as f64, 1.0)).collect();
    let cut = |g: &vike_datahub::md::SessionGuard| -> usize {
        h.sink().l2_snapshot("binance", "BTCUSDT.P", 0.1, bids.clone(), asks.clone(), 1);
        h.publish_tick();
        drain(g.mailbox())
            .into_iter()
            .find_map(|f| match f {
                MdFrame::Depth(bk) => Some(bk.bids.len()),
                _ => None,
            })
            .expect("a depth frame")
    };

    assert_eq!(cut(&a), 200, "while the deep subscriber is here, the key is cut at ITS depth");
    // The deep one leaves. The shallow one's frame must come back DOWN.
    b.release_at(now());
    assert_eq!(cut(&a), 20, "the fold is over the subscribers that REMAIN, not a high-water mark");
    // ...and the RESIDENT floor survives the last client leaving.
    a.release_at(now());
    let mut c = h.open_session().unwrap();
    h.acquire(c.id(), &MdSpec { depth_levels: Some(5), ..res.clone() }).unwrap();
    assert_eq!(cut(&c), 10, "the operator's DECLARED resident depth is a floor");
    c.release_at(now());
}
