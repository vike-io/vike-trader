//! **A bar run's equity curve, rebuilt from the log of the events that move its cash or a position, and its
//! bars** (decision 0078's amendment of 2026-11-19: a saved bar run keeps this log, not its curve). With
//! [`OrderBrokerSimulator::ledger_events`] `Some` ([`OrderBrokerSimulator::start_ledger_log`]), every mutator of
//! `cash` or of a position pushes one [`LedgerEvent`], stamped with the step it happened in:
//!
//! - a fill, a settlement, an expiry, an exercise, a liquidation, a combo leg (`OrderBrokerSimulator::fold_fill`, the
//!   one door each books through);
//! - a funding charge (the bar rule in `StrategyEngine::run`, the schedule in
//!   `OrderBrokerSimulator::charge_funding_schedule`);
//! - a conversion's new rate (`OrderBrokerSimulator::advance_quote_rates`): the symbol's new value and the cash the
//!   held position's entry notional moved by.
//!
//! The MARK is no event: it is each step's close, read from the bars. `settle_variation_margin` moves the cost
//! basis and the realized PnL, never cash or a size, so it logs nothing. [`rebuild_equity_curve`] replays the log
//! through the engine's own folds ([`fill_cash`], `vike_model::TradeFold`, [`equity_of`]), no second formula, so a
//! rebuilt step equals the engine's bit for bit. ⚠ A NEW site that moves `cash` or a position must log here: the
//! test build checks EVERY bar run against its rebuild (`StrategyEngine::run`, under `test-support`), and the
//! harness checks each run it logs before it saves the log in place of the curve
//! (`crates/vike-backtest/src/run/equity_log.rs`), keeping the curve where the two differ.
//!
//! On disk ([`encode_ledger_events`]): the eight bytes of `LEDGER_EVENTS_MAGIC` (its last byte the format's
//! version), the event count as a little-endian `u64`, then per event a tag byte, the step and the symbol index as
//! little-endian `u32`, and the payload: a fill's side byte and its size, price and fee; a funding's amount; a
//! revalue's kind byte, per-unit factor and cash. Every `f64` as its little-endian bits, so it reads back bit for
//! bit.

use vike_model::run_request::decide::DecideMode;
use vike_model::{ContractValue, NotionalKind, TradeFold};

use super::fills::fill_cash;
use super::lookups::equity_of;
use super::state::OrderBrokerSimulator;

/// The version of [`encode_ledger_events`]' format: what a saved run records beside its log, and the last byte
/// of `LEDGER_EVENTS_MAGIC`.
pub const LEDGER_EVENTS_FORMAT: u32 = 1;

/// The first eight bytes of an encoded log: its name and the format's version ([`LEDGER_EVENTS_FORMAT`]).
const LEDGER_EVENTS_MAGIC: &[u8; 8] = b"vikeelg1";

/// One event that moved the ledger's cash or a position, in the order the engine booked it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LedgerEvent {
    /// The bar step it was booked in (an end-of-run sweep's: the last).
    pub step: u32,
    /// The symbol's index, as `OrderBrokerSimulator::symbols`.
    pub si: u32,
    pub kind: LedgerEventKind,
}

/// What a [`LedgerEvent`] did.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LedgerEventKind {
    /// `side`·`size` at `price` (every cost already in it), `fee` paid.
    Fill { side: i8, size: f64, price: f64, fee: f64 },
    /// Funding charged: cash less `amount`.
    Funding { amount: f64 },
    /// A conversion's new rate: the symbol's value from now on, and the cash it moved.
    Revalue { value: ContractValue, cash: f64 },
}

/// The ledger when the run starts, every position flat: its cash, each symbol's value, and the order equity sums
/// its symbols in (`DecideMode::cross_section_order` of the run's symbols).
#[derive(Debug, Clone, PartialEq)]
pub struct LedgerStart {
    pub cash: f64,
    pub values: Vec<ContractValue>,
    pub cross_section: Vec<usize>,
}

impl LedgerStart {
    /// The start of a run of `symbols` under `decide` with `cash` and each symbol's `values`: the one rule the
    /// engine resolves its summing order by.
    pub fn of(
        cash: f64,
        values: Vec<ContractValue>,
        symbols: &[String],
        decide: DecideMode,
    ) -> Self {
        LedgerStart { cash, values, cross_section: decide.cross_section_order(symbols) }
    }
}

/// The equity after each of `steps` steps, from `start`, `events` (in booked order) and `close(si, step)`,
/// the close each symbol is marked at in that step.
pub fn rebuild_equity_curve(
    start: &LedgerStart,
    events: &[LedgerEvent],
    steps: usize,
    close: impl Fn(usize, usize) -> f64,
) -> Vec<f64> {
    let mut cash = start.cash;
    let mut values = start.values.clone();
    // Only `size` is read; the fold is the engine's own, so a flip or a dust close lands as it did there.
    let mut folds = vec![TradeFold::default(); values.len()];
    let mut next = events.iter().peekable();
    (0..steps)
        .map(|step| {
            while let Some(e) = next.next_if(|e| e.step as usize <= step) {
                let si = e.si as usize;
                match e.kind {
                    LedgerEventKind::Fill { side, size, price, fee } => {
                        let side = i32::from(side);
                        cash = fill_cash(cash, values[si], f64::from(side) * size, price, fee);
                        folds[si].apply_value(side, size, price, fee, 0, values[si]);
                    }
                    LedgerEventKind::Funding { amount } => cash -= amount,
                    LedgerEventKind::Revalue { value, cash: moved } => {
                        cash += moved;
                        values[si] = value;
                    }
                }
            }
            equity_of(cash, &start.cross_section, |si| {
                (values[si], folds[si].size, close(si, step))
            })
        })
        .collect()
}

/// `events` as the bytes this module's doc lays out.
pub fn encode_ledger_events(events: &[LedgerEvent]) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + 34 * events.len());
    out.extend_from_slice(LEDGER_EVENTS_MAGIC);
    out.extend_from_slice(&(events.len() as u64).to_le_bytes());
    let put = |out: &mut Vec<u8>, xs: &[f64]| {
        for x in xs {
            out.extend_from_slice(&x.to_bits().to_le_bytes());
        }
    };
    for e in events {
        let tag = match e.kind {
            LedgerEventKind::Fill { .. } => 0u8,
            LedgerEventKind::Funding { .. } => 1,
            LedgerEventKind::Revalue { .. } => 2,
        };
        out.push(tag);
        out.extend_from_slice(&e.step.to_le_bytes());
        out.extend_from_slice(&e.si.to_le_bytes());
        match e.kind {
            LedgerEventKind::Fill { side, size, price, fee } => {
                out.extend_from_slice(&side.to_le_bytes());
                put(&mut out, &[size, price, fee]);
            }
            LedgerEventKind::Funding { amount } => put(&mut out, &[amount]),
            LedgerEventKind::Revalue { value, cash } => {
                out.push(u8::from(value.kind == NotionalKind::Inverse));
                put(&mut out, &[value.per_unit, cash]);
            }
        }
    }
    out
}

/// [`encode_ledger_events`]' events back, or why `bytes` are not a log: another file, another version, a cut or
/// padded body, an unknown tag.
pub fn decode_ledger_events(bytes: &[u8]) -> Result<Vec<LedgerEvent>, String> {
    let Some((magic, mut rest)) = bytes.split_first_chunk::<8>() else {
        return Err("not an event log (it is shorter than its header)".into());
    };
    if magic != LEDGER_EVENTS_MAGIC {
        return Err("not an event log of this format (its first bytes are not the format's)".into());
    }
    let count = u64::from_le_bytes(take(&mut rest)?);
    let mut events = Vec::new();
    for _ in 0..count {
        let [tag] = take(&mut rest)?;
        let step = u32::from_le_bytes(take(&mut rest)?);
        let si = u32::from_le_bytes(take(&mut rest)?);
        let kind = match tag {
            0 => {
                let side = i8::from_le_bytes(take(&mut rest)?);
                let (size, price, fee) = (bits(&mut rest)?, bits(&mut rest)?, bits(&mut rest)?);
                LedgerEventKind::Fill { side, size, price, fee }
            }
            1 => LedgerEventKind::Funding { amount: bits(&mut rest)? },
            2 => {
                let [inverse] = take(&mut rest)?;
                let (per_unit, cash) = (bits(&mut rest)?, bits(&mut rest)?);
                let value = match inverse {
                    1 => ContractValue::inverse(per_unit),
                    _ => ContractValue::linear(per_unit),
                };
                LedgerEventKind::Revalue { value, cash }
            }
            other => return Err(format!("the event log holds an unknown event tag {other}")),
        };
        events.push(LedgerEvent { step, si, kind });
    }
    if !rest.is_empty() {
        return Err(format!("{} bytes follow the event log's {count} events", rest.len()));
    }
    Ok(events)
}

/// The next `N` bytes of `rest`, moving past them, or the refusal of a log cut short.
fn take<const N: usize>(rest: &mut &[u8]) -> Result<[u8; N], String> {
    let (head, tail) = rest.split_first_chunk::<N>().ok_or("the event log is cut short")?;
    *rest = tail;
    Ok(*head)
}

/// The next `f64` of `rest`, from its little-endian bits.
fn bits(rest: &mut &[u8]) -> Result<f64, String> {
    Ok(f64::from_bits(u64::from_le_bytes(take(rest)?)))
}

impl OrderBrokerSimulator {
    /// Push `kind` on `symbols[si]` at this step, when the log is on.
    #[inline]
    pub(crate) fn log_ledger(&mut self, si: usize, kind: LedgerEventKind) {
        if let Some(log) = &mut self.ledger_events {
            log.push(LedgerEvent { step: self.step as u32, si: si as u32, kind });
        }
    }

    /// Turn the log on (empty) and answer the ledger as it stands: what a caller asks before
    /// `StrategyEngine::run` to rebuild its curve after.
    pub fn start_ledger_log(&mut self) -> LedgerStart {
        self.ledger_events = Some(Vec::new());
        LedgerStart {
            cash: self.cash,
            values: self.value_by_si.clone(),
            cross_section: self.cross_section.clone(),
        }
    }

    /// The test build's check of a bar run: `curve` rebuilt from `start`, the log and the bars, bit for bit, or
    /// a panic naming the first step that differs.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn assert_ledger_rebuilds(&self, start: &LedgerStart, curve: &[f64]) {
        let events = self.ledger_events.as_deref().unwrap_or(&[]);
        let rebuilt =
            rebuild_equity_curve(start, events, curve.len(), |si, step| self.bars[si][step].close);
        if let Some(k) = (0..curve.len()).find(|&k| rebuilt[k].to_bits() != curve[k].to_bits()) {
            let near: Vec<&LedgerEvent> = events.iter().filter(|e| e.step as usize == k).collect();
            panic!(
                "EQUITY-REBUILD differs at step {k} of {}: engine {:?}, rebuilt {:?} ({} events; this step's: {near:?})",
                curve.len(),
                curve[k],
                rebuilt[k],
                events.len()
            );
        }
    }
}
