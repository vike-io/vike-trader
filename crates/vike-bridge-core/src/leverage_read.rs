//! **The READ half of an account's stated leverage on a venue that POSTS one** (decision 0182):
//! the shape every posting bridge's reader fills in, and the two pure rules over it that the
//! gatewayhub's lane drives, so bybit, okx and binance cannot judge a figure three different ways.
//!
//! ## One writer, and this is not it
//!
//! The venue is WRITTEN by the bridge's exec thread, once, at its start, through its own signed client
//! (`set_leverage`, fed `leverage_for(profile)` — [`crate::leverage`]). The gatewayhub hands that thread
//! the account's figure as the profile's number and then only READS: [`probe`] before the start, to
//! refuse early, and [`confirm`] after it, to see the venue hold exactly that number. [`LeverageReader`]
//! has read methods only, so "the lane never writes" is a compile-time property, not a promise.
//!
//! ## What each read must refuse
//!
//! A reader answers [`LeverageReader::check_account`] `Err` unless the account is in the margin mode
//! the engine models (plain cross margin, one-way positions). Isolated, portfolio and hedge accounts
//! are refused, not modelled. The words of every refusal reach the operator's log and never carry a
//! credential.
//!
//! ## The poll is bounded and never assumes
//!
//! The exec thread posts on its own thread after the start returns, so the first read may still see
//! the old number. [`confirm`] reads on a fixed schedule ([`POLL_WAITS`], about 5 s in all), stops at
//! the first read equal to the figure and otherwise answers `Err`. A read error ends it at once: an
//! unreadable venue confirms nothing. `sleep` is a parameter, so the tests run on a fake clock.

use std::time::Duration;

/// What the venue holds for the account's ONE mounted symbol, as read.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HeldLeverage {
    /// The leverage the venue holds for the symbol right now.
    pub held: f64,
    /// Whether the account holds an open position on the symbol.
    pub position_open: bool,
}

/// A READ-ONLY view of one account's leverage on one symbol of a venue that posts a leverage. It is
/// built for the mounted symbol, so no method takes one. There is no write method, deliberately.
pub trait LeverageReader {
    /// `Ok` exactly when the account is in plain cross margin with one-way positions: the mode the
    /// engine's margin model describes and the bridge's orders (`positionIdx 0`, `posSide net`,
    /// `positionSide BOTH`) assume. Anything else, including an answer this cannot read, is the
    /// refusal in the operator's words.
    ///
    /// # Errors
    /// The account's mode is not plain cross, or the venue's answer could not be read.
    fn check_account(&self) -> Result<(), String>;

    /// The venue's published maximum leverage for the symbol; `Ok(None)` when this venue's reads carry
    /// none (binance: the bracket read is not required for correctness, the poll catches a venue
    /// that rejected the figure).
    ///
    /// # Errors
    /// The read failed.
    fn venue_max(&self) -> Result<Option<f64>, String>;

    /// The leverage the venue holds for the symbol, and whether a position is open.
    ///
    /// # Errors
    /// The read failed, or its shape is one this reader refuses (no row, several rows: hedge mode, a
    /// leverage that is empty or not a figure).
    fn held(&self) -> Result<HeldLeverage, String>;
}

/// Two leverage figures are the same figure.
#[must_use]
pub fn same_leverage(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

/// **Whether the venue may be asked to hold `candidate`**, read-only and PURE over the reader: the
/// account mode is plain cross, `candidate` is within the symbol's published maximum, and no open
/// position would be moved to a different leverage under it.
///
/// A refusal leaves the account exactly as an account that states nothing (the caller hands the bridge
/// the profile it always got).
///
/// # Errors
/// The first reason the figure is refused, in the operator's words.
pub fn probe(reader: &dyn LeverageReader, candidate: f64) -> Result<(), String> {
    reader.check_account()?;
    let held = reader.held()?;
    if let Some(max) = reader.venue_max()?
        && candidate > max
    {
        return Err(format!("{candidate}x is above the venue's maximum of {max}x for this symbol"));
    }
    if held.position_open && !same_leverage(held.held, candidate) {
        return Err(format!(
            "an open position is held at {}x and the figure asked for is {candidate}x: this feature \
             never moves the leverage under a live position",
            held.held
        ));
    }
    Ok(())
}

/// The waits before each read of [`confirm`]'s poll, in order: the first read is immediate, the last
/// ends about five seconds in. Seven reads, none of them inside any venue's request budget.
pub const POLL_WAITS: [Duration; 7] = [
    Duration::ZERO,
    Duration::from_millis(250),
    Duration::from_millis(500),
    Duration::from_millis(750),
    Duration::from_millis(1000),
    Duration::from_millis(1000),
    Duration::from_millis(1500),
];

/// **See the venue hold `candidate`** after the bridge's one write: [`probe`], then a bounded poll of
/// [`LeverageReader::held`] on [`POLL_WAITS`] until a read equals `candidate`. `sleep` is the clock
/// (`std::thread::sleep` in production).
///
/// # Errors
/// [`probe`]'s refusal; a read that failed; or the venue still not holding the figure when the
/// schedule ends (the write did not land: a read-only key, a figure above a risk tier, a rejected
/// request). Never "assume it worked".
pub fn confirm(
    reader: &dyn LeverageReader,
    candidate: f64,
    mut sleep: impl FnMut(Duration),
) -> Result<f64, String> {
    probe(reader, candidate)?;
    let mut last = f64::NAN;
    for wait in POLL_WAITS {
        if !wait.is_zero() {
            sleep(wait);
        }
        let read = reader.held()?;
        if same_leverage(read.held, candidate) {
            return Ok(read.held);
        }
        last = read.held;
    }
    Err(format!(
        "the venue still holds {last}x for it after {} s, not the {candidate}x written at the start \
         (the write did not land: a key without trade permission, or a figure the venue's risk tier \
         refuses)",
        POLL_WAITS.iter().sum::<Duration>().as_secs()
    ))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use super::*;

    /// A reader that answers a script: the account check, the maximum, and a queue of `held` reads
    /// (the last one repeats). Counts every call, so "no read at all" is assertable.
    struct Scripted {
        account: Result<(), String>,
        max: Result<Option<f64>, String>,
        reads: RefCell<VecDeque<Result<HeldLeverage, String>>>,
        calls: RefCell<Vec<&'static str>>,
    }

    impl Scripted {
        fn new(reads: Vec<Result<HeldLeverage, String>>) -> Self {
            Scripted {
                account: Ok(()),
                max: Ok(Some(100.0)),
                reads: RefCell::new(reads.into()),
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    impl LeverageReader for Scripted {
        fn check_account(&self) -> Result<(), String> {
            self.calls.borrow_mut().push("account");
            self.account.clone()
        }
        fn venue_max(&self) -> Result<Option<f64>, String> {
            self.calls.borrow_mut().push("max");
            self.max.clone()
        }
        fn held(&self) -> Result<HeldLeverage, String> {
            self.calls.borrow_mut().push("held");
            let mut reads = self.reads.borrow_mut();
            if reads.len() > 1 { reads.pop_front().expect("one") } else { reads[0].clone() }
        }
    }

    fn flat(held: f64) -> Result<HeldLeverage, String> {
        Ok(HeldLeverage { held, position_open: false })
    }

    fn open(held: f64) -> Result<HeldLeverage, String> {
        Ok(HeldLeverage { held, position_open: true })
    }

    /// Every refusal of [`probe`], each with its reason, and the one `Ok`.
    #[test]
    fn the_probe_refuses_each_unsafe_account_in_words() {
        // Clean: flat, within the maximum.
        assert_eq!(probe(&Scripted::new(vec![flat(2.0)]), 10.0), Ok(()));
        // The account mode is not plain cross.
        let mut isolated = Scripted::new(vec![flat(2.0)]);
        isolated.account = Err("the account is in isolated margin".to_string());
        assert_eq!(probe(&isolated, 10.0), Err("the account is in isolated margin".to_string()));
        assert_eq!(*isolated.calls.borrow(), ["account"], "no further read once the mode refuses");
        // A figure above the symbol's maximum.
        let mut capped = Scripted::new(vec![flat(2.0)]);
        capped.max = Ok(Some(5.0));
        let why = probe(&capped, 10.0).unwrap_err();
        assert!(why.contains("above the venue's maximum of 5x"), "{why}");
        // Exactly the maximum is allowed.
        assert_eq!(probe(&capped, 5.0), Ok(()));
        // An unknown maximum refuses nothing (binance): the poll is the judge there.
        let mut unknown = Scripted::new(vec![flat(2.0)]);
        unknown.max = Ok(None);
        assert_eq!(probe(&unknown, 125.0), Ok(()));
        // An open position at a different leverage.
        let why = probe(&Scripted::new(vec![open(3.0)]), 10.0).unwrap_err();
        assert!(why.contains("open position is held at 3x"), "{why}");
        // …but an open position already at the figure is a no-op, not a refusal.
        assert_eq!(probe(&Scripted::new(vec![open(10.0)]), 10.0), Ok(()));
        // A failed read confirms nothing.
        assert_eq!(
            probe(&Scripted::new(vec![Err("hedge mode: 2 rows".to_string())]), 10.0),
            Err("hedge mode: 2 rows".to_string())
        );
        let mut broken = Scripted::new(vec![flat(2.0)]);
        broken.max = Err("instruments could not be read".to_string());
        assert!(probe(&broken, 10.0).is_err());
    }

    /// The poll on a fake clock: equal at once (no sleep), equal on the third read, never equal, and a
    /// read error.
    #[test]
    fn the_poll_stops_at_the_first_equal_read_and_never_assumes() {
        let slept = RefCell::new(Vec::new());
        let sleep = |d: Duration| slept.borrow_mut().push(d);

        // Equal at once: the probe's read and the poll's first read, and nothing slept.
        let at_once = Scripted::new(vec![flat(10.0)]);
        assert_eq!(confirm(&at_once, 10.0, sleep), Ok(10.0));
        assert!(slept.borrow().is_empty(), "no sleep before the first read");

        // The probe's read and the first two polls still see the old number; the third poll (after two
        // sleeps) sees the write.
        slept.borrow_mut().clear();
        let slow = Scripted::new(vec![flat(2.0), flat(2.0), flat(2.0), flat(10.0)]);
        assert_eq!(confirm(&slow, 10.0, |d| slept.borrow_mut().push(d)), Ok(10.0));
        assert_eq!(
            *slept.borrow(),
            [Duration::from_millis(250), Duration::from_millis(500)],
            "it slept only between reads, and stopped at the first equal one"
        );

        // Never equal: the schedule ends and the answer is an error naming both figures.
        slept.borrow_mut().clear();
        let never = Scripted::new(vec![flat(2.0)]);
        let why = confirm(&never, 10.0, |d| slept.borrow_mut().push(d)).unwrap_err();
        assert!(why.contains("still holds 2x") && why.contains("10x"), "{why}");
        assert_eq!(slept.borrow().iter().sum::<Duration>(), Duration::from_millis(5000));
        let held_reads = never.calls.borrow().iter().filter(|c| **c == "held").count();
        assert_eq!(held_reads, 1 + POLL_WAITS.len(), "the probe's read plus the whole schedule");

        // A read error mid-poll ends it at once.
        slept.borrow_mut().clear();
        let flaky = Scripted::new(vec![flat(2.0), flat(2.0), Err("timed out".to_string())]);
        assert_eq!(confirm(&flaky, 10.0, |d| slept.borrow_mut().push(d)), Err("timed out".into()));
        assert_eq!(slept.borrow().len(), 1, "it did not keep polling a venue it cannot read");

        // The probe's refusal comes before any poll: an open position at another leverage.
        let moved = Scripted::new(vec![open(3.0)]);
        assert!(confirm(&moved, 10.0, |_| panic!("no sleep after a refusal")).is_err());
    }

    #[test]
    fn leverage_figures_compare_within_a_nano() {
        assert!(same_leverage(10.0, 10.0));
        assert!(same_leverage(10.0, 10.0 + 1e-12));
        assert!(!same_leverage(10.0, 10.5));
        assert!(!same_leverage(f64::NAN, 10.0));
    }
}
