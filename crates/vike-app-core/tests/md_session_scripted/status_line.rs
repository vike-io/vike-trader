//! The per-venue status line is PARSED: it never reads Connected while nothing is serving.

use super::*;

// ── 10. the status vocabulary is PARSED ──────────────────────────────────────────────────────────

/// **NO STATUS LINE THIS SESSION WRITES MAY READ `Connected` WHILE NOTHING IS FLOWING.**
///
/// `vike_model::feed_status::parse_feed_status` is an ORDERED substring classifier — `disconnected`,
/// then `fault|error|failed`, then `connected|live|streaming|subscribed`, then `connect*` — and the
/// Connections tool renders whatever it answers. That makes the word `live` a wire format, not
/// prose, and this session has two lines that naturally contain it and must not:
///
/// * the no-plane refusal, whose whole job is to name `flags.datahub_live` and `live-feeds` — so the
///   line carrying it is prefixed `error:`, which is tested one rung EARLIER;
/// * `{live}/{wanted} stream(s) live`, which at `0/1` said Connected about a venue serving nothing,
///   and said it again as `reconnecting (the link dropped) — 0/2 stream(s) live`.
///
/// Both were live defects. A comment cannot hold this; only reading it back through the real
/// classifier can.
///
/// The server that LOSES its plane is the way to reach the first one now that a server with no plane
/// mounts no line at all: lines are mounted from a good server's advertisement, a key is wanted on
/// one of them, and the session is re-pointed at a plane-less one — the dial is refused, and every
/// line that still exists is rewritten with the refusal.
#[test]
fn a_status_line_never_reads_connected_while_no_stream_is_serving() {
    use vike_model::feed_status::{ConnectionState, parse_feed_status};

    let good =
        spawn_fake(md_features(), Box::new(|specs| ScriptedAnswer::accept_all(specs, Vec::new())));
    // A server with no market-data plane at all: the refusal NAMES the two `live`-bearing tokens.
    let none = spawn_fake(
        vec!["load_bars".to_string()],
        Box::new(|specs| ScriptedAnswer::accept_all(specs, Vec::new())),
    );
    let mut h = Harness::new();
    h.attach(&good);
    h.feed("binance").subscribe_depth("BTCUSDT").expect("an advertised venue");
    h.mount.session.set_addr(Some(&none.addr.to_string()));

    until("the no-plane refusal to reach the status line", || {
        h.status("binance").contains("live-feeds")
    });
    let line = h.status("binance");
    assert_eq!(
        parse_feed_status(&line),
        ConnectionState::Error,
        "a datahub with NO market-data plane rendered as {:?} in the Connections tool: {line:?}",
        parse_feed_status(&line)
    );

    // ...and the same rule on the other venues, whose lines this session also rewrote.
    for venue in ["okx", "polymarket"] {
        let line = h.status(venue);
        assert_ne!(
            parse_feed_status(&line),
            ConnectionState::Connected,
            "{venue} rendered Connected against a server with no plane: {line:?}"
        );
    }
}

/// The other half of the vocabulary rule, and — because the two share a fixture — the CAP ladder's
/// kill proof as well.
///
/// A venue that WANTS keys and is serving none must not read `Connected`: the `0/{wanted}` arm,
/// which used to render `0/1 stream(s) live` and be classified Connected. The fake refuses
/// everything with a CAP, the RETRYABLE half, so the specs stay wanted and `served` stays empty.
///
/// ⚠ That state is also the one that exposes a hot loop, and reaching it needed the fake to be made
/// honest first: its `MdUpdate` arm used to accept whatever it was sent, so a capped key became
/// served within a millisecond or two and the state under test evaporated. With the script
/// answering both verbs, the cap PERSISTS — which is what a real cap does — and the second
/// assertion below can measure the dial rate it produces.
#[test]
fn a_venue_wanting_keys_and_serving_none_does_not_read_connected() {
    use vike_model::feed_status::{ConnectionState, parse_feed_status};

    let fake = spawn_fake(
        md_features(),
        Box::new(|specs| ScriptedAnswer {
            accepted: Vec::new(),
            refused: specs
                .iter()
                .map(|s| (s.clone(), MdRefusal::KeyCapTotal { held: 64, cap: 64 }))
                .collect(),
            push: Vec::new(),
        }),
    );
    let mut h = Harness::new();
    h.attach(&fake);
    h.feed("bybit").subscribe_depth("BTCUSDT").expect("an advertised venue");

    until("the capped subscribe to be answered", || h.status("bybit").contains("0/1"));
    let line = h.status("bybit");
    assert_ne!(
        parse_feed_status(&line),
        ConnectionState::Connected,
        "a venue serving NOTHING rendered Connected: {line:?}"
    );

    // ⚠ **AND THE SECOND HOT-LOOP KILL PROOF.** A cap stays WANTED by design, so `diff()` never
    // empties against this server — and a control ladder reset on a successful REPLY rather than on
    // a converged SET re-sends the key with no delay at all, one fresh connection per loop
    // iteration, for ever, against the server that just said it had no room. `retry_backoff` is
    // 1 s, 2 s, 4 s…, so a correct client spends a handful of dials in this window and a broken one
    // spends thousands.
    let before = fake.accepted.load(Ordering::SeqCst);
    thread::sleep(Duration::from_millis(1_200));
    let dials = fake.accepted.load(Ordering::SeqCst) - before;
    assert!(
        dials <= 4,
        "the reconciler opened {dials} connections in 1.2 s for ONE capped key — the control \
         ladder is being reset on the reply instead of on convergence"
    );
}

/// **THE ARM THAT WAS MISSED: a venue with NOTHING subscribed.** It is not an edge case, it is the
/// steady state — `reconcile_loop` renders it through `refresh_statuses("idle")` on the
/// `desired_specs().is_empty()` branch, which every venue is in from the first frame an address
/// resolves until a Trade window or chart opens on it.
///
/// ⚠ The two tests above could not reach it: BOTH call `subscribe_depth` before asserting, so
/// `wanted >= 1` on the venue they assert about and the zero arm never executes. It read
/// `{link} — nothing subscribed on this venue`, and `subscribed` is a rung-3 keyword — so at launch
/// ALL SIX venues rendered **Connected** with no socket open at all, and after one binance ladder
/// opened the other five stayed that way for the session. Prefixing the link does not save it:
/// rung 1's `idle` test is `==`, not `contains`.
#[test]
fn a_venue_with_nothing_subscribed_never_reads_connected() {
    use vike_model::feed_status::{ConnectionState, parse_feed_status};

    let fake =
        spawn_fake(md_features(), Box::new(|specs| ScriptedAnswer::accept_all(specs, Vec::new())));
    let mut h = Harness::new();
    // No `subscribe_*` at all — exactly what the shell does before a window is opened.
    h.attach(&fake);

    until("the reconciler to render the idle state", || {
        h.status("binance").contains("no streams wanted")
    });
    assert_eq!(h.mounted().len(), 6, "the six advertised venues are mounted");
    for venue in h.mount.feed_statuses.keys() {
        let line = h.status(venue);
        assert_ne!(
            parse_feed_status(&line),
            ConnectionState::Connected,
            "{venue} rendered Connected with nothing subscribed and no socket open: {line:?}"
        );
    }
    // ...and NOTHING was subscribed to say so: a session wanting no keys opens no stream. The one
    // connection is the probe that learned the advertisement, and it sent nothing past the handshake.
    assert_eq!(
        fake.accepted.load(Ordering::SeqCst),
        1,
        "a session with an empty desired set must open no connection beyond the probe"
    );
}

/// **THE OTHER MISSED ARM: a PERMANENT refusal whose own text carries a rung-3 keyword.**
///
/// `MdRefusal::LaneUnsupported` forwards `vike_data::require_live_verb`'s words verbatim — every one
/// of which names `VenueCaps.live_data`, i.e. `live` — and the arm interpolated `{why:?}`. So a
/// venue this server permanently refused rendered **Connected**, and because that arm outranked
/// every other it kept saying so while the link was down. The fixture the suite already had used
/// `KeyCapTotal`, whose `Debug` carries no keyword, so this arm had no classifier test at all.
#[test]
fn a_lane_refusal_carrying_the_word_live_does_not_read_connected() {
    use vike_model::feed_status::{ConnectionState, parse_feed_status};

    let fake = spawn_fake(
        md_features(),
        Box::new(|specs| ScriptedAnswer {
            accepted: Vec::new(),
            refused: specs
                .iter()
                .map(|s| {
                    (
                        s.clone(),
                        // The server's REAL text for this refusal — `hub/subscribe.rs`'s `acquire` builds it
                        // as `MdRefusal::LaneUnsupported(e.to_string())` from `require_live_verb`.
                        MdRefusal::LaneUnsupported(
                            "live-data verb unsupported: venue's declared VenueCaps.live_data \
                             serves no L2 depth-snapshot lane"
                                .to_string(),
                        ),
                    )
                })
                .collect(),
            push: Vec::new(),
        }),
    );
    let mut h = Harness::new();
    h.attach(&fake);
    h.feed("binance").subscribe_depth("BTCUSDT").expect("an advertised venue");

    until("the permanent refusal to reach the status line", || {
        h.status("binance").contains("refused")
    });
    let line = h.status("binance");
    assert!(
        !line.contains("live"),
        "the server's own words were interpolated into a PARSED string: {line:?}"
    );
    assert_ne!(
        parse_feed_status(&line),
        ConnectionState::Connected,
        "a permanently refused venue rendered Connected: {line:?}"
    );
}
