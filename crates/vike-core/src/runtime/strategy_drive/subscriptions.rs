//! The audience rule: which mounts a market message reaches.
//!
//! This file is the ONE place that says who hears what. Every other strategy lane (the closed bar,
//! feed status, flow, the underlying mark, the cross-venue reference quote, a params update) asks
//! [`CoreThread::mount_hears`] with its own [`Audience`] and acts on a `true`; the per-message tick
//! lane reads a table built from this same rule (decision 0110,
//! `docs/decisions/0110-no-general-message-bus-the-core-routes-strategies-through-one-subscription-index.md`),
//! so no two lanes can disagree about the audience of a message.
//!
//! Not this rule: which mount OWNS a fill or an order event. That is not a subscription at all: the
//! mount that MINTED the order hears it
//! (`crates/vike-core/src/runtime/strategy_drive/broker_drain.rs`'s `mount_for_coid`), and an event
//! no mount minted reaches none (decision 0116). Do not unify the two laws.
//!
//! **The tick table** ([`CoreThread::rebuild_tick_audience`], read through
//! [`CoreThread::tick_audience_of`]) is venue → symbol → the slots that hear a tick about that pair.
//! It holds nothing the rule has not said `true` to (bar a slot a hook panic leaves empty, below),
//! and it is rebuilt wherever the rule's inputs
//! (`mounts`, `mount_symbols`, `any_mount_multi`) change after assembly: at the end of
//! `crates/vike-core/src/runtime/assemble.rs`'s `assemble_core` and LAST in
//! `crates/vike-core/src/runtime/mount_runtime.rs`'s `recompute_mount_gates`, which both runtime
//! mount and unmount end in. ⚠ A new writer of those fields that skips the rebuild leaves a mount
//! DEAF to its ticks (or hearing another's) with nothing failing: end it in `recompute_mount_gates`.
//! A strategy hook's take/restore of its own slot is not such a write: it changes no membership.
//! ⚠ The one case where the table lists a slot the rule rejects is a hook that PANICS: it unwinds
//! between `mounts[idx].take()` and the restore, `crates/vike-core/src/runtime/run_loop.rs`'s
//! `handle` catches it and enters safe-state, the core keeps dispatching, and the slot stays `None`
//! with NO rebuild. The tick lane's `is_some()` re-check on every listed slot is what keeps that
//! stale entry harmless (without it the next tick on the pair would panic in `step_mount_on_tick`
//! and every mount listed after the empty slot would go deaf): do not remove it.
//!
//! Why a table for this lane alone: it is per market message, and the table saves about 5 ns per
//! NON-hearing mount per message (decision 0110's measurement); the rarer lanes scan with the rule.
//!
//! A lane keeps its own loop shape, and that is not this file's business: the bar lane walks the slot
//! indices and the tick lane walks its table's slot list (a step needs `&mut self`, and collecting
//! would allocate per message), the rarer lanes collect the matching slots first, and the params lane
//! resolves ONE addressee
//! (`crates/vike-core/src/runtime/strategy_drive/hooks.rs`'s `params_target`: the mount its `mount_id`
//! names, else the sole mount on the series, and a refusal when the series is shared). The rule only
//! answers "does slot `idx` hear it".

use std::collections::HashMap;

use super::*;

/// Which lane is asking, and so which rule decides whether a mount is a message's audience.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Audience<'a> {
    /// A CLOSED bar of `(venue, symbol, interval)`: the mount's own symbol or a declared leg's, on
    /// the mount's venue, at the mount's interval.
    ///
    /// EVERY such mount, not the first: two mounts may share one series
    /// (`StrategyMount::controller_id` exists to allow it), and a first-match lookup ran only the
    /// first mount's `on_bar` (bug D in `crates/vike-core/src/runtime/tests/multi_mount.rs`).
    /// Together with the series-symbol stamp in `crates/vike-core/src/runtime/dispatch.rs`'s
    /// `BarClose` arm this is what lets a two-leg strategy receive both legs' bars and tell them
    /// apart by `bar.symbol`.
    Bar { interval: &'a str },
    /// A quote / trade / book of `(venue, symbol)`: the mount's own symbol or a declared leg's, on
    /// the mount's venue. Any interval: a tick belongs to no interval.
    ///
    /// ⚠ The MESSAGE's venue must be the mount's own, and a declared leg is matched by its SYMBOL
    /// ALONE: the leg's venue is ignored. So a message of a FOREIGN venue never arrives here, even for
    /// a leg declared on it (`MountLeg::at`), while a `sim` mount with a leg `at("X", "okx")` does hear
    /// `sim`'s own `X`. That is deliberate as to the first half: draining a foreign venue's tick would
    /// hand the strategy a broker whose drain series is the FOREIGN one, and a symbol-less tagged
    /// maker quote buffered there would resolve to, and be placed on, the wrong venue. Cross-venue
    /// touches ride [`Audience::Reference`], which drains on the mount's OWN series.
    Tick,
    /// Feed status or flow toxicity of `(venue, symbol)`: the mount's OWN pair only, at any
    /// interval (a 1m and a 5m mount on one symbol both hear the same feed-death signal). A declared
    /// leg is not consulted: the toxic tape's asset IS the mounted token.
    OwnPair,
    /// A params update addressed to `(venue, symbol, interval)`: the mount's own series exactly.
    /// No declared leg: a `ParamsUpdate` names one series. Only the update WITHOUT a `mount_id`
    /// asks (to find the sole mount on the series, or to find that it is shared); an addressed
    /// update finds its mount by id and asks to confirm the series matches.
    OwnSeries { interval: &'a str },
    /// An underlying mark of `(venue, symbol)`: mounts on that venue whose `underlying_symbol` is
    /// `symbol`, a DIFFERENT symbol from the one they trade.
    Underlying,
    /// A quote on a FOREIGN venue: mounts on another venue that declared `(symbol, venue)` as a leg
    /// (the xEMM reference-price lane). The `!=` on the venue makes this lane DISJOINT from
    /// [`Audience::Tick`], whose venue test is `==`, so one message never reaches one mount through
    /// both and nothing is dispatched twice.
    Reference,
}

impl<C: ExecutionClient> CoreThread<C> {
    /// Does mount slot `idx` hear a message about `(venue, symbol)` on this lane? `false` for a
    /// tombstoned slot. THE rule: every strategy lane asks it, and the tick lane's table is built
    /// from it, so no two lanes can disagree about who hears what.
    ///
    /// `any_mount_multi` (false for every runtime that declares no leg) keeps the declared-leg arm
    /// from being evaluated at all, so a single-symbol runtime pays the same string compares it
    /// always did. [`Audience::Reference`] does not consult it: its callers are gated on
    /// `any_mount_ref` instead.
    pub(crate) fn mount_hears(
        &self,
        idx: usize,
        venue: &str,
        symbol: &str,
        audience: Audience<'_>,
    ) -> bool {
        let Some(m) = self.mounts[idx].as_ref() else {
            return false;
        };
        let leg =
            || self.any_mount_multi && self.mount_symbols[idx].iter().any(|l| l.symbol == symbol);
        match audience {
            Audience::Bar { interval } => {
                m.venue == venue && m.interval == interval && (m.symbol == symbol || leg())
            }
            Audience::Tick => m.venue == venue && (m.symbol == symbol || leg()),
            Audience::OwnPair => m.venue == venue && m.symbol == symbol,
            Audience::OwnSeries { interval } => {
                m.venue == venue && m.symbol == symbol && m.interval == interval
            }
            Audience::Underlying => {
                m.venue == venue && m.underlying_symbol.as_deref() == Some(symbol)
            }
            Audience::Reference => {
                m.venue != venue
                    && self.mount_symbols[idx]
                        .iter()
                        .any(|l| l.symbol == symbol && l.venue.as_deref() == Some(venue))
            }
        }
    }

    /// Rebuild the tick lane's table (`CoreThread::tick_audience`) from [`Self::mount_hears`] with
    /// [`Audience::Tick`].
    ///
    /// For every LIVE slot, in ascending order, the CANDIDATE pairs are `(m.venue, m.symbol)` and
    /// `(m.venue, leg.symbol)` for each declared leg (the leg's own venue is not part of the key:
    /// the tick rule compares the MOUNT's venue). A candidate enters the table only when the rule
    /// says the slot hears it, so the table cannot hold what the rule rejects (`any_mount_multi`
    /// included: the rule reads it, this does not). Every pair the rule accepts for a slot is one of
    /// its candidates, so the table misses nothing either. A slot enters a pair at most once (a leg
    /// naming the mount's own symbol, or two legs naming one symbol, must not step it twice), and
    /// slots are pushed in ascending order, so each list keeps mount order.
    ///
    /// COLD PATH (allocations fine): called at the end of `assemble_core` and last in
    /// `recompute_mount_gates`, after `any_mount_multi` is re-derived, since the rule reads it.
    pub(crate) fn rebuild_tick_audience(&mut self) {
        let mut table: HashMap<String, HashMap<String, Vec<usize>>> = HashMap::new();
        for idx in 0..self.mounts.len() {
            let Some(m) = self.mounts[idx].as_ref() else {
                continue;
            };
            let candidates = std::iter::once(m.symbol.as_str())
                .chain(self.mount_symbols[idx].iter().map(|leg| leg.symbol.as_str()));
            for symbol in candidates {
                if !self.mount_hears(idx, &m.venue, symbol, Audience::Tick) {
                    continue;
                }
                let slots = table
                    .entry(m.venue.clone())
                    .or_default()
                    .entry(symbol.to_string())
                    .or_default();
                if slots.last() != Some(&idx) {
                    slots.push(idx);
                }
            }
        }
        self.tick_audience = table
            .into_iter()
            .map(|(venue, by_symbol)| {
                let by_symbol = by_symbol
                    .into_iter()
                    .map(|(symbol, slots)| (symbol, Arc::<[usize]>::from(slots)))
                    .collect();
                (venue, by_symbol)
            })
            .collect();
    }

    /// The slots that hear a tick about `(venue, symbol)`, in mount order; `None` when no live mount
    /// does. The tick lane's lookup, per market message: two hash lookups and a refcount bump
    /// (`.cloned()` on an `Arc`), no allocation, so the caller can step mounts through `&mut self`
    /// while it holds the list.
    pub(crate) fn tick_audience_of(&self, venue: &str, symbol: &str) -> Option<Arc<[usize]>> {
        self.tick_audience.get(venue).and_then(|by_symbol| by_symbol.get(symbol)).cloned()
    }
}
