//! `RateLimits` — the STATIC per-venue, per-market RATE-LIMIT capability table (step-1 declaration).
//!
//! A venue's rate limit is a **fact about the venue**, true for everyone and everywhere: Binance's
//! spot host permits 6000 weight units per minute whoever is asking, and exceeding it earns an HTTP
//! 429 that escalates to a 418 IP BAN. It is therefore emphatically **not a setting**. An operator
//! does not choose it, cannot negotiate it, and must never be able to raise it — a slider that goes
//! "faster" here is a slider that goes "banned". So these numbers live beside the workspace's other
//! per-venue fact tables ([`crate::venue_caps::caps_for`], [`crate::venue_margin_support::venue_margin_support`],
//! [`crate::fees::fee_schedule_for`], `vike_bridge_core::tif::venue_tif`) and follow the same
//! playbook: one row per venue citing the adapter code it was read from, a verbatim matrix pin test,
//! and a completeness test iterating [`crate::venues::VENUES`] so a new bridge crate cannot ship
//! without declaring what its venue permits.
//!
//! ## What this is NOT
//! Three neighbours are deliberately separate, and confusing any of them with this table is how a
//! venue fact turns into a tuning knob:
//! - [`crate::rate_limits::RateLimitConfig`] — the OPERATOR knob: what FRACTION of the budget below
//!   vike is allowed to spend. Bounded to `[0.05, 0.95]` precisely so it can never express a ban.
//!   That module owns the fraction; **this one owns the budget the fraction is a fraction OF.**
//! - [`crate::rate_limits::PaceBook`] — what a run MEASURED (how long a request took, what one
//!   cost). Evidence, never permission.
//! - `vike_bridge_core::rate_discovery` — what the venue publishes AT RUNTIME, read out of an
//!   `exchangeInfo` body the backfill already downloads. **Discovery still wins where a venue
//!   answers**; this table is the FALLBACK, and the ceiling of record for everything discovery
//!   cannot see (see [`History`]).
//!
//! ## Independent meters, never one number
//! Every venue here runs at least two INDEPENDENT counters, and one says nothing about the other:
//! - [`RateLimits::orders`] — the ORDER meter, bound to order submit/cancel/amend. Binance's spot
//!   `ORDERS` budget is 100 per 10 s; its `REQUEST_WEIGHT` pool is a different counter entirely.
//! - [`RateLimits::history`] — the paged-HISTORY budget the kline/candle backfill draws on
//!   (`REQUEST_WEIGHT` on the Binance-grammar venues; nothing at all on the others).
//! - [`RateLimits::ws_sends`] — the client→server WEBSOCKET send meter, a third counter again.
//! - [`RateLimits::rest_ip_weight`] — the shared per-IP REST WEIGHT pool that ALL of a venue's REST
//!   traffic draws from at once, reads and order actions alike. A fourth axis, and the one that is
//!   not a counter at all: its unit is the WEIGHT a request costs, not the request. Only
//!   [`HYPERLIQUID`] gates one today (1200 weight/min), and it is the venue's ONLY budget — a
//!   Hyperliquid order submit competes with a `/info` read in the same window, which is precisely
//!   what neither [`RateLimits::orders`] nor [`RateLimits::history`] could say.
//!
//! Folding them into one row-level scalar would understate whichever is not the binding constraint,
//! which is exactly why `vike_bridge_core::rate_discovery` reads only `REQUEST_WEIGHT` and
//! deliberately ignores the `ORDERS` row sitting beside it in the same array.
//!
//! ## The published cap and the admitted rate are DIFFERENT numbers
//! Before this table, every gate baked two things into one integer: the venue's published cap and an
//! ad-hoc safety margin someone divided out once (`const SPOT_ORDERS: usize = 90; // 100/10s, gate
//! ~10% under`). The cap was then unreadable — nothing in the build knew what 90 was 10 % of, so
//! nothing could check that the margin still existed. [`Meter::Published`] carries BOTH: `published`
//! is the venue's number (never ours to raise) and `admitted` is what vike's gate lets through, with
//! the difference being the margin. A `const _: () = assert!(…)` per row then enforces
//! `admitted <= published` at COMPILE time — the same discipline the two `weight_soft_limit`
//! invariants had in `vike_binance::data`/`vike_aster::data`, generalised to every meter and moved
//! here beside the numbers they constrain.
//!
//! ## Absence is modelled, never fabricated
//! Most of the roster publishes nothing this table could carry, and the honest encoding of "we do
//! not know" is a VARIANT, not a plausible-looking integer:
//! - [`Meter::Unpublished`] — the venue documents no cap for this meter, yet vike still runs a
//!   runaway brake (bybit's and polymarket's WS gates). The number is OURS. It must never be read
//!   back as a venue fact, so [`Meter::published`] REFUSES to answer for it.
//! - [`History::Unweighted`] — bybit, okx and deribit run no request-weight system at all and
//!   publish no machine-readable rate-limit metadata, so there is no ceiling to stay under, no
//!   `X-MBX-USED-WEIGHT-1M` header to cool down on, and nothing
//!   `vike_bridge_core::rate_discovery` can ever answer for them. Their whole pacing story is a
//!   fixed page delay. (This is NOT "these venues have no limits" — each documents a request-COUNT
//!   limit in prose, cited in its row below. It is that none of them is a WEIGHT budget, and none is
//!   machine-readable, so neither the table nor discovery can carry one.)
//! - [`History::NotPaged`] / [`Meter::Ungated`] / [`Provenance::NotDeclared`] — vike runs no such
//!   gate at this venue. Reading a rate out of any of them is a compile error in a const context
//!   (they `panic!`), which is what makes the fallback fail-CLOSED: a row must be DECLARED before a
//!   gate can be built from it, and an undeclared venue cannot silently produce a permissive one.
//!
//! ## What was measured, and when (2026-08-04, from the CI box)
//! The request-weight facts were read off the venues' own live `exchangeInfo`, not assumed:
//! - **binance spot** (`api.binance.com`): `REQUEST_WEIGHT` `MINUTE` = **6000**, and
//!   `/api/v3/klines?limit=1000` costs weight **2** (`x-mbx-used-weight`), paging **1000** rows.
//! - **binance fapi** (`fapi.binance.com`): `REQUEST_WEIGHT` `MINUTE` = **2400** — NOT spot's 6000
//!   — and `/fapi/v1/klines?limit=1000` costs weight **5**, paging **1000** rows.
//! - **aster** (`fapi.asterdex.com`): a byte-identical Binance fork — **2400**/min at weight **5**,
//!   1000 rows. Its 2400 was inherited as 6000 from Binance's SPOT template for its entire life
//!   before this probe; the old `weight_soft_limit: 5000` therefore sat ABOVE the real ceiling and
//!   could never fire.
//! - **okx** pages **100** rows where every other venue here pages 1000, so the same 200 ms page
//!   delay buys a tenth of the history per unit of budget. That page size is the endpoint's own
//!   (`MAX_LIMIT` in each venue's `data.rs`), not a rate knob, so it is recorded here as provenance
//!   rather than as a field.
//!
//! The ORDER and WS caps predate that probe and come from the 2026-07 net-hardening pass (spec §A) —
//! binance's from live `exchangeInfo`, the rest transcribed from each venue's published docs.
//! Hyperliquid's per-IP weight pool predates it too: transcribed from HL's published docs in the
//! 2026-07-16 adapter research pass, and carried inside `vike_hyperliquid::transport` until this
//! table grew the axis to hold it. Each row's [`Provenance`] records the OLDEST of its checks,
//! because a row is only as fresh as its stalest cap; the per-number evidence is in the row's own
//! doc comment.
//!
//! ## Honesty rule (mirrors `venue_caps`/`venue_margin_support`)
//! Every field records TODAY'S REALITY — what the adapter actually does on the wire right now, not
//! what it should do. Where a value is a known-imperfect carry-over it is declared as such with a ⚠
//! in the row (see [`ASTER_SPOT`]), never quietly corrected: changing a number is a retune, and a
//! retune is a separate change from moving where the number lives.

use std::time::Duration;

/// Which of a venue's METERS a row describes — venues whose spot and perp sides are metered
/// separately need two rows.
///
/// Only **binance** and **aster** are genuinely market-split today (binance's two hosts publish
/// different weight ceilings AND different `ORDERS` budgets; aster's spot and perp `ORDERS` budgets
/// differ by 12x). Every other venue meters one pool, so [`rate_limits_for`] serves it the SAME row
/// for both markets — asserted by `market_split_venues_are_exactly_binance_and_aster` rather than
/// left implicit.
///
/// The two spellings match the `"spot"` / `"perp"` strings already used as the second half of a
/// [`crate::rate_limits::PaceBook`] key and returned by `vike_binance::data::kline_market`, so a
/// persisted pace record and a table lookup name the same thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Market {
    /// The venue's spot/cash book.
    Spot,
    /// The venue's perpetual-futures book (binance/aster USDⓈ-M, okx SWAP, bybit linear, …).
    Perp,
}

impl Market {
    /// The canonical lowercase string — the same token a [`crate::rate_limits::PaceBook`] key and
    /// `vike_binance::data::kline_market` use.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Market::Spot => "spot",
            Market::Perp => "perp",
        }
    }

    /// The inverse of [`Self::as_str`], for a caller holding a market STRING (a `PaceBook` row, a
    /// `kline_market` return). `None` for anything else — an unrecognised market must not silently
    /// resolve to spot, because spot and perp are exactly the pair whose budgets differ.
    #[must_use]
    pub fn from_market_str(s: &str) -> Option<Market> {
        match s {
            "spot" => Some(Market::Spot),
            "perp" => Some(Market::Perp),
            _ => None,
        }
    }
}

/// One metered budget: what the VENUE permits per window, and what vike's gate actually admits.
///
/// The split is the point. A single integer (`90`) cannot be checked against anything; the pair
/// (`published: 100`, `admitted: 90`) can, and [`Self::stays_under_published_cap`] does — at compile
/// time, on every row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Meter {
    /// The venue publishes a cap for this meter and vike gates UNDER it.
    Published {
        /// The venue's OWN cap per [`window`](Self::window) — a FACT, never ours to raise.
        published: usize,
        /// What vike's `vike_bridge_core::ratelimit::RateGate` admits per window today. The
        /// difference from `published` IS the safety margin; it is deliberately stored rather than
        /// derived from a fraction, because the historical margins are not one fraction (binance
        /// ~10 %, okx ~17 %, deribit 0 %) and re-deriving them would change values.
        admitted: usize,
        /// The window both numbers are metered over. Part of the FACT: binance meters `ORDERS` per
        /// 10 s where aster meters the same counter per 60 s, so a bare "90" means nothing without
        /// it.
        window: Duration,
    },
    /// The venue publishes NO cap for this meter, but vike runs a runaway brake anyway — sized
    /// generously so it never bites legitimate traffic and only clamps a pathological
    /// subscribe/reconnect loop.
    ///
    /// `admitted` is **vike's own number**, chosen by us, and there is nothing to check it against.
    /// [`Self::published`] therefore refuses to answer for this variant rather than handing back a
    /// value a caller could mistake for a venue cap.
    Unpublished {
        /// What vike's gate admits per window. Ours, not the venue's.
        admitted: usize,
        /// The window that brake is metered over.
        window: Duration,
    },
    /// vike runs no gate on this meter at this venue — nothing metered is sent on this axis, or the
    /// venue meters it but no `RateGate` sits on it.
    ///
    /// ⚠ It is a statement about VIKE'S GATES, never about the venue. The documented case is
    /// [`BINANCE_SPOT`]'s [`RateLimits::rest_ip_weight`]: binance's `REQUEST_WEIGHT` pool is a real
    /// per-IP weight budget of exactly the class that axis describes, but nothing gates it — the
    /// kline pager self-throttles off the live `X-MBX-USED-WEIGHT-1M` header instead, and the
    /// ceiling is declared once, in [`RateLimits::history`]. Each row's doc says which it is.
    Ungated,
}

impl Meter {
    /// What vike's gate admits per [`Self::window`] — the first argument of
    /// `vike_bridge_core::ratelimit::RateGate::new`.
    ///
    /// **Panics** on [`Meter::Ungated`]: there is no rate to admit, and inventing one (0? usize::MAX?)
    /// would be either a hang or a hammer. In a `const` initializer — which is how every bridge
    /// consumes this — that panic is a COMPILE error, so an undeclared meter cannot be turned into a
    /// gate by accident.
    #[must_use]
    pub const fn admitted(&self) -> usize {
        match self {
            Meter::Published { admitted, .. } | Meter::Unpublished { admitted, .. } => *admitted,
            Meter::Ungated => {
                panic!("no gate is declared for this meter: there is no admitted rate")
            }
        }
    }

    /// The window [`Self::admitted`] (and, where it exists, [`Self::published`]) is metered over —
    /// the second argument of `RateGate::new`. **Panics** on [`Meter::Ungated`], for the reason
    /// given there.
    #[must_use]
    pub const fn window(&self) -> Duration {
        match self {
            Meter::Published { window, .. } | Meter::Unpublished { window, .. } => *window,
            Meter::Ungated => panic!("no gate is declared for this meter: there is no window"),
        }
    }

    /// The VENUE's own cap per window.
    ///
    /// **Panics** for [`Meter::Unpublished`] and [`Meter::Ungated`] — deliberately, and this is the
    /// variant's whole reason for existing. A venue that documents no cap has none to report, and a
    /// caller that gets a number back here would treat vike's runaway brake as the venue's limit.
    /// Ask [`Self::published_cap`] instead when the answer may legitimately be "there is none".
    #[must_use]
    pub const fn published(&self) -> usize {
        match self {
            Meter::Published { published, .. } => *published,
            Meter::Unpublished { .. } => {
                panic!("the venue publishes no cap for this meter: that rate is vike's own brake")
            }
            Meter::Ungated => {
                panic!("no gate is declared for this meter: there is no published cap")
            }
        }
    }

    /// The venue's own cap per window, or `None` when it publishes none — the total form of
    /// [`Self::published`], for a caller that wants to branch instead of assert.
    #[must_use]
    pub const fn published_cap(&self) -> Option<usize> {
        match self {
            Meter::Published { published, .. } => Some(*published),
            Meter::Unpublished { .. } | Meter::Ungated => None,
        }
    }

    /// Does this meter's admitted rate stay inside the venue's published cap, and are both numbers
    /// usable at all?
    ///
    /// A `const fn` because it backs the `const _: () = assert!(…)` invariants below: a row whose
    /// gate would out-spend the venue fails the BUILD, not a test run. `admitted == published` is
    /// legal (deribit's matching-engine gate sits exactly at the published 5/s), a zero-occurrence
    /// gate is not, and neither is a zero window.
    ///
    /// ⚠ A zero-occurrence gate fails **OPEN**, not closed — it admits EVERYTHING, instantly and
    /// forever, while still looking like a limiter. (This sentence used to say it "admits nothing and
    /// hangs the sender", which is the reassuring direction and the wrong one;
    /// `vike_bridge_core::ratelimit::RateGate::new` carries the mechanism.) That is what makes this
    /// `const` rejection load-bearing rather than tidy: the failure it prevents is silent
    /// over-sending to a venue, not a stall somebody would notice.
    #[must_use]
    pub const fn stays_under_published_cap(&self) -> bool {
        match self {
            Meter::Published { published, admitted, window } => {
                *published > 0 && *admitted > 0 && *admitted <= *published && !window.is_zero()
            }
            // Nothing published ⇒ nothing to exceed; only the brake's own usability is checkable.
            Meter::Unpublished { admitted, window } => *admitted > 0 && !window.is_zero(),
            Meter::Ungated => true,
        }
    }
}

/// The paged-HISTORY budget a kline/candle backfill draws on — a different counter from
/// [`RateLimits::orders`] on every venue that has both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum History {
    /// The venue meters request WEIGHT and publishes a ceiling (the Binance grammar: binance and
    /// its aster fork).
    Weighted {
        /// The venue's OWN published `REQUEST_WEIGHT` ceiling, in weight units per MINUTE. This is
        /// the number a 429 → 418 IP ban is measured against and it is **never ours to raise**.
        ceiling_per_min: u64,
        /// The `X-MBX-USED-WEIGHT-1M` value at/above which the pager proactively cools down.
        ///
        /// STRICTLY below `ceiling_per_min`, enforced at compile time: a soft limit at or above the
        /// ceiling can never fire, so the guard silently becomes dead code and the venue's IP ban is
        /// what stops you instead. Both binance hosts shared ONE spec at 5000 — the SPOT budget —
        /// until 2026-08-04, and nothing caught it because nothing read the number.
        soft_limit: u64,
        /// The FALLBACK inter-page sleep: what the pager uses when runtime discovery does not
        /// answer (no `exchange_info_url`, a network error, an unparseable body). With discovery
        /// live this is the slower path by design — see `vike_binance::family::klines`.
        page_delay: Duration,
    },
    /// The venue runs **no request-weight meter and publishes no weight ceiling**, so there is no
    /// number to stay under, no used-weight header to cool down on, and nothing
    /// `vike_bridge_core::rate_discovery` can ever answer for it. A fixed `page_delay` is the whole
    /// pacing story.
    ///
    /// ⚠ This is NOT "the venue has no limits". bybit, okx and deribit each document a request-COUNT
    /// limit in prose (cited in their rows below), and the 200 ms delay was chosen to sit an order
    /// of magnitude under it. It is that none of those is a WEIGHT budget and none is
    /// machine-readable, so neither this table nor discovery can carry one — and a fabricated
    /// ceiling would be worse than an absent one.
    Unweighted {
        /// The fixed inter-page sleep — here, the ONLY pacing there is.
        page_delay: Duration,
    },
    /// vike pages no bar history from this venue at all.
    NotPaged,
}

impl History {
    /// The fixed inter-page sleep. **Panics** on [`History::NotPaged`] — a venue vike does not page
    /// has no page delay, and in a `const` initializer that panic is a compile error.
    #[must_use]
    pub const fn page_delay(&self) -> Duration {
        match self {
            History::Weighted { page_delay, .. } | History::Unweighted { page_delay } => {
                *page_delay
            }
            History::NotPaged => {
                panic!("vike pages no history from this venue: there is no page delay")
            }
        }
    }

    /// The proactive-cooldown threshold. **Panics** unless this is [`History::Weighted`]: a venue
    /// with no weight meter has no used-weight counter to be soft about, and any number returned
    /// here would be invented.
    #[must_use]
    pub const fn soft_limit(&self) -> u64 {
        match self {
            History::Weighted { soft_limit, .. } => *soft_limit,
            History::Unweighted { .. } => {
                panic!("this venue runs no request-weight meter: there is no soft limit to read")
            }
            History::NotPaged => {
                panic!("vike pages no history from this venue: there is no soft limit")
            }
        }
    }

    /// The venue's published `REQUEST_WEIGHT` ceiling per minute. **Panics** unless this is
    /// [`History::Weighted`], for the same reason as [`Self::soft_limit`]. Ask
    /// [`Self::published_ceiling_per_min`] when "there is none" is a legitimate answer.
    #[must_use]
    pub const fn ceiling_per_min(&self) -> u64 {
        match self {
            History::Weighted { ceiling_per_min, .. } => *ceiling_per_min,
            History::Unweighted { .. } => {
                panic!("this venue publishes no request-weight ceiling: nothing to stay under")
            }
            History::NotPaged => {
                panic!("vike pages no history from this venue: there is no ceiling")
            }
        }
    }

    /// The venue's published ceiling, or `None` when it publishes none — the total form of
    /// [`Self::ceiling_per_min`].
    #[must_use]
    pub const fn published_ceiling_per_min(&self) -> Option<u64> {
        match self {
            History::Weighted { ceiling_per_min, .. } => Some(*ceiling_per_min),
            History::Unweighted { .. } | History::NotPaged => None,
        }
    }

    /// Is the soft limit strictly under the published ceiling (and is every number usable)? Backs
    /// the compile-time invariants below — the generalisation of the two `const _: ()` asserts that
    /// used to live beside `BINANCE_KLINE_SPOT`/`BINANCE_KLINE_PERP` and `ASTER_KLINE`.
    #[must_use]
    pub const fn stays_under_published_cap(&self) -> bool {
        match self {
            History::Weighted { ceiling_per_min, soft_limit, page_delay } => {
                *ceiling_per_min > 0
                    && *soft_limit > 0
                    && *soft_limit < *ceiling_per_min
                    && !page_delay.is_zero()
            }
            // A zero page delay is a no-throttle hammer, which is never what a fallback should be.
            History::Unweighted { page_delay } => !page_delay.is_zero(),
            History::NotPaged => true,
        }
    }
}

/// Where a row's numbers came from — the field that makes a stale table visible instead of
/// invisible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// Every VENUE-PUBLISHED cap in this row was checked against the venue on this date (ISO
    /// `YYYY-MM-DD`, or `YYYY-MM` where only the month was recorded at the time).
    ///
    /// Where a row's numbers were checked on DIFFERENT dates the **oldest** is recorded — a row is
    /// only as fresh as its stalest cap, and the useful reading of this field is "when is a
    /// re-probe due". The per-number evidence, including the fresher checks, is in the row's own doc
    /// comment.
    Measured {
        /// The date of the oldest check backing this row.
        on: &'static str,
    },
    /// The venue publishes NO rate-limit numbers for the meters this row declares. Every rate in it
    /// is vike's OWN runaway brake, and none of it may be read back as a venue cap — which is why
    /// [`Meter::published`] panics rather than answering.
    Unpublished,
    /// This table declares NO limits for this venue: vike runs no rate gate here and pages no
    /// history from it. **Not** a claim that the venue is unlimited — see the row's doc for what
    /// exists elsewhere and why it was not moved here.
    NotDeclared,
}

/// The declared static rate-limit row for ONE (venue, market). `Copy` — three small enums — so a
/// bridge holds it by value in a `const`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimits {
    /// The ORDER meter: order submit / cancel / amend. A **different counter** from
    /// [`Self::history`] on every venue that has both, and one says nothing about the other.
    pub orders: Meter,
    /// The client→server WEBSOCKET send meter (subscribe / login / keepalive frames). A third
    /// counter again, per-CONNECTION on most venues.
    pub ws_sends: Meter,
    /// The shared per-IP REST **weight** pool: one budget that ALL of vike's REST traffic at this
    /// venue draws from — reads and order actions alike — through ONE
    /// `vike_bridge_core::ratelimit::RateGate` charged per request by a venue-specific weight
    /// schedule.
    ///
    /// ⚠ **The unit is WEIGHT, not requests.** [`Self::orders`] and [`Self::ws_sends`] count
    /// occurrences; this one counts what those occurrences COST, so a `published` of 1200 is 1200
    /// weight units per window — anywhere from 20 to 1200 actual requests depending on which ones.
    /// The same `RateGate` type serves both because its slots are unitless: a weight pool charges
    /// `proceed_cost(weight)` where a counter charges `proceed()`.
    ///
    /// Why it is a SEPARATE axis rather than a value under [`Self::orders`]: a venue that meters
    /// this way has no order-specific counter to declare. On [`HYPERLIQUID`] — the only row that
    /// gates one today — an order submit and an `/info` read spend the same 1200-weight minute, so
    /// putting 1200 under `orders` would claim the venue permits 1200 orders per minute, which is
    /// neither what it meters nor what the gate does.
    ///
    /// [`Meter::Ungated`] on every other row. That is about vike's gates, NOT about the venues:
    /// binance and aster meter a `REQUEST_WEIGHT` pool of exactly this class, but no `RateGate` sits
    /// on it and its ceiling is already declared in [`Self::history`] — re-declaring it here would
    /// give one fact two homes and two chances to drift.
    pub rest_ip_weight: Meter,
    /// The paged-HISTORY budget the kline/candle backfill draws on.
    pub history: History,
    /// Where this row's numbers came from, and when.
    pub provenance: Provenance,
}

impl RateLimits {
    /// The fail-CLOSED row for a venue with nothing declared — every meter [`Meter::Ungated`],
    /// history [`History::NotPaged`], provenance [`Provenance::NotDeclared`].
    ///
    /// Fail-closed is literal here, not aspirational: every accessor on an ungated meter PANICS, so
    /// this row cannot be turned into a permissive `RateGate` at all. A venue must be DECLARED
    /// before a gate can be built from it.
    pub const NOT_DECLARED: RateLimits = RateLimits {
        orders: Meter::Ungated,
        ws_sends: Meter::Ungated,
        rest_ip_weight: Meter::Ungated,
        history: History::NotPaged,
        provenance: Provenance::NotDeclared,
    };

    /// Does every meter in this row admit no more than the venue published? Backs the compile-time
    /// invariants below.
    #[must_use]
    pub const fn stays_under_published_caps(&self) -> bool {
        self.orders.stays_under_published_cap()
            && self.ws_sends.stays_under_published_cap()
            && self.rest_ip_weight.stays_under_published_cap()
            && self.history.stays_under_published_cap()
    }
}

impl Default for RateLimits {
    fn default() -> Self {
        RateLimits::NOT_DECLARED
    }
}

// ---------------------------------------------------------------------------------------------
// The per-venue rows. Each cites the adapter code its numbers were read from.
// ---------------------------------------------------------------------------------------------

/// Binance **spot** (`api.binance.com`).
///
/// - `orders` — `ORDERS` = 100 / 10 s, read from live `exchangeInfo` `rateLimits` in the 2026-07
///   net-hardening pass; `vike_binance::ratelimit` gates ~10 % under at 90.
/// - `ws_sends` — Binance documents 5 incoming (client→server) messages per second per connection
///   (WS-API `General Info`; PING/PONG count toward it); gated ~20 % under at 4/s as a runaway
///   floor. The spot user-data handshake sends exactly ONE combined subscribe+auth frame, so this
///   rarely bites.
/// - `history` — `REQUEST_WEIGHT` `MINUTE` = **6000**, MEASURED 2026-08-04 from live
///   `exchangeInfo`; `/api/v3/klines?limit=1000` costs weight **2**, 1000 rows per page. The 50 ms
///   fallback delay yields ~1200 req/min ⇒ ~2400 weight/min, ~40 % of budget. Spot DOES publish a
///   machine-readable budget, so `vike_binance::data`'s spec names its `exchangeInfo` URL and
///   runtime discovery normally supersedes this page delay.
/// - `rest_ip_weight` — [`Meter::Ungated`]. Binance's `REQUEST_WEIGHT` IS a per-IP weight pool of
///   exactly that axis's class, but no `RateGate` meters it: the pager cools down off the live
///   `X-MBX-USED-WEIGHT-1M` header instead, and the 6000 ceiling is declared under `history` above
///   — once, not twice.
pub const BINANCE_SPOT: RateLimits = RateLimits {
    orders: Meter::Published { published: 100, admitted: 90, window: Duration::from_secs(10) },
    ws_sends: Meter::Published { published: 5, admitted: 4, window: Duration::from_secs(1) },
    // The REQUEST_WEIGHT pool is real but ungated — it is `history`'s `ceiling_per_min`.
    rest_ip_weight: Meter::Ungated,
    history: History::Weighted {
        ceiling_per_min: 6000,
        soft_limit: 4800,
        page_delay: Duration::from_millis(50),
    },
    provenance: Provenance::Measured { on: "2026-07" },
};

/// Binance **USDⓈ-M futures** (`fapi.binance.com`) — a SEPARATE row because fapi's budgets are not
/// spot's, on BOTH meters.
///
/// - `orders` — `ORDERS` = 300 / 10 s (three times spot's), live `exchangeInfo`, 2026-07; gated
///   ~10 % under at 270.
/// - `ws_sends` — the same per-connection 5/s Binance documents for spot, same 4/s floor.
/// - `history` — `REQUEST_WEIGHT` `MINUTE` = **2400**, a QUARTER of spot's 6000, MEASURED
///   2026-08-04; `/fapi/v1/klines?limit=1000` costs weight **5** (confirmed on a clean probe of
///   aster's byte-identical fapi twin, whose counter carried no concurrent traffic), 1000 rows per
///   page ⇒ a 480 req/min ceiling.
/// - `rest_ip_weight` — [`Meter::Ungated`], for the same reason as [`BINANCE_SPOT`]: the fapi
///   `REQUEST_WEIGHT` pool is ungated and its 2400 ceiling is declared under `history`.
///
/// ⚠ Both hosts previously shared ONE spec with `weight_soft_limit: 5000` — the SPOT budget — which
/// on this path is unreachable dead code: the hard 2400 ceiling (429 → 418 IP ban) fires long before
/// a 5000 counter could. It was harmless only while a fixed 300 ms delay held usage near 600/min —
/// safety by accident, not by design. The `soft_limit < ceiling_per_min` invariant below is what now
/// makes that shape a build failure.
pub const BINANCE_PERP: RateLimits = RateLimits {
    orders: Meter::Published { published: 300, admitted: 270, window: Duration::from_secs(10) },
    ws_sends: Meter::Published { published: 5, admitted: 4, window: Duration::from_secs(1) },
    // The REQUEST_WEIGHT pool is real but ungated — it is `history`'s `ceiling_per_min`.
    rest_ip_weight: Meter::Ungated,
    history: History::Weighted {
        ceiling_per_min: 2400,
        soft_limit: 2000,
        page_delay: Duration::from_millis(150),
    },
    provenance: Provenance::Measured { on: "2026-07" },
};

/// Aster DEX **spot** (`sapi.asterdex.com`) — a Binance-fork API with its own budgets.
///
/// - `orders` — `ORDERS` = 100 per **MINUTE** (per the Aster API docs, transcribed when the bridge
///   landed 2026-07); `vike_aster::ratelimit` gates ~10 % under at 90. ⚠ The WINDOW is the
///   divergence that matters: aster meters `ORDERS` per 60 s where binance meters the same counter
///   per 10 s, so the identical `90` is a 6x tighter gate here.
/// - `ws_sends` — aster's futures WS documents 10 incoming messages per second per connection;
///   gated ~20 % under at 8/s.
/// - `history` — the fapi `REQUEST_WEIGHT` `MINUTE` = **2400** measured 2026-08-04 (see
///   [`ASTER_PERP`]).
/// - `rest_ip_weight` — [`Meter::Ungated`], as at [`BINANCE_SPOT`]: aster forks binance's
///   `REQUEST_WEIGHT` pool verbatim, nothing gates it, and its ceiling is declared under `history`.
///
/// ⚠ **The history row used to carry the FAPI budget.** `vike_aster::data::fetch_klines_range`
/// builds ONE `KlineRateLimit` and uses it whichever host `range_target` resolves; a bare
/// (non-`.P`) symbol resolves the sapi host, so this row was recording what that pager actually
/// fell back to rather than what sapi publishes. Both halves are now MEASURED against sapi itself:
///
/// - **ceiling `6000`** — `https://sapi.asterdex.com/api/v3/exchangeInfo` publishes
///   `REQUEST_WEIGHT`/`MINUTE` = 6000 (its testnet twin publishes 6000 too), probed 2026-08-05.
/// - **`/api/v3/klines?limit=1000` costs weight 5** — MEASURED 2026-08-05, three consecutive
///   keyless requests returning `x-mbx-used-weight-1m` of 5 → 10 → 15, a clean +5 per page.
///   ⚠ NOT binance's 2. The earlier note here predicted 2 "on binance's grammar"; that inference
///   was wrong, which is why the retune waited for a probe instead of an analogy.
///
/// ⚠ **`soft_limit`, not the ceiling, was the binding constraint.** `vike_aster::data` DISCOVERS
/// per host, so a real spot backfill already paced against the venue's own 6000 — but
/// `should_cool_down` fires on the hand-set `soft_limit`, which applies ON TOP of whatever is
/// discovered. At 2000 that braked a 6000-ceiling host at **33 %**, so raising the ceiling alone
/// would have changed nothing. It moves to **4800**, the same 80 % of ceiling
/// [`BINANCE_SPOT`] uses on the identical 6000 budget.
///
/// `page_delay` is the fallback for a run where discovery did not answer, and it is DERIVED, not
/// guessed: 6000 × [`crate::rate_limits::DEFAULT_UTILIZATION`] = 2400 weight/min ÷ weight-5 pages
/// = 480 pages/min ⇒ **125 ms**. (Binance spot's 50 ms is the same arithmetic over weight-2 pages;
/// copying it here would have paced 2.5× over target.) In practice the ~280 ms round trip
/// dominates a sequential pager, so this knob moves the realised rate very little — the soft-limit
/// change above is what this retune is actually for.
pub const ASTER_SPOT: RateLimits = RateLimits {
    orders: Meter::Published { published: 100, admitted: 90, window: Duration::from_secs(60) },
    ws_sends: Meter::Published { published: 10, admitted: 8, window: Duration::from_secs(1) },
    // Binance's REQUEST_WEIGHT pool, forked verbatim and likewise ungated — see `history`.
    rest_ip_weight: Meter::Ungated,
    history: History::Weighted {
        ceiling_per_min: 6000,
        soft_limit: 4800,
        page_delay: Duration::from_millis(125),
    },
    provenance: Provenance::Measured { on: "2026-08" },
};

/// Aster DEX **USDⓈ-M futures** (`fapi.asterdex.com`).
///
/// - `orders` — `ORDERS` = **1200 per MINUTE** (12x its own spot budget, per the Aster API docs);
///   gated ~10 % under at 1080.
/// - `ws_sends` — the same documented 10/s per connection, same 8/s floor.
/// - `history` — `REQUEST_WEIGHT` `MINUTE` = **2400** and `fapi/v3/klines?limit=1000` = weight
///   **5**, MEASURED 2026-08-04 from live `exchangeInfo` and a clean `x-mbx-used-weight-1m` probe
///   (aster's counter carried no concurrent traffic, unlike binance's, which a running backfill was
///   polluting). NOT the 6000 this budget inherited from Binance's SPOT template for its entire
///   life.
/// - `rest_ip_weight` — [`Meter::Ungated`], as at [`BINANCE_PERP`]: the forked `REQUEST_WEIGHT`
///   pool is ungated and its ceiling is declared under `history`.
///
/// ⚠ Aster **discovers** (since the `KlineSpec::exchange_info_url` field became a `Cow`, which a
/// per-`Environment` endpoint can fill): `vike_aster::data` composes the `exchangeInfo` URL of the
/// exact host it is paging, so a mainnet perp backfill reads the 2400 below off the venue each run
/// and the numbers here are its FALLBACK. RE-CONFIRMED live 2026-08-05 —
/// `https://fapi.asterdex.com/fapi/v3/exchangeInfo` still publishes `REQUEST_WEIGHT` MINUTE 2400,
/// matching this row exactly.
///
/// ⚠ The TESTNET fapi host publishes `"limit": -2`, which is not a budget; `vike_bridge_core::
/// rate_discovery` rejects non-positive limits, so a testnet backfill silently keeps the
/// `page_delay` below. That is the correct outcome and the reason the discovery URL must be
/// resolved per `Environment` rather than pinned to one network.
pub const ASTER_PERP: RateLimits = RateLimits {
    orders: Meter::Published { published: 1200, admitted: 1080, window: Duration::from_secs(60) },
    ws_sends: Meter::Published { published: 10, admitted: 8, window: Duration::from_secs(1) },
    // Binance's REQUEST_WEIGHT pool, forked verbatim and likewise ungated — see `history`.
    rest_ip_weight: Meter::Ungated,
    history: History::Weighted {
        ceiling_per_min: 2400,
        soft_limit: 2000,
        page_delay: Duration::from_millis(150),
    },
    provenance: Provenance::Measured { on: "2026-07" },
};

/// Bybit V5 (one row for spot and linear perp — bybit meters request COUNT per IP/UID, not per
/// market).
///
/// - `orders` — per-UID linear create = 20/s and cancel = 20/s (v5 rate-limit docs, verified
///   2026-07); `vike_bybit::ratelimit` gates ~10 % under at 18/s. Amend's tighter 10/s bucket is
///   not distinctly gated (amend-heavy bursts are rare and rejections are recoverable).
/// - `ws_sends` — bybit documents **no** WebSocket message-rate limit at all (only a
///   500-connections/5-min cap and a structural args-per-subscribe cap, both enforced elsewhere), so
///   the 30/10 s gate is purely vike's runaway catcher — [`Meter::Unpublished`], and
///   [`Meter::published`] refuses to report a cap for it.
/// - `history` — [`History::Unweighted`]: bybit runs no weight system. Its public REST is IP-limited
///   to ~600 req / 5 s (documented in prose in `vike_bybit::data`'s module doc, approximate and
///   never machine-readable), and the 200 ms page delay ≈ 5 req/s sits two orders of magnitude under
///   it. That request-COUNT limit is deliberately not a field: it is not a weight budget, nothing
///   reads it, and typing an approximation would make it look checkable.
/// - `rest_ip_weight` — [`Meter::Ungated`]: bybit meters request COUNT, never weight, so there is no
///   shared weight pool to gate. Its ~600 req/5 s IP limit is the prose one noted under `history`.
pub const BYBIT: RateLimits = RateLimits {
    orders: Meter::Published { published: 20, admitted: 18, window: Duration::from_secs(1) },
    ws_sends: Meter::Unpublished { admitted: 30, window: Duration::from_secs(10) },
    // Count-metered venue: no weight pool exists to gate.
    rest_ip_weight: Meter::Ungated,
    history: History::Unweighted { page_delay: Duration::from_millis(200) },
    provenance: Provenance::Measured { on: "2026-07" },
};

/// OKX v5 (one row for spot and SWAP — okx meters request COUNT).
///
/// - `orders` — place order = **60 / 2 s** keyed to *User ID + Instrument ID* (v5 docs, verified
///   2026-07), with a 1000 / 2 s sub-account aggregate ceiling above it. Each `OkxPerpRest` is
///   per-symbol, so a per-transport gate maps onto the per-INSTRUMENT bucket; `vike_okx::ratelimit`
///   gates ~17 % under at 50 — the widest margin on the roster, and the reason `admitted` is stored
///   rather than derived from one shared fraction. N symbols run N such gates (≈ N × 50/2 s), which
///   stays under the sub-account ceiling for ~20 symbols.
/// - `ws_sends` — okx documents **480 subscribe/unsubscribe/login requests per connection per
///   hour**, counting all three toward ONE budget; gated ~8 % under at 440/hr.
/// - `history` — [`History::Unweighted`]: no weight system. `history-candles` is IP-limited to
///   ~20 req / 2 s (prose, `vike_okx::data`'s module doc) and the 200 ms delay ≈ 10 req / 2 s sits
///   at half of it.
/// - `rest_ip_weight` — [`Meter::Ungated`]: okx meters request COUNT per endpoint bucket, not one
///   shared weight pool, so there is nothing of that shape to declare.
///
/// ⚠ okx pages **100** rows per response where every other venue here pages 1000, so the same
/// 200 ms delay buys a TENTH of the history per unit of budget. That is the endpoint's own page size
/// (`MAX_LIMIT` in `vike_okx::data`), not a rate knob, so it is recorded as provenance rather than
/// as a field — but it is the number to remember when comparing okx's pace to a sibling's.
pub const OKX: RateLimits = RateLimits {
    orders: Meter::Published { published: 60, admitted: 50, window: Duration::from_secs(2) },
    ws_sends: Meter::Published { published: 480, admitted: 440, window: Duration::from_secs(3600) },
    // Count-metered per endpoint bucket: no shared weight pool exists to gate.
    rest_ip_weight: Meter::Ungated,
    history: History::Unweighted { page_delay: Duration::from_millis(200) },
    provenance: Provenance::Measured { on: "2026-07" },
};

/// Deribit (options + perp). Deribit meters CREDITS across two independent pools, and both of this
/// row's meters ride WebSockets rather than REST.
///
/// - `orders` — the **matching-engine** pool: `private/buy|sell|cancel|edit` are limited to ~5 req/s
///   sustained on the default tier (Tier4, < $1M 7-day volume; higher tiers allow more).
///   `vike_deribit::ratelimit`'s order-socket gate sits at **exactly** 5 — the only meter on the
///   roster with NO safety margin, which is why `stays_under_published_cap` admits
///   `admitted == published` rather than requiring a strict inequality. It is the load-bearing gate:
///   it paces a market-maker's cancel/replace burst to the sustainable rate instead of racing into
///   `too_many_requests` rejections (stale quotes → adverse fills).
/// - `ws_sends` — the **non**-matching-engine pool (auth, `subscribe`, reconcile reads): ~20 req/s
///   sustained, gated ~10 % under at 18/s. Both `order_ws_gate`'s default bucket and the fill-stream
///   pump's gate are this number.
/// - `history` — [`History::Unweighted`]: the public chart-data REST draws the same non-ME credit
///   pool (~20 req/s, prose), and the 200 ms page delay keeps a long backfill an order of magnitude
///   under it while still pulling ~5000 bars per page.
/// - `rest_ip_weight` — [`Meter::Ungated`]. Deribit's credit pools are the nearest thing on the
///   roster to a shared budget, but they are declared where vike actually gates them — as request
///   RATES under `orders` (matching-engine) and `ws_sends` (non-ME) — and both ride WebSockets, not
///   REST. There is no third, REST-wide, weight-metered gate here.
pub const DERIBIT: RateLimits = RateLimits {
    orders: Meter::Published { published: 5, admitted: 5, window: Duration::from_secs(1) },
    ws_sends: Meter::Published { published: 20, admitted: 18, window: Duration::from_secs(1) },
    // The credit pools are gated as rates under `orders`/`ws_sends`, and ride WS, not REST.
    rest_ip_weight: Meter::Ungated,
    history: History::Unweighted { page_delay: Duration::from_millis(200) },
    provenance: Provenance::Measured { on: "2026-07" },
};

/// Polymarket CLOB.
///
/// - `orders` — [`Meter::Ungated`]: orders are REST and carry no rate gate today.
/// - `ws_sends` — Polymarket's CLOB WebSocket documents no subscribe/message RATE limit (the user
///   channel takes one auth-bearing subscribe per connection, then a 10 s `PING` keepalive), so the
///   30 / 10 s gate is a pure runaway catcher of vike's own choosing — [`Meter::Unpublished`].
/// - `history` — [`History::NotPaged`]: there is no kline endpoint to page. Polymarket history
///   arrives through archive readers instead (`vike_backfill`'s `pmxt` / `clickhouse_poly` / the
///   raw-frame `poly_reparse`), which read files and a local database, not a metered venue API.
///
/// - `rest_ip_weight` — [`Meter::Ungated`]: vike runs no REST gate of any kind at this venue, of
///   either shape (see `orders`).
///
/// [`Provenance::Unpublished`] because the one rate this row declares is entirely ours.
pub const POLYMARKET: RateLimits = RateLimits {
    orders: Meter::Ungated,
    ws_sends: Meter::Unpublished { admitted: 30, window: Duration::from_secs(10) },
    // No REST gate of either shape at this venue.
    rest_ip_weight: Meter::Ungated,
    history: History::NotPaged,
    provenance: Provenance::Unpublished,
};

/// Hyperliquid — the roster's ONE shared-pool venue, and the row [`RateLimits::rest_ip_weight`]
/// exists for. (This row declared the omission when the table landed; the axis now carries it.)
///
/// - `rest_ip_weight` — **1200 weight per rolling minute, per IP**, transcribed from HL's published
///   docs in the 2026-07-16 adapter research pass (§9, the source `vike_hyperliquid::transport`
///   cites) and held there as `IP_WEIGHT_PER_MIN`/`RATE_WINDOW` until this axis existed. ONE
///   `RateGate` meters **every REST call the bridge makes**: `/info` reads charge 2 (`l2Book` /
///   `clearinghouseState` / `orderStatus` / `spotClearinghouseState` / `allMids`), 60 (`userRole`)
///   or 20 (everything else); `/exchange` actions charge `1 + floor(batch_len / 40)`, so batching
///   amortises. The per-endpoint weight SCHEDULE stays in the bridge: it is a cost map over HL's
///   endpoints, not a budget — the same call as okx's page size, recorded as provenance here rather
///   than typed as a field.
///   The COMPOSITION ROOT clones one gate across the three REST consumers a live mount opens — the
///   exec thread's signed `/exchange`, the funding poller's `/info`, and the reconcile client
///   (`vike_mount::hyperliquid`'s `hl_ip_gate_and_transport`) — so they ride one window; that
///   sharing is a property of the METER, which is why this axis says per-IP. It is also why the
///   sharing has to be arranged rather than assumed: `vike_hyperliquid::transport`'s
///   `HyperliquidTransport::new` mints a fresh gate per transport, so a per-consumer gate does not
///   halve the risk, it MULTIPLIES the spend — N windows each correctly reporting itself inside a
///   quota the process as a whole is N-times over.
///   Feeds are NOT in that set: HL's market and user-data pumps are WebSocket and draw no REST
///   weight at all, which is what `ws_sends` below would cover if vike gated it.
///
///   ⚠ **Zero margin: `admitted == published == 1200`.** The second such meter on the roster after
///   [`DERIBIT`]'s matching-engine gate, and it is today's literal behaviour — the gate was
///   `RateGate::new(IP_WEIGHT_PER_MIN, RATE_WINDOW)`, sized AT the venue's cap. Whether it should
///   carry headroom like every other meter is a retune, and a retune is a separate change from
///   moving the number.
/// - `orders` — [`Meter::Ungated`], and that is the finding, not an omission: HL runs no separate
///   order counter for vike to gate. An order submit is charged to the shared pool above, competing
///   with `/info` reads inside the same minute. Declaring 1200 here would claim the venue permits
///   1200 orders per minute — neither what it meters nor what the gate does.
/// - `ws_sends` — [`Meter::Ungated`]: HL's market and user-data pumps carry no vike-side WS send
///   gate.
/// - `history` — [`History::NotPaged`]: vike pages no HL bar history.
pub const HYPERLIQUID: RateLimits = RateLimits {
    orders: Meter::Ungated,
    ws_sends: Meter::Ungated,
    rest_ip_weight: Meter::Published {
        published: 1200,
        admitted: 1200,
        window: Duration::from_secs(60),
    },
    history: History::NotPaged,
    provenance: Provenance::Measured { on: "2026-07" },
};

/// OANDA v20 (FX/CFD). vike runs no rate gate on this venue and pages no bar history through a
/// metered endpoint. → nothing declared.
pub const OANDA: RateLimits = RateLimits::NOT_DECLARED;

/// IG (FX/CFD). No rate gate, no paged history. → nothing declared.
pub const IG: RateLimits = RateLimits::NOT_DECLARED;

/// FXCM ForexConnect (FX/CFD). Execution rides a native C++ SDK behind the `fxcm` feature, which
/// does its own transport pacing; vike declares no gate. → nothing declared.
pub const FXCM: RateLimits = RateLimits::NOT_DECLARED;

/// Dukascopy (FX). History is keyless `.bi5` static files (no metered API) and execution runs
/// through the JForex Java sidecar over stdio, so there is no HTTP/WS meter to gate. → nothing
/// declared.
pub const DUKASCOPY: RateLimits = RateLimits::NOT_DECLARED;

/// Interactive Brokers. Execution and history run over TWS/IB-Gateway sockets and the CP Gateway,
/// whose pacing rules live in the gateway rather than in a published per-IP budget vike gates on.
/// → nothing declared.
pub const IBKR: RateLimits = RateLimits::NOT_DECLARED;

/// cTrader (FX/CFD). A dedicated authed protobuf socket per session; no vike-side rate gate. →
/// nothing declared.
pub const CTRADER: RateLimits = RateLimits::NOT_DECLARED;

/// Alpaca (US equities + crypto). No vike-side rate gate today. → nothing declared.
pub const ALPACA: RateLimits = RateLimits::NOT_DECLARED;

// vike:new-venue:row /// TODO(new-venue: {venue}): read the venue's PUBLISHED limits. `NOT_DECLARED` is the honest
// vike:new-venue:row /// scaffolded value — vike gates nothing here yet — and it is a real classification only
// vike:new-venue:row /// because it is NAMED (`undeclared_roster_venues_are_named_rows_not_fall_through`).
// vike:new-venue:row pub const {VENUE}: RateLimits = RateLimits::NOT_DECLARED;
// vike:new-venue:row

// ---------------------------------------------------------------------------------------------
// COMPILE-TIME invariants, not tests: a row whose gate would out-spend the venue fails the BUILD.
//
// This is the generalisation of the two `const _: () = assert!(soft_limit < ceiling)` guards that
// used to sit beside `BINANCE_KLINE_SPOT`/`BINANCE_KLINE_PERP` in `vike_binance::data` and
// `ASTER_KLINE` in `vike_aster::data`. Those covered ONE field on THREE specs; these cover every
// meter on every row, and they live beside the numbers they constrain instead of beside the
// consumer that happened to read them.
// ---------------------------------------------------------------------------------------------
const _: () = assert!(BINANCE_SPOT.stays_under_published_caps());
const _: () = assert!(BINANCE_PERP.stays_under_published_caps());
const _: () = assert!(ASTER_SPOT.stays_under_published_caps());
const _: () = assert!(ASTER_PERP.stays_under_published_caps());
const _: () = assert!(BYBIT.stays_under_published_caps());
const _: () = assert!(OKX.stays_under_published_caps());
const _: () = assert!(DERIBIT.stays_under_published_caps());
const _: () = assert!(POLYMARKET.stays_under_published_caps());
const _: () = assert!(HYPERLIQUID.stays_under_published_caps());
const _: () = assert!(RateLimits::NOT_DECLARED.stays_under_published_caps());

/// The registry: the declared [`RateLimits`] for a canonical `(venue, market)` pair — one arm per
/// [`crate::venues::VENUES`] entry, mirroring [`crate::venue_margin_support::venue_margin_support`].
///
/// **binance and aster are the only market-SPLIT venues**; every other roster venue meters one pool
/// and is served the SAME row for both markets (asserted, not assumed — see
/// `market_split_venues_are_exactly_binance_and_aster`).
///
/// An unknown venue string returns [`RateLimits::NOT_DECLARED`], which is fail-CLOSED in the literal
/// sense: every accessor on it panics, so a typo can never yield a permissive gate.
#[must_use]
pub fn rate_limits_for(venue: &str, market: Market) -> RateLimits {
    match (venue, market) {
        ("binance", Market::Spot) => BINANCE_SPOT,
        ("binance", Market::Perp) => BINANCE_PERP,
        ("aster", Market::Spot) => ASTER_SPOT,
        ("aster", Market::Perp) => ASTER_PERP,
        ("bybit", _) => BYBIT,
        ("okx", _) => OKX,
        ("deribit", _) => DERIBIT,
        ("polymarket", _) => POLYMARKET,
        ("hyperliquid", _) => HYPERLIQUID,
        ("oanda", _) => OANDA,
        ("ig", _) => IG,
        ("fxcm", _) => FXCM,
        ("dukascopy", _) => DUKASCOPY,
        ("ibkr", _) => IBKR,
        ("ctrader", _) => CTRADER,
        ("alpaca", _) => ALPACA,
        // vike:new-venue:row ("{venue}", _) => {VENUE}, // TODO(new-venue: {venue}): split into (Spot, Perp) arms if the venue meters two pools
        _ => RateLimits::NOT_DECLARED,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every roster venue paired with the row(s) it declares. The completeness test below asserts
    /// this covers [`crate::venues::VENUES`] exactly.
    ///
    /// `#[rustfmt::skip]`: a `just new-venue` marker at the TAIL of a bracketed literal is
    /// re-indented by rustfmt once a row ending in a trailing `//` comment is generated above it,
    /// which defeats `--remove`. Gated by `crates/vike-ops/tests/new_venue_gate.rs`'s
    /// `a_trailing_comment_marker_is_rustfmt_skipped_unless_a_recognised_sibling_follows`.
    #[rustfmt::skip]
    const MATRIX: &[(&str, Market, RateLimits)] = &[
        ("binance", Market::Spot, BINANCE_SPOT),
        ("binance", Market::Perp, BINANCE_PERP),
        ("aster", Market::Spot, ASTER_SPOT),
        ("aster", Market::Perp, ASTER_PERP),
        ("bybit", Market::Spot, BYBIT),
        ("okx", Market::Spot, OKX),
        ("deribit", Market::Spot, DERIBIT),
        ("polymarket", Market::Spot, POLYMARKET),
        ("hyperliquid", Market::Spot, HYPERLIQUID),
        ("oanda", Market::Spot, OANDA),
        ("ig", Market::Spot, IG),
        ("fxcm", Market::Spot, FXCM),
        ("dukascopy", Market::Spot, DUKASCOPY),
        ("ibkr", Market::Spot, IBKR),
        ("ctrader", Market::Spot, CTRADER),
        ("alpaca", Market::Spot, ALPACA),
        // vike:new-venue:row ("{venue}", Market::Spot, {VENUE}), // TODO(new-venue: {venue}): a market-SPLIT venue declares two rows
    ];

    /// The ORDER meter, pinned VERBATIM as `(venue, market, published, admitted, window_secs)`.
    ///
    /// These are the numbers `crates/bridges/*/src/ratelimit.rs` built its `RateGate`s from before
    /// this table existed — the `admitted` column IS the old `SPOT_ORDERS`/`PERP_ORDERS`/`ORDER_OPS`
    /// const, and the `window_secs` column IS the old `ORDERS_WINDOW`/`ORDER_WINDOW`. Any drift is
    /// loud here, which is what makes "this is a MOVE, not a retune" checkable rather than asserted.
    #[test]
    fn order_meters_are_pinned() {
        #[rustfmt::skip]
        let rows: &[(&str, Market, usize, usize, u64)] = &[
            // binance: ORDERS metered per 10s, spot and perp budgets differ 3x
            ("binance", Market::Spot,  100,   90, 10),
            ("binance", Market::Perp,  300,  270, 10),
            // aster: the same counter metered per MINUTE, and a 12x spot/perp split
            ("aster",   Market::Spot,  100,   90, 60),
            ("aster",   Market::Perp, 1200, 1080, 60),
            // bybit/okx/deribit: one pool, one row
            ("bybit",   Market::Spot,   20,   18,  1),
            ("okx",     Market::Spot,   60,   50,  2),
            ("deribit", Market::Spot,    5,    5,  1),   // matching-engine, NO margin
        ];
        for (venue, market, published, admitted, window_secs) in rows {
            let m = rate_limits_for(venue, *market).orders;
            assert_eq!(m.published(), *published, "{venue} {market:?} published");
            assert_eq!(m.admitted(), *admitted, "{venue} {market:?} admitted");
            assert_eq!(m.window(), Duration::from_secs(*window_secs), "{venue} {market:?} window");
        }
    }

    /// The WS-send meter, pinned verbatim: `(venue, published_or_none, admitted, window_secs)`.
    /// `None` in the published column is [`Meter::Unpublished`] — the venue documents no WS rate and
    /// the number is vike's own runaway brake.
    #[test]
    fn ws_send_meters_are_pinned() {
        #[rustfmt::skip]
        let rows: &[(&str, Option<usize>, usize, u64)] = &[
            ("binance",    Some(5),    4,    1),
            ("aster",      Some(10),   8,    1),
            ("deribit",    Some(20),  18,    1),   // the non-matching-engine credit pool
            ("okx",        Some(480), 440, 3600),  // 480 subscribe/unsubscribe/login per hour
            ("bybit",      None,      30,   10),   // no documented WS rate: a runaway catcher
            ("polymarket", None,      30,   10),   // ditto
        ];
        for (venue, published, admitted, window_secs) in rows {
            let m = rate_limits_for(venue, Market::Spot).ws_sends;
            assert_eq!(m.published_cap(), *published, "{venue} ws published");
            assert_eq!(m.admitted(), *admitted, "{venue} ws admitted");
            assert_eq!(m.window(), Duration::from_secs(*window_secs), "{venue} ws window");
        }
        // The perp rows carry the SAME WS numbers — the WS handshake is per-connection, not
        // per-market, on both split venues.
        for venue in ["binance", "aster"] {
            let (spot, perp) =
                (rate_limits_for(venue, Market::Spot), rate_limits_for(venue, Market::Perp));
            assert_eq!(
                spot.ws_sends, perp.ws_sends,
                "{venue} ws is per-connection, not per-market"
            );
        }
    }

    /// The shared per-IP REST WEIGHT pool, pinned verbatim — and pinned as EXCLUSIVE, because the
    /// axis's whole risk is a future row duplicating a number that already lives elsewhere.
    ///
    /// Hyperliquid's literals ARE `vike_hyperliquid::transport`'s former `IP_WEIGHT_PER_MIN` /
    /// `RATE_WINDOW` consts, so a drift here changes live HL pacing and fails loudly. Every other
    /// roster venue is [`Meter::Ungated`] — which says vike gates no such pool there, NOT that the
    /// venue meters none: binance/aster's `REQUEST_WEIGHT` is the same class of budget and is
    /// declared once, under `history`. A row that "fills in" 6000 here would give that fact two
    /// homes; this test is what refuses it.
    #[test]
    fn rest_ip_weight_meters_are_pinned() {
        for market in [Market::Spot, Market::Perp] {
            let m = rate_limits_for("hyperliquid", market).rest_ip_weight;
            assert_eq!(m.published(), 1200, "HL publishes 1200 weight/min per IP");
            assert_eq!(m.admitted(), 1200, "the gate sits AT the cap — no margin today");
            assert_eq!(m.window(), Duration::from_secs(60), "a rolling MINUTE");
        }
        for &v in crate::venues::VENUES {
            if v == "hyperliquid" {
                continue;
            }
            for market in [Market::Spot, Market::Perp] {
                let m = rate_limits_for(v, market).rest_ip_weight;
                assert_eq!(m, Meter::Ungated, "{v} {market:?}: no vike-gated shared REST pool");
                assert_eq!(m.published_cap(), None, "{v} {market:?}");
            }
        }
        // The binance/aster weight ceilings stay where they were — one fact, one home.
        assert_eq!(BINANCE_SPOT.history.ceiling_per_min(), 6000);
        assert_eq!(ASTER_PERP.history.ceiling_per_min(), 2400);
    }

    /// The paged-history budget, pinned verbatim. `Weighted` rows carry the MEASURED 2026-08-04
    /// ceilings and the `page_delay`/`weight_soft_limit` that `vike_binance::data`'s and
    /// `vike_aster::data`'s `KlineSpec`s used to hold as local consts.
    #[test]
    fn history_budgets_are_pinned() {
        #[rustfmt::skip]
        let weighted: &[(&str, Market, u64, u64, u64)] = &[
            // (venue, market, ceiling_per_min, soft_limit, page_delay_ms)
            ("binance", Market::Spot, 6000, 4800,  50),
            ("binance", Market::Perp, 2400, 2000, 150),
            ("aster",   Market::Spot, 6000, 4800, 125),  // sapi's own, measured 2026-08-05
            ("aster",   Market::Perp, 2400, 2000, 150),
        ];
        for (venue, market, ceiling, soft, delay_ms) in weighted {
            let h = rate_limits_for(venue, *market).history;
            assert_eq!(h.ceiling_per_min(), *ceiling, "{venue} {market:?} ceiling");
            assert_eq!(h.soft_limit(), *soft, "{venue} {market:?} soft limit");
            assert_eq!(
                h.page_delay(),
                Duration::from_millis(*delay_ms),
                "{venue} {market:?} delay"
            );
        }
        // The three venues that publish NO weight budget: a fixed 200ms page delay is the whole
        // pacing story, and there is deliberately no ceiling to read.
        for venue in ["bybit", "okx", "deribit"] {
            let h = rate_limits_for(venue, Market::Spot).history;
            assert_eq!(
                h,
                History::Unweighted { page_delay: Duration::from_millis(200) },
                "{venue}"
            );
            assert_eq!(h.published_ceiling_per_min(), None, "{venue} publishes no weight ceiling");
        }
        // ...and the venues vike pages nothing from.
        for venue in [
            "polymarket",
            "hyperliquid",
            "oanda",
            "ig",
            "fxcm",
            "dukascopy",
            "ibkr",
            "ctrader",
            "alpaca",
        ] {
            assert_eq!(rate_limits_for(venue, Market::Spot).history, History::NotPaged, "{venue}");
        }
    }

    /// Completeness: every registry venue resolves to its declared const, so the pinned tables above
    /// are authoritative.
    #[test]
    fn registry_maps_every_known_venue() {
        for (venue, market, want) in MATRIX {
            assert_eq!(rate_limits_for(venue, *market), *want, "{venue} {market:?}");
        }
    }

    /// Completeness vs the canonical roster: every [`crate::venues::VENUES`] entry has a NAMED
    /// declared row, and the registry serves exactly it for BOTH markets. Adding a venue to the
    /// roster fails here until its rows exist — the whole point of the playbook.
    ///
    /// A row equal to [`RateLimits::NOT_DECLARED`] still counts as declared: the named row IS the
    /// declaration that the venue was classified (vike gates nothing here) rather than forgotten,
    /// exactly as `venue_margin_support`' FX rows name `VenueMarginSupport::UNKNOWN`. That the seven such rows are
    /// NAMED and not the registry's `_` fall-through is the sibling test below.
    #[test]
    fn every_roster_venue_has_a_declared_row() {
        for &v in crate::venues::VENUES {
            let rows: Vec<(&str, Market, RateLimits)> =
                MATRIX.iter().copied().filter(|(rv, _, _)| *rv == v).collect();
            assert!(!rows.is_empty(), "no RateLimits row declared for roster venue {v}");
            for (_, market, want) in rows.iter().copied() {
                assert_eq!(rate_limits_for(v, market), want, "{v}: registry must serve its row");
            }
            // A market-UNIFORM venue's single row answers both markets — a venue is never
            // half-declared, whatever `MATRIX` happens to list.
            if rows.len() == 1 {
                let want = rows[0].2;
                for market in [Market::Spot, Market::Perp] {
                    assert_eq!(rate_limits_for(v, market), want, "{v} {market:?}");
                }
            } else {
                assert_eq!(rows.len(), 2, "{v}: a split venue declares exactly spot + perp");
            }
        }
        assert_eq!(MATRIX.len(), crate::venues::VENUES.len() + 2, "14 venues, 2 of them split");
    }

    /// The registry's `_` fallback arm is unreachable for a roster venue: every `NOT_DECLARED` row a
    /// roster venue gets is a NAMED const. Without this, deleting a venue's arm would look identical
    /// to declaring it empty.
    ///
    /// ⚠ [`HYPERLIQUID`] used to be in this list — it was the row that declared its own omission.
    /// It now carries a real [`RateLimits::rest_ip_weight`] budget, so it is a DECLARED venue and
    /// the `declared ^ is_named` sweep below is what would catch it drifting back.
    #[test]
    fn undeclared_roster_venues_are_named_rows_not_fall_through() {
        #[rustfmt::skip]
        let named: &[(&str, RateLimits)] = &[
            ("oanda", OANDA), ("ig", IG), ("fxcm", FXCM),
            ("dukascopy", DUKASCOPY), ("ibkr", IBKR), ("ctrader", CTRADER), ("alpaca", ALPACA),
            // vike:new-venue:row ("{venue}", {VENUE}), // TODO(new-venue: {venue}): DELETE this row once the venue declares real numbers
        ];
        for (venue, row) in named {
            assert_eq!(*row, RateLimits::NOT_DECLARED, "{venue} declares nothing today");
            for market in [Market::Spot, Market::Perp] {
                assert_eq!(rate_limits_for(venue, market), *row, "{venue} {market:?}");
            }
        }
        // Every roster venue is either in `named` or carries real numbers — no third state.
        for &v in crate::venues::VENUES {
            let declared = rate_limits_for(v, Market::Spot) != RateLimits::NOT_DECLARED;
            let is_named = named.iter().any(|(nv, _)| *nv == v);
            assert!(declared ^ is_named, "{v} must be exactly one of declared / explicitly empty");
        }
    }

    /// Only binance and aster are market-split; every other roster venue serves ONE row for both
    /// markets. Left implicit this is the kind of thing that silently rots when a venue gains a
    /// second host.
    #[test]
    fn market_split_venues_are_exactly_binance_and_aster() {
        for &v in crate::venues::VENUES {
            let spot = rate_limits_for(v, Market::Spot);
            let perp = rate_limits_for(v, Market::Perp);
            let split = spot != perp;
            assert_eq!(
                split,
                v == "binance" || v == "aster",
                "{v}: market-split is {split}, expected only binance/aster to differ"
            );
        }
        // ...and the split is real on the meter that motivates it, not a cosmetic difference.
        assert_ne!(BINANCE_SPOT.orders, BINANCE_PERP.orders);
        assert_ne!(BINANCE_SPOT.history, BINANCE_PERP.history);
        assert_ne!(ASTER_SPOT.orders, ASTER_PERP.orders);
        // Aster's HISTORY is split too, as of the sapi retune: sapi publishes 6000 where fapi
        // publishes 2400 — the same shape binance has, on the same two host roles. It used to be
        // deliberately UNsplit, with one fallback serving both hosts, which meant a spot backfill
        // braked on the fapi soft limit at 33 % of the budget discovery had already found for it.
        assert_ne!(ASTER_SPOT.history, ASTER_PERP.history);
    }

    /// The safety property this table exists to make checkable: no gate anywhere admits more than
    /// the venue published. Enforced at COMPILE time by the `const _: ()` block above; asserted here
    /// over the whole roster so a NEW row cannot land without it.
    #[test]
    fn no_row_admits_more_than_the_venue_published() {
        for &v in crate::venues::VENUES {
            for market in [Market::Spot, Market::Perp] {
                let row = rate_limits_for(v, market);
                assert!(row.stays_under_published_caps(), "{v} {market:?} out-spends the venue");
                for (name, m) in [
                    ("orders", row.orders),
                    ("ws_sends", row.ws_sends),
                    ("rest_ip_weight", row.rest_ip_weight),
                ] {
                    if let Some(published) = m.published_cap() {
                        assert!(m.admitted() <= published, "{v} {market:?} {name}");
                        assert!(m.admitted() > 0, "{v} {market:?} {name} would admit nothing");
                    }
                }
                if let Some(ceiling) = row.history.published_ceiling_per_min() {
                    assert!(row.history.soft_limit() < ceiling, "{v} {market:?} soft limit");
                }
            }
        }
        // An unknown venue is fail-closed, and equals Default.
        assert_eq!(rate_limits_for("nasdaq", Market::Spot), RateLimits::NOT_DECLARED);
        assert_eq!(RateLimits::default(), RateLimits::NOT_DECLARED);
        assert!(RateLimits::NOT_DECLARED.stays_under_published_caps());
    }

    /// The meters whose gate sits EXACTLY at the published cap — the reason the invariant is `<=`
    /// rather than `<`. Pinned as an exhaustive list so that "every gate has headroom" is never
    /// assumed, and so a NEW zero-margin gate has to be added here deliberately.
    ///
    /// **Two today.** Deribit's matching-engine gate at 5/s, and Hyperliquid's per-IP REST weight
    /// pool at 1200/min — the latter surfaced by moving the number into this table, where the
    /// margin column made it visible. Both are today's literal behaviour; adding headroom to either
    /// would change live pacing, which is a retune and belongs in its own change.
    #[test]
    fn zero_margin_meters_are_pinned() {
        let mut zero_margin = Vec::new();
        for &v in crate::venues::VENUES {
            for market in [Market::Spot, Market::Perp] {
                let row = rate_limits_for(v, market);
                for (name, m) in [
                    ("orders", row.orders),
                    ("ws_sends", row.ws_sends),
                    ("rest_ip_weight", row.rest_ip_weight),
                ] {
                    // `published_cap()` is `Some` only for `Meter::Published`, which is also the
                    // only variant `admitted()` may be asked of here — an ungated meter panics.
                    if m.published_cap().is_some_and(|p| p == m.admitted()) {
                        zero_margin.push(format!("{v}/{name}"));
                    }
                }
            }
        }
        zero_margin.sort();
        zero_margin.dedup();
        assert_eq!(
            zero_margin,
            ["deribit/orders", "hyperliquid/rest_ip_weight"],
            "an un-noticed zero-margin gate appeared"
        );
        assert_eq!(DERIBIT.orders.published(), 5);
        assert_eq!(DERIBIT.orders.admitted(), 5);
        assert_eq!(HYPERLIQUID.rest_ip_weight.published(), 1200);
        assert_eq!(HYPERLIQUID.rest_ip_weight.admitted(), 1200);
    }

    /// The three venues that publish nothing machine-readable get a VARIANT, never a number. This is
    /// the honesty rule as a test: a future edit that "fills in" a plausible ceiling for bybit/okx/
    /// deribit fails here.
    #[test]
    fn unpublished_budgets_are_a_variant_not_a_fabricated_number() {
        for venue in ["bybit", "okx", "deribit"] {
            let h = rate_limits_for(venue, Market::Spot).history;
            assert!(matches!(h, History::Unweighted { .. }), "{venue} publishes no weight ceiling");
            assert_eq!(h.published_ceiling_per_min(), None, "{venue}");
        }
        // The WS meters with no documented venue cap: the admitted rate exists, the published one
        // does NOT, and asking for it is a panic rather than a plausible integer.
        for venue in ["bybit", "polymarket"] {
            let m = rate_limits_for(venue, Market::Spot).ws_sends;
            assert!(matches!(m, Meter::Unpublished { .. }), "{venue} documents no WS rate");
            assert_eq!(m.published_cap(), None, "{venue}");
            assert_eq!(m.admitted(), 30, "{venue}: vike's own runaway brake");
        }
        // Provenance says so at row level too.
        assert_eq!(POLYMARKET.provenance, Provenance::Unpublished);
    }

    /// An absent cap REFUSES to answer rather than returning a number a caller could mistake for a
    /// venue fact. In a `const` initializer — how every bridge consumes this — each of these is a
    /// COMPILE error, which is what makes an undeclared row unable to produce a gate; here they are
    /// reached through the runtime registry, so they are ordinary panics a test can observe.
    #[test]
    #[should_panic(expected = "the venue publishes no cap for this meter")]
    fn an_unpublished_meter_refuses_to_report_a_published_cap() {
        let _ = rate_limits_for("bybit", Market::Spot).ws_sends.published();
    }

    #[test]
    #[should_panic(expected = "no gate is declared for this meter")]
    fn an_ungated_meter_refuses_to_produce_a_rate() {
        let _ = rate_limits_for("oanda", Market::Spot).orders.admitted();
    }

    #[test]
    #[should_panic(expected = "vike pages no history from this venue")]
    fn a_not_paged_history_refuses_to_produce_a_page_delay() {
        let _ = rate_limits_for("polymarket", Market::Spot).history.page_delay();
    }

    #[test]
    #[should_panic(expected = "this venue runs no request-weight meter")]
    fn an_unweighted_history_refuses_to_produce_a_soft_limit() {
        let _ = rate_limits_for("bybit", Market::Spot).history.soft_limit();
    }

    /// Provenance: every row carrying VENUE numbers is dated, every row whose numbers are only ours
    /// says `Unpublished`, and every empty row says `NotDeclared`. A row can never carry a published
    /// cap AND claim nothing was published.
    #[test]
    fn provenance_matches_what_each_row_actually_carries() {
        for &v in crate::venues::VENUES {
            for market in [Market::Spot, Market::Perp] {
                let row = rate_limits_for(v, market);
                let has_published_cap = row.orders.published_cap().is_some()
                    || row.ws_sends.published_cap().is_some()
                    || row.rest_ip_weight.published_cap().is_some()
                    || row.history.published_ceiling_per_min().is_some();
                match row.provenance {
                    Provenance::Measured { on } => {
                        assert!(has_published_cap, "{v} {market:?}: dated but publishes nothing");
                        // ISO shape (`YYYY-MM` or `YYYY-MM-DD`), asserted on the SHAPE rather than
                        // on a year literal so this does not have to be edited every January.
                        let b = on.as_bytes();
                        assert!(
                            (b.len() == 7 || b.len() == 10) && b[4] == b'-',
                            "{v} {market:?}: {on:?} is not an ISO YYYY-MM[-DD] date"
                        );
                        assert!(b[..4].iter().all(u8::is_ascii_digit), "{v} {market:?}: {on:?}");
                    }
                    Provenance::Unpublished => {
                        assert!(!has_published_cap, "{v} {market:?}: claims nothing was published");
                        // ...but SOMETHING is gated, else this would be NotDeclared.
                        assert_ne!(row, RateLimits::NOT_DECLARED, "{v} {market:?}");
                    }
                    Provenance::NotDeclared => {
                        assert_eq!(row, RateLimits::NOT_DECLARED, "{v} {market:?}");
                    }
                }
            }
        }
        // The dated rows are exactly the six crypto venues with venue-published caps (hyperliquid
        // joined when its per-IP weight pool moved in, in roster order).
        let dated: Vec<&str> = crate::venues::VENUES
            .iter()
            .copied()
            .filter(|v| {
                matches!(rate_limits_for(v, Market::Spot).provenance, Provenance::Measured { .. })
            })
            .collect();
        assert_eq!(dated, ["binance", "bybit", "okx", "deribit", "aster", "hyperliquid"]);
    }

    /// The market token round-trips against the string form a `PaceBook` key and
    /// `vike_binance::data::kline_market` already use, and nothing else parses — spot and perp are
    /// exactly the pair whose budgets differ, so a near-miss must not resolve.
    #[test]
    fn market_strings_round_trip_and_reject_everything_else() {
        for m in [Market::Spot, Market::Perp] {
            assert_eq!(Market::from_market_str(m.as_str()), Some(m));
        }
        assert_eq!(Market::Spot.as_str(), "spot");
        assert_eq!(Market::Perp.as_str(), "perp");
        for bad in ["", "Spot", "SPOT", "perpetual", "futures", "swap", "linear"] {
            assert_eq!(Market::from_market_str(bad), None, "{bad:?} must not resolve");
        }
    }
}
