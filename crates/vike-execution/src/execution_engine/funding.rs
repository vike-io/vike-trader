//! A PAPER engine's FUNDING CLOCK (`crate::FundingClock`): the venue's public funding rate, told by
//! the core's funding-rate arm (`vike_core`'s `note_funding_rate`), says WHEN each settlement is (the
//! symbol's own `next_timestamp`, whatever its interval: 1, 2, 4 or 8 hours, or one an hour on a capped rate)
//! and at what rate. The core settles it at that instant (a timer armed at `next_timestamp`, or the venue's roll
//! when that comes first), prices it on this engine's own position with the one funding law
//! (`vike_model::ContractValue::funding`, handed to the core by its host) and folds it as an ordinary
//! journaled `Event::Funding`, so a replay books the same money without the rate lane.
//!
//! The clock is snapshotted (a restart between two ticks still books the settlement it passed, on the
//! restored position) and not hashed (the rate lane is not journaled). A live engine never has one: the
//! core keeps clocks only on an engine whose STATED tier is paper.

use super::{Engine, ExecutionClient};
use crate::FundingClock;

/// A settlement the venue's funding feed says is due and this engine has not booked yet.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FundingDue {
    /// The settlement time, epoch-ms (the venue's `next_timestamp` for it).
    pub settled_timestamp: i64,
    /// The rate the venue stated last for that settlement, per its funding interval.
    pub rate: f64,
    /// The price the venue charges it on when that is not the mark (Hyperliquid's oracle), as stated
    /// last; `None` is the mark.
    pub price: Option<f64>,
}

/// What one funding observation did to this engine's clock of its symbol ([`Engine::note_funding_rate`]).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct FundingNote {
    /// The settlement the venue ROLLED past that this engine had not booked.
    pub due: Option<FundingDue>,
    /// The instant the core should arm its settlement timer at: the clock's `next_timestamp`, the first time
    /// this process sees it.
    pub arm_at: Option<i64>,
}

impl<C: ExecutionClient> Engine<C> {
    /// Tell this engine one funding observation of `symbol`: `rate` (and `price`, when the venue charges
    /// on something other than the mark) for the settlement at `next_timestamp`.
    ///
    /// The first observation of a symbol starts its clock; a rate that is not a finite number changes
    /// nothing; one with the same `next_timestamp` refreshes the rate. One with an EARLIER `next_timestamp` that is still
    /// past the last booked settlement means the venue brought its settlement forward (binance and okx drop
    /// to an hourly interval when the rate hits its cap): the clock moves to it and its timer is re-armed;
    /// one at or before the last booked settlement is an older observation and changes nothing. One with a
    /// LATER `next_timestamp` means the venue rolled: the held settlement happened, and it is answered in
    /// [`FundingNote::due`] at the last rate stated for it unless it was booked already (by the timer, or
    /// before a restart). A due settlement is marked booked here whether or not the engine then holds a
    /// position, so a position opened after it is never charged for it.
    ///
    /// COLD: called once per funding observation (about a second per perpetual), never by the fold.
    pub fn note_funding_rate(
        &mut self,
        symbol: &str,
        rate: f64,
        price: Option<f64>,
        next_ts: i64,
    ) -> FundingNote {
        if !rate.is_finite() {
            return FundingNote::default();
        }
        let price = price.filter(|p| p.is_finite() && *p > 0.0);
        let Some(clock) = self.funding_clocks.iter_mut().find(|c| c.symbol == symbol) else {
            self.funding_clocks.push(FundingClock {
                symbol: symbol.to_string(),
                next_timestamp: next_ts,
                rate,
                price,
                settled_timestamp: 0,
                armed: true,
            });
            return FundingNote { due: None, arm_at: Some(next_ts) };
        };
        if next_ts <= clock.settled_timestamp {
            return FundingNote::default();
        }
        let mut note = FundingNote::default();
        if next_ts < clock.next_timestamp {
            clock.next_timestamp = next_ts;
            clock.armed = false;
        } else if next_ts > clock.next_timestamp {
            if clock.next_timestamp > clock.settled_timestamp {
                note.due = Some(FundingDue {
                    settled_timestamp: clock.next_timestamp,
                    rate: clock.rate,
                    price: clock.price,
                });
                clock.settled_timestamp = clock.next_timestamp;
            }
            clock.next_timestamp = next_ts;
            clock.armed = false;
        }
        clock.rate = rate;
        clock.price = price;
        if !clock.armed {
            clock.armed = true;
            note.arm_at = Some(next_ts);
        }
        note
    }

    /// Every settlement whose time the core's clock has reached (`next_timestamp <= now_ms`) and this engine
    /// has not booked: `(symbol, settlement)`, each marked booked. The core asks it on the timer it armed
    /// at a settlement time, so a settlement is charged on the position held AT that instant, not when
    /// the venue's next observation arrives. COLD: once per armed settlement time.
    pub fn settle_due_funding(&mut self, now_ms: i64) -> Vec<(String, FundingDue)> {
        let mut due = Vec::new();
        for clock in &mut self.funding_clocks {
            if clock.next_timestamp <= now_ms && clock.next_timestamp > clock.settled_timestamp {
                clock.settled_timestamp = clock.next_timestamp;
                due.push((
                    clock.symbol.clone(),
                    FundingDue {
                        settled_timestamp: clock.next_timestamp,
                        rate: clock.rate,
                        price: clock.price,
                    },
                ));
            }
        }
        due
    }

    /// This engine's funding clocks, one per perpetual it was told a rate for (empty on a live engine).
    pub fn funding_clocks(&self) -> &[FundingClock] {
        &self.funding_clocks
    }

    /// A funding event of `symbol` stamped `ts` folded on this engine (live, or replayed from the
    /// journal): the clock counts that settlement as booked. This is what keeps a crash between the
    /// event's journal write and the next `Snapshot` from booking it twice: the refold of the journal tail
    /// passes it here too, and the restored clock then says it was booked. The core's restart catch-up
    /// (`vike_core`'s `catch_up_funding`) calls it too, with the newest settlement it passed.
    #[cold]
    pub fn settle_funding_clock(&mut self, symbol: &str, ts: i64) {
        if let Some(clock) = self.funding_clocks.iter_mut().find(|c| c.symbol == symbol) {
            clock.settled_timestamp = clock.settled_timestamp.max(ts);
        }
    }
}

/// `clocks` as a restore puts them back: none armed in this process, which is what the journal's bytes
/// give (`FundingClock::armed` is not serialized) and what tells the core's catch-up a clock is restored.
pub(super) fn restored_clocks(clocks: &[FundingClock]) -> Vec<FundingClock> {
    clocks.iter().map(|c| FundingClock { armed: false, ..c.clone() }).collect()
}
