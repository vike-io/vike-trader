//! The `Backfill` verb family: the Control-scope store WRITE that runs a collector inline
//! (`backfill_verb`), and the operator's door onto what is running (`cancel_backfill_verb`, plus the
//! refusal both registry verbs share, `no_backfill_registry`). `handle_request` (the parent module)
//! routes the three requests here; the request's stop probe is the parent's `StopProbe`, which this
//! verb hands to the collector and reads the peer address from.

use super::*;

/// The `Backfill` verb: validate the bounded range and the interval, dispatch venue → collector
/// through the mounted [`BackfillTable`], then read the range BACK through the served store handle
/// — the write-through proof — and answer [`Response::BackfillDone`]. Every failure (no table,
/// unmeasurable interval, zero-width interval, unknown venue, inverted range, collector error,
/// read-back error) is a clean [`Response::Error`], so the client always learns the outcome; a
/// partial `BackfillDone` is never sent. The ONE request that answers nothing is one whose client
/// has gone — the next section.
///
/// # ⚠ A request whose client has gone STOPS, and answers by closing
///
/// The collector runs inline, for as long as its window takes, and nothing else reads the socket
/// meanwhile — so the request's [`StopProbe`] goes to the collector as its fifth argument
/// (`crate::backfill::BackfillFn`'s doc), and a CHUNKED collector asks it between chunks: a
/// non-blocking peek that reads a FIN as gone and a pipelined frame as present. Once it has seen
/// the client gone, the ingest stops at that boundary with every chunk before it stored, this verb
/// logs ONE info line — the series, the window, and the collector's own account of the chunks it
/// stored, the rows it wrote and that repeating the request resumes — and the connection closes
/// WITHOUT a reply ([`Step::Close`]): nobody would read one. The read-back below does not run.
/// `docs/superpowers/specs/2026-10-01-backfill-cancel-on-client-drop-design.md` §1–§3 is the
/// argument, and the probe's own doc carries what it cannot see (a half-OPEN peer) and what it
/// reads too eagerly (a half-CLOSE, which cancels).
///
/// A ONE-BATCH lane — the keyless kline fold and the funding lane — never asks the probe, so it
/// runs to its end exactly as before, whoever is still listening; so does a chunked request whose
/// client stays. Nothing about the wire moved: no frame is written before the reply, `BackfillDone`
/// is unchanged and so is `PROTO_VERSION`.
///
/// # ⚠ An OPERATOR'S cancel stops it too — and is ANSWERED
///
/// While its collector runs, the request is REGISTERED in the table's registry
/// (`crate::backfill::BackfillTable::register`), which is what `Request::ListBackfills` lists and
/// `Request::CancelBackfill` flags from another connection. The probe the collector gets is the
/// COMPOSED one, `cancelled || peer gone`, latched: the cancel flag is asked first (it costs no
/// syscall) and, once raised, is never lowered. Each ask also counts one boundary, the listing's
/// progress figure. A request a cancel stopped differs from one whose client left in exactly one
/// way: its client is still THERE, so it gets a [`Response::Error`] naming the cancel and carrying
/// the collector's own account — the chunks stored, the rows written, and that repeating the
/// request resumes (the design's §3 and Q5). Never `BackfillDone`: the window is not in the store.
/// A collector that finished before it reached a boundary at which the flag was up answers
/// `BackfillDone` as usual — the window IS whole, and a cancel that stopped nothing is no reason to
/// say otherwise. The registration is dropped as soon as the collector returns, and its `Drop` is
/// what removes the entry on a PANIC too.
///
/// # ⚠ The read-back asks for the range's EDGES, never for its bars
///
/// The proof reports two numbers — the range's first and last stored `ts` — and the window it reads
/// is the CLIENT's, so it can be years of a fine interval. Reading it back with
/// `HistStore::load_bars` built every row of that window inside this daemon, whose memory cap is
/// shared with the market-data and recorder planes, to look at two of them: one long request could
/// OOM-kill the process. It goes through `HistStore::bar_edges` instead, which `DataFusionHist` —
/// the store this daemon serves — answers from the `ts` column alone
/// (`crates/vike-datahub/tests/backfill_readback.rs`'s `the_backfill_readback_never_loads_the_range`
/// pins that this verb no longer reaches `load_bars`). What the proof proves narrows with it: the
/// rows are committed and readable through the served handle, not that every column of them
/// decodes — `bar_edges`' own doc says so.
///
/// The reply is byte-identical to the load-based one for every lane, `first_ts`/`last_ts` included:
/// `crates/vike-data/tests/reads/bar_edges.rs` holds the store's answer equal to the edges derived from
/// `load_bars`, and `crates/vike-datahub/tests/backfill_readback.rs` holds the verb's reply equal
/// to them through a real store.
///
/// # The funding lane
///
/// An `interval` of `vike_data::source::FUNDING_INTERVAL` (`"funding"`) is a reserved LABEL, not a
/// step: it routes to `venue`'s [`crate::backfill::BackfillLane::Funding`] entry — the market
/// funding-rate series, one row per `crate::backfill::FUNDING_SOURCES` source — instead of a bar
/// lane. It is exempt from the forming-bar refusal below, and it ALONE is: a funding point is a
/// settled event with no close time, while every other unmeasurable step is still refused. Its
/// unknown-venue refusal names the FUNDING set rather than the bar one, because the two differ and
/// a bar-venue list would send an operator to a venue with no funding source. A client learns the
/// lane exists from [`FEATURE_BACKFILL_FUNDING`], never from this refusal.
///
/// # ⚠ The interval check, and why it is a REFUSAL rather than validation hygiene
///
/// `vike_backfill`'s shared ingest (`klines::ingest_klines`) drops the still-forming last candle
/// through `drop_forming_tail`, which measures the step with `vike_model::time::interval_ms` — and
/// that parser splits on a SINGLE trailing character over `s`/`m`/`h`/`d`. `1w`, `1M` and `1mo`
/// have no width there, so the guard **declines to act** (its own pinned test,
/// `unparseable_interval_leaves_bars_untouched`, says so: an unknown interval must never panic or
/// drop data, so the decision belongs to the caller). The caller is this verb.
///
/// ⚠ **The seam decides too now, and this verb's refusal is still not redundant.** 0059 Phase 1 put
/// the same refusal into `vike_backfill::klines::ingest_klines`, so a request that got past here
/// would be refused one layer down rather than writing a forming bar. Two things keep this one: it
/// answers BEFORE the venue lookup, so the message is about the step rather than about a collector
/// (`the_interval_refusal_precedes_the_venue_lookup` pins that ordering), and it answers before a
/// collector is entered at all, which on a synchronous per-request verb is the difference between a
/// refusal and a held connection. Both spell the one predicate,
/// `vike_model::time::measures_bar_step`, so they cannot drift about WHICH steps are refused.
///
/// What that costs if nobody decides: the venue's still-open weekly candle is stored as a CLOSED
/// bar, and the window's commit key is spent. `vike_data::DataFusionHist`'s `commit_rows` checks
/// `has_commit` before anything else, so a corrective re-fetch of the same window returns `Ok(0)`
/// and reports success over the wrong row. There is no verb, flag or helper in this workspace that
/// retires a single commit key — the only remedy is destroying the whole `(venue, symbol,
/// interval)` series. And on a KEY-LESS server this verb is served while `delete_series_verb` is
/// withheld (`crate::datahub_cli`'s module doc, per
/// `docs/decisions/0050-a-key-less-datahub-serves-no-delete-verb.md`), on the argument that
/// "a backfill writes rows a re-fetch restores" — which is exactly the premise that fails here.
///
/// So the refusal sits where the seed verb's already does (`validate_seed_interval`, a narrower
/// allowlist on a narrower surface). This was the one automated dispatch path with no gate at all,
/// which is why 0059 Phase 2 could not widen the table without adding it — and since
/// docs/decisions/0094 deleted the collector supervisor, whose roster validator refused the same
/// steps at startup, it is the only refusal that answers before the collector seam.
///
/// ⚠ It is deliberately NOT a per-venue interval table —
/// `docs/decisions/0059-bars-and-ticks-for-every-venue-are-two-asks-not-one.md`'s Phase 1 owns
/// that, and a whole-verb refusal of an interval the STORE cannot measure is a different claim
/// from "this venue serves that step". Nor is it `SEED_INTERVALS`: that set is narrower still and
/// belongs to an OBSERVE-scoped verb; this one is Control-scoped and an operator asking for `3h`
/// on deribit is asking for something real.
pub(super) fn backfill_verb(
    venue: &str,
    symbol: &str,
    interval: &str,
    (start, end): (i64, i64),
    table: Option<&BackfillTable>,
    store: &Arc<dyn HistStore + Send + Sync>,
    probe: &StopProbe,
) -> Response {
    let Some(table) = table else {
        return Response::Error(format!(
            "backfill: not compiled into this build — rebuild vike-datahub with \
             `--features backfill-serve`. (It is a Cargo feature because the collectors are \
             heavy: they pull the venue bridge crates into this binary.) The `{FEATURE_BACKFILL}` \
             capability is deliberately absent from this server's Welcome.features."
        ));
    };
    if start > end {
        return Response::Error(format!(
            "backfill: inverted range — start {start} > end {end} (v1 takes one bounded \
             inclusive epoch-ms range per request)"
        ));
    }
    // THE FUNDING LANE. `interval=funding` is a reserved LABEL, not a step
    // (`vike_data::source::FUNDING_INTERVAL`): a funding point is a settled event with no close
    // time, so the forming-bar refusal below does not apply to it.
    let funding = interval == vike_data::source::FUNDING_INTERVAL;
    // THE FORMING-BAR REFUSAL. Before the venue is looked up, so an operator gets the same answer
    // whichever venue they named — the `seed_series_verb` ordering rule — and before anything is
    // fetched, because the row this prevents cannot be taken back.
    if !funding && !vike_model::time::measures_bar_step(interval) {
        return Response::Error(format!(
            "backfill: interval {interval:?} has no bar width this store can measure \
             (`vike_model::time::interval_ms` reads a count plus one of s/m/h/d, so `1w`, `1M` and \
             `1mo` are outside it). Refused HERE, before any venue is dispatched to, because the \
             collectors' still-forming-candle guard silently DECLINES on an interval it cannot \
             measure: the venue's open candle would be stored as a closed bar and the window's \
             commit key spent, making a corrective re-fetch a silent zero-row success. Ask for a \
             step the store can measure, or resample from one."
        ));
    }
    // THE ZERO-WIDTH REFUSAL, for every lane. `0m` MEASURES — `interval_ms` reads it as `Some(0)` —
    // so the forming-bar refusal above lets it through, but a bucket of no width is no bar: the
    // tick lane's resample would divide by it, and no kline venue serves such a step. Before the
    // venue lookup, for the same reason as the refusal above.
    if vike_model::time::interval_ms(interval) == Some(0) {
        return Response::Error(format!(
            "backfill: interval {interval:?} has zero width — a bar of no duration is no bar, so \
             there is nothing to fetch or resample. Refused before any venue is dispatched to; \
             nothing was fetched and no commit key was spent. Ask for a positive step such as `1m`."
        ));
    }
    let Some((lane, collector)) = table.get_with_lane(venue, interval) else {
        let (what, set) = if funding {
            ("funding-rate collector", table.funding_supported())
        } else {
            ("collector", table.supported())
        };
        return Response::Error(format!(
            "backfill: venue `{venue}` has no {what} in this build. Supported: [{}]",
            set.join(", ")
        ));
    };
    // THE REGISTRY. Only a request that is about to RUN is registered — every refusal above has
    // answered already — and it stays listed exactly while its collector runs: `registration`'s
    // `Drop` removes it below, or on the unwind if the collector panics.
    let registration = table.register(venue, symbol, interval, (start, end), probe.peer, lane);
    let running = registration.request();
    // Set when the probe said "stop" because of the OPERATOR's flag rather than the peer.
    let stopped_by_cancel = Cell::new(false);
    // THE COMPOSED PROBE: `cancelled || peer gone`, latched by both halves. The flag first — it is
    // an atomic load where the other half is three syscalls — and every ask counts one boundary.
    let should_stop = || {
        running.reached_a_boundary();
        if running.cancelled() {
            stopped_by_cancel.set(true);
            return true;
        }
        probe.should_stop()
    };
    // Collector runs INLINE (v1 is synchronous per request; the collectors page + pace
    // internally), writing through the same store this server serves — under the request's stop
    // probe, which a chunked collector asks between chunks (see the doc above).
    let outcome = collector(symbol, interval, start, end, &should_stop);
    // It has stopped running, whatever it answered: out of the registry before anything else, so a
    // cancel that arrives during the read-back below finds nothing to flag rather than a request
    // that can no longer stop.
    drop(registration);
    // THE CLIENT IS GONE. Whatever the collector answered, nobody is there to read it: one info line
    // at the request boundary, and `dispatch` turns this into `Step::Close` on the probe's latch, so
    // the `Response` below is never written. No read-back — it would prove a write to nobody.
    if probe.peer_gone() {
        let account = match &outcome {
            Ok(rows) => format!("the collector finished first: {rows} rows written"),
            Err(e) => e.clone(),
        };
        tracing::info!(
            peer = ?probe.peer,
            venue,
            symbol,
            interval,
            start,
            end,
            "vike-datahub: backfill STOPPED at a chunk boundary — its client closed the connection \
             while it ran, so no reply is written and the connection closes. Every chunk before the \
             boundary stays stored, and repeating the request resumes there. The collector's \
             account: {account}"
        );
        return Response::Error(format!(
            "backfill {venue}/{symbol}@{interval} [{start}, {end}]: stopped — its client closed \
             the connection while it ran (nothing failed; repeating the request resumes). \
             {account}"
        ));
    }
    // AN OPERATOR CANCELLED IT. The client is still here, so it is ANSWERED — with an error, never
    // `BackfillDone`, because the window is not in the store. The collector's own text is the
    // account of what IS: its stop names the boundary, the chunks stored and the rows written.
    // (An `Ok` here means the collector reached its end without stopping on the flag: the window is
    // whole and the ordinary answer below is the true one.)
    if stopped_by_cancel.get()
        && let Err(account) = &outcome
    {
        tracing::info!(
            peer = ?probe.peer,
            venue,
            symbol,
            interval,
            start,
            end,
            "vike-datahub: backfill STOPPED at a chunk boundary — CANCELLED by an operator \
             (`CancelBackfill` from another connection); its client is answered with the cancel. \
             Every chunk before the boundary stays stored, and repeating the request resumes there. \
             The collector's account: {account}"
        );
        return Response::Error(format!(
            "backfill {venue}/{symbol}@{interval} [{start}, {end}]: CANCELLED by an operator \
             (`CancelBackfill`) and stopped at a chunk boundary — nothing failed: every chunk \
             before the boundary is stored, and repeating the request resumes there. {account}"
        ));
    }
    let rows_written = match outcome {
        Ok(n) => n as u64,
        Err(e) => {
            return Response::Error(format!("backfill {venue}/{symbol}@{interval} failed: {e}"));
        }
    };
    // Write-through proof + the client's seam bookkeeping: what the requested range now holds,
    // asked of the SERVED handle — the same series, range, parts and row filter the client's
    // follow-up LoadBars would read. ⚠ EDGES, never bars: this window is the CLIENT's and can span
    // years of a fine interval, and loading it would hold every row in this daemon's memory only to
    // read two timestamps off the ends (see the doc above, and `HistStore::bar_edges`).
    match store.bar_edges(venue, symbol, interval, TsRange { start: Some(start), end: Some(end) }) {
        Ok(edges) => Response::BackfillDone(BackfillDone {
            rows_written,
            first_ts: edges.first_ts,
            last_ts: edges.last_ts,
        }),
        Err(e) => Response::Error(format!(
            "backfill {venue}/{symbol}@{interval}: collector wrote {rows_written} rows but the \
             read-back failed: {e}"
        )),
    }
}

/// The refusal a server with NO collector table answers both registry verbs with — it runs no
/// backfill, so there is nothing to list and nothing to stop, and `served_features` withholds
/// [`FEATURE_BACKFILL_CANCEL`] for the same reason. An error rather than an empty list, so "this
/// server cannot run a backfill at all" never reads as "nothing is running".
///
/// ⚠ It must not mention a SCOPE: a key-less server answers it to every caller, and
/// `crates/vike-datahub/tests/auth_roundtrip.rs` reads a scope word in a key-less answer as a
/// refusal on authentication grounds.
pub(super) fn no_backfill_registry(verb: &str) -> String {
    format!(
        "{verb}: this datahub mounts no collector table, so it runs no Backfill to list or stop — \
         the `{FEATURE_BACKFILL_CANCEL}` capability is deliberately absent from its \
         Welcome.features. Nothing was changed."
    )
}

/// **The `CancelBackfill` verb**: raise the cancel flag of every running request on one series
/// that can stop, and answer at once with which it flagged and which it could not
/// (`crate::backfill::BackfillTable::cancel` does the work; this verb adds the door and the log).
///
/// It does not wait: a flagged request stops at its NEXT chunk boundary on its own connection's
/// thread, and answers ITS client there (see [`backfill_verb`]'s cancel section). Nothing stored is
/// touched, which is why the verb is Control and not keys-only, and is served on a key-less
/// loopback server exactly as `Backfill` is —
/// `docs/decisions/0101-cancelling-a-backfill-is-a-control-verb-served-wherever-backfill-is.md`.
/// The scope check happened before this was reached, in `handle_connection`.
///
/// ONE info line per cancel — an operator's act on other connections' work is worth the line, and
/// it is at a request boundary, not per frame.
pub(super) fn cancel_backfill_verb(
    venue: &str,
    symbol: &str,
    interval: &str,
    table: Option<&BackfillTable>,
    peer: Option<SocketAddr>,
) -> Response {
    let Some(table) = table else {
        return Response::Error(no_backfill_registry("CancelBackfill"));
    };
    let done = table.cancel(venue, symbol, interval);
    let ids = |rows: &[vike_datahub_client::proto::RunningBackfill]| {
        rows.iter().map(|r| r.id).collect::<Vec<_>>()
    };
    tracing::info!(
        ?peer,
        venue,
        symbol,
        interval,
        flagged = ?ids(&done.flagged),
        unstoppable = ?ids(&done.unstoppable),
        "vike-datahub: CancelBackfill — the flagged requests stop at their next chunk boundary; \
         the unstoppable ones run to their end ({BACKFILL_ONE_BATCH})"
    );
    Response::BackfillsCancelled(done)
}
