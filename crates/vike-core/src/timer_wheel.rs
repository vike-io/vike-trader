//! [`DeadlineTimerWheel`] — an allocation-free, deterministic deadline timer facility (audit co6).
//!
//! One O(1)-insert / O(1)-cancel timer wheel that the runtime advances ONCE per drain-loop
//! boundary (never per message — the p99<10µs core fold is untouched). It is the single facility
//! meant to consolidate the ad-hoc timers (stuck-order watchdog, data-freshness, dead-man,
//! order-TTL); this increment wires exactly ONE of them (the stuck-order watchdog sweep cadence,
//! see `runtime.rs`), keeping the rest as follow-ups.
//!
//! **Deterministic — no wall clock inside.** Every method that needs "now" takes it as an injected
//! `now_ms` (epoch milliseconds), matching the codebase's `Clock::now_ms`-fed style. The wheel is a
//! pure function of its inserts/cancels and the `now_ms` values passed to [`DeadlineTimerWheel::advance`];
//! it reads no clock of its own, so a replay/harness driving it with recorded stamps reproduces the
//! exact expiries.
//!
//! **Structure — a hashed timing wheel over a slab.** Deadlines are quantized to ticks of
//! `tick_ms` (the resolution) and hashed into `num_slots` (a power of two) intrusive doubly-linked
//! lists carved out of one pre-allocated node slab with a free list. Insert reuses a free node
//! (no allocation on the steady path); cancel is an O(1) unlink; the slab grows only when more
//! timers are concurrently armed than ever before.
//!
//! **Advance cost.** A monotonically-maintained lower bound on all armed deadlines (`min_tick`)
//! makes the common boundary advance — "no timer is due yet" — a single integer compare, O(1),
//! regardless of how many timers are armed. When something IS due, expiry walks only the crossed
//! slots (bounded by `num_slots`) and emits the expired payloads; the exact `min_tick` is then
//! re-established by one slot scan (O(num_slots + armed)). Firing is the rare path (the wheel is
//! advanced far more often than a timer fires), so this keeps the hot boundary check at O(1).
//!
//! **Ordering.** Within one [`DeadlineTimerWheel::advance`] call, expired payloads are appended to
//! the caller's buffer in nondecreasing deadline order; payloads sharing a tick come out in an
//! unspecified-but-deterministic order.

/// Sentinel "no node" index for the intrusive lists and the free list.
const NIL: u32 = u32::MAX;

/// A handle to one armed timer, returned by [`DeadlineTimerWheel::insert`] and consumed by
/// [`DeadlineTimerWheel::cancel`]. Carries a generation so a stale id (whose slab slot has since
/// been reused by a different timer) is rejected rather than cancelling the wrong timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimerId {
    idx: u32,
    generation: u32,
}

/// One slab node: a timer when `in_use`, otherwise a free-list link. `next`/`prev` are the
/// intrusive doubly-linked slot list while armed, and `next` alone is the free-list link when free.
struct Node<T> {
    /// absolute deadline in ticks; meaningful only while `in_use`.
    deadline_tick: i64,
    next: u32,
    prev: u32,
    /// bumped on every free, so a `TimerId` minted before the free no longer matches.
    generation: u32,
    in_use: bool,
    /// `Some` while armed; `take`n on expiry/cancel.
    item: Option<T>,
}

impl<T> Default for Node<T> {
    fn default() -> Self {
        Node { deadline_tick: 0, next: NIL, prev: NIL, generation: 0, in_use: false, item: None }
    }
}

/// An allocation-free hashed timing wheel keyed by absolute `now_ms` deadlines. See the module docs.
pub struct DeadlineTimerWheel<T> {
    /// resolution: one tick = `tick_ms` milliseconds (deadlines quantized up to the next tick).
    tick_ms: i64,
    /// `num_slots - 1`; `num_slots` is a power of two so `slot = tick as usize & mask`.
    mask: usize,
    /// per-slot intrusive-list head (`NIL` when empty). Length `num_slots`, allocated once.
    slots: Box<[u32]>,
    /// the node slab; grows only past the historical high-water-mark of concurrent timers.
    nodes: Vec<Node<T>>,
    /// free-list head (`NIL` when the slab is fully in use).
    free_head: u32,
    /// number of currently-armed timers.
    len: usize,
    /// last tick advanced to (all armed deadlines are `> cursor_tick` after any advance).
    cursor_tick: i64,
    /// whether [`Self::advance`] has been called yet (first call pins the cursor).
    cursor_init: bool,
    /// a valid LOWER BOUND on every armed deadline tick (exact after any firing advance);
    /// `i64::MAX` when empty. The O(1) "nothing due yet" fast path compares against this.
    min_tick: i64,
    /// reusable scratch for the rare full-rotation expiry pass (kept to avoid steady-state alloc).
    scratch: Vec<u32>,
}

impl<T> DeadlineTimerWheel<T> {
    /// Build a wheel with `tick_ms` resolution and `num_slots` buckets. `num_slots` is rounded up to
    /// a power of two (min 2); `tick_ms` must be `>= 1`. A deadline never fires EARLY — it is
    /// quantized up to the next tick, so a coarser `tick_ms` only makes a timer fire up to `tick_ms`
    /// late. Panics on `tick_ms < 1` (a programmer error, not a runtime condition).
    pub fn new(tick_ms: i64, num_slots: usize) -> Self {
        assert!(tick_ms >= 1, "tick_ms must be >= 1");
        let num_slots = num_slots.max(2).next_power_of_two();
        DeadlineTimerWheel {
            tick_ms,
            mask: num_slots - 1,
            slots: vec![NIL; num_slots].into_boxed_slice(),
            nodes: Vec::new(),
            free_head: NIL,
            len: 0,
            cursor_tick: 0,
            cursor_init: false,
            min_tick: i64::MAX,
            scratch: Vec::new(),
        }
    }

    /// Number of currently-armed timers.
    pub fn len(&self) -> usize {
        self.len
    }

    /// True when no timer is armed — the runtime's boundary gate, so advancing an empty wheel is
    /// skipped entirely (the default path stays byte-identical to a wheel-free runtime).
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The slab's node count — the allocation-free-steady-state probe used by tests: it only grows
    /// past the historical peak of concurrently-armed timers, never on the reuse path.
    pub fn node_capacity(&self) -> usize {
        self.nodes.len()
    }

    /// Ceil-quantize a millisecond deadline to a tick, so a timer never fires before its deadline.
    fn to_tick_ceil(&self, ms: i64) -> i64 {
        if ms <= 0 {
            0
        } else {
            (ms - 1) / self.tick_ms + 1
        }
    }

    /// Floor-quantize a millisecond "now" to the tick it belongs to.
    fn to_tick_floor(&self, ms: i64) -> i64 {
        if ms <= 0 {
            0
        } else {
            ms / self.tick_ms
        }
    }

    /// Arm a timer for `deadline_ms` (epoch ms) carrying `item`; returns its [`TimerId`]. O(1),
    /// allocation-free once the slab has reached its concurrent-timer peak. A deadline already in
    /// the past (at/behind the cursor) is clamped to fire on the next [`Self::advance`].
    pub fn insert(&mut self, deadline_ms: i64, item: T) -> TimerId {
        let mut dt = self.to_tick_ceil(deadline_ms);
        if self.cursor_init && dt <= self.cursor_tick {
            dt = self.cursor_tick + 1; // never strand a past deadline in a not-yet-visited slot
        }
        let idx = self.alloc_node();
        {
            let node = &mut self.nodes[idx as usize];
            node.deadline_tick = dt;
            node.in_use = true;
            node.item = Some(item);
            node.prev = NIL;
        }
        let slot = (dt as usize) & self.mask;
        let head = self.slots[slot];
        self.nodes[idx as usize].next = head;
        if head != NIL {
            self.nodes[head as usize].prev = idx;
        }
        self.slots[slot] = idx;
        self.len += 1;
        if dt < self.min_tick {
            self.min_tick = dt;
        }
        TimerId { idx, generation: self.nodes[idx as usize].generation }
    }

    /// Cancel an armed timer, returning its payload if `id` is still live (unmatched generation or
    /// an already-fired/cancelled timer returns `None`). O(1) — an intrusive doubly-linked unlink.
    /// A cancel may leave `min_tick` a loose (still valid) lower bound; the next firing advance
    /// re-tightens it, so cancel never scans.
    pub fn cancel(&mut self, id: TimerId) -> Option<T> {
        let i = id.idx as usize;
        if i >= self.nodes.len() {
            return None;
        }
        let (in_use, generation) = {
            let n = &self.nodes[i];
            (n.in_use, n.generation)
        };
        if !in_use || generation != id.generation {
            return None;
        }
        self.unlink(id.idx);
        let item = self.nodes[i].item.take();
        self.free_node(id.idx);
        item
    }

    /// Advance the wheel to `now_ms`, appending every timer whose deadline is `<= now_ms` to `out`
    /// (in nondecreasing deadline order) and removing them. Time only moves forward: a `now_ms`
    /// at/behind the current cursor is a no-op. This is the ONLY method the runtime calls per
    /// drain-loop boundary; the "nothing due yet" case is a single compare (O(1)).
    pub fn advance(&mut self, now_ms: i64, out: &mut Vec<T>) {
        let target = self.to_tick_floor(now_ms);
        if !self.cursor_init {
            self.cursor_init = true;
            self.cursor_tick = target;
            if self.min_tick <= target {
                self.full_pass(target, out);
                self.min_tick = self.recompute_min();
            }
            return;
        }
        if target <= self.cursor_tick {
            return; // no forward progress
        }
        if target < self.min_tick {
            self.cursor_tick = target; // nothing can be due — the O(1) hot path
            return;
        }
        let span = target - self.cursor_tick;
        if span >= self.slots.len() as i64 {
            // the crossed span covers at least a full rotation — visit every slot exactly once.
            self.full_pass(target, out);
        } else {
            // walk only the crossed ticks' slots, in ascending order (⇒ ascending deadline).
            let mut t = self.cursor_tick + 1;
            while t <= target {
                self.expire_tick(t, target, out);
                t += 1;
            }
        }
        self.cursor_tick = target;
        self.min_tick = self.recompute_min();
    }

    /// Expire the due nodes hashed to tick `t`'s slot (those with `deadline_tick <= target`),
    /// appending their payloads to `out`. Used on the bounded stepping path.
    fn expire_tick(&mut self, t: i64, target: i64, out: &mut Vec<T>) {
        let slot = (t as usize) & self.mask;
        let mut cur = self.slots[slot];
        while cur != NIL {
            let next = self.nodes[cur as usize].next;
            if self.nodes[cur as usize].deadline_tick <= target {
                self.unlink(cur);
                if let Some(item) = self.nodes[cur as usize].item.take() {
                    out.push(item);
                }
                self.free_node(cur);
            }
            cur = next;
        }
    }

    /// Full-rotation expiry: collect every due node across all slots, emit in deadline order.
    /// Only taken when a single advance crosses a whole rotation (a long idle gap) — rare, so the
    /// sort is affordable; `scratch` is reused to keep it allocation-free on the steady path.
    fn full_pass(&mut self, target: i64, out: &mut Vec<T>) {
        self.scratch.clear();
        for slot in 0..self.slots.len() {
            let mut cur = self.slots[slot];
            while cur != NIL {
                let next = self.nodes[cur as usize].next;
                if self.nodes[cur as usize].deadline_tick <= target {
                    self.scratch.push(cur);
                }
                cur = next;
            }
        }
        // ascending deadline order for a stable, sorted emission across the whole batch.
        let nodes = &self.nodes;
        self.scratch.sort_by_key(|&i| nodes[i as usize].deadline_tick);
        let n = self.scratch.len();
        for k in 0..n {
            let idx = self.scratch[k];
            self.unlink(idx);
            if let Some(item) = self.nodes[idx as usize].item.take() {
                out.push(item);
            }
            self.free_node(idx);
        }
        self.scratch.clear();
    }

    /// Exact minimum armed deadline tick (or `i64::MAX` when empty). O(num_slots + armed); called
    /// only after a firing advance to re-tighten `min_tick`.
    fn recompute_min(&self) -> i64 {
        let mut m = i64::MAX;
        for slot in 0..self.slots.len() {
            let mut cur = self.slots[slot];
            while cur != NIL {
                let n = &self.nodes[cur as usize];
                if n.deadline_tick < m {
                    m = n.deadline_tick;
                }
                cur = n.next;
            }
        }
        m
    }

    /// Unlink a node from its slot's doubly-linked list (does NOT free it).
    fn unlink(&mut self, idx: u32) {
        let (prev, next, dt) = {
            let n = &self.nodes[idx as usize];
            (n.prev, n.next, n.deadline_tick)
        };
        if prev != NIL {
            self.nodes[prev as usize].next = next;
        } else {
            let slot = (dt as usize) & self.mask;
            self.slots[slot] = next;
        }
        if next != NIL {
            self.nodes[next as usize].prev = prev;
        }
    }

    /// Reserve a slab node, reusing a freed one if any (no allocation on the reuse path).
    fn alloc_node(&mut self) -> u32 {
        if self.free_head != NIL {
            let idx = self.free_head;
            self.free_head = self.nodes[idx as usize].next;
            idx
        } else {
            let idx = self.nodes.len() as u32;
            assert!(idx != NIL, "timer wheel slab exhausted (u32::MAX nodes)");
            self.nodes.push(Node::default());
            idx
        }
    }

    /// Return a node to the free list, bumping its generation so any lingering [`TimerId`] for it
    /// no longer matches. The caller must have already unlinked it and taken its item.
    fn free_node(&mut self, idx: u32) {
        let node = &mut self.nodes[idx as usize];
        node.in_use = false;
        node.generation = node.generation.wrapping_add(1);
        node.deadline_tick = 0;
        node.prev = NIL;
        node.next = self.free_head;
        self.free_head = idx;
        self.len -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A wheel with millisecond resolution (tick_ms = 1) so deadlines map 1:1 to ms — the clearest
    /// setting for the ordering/cancellation semantics.
    fn wheel() -> DeadlineTimerWheel<u32> {
        DeadlineTimerWheel::new(1, 64)
    }

    #[test]
    fn empty_advance_is_noop() {
        let mut w = wheel();
        let mut out = Vec::new();
        w.advance(1_000, &mut out);
        assert!(out.is_empty());
        assert!(w.is_empty());
    }

    #[test]
    fn fires_at_or_after_deadline_not_before() {
        let mut w = wheel();
        w.advance(100, &mut Vec::new()); // pin cursor at 100
        w.insert(150, 7);
        let mut out = Vec::new();
        w.advance(149, &mut out);
        assert!(out.is_empty(), "must not fire before the deadline");
        w.advance(150, &mut out);
        assert_eq!(out, vec![7], "fires exactly at the deadline");
        assert!(w.is_empty());
    }

    #[test]
    fn expires_in_nondecreasing_deadline_order() {
        let mut w = wheel();
        w.advance(0, &mut Vec::new());
        // insert out of deadline order
        w.insert(300, 3);
        w.insert(100, 1);
        w.insert(200, 2);
        w.insert(250, 25);
        let mut out = Vec::new();
        w.advance(1_000, &mut out);
        assert_eq!(out, vec![1, 2, 25, 3], "emitted in ascending deadline order");
    }

    #[test]
    fn partial_advance_only_expires_due_timers() {
        let mut w = wheel();
        w.advance(0, &mut Vec::new());
        w.insert(100, 1);
        w.insert(200, 2);
        w.insert(300, 3);
        let mut out = Vec::new();
        w.advance(200, &mut out);
        assert_eq!(out, vec![1, 2]);
        assert_eq!(w.len(), 1, "the 300 timer is still armed");
        out.clear();
        w.advance(300, &mut out);
        assert_eq!(out, vec![3]);
        assert!(w.is_empty());
    }

    #[test]
    fn cancel_removes_before_firing_and_returns_payload() {
        let mut w = wheel();
        w.advance(0, &mut Vec::new());
        let id = w.insert(500, 42);
        w.insert(600, 43);
        assert_eq!(w.cancel(id), Some(42));
        assert_eq!(w.len(), 1);
        let mut out = Vec::new();
        w.advance(1_000, &mut out);
        assert_eq!(out, vec![43], "the cancelled timer never fires");
    }

    #[test]
    fn cancel_is_idempotent_and_generation_safe() {
        let mut w = wheel();
        w.advance(0, &mut Vec::new());
        let id = w.insert(500, 1);
        assert_eq!(w.cancel(id), Some(1));
        assert_eq!(w.cancel(id), None, "double-cancel is a no-op");
        // reuse the freed slab slot; the stale id must not match the new timer.
        let id2 = w.insert(700, 2);
        assert_eq!(id2.idx, id.idx, "slab slot reused");
        assert_ne!(id2, id, "generation differs");
        assert_eq!(w.cancel(id), None, "the stale id cannot cancel the reused slot");
        assert_eq!(w.len(), 1);
        assert_eq!(w.cancel(id2), Some(2));
    }

    #[test]
    fn cancelling_the_minimum_still_fires_the_rest() {
        // exercises the "cancel leaves min_tick a loose lower bound" self-healing path.
        let mut w = wheel();
        w.advance(0, &mut Vec::new());
        let early = w.insert(100, 1);
        w.insert(200, 2);
        assert_eq!(w.cancel(early), Some(1));
        let mut out = Vec::new();
        w.advance(150, &mut out);
        assert!(out.is_empty(), "the only <=150 timer was cancelled");
        w.advance(250, &mut out);
        assert_eq!(out, vec![2]);
    }

    #[test]
    fn past_deadline_fires_on_next_advance() {
        let mut w = wheel();
        w.advance(1_000, &mut Vec::new()); // cursor at 1000
        w.insert(500, 9); // already in the past
        let mut out = Vec::new();
        w.advance(1_001, &mut out);
        assert_eq!(out, vec![9], "a past deadline fires promptly, not a rotation later");
    }

    #[test]
    fn deadlines_beyond_one_rotation_still_fire() {
        // num_slots = 8 ⇒ rotation = 8 ticks; schedule well beyond it and jump across in one advance
        // (the full-rotation expiry path).
        let mut w: DeadlineTimerWheel<u32> = DeadlineTimerWheel::new(1, 8);
        w.advance(0, &mut Vec::new());
        w.insert(5, 5);
        w.insert(37, 37); // > 4 rotations away, same slot as tick 5 (5 % 8 == 37 % 8)
        w.insert(64, 64);
        let mut out = Vec::new();
        w.advance(100, &mut out);
        assert_eq!(out, vec![5, 37, 64], "all fire, in order, across many rotations");
        assert!(w.is_empty());
    }

    #[test]
    fn stepping_and_full_pass_agree_across_a_rotation_boundary() {
        // Same schedule advanced two ways: one tick at a time (stepping) vs one big jump
        // (full-rotation pass). Both must emit the identical ordered batch.
        let schedule = [(3u32, 3i64), (9, 9), (12, 12), (20, 20), (5, 5)];
        let mut stepped = Vec::new();
        {
            let mut w: DeadlineTimerWheel<u32> = DeadlineTimerWheel::new(1, 8);
            w.advance(0, &mut Vec::new());
            for (item, dl) in schedule {
                w.insert(dl, item);
            }
            for now in 1..=25 {
                w.advance(now, &mut stepped);
            }
        }
        let mut jumped = Vec::new();
        {
            let mut w: DeadlineTimerWheel<u32> = DeadlineTimerWheel::new(1, 8);
            w.advance(0, &mut Vec::new());
            for (item, dl) in schedule {
                w.insert(dl, item);
            }
            w.advance(25, &mut jumped);
        }
        assert_eq!(stepped, vec![3, 5, 9, 12, 20]);
        assert_eq!(jumped, stepped, "stepping and one-shot advance agree");
    }

    #[test]
    fn advance_across_many_future_deadlines_is_selective() {
        // "O(1) advance across many deadlines": with 10k timers far in the future plus one near,
        // advancing just past the near one expires EXACTLY that one — the 10k others are neither
        // scanned into `out` nor removed. The pre-`min_tick` fast path also skips cheaply.
        let mut w = DeadlineTimerWheel::new(1, 1024);
        w.advance(0, &mut Vec::new());
        for i in 0..10_000u32 {
            w.insert(1_000_000 + i as i64, i);
        }
        w.insert(500, 7);
        let mut out = Vec::new();
        // many advances that cross NO deadline: fast-skip, nothing expires.
        for now in [10, 50, 100, 400] {
            w.advance(now, &mut out);
            assert!(out.is_empty());
        }
        w.advance(500, &mut out);
        assert_eq!(out, vec![7], "only the due timer expires");
        assert_eq!(w.len(), 10_000, "the far-future timers are untouched");
    }

    #[test]
    fn no_allocation_on_the_steady_path() {
        // Warm the slab to a steady concurrent peak, then run many insert→expire cycles and assert
        // the slab never grows again (nodes are reused via the free list) and the reused output
        // buffer never regrows.
        let mut w = DeadlineTimerWheel::new(1, 256);
        w.advance(0, &mut Vec::new());
        let mut out = Vec::new();
        let mut now = 0i64;
        // warm up: keep ~64 timers concurrently in flight for a few rounds.
        for round in 0..8 {
            for k in 0..64 {
                w.insert(now + 10 + k, round * 64 + k as u32);
            }
            now += 100;
            w.advance(now, &mut out);
            out.clear();
        }
        let cap_after_warmup = w.node_capacity();
        let out_cap_after_warmup = out.capacity();
        assert!(cap_after_warmup >= 64);
        for round in 8..2_000 {
            for k in 0..64 {
                w.insert(now + 10 + k, round * 64 + k as u32);
            }
            now += 100;
            w.advance(now, &mut out);
            out.clear();
        }
        assert_eq!(
            w.node_capacity(),
            cap_after_warmup,
            "slab did not reallocate on the steady path"
        );
        assert_eq!(out.capacity(), out_cap_after_warmup, "reused output buffer did not regrow");
        assert!(w.is_empty());
    }

    #[test]
    fn deterministic_under_injected_now() {
        // The wheel reads no clock: the same inserts + the same now_ms sequence give byte-identical
        // expiries every run.
        let run = || {
            let mut w = DeadlineTimerWheel::new(5, 128); // coarse resolution to exercise quantization
            w.advance(1_000, &mut Vec::new());
            w.insert(1_007, 1);
            w.insert(1_050, 2);
            w.insert(1_023, 3);
            let mut trace = Vec::new();
            for now in (1_000..=1_100).step_by(5) {
                let mut out = Vec::new();
                w.advance(now, &mut out);
                for item in out {
                    trace.push((now, item));
                }
            }
            trace
        };
        assert_eq!(run(), run(), "identical now_ms sequence ⇒ identical expiries");
        // and the coarse-resolution timers never fire before their real deadline
        let trace = run();
        for (fired_at, item) in trace {
            let deadline = match item {
                1 => 1_007,
                2 => 1_050,
                3 => 1_023,
                _ => unreachable!(),
            };
            assert!(fired_at >= deadline, "item {item} fired at {fired_at} before {deadline}");
        }
    }

    #[test]
    fn monotonic_advance_ignores_backward_now() {
        let mut w = wheel();
        w.advance(1_000, &mut Vec::new());
        w.insert(1_100, 1);
        let mut out = Vec::new();
        w.advance(900, &mut out); // backward — ignored
        assert!(out.is_empty());
        w.advance(1_100, &mut out);
        assert_eq!(out, vec![1]);
    }
}
