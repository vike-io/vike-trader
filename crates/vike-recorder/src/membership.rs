//! `membership` — which symbols currently belong to a subscribed family, and the
//! [`vike_data::GroupResolver`] built from that.
//!
//! **This is the piece a family subscription actually needs, and it exists because membership is not
//! static.** A Polymarket up/down family's token ids change every 5 minutes: each window is a new
//! market with new outcome tokens, so a customer cannot subscribe by token id — the ids they picked
//! would be dead before the next flush. They subscribe to the FAMILY, and something has to keep
//! answering "which symbols is that right now".
//!
//! A venue with static symbols (Binance perps) answers the same question with a filter and simply
//! never changes its answer. Spec §8.2's "families are general" decision is exactly the claim that
//! one interface covers both, with rotation as the general case and static as the degenerate one.
//!
//! The map is read on the recorder's WRITER thread (every flush consults the resolver) and written
//! by whatever refreshes membership, so it is behind an `RwLock`: reads are frequent and
//! uncontended, refreshes are rare.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use vike_data::GroupResolver;

/// How long a DEPARTED symbol keeps resolving to its old family.
///
/// Must exceed the recorder's flush horizon — `RecorderConfig::max_age`, 30 s by default — because a
/// buffer can sit that long before aging out. 90 s is three times that, so a rotation under load
/// still drains inside the window. Being generous costs only a few stale map entries, and they are
/// harmless: a retired symbol is no longer subscribed, so nothing NEW can arrive under it.
const RETIRE_GRACE: Duration = Duration::from_secs(90);

/// Live `(venue, symbol) -> family` membership, shared between refreshers and the writer thread.
///
/// Cloning shares the same map — that is the point: a refresher clones one, the resolver clones
/// another, and a rotation is visible to the next flush without re-plumbing anything.
#[derive(Clone, Default)]
pub struct Membership {
    inner: Arc<RwLock<Inner>>,
}

#[derive(Default)]
struct Inner {
    /// Symbols currently IN a family.
    live: HashMap<(String, String), String>,
    /// Symbols that have LEFT but may still have rows buffered in the writer — see
    /// [`Membership::set_family`]. Purged once older than [`RETIRE_GRACE`].
    retired: HashMap<(String, String), (String, Instant)>,
}

impl Membership {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace one family's membership wholesale.
    ///
    /// Wholesale rather than incremental BECAUSE membership rotates: when Polymarket's 14:05 window
    /// opens, the 14:00 tokens are not "removed", they simply are not the family any more. An
    /// incremental API would make forgetting to remove them the easy mistake, and a stale token in
    /// the map would keep routing a dead symbol into the group forever.
    ///
    /// ⚠ **Departing symbols are RETIRED, not forgotten** — they keep resolving to their old family
    /// for [`RETIRE_GRACE`]. Found live on the CI box **90 seconds after the first deploy** (2026-08-02):
    /// a rotation at 11:30:04 produced two stray `symbol=<token>/` quote series at 11:30:**07**. The
    /// departing window's tokens still had rows buffered in `RecorderSink`, and by the time those
    /// buffers flushed the wholesale replace had already unmapped them — so the resolver returned
    /// `None` and the rows landed PER-SYMBOL, splitting the family exactly as `runtime`'s
    /// publish-before-subscribe ordering exists to prevent at the other end.
    ///
    /// `runtime` guarantees membership is published BEFORE a symbol is subscribed. Nothing
    /// guaranteed it survived until that symbol's last rows were WRITTEN. This is that second half.
    ///
    /// Retiring is safe precisely because a departed symbol is also unsubscribed: no NEW rows can
    /// arrive under it, so a retired entry can only route rows that were already in flight.
    ///
    /// Symbols that leave keep whatever they have already written — this only decides where FUTURE
    /// rows are committed, never what the store already holds.
    pub fn set_family(&self, venue: &str, family: &str, symbols: &[String]) {
        let now = Instant::now();
        let fresh: HashSet<&String> = symbols.iter().collect();
        let mut m = self.inner.write().expect("membership lock poisoned");

        let departing: Vec<(String, String)> = m
            .live
            .iter()
            .filter(|((v, s), f)| v == venue && f.as_str() == family && !fresh.contains(s))
            .map(|(k, _)| k.clone())
            .collect();
        for key in departing {
            if let Some(f) = m.live.remove(&key) {
                m.retired.insert(key, (f, now));
            }
        }

        for s in symbols {
            let key = (venue.to_string(), s.clone());
            // A symbol that came BACK (a family that briefly resolved to nothing, then to the same
            // tokens) is live again, not retired.
            m.retired.remove(&key);
            m.live.insert(key, family.to_string());
        }

        m.retired.retain(|_, (_, at)| now.duration_since(*at) < RETIRE_GRACE);
    }

    /// The family a symbol belongs to — live first, then recently RETIRED (see [`set_family`]), so a
    /// departing symbol's still-buffered rows commit into the family they were recorded under rather
    /// than splitting off into a per-symbol series.
    ///
    /// [`set_family`]: Membership::set_family
    pub fn family_of(&self, venue: &str, symbol: &str) -> Option<String> {
        let key = (venue.to_string(), symbol.to_string());
        let m = self.inner.read().expect("membership lock poisoned");
        m.live.get(&key).cloned().or_else(|| {
            m.retired
                .get(&key)
                .filter(|(_, at)| at.elapsed() < RETIRE_GRACE)
                .map(|(f, _)| f.clone())
        })
    }

    /// How many symbols are LIVE — for the daemon's status line and the Data Manager. Retired
    /// symbols are deliberately not counted: they are drain state, not subscriptions.
    pub fn len(&self) -> usize {
        self.inner.read().expect("membership lock poisoned").live.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The [`GroupResolver`] the store's write paths take.
    ///
    /// `None` for an unmapped symbol keeps it PER-SYMBOL, which is what makes this incremental: an
    /// explicit symbol subscription, or a family whose membership has not been resolved yet, records
    /// exactly as it did before grouping existed rather than failing or being dropped.
    pub fn group_resolver(&self) -> GroupResolver {
        let me = self.clone();
        Arc::new(move |venue: &str, symbol: &str| me.family_of(venue, symbol))
    }
}

#[path = "membership_tests.rs"]
#[cfg(test)]
mod membership_tests;
