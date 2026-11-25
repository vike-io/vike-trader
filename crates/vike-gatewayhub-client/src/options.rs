//! The client side of the gatewayhub's Deribit options surface — what the datahub's options lane
//! calls instead of linking the venue crate.
//!
//! The split is decision D7: **the datahub keeps the poll thread, the folds, the strike window and the
//! chain recorder; the gatewayhub serves the fetches and the two feeds.** So this module is two
//! things:
//!
//! * [`GatewayOptions`] — the REST half, mirroring `DeribitOptionsProvider`'s methods one for one
//!   (`list_underlyings`, `list_expiries`, `fetch_chain`, and the venue's listing with each
//!   underlying's index: [`GatewayOptions::listing`]) plus the option-contract index
//!   (`public/get_instruments`). One call is one dial, and the daemon keeps ONE persistent provider
//!   behind them, so its 5 s book cache survives across calls and across clients. Errors are the
//!   provider's own `String`s with a transport failure as `gatewayhub <addr>: ...`; a failure is
//!   never an empty answer.
//! * [`FeedOptionsGateway`] — the two push feeds (`markprice.options` and `ticker`), mirroring
//!   `spawn_deribit_feed_options_mark` / `spawn_deribit_feed_options_ticker`: rows are handed to a
//!   callback on the feed's own named thread, a lost socket is re-opened with the same focus set
//!   (reconnect == resubscribe, as the in-process feeds do), and `shutdown` stops and joins. The focus
//!   set is FIXED for the life of the feed; the datahub starts each feed once.

use std::io;
use std::net::{Shutdown, TcpStream};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};
use std::thread::{JoinHandle, ThreadId};
use std::time::{Duration, Instant};

use vike_hub_protocol::auth::Scope;
use vike_hub_protocol::call::HubCallError;
use vike_market_stream::frame::{is_timeout, read_capped};
use vike_market_stream::opener::stream_deadline;
use vike_marketdata::{OptionChain, OptionChainExpiry, OptionTicker};
use vike_model::Instrument;
use vike_venues::reconnect::ReconnectBackoff;

use crate::client::{
    FeedOptionsStream, FetchError, GatewayClient, GatewayLink, gateway_text, gateway_words,
    unexpected,
};
use crate::proto::{
    FEATURE_GATEWAY_OPTIONS, FEATURE_GATEWAY_OPTIONS_LISTING, FetchWhat,
    GATEWAY_DOWN_FRAME_CEILING_BYTES, OptionIndexKind, OptionsCall, OptionsFeedDown,
    OptionsFeedSpec, OptionsListing, OptionsReply, Request, Response,
};

/// The REST half of the options surface. Cheap to clone; every call dials its own connection.
#[derive(Debug, Clone)]
pub struct GatewayOptions {
    link: GatewayLink,
}

impl GatewayOptions {
    /// An options client over `link`.
    pub fn new(link: GatewayLink) -> Self {
        Self { link }
    }

    fn text(&self, e: &HubCallError) -> String {
        gateway_text(self.link.addr(), e)
    }

    fn words(&self, words: &dyn std::fmt::Display) -> String {
        gateway_words(self.link.addr(), words)
    }

    fn rest(&self, call: OptionsCall) -> Result<OptionsReply, String> {
        let mut client =
            GatewayClient::connect(&self.link, Scope::Read).map_err(|e| self.text(&e))?;
        client.require_feature(FEATURE_GATEWAY_OPTIONS).map_err(|e| self.text(&e))?;
        if matches!(call, OptionsCall::Listing { .. }) {
            client.require_feature(FEATURE_GATEWAY_OPTIONS_LISTING).map_err(|e| self.text(&e))?;
        }
        match client.exchange(&Request::OptionsRest { call }).map_err(|e| self.text(&e))? {
            Response::OptionsReplied(reply) => Ok(reply),
            // The provider's own failure text, unchanged.
            Response::Error(text) => Err(text),
            other => Err(self.text(&unexpected(client.addr(), "OptionsReplied", &other))),
        }
    }

    /// `DeribitOptionsProvider::list_underlyings`, over the wire (the served set, in the provider's
    /// order).
    pub fn list_underlyings(&self) -> Result<Vec<String>, String> {
        match self.rest(OptionsCall::Underlyings)? {
            OptionsReply::Underlyings(v) => Ok(v),
            other => Err(self.words(&format!("expected Underlyings, got {}", reply_kind(&other)))),
        }
    }

    /// The venue's option LISTING, over the wire: the venue the daemon serves options of, and every
    /// underlying it lists with its `markprice.options` index, in the listing's order. `refresh` asks the
    /// daemon to read it from the venue now (it answers the listing it holds when it read one within
    /// the last minute). An older daemon, which lacks `gateway_options_listing`, is refused HERE, naming the
    /// string; a daemon that cannot read the listing answers its own words, never an empty one.
    pub fn listing(&self, refresh: bool) -> Result<OptionsListing, String> {
        match self.rest(OptionsCall::Listing { refresh })? {
            OptionsReply::Listing(listing) => Ok(listing),
            other => Err(self.words(&format!("expected Listing, got {}", reply_kind(&other)))),
        }
    }

    /// `DeribitOptionsProvider::list_expiries(underlying)`, over the wire.
    pub fn list_expiries(&self, underlying: &str) -> Result<Vec<OptionChainExpiry>, String> {
        match self.rest(OptionsCall::Expiries { underlying: underlying.to_string() })? {
            OptionsReply::Expiries(v) => Ok(v),
            other => Err(self.words(&format!("expected Expiries, got {}", reply_kind(&other)))),
        }
    }

    /// `DeribitOptionsProvider::fetch_chain(underlying, expiry_iso, strikes, r)`, over the wire.
    /// `strikes: None` is the FULL chain — what the datahub's chain recorder records before it
    /// applies its own display window.
    pub fn fetch_chain(
        &self,
        underlying: &str,
        expiry_iso: &str,
        strikes: Option<usize>,
        r: f64,
    ) -> Result<OptionChain, String> {
        let strike_window = match strikes {
            None => None,
            Some(n) => Some(
                u32::try_from(n)
                    .map_err(|_| format!("a strike window of {n} does not fit the wire"))?,
            ),
        };
        let call = OptionsCall::Chain {
            underlying: underlying.to_string(),
            expiry: expiry_iso.to_string(),
            strike_window,
            r,
        };
        match self.rest(call)? {
            OptionsReply::Chain(chain) => Ok(*chain),
            other => Err(self.words(&format!("expected Chain, got {}", reply_kind(&other)))),
        }
    }

    /// Deribit's `public/get_instruments` for `currency=any` and one `kind`, over the wire — a paged
    /// fetch, so a listing past the frame ceiling reassembles whole.
    pub fn instruments(&self, kind: OptionIndexKind) -> Result<Vec<Instrument>, String> {
        let mut client =
            GatewayClient::connect(&self.link, Scope::Read).map_err(|e| self.text(&e))?;
        let reply = client
            .fetch(&FetchWhat::OptionIndex { kind }, false, None)
            .map_err(FetchError::into_text)?;
        reply.assembler.finish_instruments(reply.pages, reply.rows).map_err(|why| self.words(&why))
    }

    /// The instruments of the option index: options, then futures, concatenated — the datahub's
    /// `fetch_option_index` input. **Fails as a whole when either call fails**, so the caller keeps
    /// its last-good index rather than half of one.
    pub fn option_index_instruments(&self) -> Result<Vec<Instrument>, String> {
        let mut all = Vec::new();
        for kind in [OptionIndexKind::Option, OptionIndexKind::Future] {
            all.extend(self.instruments(kind)?);
        }
        Ok(all)
    }
}

/// An options reply's variant name, for a desync message — never its payload.
fn reply_kind(reply: &OptionsReply) -> &'static str {
    match reply {
        OptionsReply::Underlyings(_) => "Underlyings",
        OptionsReply::Expiries(_) => "Expiries",
        OptionsReply::Chain(_) => "Chain",
        OptionsReply::Listing(_) => "Listing",
    }
}

// ---- The push feeds ------------------------------------------------------------------------------

/// A sleep that a stop interrupts.
struct StopSignal {
    stopping: Mutex<bool>,
    changed: Condvar,
}

impl StopSignal {
    fn new() -> Self {
        Self { stopping: Mutex::new(false), changed: Condvar::new() }
    }

    fn stop(&self) {
        *self.stopping.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.changed.notify_all();
    }

    fn is_stopped(&self) -> bool {
        *self.stopping.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Sleep `d`, waking early on a stop; `true` when stopped.
    fn sleep(&self, d: Duration) -> bool {
        let guard = self.stopping.lock().unwrap_or_else(PoisonError::into_inner);
        let (guard, _) = self
            .changed
            .wait_timeout_while(guard, d, |stopped| !*stopped)
            .unwrap_or_else(PoisonError::into_inner);
        *guard
    }
}

struct FeedShared {
    stop: StopSignal,
    /// The feed thread, set when it starts: a callback runs on it, and a `shutdown` made from there
    /// cannot join itself.
    thread: OnceLock<ThreadId>,
    /// The current socket, so a stop can break a reader parked in `read`.
    socket: Mutex<Option<TcpStream>>,
}

/// One running options feed. [`FeedOptionsGateway::shutdown`] (or dropping it) stops the reader and
/// joins its thread — nothing is ever detached.
pub struct FeedOptionsGateway {
    shared: Arc<FeedShared>,
    handle: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for FeedOptionsGateway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FeedOptionsGateway").finish_non_exhaustive()
    }
}

impl FeedOptionsGateway {
    /// The `markprice.options` feed over `indexes`: each decoded batch (mark + implied vol only) goes
    /// to `on_rows` on the feed's own thread. Mirrors `spawn_deribit_feed_options_mark`. Never
    /// dials on the caller's thread; the only `Err` is the OS refusing a thread.
    pub fn markprice(
        link: GatewayLink,
        indexes: Vec<String>,
        mut on_rows: impl FnMut(Vec<OptionTicker>) + Send + 'static,
    ) -> io::Result<Self> {
        Self::spawn(link, OptionsFeedSpec::MarkPrice { indexes }, "gw-opt-marks", move |frame| {
            match frame {
                OptionsFeedDown::Marks(rows) => {
                    on_rows(rows);
                    Ok(())
                }
                other => Err(format!("a {other:?} frame on a markprice feed")),
            }
        })
    }

    /// The per-instrument `ticker` feed over the focus set `instruments`: each decoded row goes to
    /// `on_row` on the feed's own thread. Mirrors `spawn_deribit_feed_options_ticker`.
    pub fn ticker(
        link: GatewayLink,
        instruments: Vec<String>,
        mut on_row: impl FnMut(OptionTicker) + Send + 'static,
    ) -> io::Result<Self> {
        Self::spawn(link, OptionsFeedSpec::Ticker { instruments }, "gw-opt-ticker", move |frame| {
            match frame {
                OptionsFeedDown::Ticker(row) => {
                    on_row(*row);
                    Ok(())
                }
                other => Err(format!("a {other:?} frame on a ticker feed")),
            }
        })
    }

    fn spawn(
        link: GatewayLink,
        spec: OptionsFeedSpec,
        name: &str,
        mut on_frame: impl FnMut(OptionsFeedDown) -> Result<(), String> + Send + 'static,
    ) -> io::Result<Self> {
        let shared = Arc::new(FeedShared {
            stop: StopSignal::new(),
            thread: OnceLock::new(),
            socket: Mutex::new(None),
        });
        let for_thread = Arc::clone(&shared);
        let handle = std::thread::Builder::new().name(name.to_string()).spawn(move || {
            let _ = for_thread.thread.set(std::thread::current().id());
            let t = link.timings();
            // The one reconnect ladder, started over by a healthy session (the reader's own waits:
            // `crates/vike-market-stream/src/opener_tests.rs`'s `the_hub_links_waits_are_pinned_verbatim`).
            let mut backoff =
                ReconnectBackoff::new(t.stream.reconnect_base, t.stream.reconnect_max);
            let mut last_reason = String::new();
            while !for_thread.stop.is_stopped() {
                let began = Instant::now();
                // The socket is registered the moment it is dialled, so a stop can break a handshake or an
                // open reply parked on a daemon that went quiet instead of waiting out its deadline.
                let observe = |stream: &TcpStream| {
                    *for_thread.socket.lock().unwrap_or_else(PoisonError::into_inner) =
                        stream.try_clone().ok();
                    if for_thread.stop.is_stopped() {
                        let _ = stream.shutdown(Shutdown::Both);
                    }
                };
                let why = match GatewayClient::connect_observed(&link, Scope::Read, observe)
                    .and_then(|c| c.open_options_feed(&spec))
                {
                    Ok(feed) => read_feed(&for_thread, feed, &t, &mut on_frame),
                    Err(e) => gateway_text(link.addr(), &e),
                };
                if for_thread.stop.is_stopped() {
                    break;
                }
                if began.elapsed() >= Duration::from_secs(10) {
                    backoff.reset();
                }
                let delay = backoff.failed();
                // `warn` when the reason CHANGES (a refused feature or a missing key is a configuration
                // fault), `debug` for the same reason on every retry.
                if why == last_reason {
                    tracing::debug!(reason = %why, retry_in = ?delay, "gatewayhub options feed: not connected");
                } else {
                    tracing::warn!(reason = %why, retry_in = ?delay, "gatewayhub options feed: not connected");
                    last_reason = why;
                }
                if for_thread.stop.sleep(delay) {
                    break;
                }
            }
        })?;
        Ok(Self { shared, handle: Some(handle) })
    }

    /// Stop the feed and join its thread. Idempotent.
    pub fn shutdown(&mut self) {
        let Some(handle) = self.handle.take() else { return };
        self.shared.stop.stop();
        if let Some(socket) =
            self.shared.socket.lock().unwrap_or_else(PoisonError::into_inner).as_ref()
        {
            let _ = socket.shutdown(Shutdown::Both);
        }
        if self.shared.thread.get() == Some(&std::thread::current().id()) {
            // Called from inside a row callback: this thread IS the feed thread and cannot join
            // itself. The stop is raised and the socket broken; the thread ends when the callback
            // returns (the one case where a handle is dropped unjoined).
            drop(handle);
            return;
        }
        let _ = handle.join();
    }
}

impl Drop for FeedOptionsGateway {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Read one opened feed until it ends; the reason it ended.
fn read_feed(
    shared: &FeedShared,
    feed: FeedOptionsStream,
    t: &crate::client::GatewayTimings,
    on_frame: &mut impl FnMut(OptionsFeedDown) -> Result<(), String>,
) -> String {
    let FeedOptionsStream { mut stream, heartbeat, .. } = feed;
    let deadline = stream_deadline(t.stream.stream_read_floor, heartbeat);
    if let Err(e) = stream.set_read_timeout(Some(deadline)) {
        return e.to_string();
    }
    // The socket was registered when it was dialled; a stop that raced that must not wait out a read.
    if shared.stop.is_stopped() {
        *shared.socket.lock().unwrap_or_else(PoisonError::into_inner) = None;
        return "stopped".to_string();
    }
    let why = loop {
        match read_capped::<_, OptionsFeedDown>(&mut stream, GATEWAY_DOWN_FRAME_CEILING_BYTES) {
            Ok(OptionsFeedDown::Heartbeat) => {}
            Ok(OptionsFeedDown::Bye(reason)) => {
                break format!("the daemon ended the feed ({reason:?})");
            }
            Ok(frame) => {
                if let Err(why) = on_frame(frame) {
                    break why;
                }
            }
            Err(e) if is_timeout(&e) => {
                break format!(
                    "no frame for {deadline:?} — the link is dead or the daemon is wedged"
                );
            }
            Err(e) => break e.to_string(),
        }
    };
    *shared.socket.lock().unwrap_or_else(PoisonError::into_inner) = None;
    why
}
