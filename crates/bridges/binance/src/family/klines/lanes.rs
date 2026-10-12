//! The CONCURRENT kline pager: [`walk_forward_spans`] and [`fetch_klines_range_lanes`].

use std::time::Duration;

use vike_model::Bar;
use vike_model::rate_limits::PaceSample;

use super::{
    KlineSpec, MAX_LIMIT, PROBE_PAGES, fetch_exchange_info, fetch_klines_range_paced,
    fetch_page_rate_limited, pacer_for, paged_walk, probe_pages, remaining_pages, seed_pacer,
    walk_forward_pages,
};

/// The CONCURRENT half of the walk, pure and network-free: split `[start_ms, end_ms]` into `lanes`
/// disjoint ascending spans ([`vike_bridge_core::concurrent::split_range`]) and run
/// [`walk_forward_pages`] over each, merged ascending and deduped by `ts`.
///
/// `fetch_page(cursor, window_end)` is the SAME closure shape the sequential walk takes, so the two
/// paths are provably the same walk over the same page source — which is exactly what the
/// `lanes_return_the_same_bars_as_the_sequential_walk` test below asserts, over dense history and
/// over every gap shape (leading, interior, trailing, sparse) that could make them disagree.
///
/// `lanes <= 1` is one span, i.e. one inline [`walk_forward_pages`] on the caller's own thread over
/// the caller's own window — not merely equivalent to the sequential walk but literally it.
///
/// The abort check between pages is what bounds the cost of a failure to one round trip per lane. A
/// lane that stops on it returns a PARTIAL page set, which is sound only because
/// [`vike_bridge_core::concurrent::run_lanes`] sets that flag exclusively when some lane already
/// returned `Err` — so the whole call fails and every lane's rows are discarded.
pub fn walk_forward_spans<F>(
    start_ms: i64,
    end_ms: i64,
    rows_per_page: usize,
    lanes: usize,
    fetch_page: F,
) -> Result<Vec<Bar>, String>
where
    F: Fn(i64, i64) -> Result<Vec<Bar>, String> + Sync,
{
    let spans = vike_bridge_core::concurrent::split_range(start_ms, end_ms, lanes);
    let mut out = vike_bridge_core::concurrent::run_lanes(&spans, |flag, span_start, span_end| {
        walk_forward_pages(span_start, span_end, rows_per_page, |cursor, window_end| {
            if flag.aborted() {
                return Ok(Vec::new());
            }
            fetch_page(cursor, window_end)
        })
    })?;
    // Insurance, not a correction: spans are disjoint and ascending and each walk returns its own
    // window ascending, so this is already sorted and unique. It is the same discipline
    // `vike_bridge_core::klines::walk_backward_pages` applies, and it is what makes "the concurrent result IS
    // the sequential result" a property of this function rather than of the caller.
    out.sort_by_key(|b| b.ts);
    out.dedup_by_key(|b| b.ts);
    Ok(out)
}

/// [`fetch_klines_range_paced`] with BOUNDED CONCURRENCY: the same walk, the same pages, the same
/// aggregate rate — several windows in flight instead of one.
///
/// ## Why this exists (MEASURED, the CI box 2026-08-04)
/// A sequential pager cannot space requests closer together than one request TAKES (~280 ms through
/// the pooled agent). On binance **fapi** that is irrelevant: 2400 weight/min at the 0.40 default and
/// weight-5 pages is a 312 ms target gap, wider than the round trip, so one request at a time already
/// hits the target and this function derives **one lane** and changes nothing. On binance **spot** it
/// is the whole story: 6000 weight/min at weight 2 is a **50 ms** target gap,
/// [`vike_bridge_core::pacer::Pacer::next_delay`] is already floored at 1 ms, and the backfill
/// delivers ~18 % of the budget the operator asked for with no knob left to turn.
///
/// ## What is and is not raised
/// **The target rate is unchanged.** Every lane takes its dispatch slot from ONE shared
/// [`vike_bridge_core::concurrent::LaneGate`], which admits one request per
/// [`vike_bridge_core::pacer::Pacer::target_gap`] however many lanes are waiting — so the venue sees
/// the same weight/min at six lanes as at one, and the pacer's two proactive guards (the hand-set
/// `weight_soft_limit` and `should_cool_down`) pause EVERY lane at once when the counter crosses.
/// What concurrency changes is only that the target becomes *reachable*.
///
/// The lane COUNT is derived from the pacer's own measurement
/// ([`vike_bridge_core::pacer::Pacer::suggested_lanes`] = `ceil(request_time / target_gap)`), capped
/// at `max_lanes` — and the measurement it reads is the one [`PROBE_PAGES`] pages produced, which is
/// why there are TWO of them rather than one. On a cold run with a single probe the estimate is
/// still the pacer's pessimistic seed and spot derives **3** lanes on a **125 ms** gap; measured, it
/// derives 6 on 50 ms.
///
/// ## What this is actually worth (ARITHMETIC, not a measurement)
/// Sequentially, spot manages `60 / (0.280 + 0.001)` = **213 requests/min** — the round trip plus the
/// floored sleep. Concurrently the gate admits one per 50 ms target gap = **1200/min**, so a
/// 24-month 1m spot backfill (1052 pages) goes from **~4.9 min to ~55 s**, about **5.4x**. A longer
/// run settles nearer **4.8x**: once consumption actually reaches the 2400 weight/min target,
/// `should_cool_down` fires near the end of each venue window and adds a `weight_cooldown` pause the
/// ETA does not model, costing roughly 10 s in every 70. Perp is **unchanged** — it derives one lane.
/// ⚠ Both figures are arithmetic from the measured round trip and the published budget. Nothing here
/// has been timed end to end.
///
/// ⚠ **No discovered budget ⇒ the SEQUENTIAL pager, not a one-lane concurrent one.** Discovery is a
/// runtime question: a spec naming an `exchange_info_url` still degrades to a `Fixed` pacer on a
/// network error or an unparseable body, and a `Fixed` pacer's `target_gap` is the caller's hand-set
/// `page_delay` — which a shared gate would space requests by, where the sequential pager spaces them
/// `page_delay + request` apart. Driving the gate off it would be a ~1.5x rate increase against a
/// venue whose ceiling we had just failed to read. So this function checks `is_discovered()` before
/// any lane exists and hands the whole fetch back to `paged_walk`. Aster — whose
/// `exchange_info_url` is `None` because its host is env-resolved — takes the same exit, by the same
/// rule rather than by name.
///
/// ## The probe page
/// The first page is fetched SEQUENTIALLY, exactly as `fetch_klines_range_paced`'s is, for three
/// reasons that all matter: it is what MEASURES the round trip the lane count is a ratio of; it emits
/// the one ETA line; and its last `openTime` is where real history begins, so the split covers only
/// the window that still has data instead of spending lanes on a symbol's pre-listing void. A probe
/// that comes back empty or short ends the fetch with no lane ever spawned.
///
/// ## Ordering, dedup and failure
/// Spans are disjoint and ascending and each lane's walk returns its own window ascending, so the
/// concatenation is already ordered; it is nonetheless sorted and deduped by `ts` (the same
/// insurance `walk_backward_pages` applies), so the result is the identical `Vec<Bar>` the sequential
/// path produces — ascending, no duplicates, clipped to `[start_ms, end_ms]`. **Any lane failure
/// fails the whole call** and discards every row: this window is written under ONE commit key, so a
/// partial result reported as success would mark it permanently ingested.
///
/// `max_lanes <= 1` delegates to [`fetch_klines_range_paced`] before anything else happens — same
/// function, same agent, same requests, same sleeps.
pub fn fetch_klines_range_lanes(
    base: &str,
    symbol: &str,
    interval: &str,
    start_ms: i64,
    end_ms: i64,
    spec: &KlineSpec,
    seed: Option<&PaceSample>,
    max_lanes: usize,
) -> Result<(Vec<Bar>, Option<PaceSample>), String> {
    if max_lanes <= 1 {
        // Not "equivalent to" the sequential path — it IS the sequential path.
        return fetch_klines_range_paced(base, symbol, interval, start_ms, end_ms, spec, seed);
    }
    if end_ms < start_ms {
        return Ok((Vec::new(), None)); // an empty window never touches the venue
    }

    // Sized for the lanes up front: ureq pools only 3 idle connections per host by default, and a
    // surplus lane would pay a fresh TLS handshake per page — which would also inflate the very
    // round-trip measurement the lane count is derived from.
    let agent = vike_bridge_core::http::blocking_agent_for_lanes(max_lanes);
    // ONCE per call, never per page: the budget is a property of the host, not of the window.
    let mut pacer = pacer_for(spec, fetch_exchange_info(&agent, spec, base).as_deref());
    seed_pacer(&mut pacer, spec, seed);

    // ⚠ Discovery is a RUNTIME question, not a static one: a spec that names an `exchange_info_url`
    // still falls back to a `Fixed` pacer on a network error, a non-2xx, or an unparseable body. A
    // `Fixed` pacer's `target_gap` is the caller's hand-set `page_delay`, and a shared gate paced on
    // THAT would space requests `page_delay` apart where the sequential pager spaces them
    // `page_delay + request` apart — a ~1.5x rate increase against a venue whose ceiling we just
    // failed to read. So a discovery miss is not a slower concurrent path; it is the SEQUENTIAL path.
    if !pacer.is_discovered() {
        tracing::debug!(
            target: "vike_binance::family::klines",
            venue = spec.venue,
            symbol,
            "kline lanes: no discovered budget this run ⇒ falling back to the sequential pager"
        );
        return paged_walk(&agent, pacer, base, symbol, interval, start_ms, end_ms, spec);
    }

    // ---- the probe phase: SEQUENTIAL, and every lane decision rests on what it measures ----------
    // Two pages, not one — see `PROBE_PAGES`. This is also the only window in which a used-weight
    // delta is attributable to a request, because it is the only one with a single request
    // outstanding (see `vike_bridge_core::pacer::Pacer::observe_absolute`).
    let mut first = true;
    let mut probe_elapsed = Duration::ZERO;
    let mut probe_weight = None;
    let (mut out, resume) =
        probe_pages(start_ms, end_ms, MAX_LIMIT, PROBE_PAGES, |cursor, window_end| {
            if !first {
                std::thread::sleep(pacer.next_delay()); // the sequential throttle, unchanged
            }
            first = false;
            let started = std::time::Instant::now();
            let (page, used_weight) =
                fetch_page_rate_limited(&agent, base, symbol, interval, cursor, window_end, spec)?;
            let elapsed = started.elapsed();
            // `observe_request`, not `observe_absolute`: one request was outstanding, so the delta
            // between this reading and the previous one IS this request's cost. That is the whole
            // reason the probe is sequential and the whole reason there are two of it.
            pacer.observe_request(used_weight, elapsed);
            probe_elapsed = elapsed;
            probe_weight = used_weight;
            if !page.is_empty()
                && (used_weight.is_some_and(|w| w >= spec.rate_limit.weight_soft_limit)
                    || pacer.should_cool_down())
            {
                std::thread::sleep(spec.rate_limit.weight_cooldown);
            }
            Ok(page)
        })?;
    // `None` = the probe exhausted the window (empty page, short page, or a cursor that could not
    // move forward) — there is nothing left to split and no lane is ever spawned.
    let Some(cursor) = resume else {
        return Ok((out, pacer.measured()));
    };

    let lanes = pacer.suggested_lanes(max_lanes);
    // The ONE line per backfill, now also carrying what the measurement bought.
    let pages = remaining_pages(cursor, end_ms, interval);
    if let Some((pages, eta)) = pages.zip(pages.and_then(|n| pacer.eta(n))) {
        tracing::info!(
            target: "vike_binance::family::klines",
            venue = spec.venue,
            symbol,
            interval,
            discovered = pacer.is_discovered(),
            request_ms = probe_elapsed.as_millis() as u64,
            // MEASURED by the probe's SECOND page, not the pacer's seed constant — that difference
            // is the lane count (3 vs 6 on spot) and the gate's own gap (125 ms vs 50 ms).
            page_weight = pacer.per_request_weight(),
            used_weight = probe_weight,
            target_gap_ms = pacer.target_gap().as_millis() as u64,
            lanes,
            remaining_pages = pages,
            // The SEQUENTIAL ETA — what this backfill would have cost one window at a time. The
            // concurrent one is roughly this over `lanes`, but only roughly (the venue's own history
            // gaps decide how much work each span actually holds), so the honest thing to print is
            // the number that is not a guess.
            sequential_eta_secs = eta.as_secs(),
            "kline backfill pace: measured request time + derived lane count"
        );
    }

    // ---- the lanes -----------------------------------------------------------------------------
    let gate = vike_bridge_core::concurrent::LaneGate::new(pacer);
    let rest = walk_forward_spans(cursor, end_ms, MAX_LIMIT, lanes, |cursor, window_end| {
        gate.admit(); // the shared slot — this is where the aggregate budget is enforced
        let started = std::time::Instant::now();
        let (page, used_weight) =
            fetch_page_rate_limited(&agent, base, symbol, interval, cursor, window_end, spec)?;
        let elapsed = started.elapsed();
        // Observe + BOTH proactive guards, applied to the SHARED schedule so a cooldown pauses every
        // lane once rather than each lane separately.
        gate.complete(
            used_weight,
            elapsed,
            spec.rate_limit.weight_soft_limit,
            spec.rate_limit.weight_cooldown,
        );
        Ok(page)
    })?;

    out.extend(rest);
    // The probe's bars all precede `cursor`, and `walk_forward_spans` already returned its half
    // sorted and deduped — so this is insurance over an already-ordered concatenation, at the one
    // function that PROMISES an ascending, duplicate-free `Vec<Bar>`.
    out.sort_by_key(|b| b.ts);
    out.dedup_by_key(|b| b.ts);
    Ok((out, gate.pacer().measured()))
}
