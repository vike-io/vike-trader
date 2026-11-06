//! The one retry policy every feed `subscribe_*` shares: `retry_backoff` and `FeedRetries`.

use super::*;
use std::time::{Duration, Instant};
use vike_data::LiveDataError;

// ---------------------------------------------------------------------------------------------
// The one retry policy every `subscribe_*` in this module shares.
// ---------------------------------------------------------------------------------------------

/// The first cooldown after a failed subscribe; doubles per consecutive failure up to
/// [`RETRY_MAX`]. One second is chosen so a blip recovers within a delay a human reads as "it came
/// back", not as "it never worked".
pub const RETRY_BASE: Duration = Duration::from_secs(1);

/// The cooldown ceiling. Deliberately 60s — the same steady-state re-check cadence
/// `VIKE_RECONCILE_INTERVAL_MS` defaults to, so this workspace has ONE answer to "how often does a
/// background thing re-ask a venue that is currently saying no".
pub const RETRY_MAX: Duration = Duration::from_secs(60);

/// The pure backoff schedule: `attempts` (`1` = the failure that just happened) ⇒ how long to wait
/// before re-attempting. `1s, 2s, 4s, 8s, 16s, 32s, 60s, 60s, …` — exponential until it reaches
/// the [`RETRY_MAX`] ceiling, then flat **forever**: there is no attempt limit, because a venue
/// that is down for an hour must still be able to come back on its own without the operator
/// restarting the GUI. Saturating throughout, so a pathological attempt count can neither overflow
/// the shift nor the multiply.
pub fn retry_backoff(attempts: u32) -> Duration {
    let steps = attempts.saturating_sub(1).min(20);
    RETRY_MAX.min(RETRY_BASE.saturating_mul(1u32 << steps))
}

/// Which gate a [`FeedRetries`] record shadows.
///
/// The retry logic is identical across lanes; the lane exists so [`reap_orphaned_feeds`] can sweep
/// the SERIES records — the only ones whose "is this series still wanted?" question it can answer —
/// without touching the depth/cockpit records, whose keyspaces (`trade_depth`'s `(venue, symbol)`
/// pairs, `poly_subs`' token ids) it knows nothing about. A lane-tagged key rather than a string
/// prefix precisely because a series key already contains a `"venue:"` prefix of its own, and a
/// sweep that guessed wrong would silently drop a live record.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum RetryLane {
    /// [`ensure_feed_on`]'s kline subscribe and [`ensure_trade_feed_on`]'s trade tape, keyed by the
    /// `spawned` key — so the reaper sweeps it under exactly the rules it sweeps `unroutable` with.
    Series,
    /// [`ensure_depth`]'s `subscribe_depth` leg, keyed `"{venue}:{inst}"` (the `trade_depth`
    /// key). Its gate set already records the leg only on success, so this lane does not *create*
    /// the retry — it THROTTLES one that existed but ran on every frame, i.e. ~60 Hz of re-dialling
    /// (and warning) against a live venue socket.
    TradeDepth,
    /// [`ensure_poly_book`]'s `subscribe_book` leg, keyed by token id (the `poly_subs` key). Same
    /// throttle-only role as [`RetryLane::TradeDepth`].
    PolyBook,
    /// [`ensure_poly_book`]'s SECOND `subscribe_trades` leg, keyed by token id. It gets its own lane
    /// because it has no gate set of its own: `poly_subs` deliberately records the token once the
    /// BOOK is live (the ladder paints), so a failed trade leg had nothing left that could reopen it
    /// and that token's trade tape was permanently absent.
    PolyTrades,
}

/// One failing subscribe leg: its [`RetryLane`] plus the lane-local id. Built through the four
/// named constructors so no call site has to remember which string a lane is keyed by.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct RetryKey {
    lane: RetryLane,
    id: String,
}

impl RetryKey {
    /// A kline or `@trades` series key — the same string `spawned`/`unroutable` hold.
    pub fn series(key: &str) -> RetryKey {
        RetryKey { lane: RetryLane::Series, id: key.to_string() }
    }
    /// A depth leg, keyed exactly like its `trade_depth` entry (venue + venue-NATIVE symbol).
    pub fn trade_depth(venue: &str, inst: &str) -> RetryKey {
        RetryKey { lane: RetryLane::TradeDepth, id: format!("{venue}:{inst}") }
    }
    /// A cockpit book leg, keyed by the Polymarket token id.
    pub fn poly_book(token: &str) -> RetryKey {
        RetryKey { lane: RetryLane::PolyBook, id: token.to_string() }
    }
    /// A cockpit trade-tape leg, keyed by the Polymarket token id.
    pub fn poly_trades(token: &str) -> RetryKey {
        RetryKey { lane: RetryLane::PolyTrades, id: token.to_string() }
    }
}

/// What is outstanding for one leg. Three states, because a subscribe can fail in three
/// meaningfully different ways and collapsing them is what produced either a permanent silence or
/// a ~60 Hz retry loop.
#[derive(Clone, Debug)]
enum RetryState {
    /// The venue has **no registered client** at all (the depth/cockpit lanes only — the series lane
    /// records this in [`FeedSlots::unroutable`], which the reaper owns). Re-checking costs a
    /// hashmap lookup, so this state is ALWAYS due; the record exists only to warn once.
    Missing,
    /// The venue answered [`LiveDataError::Subscribe`]. Retried when `next_at` passes.
    Backoff { attempts: u32, next_at: Instant },
    /// The venue answered [`LiveDataError::Unsupported`] — a DECLARED-capability refusal, derived
    /// from the static `VenueCaps.live_data` matrix by `vike_data::require_live_verb`. It cannot
    /// become true by waiting, so it is never retried; the record is kept so the loss stays
    /// visible rather than being forgotten.
    Refused,
}

impl RetryState {
    fn due_now(&self) -> bool {
        match self {
            RetryState::Missing => true,
            RetryState::Backoff { next_at, .. } => Instant::now() >= *next_at,
            RetryState::Refused => false,
        }
    }
}

/// **The record of every subscribe leg that is currently failing** — one map, one policy, shared by
/// all four `ensure_*` functions.
///
/// # Why this exists
///
/// Four sites claimed their idempotency slot on a path where nothing was actually subscribed:
/// [`ensure_feed_on`] and [`ensure_trade_feed_on`] held `spawned` after a failed `subscribe_*`,
/// [`ensure_depth`] inserted `trade_depth` even when its inner 1m `subscribe_bars` failed, and
/// [`ensure_poly_book`] inserted `poly_subs` even when its `subscribe_trades` failed. Each was a
/// permanent loss for the life of the process — a dead chart, a bar-less paper engine, an absent
/// trade tape — behind at most one `warn!`. #946 fixed the *missing-venue* half of the first two
/// and named this the remaining gap; this closes it, for all four, the same way.
///
/// # The two questions this had to answer
///
/// **Retry cadence.** #946's rule was "retry freely, warn once", and it was right *there* because
/// its retry costs a `HashMap::get` — no venue is touched. A failed subscribe is the opposite: the
/// venue was reached and answered, and every caller here is per-frame, so an unconditional retry is
/// a ~60 Hz socket loop plus the ~60 Hz log loop behind this workspace's 341 GB trace-file
/// incident. So the shared rule generalises both: **the retry rate matches the cost of the
/// attempt** — free re-checks stay per-frame (a missing-venue record here, and
/// [`FeedSlots::unroutable`] in the series lane), venue calls back off ([`retry_backoff`]: 1s →
/// 60s, then flat forever). And **the log rate is one line per state transition**, never per
/// frame: the first failure `warn!`s with the schedule, later failures are `debug!`, and the
/// recovery is one `info!`.
///
/// **Is a transient error distinguishable from a permanent one?** Partly, and the part that is
/// distinguishable is worth acting on. [`LiveDataError`] has exactly two variants:
/// [`LiveDataError::Unsupported`] is a DECLARED-capability refusal — `vike_data::require_live_verb`
/// derives it from the static `vike_venues::caps_for(venue).live_data` matrix — so it is *provably*
/// permanent and retrying it can only ever be noise (recorded, warned once, never re-attempted).
/// [`LiveDataError::Subscribe`] genuinely mixes the transient (the OS
/// refused a feed thread; the socket is down) with the permanent (the venue rejected the symbol up
/// front) and its payload is an opaque `String` this layer must not pattern-match. So it is treated
/// as retryable — but bounded, which is exactly the "treat all as retryable, bound the noise"
/// answer: an eventually-hopeless symbol costs one warn plus one `debug!` per minute, forever,
/// while a genuine blip recovers in a second.
///
/// # Lifetime
///
/// A record is created on failure and removed on success, so the healthy path holds NOTHING
/// (`is_empty()`). The SERIES lane is additionally swept by [`reap_orphaned_feeds`] (step 2e)
/// under the same rules as [`FeedSlots::unroutable`], so it can never become the insert-only set
/// #946 exists to have removed. The depth/cockpit lanes are swept by
/// [`reap_orphaned_trade_cockpit_streams`] under ITS live-window rules — they used to be unsweepable
/// because `trade_depth`/`poly_subs` had no teardown path at all, which is the gap that function
/// closed — so no lane can grow insert-only for the life of the process anymore.
#[derive(Default)]
pub struct FeedRetries {
    inner: HashMap<RetryKey, RetryState>,
}

impl FeedRetries {
    /// Is a record outstanding for this leg — did it fail and not since succeed? The observable
    /// question: `true` means that series/leg is currently delivering nothing.
    pub fn is_pending(&self, key: &RetryKey) -> bool {
        self.inner.contains_key(key)
    }

    /// A record exists **and** its cooldown has elapsed. Used at the sites whose own gate set
    /// (`spawned`, `trade_depth`, `poly_subs`) already says "done", so this is the only thing that
    /// can reopen the attempt.
    pub fn is_retry_due(&self, key: &RetryKey) -> bool {
        self.inner.get(key).is_some_and(RetryState::due_now)
    }

    /// A record exists and its cooldown has **not** elapsed — the exact complement of
    /// [`Self::is_retry_due`] over "a record exists". Used at the sites whose gate set already says
    /// "not done" (so the caller would retry anyway) and this only throttles it.
    pub fn is_blocked(&self, key: &RetryKey) -> bool {
        self.inner.get(key).is_some_and(|st| !st.due_now())
    }

    /// Record that this leg's venue has **no registered client**. Returns `true` the first time,
    /// which is the caller's warn-once gate. Always immediately due afterwards: re-checking a
    /// hashmap costs nothing, so the backoff rule deliberately does not apply (this is the
    /// depth/cockpit twin of what [`FeedSlots::unroutable`] does in the series lane).
    pub fn note_missing(&mut self, key: &RetryKey) -> bool {
        if matches!(self.inner.get(key), Some(RetryState::Missing)) {
            return false;
        }
        self.inner.insert(key.clone(), RetryState::Missing);
        true
    }

    /// Record a subscribe the venue **answered no** to, log it at a bounded rate, and either
    /// schedule the retry or refuse it forever. `what` is the site label the log line opens with
    /// (e.g. `"ensure_feed(BTCUSDT@1m): subscribe_bars"`), built by the caller so the line names
    /// the real key rather than a generic one.
    pub fn note_error(&mut self, key: &RetryKey, err: &LiveDataError, what: &str) {
        if matches!(err, LiveDataError::Unsupported(_)) {
            // Provably permanent (see this type's doc): re-asking a declared-capability refusal can
            // only ever be noise. Recorded rather than forgotten, so the gap stays visible.
            if !matches!(self.inner.get(key), Some(RetryState::Refused)) {
                self.inner.insert(key.clone(), RetryState::Refused);
                tracing::warn!(
                    "{what} refused: {err} — the venue's DECLARED capabilities cannot serve this, \
                     so it is not retried; this series stays empty"
                );
            }
            return;
        }
        let attempts = match self.inner.get(key) {
            Some(RetryState::Backoff { attempts, .. }) => attempts.saturating_add(1),
            _ => 1,
        };
        let delay = retry_backoff(attempts);
        self.inner
            .insert(key.clone(), RetryState::Backoff { attempts, next_at: Instant::now() + delay });
        // One line per state transition: the FIRST failure is the operator-visible event; the
        // repeats are `debug!` so a permanently-broken symbol cannot flood the trace file.
        if attempts == 1 {
            tracing::warn!(
                "{what} failed: {err} — retrying in {}s (doubling, capped at {}s)",
                delay.as_secs(),
                RETRY_MAX.as_secs()
            );
        } else {
            tracing::debug!(
                "{what} failed again (attempt {attempts}): {err} — next retry in {}s",
                delay.as_secs()
            );
        }
    }

    /// Forget this leg's record because it just SUCCEEDED, returning the failed-attempt count it
    /// was holding (`0` for a missing-venue or refused record) so the caller can report the
    /// recovery. `None` = there was nothing outstanding, i.e. the ordinary first-time success.
    pub fn clear(&mut self, key: &RetryKey) -> Option<u32> {
        self.inner.remove(key).map(|st| match st {
            RetryState::Backoff { attempts, .. } => attempts,
            RetryState::Missing | RetryState::Refused => 0,
        })
    }

    /// [`reap_orphaned_feeds`]'s sweep (step 2e): drop every **series-lane** record whose key
    /// nothing wants anymore, leaving the depth/cockpit lanes alone (see [`RetryLane`]). Without it
    /// this map would grow insert-only for the life of the process — the very shape #946 removed
    /// one layer down, re-introduced one layer up.
    pub fn retain_series(&mut self, keep: impl Fn(&str) -> bool) {
        self.inner.retain(|k, _| k.lane != RetryLane::Series || keep(&k.id));
    }

    /// [`reap_orphaned_trade_cockpit_streams`]'s sweep — the depth/cockpit twin of
    /// [`Self::retain_series`], leaving the SERIES lane alone for the mirror reason: each reaper
    /// can answer "is this key still wanted?" only for the keyspaces it knows. `keep` receives the
    /// record's [`RetryLane`] plus its lane-local id (the [`RetryKey`] constructors' `id` strings),
    /// and is never consulted for [`RetryLane::Series`].
    pub fn retain_trade_cockpit(&mut self, keep: impl Fn(RetryLane, &str) -> bool) {
        self.inner.retain(|k, _| k.lane == RetryLane::Series || keep(k.lane, &k.id));
    }

    /// How many legs are currently failing. `0` on the healthy path — a non-zero count is the
    /// number of charts/ladders/tapes delivering nothing right now.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// `true` when nothing is failing.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// TEST ONLY: pretend every scheduled cooldown has already elapsed, so a test can drive the
    /// retry path without sleeping for real seconds. The schedule itself is covered separately by
    /// [`retry_backoff`]'s own unit test, and the "a retry does NOT happen before the cooldown"
    /// property is testable without this (a test runs in microseconds; the floor is one second).
    #[cfg(test)]
    pub(super) fn expire_all(&mut self) {
        let now = Instant::now();
        for st in self.inner.values_mut() {
            if let RetryState::Backoff { next_at, .. } = st {
                *next_at = now;
            }
        }
    }
}
