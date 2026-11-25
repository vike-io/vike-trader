//! One connection's loop: the handshake, the plane and scope checks, and the write of each step.

use std::io;
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use vike_data::HistoryStore;
use vike_datahub_client::proto::{
    Request, Response, request_kind, required_scope, writes_under_read_scope,
};
use vike_hub_protocol::access::{ACCESS_DENIED, AccessEntry, AccessList, Person, recheck};
use vike_hub_protocol::auth::{HubKeys, Scope, VerbScope, scope_admits};
use vike_hub_protocol::frame::{
    BodyCodec, decode_body, read_frame_raw, write_frame, write_frame_raw, write_frame_reserved,
    write_reply_as,
};

use super::range_reads::RangeReadGate;
use vike_datahub_client::PROTO_VERSION;
use vike_hub_protocol::handshake::{
    HandshakeFrame, HandshakeOutcome, HandshakeReply, already_authenticated, handshake_frame_of,
    reads_refusal_frame,
};
use vike_hub_protocol::plane::{Plane, foreign_verb, wrong_plane_message};

use crate::backfill::BackfillTable;
use crate::catalog::CatalogLane;
use crate::import::ImportLane;
use crate::market_data::MarketDataHub;
use crate::options::OptionsLane;
use crate::seed::SeedLane;
use crate::strat::StrategyLane;
use crate::tool_feeds::ToolFeedLane;

use super::features::served_features;
use super::gateway_status::GatewayStatusLane;
use super::handshake::run_handshake;
use super::limits::{ReadCeilings, ServerTimings};
use super::market_data::run_market_writer;
use super::route::dispatch;
use super::step::{Step, StopProbe};

#[cfg(doc)]
use vike_hub_protocol::limits::{SERVER_HANDSHAKE_DEADLINE, SERVER_IDLE_READ_TIMEOUT};

/// The answer to an `Auth` on a server with no hub keys: nothing verified the client's mac, and it is told so.
const AUTH_NEEDS_KEYS: &str =
    "this datahub has no hub keys configured and authenticates nothing; connect without Auth";

/// What every connection of ONE server is handed, fixed for the life of the process: the range
/// reads' ceilings ([`ReadCeilings::PRODUCTION`] from every production entry), the pre-auth
/// handshake deadline (`SERVER_HANDSHAKE_DEADLINE` from every production entry) and the two timings the
/// operator may set ([`ServerTimings::PRODUCTION`] from every entry but the one the boot calls). One
/// `Copy` value rather than three parameters, so the connection loop and the router carry it whole.
#[derive(Clone, Copy, Debug)]
pub(super) struct ConnectionLimits {
    pub(super) ceilings: ReadCeilings,
    pub(super) handshake_deadline: Duration,
    pub(super) timings: ServerTimings,
}

/// The OBSERVE lanes every connection of ONE server reads, none of which writes the store: the
/// venue catalog (`Request::VenueCatalog`), the options chain (`Request::OptionsChain`) and the tool
/// feeds (`Request::NewsFeed`, `Request::EconomicCalendar`, `Request::EquityCalendar`). Bundled so
/// the connection loop and the router carry them as ONE value — the options lane would otherwise
/// have pushed `dispatch` and `handle_request` past clippy's argument threshold. `None` in a field
/// is that lane unmounted, and its verbs answer `NotArmed`.
#[derive(Clone, Default)]
pub struct ServerLanes {
    pub catalog: Option<Arc<CatalogLane>>,
    pub options: Option<Arc<OptionsLane>>,
    pub tool_feeds: Option<Arc<ToolFeedLane>>,
    /// The STRATEGY LANE (`crate::strat`), mounted only by a datahub that holds the strategy-lane key.
    /// Unlike the three lanes above it is not an Observe surface: it is fenced by `Scope::Trade`, so it is
    /// served on a KEYED server alone (`strategy_open_verb`), and `None` answers `NO_STRATEGY_LANE`.
    pub strat: Option<Arc<StrategyLane>>,
    /// What `Request::GatewayStatus` answers from. `None` answers that this datahub holds no link.
    pub gateway_status: Option<Arc<GatewayStatusLane>>,
    /// How `Request::CredentialsChanged` reaches the gatewayhub (decision 0287). `None` answers that this datahub
    /// holds no link.
    pub credentials_relay: Option<super::gateway_status::CredentialsRelay>,
    /// The project's settings store: what `Request::GatewayLinks` reads, the user's symbol lists (`Request::SymbolSets`
    /// and its two writes) and the operator's own instrument lists. `None` answers that this datahub holds none of them.
    pub settings_store: Option<vike_secrets::CredentialStore>,
    /// `<project>/instruments`, where the venues' kept lists live (`crate::instrument_lists`). `None` (no project) keeps
    /// none and refuses a refresh in words.
    pub instruments: Option<std::path::PathBuf>,
    /// How an instrument the operator adds is looked up on its venue (`Request::OperatorInstruments`): the gatewayhub's
    /// credentialed lookup in production (`super::operator_instruments::gateway_lookup`). `None` answers that this
    /// datahub can look nothing up, and a row with stated facts is still taken.
    pub instrument_lookup: Option<super::operator_instruments::InstrumentLookup>,
    /// How a symbol's facts the store does not hold are read for a tradehub paper account
    /// (`Request::StrategySymbolFacts`): the gatewayhub's KEYLESS catalog search in production
    /// (`super::strat::keyless_lookup`). `None` answers the store's row alone, and its absence in words.
    pub keyless_lookup: Option<super::operator_instruments::InstrumentLookup>,
    /// The split watch the store shows its writes to (`crate::split_watch`): what `Request::SplitWarnings` answers
    /// from. `None` (a server whose store is not watched) answers no warning.
    pub split_watch: Option<Arc<crate::split_watch::SplitWatch>>,
    /// What the recorder holds of the rolling families it records (`Request::LiveFamilyTokens`). `None`: this datahub
    /// runs no recorder, answered in words.
    pub live_family_tokens: Option<crate::live_family_tokens::LiveFamilyTokensHeld>,
    /// The EXECUTION STREAM (`crate::execution_stream`): where a tradehub's live executions are stored and each
    /// journal's cursor kept. Its presence is what `Welcome` advertises `execution_stream` and `exec_materialize` by;
    /// `None` (no settings database to keep a cursor in) answers `crate::execution_stream::NO_EXECUTION_STREAM`.
    pub execution_stream: Option<Arc<crate::execution_stream::ExecutionStreamLane>>,
}

/// **The access list this server admits devices by** (decision 0288): one `access_key` row read by key id, afresh
/// at every call (`vike_secrets::access::find_access_key`), so a revoke or a role change needs no restart. A store
/// that exists and cannot be read admits nobody and says so at `error` (the store policy is
/// `AccessList::from_store`'s).
pub fn access_list(settings_dir: std::path::PathBuf) -> AccessList {
    let held_dir = settings_dir.clone();
    AccessList::from_store(
        move |key_id| {
            vike_secrets::access::find_access_key(&settings_dir, key_id).map(|row| {
                row.map(|r| AccessEntry {
                    person: r.person,
                    device: r.device,
                    role: r.role,
                    public_key: r.public_key,
                    revoked: r.revoked_ms.is_some(),
                })
            })
        },
        move || vike_secrets::access::access_list_held(&held_dir),
        |e| tracing::error!(error = %e, "vike-datahub: the access list is UNREADABLE — it admits nobody"),
    )
}

/// Drive one connection: read requests, answer each, until the peer closes, a read times out, or a
/// transport error ends the loop. Never panics the caller — a fault logs and returns, dropping the
/// socket.
///
/// Framing and decoding are separate (PR-2): the body bytes are read with [`read_frame_raw`] and
/// then decoded, so a well-framed but undecodable request is answered with [`Response::Error`] and
/// the loop CONTINUES — the connection survives a bad request.
///
/// **Each request is answered in the form it came in** (`vike_hub_protocol::frame::BodyCodec`: JSON, msgpack, or msgpack in
/// LZ4 for a thin link; `FEATURE_WIRE_MSGPACK`), and so is every frame of a stream it opens: the form is the
/// CLIENT's choice, so this loop keeps none of its own and an old client is answered JSON as it always was. A read timeout (an idle/half-open
/// peer) or any other read fault CLOSES the connection; a timeout is never recovered into a resumed
/// loop, which would desync the stream.
///
/// `facts.handshake_deadline` is the deadline of the whole pre-auth phase on a KEYED server —
/// [`SERVER_HANDSHAKE_DEADLINE`] from every production entry. `facts.timings.idle_read_timeout` is the read timeout of every frame
/// after it ([`SERVER_IDLE_READ_TIMEOUT`] unless the box wrote a row), and `facts.timings.market_data_heartbeat`
/// is the period handed to the market-data writer.
pub(super) fn handle_connection(
    mut stream: TcpStream,
    store: Arc<dyn HistoryStore + Send + Sync>,
    backfill: Option<Arc<BackfillTable>>,
    keys: Option<&HubKeys>,
    market_data: Option<Arc<MarketDataHub>>,
    seed: Option<Arc<SeedLane>>,
    lanes: ServerLanes,
    import: Option<Arc<ImportLane>>,
    facts: ConnectionLimits,
    range_reads: &'static RangeReadGate,
) {
    let ConnectionLimits { handshake_deadline, timings, .. } = facts;
    let peer = stream.peer_addr().ok();
    tracing::info!(?peer, "vike-datahub: connection opened");
    // Nagle OFF before the first frame, on every connection — positional and, after a mode switch,
    // the market-data stream alike (`run_market_writer` inherits it). Why, and why a refusal is
    // only logged: `vike_hub_protocol::frame::configure_hub_stream`.
    if let Err(e) = vike_hub_protocol::frame::configure_hub_stream(&stream) {
        tracing::warn!(?peer, error = %e, "vike-datahub: TCP_NODELAY refused; frames may wait for an ACK");
    }

    let features = served_features(
        backfill.is_some(),
        backfill.as_deref().is_some_and(BackfillTable::has_funding),
        backfill.as_deref().is_some_and(BackfillTable::has_option_trades),
        backfill.as_deref().is_some_and(BackfillTable::has_market_rules),
        keys.is_some(),
        market_data.as_deref(),
        seed.is_some(),
        lanes.catalog.is_some(),
        import.as_deref(),
        lanes.execution_stream.is_some(),
        lanes.strat.as_deref(),
        lanes.instruments.is_some(),
    );

    // On a KEYED server the PRE-AUTH phase runs first, under a far shorter deadline than the idle
    // timeout below: an unauthenticated peer has nothing to think about, so a handshake that has not
    // completed in seconds is a socket somebody opened and said nothing on.
    //
    // `None` — the key-less server — takes NEITHER branch: no handshake, no deadline, no scope. That
    // is what makes the key-less path byte-identical to the pre-auth protocol rather than merely
    // similar to it.
    // `person`: who, when a device key signed (decision 0288); its row is re-read before every write. `key_id`: the
    // non-secret fingerprint of the key that signed, which an execution stream's cursor row records, and the CLIENT
    // this connection's range reads are counted as (`RangeReadGate`).
    let (authed, person, key_id): (Option<Scope>, Option<Person>, Option<String>) = match keys {
        Some(keys) => {
            if let Err(e) = stream.set_read_timeout(Some(handshake_deadline)) {
                tracing::warn!(?peer, error = %e, "vike-datahub: could not set handshake deadline, closing connection");
                return;
            }
            match run_handshake(&mut stream, keys, &features, peer, handshake_deadline) {
                (HandshakeOutcome::Authed { scope, person, key_id }, _) => {
                    (Some(scope), person, key_id)
                }
                (HandshakeOutcome::Closed, why) => {
                    tracing::info!(
                        ?peer,
                        why = why.as_str(),
                        "vike-datahub: connection closed (handshake refused)"
                    );
                    return;
                }
            }
        }
        None => (None, None, None),
    };
    // Who this connection's range reads count against: its key, or on a key-less server its peer's address.
    let client =
        key_id.clone().unwrap_or_else(|| peer.map_or_else(String::new, |p| p.ip().to_string()));

    // Bound how long a single read may block, so a half-open peer cannot pin this thread forever. If
    // even setting the timeout fails, close rather than risk an unbounded park. On the keyed path
    // this also RESETS the short handshake deadline — an authenticated client may legitimately idle
    // between requests, exactly like an unauthenticated one always could.
    if let Err(e) = stream.set_read_timeout(Some(timings.idle_read_timeout)) {
        tracing::warn!(?peer, error = %e, "vike-datahub: could not set read timeout, closing connection");
        return;
    }

    // The per-write re-check (decision 0288 item 4): whether the device that signed this connection is still
    // admitted at `scope`. A shared key, or a key-less server, has no row and passes.
    let still_admitted = |scope: Scope, verb: &str| {
        recheck(keys.and_then(HubKeys::access), person.as_ref(), scope)
            .inspect_err(|why| {
                tracing::warn!(
                    ?peer, person = ?person, why = why.as_str(), verb,
                    "vike-datahub: a write REFUSED — the device's access was re-read"
                );
            })
            .is_ok()
    };

    // The client said it reads the refusal frame (`HandshakeFrame::Reads`): until then, an older client's
    // answers are the ones it always read.
    let mut reads_frame = false;
    loop {
        // Read the next frame's BODY BYTES only — decode is deliberately separate (below).
        let body = match read_frame_raw(&mut stream) {
            Ok(body) => body,
            Err(e) => {
                match e.kind() {
                    // The normal way a connection ends — no log.
                    io::ErrorKind::UnexpectedEof => {}
                    // No (further) bytes arrived within the window: an idle or half-open connection.
                    // Close it to free the thread; a live client simply reconnects for its next
                    // request. We never RESUME after a timeout — a mid-frame timeout would otherwise
                    // leave the stream desynced.
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => {
                        tracing::info!(
                            ?peer,
                            "vike-datahub: idle read timeout, closing connection"
                        );
                    }
                    _ => {
                        tracing::warn!(?peer, error = %e, "vike-datahub: read fault, closing connection");
                    }
                }
                break;
            }
        };
        // The form this request came in, which its answer (and a stream it opens) is written in. A body of no
        // form is answered JSON, which every client reads.
        let codec = BodyCodec::of(&body);

        // A HANDSHAKE frame (`vike_hub_protocol::handshake`'s; no verb of this enum) after the handshake: an
        // authenticated connection refuses it by name and is kept. A KEY-LESS server (a test's: production is always
        // keyed) runs no handshake, so it answers `Hello` with its `Welcome`, which carries no nonce and no `auth`
        // (`Hello` informs there, it does not gate), and an answer to a challenge it never made with the refusal that
        // says so. JSON only (`handshake_frame_of`): a verb's msgpack or LZ4 body is decoded once, below.
        if let Some(frame) = handshake_frame_of(&body) {
            // What the client reads (the refusal frame): recorded, never answered, keyed or not.
            if let Some(reads) = reads_refusal_frame(&frame) {
                reads_frame = reads;
                continue;
            }
            let written = match (authed, frame) {
                (Some(scope), frame) => {
                    let kind = frame.kind();
                    tracing::info!(
                        ?peer,
                        ?scope,
                        verb = kind,
                        "vike-datahub: a {kind} refused — this connection is already authenticated and \
                         its scope is never re-negotiated"
                    );
                    write_reply_as(
                        &mut stream,
                        codec,
                        &Response::Error(already_authenticated(&frame)),
                    )
                }
                (None, HandshakeFrame::Hello { .. }) => write_reply_as(
                    &mut stream,
                    codec,
                    &HandshakeReply::Welcome {
                        proto_version: PROTO_VERSION,
                        features: features.clone(),
                        nonce: None,
                    },
                ),
                (None, _) => write_reply_as(
                    &mut stream,
                    codec,
                    &HandshakeReply::AuthDenied { reason: AUTH_NEEDS_KEYS.to_string() },
                ),
            };
            if let Err(e) = written {
                write_fault(peer, &e, None);
                break;
            }
            continue;
        }

        // THIS REQUEST'S STOP PROBE, over a SHARED borrow of the stream: from here until `dispatch`
        // returns, this loop does not read the socket, so a peek is the only thing looking at it.
        // Only `backfill_verb` and the archive import ever ask it — every other verb leaves it
        // unasked, which costs them nothing and changes nothing. See `StopProbe`.
        let probe = StopProbe::new(&stream, peer);

        // Decode the body. A well-framed body that is not a known `Request` (an unknown/incompatible
        // verb from a mismatched client) is answered with `Response::Error` and the loop CONTINUES —
        // one bad request never drops the connection.
        //
        // ⚠ **THE STEP IS A `ControlFlow`-SHAPED VALUE RATHER THAN A `Response`, and §8 item 10's
        // "ONE arm" understates it.** `MarketDataSubscribe` is this wire's one MODE SWITCH: it must run
        // AFTER the scope check on BOTH the keyed and the key-less arms, and then RETURN without
        // falling through to the unconditional write below — because from that point the socket
        // belongs to `run_market_writer` and the reply it writes is the LAST positional frame.
        // An ADMITTED range read's place in this server's gate (`RangeReadGate`): taken before its store
        // read, held until its reply is written (an LZ4 reply gives it back before its write), dropped at the
        // end of this turn of the loop, a write fault and an unwind included.
        let mut range_place = None;
        let step = match decode_body::<Request>(&body) {
            Ok(request) => match authed {
                // KEYED: check the verb against the connection's authenticated ceiling BEFORE it
                // reaches `handle_request`. A refusal is answered and the loop CONTINUES — an
                // Observe client asking for a Control verb has made a bad *request*, not opened a
                // bad *connection*, and dropping it would make an over-scoped read indistinguishable
                // from a transport fault. (An UNAUTHENTICATED verb is a different matter and is
                // refused with a closed socket, in `run_handshake`.)
                Some(scope) => {
                    let needed = required_scope(&request);
                    let admitted = scope_admits(scope, needed);
                    // An admitted WRITE re-reads the device's row (decision 0288 item 4), a write on the
                    // Read side under the scope it needs; a read never does.
                    let write = match needed {
                        VerbScope::Write => Some(Scope::Write),
                        VerbScope::Read if writes_under_read_scope(&request) => Some(Scope::Read),
                        _ => None,
                    };
                    let revoked = admitted
                        && write.is_some_and(|w| !still_admitted(w, request_kind(&request)));
                    if revoked {
                        Step::reply(Response::Error(ACCESS_DENIED.to_string()))
                    } else if admitted {
                        range_place = range_reads.admit(&request, &client);
                        dispatch(
                            request,
                            &store,
                            backfill.as_deref(),
                            true,
                            market_data.as_ref(),
                            seed.as_deref(),
                            &lanes,
                            import.as_deref(),
                            facts,
                            &probe,
                            codec,
                        )
                    } else {
                        let kind = request_kind(&request);
                        tracing::info!(
                            ?peer,
                            ?scope,
                            ?needed,
                            verb = kind,
                            "vike-datahub: verb refused — outside this connection's scope"
                        );
                        Step::reply(Response::Error(match needed {
                            // The strategy lane's own scope, refused to a key that is not its key:
                            // the lane is not a superset of anything and nothing else admits it.
                            VerbScope::Trade => format!(
                                "{kind} requires the Trade scope (the strategy lane's own key, \
                                 VIKE_DATAHUB_TRADE_KEY); this connection authenticated as {}",
                                scope.name()
                            ),
                            // A `Trade` connection that sent a store verb: it reaches the lane and
                            // nothing else.
                            _ if scope == Scope::Trade => format!(
                                "{kind} is outside the Trade scope: a tradehub's connection reaches \
                                 StrategyOpen, StrategyFundingHistory, StrategyInputBars, \
                                 StrategySymbolFacts, StrategyForecasts and ExecutionStreamOpen \
                                 and nothing else on this server"
                            ),
                            // ⚠ The `Run*` verbs are NOT named here any more — they are not served
                            // by this daemon at all since ruling 7, and the guard above answers
                            // them before this arm is reached. `Backfill`, `ImportArchive` and
                            // `DeleteSeries` are what is left behind the Control scope on the data
                            // plane (`ImportArchive` since `docs/decisions/0100`'s verdict 1).
                            _ => format!(
                                "{kind} requires the Control scope; this connection authenticated \
                                 as Observe. The Backfill store WRITE, the ImportArchive store \
                                 WRITE and the DeleteSeries store REMOVAL are Control-only"
                            ),
                        }))
                    }
                }
                // KEY-LESS: no scope to check, and unchanged for every verb that predates the
                // keys. ⚠ NOT "every verb", which is what this comment claimed until 2026-09-07:
                // the `false` below IS the `keyed` argument `delete_series_verb` refuses on, so
                // the code this line annotates is what makes the old claim false.
                None => {
                    range_place = range_reads.admit(&request, &client);
                    dispatch(
                        request,
                        &store,
                        backfill.as_deref(),
                        false,
                        market_data.as_ref(),
                        seed.as_deref(),
                        &lanes,
                        import.as_deref(),
                        facts,
                        &probe,
                        codec,
                    )
                }
            },
            // ⚠ A BACKTEST VERB DOES NOT DECODE HERE, and the operator who dialled the wrong port is
            // still owed the address (`vike_hub_protocol::plane`'s module doc). Answered on this arm, it
            // also comes BEFORE the scope check, and the order is the whole point of it (ruling 7). A
            // verb this daemon does not serve AT ALL is not a scope question: an Observe connection
            // sending `RunBacktest` would otherwise be told "that requires the Control scope", which is
            // true of the OTHER daemon and useless here — it sends the reader looking for a key when
            // what they need is a different address.
            //
            // It applies on BOTH auth arms (key-less and keyed) because it is a property of this
            // build's served surface, not of the connection.
            Err(e) => match foreign_verb(&body, Plane::Data) {
                Some(kind) => {
                    tracing::info!(
                        ?peer,
                        verb = kind,
                        "vike-datahub: verb refused — it moved to the backtesthub"
                    );
                    Step::reply(Response::Error(wrong_plane_message(
                        kind,
                        Plane::Data,
                        Plane::Backtest,
                    )))
                }
                None => {
                    Step::reply(Response::Error(format!("unrecognized/undecodable request: {e}")))
                }
            },
        };
        // Read before the stream is borrowed mutably to write: the probe's borrow ends here.
        let mode_lost = probe.mode_lost();
        // A range reply is written under an IDLE write bound (`RANGE_REPLY_WRITE_TIMEOUT`): a peer that stops
        // reading, or a tunnel that stalls, closes this connection and releases the read's place instead of
        // holding it for good, and a slow link that still drains is never cut. Disarmed after the reply.
        let range_bound = range_place.is_some().then_some(timings.range_reply_write_timeout);
        if let Some(bound) = range_bound
            && let Err(e) = stream.set_write_timeout(Some(bound))
        {
            tracing::warn!(?peer, error = %e, "vike-datahub: could not bound a range reply's write, closing connection");
            break;
        }

        match step {
            Step::Reply(response) => {
                // A refusal frame to a client that does not read one goes as the `Error` it always read.
                let response = match response.older_form(reads_frame) {
                    Some(older) => Box::new(older),
                    None => response,
                };
                let written = match write_reply_as(&mut stream, codec, &*response) {
                    // A range reply is fitted to the frame by its JSON length, and msgpack can be longer (a short
                    // decimal is 3 bytes of JSON and 9 of msgpack): one msgpack refuses before its first byte goes
                    // as JSON, which fits by construction and which every client reads.
                    Err(e)
                        if e.kind() == io::ErrorKind::InvalidData && codec != BodyCodec::Json =>
                    {
                        write_frame(&mut stream, &*response)
                    }
                    written => written,
                };
                if let Err(e) = written {
                    write_fault(peer, &e, range_bound);
                    break;
                }
                // A peek that could not put the socket back into blocking mode leaves it in a mode
                // nobody can vouch for: the reply is written, and then the connection closes rather
                // than reading the next frame from a socket that may answer `WouldBlock` mid-frame.
                if mode_lost {
                    tracing::warn!(
                        ?peer,
                        "vike-datahub: a backfill's peer check could not restore blocking mode on \
                         this socket; closing the connection after its reply"
                    );
                    break;
                }
            }
            // The columns: bytes, not a value. No peer check runs for a range read, so there is no `mode_lost`
            // to honour here. A plain page was encoded in ONE pass straight into its frame (four bytes reserved
            // for the prefix, so the body is never copied), its rows dropped before the write; it goes in one
            // write. An LZ4 body was built whole, and its rows are gone, so its place goes back BEFORE its write: a
            // thin link's slow write then holds no place another reader needs.
            Step::Columns(frame) => {
                if let Err(e) = write_frame_reserved(&mut stream, frame) {
                    write_fault(peer, &e, range_bound);
                    break;
                }
            }
            Step::Bytes(body) => {
                drop(range_place.take());
                if let Err(e) = write_frame_raw(&mut stream, &body) {
                    write_fault(peer, &e, range_bound);
                    break;
                }
            }
            // THE CLOSE. The request ran for a client that has gone; nobody would read a reply, so
            // none is written — which is also what keeps a misleading "write fault" out of the log.
            // `backfill_verb` has logged the request's own line already.
            Step::Close => break,
            // ⚠ THE MODE SWITCH. Everything after this point on this socket is a pushed
            // `Response::MarketData`, and nothing reads this direction again.
            Step::ModeSwitch(guard, specs) => {
                let hub = market_data.expect("ModeSwitch is produced only where a hub is mounted");
                let heartbeat = timings.market_data_heartbeat;
                run_market_writer(stream, hub, guard, specs, peer, heartbeat, codec);
                return;
            }
            // ⚠ THE OTHER MODE SWITCH: from here this socket speaks the tube (`StreamUp` in,
            // `StreamDown` out) and nothing reads it positionally again.
            Step::StrategySwitch(guard) => {
                let hub = lanes
                    .strat
                    .as_ref()
                    .expect("StratSwitch is produced only where a strategy lane is mounted");
                crate::strat::serve(stream, hub, guard, peer, codec, reads_frame);
                return;
            }
            // ⚠ THE THIRD MODE SWITCH, which may still be refused: a refused open was answered positionally and the
            // loop CONTINUES; an answered one ran its session on this socket, which nothing reads positionally again.
            Step::ExecutionStream(opening) => {
                let lane = lanes
                    .execution_stream
                    .as_ref()
                    .expect("ExecutionStream is produced only where the lane is mounted");
                let end = crate::execution_stream::serve(
                    &mut stream,
                    lane,
                    store.as_ref(),
                    *opening,
                    key_id.as_deref(),
                    peer,
                    timings.idle_read_timeout,
                    codec,
                );
                if end == crate::execution_stream::ExecutionStreamEnd::Closed {
                    return;
                }
            }
            // A user's CSV upload: its frames, then back to this loop (or a close on a fault). Each chunk
            // and the end re-read the device's row, as the upload's opening did above.
            Step::Upload(spec) => {
                let admitted = |verb: &str| still_admitted(Scope::Read, verb);
                if !super::upload::serve(
                    &mut stream,
                    import.as_deref(),
                    *spec,
                    peer,
                    &admitted,
                    codec,
                    reads_frame,
                ) {
                    break;
                }
            }
        }
        // The range reply is written: its write bound comes off (no other reply on this wire had one), and
        // `range_place` drops with this turn.
        if range_bound.is_some()
            && let Err(e) = stream.set_write_timeout(None)
        {
            tracing::warn!(?peer, error = %e, "vike-datahub: could not clear a range reply's write bound, closing connection");
            break;
        }
    }
    tracing::info!(?peer, "vike-datahub: connection closed");
}

/// The log line of a reply write that failed, after which the connection closes. A RANGE reply whose
/// write made no progress for its idle `bound` says so in words: the peer stopped reading (or its link
/// stalled), and closing releases the read's place for the readers waiting on it.
fn write_fault(peer: Option<std::net::SocketAddr>, e: &io::Error, bound: Option<Duration>) {
    let stalled = matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut);
    match bound {
        Some(bound) if stalled => tracing::warn!(
            ?peer,
            ?bound,
            "vike-datahub: a range reply's peer took no byte for its write bound (it stopped reading, or \n             its link stalled); closing the connection and releasing its read's place"
        ),
        _ => tracing::warn!(?peer, error = %e, "vike-datahub: write fault, closing connection"),
    }
}
