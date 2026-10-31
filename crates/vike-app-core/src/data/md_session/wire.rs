//! `MdSession`'s background threads: the reader, the reconciler and the bounded dial helpers.

use super::*;

// ── The reader thread ───────────────────────────────────────────────────────────────────────────

/// The reader's OWN state, which lives for exactly one connection and is shared with nothing: the
/// per-key wire sequence, and which keys have had a tape hole DISCLOSED on this connection.
///
/// It is a struct rather than two parameters because both are per-connection and both are read by
/// the same arm — §7.2's backstop, whose diagnosis depends on the second one.
#[derive(Default)]
pub(super) struct ReaderState {
    pub(super) last_seq: HashMap<(String, String), u64>,
    pub(super) disclosed: HashSet<(String, String)>,
}

/// One CONNECTION's life. Owns the stream; reads until a fault, a `Bye`, a non-`Md` response or a
/// commanded stop; then hands the session back a torn-down state and lets the reconciler re-dial.
fn reader_loop(session: Arc<MdSession>, mut stream: TcpStream) {
    let mut st = ReaderState::default();
    let why = loop {
        match read_frame::<_, Response>(&mut stream) {
            Ok(Response::Md(frame)) => {
                if !session.apply_frame(*frame, &mut st) {
                    break "the server said goodbye";
                }
            }
            Ok(other) => {
                // The stream socket carries `Response::Md` and nothing else after `MdSubscribed`
                // (the `market` module's §0 invariant), so anything else is a protocol desync.
                tracing::error!(
                    "datahub market-data stream desync: expected Md, got {other:?} — closing"
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
        session.refresh_statuses(&format!("reconnecting ({why})"));
        (session.wake)();
        session.poke();
    }
}

// ── The reconciler thread ───────────────────────────────────────────────────────────────────────

/// The ONE thread that dials. See the module doc for why it is not the reader.
pub(super) fn reconcile_loop(session: Arc<MdSession>) {
    // Two INDEPENDENT attempt counters over ONE schedule (`feed_lifecycle::retry_backoff`: 1s → 60s
    // then flat forever, no attempt limit). Reusing that function is deliberate: it is already this
    // crate's one answer to "how often does a background thing re-ask something currently saying
    // no", so this whole design introduces ZERO new timing constants.
    let mut dial_attempts: u32 = 0;
    let mut dial_after: Option<Instant> = None;
    let mut ctrl_attempts: u32 = 0;
    let mut ctrl_after: Option<Instant> = None;
    // The keys the last `MdUpdate` tried to add, so a ladder earned on a capped key can be told
    // from one gating a key that has never been asked. See where it is consulted.
    let mut last_add: HashSet<(String, String, MdLane)> = HashSet::new();
    // The address every cached fact in this session DESCRIBES — set by a dial ATTEMPT, not by a
    // success, because `dial` caches the advertisement before it refuses (see [`Caps`]).
    let mut server: Option<String> = None;
    // The key NAME the resolved `session.keys` came from; re-resolved off this thread when the
    // shell moves it. See [`resolve_keys_if_renamed`].
    let mut key_name = lock(&session.key_name).clone();
    // When the live stream opened. ⚠ **A dial that SUCCEEDS does not reset the ladder — a
    // connection that LASTED does.** Without this a server that answers `MdSubscribed` and then
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
        let mut addr = lock(&session.addr).clone();
        let live = lock(&session.session).is_some();
        resolve_keys_if_renamed(&session, &mut key_name);

        // ⚠ **A `None` address is "no new answer", NOT a different server** — see
        // [`MdSession::set_addr`]. The observe bridge clears its advertisement on every link blip,
        // and on the documented default box that advertisement is the whole address ladder, so
        // treating `None` as a re-point tore the entire market-data plane down on a blip the bridge
        // exists to absorb. Hold what this session already dialled.
        if addr.is_none() {
            addr = server.clone();
        }

        // A change to a DIFFERENT address IS a different server: tear the stream down and forget
        // every fact that describes the old one.
        if server.is_some() && server != addr {
            if live {
                // ⚠ **SHUT THE SOCKET AND STOP HERE.** `served` and `session` belong to the READER,
                // which owns §7.3 rule 4's bracket and builds it by reading `served` —
                // `open_gap_bracket`'s entire input is `lock(&self.served).clone()`. Clearing it
                // here raced that read away: the reconciler is already running and needs three
                // uncontended mutex ops, while the reader must first be woken by the kernel, so the
                // reconciler won essentially always and the bracket forgot NOTHING — no
                // `BookStore::remove`, no `GapStart`, no `bump_gap`, not even its own `warn!`. The
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
            *lock(&session.caps) = None;
            lock(&session.refused).clear();
            server = None;
            connected_at = None;
            dial_attempts = 0;
            dial_after = None;
            ctrl_attempts = 0;
            ctrl_after = None;
            last_add.clear();
            continue;
        }

        let Some(addr) = addr else {
            session.set_all_statuses(
                "disconnected — no datahub configured; set `config.datahub_addr`, or connect a \
                 backend that advertises one",
            );
            session.park(seen, RETRY_MAX);
            continue;
        };

        if !live {
            // A stream that just ended: was it up long enough to count as a working link?
            if let Some(at) = connected_at.take() {
                if at.elapsed() >= RETRY_BASE {
                    dial_attempts = 0;
                } else {
                    dial_attempts = dial_attempts.saturating_add(1);
                    dial_after = Some(Instant::now() + retry_backoff(dial_attempts));
                }
            }
            let nothing_wanted = session.desired_specs().is_empty();
            if nothing_wanted && session.has_caps() {
                // Nothing is wanted and the advertisement is known: do not open a connection to say
                // so. The first Trade window or chart pokes us. (A chart can only want something
                // through a feed `MdSession::sync_mount` mounted from that advertisement.)
                session.refresh_statuses(&session.idle_link(&addr));
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
            // a `Caps { market_data: false }` from a plane-less server could never be cleared —
            // see that type's doc. The probe caches the advertisement too, so it sets this as well.
            server = Some(addr.clone());
            if nothing_wanted {
                // ⚠ **THE PROBE.** Nothing is wanted AND nothing is known about this server, and
                // that is exactly the state in which no feed can exist to want anything: the feeds
                // are mounted FROM the advertisement (`MdSession::sync_mount`). Waiting for a
                // subscription would wait for ever, so open the handshake alone — no `md_subscribe`,
                // no stream slot spent on the server — read what it advertises, and wake the frame
                // thread so it can mount.
                match probe(&session, &addr) {
                    Ok(()) => {
                        // The link is demonstrably up, so the ladder starts over. There is no
                        // connection to LAST (see `connected_at`): the probe is closed already.
                        dial_attempts = 0;
                        dial_after = None;
                        session.refresh_statuses(&session.idle_link(&addr));
                        (session.wake)();
                    }
                    Err(msg) => {
                        dial_attempts = dial_attempts.saturating_add(1);
                        // The first failure of an outage is the event; the retries are a minute apart
                        // at most, and with no line mounted nothing else records them.
                        if dial_attempts == 1 {
                            tracing::warn!(
                                "datahub market-data: handshake with {addr} failed: {msg}"
                            );
                        } else {
                            tracing::debug!(
                                "datahub market-data: handshake with {addr} failed again: {msg}"
                            );
                        }
                        dial_after = Some(dial_failed(&session, &addr, &msg, dial_attempts));
                    }
                }
                continue;
            }
            match dial(&session, &addr) {
                Ok(()) => {
                    // ⚠ NOT `dial_attempts = 0` — see `connected_at`. The ladder is reset by a
                    // connection that LASTED, which is judged when this one ends.
                    dial_after = None;
                    connected_at = Some(Instant::now());
                    // A fresh connection re-asks the whole desired set through `md_subscribe`, so
                    // whatever the control ladder was climbing was a question about a socket that
                    // no longer exists.
                    ctrl_attempts = 0;
                    ctrl_after = None;
                    last_add.clear();
                }
                Err(msg) => {
                    dial_attempts = dial_attempts.saturating_add(1);
                    dial_after = Some(dial_failed(&session, &addr, &msg, dial_attempts));
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
        // arm), so `diff()` stays non-empty for ever against a capped server and `ctrl_attempts`
        // climbs to `RETRY_MAX`. That backoff is right — it is what makes a permanently capped key
        // cost one dial a minute instead of thousands — but it is SESSION-wide, so a user opening a
        // DOM window on a perfectly servable key waited up to a MINUTE with no wire traffic and no
        // explanation. Caps are a designed feature (§5.4 argues them from a desktop with several
        // ladders plus a cockpit), not a pathology, so this is the ordinary case.
        //
        // The test is "does this `add` list name a key the last one did not". A key already asked
        // for and capped does not reset anything, so the hot loop the ladder exists to stop stays
        // stopped; a genuinely NEW key goes out at once.
        let add_keys: HashSet<(String, String, MdLane)> =
            add.iter().map(|s| (s.venue.clone(), s.symbol.clone(), s.lane)).collect();
        if !add_keys.is_subset(&last_add) {
            ctrl_attempts = 0;
            ctrl_after = None;
        }
        if let Some(at) = ctrl_after
            && Instant::now() < at
        {
            session.park(seen, at.saturating_duration_since(Instant::now()));
            continue;
        }
        last_add = add_keys;
        match push_update(&session, &addr, add, remove) {
            Ok(()) => {
                // ⚠ **A SUCCESSFUL EXCHANGE IS NOT A CONVERGED SET**, and resetting the ladder on
                // the transport's verdict rather than the OUTCOME's is a hot loop against the one
                // server that just said no.
                //
                // A CAP refusal deliberately leaves its spec in the desired set — a cap frees up
                // when another window closes, so the key stays wanted and is retried. That means
                // `diff()` answers non-empty again on the very next pass, and with the ladder zeroed
                // there is no delay before the next dial: an `MdUpdate` per loop iteration, for
                // ever, on a fresh connection each time. It is the same failure the `MdSpec::key()`
                // diff avoids, reached from the other side, and it is invisible against a fake that
                // accepts whatever it is sent.
                //
                // So this is the control-lane twin of `connected_at` above: the ladder resets on
                // CONVERGENCE, not on a reply. A set that did not converge advances it, which is
                // what makes a permanently capped key cost one dial a minute instead of thousands.
                let (still_add, still_remove) = session.diff();
                if still_add.is_empty() && still_remove.is_empty() {
                    ctrl_attempts = 0;
                    ctrl_after = None;
                } else {
                    ctrl_attempts = ctrl_attempts.saturating_add(1);
                    ctrl_after = Some(Instant::now() + retry_backoff(ctrl_attempts));
                }
            }
            Err(msg) => {
                ctrl_attempts = ctrl_attempts.saturating_add(1);
                let wait = retry_backoff(ctrl_attempts);
                ctrl_after = Some(Instant::now() + wait);
                tracing::warn!(
                    "datahub market-data: MdUpdate failed ({msg}) — the served set is UNKNOWN, not \
                     empty; retrying in {}s",
                    wait.as_secs()
                );
            }
        }
    }
}

/// Account one failed dial or probe on every mounted line (and the link), and return when the
/// ladder next allows an attempt (`attempts` is the count INCLUDING this failure).
///
/// ⚠ `error:` is not decoration — see `refresh_statuses`. `msg` routinely CARRIES the word `live`
/// (`NO_MD_PLANE` names `flags.datahub_live` and `live-feeds`, which is the whole point of that
/// message), and `parse_feed_status` tests the error rung BEFORE the live one. Without this prefix a
/// datahub with no plane at all renders as Connected in the Connections tool.
fn dial_failed(session: &MdSession, addr: &str, msg: &str, attempts: u32) -> Instant {
    let wait = retry_backoff(attempts);
    session.set_all_statuses(&format!(
        "datahub {addr} error: {msg} (retrying in {}s)",
        wait.as_secs()
    ));
    (session.wake)();
    Instant::now() + wait
}

impl MdSession {
    /// Shut the live stream's socket so its reader returns at once, WITHOUT raising the global stop
    /// flag — the reader then takes its ordinary fault path (bracket, clear, poke) and the
    /// reconciler re-dials.
    fn begin_stream_teardown(&self) {
        if let Some(s) = lock(&self.stream_handle).as_ref() {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    }
}

/// Re-resolve the datahub observe key when the shell has moved its NAME — a backend switch, or an
/// edit to the active record.
///
/// ⚠ **It runs on the RECONCILER, and that is the whole reason the name and the key are separate
/// fields.** [`crate::backend::backend_registry::datahub_observe_keys`] opens the settings
/// database, and the shell pushes this per frame; resolving at the push site would put a store read
/// on the frame thread. A change does NOT tear the live stream down: that connection is already
/// authenticated and working, and a re-key buys nothing until the next dial — which a re-POINT
/// (the usual companion of a switch) triggers on its own through the address arm.
fn resolve_keys_if_renamed(session: &Arc<MdSession>, current: &mut String) {
    let wanted = lock(&session.key_name).clone();
    if wanted == *current {
        return;
    }
    let (keys, notices) = crate::backend::backend_registry::datahub_observe_keys(
        session.settings_dir.as_deref(),
        &wanted,
    );
    for n in notices {
        tracing::warn!("datahub market-data: {n}");
    }
    tracing::info!(
        key_name = %wanted,
        configured = keys.is_some(),
        "datahub market-data: the observe key NAME changed — the next dial signs with it"
    );
    *lock(&session.keys) = keys;
    *current = wanted;
}

/// Run one blocking control exchange on a SHORT-LIVED helper thread and give up on it after
/// [`CTRL_DEADLINE`].
///
/// ⚠ **This is the module doc's bounded-control-exchange rule, and it is a THREAD because the
/// socket option is not ours to set**: `DatahubClient` clears its read timeout at the end of every
/// `connect`, deliberately, and exposes no hatch to re-arm it for one verb. Abandoning the helper is
/// safe because it borrows NOTHING from the session — it returns a value, and a value nobody
/// receives is dropped, closing the socket inside it. The cost of a wedged peer is therefore one
/// leaked thread, bounded by how often the dial ladder fires (1 s → 60 s, flat), instead of a
/// reconciler that never returns and a market-data plane that never comes back.
fn bounded<T: Send + 'static>(
    what: &str,
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    // ⚠ `sync_channel(1)`, so the helper's `send` NEVER blocks: a rendezvous channel would park the
    // abandoned thread for ever on the very path this function exists to bound.
    let (tx, rx) = std::sync::mpsc::sync_channel::<T>(1);
    std::thread::Builder::new()
        .name("md-ctrl".to_string())
        .spawn(move || {
            let _ = tx.send(f());
        })
        .map_err(|e| format!("the OS refused the market-data control thread: {e}"))?;
    rx.recv_timeout(CTRL_DEADLINE).map_err(|_| {
        format!(
            "{what} did not answer within {}s — treating the link as dead (the datahub client \
             clears its read timeout past the handshake, so this deadline is the only one there is)",
            CTRL_DEADLINE.as_secs()
        )
    })
}

/// What [`dial_blocking`] learned: the advertisement (on BOTH arms — it is read before the refusal
/// and [`MdSession::want`] needs it either way, see [`Caps`]) plus the opened stream or the reason
/// there is none. A named alias so the signature stays under `clippy::type_complexity`.
type Dialed = (Option<Caps>, Result<(MdSubscribedInfo, TcpStream), String>);

/// The BLOCKING half of a dial: connect, learn the advertisement, `md_subscribe`. It touches no
/// session state at all, which is exactly what makes it safe to abandon — see [`bounded`]. Every
/// publication of what it learned happens back on the reconciler, in [`dial`].
///
/// The dial is authenticated when this desktop holds a datahub observe key and plain otherwise
/// (`DatahubClient::connect_with_keys`, at `Scope::Read`); `keys` is OWNED because this runs on
/// [`bounded`]'s helper thread and must borrow nothing from the session.
fn dial_blocking(addr: String, keys: Option<HubKeys>, specs: Vec<MdSpec>) -> Dialed {
    let client = match DatahubClient::connect_with_keys(&addr, keys.as_ref(), Scope::Read) {
        Ok(c) => c,
        Err(e) => return (None, Err(e.to_string())),
    };
    let caps = Caps::from_features(client.features());
    if !caps.market_data {
        return (Some(caps), Err(NO_MD_PLANE.to_string()));
    }
    let opened = client.md_subscribe(specs).map_err(|(_client, msg)| msg);
    (Some(caps), opened)
}

/// The BLOCKING half of a probe: connect, read `Welcome.features`, and drop the connection. It sends
/// no request at all after the handshake, so the server spends no stream slot and no key on it, and
/// like [`dial_blocking`] it touches no session state, which is what makes it safe to abandon.
fn probe_blocking(addr: String, keys: Option<HubKeys>) -> Result<Caps, String> {
    let client = DatahubClient::connect_with_keys(&addr, keys.as_ref(), Scope::Read)
        .map_err(|e| e.to_string())?;
    Ok(Caps::from_features(client.features()))
}

/// Learn what the server advertises WITHOUT asking it for anything: the handshake alone, cached as
/// [`Caps`] exactly as a dial caches it. The reconciler runs this when nothing is wanted and nothing
/// is known — see the module doc for why mounting the feeds forces it.
///
/// A server with no market-data plane is a SUCCESSFUL probe: it answered, and what it advertises is
/// "nothing to mount". The reason is the link text's job (`MdSession::idle_link`), not an error here.
fn probe(session: &Arc<MdSession>, addr: &str) -> Result<(), String> {
    let keys = lock(&session.keys).clone();
    let a = addr.to_string();
    let caps = bounded("the datahub handshake", move || probe_blocking(a, keys))??;
    *lock(&session.caps) = Some(caps);
    Ok(())
}

/// Open the stream: connect, cache the advertisement, `md_subscribe`, spawn the reader.
///
/// A free function taking the `Arc` because the reader thread needs one and only the reconciler —
/// which was spawned WITH the `Arc` — can hand it over. Re-deriving one from `&self` is not
/// expressible, and a `Weak` back-pointer would be machinery bought for nothing.
///
/// ⚠ The advertisement is cached BEFORE the subscribe, so a server that answers `Response::Error`
/// (one predating the verb, or built with no market-data plane) still teaches [`MdSession::want`]
/// to refuse locally from the next frame on, instead of re-attempting for ever.
fn dial(session: &Arc<MdSession>, addr: &str) -> Result<(), String> {
    let keys = lock(&session.keys).clone();
    let specs = session.desired_specs();
    let a = addr.to_string();
    let (caps, opened) =
        bounded("the datahub market-data subscribe", move || dial_blocking(a, keys, specs))?;
    if let Some(c) = caps {
        *lock(&session.caps) = Some(c);
    }
    let (info, stream) = opened?;

    *lock(&session.session) = Some(info.session);
    *lock(&session.served) = info.accepted.clone();
    // A fresh connection knows nothing about any key's flow yet, and the server re-states each
    // one's `Status` as its first frame.
    lock(&session.gapped).clear();
    session.absorb_refusals(info.refused);
    // §7.3 rule 4's CLOSING half for the tape: every accepted trade key starts from a fresh
    // aggregator, because a reconnect is a hole and the server keeps no replay buffer.
    for spec in &info.accepted {
        if spec.lane == MdLane::Trades {
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
    session.refresh_statuses(&format!("datahub {addr}"));
    (session.wake)();

    let reader = session.clone();
    match std::thread::Builder::new()
        .name("md-reader".to_string())
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
    session: &Arc<MdSession>,
    addr: &str,
    add: Vec<MdSpec>,
    remove: Vec<MdSpec>,
) -> Result<(), String> {
    let Some(session_id) = *lock(&session.session) else {
        return Err("the stream closed before the update was sent".to_string());
    };
    let keys = lock(&session.keys).clone();
    let a = addr.to_string();
    // Bounded for the reason [`bounded`] states: this read has no deadline of its own either, and a
    // wedge here is the same permanently-dark plane a wedged subscribe would be.
    let info =
        bounded("the datahub market-data update", move || -> Result<MdUpdatedInfo, String> {
            let mut client = DatahubClient::connect_with_keys(&a, keys.as_ref(), Scope::Read)
                .map_err(|e| e.to_string())?;
            client.md_update(session_id, add, remove)
        })??;
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
        if spec.lane == MdLane::Trades {
            session.bump_gap(&spec.venue, &spec.symbol);
        }
    }
    session.absorb_refusals(info.refused);
    session.refresh_statuses(&format!("datahub {addr}"));
    (session.wake)();
    Ok(())
}
