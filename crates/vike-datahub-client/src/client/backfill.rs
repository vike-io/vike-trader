//! `DatahubClient`'s backfill verbs: backfill-on-demand (`backfill`), the running-backfill registry
//! (`list_backfills`, `cancel_backfill`) and the history-channels read (`history_channels`) — each
//! capability-checked and refused locally, sending nothing, when the server does not advertise it
//! (`docs/decisions/0112-an-additive-verb-is-negotiated-by-a-feature-string-never-by-a-version-bump.md`).
//! `use super::*` reaches the parent module's imports and shared helpers.

use super::*;

impl DatahubClient {
    /// Backfill-on-demand: ask the SERVER to fetch `(venue, symbol, interval)` history — through
    /// whichever of [`Request::Backfill`]'s three lanes serves it: klines, Dukascopy's
    /// tick-resampled bars, or the funding-rate series — over the inclusive `[start_ms, end_ms]`
    /// epoch-ms range into ITS store, and return the [`BackfillDone`] outcome once the rows are
    /// written ("clients request, never fetch"). Synchronous per request over a BOUNDED range (see
    /// [`Request::Backfill`] for why there is no job/progress story).
    ///
    /// ⚠ Refused CLIENT-SIDE, sending nothing, unless the server advertised [`FEATURE_BACKFILL`],
    /// which only a `backfill-serve` build with collectors mounted does. Both a transport failure
    /// and a server-side [`Response::Error`] surface through the ONE `Err(String)` channel.
    ///
    /// ⚠ An `interval` of `vike_data::source::FUNDING_INTERVAL` is checked a SECOND time, against
    /// [`FEATURE_BACKFILL_FUNDING`]: a server predating the funding lane refuses that label as an
    /// unmeasurable bar step, which would read as a verdict on funding rather than on the server.
    pub fn backfill(
        &mut self,
        venue: &str,
        symbol: &str,
        interval: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<BackfillDone, String> {
        if !self.features.iter().any(|f| f == FEATURE_BACKFILL) {
            return Err(format!(
                "datahub server does not advertise `{FEATURE_BACKFILL}` (advertised: {:?}) — \
                 nothing was sent. Backfill needs a server built with `--features backfill-serve` \
                 (or a newer server; this verb is capability-negotiated, not version-gated).",
                self.features
            ));
        }
        if interval == vike_data::source::FUNDING_INTERVAL
            && !self.features.iter().any(|f| f == FEATURE_BACKFILL_FUNDING)
        {
            return Err(format!(
                "datahub server does not advertise `{FEATURE_BACKFILL_FUNDING}` (advertised: \
                 {:?}) — nothing was sent. A funding-rate backfill (`VENUE:SYMBOL:funding`) needs \
                 a `backfill-serve` server from a release that carries the funding lane.",
                self.features
            ));
        }
        let request = Request::Backfill {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            interval: interval.to_string(),
            start: start_ms,
            end: end_ms,
        };
        write_frame(&mut self.stream, &request).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::BackfillDone(done) => Ok(done),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected BackfillDone, got {}", resp_kind(&other)))
            }
        }
    }

    /// Whether this server advertised [`FEATURE_BACKFILL_CANCEL`] — the operator's door onto running
    /// backfills ([`Self::list_backfills`], [`Self::cancel_backfill`]).
    pub fn serves_backfill_cancel(&self) -> bool {
        self.features.iter().any(|f| f == FEATURE_BACKFILL_CANCEL)
    }

    /// The ONE local refusal both backfill-registry verbs give a server that does not advertise
    /// [`FEATURE_BACKFILL_CANCEL`].
    fn refuse_without_backfill_cancel(&self, verb: &str) -> Result<(), String> {
        if self.serves_backfill_cancel() {
            return Ok(());
        }
        Err(format!(
            "datahub server does not advertise `{FEATURE_BACKFILL_CANCEL}` (advertised: {:?}) — \
             nothing was sent, so no {verb} happened. Either the server predates the verbs that list \
             and stop a running backfill, or it mounts no collector table (a build without \
             `--features backfill-serve`) and so runs no backfill to list or stop. On a server that \
             predates them, a restart of the data daemon is the only stop for a running backfill.",
            self.features
        ))
    }

    /// **List every `Backfill` this server is running** — [`Request::ListBackfills`], on every
    /// connection rather than this one, in the order the server began them. An empty list is the
    /// truthful "nothing is running".
    ///
    /// ⚠ Refused LOCALLY, sending nothing, unless the server advertised
    /// [`FEATURE_BACKFILL_CANCEL`]. A `VerbScope::Read` verb, so an Observe connection may send it
    /// (`docs/decisions/0101-cancelling-a-backfill-is-a-control-verb-served-wherever-backfill-is.md`).
    pub fn list_backfills(&mut self) -> Result<Vec<RunningBackfill>, String> {
        self.refuse_without_backfill_cancel("listing")?;
        write_frame(&mut self.stream, &Request::ListBackfills).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::RunningBackfills(running) => Ok(running),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected RunningBackfills, got {}",
                resp_kind(&other)
            )),
        }
    }

    /// **Stop every running `Backfill` on `venue`/`symbol`/`interval`** at its next chunk boundary —
    /// [`Request::CancelBackfill`]. Returns at once, with which requests were flagged and which no
    /// cancel can stop ([`BackfillCancelDone`]); it does not wait for any of them to stop, so a later
    /// [`Self::list_backfills`] is how a caller watches them go.
    ///
    /// Nothing stored is touched: every chunk a request committed before its boundary stays, and
    /// repeating that request resumes after it. The cancelled request's OWN client is answered with
    /// a `Response::Error` naming the cancel.
    ///
    /// ⚠ Refused LOCALLY, sending nothing, unless the server advertised
    /// [`FEATURE_BACKFILL_CANCEL`]. A `VerbScope::Write` verb: an Observe connection to a KEYED
    /// server is refused by the server, and a key-less loopback server serves it as it serves
    /// `Backfill` (0101's verdict 1).
    pub fn cancel_backfill(
        &mut self,
        venue: &str,
        symbol: &str,
        interval: &str,
    ) -> Result<BackfillCancelDone, String> {
        self.refuse_without_backfill_cancel("cancel")?;
        let request = Request::CancelBackfill {
            venue: venue.to_string(),
            symbol: symbol.to_string(),
            interval: interval.to_string(),
        };
        write_frame(&mut self.stream, &request).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::BackfillsCancelled(done) => Ok(done),
            Response::Error(msg) => Err(msg),
            other => Err(format!(
                "protocol desync: expected BackfillsCancelled, got {}",
                resp_kind(&other)
            )),
        }
    }

    /// Whether this server advertised [`FEATURE_HISTORY_CHANNELS`] — whether it is NEWER than the
    /// history-channels read. A build fact: a server that advertises it answers whatever it mounts.
    pub fn serves_history_channels(&self) -> bool {
        self.features.iter().any(|f| f == FEATURE_HISTORY_CHANNELS)
    }

    /// **Every roster venue's history channels as this server's build declares them, with the
    /// server's overlay** — [`Request::HistoryChannels`]. The request names nothing; see
    /// [`crate::history`] for what the reply is and is not.
    ///
    /// ⚠ Refused LOCALLY, sending nothing, unless the server advertised
    /// [`FEATURE_HISTORY_CHANNELS`]. A caller that gets that refusal has a server OLDER than the
    /// verb, and renders [`crate::history::compiled_report`] under
    /// [`crate::history::COMPILED_TABLE_CAPTION`] — this client's own table, the overlay marked not
    /// known — rather than an error. [`Self::serves_history_channels`] asks first, without a round
    /// trip. A `VerbScope::Read` verb, so an Observe connection may send it
    /// (`docs/decisions/0102-the-history-channels-read-is-an-observe-verb.md`).
    pub fn history_channels(&mut self) -> Result<HistoryChannelsReport, String> {
        if !self.serves_history_channels() {
            return Err(format!(
                "datahub server does not advertise `{FEATURE_HISTORY_CHANNELS}` (advertised: {:?}) \
                 — nothing was sent. The server predates the history-channels read; this client's \
                 own compiled table is the answer it can give, without the server's overlay.",
                self.features
            ));
        }
        write_frame(&mut self.stream, &Request::HistoryChannels).map_err(|e| e.to_string())?;
        match read_frame::<_, Response>(&mut self.stream).map_err(|e| e.to_string())? {
            Response::HistoryChannels(report) => Ok(report),
            Response::Error(msg) => Err(msg),
            other => {
                Err(format!("protocol desync: expected HistoryChannels, got {}", resp_kind(&other)))
            }
        }
    }
}
