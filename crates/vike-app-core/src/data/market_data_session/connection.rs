//! `MarketDataSession`'s background threads: the reader, the reconciler and the bounded dial helpers.

use super::*;
use vike_model::StreamLabel;
use vike_utility::thread::ThreadNoAnswer;

// ── The reader thread ───────────────────────────────────────────────────────────────────────────

/// The reader's OWN state, which lives for exactly one connection and is shared with nothing: the
/// per-key wire sequence, and which keys have had a tape hole DISCLOSED on this connection.
///
/// It is a struct rather than two parameters because both are per-connection and both are read by
/// the same arm — §7.2's backstop, whose diagnosis depends on the second one.
#[derive(Default)]
pub(super) struct MarketDataSessionReaderState {
    pub(super) last_sequence: HashMap<(String, String), u64>,
    pub(super) disclosed: HashSet<(String, String)>,
}

/// One CONNECTION's life. Owns the stream; reads until a fault, a `Bye`, a non-`MarketData` response or a
/// commanded stop; then hands the session back a torn-down state and lets the reconciler re-dial.
fn reader_loop(session: Arc<MarketDataSession>, mut stream: TcpStream) {
    let mut st = MarketDataSessionReaderState::default();
    let why = loop {
        match read_frame::<_, Response>(&mut stream) {
            Ok(Response::MarketData(frame)) => {
                if !session.apply_frame(*frame, &mut st) {
                    break "the server said goodbye";
                }
            }
            Ok(other) => {
                // The stream socket carries `Response::MarketData` and nothing else after `MarketDataSubscribed`
                // (the `market` module's §0 invariant), so anything else is a protocol desync.
                tracing::error!(
                    "datahub market-data stream desync: expected MarketData, got {other:?} — closing"
                );
                break "protocol desync";
            }
            // ⚠ A read TIMEOUT lands here too, and that is deliberate — see the module doc.
            Err(_) => break "the link dropped",
        }
    };

    let commanded = session.stopping();
    // A COMMANDED stop is not a dropped link: no bracket, no status churn, no re-dial — the
    // `observe_bridge` discipline, verbatim.
    if !commanded {
        session.open_gap_bracket(why);
    }
    *lock(&session.served) = Vec::new();
    *lock(&session.session) = None;
    *lock(&session.stream_handle) = None;
    // Per-CONNECTION health, so the next one starts from "wanted, not yet live" rather than
    // inheriting a mark for a key the new server may serve perfectly.
    lock(&session.gapped).clear();
    if !commanded {
        session.refresh_statuses(&DataClientStatus::new(
            DataClientConnectionState::Connecting,
            format!("reconnecting ({why})"),
        ));
        (session.wake)();
        session.poke();
    }
}

// ── The reconciler thread ───────────────────────────────────────────────────────────────────────

/// The ONE thread that dials. See the module doc for why it is not the reader.
pub(super) fn reconcile_loop(session: Arc<MarketDataSession>) {
    // Two INDEPENDENT runs over ONE schedule (`feed_retry::RetryRun`: 1s → 60s then flat forever, no
    // attempt limit). Reusing it is deliberate: it is already this crate's one answer to "how often
    // does a background thing re-ask something currently saying no", so this whole design introduces
    // ZERO new timing constants.
    let mut dial_run = RetryRun::default();
    let mut dial_after: Option<Instant> = None;
    let mut control = RetryRun::default();
    let mut control_after: Option<Instant> = None;
    // The keys the last `MarketDataUpdate` tried to add, so a ladder earned on a capped key can be told
    // from one gating a key that has never been asked. See where it is consulted.
    let mut last_add: HashSet<(String, String, StreamLabel)> = HashSet::new();
    // The address every cached fact in this session DESCRIBES — set by a dial ATTEMPT, not by a
    // success, because `dial` caches the advertisement before it refuses (see [`MarketDataSessionCapabilities`]).
    let mut server: Option<String> = None;
    // The device key's epoch the resolved `session.keys` came from; re-resolved off this thread
    // when it moves. See [`resolve_keys_if_moved`].
    let mut resolved_for = session.keys_epoch;
    // When the live stream opened. ⚠ **A dial that SUCCEEDS does not reset the ladder — a
    // connection that LASTED does.** Without this a server that answers `MarketDataSubscribed` and then
    // immediately writes `Bye(TooSlow)` (or drops) is dialled again at full speed for ever: every
    // attempt "succeeds", so a success-keyed reset never lets the backoff climb. A connection that
    // lived at least one [`RETRY_BASE`] is treated as healthy and zeroes the count; anything
    // shorter is a failed attempt wearing a success's clothes.
    let mut connected_at: Option<Instant> = None;

    loop {
        if session.stopping() {
            return;
        }
        let seen = session.poke_generation();
        let mut address = lock(&session.address).clone();
        let live = lock(&session.session).is_some();
        if resolve_keys_if_moved(&session, &mut resolved_for) && !live {
            // A key that was not there: dial with it now, not after the key-less attempts' backoff.
            dial_run.reset();
            dial_after = None;
        }

        // ⚠ **A `None` address is "no new answer", NOT a different server** — see
        // [`MarketDataSession::set_address`]. The observe bridge clears its advertisement on every link blip,
        // and on the documented default box that advertisement is the whole address ladder, so
        // treating `None` as a re-point tore the entire market-data plane down on a blip the bridge
        // exists to absorb. Hold what this session already dialled.
        if address.is_none() {
            address = server.clone();
        }

        // A change to a DIFFERENT address IS a different server: tear the stream down and forget
        // every fact that describes the old one.
        if server.is_some() && server != address {
            if live {
                // ⚠ **SHUT THE SOCKET AND STOP HERE.** `served` and `session` belong to the READER,
                // which owns §7.3 rule 4's bracket and builds it by reading `served` —
                // `open_gap_bracket`'s entire input is `lock(&self.served).clone()`. Clearing it
                // here raced that read away: the reconciler is already running and needs three
                // uncontended mutex ops, while the reader must first be woken by the kernel, so the
                // reconciler won essentially always and the bracket forgot NOTHING — no
                // `L2BookStore::remove`, no `GapStart`, no `bump_gap`, not even its own `warn!`. The
                // previous server's ladder then stayed painted (flagged STALE after
                // `DOM_STALE_MS`) and nothing in this binary would ever remove it.
                //
                // The reader POKES when it has finished, so the park below normally lasts
                // microseconds; the duration is only the backstop for the one case where
                // `stream_handle` could not be cloned, the shutdown therefore did nothing, and the
                // reader is waiting out its own read deadline instead.
                session.begin_stream_teardown();
                session.park(seen, RETRY_MAX);
                continue;
            }
            *lock(&session.capabilities) = None;
            lock(&session.refused).clear();
            lock(&session.unheld).clear();
            server = None;
            connected_at = None;
            dial_run.reset();
            dial_after = None;
            control.reset();
            control_after = None;
            last_add.clear();
            continue;
        }

        let Some(address) = address else {
            session.set_all_statuses(DataClientStatus::new(
                DataClientConnectionState::Disconnected,
                "disconnected — no datahub configured; set `config.datahub_addr`, or connect a \
                 backend that advertises one",
            ));
            session.park(seen, RETRY_MAX);
            continue;
        };

        if !live {
            // A stream that just ended: was it up long enough to count as a working link?
            if let Some(at) = connected_at.take() {
                if at.elapsed() >= RETRY_BASE {
                    dial_run.reset();
                } else {
                    dial_after = Some(Instant::now() + dial_run.failed());
                }
            }
            let nothing_wanted = session.desired_specs().is_empty();
            if nothing_wanted && session.has_capabilities() {
                // Nothing is wanted and the advertisement is known: do not open a connection to say
                // so. The first Trade window or chart pokes us. (A chart can only want something
                // through a feed `MarketDataSession::sync_mount` mounted from that advertisement.)
                session.refresh_statuses(&session.idle_link(&address));
                session.park(seen, RETRY_MAX);
                continue;
            }
            if let Some(at) = dial_after
                && Instant::now() < at
            {
                session.park(seen, at.saturating_duration_since(Instant::now()));
                continue;
            }
            // ⚠ Set BEFORE the dial, and on EVERY attempt: `dial` caches the advertisement before
            // it can refuse for a capability reason, so a refused attempt leaves cached facts
            // describing this address just as a successful one does. Keyed on a CONNECTION instead,
            // a `MarketDataSessionCapabilities { market_data: false }` from a plane-less server could never be cleared —
            // see that type's doc. The probe caches the advertisement too, so it sets this as well.
            server = Some(address.clone());
            if nothing_wanted {
                // ⚠ **THE PROBE.** Nothing is wanted AND nothing is known about this server, and
                // that is exactly the state in which no feed can exist to want anything: the feeds
                // are mounted FROM the advertisement (`MarketDataSession::sync_mount`). Waiting for a
                // subscription would wait for ever, so open the handshake alone — no `market_data_subscribe`,
                // no stream slot spent on the server — read what it advertises, and wake the frame
                // thread so it can mount.
                match probe(&session, &address) {
                    Ok(()) => {
                        // The link is demonstrably up, so the ladder starts over. There is no
                        // connection to LAST (see `connected_at`): the probe is closed already.
                        dial_run.reset();
                        dial_after = None;
                        session.refresh_statuses(&session.idle_link(&address));
                        (session.wake)();
                    }
                    Err(message) => {
                        let wait = dial_run.failed();
                        // The first failure of an outage is the event; the retries are a minute apart
                        // at most, and with no line mounted nothing else records them.
                        if dial_run.attempts() == 1 {
                            tracing::warn!(
                                "datahub market-data: handshake with {address} failed: {message}"
                            );
                        } else {
                            tracing::debug!(
                                "datahub market-data: handshake with {address} failed again: {message}"
                            );
                        }
                        dial_after = Some(dial_failed(&session, &address, &message, wait));
                    }
                }
                continue;
            }
            match dial(&session, &address) {
                Ok(()) => {
                    // ⚠ NOT `dial_run.reset()` — see `connected_at`. The ladder is reset by a
                    // connection that LASTED, which is judged when this one ends.
                    dial_after = None;
                    connected_at = Some(Instant::now());
                    // A fresh connection re-asks the whole desired set through `market_data_subscribe`, so
                    // whatever the control ladder was climbing was a question about a socket that
                    // no longer exists.
                    control.reset();
                    control_after = None;
                    last_add.clear();
                }
                Err(message) => {
                    let wait = dial_run.failed();
                    dial_after = Some(dial_failed(&session, &address, &message, wait));
                }
            }
            continue;
        }

        let (add, remove) = session.diff();
        if add.is_empty() && remove.is_empty() {
            session.park(seen, RETRY_MAX);
            continue;
        }
        // ⚠ **A LADDER EARNED ON A CAPPED KEY MUST NOT GATE A KEY IT HAS NOTHING TO DO WITH.**
        // A cap refusal deliberately leaves its spec in `desired` (`absorb_refusals`' non-permanent
        // arm), so `diff()` stays non-empty for ever against a capped server and the `control` run
        // climbs to `RETRY_MAX`. That backoff is right — it is what makes a permanently capped key
        // cost one dial a minute instead of thousands — but it is SESSION-wide, so a user opening a
        // DOM window on a perfectly servable key waited up to a MINUTE with no wire traffic and no
        // explanation. Caps are a designed feature (§5.4 argues them from a desktop with several
        // ladders plus a cockpit), not a pathology, so this is the ordinary case.
        //
        // The test is "does this `add` list name a key the last one did not". A key already asked
        // for and capped does not reset anything, so the hot loop the ladder exists to stop stays
        // stopped; a genuinely NEW key goes out at once.
        let add_keys: HashSet<(String, String, StreamLabel)> =
            add.iter().map(|s| (s.venue.clone(), s.symbol.clone(), s.lane)).collect();
        if !add_keys.is_subset(&last_add) {
            control.reset();
            control_after = None;
        }
        if let Some(at) = control_after
            && Instant::now() < at
        {
            session.park(seen, at.saturating_duration_since(Instant::now()));
            continue;
        }
        last_add = add_keys;
        match push_update(&session, &address, add, remove) {
            Ok(()) => {
                // ⚠ **A SUCCESSFUL EXCHANGE IS NOT A CONVERGED SET**, and resetting the ladder on
                // the transport's verdict rather than the OUTCOME's is a hot loop against the one
                // server that just said no.
                //
                // A CAP refusal deliberately leaves its spec in the desired set — a cap frees up
                // when another window closes, so the key stays wanted and is retried. That means
                // `diff()` answers non-empty again on the very next pass, and with the ladder zeroed
                // there is no delay before the next dial: a `MarketDataUpdate` per loop iteration, for
                // ever, on a fresh connection each time. It is the same failure the `MarketDataSpec::key()`
                // diff avoids, reached from the other side, and it is invisible against a fake that
                // accepts whatever it is sent.
                //
                // So this is the control-lane twin of `connected_at` above: the ladder resets on
                // CONVERGENCE, not on a reply. A set that did not converge advances it, which is
                // what makes a permanently capped key cost one dial a minute instead of thousands.
                let (still_add, still_remove) = session.diff();
                if still_add.is_empty() && still_remove.is_empty() {
                    control.reset();
                    control_after = None;
                } else {
                    control_after = Some(Instant::now() + control.failed());
                }
            }
            Err(message) => {
                let wait = control.failed();
                control_after = Some(Instant::now() + wait);
                tracing::warn!(
                    "datahub market-data: MarketDataUpdate failed ({message}) — the served set is UNKNOWN, not \
                     empty; retrying in {}s",
                    wait.as_secs()
                );
            }
        }
    }
}

/// Account one failed dial or probe on every mounted line (and the link), as an `Error`, and return
/// when the ladder next allows an attempt (`wait` is the ladder's wait after this failure). `msg`
/// routinely carries the word `live` (`NO_MARKET_DATA_PLANE` names `flags.datahub_live`), which is
/// harmless because the state is set here, not read from the words.
fn dial_failed(
    session: &MarketDataSession,
    address: &str,
    message: &str,
    wait: Duration,
) -> Instant {
    session.set_all_statuses(DataClientStatus::new(
        DataClientConnectionState::Error,
        format!("datahub {address} error: {message} (retrying in {}s)", wait.as_secs()),
    ));
    (session.wake)();
    Instant::now() + wait
}

impl MarketDataSession {
    /// Shut the live stream's socket so its reader returns at once, WITHOUT raising the global stop
    /// flag — the reader then takes its ordinary fault path (bracket, clear, poke) and the
    /// reconciler re-dials.
    fn begin_stream_teardown(&self) {
        if let Some(s) = lock(&self.stream_handle).as_ref() {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    }
}

/// Re-resolve this desktop's device key when it moved
/// ([`crate::backend::registry::device_key_epoch`]): the Server group makes it mid-session,
/// and a session built before it would otherwise dial key-less, refused by every keyed datahub, for
/// as long as the process lives. `true` when the keys were resolved again.
///
/// ⚠ **It runs on the RECONCILER.** [`crate::backend::registry::datahub_dial_keys`] opens the
/// settings database, and the shell's [`MarketDataSession::follow_device_key`] runs per frame;
/// resolving there would put a store read on the frame thread. A change does NOT tear the live
/// stream down: that connection is already authenticated and working, and a re-key buys nothing
/// until the next dial.
fn resolve_keys_if_moved(session: &Arc<MarketDataSession>, current: &mut u64) -> bool {
    // The epoch BEFORE the read, so a key made during it moves the epoch again and is read next pass.
    let wanted = crate::backend::registry::device_key_epoch();
    if wanted == *current {
        return false;
    }
    let (keys, notices) =
        crate::backend::registry::datahub_dial_keys(session.settings_dir.as_deref());
    for n in notices {
        tracing::warn!("datahub market-data: {n}");
    }
    tracing::info!(
        configured = keys.is_some(),
        "datahub market-data: this desktop's device key changed — the next dial signs with it"
    );
    *lock(&session.keys) = keys;
    *current = wanted;
    true
}

/// Run one blocking control exchange on a SHORT-LIVED helper thread and give up on it after
/// [`CONTROL_DEADLINE`].
///
/// ⚠ **This is the module doc's bounded-control-exchange rule, and it is a THREAD because the
/// socket option is not ours to set**: `DatahubClient` clears its read timeout at the end of every
/// `connect`, deliberately, and exposes no hatch to re-arm it for one verb. Abandoning the helper is
/// safe because it borrows NOTHING from the session — it returns a value, and a value nobody
/// receives is dropped, closing the socket inside it. The cost of a wedged peer is therefore one
/// leaked thread, bounded by how often the dial ladder fires (1 s → 60 s, flat), instead of a
/// reconciler that never returns and a market-data plane that never comes back.
///
/// The thread, the one-slot channel and the wait are `vike_utility::thread::bounded`; this caller
/// keeps its thread name, its deadline and its words.
fn bounded<T: Send + 'static>(
    what: &str,
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    vike_utility::thread::bounded("ui-data-control", CONTROL_DEADLINE, f).map_err(|e| match e {
        ThreadNoAnswer::NotStarted(e) => {
            format!("the OS refused the market-data control thread: {e}")
        }
        ThreadNoAnswer::TimedOut | ThreadNoAnswer::Panicked => format!(
            "{what} did not answer within {}s — treating the link as dead (the datahub client \
             clears its read timeout past the handshake, so this deadline is the only one there is)",
            CONTROL_DEADLINE.as_secs()
        ),
    })
}

/// What [`dial_blocking`] learned: the advertisement (on BOTH arms — it is read before the refusal
/// and [`MarketDataSession::want`] needs it either way, see [`MarketDataSessionCapabilities`]) plus the opened stream or the reason
/// there is none. A named alias so the signature stays under `clippy::type_complexity`.
type Dialed =
    (Option<MarketDataSessionCapabilities>, Result<(MarketDataSubscribedInfo, TcpStream), String>);

/// The BLOCKING half of a dial: connect, learn the advertisement, `market_data_subscribe`. It touches no
/// session state at all, which is exactly what makes it safe to abandon — see [`bounded`]. Every
/// publication of what it learned happens back on the reconciler, in [`dial`].
///
/// The dial is authenticated when this desktop holds its device key and plain otherwise
/// (`DatahubClient::connect_with_keys`, at `Scope::Read`); `keys` is OWNED because this runs on
/// [`bounded`]'s helper thread and must borrow nothing from the session.
fn dial_blocking(address: String, keys: Option<HubKeys>, specs: Vec<MarketDataSpec>) -> Dialed {
    let client = match DatahubClient::connect_with_keys(&address, keys.as_ref(), Scope::Read) {
        Ok(c) => c,
        Err(e) => return (None, Err(e.to_string())),
    };
    let capabilities = MarketDataSessionCapabilities::from_features(client.features());
    if !capabilities.market_data {
        return (Some(capabilities), Err(NO_MARKET_DATA_PLANE.to_string()));
    }
    let opened = client.market_data_subscribe(specs).map_err(|(_client, message)| message);
    (Some(capabilities), opened)
}

/// The BLOCKING half of a probe: connect, read `Welcome.features`, and drop the connection. It sends
/// no request at all after the handshake, so the server spends no stream slot and no key on it, and
/// like [`dial_blocking`] it touches no session state, which is what makes it safe to abandon.
fn probe_blocking(
    address: String,
    keys: Option<HubKeys>,
) -> Result<MarketDataSessionCapabilities, String> {
    let client = DatahubClient::connect_with_keys(&address, keys.as_ref(), Scope::Read)
        .map_err(|e| e.to_string())?;
    Ok(MarketDataSessionCapabilities::from_features(client.features()))
}

/// Learn what the server advertises WITHOUT asking it for anything: the handshake alone, cached as
/// [`MarketDataSessionCapabilities`] exactly as a dial caches it. The reconciler runs this when nothing is wanted and nothing
/// is known — see the module doc for why mounting the feeds forces it.
///
/// A server with no market-data plane is a SUCCESSFUL probe: it answered, and what it advertises is
/// "nothing to mount". The reason is the link text's job (`MarketDataSession::idle_link`), not an error here.
fn probe(session: &Arc<MarketDataSession>, address: &str) -> Result<(), String> {
    let keys = lock(&session.keys).clone();
    let a = address.to_string();
    let capabilities = bounded("the datahub handshake", move || probe_blocking(a, keys))??;
    *lock(&session.capabilities) = Some(capabilities);
    Ok(())
}

/// Open the stream: connect, cache the advertisement, `market_data_subscribe`, spawn the reader.
///
/// A free function taking the `Arc` because the reader thread needs one and only the reconciler —
/// which was spawned WITH the `Arc` — can hand it over. Re-deriving one from `&self` is not
/// expressible, and a `Weak` back-pointer would be machinery bought for nothing.
///
/// ⚠ The advertisement is cached BEFORE the subscribe, so a server that answers `Response::Error`
/// (one predating the verb, or built with no market-data plane) still teaches [`MarketDataSession::want`]
/// to refuse locally from the next frame on, instead of re-attempting for ever.
fn dial(session: &Arc<MarketDataSession>, address: &str) -> Result<(), String> {
    let keys = lock(&session.keys).clone();
    let specs = session.desired_specs();
    let a = address.to_string();
    let (capabilities, opened) =
        bounded("the datahub market-data subscribe", move || dial_blocking(a, keys, specs))?;
    if let Some(c) = capabilities {
        // A quote key wanted before this server's word was known: the client refused this dial for it
        // with nothing sent, and without it the next one goes through.
        if !c.quote_lane {
            session.refuse_quote_keys();
        }
        *lock(&session.capabilities) = Some(c);
    }
    let (info, stream) = opened?;

    *lock(&session.session) = Some(info.session);
    *lock(&session.served) = info.accepted.clone();
    // A fresh connection knows nothing about any key's flow yet, and the server re-states each
    // one's `Status` as its first frame.
    lock(&session.gapped).clear();
    // The log forgets what the last connection carried; a gap it WARNed of stays open until data.
    for log in lock(&session.streams).values_mut() {
        (log.flowed, log.heard) = (false, false);
    }
    session.absorb_refusals(info.refused);
    // §7.3 rule 4's CLOSING half for the tape: every accepted trade key starts from a fresh
    // aggregator, because a reconnect is a hole and the server keeps no replay buffer.
    for spec in &info.accepted {
        if spec.lane == StreamLabel::Trades {
            session.bump_gap(&spec.venue, &spec.symbol);
        }
    }
    match stream.try_clone() {
        Ok(handle) => *lock(&session.stream_handle) = Some(handle),
        Err(e) => tracing::warn!(
            "datahub market-data: could not clone the stream handle ({e}) — teardown will wait out \
             the read deadline instead of returning at once"
        ),
    }
    session.refresh_statuses(&DataClientStatus::new(
        DataClientConnectionState::Connected,
        format!("datahub {address}"),
    ));
    (session.wake)();

    let reader = session.clone();
    match std::thread::Builder::new()
        .name("ui-data-reader".to_string())
        .spawn(move || reader_loop(reader, stream))
    {
        Ok(h) => {
            if let Some(old) = lock(&session.reader).replace(h) {
                // The previous connection's reader has already RETURNED — clearing `session` is
                // what let us dial again — so this join reaps a finished thread rather than waiting
                // on a live one.
                let _ = old.join();
            }
            Ok(())
        }
        Err(e) => {
            *lock(&session.session) = None;
            *lock(&session.served) = Vec::new();
            *lock(&session.stream_handle) = None;
            Err(format!("the OS refused the market-data reader thread: {e}"))
        }
    }
}

/// Change the live session's set on a SHORT-LIVED connection of its own. ⚠ Never on the stream
/// socket: the server's side of that one has left its read loop and a frame written there is never
/// read. There is deliberately no long-lived control connection — a dead one would be a state that
/// leaks topics.
fn push_update(
    session: &Arc<MarketDataSession>,
    address: &str,
    add: Vec<MarketDataSpec>,
    remove: Vec<MarketDataSpec>,
) -> Result<(), String> {
    let Some(session_id) = *lock(&session.session) else {
        return Err("the stream closed before the update was sent".to_string());
    };
    let keys = lock(&session.keys).clone();
    let a = address.to_string();
    // Bounded for the reason [`bounded`] states: this read has no deadline of its own either, and a
    // wedge here is the same permanently-dark plane a wedged subscribe would be.
    let info = bounded(
        "the datahub market-data update",
        move || -> Result<MarketDataUpdatedInfo, String> {
            let mut client = DatahubClient::connect_with_keys(&a, keys.as_ref(), Scope::Read)
                .map_err(|e| e.to_string())?;
            client.market_data_update(session_id, add, remove)
        },
    )??;
    {
        let mut served = lock(&session.served);
        served.retain(|s| !info.released.iter().any(|r| r.key() == s.key()));
        for spec in &info.accepted {
            if !served.iter().any(|s| s.key() == spec.key()) {
                served.push(spec.clone());
            }
        }
    }
    for spec in &info.accepted {
        if spec.lane == StreamLabel::Trades {
            session.bump_gap(&spec.venue, &spec.symbol);
        }
    }
    session.absorb_refusals(info.refused);
    session.refresh_statuses(&DataClientStatus::new(
        DataClientConnectionState::Connected,
        format!("datahub {address}"),
    ));
    (session.wake)();
    Ok(())
}
