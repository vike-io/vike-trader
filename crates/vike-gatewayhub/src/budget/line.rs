//! `Line` — ONE counter: a [`Ceiling`], its [`Split`], the two sliding windows that count against
//! it, and the would-throttle detector's warn-once state. A pure state machine: every method takes
//! `now`, none reads a clock, none logs (the caller logs when [`Observation::announce`] says to).

use std::time::Instant;

use serde::Serialize;

use super::ceiling::{Axis, Ceiling};
use super::class::Class;
use super::key::BudgetKey;
use super::mode::{Admission, Mode};
use super::reserve::{self, Split};
use super::window::SlidingWindow;

/// What [`Line::spend`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Observation {
    /// The request, judged against the split, exceeds what its class may draw: under
    /// [`Mode::Enforce`] it would be refused, and in [`Mode::Shadow`] it WOULD have been.
    pub over: bool,
    /// The caller should WARN now: the request is over and no warning has been announced for this
    /// line in the last window. ONE warning per line per window, whatever the request rate.
    pub announce: bool,
    /// What the mode decided: [`Admission::Admit`] always in shadow mode.
    pub admission: Admission,
}

/// One line's counters as a value a status verb can serialise. Taken at an instant: the `*_used`
/// figures are the window's sum as of the snapshot's `now`, so an idle line reads zero.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LineSnapshot {
    /// Whose budget this is.
    pub key: BudgetKey,
    /// Which meter of the venue's row.
    pub axis: Axis,
    /// The window length, in milliseconds.
    pub window_ms: u64,
    /// The venue's published cap per window, or `None` when the venue publishes none.
    pub published: Option<u64>,
    /// The cap this line judges against (a margin under `published` where there is one).
    pub admitted: u64,
    /// The part of `admitted` only orders draw first.
    pub order_reserve: u64,
    /// The part every class draws.
    pub shared_pool: u64,
    /// Units spent from the order reserve inside the window.
    pub order_used: u64,
    /// Units spent from the shared pool inside the window (an order's overflow included).
    pub shared_used: u64,
    /// Units counted since the daemon started (a refused request is not counted).
    pub counted: u64,
    /// Requests since the daemon started that were judged over their split.
    pub would_throttle: u64,
}

/// See the module doc.
#[derive(Debug, Clone)]
pub struct Line {
    ceiling: Ceiling,
    split: Split,
    order: SlidingWindow,
    shared: SlidingWindow,
    counted: u64,
    would_throttle: u64,
    warned_at: Option<Instant>,
}

impl Line {
    /// A line over `ceiling`, split by the axis's order share ([`Axis::order_share_percent`]) of the
    /// cap the line judges against (`ceiling.admitted`).
    #[must_use]
    pub fn new(ceiling: Ceiling) -> Self {
        let split = reserve::split(ceiling.admitted, ceiling.axis.order_share_percent());
        Self {
            ceiling,
            split,
            order: SlidingWindow::new(ceiling.window),
            shared: SlidingWindow::new(ceiling.window),
            counted: 0,
            would_throttle: 0,
            warned_at: None,
        }
    }

    /// The ceiling this line follows.
    #[must_use]
    pub fn ceiling(&self) -> &Ceiling {
        &self.ceiling
    }

    /// Spend `cost` units of `class` at `now`.
    ///
    /// An [`Class::Order`] request draws the order reserve first and the shared pool only for the
    /// rest; every other class draws the pool alone. The request is OVER when the pool part would
    /// push the pool past its size — an order that fits the reserve is never over, however busy the
    /// pool is. Under [`Mode::Shadow`] the request is counted whatever it is judged; under
    /// [`Mode::Enforce`] an over request is refused and NOT counted.
    pub fn spend(&mut self, class: Class, cost: u64, now: Instant, mode: Mode) -> Observation {
        let order_used = self.order.used(now);
        let shared_used = self.shared.used(now);
        let (from_reserve, from_pool) = if class.draws_order_reserve() {
            let from_reserve = cost.min(self.split.order_reserve.saturating_sub(order_used));
            (from_reserve, cost - from_reserve)
        } else {
            (0, cost)
        };
        let over = from_pool > 0 && shared_used.saturating_add(from_pool) > self.split.shared_pool;
        let admission =
            if over && mode == Mode::Enforce { Admission::Refuse } else { Admission::Admit };
        if admission == Admission::Admit {
            self.order.add(now, from_reserve);
            self.shared.add(now, from_pool);
            self.counted = self.counted.saturating_add(cost);
        }
        if over {
            self.would_throttle = self.would_throttle.saturating_add(1);
        }
        Observation { over, announce: over && self.warn_due(now), admission }
    }

    /// Is a warning due at `now` — none announced yet, or the last one a whole window ago? Marks
    /// `now` as announced when it is.
    fn warn_due(&mut self, now: Instant) -> bool {
        let due = match self.warned_at {
            None => true,
            Some(at) => now.saturating_duration_since(at) >= self.ceiling.window,
        };
        if due {
            self.warned_at = Some(now);
        }
        due
    }

    /// This line's counters as of `now`, labelled with its `key`.
    pub fn snapshot(&mut self, key: &BudgetKey, now: Instant) -> LineSnapshot {
        LineSnapshot {
            key: key.clone(),
            axis: self.ceiling.axis,
            window_ms: u64::try_from(self.ceiling.window.as_millis()).unwrap_or(u64::MAX),
            published: self.ceiling.published,
            admitted: self.ceiling.admitted,
            order_reserve: self.split.order_reserve,
            shared_pool: self.split.shared_pool,
            order_used: self.order.used(now),
            shared_used: self.shared.used(now),
            counted: self.counted,
            would_throttle: self.would_throttle,
        }
    }
}
