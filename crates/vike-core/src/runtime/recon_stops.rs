//! POSITION WITHOUT STOP: the reconcile pass's net under a restart that leaves the book resting.
//!
//! With `flags.cancel_orders_on_shutdown` off, a restart leaves every resting order at the venue and
//! drops every core-held protection (an emulated stop, an unreleased bracket exit). Nothing else in
//! the tree says that a position is left with no stop, so each delivered pass asks it of every
//! non-flat position row it carries and says so when the answer turns to no, and again when it turns
//! back (`docs/ops/reconcile-on-restart.md`, "Restarting with the book left resting", is the
//! operator's page).
//!
//! ## What counts as cover
//! A STOP on the opposite side of the position, summed in quantity (a smaller stop is PARTIAL cover
//! and the line names what is left). A take-profit is NOT cover: it closes a winner and leaves the
//! loss open. Four sources, each read through nothing but the two report types and the core's own
//! books, bucketed by (symbol, side) in ONE walk each:
//! - the pass's order report: a live row typed `Stop` or `StopLimit`, or an `Unrecognized` word naming
//!   a stop or a trigger that is not a take-profit ([`is_stop_word`]);
//! - the engine registry: a live `Stop` or `StopLimit` the report did not name (counted once, by coid
//!   or venue order id). The reduce-only flag is NOT tested: the report cannot carry it, and a venue
//!   with no reduce-only (oanda, ig, ibkr, alpaca) could never satisfy it;
//! - an armed conditional (`Stop` or `Trailing`) of the same (venue, symbol) that fires onto THIS
//!   engine;
//! - a held bracket exit (`Stop` or `StopLimit`) of the same (venue, symbol) that releases onto THIS
//!   engine, and only while the engine's OWN book holds a position that exit would close (a SELL
//!   against a long, a BUY against a short). A held exit is released by its entry's fill as the
//!   core folds it live; an entry that fills while the node is down reaches the book as a reconcile
//!   fill, which never releases it. So a held exit with a flat local book is the unreleased-bracket
//!   case (audit gap G1), the very one this line exists to catch, and it is not cover.
//!
//! The last two are core memory: they do not protect while the node is down and a restart drops them
//! unless the journal is on. They count as cover because the node is up when the question is asked.
//! ⚠ Under `quarantine` a registry row can be stale (a stop cancelled at the venue whose event was
//! missed) and keep the position "covered"; the held reconcile alert is the signal for that.
//!
//! ## It errs toward a false alarm, and every line says so
//! The order report of a venue may not list its resting stops (Bybit's parser maps `orderType` only,
//! so a conditional order is not typed as a stop), and the report is all this reads.
//! So every line says, in plain words and for every venue alike, that the report may not list resting
//! stops and that the operator should check the venue before placing another one
//! ([`REPORT_MAY_MISS_STOPS`]). There is no per-venue table here; a claim from the venue's
//! `ReconClient` that its report lists them is the way to suppress it, later. (With the OKX
//! algo-stop read, OKX lists its algo stops typed as `Stop`, so a bare OKX line is a real signal; it
//! does not yet claim the list is complete, so the line stays qualified for it too.)
//! ⚠ Until the Bybit parser types a conditional order as a stop, a Bybit position that DOES have a
//! resting venue stop is named after EVERY restart (the state is in memory), qualified and
//! aggregated as above, and restated hourly while it stays bare.
//!
//! ## What is named, and how often
//! - A position must be bare on TWO consecutive passes (`AccountNaked::pending` is the first) before
//!   it is named: a market maker (vike-mm places no stops) is flat-to-open and back inside one
//!   interval. ⚠ That is one reconcile interval per pass, so with a large
//!   `config.reconcile_interval_ms` "two consecutive passes" is a long time, after every restart too.
//! - ONE line per account per pass for what entered, one for what left, naming the first
//!   [`NAMED_PER_LINE`] (three) symbols and counting the rest; each is a WARN and a recent-events line
//!   (the shipped units log at `warn`, so the clear is a WARN too).
//! - A position that stays bare is not repeated per pass; the account's set is restated once an hour,
//!   on `recon_held`'s summary clock ([`HELD_SUMMARY_INTERVAL_MS`]), so a position bare for days is
//!   not one line that scrolled away. Not for a venue that cannot rest a stop (below): that one is
//!   said once per position.
//! - An EMPTY position report is what a client with no position concept sends, and what a venue sends
//!   for a closed position: one alone judges nothing and clears nothing, a second in a row
//!   ([`EMPTY_PASSES_TO_CLEAR`]) reads as flat.
//! - Never named: a flat or dust position (below the instrument's lot step, judged only on a grid that
//!   belongs to the symbol), and a stopped account's tombstone engine.
//!
//! ## A venue that cannot rest a stop
//! A venue whose caps carry no reduce-only flag or no trigger kind (`vike_venues::caps_for`) cannot
//! hold a protective stop for the position, so naming it on every pass would only be noise. It is
//! judged the same way and said once, in words that say so ("this venue cannot rest a protective stop
//! for this position").
//!
//! ## Where it runs
//! [`CoreThread::note_positions_without_stop`] is called once, from `reconcile_reports`, after the
//! diff and the held-alert step: the reconcile pass cadence, never the per-message fold. Nothing here
//! is reached from `run_loop`, `dispatch`, ingest or publish. The edge state is [`NakedPositions`],
//! held by `recon_held::HeldAnnouncer` (it has that owner's lifetime: reconcile boundary only, one
//! row per account, emptied by an account stop).

use std::collections::{HashMap, HashSet};

use vike_model::accounts::account_keys::label_of_route_key;
use vike_model::events::PositionSide;
use vike_model::{OrderKind, OrderStatusReport, OrderType, PositionStatusReport};

use super::recon_held::HELD_SUMMARY_INTERVAL_MS;
use super::*;

/// The absolute quantity tolerance of every size comparison here: the value
/// `ExecutionEngine::local_view` hands `recon::diff` (`LOCAL_VIEW_QTY_TOL`, `pub(crate)` to
/// vike-exec). A copy, because it cannot be named from this crate; `recon_stops`' tests pin the two
/// equal.
pub(super) const STOP_COVER_QTY_TOL: f64 = 1e-9;

/// The relative part of the tolerance, per unit of the position: a large position's sums carry float
/// noise an absolute 1e-9 cannot absorb.
const STOP_COVER_REL_TOL: f64 = 1e-9;

/// How many symbols one line names (three); the rest are counted.
const NAMED_PER_LINE: usize = 3;

/// How many consecutive EMPTY position reports read as "flat" (two).
const EMPTY_PASSES_TO_CLEAR: u8 = 2;

/// Said on every line that asks the operator to act, for every venue alike.
const REPORT_MAY_MISS_STOPS: &str = "The order report of this venue may not list resting stops \
     (known: bybit); check the venue before placing another stop.";

type Key = (String, PositionSide);

/// One position found short of cover this pass.
struct Gap {
    /// `(symbol, position_side)`: what makes two positions the same one across passes.
    key: Key,
    /// +1 long, -1 short: the position's direction (a `Both` row by the sign of its quantity).
    dir: i32,
    /// The venue's absolute quantity.
    size: f64,
    /// How much of it the stops found cover.
    covered: f64,
}

/// What `NakedPositions::observe` found this pass.
#[derive(Default)]
struct Seen<'g> {
    /// Bare for the second pass running: named now.
    entered: Vec<&'g Gap>,
    /// Said earlier, still bare, and the summary clock is due.
    restate: Vec<&'g Gap>,
    /// Said earlier, covered or gone now.
    left: Vec<Key>,
}

/// One account's edge state.
#[derive(Debug, Default)]
struct AccountNaked {
    /// Named, and not yet covered or closed.
    said: Vec<Key>,
    /// Bare on the last pass, not named yet.
    pending: Vec<Key>,
    /// Consecutive empty position reports.
    empty_passes: u8,
    /// Core-clock ms of the last line said for this account.
    last_ms: i64,
}

/// The positions already said to have no stop, per account (route key): the edge state of the line.
/// Empty on a box with every position covered and no report to judge (one lookup); `observe`
/// allocates the account's key and entry on every pass that carries a position row.
#[derive(Debug, Default)]
pub(crate) struct NakedPositions {
    accounts: HashMap<String, AccountNaked>,
}

impl NakedPositions {
    /// Whether `account` has anything remembered (a named position, a candidate, an empty streak).
    pub(super) fn holds(&self, account: &str) -> bool {
        self.accounts.contains_key(account)
    }

    /// Fold one pass of `account`: `gaps` are the positions found bare, `report_empty` says the
    /// position report carried no row at all, and `may_restate` is false for a venue that cannot
    /// rest a stop (said once per position, never restated).
    fn observe<'g>(
        &mut self,
        account: &str,
        now_ms: i64,
        gaps: &'g [Gap],
        report_empty: bool,
        may_restate: bool,
    ) -> Seen<'g> {
        let st = self.accounts.entry(account.to_string()).or_default();
        if report_empty {
            st.empty_passes = st.empty_passes.saturating_add(1);
            if st.empty_passes < EMPTY_PASSES_TO_CLEAR {
                return Seen::default();
            }
        }
        st.empty_passes = 0;
        let entered: Vec<&Gap> = gaps
            .iter()
            .filter(|g| st.pending.contains(&g.key) && !st.said.contains(&g.key))
            .collect();
        let still: Vec<&Gap> = gaps.iter().filter(|g| st.said.contains(&g.key)).collect();
        let left: Vec<Key> =
            st.said.iter().filter(|k| !gaps.iter().any(|g| &g.key == *k)).cloned().collect();
        let first_seen: Vec<Key> = gaps
            .iter()
            .filter(|g| !st.said.contains(&g.key) && !st.pending.contains(&g.key))
            .map(|g| g.key.clone())
            .collect();
        st.said = still.iter().chain(entered.iter()).map(|g| g.key.clone()).collect();
        st.pending = first_seen;
        if now_ms < st.last_ms {
            st.last_ms = now_ms; // a clock that ran backwards re-arms the summary, it never fires it
        }
        let due = may_restate && now_ms.saturating_sub(st.last_ms) >= HELD_SUMMARY_INTERVAL_MS;
        let restate = if entered.is_empty() && left.is_empty() && due { still } else { Vec::new() };
        if !entered.is_empty() || !left.is_empty() || !restate.is_empty() {
            st.last_ms = now_ms;
        }
        if st.said.is_empty() && st.pending.is_empty() {
            self.accounts.remove(account);
        }
        Seen { entered, restate, left }
    }

    /// Forget `account` (it was stopped: no pass will clear what it was said to hold).
    pub(super) fn forget(&mut self, account: &str) {
        self.accounts.remove(account);
    }
}

impl<C: ExecutionClient> CoreThread<C> {
    /// Judge every non-flat position row of one delivered pass and say the edges (module doc).
    ///
    /// `orders` and `positions` are the pass's own reports (after the restore hook's translation),
    /// `account_key` the route key the pass resolved.
    pub(super) fn note_positions_without_stop(
        &mut self,
        idx: usize,
        venue: &str,
        account_key: &str,
        orders: &[OrderStatusReport],
        positions: &[PositionStatusReport],
    ) {
        // A stopped account's tombstone takes no more passes; if one reaches it anyway it is not named.
        // And with nothing to judge and nothing said, this is one lookup on an empty map.
        let nothing_to_do = positions.is_empty() && !self.recon_announce.naked.holds(account_key);
        if self.is_retired(idx) || nothing_to_do {
            return;
        }
        let gaps = self.stop_gaps(idx, venue, orders, positions);
        let now = self.engine.now_ms;
        let caps = vike_venues::caps_for(venue);
        let can_rest = caps.supports_reduce_only && !caps.trigger_types.is_empty();
        let seen = self.recon_announce.naked.observe(
            account_key,
            now,
            &gaps,
            positions.is_empty(),
            can_rest,
        );
        if seen.entered.is_empty() && seen.restate.is_empty() && seen.left.is_empty() {
            return;
        }
        if !seen.entered.is_empty() {
            let text =
                bare_text("POSITION WITHOUT STOP:", account_key, venue, &seen.entered, can_rest);
            self.say(text);
        }
        if !seen.restate.is_empty() {
            let text = bare_text(
                "POSITION WITHOUT STOP still:",
                account_key,
                venue,
                &seen.restate,
                can_rest,
            );
            self.say(text);
        }
        if !seen.left.is_empty() {
            let symbols: Vec<&str> = seen.left.iter().map(|(s, _)| s.as_str()).collect();
            let text = format!(
                "POSITION WITHOUT STOP cleared: {account_key} {venue}: {} now covered by a stop or no \
                 longer open: {}",
                count_of(symbols.len()),
                names(&symbols)
            );
            self.say(text);
        }
        self.dirty = true;
    }

    /// One WARN and one recent-events line (the shipped units log at `warn`).
    fn say(&mut self, text: String) {
        tracing::warn!(target: "vike_core::core", "{text}");
        self.note(text);
    }

    /// The positions of this pass that no stop covers: non-flat, not dust, with `size - covered`
    /// beyond the tolerance.
    fn stop_gaps(
        &self,
        idx: usize,
        venue: &str,
        orders: &[OrderStatusReport],
        positions: &[PositionStatusReport],
    ) -> Vec<Gap> {
        if positions.is_empty() {
            return Vec::new();
        }
        let table = self.stop_cover_table(idx, venue, orders);
        let mut gaps = Vec::new();
        for p in positions {
            let size = p.qty.abs();
            if size.is_nan() || size <= STOP_COVER_QTY_TOL || self.is_dust(idx, &p.symbol, size) {
                continue;
            }
            // shortcut: a hedge-mode leg is judged against the opposite side alone, like a one-way
            // position; the order report names no position side to tell the legs' stops apart.
            let dir = match p.position_side {
                PositionSide::Long => 1,
                PositionSide::Short => -1,
                PositionSide::Both if p.qty > 0.0 => 1,
                PositionSide::Both => -1,
            };
            let covered =
                slot(-dir).and_then(|s| table.get(p.symbol.as_str()).map(|c| c[s])).unwrap_or(0.0);
            if size - covered > STOP_COVER_QTY_TOL + STOP_COVER_REL_TOL * size {
                gaps.push(Gap { key: (p.symbol.clone(), p.position_side), dir, size, covered });
            }
        }
        gaps
    }

    /// Whether `size` of `symbol` is below the instrument's lot step: nothing could be sent to
    /// close it, so nothing could stop it. Judged only on a grid that is the symbol's own (the
    /// engine's mounted symbol, or a row the engine holds for it), never on the mount's scalars
    /// lent to another symbol.
    fn is_dust(&self, idx: usize, symbol: &str, size: f64) -> bool {
        let eng = self.eng(idx);
        let limits = &eng.gate.limits;
        let lot = if symbol == eng.symbol {
            limits.lot_size
        } else {
            limits.grid_by_symbol.get(symbol).and_then(|g| g.lot_size)
        };
        lot.is_some_and(|lot| size + STOP_COVER_QTY_TOL < lot)
    }

    /// Whether the engine's OWN book (not the venue's report) holds a position in `symbol` that an
    /// order on `side` would close: a SELL closes a one-way or hedge-mode long, a BUY a short. A held
    /// bracket exit is cover only against such a position (module doc).
    fn local_position_closed_by(&self, idx: usize, symbol: &str, side: i32) -> bool {
        let e = self.eng(idx);
        let size = |ps| e.position_size_of(symbol, ps);
        if side < 0 {
            size(PositionSide::Both) > STOP_COVER_QTY_TOL
                || size(PositionSide::Long) > STOP_COVER_QTY_TOL
        } else {
            size(PositionSide::Both) < -STOP_COVER_QTY_TOL
                || size(PositionSide::Short) < -STOP_COVER_QTY_TOL
        }
    }

    /// The quantity stopped per symbol, as `[buy stops, sell stops]`: the four sources of the module
    /// doc, each walked once.
    fn stop_cover_table(
        &self,
        idx: usize,
        venue: &str,
        orders: &[OrderStatusReport],
    ) -> HashMap<String, [f64; 2]> {
        let mut table: HashMap<String, [f64; 2]> = HashMap::new();
        let mut add = |symbol: &str, side: i32, qty: f64| {
            if let Some(s) = slot(side)
                && qty > 0.0
            {
                table.entry(symbol.to_string()).or_default()[s] += qty;
            }
        };
        let remaining = |qty: f64, filled: f64| (qty - filled).max(0.0);
        // What the report names, by coid and by venue order id (any kind): a registry row it names
        // is the SAME order, counted once, by the report's word (fresher than the registry's).
        let mut reported: HashSet<&str> = HashSet::new();
        for o in orders {
            reported.extend(o.client_order_id.as_deref());
            reported.insert(o.venue_order_id.as_str());
            let stop = is_stop_kind(&o.order_type)
                || matches!(&o.order_type, OrderType::Unrecognized(word) if is_stop_word(word));
            if stop && o.status.is_live() {
                add(&o.symbol, o.side, remaining(o.qty, o.filled_qty));
            }
        }
        reported.remove("");
        for (coid, mo) in self.eng(idx).registry.iter() {
            let r = &mo.request;
            if is_stop_kind(&r.order_type)
                && mo.status.is_live()
                && !reported.contains(coid.as_str())
                && !mo.venue_order_id.as_deref().is_some_and(|v| reported.contains(v))
            {
                add(&r.symbol, r.side, remaining(r.qty, mo.filled_qty));
            }
        }
        for (coid, r) in &self.held_orders {
            if r.venue == venue
                && is_stop_kind(&r.order_type)
                && self.held_release_engine(coid, &r.venue) == idx
                && self.local_position_closed_by(idx, &r.symbol, r.side)
            {
                add(&r.symbol, r.side, r.qty);
            }
        }
        for ((book_venue, symbol), book) in &self.conditional_books {
            if book_venue != venue {
                continue;
            }
            for (id, arm) in book.iter() {
                let w = &arm.order;
                if matches!(w.kind, OrderKind::Stop | OrderKind::Trailing)
                    && self.armed_engine(id, venue) == Some(idx)
                {
                    add(symbol, w.side, w.size);
                }
            }
        }
        table
    }
}

/// The table slot of an order side: 0 for a buy, 1 for a sell, none for anything else.
fn slot(side: i32) -> Option<usize> {
    match side {
        1 => Some(0),
        -1 => Some(1),
        _ => None,
    }
}

/// A kind that stops a loss: `Stop` or `StopLimit`. A `TakeProfit` is not one.
fn is_stop_kind(kind: &OrderType) -> bool {
    matches!(kind, OrderType::Stop | OrderType::StopLimit)
}

/// A word of an `Unrecognized` order type that names a stop, or a trigger that is not a take-profit.
/// A word that names both (cTrader's `stop_loss_take_profit`) is a stop. So a cTrader protection
/// order that holds a take-profit leg alone also reads as cover here, because the word does not say
/// which legs are set; it should become a leg-aware check once the parser exposes the legs.
///
/// shortcut: a substring test over the parsers' lower-cased words (`trailing_stop_market`,
/// `stop_loss`); upgrade when a parser maps its trigger kinds onto ours.
fn is_stop_word(word: &str) -> bool {
    let word = word.to_ascii_lowercase();
    word.contains("stop") || (word.contains("trigger") && !word.contains("profit"))
}

/// A size as a person writes it: ten decimals at most, no trailing zeros (`0.1`, not
/// `0.09999999999999998`).
fn qty(x: f64) -> String {
    let s = format!("{x:.10}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn count_of(n: usize) -> String {
    if n == 1 { "1 position".to_string() } else { format!("{n} positions") }
}

/// The first [`NAMED_PER_LINE`] names, and how many more there are.
fn names(all: &[&str]) -> String {
    let mut out = all.iter().take(NAMED_PER_LINE).copied().collect::<Vec<_>>().join(", ");
    if all.len() > NAMED_PER_LINE {
        out.push_str(&format!(" and {} more", all.len() - NAMED_PER_LINE));
    }
    out
}

/// The `vike-cli` book spelling of an account: `venue` for the default account, `venue/LABEL` for a
/// labelled one (the route key spells it `venue#LABEL`).
fn book_of(venue: &str, account_key: &str) -> String {
    match label_of_route_key(venue, account_key).and_then(|l| l.text().map(|t| t.to_string())) {
        Some(label) => format!("{venue}/{label}"),
        None => venue.to_string(),
    }
}

/// The line for positions that have just turned out to have no (or too little) stop, or that still
/// have none (`lead` tells which). One line for the whole account and pass.
fn bare_text(lead: &str, account: &str, venue: &str, gaps: &[&Gap], can_rest: bool) -> String {
    let listed: Vec<String> = gaps
        .iter()
        .take(NAMED_PER_LINE)
        .map(|g| {
            let side = if g.dir > 0 { "long" } else { "short" };
            let head = format!("{} {side} {}", g.key.0, qty(g.size));
            if g.covered > STOP_COVER_QTY_TOL {
                format!(
                    "{head} (a stop covers {}, {} has none)",
                    qty(g.covered),
                    qty(g.size - g.covered)
                )
            } else {
                head
            }
        })
        .collect();
    let more = match gaps.len().saturating_sub(NAMED_PER_LINE) {
        0 => String::new(),
        n => format!(" and {n} more"),
    };
    let flatten = format!(
        "`vike-cli trade position flatten {} {} --node <host:port>`",
        book_of(venue, account),
        gaps[0].key.0
    );
    let advice = if can_rest {
        format!(
            "If it has none, place a reduce-only stop there, or close the position with {flatten}."
        )
    } else {
        format!(
            "This venue cannot rest a protective stop for this position (no reduce-only stop kind \
             here): only a strategy stop held by this node can cover it, and that protects nothing \
             while the node is down. Close it with {flatten} if it needs protection."
        )
    };
    format!(
        "{lead} {account} {venue}: {} with no full stop: {}{more}. {REPORT_MAY_MISS_STOPS} {advice}",
        count_of(gaps.len()),
        listed.join("; ")
    )
}
