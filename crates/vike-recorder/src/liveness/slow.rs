//! The CADENCE rule: a series receiving rows far below the rate its subscription declares.

use std::collections::HashMap;

use vike_data::Liveness;

use super::{SilenceWatch, due_now};

/// One series that IS receiving rows and is receiving far too few of them.
///
/// The fault [`Silent`] is structurally blind to: a binance perp depth lane ran at 4 % of its
/// declared cadence for forty days and never went 30 s without a row, so every recency check read
/// healthy while a backtest read a book that teleported every 4.3 seconds.
///
/// [`Silent`]: super::Silent
#[derive(Debug, Clone, PartialEq)]
pub struct Slow {
    /// `"{kind}/{venue}/{symbol}"`, as in [`Silent::series`].
    ///
    /// [`Silent::series`]: super::Silent::series
    pub series: String,
    /// Ingest items per second measured over the completed window.
    pub observed_per_s: f64,
    /// The rate this series should be running at — see [`SilenceWatch::slow_series`] for how it is
    /// derived, and why it is not simply the subscription's ceiling.
    pub expected_per_s: f64,
    /// [`CADENCE_FLOOR_FRACTION`] of [`Self::expected_per_s`] — what `observed_per_s` came in
    /// under.
    pub floor_per_s: f64,
    /// The SIBLING series whose activity licensed the verdict, and its own rate. Carried in the
    /// alert body because "the tape was busy and the book was not" is the whole diagnosis, and
    /// without it an operator cannot tell a broken lane from a dead market.
    pub governor: String,
    pub governor_per_s: f64,
    /// The measured span of the window, and the items counted in it — the raw evidence, so a
    /// reader never has to trust the division.
    pub window_ms: i64,
    pub items_in_window: u64,
}

/// How long a cadence verdict is taken over.
///
/// **Fifteen minutes, and every shorter candidate was rejected for a measured reason.** The binance
/// trade tape has a per-second p50 of 4 against a max of 2,056 and 356–581 of every 3,600 seconds
/// carrying no trade at all, so any window measured in ticks reads ordinary quiet as a fault. It is
/// also 3x `crates/bridges/binance/src/family/market_feed.rs`'s `DEPTH_RESEED_INTERVAL` (300 s),
/// which forces a reconnect and a REST re-seed on a HEALTHY lane — a 300 s window would beat 1:1
/// with that and some windows would contain a whole dip. ⚠ That constant belongs to the BRIDGE and
/// not to this crate, which an earlier draft of this paragraph got wrong: bybit's and okx's own
/// `market_feed.rs` each carry a separate copy at the same value, so a venue could change its
/// re-seed cadence without anything here noticing. The 3x is sized against binance's, because
/// binance is the only venue this check judges today.
///
/// And it is 3x the 300 s silence default, which keeps the two judgements from racing: anything the
/// RECENCY watchdog catches, it catches first.
///
/// The cost is stated rather than hidden: the fault this exists for is only visible fifteen minutes
/// after a recorder starts, and it ran for forty days.
pub const CADENCE_WINDOW_MS: i64 = 900_000;

/// The fraction of a series' expected rate below which it is judged SLOW.
///
/// ⚠ **A JUDGEMENT with a measured separation under it, not a derivation** — the same shape
/// `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md` §12.7 uses for
/// `MD_LAPSE_BUDGET`, and it is stated that way so nobody re-derives a number that was never
/// derived.
///
/// What IS measured is the gap the fraction has to sit inside, for the one lane there is data for:
///
/// * BROKEN — 0.41–0.43 items/s (§12.2), and up to about **0.9/s** once the §B status markers are
///   counted, because `crates/vike-data/src/rec/live_rec.rs`'s `stream_status` has routed depth
///   `GapStart`/`Stale`/`LiveResume` through the depth lane since 2026-09-10 and they reach
///   `ingest` like any other item. Marker inflation moves a BROKEN lane UP toward the floor and
///   leaves a healthy one alone, so it is the side the margin has to be spent on.
/// * HEALTHY — **9.8 applied diffs/s** (295 applied of 309 frames in 30 s, sampled live on
///   2026-09-10; `crates/bridges/binance/src/family/depth.rs` carries it).
///
/// At `0.20` of a 10/s ceiling the floor is 2.0/s: **2.2x above the marker-inflated broken rate and
/// 4.9x below the healthy one.** `0.10` would leave the broken side 1.08x — inside the noise the
/// markers alone can produce, i.e. a check that the next reconnect loop could walk straight past.
pub const CADENCE_FLOOR_FRACTION: f64 = 0.20;

/// One completed window's verdict for one series — `None` for "not slow" AND for "not judgeable",
/// which [`SilenceWatch::slow_series`] deliberately treats identically.
///
/// A free function rather than a method so it borrows nothing of the watch: the caller is midway
/// through mutating the episode set, and threading `&mut self` through the judgement would make the
/// borrow checker the reason the code is shaped the way it is.
pub(super) fn judge(
    key: &str,
    observed: f64,
    items: u64,
    window_ms: i64,
    rates: &HashMap<&str, (f64, u64)>,
) -> Option<Slow> {
    let row = vike_data::store::series_cadence::cadence_for_series_key(key)?;
    // `None` for every class but a sampled lane with a DECLARED interval. That is the one gate
    // between this check and an invented threshold — see the cadence table's module doc.
    let ceiling = row.cadence.ceiling_per_s()?;
    let governor = governor_key(key)?;
    let &(governor_per_s, _) = rates.get(governor.as_str())?;
    // The licence: the instrument's own tape has to be busier than the sampler before a
    // ceiling-rate publish is an honest expectation.
    if governor_per_s < ceiling {
        return None;
    }
    let floor_per_s = CADENCE_FLOOR_FRACTION * ceiling;
    if observed >= floor_per_s {
        return None;
    }
    Some(Slow {
        series: key.to_string(),
        observed_per_s: observed,
        expected_per_s: ceiling,
        floor_per_s,
        governor,
        governor_per_s,
        window_ms,
        items_in_window: items,
    })
}

/// The SIBLING trade tape of a series key — `depth/binance/BTCUSDT.P` -> `trade/binance/BTCUSDT.P`.
///
/// `None` when the key is already a trade lane (a tape cannot govern itself) or is not a three-part
/// series key.
fn governor_key(series: &str) -> Option<String> {
    let mut parts = series.splitn(3, '/');
    let kind = parts.next()?;
    let venue = parts.next()?;
    let symbol = parts.next()?;
    (kind != "trade").then(|| format!("trade/{venue}/{symbol}"))
}

impl SilenceWatch {
    /// **The series running far below the cadence their subscription declares** — the rate half of
    /// the watchdog, and the fault [`check`](Self::check) cannot see.
    ///
    /// Same inputs as [`check`](Self::check) plus the window, so the caller passes nothing new.
    ///
    /// Windows TILE on one clock shared by every series (they do not slide, and they are not
    /// per-key): a verdict is taken only on the tick a window closes, and only for series that were
    /// present when it opened. Sliding would re-judge the same deficit every tick and turn one
    /// fault into thirty; per-key windows would let a lane and its governor phase-drift apart until
    /// the lane was never judged at all — see the comment on the window clock inside.
    ///
    /// # How an expectation is derived, and why it is not just the ceiling
    ///
    /// `vike_data::store::series_cadence` declares a CEILING for a sampled lane — binance depth subscribes
    /// `@depth@100ms`, so at most 10 publishes/s. It deliberately declares no FLOOR, because a
    /// sampled stream publishes only when the underlying CHANGED, and how often that happens is a
    /// property of the instrument. `crates/bridges/binance/src/family/market_feed.rs`'s
    /// `DEPTH_FRESHNESS_THRESHOLD` carries the measurement that kills the naive design: the
    /// market's thinnest actively-traded pairs gapped 44–47 s — "a near-dead pair updated only
    /// twice in 5 min", i.e. **0.0067 updates/s, sixty times slower than the lane this exists to
    /// catch**. An absolute floor sized to catch 0.42/s pages forever on every thin symbol a family
    /// glob resolves, and a pager that fires forever is a pager that gets muted.
    ///
    /// So the ceiling is a hypothesis and the SIBLING TRADE TAPE is the licence to test it:
    ///
    /// > A sampled lane is judged only while its own instrument's tape ran at or above the sampling
    /// > ceiling for the whole window. Then, and only then, the expectation IS the ceiling.
    ///
    /// Every trade changes the book, so a tape at ≥ 10 prints/s means the book had something to
    /// report in most 100 ms buckets and a ceiling-rate publish is the honest expectation. Below
    /// that, no verdict at all — which is exactly the thin-pair case, the quiet-market case, the
    /// venue-outage case (the tape dies with the book, and the RECENCY watchdog owns a total death)
    /// and the no-tape-subscribed case, all silenced by ONE rule rather than four exemptions.
    ///
    /// ⚠ **The residual, declared rather than implied.** The tape-changes-the-book argument holds
    /// on average and is violated by CLUSTERING: a tape whose prints all land inside a few hundred
    /// 100 ms buckets conflates many trades into few diffs. The fifteen-minute window and the 5x
    /// gap between the floor and a healthy rate absorb the clustering the one measured tape
    /// actually shows (84–99 % of its seconds carry a print), but a hypothetical instrument
    /// trading ≥ 9,000 times in fifteen minutes and moving its book fewer than 1,800 times would
    /// raise a false alarm. Nothing in the store shows that shape; it is not measured either way.
    ///
    /// # What it costs to be wrong in the other direction
    ///
    /// A lane whose tape is quiet goes unjudged, so this catches nothing on a thin symbol. That is
    /// the deliberate trade: the failure it exists for was on the busiest binance perp there is,
    /// and a rate collapse on an instrument nobody trades is indistinguishable from an instrument
    /// nobody trades.
    ///
    /// # ⚠ Two more accepted costs, declared because neither is obvious from the code
    ///
    /// **A lane that stops DEAD beside a busy tape pages TWICE.** Pass 1 below skips a key absent
    /// from `live` — never received a row — precisely so it does not page beside
    /// [`check`](Self::check), but a key that received rows and then STOPPED is still in `live`,
    /// still anchored, and measures 0 items/s. So it raises `recorder-series-stale` at
    /// `--silent-secs` and `recorder-series-slow` at the next window close: one fault, two rule
    /// ids, about ten minutes apart. Left as is rather than suppressed. Suppressing would mean
    /// consulting the recency verdict from here, and a suppression bug in the ONLY check for a
    /// rate collapse costs more than a duplicate page — while the second alert is not redundant,
    /// because it is the one that names the governor and so says the instrument was still trading.
    ///
    /// **A flapping governor can outrun the repeat gate.** An instrument whose tape hovers around
    /// the ceiling alternates judged / not-judged, and a `None` verdict CLOSES the slow episode
    /// (see the `None` arm below), which drops the series from `slow_alerted` and re-arms it. So a
    /// genuinely broken lane on a marginal instrument can page once per window rather than once
    /// per `repeat_secs`. This is the same trade the `None` arm argues for and it is the one path
    /// that can exceed the repeat budget; it cannot affect a HEALTHY lane, which is never judged
    /// slow in the first place.
    pub fn slow_series(
        &mut self,
        expected: &[String],
        live: &HashMap<String, Liveness>,
        now_ms: i64,
        window_ms: i64,
    ) -> Vec<Slow> {
        if window_ms <= 0 {
            return Vec::new();
        }
        self.anchors.retain(|k, _| expected.iter().any(|e| e == k));
        self.slow_now.retain(|k| expected.iter().any(|e| e == k));

        // ⚠ ONE window clock for the whole watch, not one per series — and this is a CORRECTNESS
        // property, not tidiness. A per-key window starts when that key's first row arrives, so a
        // lane whose first row lands on a different tick from its governor's is permanently
        // phase-shifted: their windows never close on the same tick, the governor's rate is never
        // available when the lane is judged, and the lane is therefore NEVER JUDGED — silently,
        // forever. That is the failure this whole feature exists to remove, reappearing inside it.
        // Caught by `crates/vike-recorder/tests/cadence_alert.rs`'s
        // `the_thinnest_measured_binance_pair_never_pages`, whose depth lane is quiet enough that
        // its first row lands four ticks after its tape's.
        let start = *self.window_start_ms.get_or_insert(now_ms);
        let span = now_ms.saturating_sub(start);
        let closing = span >= window_ms;
        // A wall-clock STEP makes `span` meaningless — `Liveness::last_ms` and `now_ms` both read
        // `vike_model::now_ms`, so an NTP correction lands between the anchor and here.
        // Discard the window and re-anchor rather than divide by a number nobody measured.
        let stepped = span > window_ms.saturating_mul(4);

        // Pass 1 — measure every series that has been present since this window OPENED, and
        // re-anchor everything when it closes. EVERY expected series is measured, not just the
        // sampled ones: a sampled lane's verdict needs its governor's rate over the SAME window.
        let mut rates: HashMap<&str, (f64, u64)> = HashMap::new();
        for key in expected {
            // Absent from `live` = never received a row. That is `check`'s fault to report, and
            // reporting it here as "0 items/s" would page twice for one thing.
            let Some(l) = live.get(key) else {
                self.anchors.remove(key);
                continue;
            };
            let Some((at_ms, rows)) = self.anchors.get(key).copied() else {
                self.anchors.insert(key.clone(), (now_ms, l.rows));
                continue;
            };
            if !closing {
                continue;
            }
            // `at_ms > start` = this series joined MID-window, so its count covers less than the
            // window and its rate would read low. Skip it and let the re-anchor below put it on
            // the shared boundary, where it is judged from the next window on.
            if at_ms <= start && !stepped {
                let items = l.rows.saturating_sub(rows);
                rates.insert(key.as_str(), (items as f64 * 1_000.0 / span as f64, items));
            }
            self.anchors.insert(key.clone(), (now_ms, l.rows));
        }

        if !closing {
            return Vec::new();
        }
        self.window_start_ms = Some(now_ms);

        // Pass 2 — judge the sampled lanes whose governor was measured over this same window.
        let mut out: Vec<Slow> = Vec::new();
        for key in expected {
            // A closed window is the only thing that can START or END an episode, so a series that
            // was not measured leaves `slow_now` exactly as it was.
            let Some(&(observed, items)) = rates.get(key.as_str()) else { continue };
            match judge(key, observed, items, span, &rates) {
                Some(slow) => {
                    self.slow_now.insert(key.clone());
                    out.push(slow);
                }
                // Not slow, OR not judgeable at all — and the two must behave the SAME here: a
                // lane whose tape goes quiet after a slow episode has no verdict either way, and
                // holding its episode open would suppress the alert for its next real one.
                None => {
                    self.slow_now.remove(key);
                }
            }
        }
        out.sort_by(|a, b| a.series.cmp(&b.series));
        out
    }

    /// Which of `slow` should raise an ALERT right now — [`alertable`](Self::alertable)'s twin, on
    /// its own map so the two faults cannot suppress each other.
    pub fn slow_alertable(&mut self, slow: &[Slow], now_ms: i64, repeat_ms: i64) -> Vec<Slow> {
        // ⚠ The two sets differ here, and that is the whole point — see [`due_now`]. `slow_now` is
        // who is still IN a slow episode (it persists between windows); `slow` is only who
        // completed a window THIS tick, which is one tick in thirty.
        let in_episode: Vec<String> = self.slow_now.iter().cloned().collect();
        let names: Vec<String> = slow.iter().map(|s| s.series.clone()).collect();
        let due = due_now(&mut self.slow_alerted, &in_episode, &names, now_ms, repeat_ms);
        slow.iter().filter(|s| due.iter().any(|d| d == &s.series)).cloned().collect()
    }
}
