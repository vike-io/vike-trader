//! Section 5: the pre-auth bounds (frame cap, handshake deadline); the composed AUTHED round trip.

use super::*;

// ---- 5. the pre-auth bounds ---------------------------------------------------------------------

/// The PRE-AUTH frame cap: a peer that declares a length above [`HANDSHAKE_MAX_FRAME_LEN`] is cut
/// off BEFORE the allocation, so it cannot make the server reserve 64 MiB per connection by sending
/// four bytes.
///
/// The length prefix is written by hand — the whole point is a declared length with no body behind
/// it, which no honest client can produce.
#[test]
fn a_pre_auth_frame_above_the_cap_is_refused_before_allocation() {
    let addr = spawn(Some(keys()));
    let mut s = TcpStream::connect(addr).expect("connect");
    let over = HANDSHAKE_MAX_FRAME_LEN + 1;
    s.write_all(&over.to_be_bytes()).expect("write the length prefix and NOTHING else");
    s.flush().expect("flush");
    s.set_read_timeout(Some(Duration::from_secs(10))).expect("read timeout");
    // The server closes without answering rather than waiting on `over` bytes that never come.
    let answer = read_frame::<_, Response>(&mut s);
    assert!(answer.is_err(), "an over-cap pre-auth frame must not be honoured: {answer:?}");

    // …and the cap leaves ample room for both REAL handshake frames, which is the other half of
    // choosing it (a cap that clipped a legitimate handshake would be a broken server, not a safe
    // one). Measured against the real encoder, not guessed.
    let mut buf: Vec<u8> = Vec::new();
    write_frame(&mut buf, &Request::Hello { proto_version: PROTO_VERSION }).expect("hello");
    write_frame(&mut buf, &Request::Auth { scope: Scope::Write, mac: vec![0u8; 32] })
        .expect("auth");
    assert!(
        buf.len() * 8 < HANDSHAKE_MAX_FRAME_LEN as usize,
        "both handshake frames are {} bytes; the cap ({HANDSHAKE_MAX_FRAME_LEN}) must keep an \
         order of magnitude of headroom over them",
        buf.len()
    );
}

/// The handshake deadline these tests run under: short enough that waiting it out costs nothing,
/// long enough that a box under load still completes a real handshake inside it.
///
/// ⚠ Production's is [`HANDSHAKE_DEADLINE`] (10 s), and these tests no longer wait it out — they
/// drive the SAME `handle_connection` through `serve_authed_with_handshake_deadline`, the server's
/// own seam, which is the `serve_with_read_ceilings` shape. What they prove is unchanged: the
/// deadline is armed before the handshake, it is the bound a silent peer is dropped at, and it is
/// replaced once `AuthOk` is written. What they no longer prove is that production passes
/// [`HANDSHAKE_DEADLINE`] rather than some other value; that is one argument at each production
/// entry, and the constant's own compile-time range assertion bounds it.
const TEST_DEADLINE: Duration = Duration::from_millis(500);

// The point of the seam is that these tests are CHEAP; a test deadline that crept up to the
// production one would make them as slow as they were, silently.
const _: () = assert!(
    TEST_DEADLINE.as_millis() * 10 <= HANDSHAKE_DEADLINE.as_millis(),
    "TEST_DEADLINE must stay an order of magnitude under production's HANDSHAKE_DEADLINE"
);

/// [`spawn`] with `deadline` in place of [`HANDSHAKE_DEADLINE`], through the server's own seam.
fn spawn_with_deadline(keys: Option<NodeKeys>, deadline: Duration) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    thread::spawn(move || {
        let _ = vike_datahub::server::serve_authed_with_handshake_deadline(
            listener, store, keys, deadline,
        );
    });
    addr
}

/// The HANDSHAKE DEADLINE: a peer that connects and says NOTHING is dropped AT the deadline, not
/// held for the 300 s idle timeout. Without it, an unauthenticated peer parks a connection thread
/// for five minutes per socket.
///
/// Both bounds, at [`TEST_DEADLINE`] rather than production's ten seconds: dropped no later than
/// ten deadlines (the pre-auth read timeout is applied — without it the server would hold the peer
/// until this test's own read timeout, twenty deadlines), and no SOONER than half of one (a server
/// that closed every unauthenticated socket at once would pass an upper bound alone).
#[test]
fn a_silent_peer_is_dropped_at_the_handshake_deadline() {
    let addr = spawn_with_deadline(Some(keys()), TEST_DEADLINE);
    let mut s = TcpStream::connect(addr).expect("connect");
    // Say nothing at all. The read below returns EOF when the server gives up on us.
    s.set_read_timeout(Some(TEST_DEADLINE * 20)).expect("read timeout");
    let started = Instant::now();
    let answer = read_frame::<_, Response>(&mut s);
    let waited = started.elapsed();
    assert!(answer.is_err(), "a silent peer must be dropped, not served: {answer:?}");
    // BOTH bounds: dropped AT the deadline — not held past it (the pre-auth timeout applied), and
    // not dropped at once (a server that closes every unauthenticated socket immediately would pass
    // an upper bound alone).
    assert!(
        waited >= TEST_DEADLINE / 2 && waited < TEST_DEADLINE * 10,
        "the silent peer was held {waited:?}, which is not the {TEST_DEADLINE:?} handshake deadline"
    );
}

/// An AUTHENTICATED connection is NOT held to the short handshake deadline: the timeout is reset to
/// the ordinary idle one once `AuthOk` is written, so a legitimately idle client is not clipped.
/// (This is the regression the deadline could easily introduce — a short timeout left armed.)
///
/// It idles FOUR deadlines, so a server that left the handshake deadline armed has closed the
/// socket three deadlines before the `Ping` is sent.
#[test]
fn an_authenticated_connection_is_not_clipped_by_the_handshake_deadline() {
    let addr = spawn_with_deadline(Some(keys()), TEST_DEADLINE);
    let mut s = authed_stream(addr, &keys(), Scope::Read);
    // Idle for well past the handshake deadline, then use the connection.
    thread::sleep(TEST_DEADLINE * 4);
    match exchange(&mut s, &Request::Ping) {
        Response::Pong => {}
        other => panic!("an idle AUTHENTICATED connection was clipped: {other:?}"),
    }
}

// ---- the composed AUTHED round trip, with real data ---------------------------------------------

/// The composed proof, in the shape of `composed_store_roundtrip.rs`: a real `DataFusionHist`
/// seeded over a temp dir, served with keys, and read back through
/// `RemoteHistStore::with_keys` — REAL bars crossing an AUTHENTICATED wire and coming back equal.
///
/// Why it belongs here rather than only in the unit tests above: everything above proves the
/// handshake in isolation over an empty `MemHistStore` (which stores no bars and can only ever
/// assert `is_empty()`). The question this answers is whether a scoped, authenticated connection
/// still CARRIES data — i.e. that the auth layer did not quietly break the thing the server is for.
#[cfg(feature = "serve-datafusion")]
mod composed {
    use super::*;
    use tempfile::TempDir;
    // `TsRange` is used ONLY by this module's reads, so it is imported HERE rather than at the
    // file head: a default (DataFusion-free) build compiles none of `composed`, and a top-level
    // import would be an unused-import warning — which is a `-D warnings` clippy failure.
    use vike_data::{DataFusionHist, TsRange};
    use vike_datahub_client::RemoteHistStore;
    use vike_model::Bar;

    const VENUE: &str = "binance";
    const SYMBOL: &str = "AUTHEDUSDT";
    const INTERVAL: &str = "1h";

    fn seeded_bars() -> Vec<Bar> {
        (0..3)
            .map(|i| Bar {
                ts: i * 3_600_000,
                open: 100.0 + i as f64,
                high: 110.0 + i as f64,
                low: 90.0 + i as f64,
                close: 105.0 + i as f64,
                volume: 10.0 + i as f64,
                funding: None,
                bid: None,
                ask: None,
                symbol: None,
            })
            .collect()
    }

    #[test]
    fn real_bars_cross_an_authenticated_wire_unchanged() {
        let dir = TempDir::new().expect("temp store root");
        let store = DataFusionHist::open(dir.path()).expect("open");
        let bars = seeded_bars();
        let n = store
            .append_bars(VENUE, SYMBOL, INTERVAL, &bars, Some("seed-authed-bars"))
            .expect("seed");
        assert_eq!(n, bars.len(), "every seeded bar lands");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let served: Arc<dyn HistStore + Send + Sync> = Arc::new(store);
        thread::spawn(move || {
            let _ = serve_authed(listener, served, None, Some(keys()), None, None, None);
        });

        // An UNAUTHENTICATED reader gets nothing — the guard that this server really is keyed.
        let bare = RemoteHistStore::new(addr.to_string());
        assert!(
            bare.load_bars(VENUE, SYMBOL, INTERVAL, TsRange { start: None, end: None }).is_err(),
            "an unauthenticated RemoteHistStore must not read from a keyed server"
        );

        // …and the OBSERVE-keyed one reads the seeded bars back, equal.
        let remote = RemoteHistStore::with_keys(addr.to_string(), keys());
        let got = remote
            .load_bars(VENUE, SYMBOL, INTERVAL, TsRange { start: None, end: None })
            .expect("authenticated read");
        assert_eq!(got, seeded_bars(), "the bars must survive the authenticated wire unchanged");

        // The catalog verbs too — the Data-Manager reads, on the same authed seam.
        let series = remote.list_series().expect("list_series");
        assert!(
            series.iter().any(|s| s.symbol == SYMBOL),
            "the seeded series must be visible over the authed connection: {series:?}"
        );
    }
}
