//! `DatahubClient`'s market-data verbs: `md_subscribe`, which CONSUMES the client and hands back the
//! raw push stream, and `md_update`, its request/response half for a short-lived connection.
//!
//! Split out of `client.rs`'s one `impl DatahubClient` by concern (behaviour byte-identical; the
//! methods moved verbatim). The answer types (`MdSubscribedInfo`, `MdUpdatedInfo`) and the
//! symbol-refusal helper stay in the parent module. `use super::*` brings in the parent module's
//! imports and items, so nothing about resolution changes.

use super::*;

impl DatahubClient {
    /// **Open a market-data push stream on this connection**, CONSUMING the client.
    ///
    /// On success the socket is a push stream and no positional verb may ever be sent on it again
    /// (see [`crate::market`]'s module doc), so the client is consumed and the raw
    /// [`TcpStream`] handed back — a reader owns it from here. The stream comes back with
    /// `max(MD_READ_TIMEOUT, 3 × heartbeat_ms)` already armed as its read timeout, so no caller can
    /// forget the deadline the heartbeat exists to serve.
    ///
    /// On failure the connection was **NOT** switched and `self` is returned intact, so the caller
    /// can keep using it positionally. That is not politeness: it is leg (3) of
    /// [`FEATURE_MARKET_DATA`]'s contract made usable — a server that predates the verb or serves no
    /// market-data plane answers [`Response::Error`] and keeps the connection, and a client that
    /// threw the socket away on that answer would turn a clean refusal into a reconnect.
    ///
    /// Like every other capability-negotiated verb here it checks the advertisement FIRST and
    /// refuses LOCALLY without sending, in the exact words `coverage_report` uses.
    ///
    /// ⚠ **It also validates the SYMBOL of every spec locally, through
    /// [`crate::market::validate_md_symbol`] — the same function the server's
    /// `crates/vike-datahub/src/md/hub.rs`'s `MdHub::acquire` calls.** One definition, two ends, so
    /// the client cannot guard a rule the server does not know nor the reverse; it is the
    /// market-data twin of the `resolve_produced_by` re-export the delete verb shares. A WHOLE
    /// request is refused rather than the offending spec, because nothing was sent and there is no
    /// per-spec channel to report on — the per-spec-versus-whole-request split
    /// [`MdRefusal`] carries is about what the SERVER answers.
    pub fn md_subscribe(
        mut self,
        specs: Vec<MdSpec>,
    ) -> Result<(MdSubscribedInfo, TcpStream), (Self, String)> {
        if let Err(msg) = refuse_bad_symbols("md_subscribe", &specs) {
            return Err((self, msg));
        }
        if !self.features.iter().any(|f| f == FEATURE_MARKET_DATA) {
            // ⚠ The ARM comes first and the rebuild second — the server's own
            // `NO_MARKET_DATA_PLANE` carries why: the plane compiles on a default build and
            // `--features live-feeds` gates no code, so an unset `VIKE_DATAHUB_LIVE` is the cause
            // in every case where the operator has not also chosen the venues.
            let msg = format!(
                "datahub server does not advertise `{FEATURE_MARKET_DATA}` (advertised: {:?}) — \
                 nothing was sent. The market-data plane is capability-negotiated, not \
                 version-gated: set VIKE_DATAHUB_LIVE=1 on the server and restart it. A rebuild is \
                 a separate question and only afterwards — `--features live-feeds` alone gates no \
                 code; the per-venue `live-<venue>` features, which imply it, are what link a \
                 venue's feed.",
                self.features
            );
            return Err((self, msg));
        }
        if let Err(e) = write_frame(&mut self.stream, &Request::MdSubscribe { specs }) {
            let msg = e.to_string();
            return Err((self, msg));
        }
        match read_frame::<_, Response>(&mut self.stream) {
            Ok(Response::MdSubscribed { session, accepted, refused, heartbeat_ms }) => {
                // The deadline the heartbeat serves, armed HERE so it cannot be forgotten. The
                // server's own period wins whenever it is SLOWER than this client's floor — the
                // whole reason `heartbeat_ms` rides the frame.
                let deadline =
                    MD_READ_TIMEOUT.max(Duration::from_millis(heartbeat_ms.saturating_mul(3)));
                if let Err(e) = self.stream.set_read_timeout(Some(deadline)) {
                    let msg = format!("md_subscribe: could not arm the stream read deadline: {e}");
                    return Err((self, msg));
                }
                // A push stream is written to, never read from, by the server — clear the request
                // write bound so a large first snapshot is not clipped by a request-shaped number.
                let _ = self.stream.set_write_timeout(None);
                let info = MdSubscribedInfo { session, accepted, refused, heartbeat_ms };
                Ok((info, self.stream))
            }
            Ok(Response::Error(msg)) => Err((self, msg)),
            Ok(other) => {
                let msg =
                    format!("protocol desync: expected MdSubscribed, got {}", resp_kind(&other));
                Err((self, msg))
            }
            Err(e) => {
                let msg = e.to_string();
                Err((self, msg))
            }
        }
    }

    /// **Change an existing session's subscription set** — the ordinary request/response half.
    ///
    /// ⚠ Send this on a SHORT-LIVED connection of its own, never on the stream socket
    /// [`md_subscribe`](Self::md_subscribe) returned: that socket's server side has left its read
    /// loop and a frame written there is never read.
    pub fn md_update(
        &mut self,
        session: MdSessionId,
        add: Vec<MdSpec>,
        remove: Vec<MdSpec>,
    ) -> Result<MdUpdatedInfo, String> {
        // The same door as `md_subscribe`, over the ADD list only: a `remove` naming a key the
        // session never held is a silent no-op by design, and `MdHub::update`'s remove loop never
        // reaches `acquire` — so validating it would refuse a request that costs the server a map
        // lookup. See `crate::market::validate_md_symbol`.
        refuse_bad_symbols("md_update", &add)?;
        if !self.features.iter().any(|f| f == FEATURE_MARKET_DATA) {
            return Err(format!(
                "datahub server does not advertise `{FEATURE_MARKET_DATA}` (advertised: {:?}) —                  nothing was sent.",
                self.features
            ));
        }
        let request = Request::MdUpdate { session, add, remove };
        write_frame(&mut self.stream, &request).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::MdUpdated { accepted, refused, released } => {
                Ok(MdUpdatedInfo { accepted, refused, released })
            }
            Response::Error(msg) => Err(msg),
            other => Err(format!("protocol desync: expected MdUpdated, got {}", resp_kind(&other))),
        }
    }
}
