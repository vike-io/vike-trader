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
//! ## A symbol admitted on demand
//!
//! The start-time write covers the MOUNTED symbol only; the exec thread's foreign-symbol clients
//! deliberately post no leverage. For a symbol admitted later the lane drives [`settle`]: the same
//! [`probe_held`] refusals, no write when the venue already holds the figure, otherwise ONE write
//! through the bridge's own verb (`VenueAdapter::set_symbol_leverage`, the bridge's `set_leverage`
//! and nothing else) and the same bounded poll. The lane still has no write method of its own. The verb
//! says HOW a failure failed ([`LeverageWriteError`]): a refusal ends the ask, a write whose outcome is unknown
//! (a timeout, a 5xx) is read back like a clean one, so a POST that landed is not reported as refused.
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

/// What the venue holds for the account's ONE symbol (the reader's), as read.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HeldLeverage {
    /// The leverage the venue holds for the symbol right now.
    pub held: f64,
    /// Whether the account holds an open position on the symbol.
    pub position_open: bool,
}

/// A READ-ONLY view of one account's leverage on one symbol of a venue that posts a leverage. It is
/// built for that one symbol (the mounted one for a start, the admitted one for [`settle`]), so no
/// method takes one. There is no write method, deliberately.
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

/// **How the bridge's per-symbol set-leverage POST failed** (`VenueAdapter::set_symbol_leverage`): the one fact
/// the lane needs beyond the words, because the two kinds call for different answers.
///
/// * [`Self::Refused`]: nothing changed on the venue (the venue said no, or no request left: no key, a tier the
///   verb is not open at, a spot symbol). [`settle`] reports it at once and polls nothing.
/// * [`Self::Ambiguous`]: a timeout, a 5xx or an unreadable answer AFTER the request may have reached the venue
///   (the transport's `E_TIMEOUT_AMBIGUOUS` class), so the figure MAY be held. [`settle`] does not report it as
///   refused: it reads the venue back, as it does after a clean write, and only a venue that still does not hold
///   the figure is an error (worded as unknown, not as refused).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeverageWriteError {
    /// The write did not happen, in the venue's (or the bridge's) words.
    Refused(String),
    /// The write MAY have happened; the words say what the wire did.
    Ambiguous(String),
}

impl LeverageWriteError {
    /// A failed venue call as the kind it is: `ambiguous` is the bridge's own "this failure leaves the outcome
    /// unknown" rule over the venue error's code (`error_codes::submit_outcome_unknown`).
    #[must_use]
    pub fn of_venue_call(ambiguous: bool, words: String) -> Self {
        if ambiguous { Self::Ambiguous(words) } else { Self::Refused(words) }
    }
}

impl std::fmt::Display for LeverageWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(words) | Self::Ambiguous(words) => f.write_str(words),
        }
    }
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
    probe_held(reader, candidate).map(|_| ())
}

/// [`probe`], keeping what it read: the leverage the venue holds for the symbol, which a caller that
/// writes ([`settle`]) needs to know whether there is anything to write.
///
/// # Errors
/// As [`probe`].
pub fn probe_held(reader: &dyn LeverageReader, candidate: f64) -> Result<HeldLeverage, String> {
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
    Ok(held)
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
    sleep: impl FnMut(Duration),
) -> Result<f64, String> {
    probe(reader, candidate)?;
    poll_held(reader, candidate, "at the start", sleep)
}

/// The bounded poll of [`confirm`] and [`settle`]: read [`LeverageReader::held`] on [`POLL_WAITS`]
/// until a read equals `candidate`. `written` says when the write happened, for the failure's words.
fn poll_held(
    reader: &dyn LeverageReader,
    candidate: f64,
    written: &str,
    mut sleep: impl FnMut(Duration),
) -> Result<f64, String> {
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
        "the venue still holds {last}x for it after {} s, not the {candidate}x written {written} \
         (the write did not land: a key without trade permission, or a figure the venue's risk tier \
         refuses)",
        POLL_WAITS.iter().sum::<Duration>().as_secs()
    ))
}

/// **Make one symbol hold `candidate`, and see it hold it** — the per-symbol twin of the start-time
/// hand-over, for a symbol admitted on demand (the exec thread's start-time write covers the mounted
/// symbol only).
///
/// [`probe_held`] first (the same refusals as a start, read-only). A venue SEEN holding `candidate`
/// already is the whole answer and nothing is written, so a repeated admission of one symbol costs
/// reads only. Otherwise `write` runs ONCE (the bridge's own set-leverage, never this module's) and the
/// venue is polled until it holds the figure ([`POLL_WAITS`]): a write that cannot be seen is an `Err`,
/// never "assumed". No clamp: a figure above the symbol's maximum was refused by the probe.
///
/// # Errors
/// [`probe_held`]'s refusal; the write's own refusal ([`LeverageWriteError::Refused`]: nothing was seen to
/// change); a read that failed; or the venue still not holding the figure when the schedule ends. A write
/// whose outcome is unknown ([`LeverageWriteError::Ambiguous`]) is polled like a clean one and is an error
/// only if the venue is then not seen holding the figure, in words that say the outcome was unknown.
pub fn settle(
    reader: &dyn LeverageReader,
    candidate: f64,
    write: impl FnOnce() -> Result<(), LeverageWriteError>,
    sleep: impl FnMut(Duration),
) -> Result<f64, String> {
    let held = probe_held(reader, candidate)?;
    if same_leverage(held.held, candidate) {
        return Ok(held.held);
    }
    match write() {
        Ok(()) => poll_held(reader, candidate, "just now", sleep),
        Err(LeverageWriteError::Refused(why)) => Err(format!("the venue was not written: {why}")),
        Err(LeverageWriteError::Ambiguous(why)) => poll_held(reader, candidate, "just now", sleep)
            .map_err(|seen| {
                format!("the write's outcome is unknown ({why}) and the venue is not seen holding it: {seen}")
            }),
    }
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

    /// `settle`: a symbol already holding the figure is never written; any other is written ONCE and then
    /// SEEN holding it; a write that fails or never lands is an error naming both numbers.
    #[test]
    fn settle_writes_once_and_only_what_it_then_sees() {
        let writes = RefCell::new(0u32);
        let write_ok = || {
            *writes.borrow_mut() += 1;
            Ok(())
        };
        let slept = RefCell::new(Vec::new());

        // Already holding the figure: the probe's own read is the evidence. No write, no sleep, no poll.
        let holding = Scripted::new(vec![flat(10.0)]);
        assert_eq!(settle(&holding, 10.0, write_ok, |d| slept.borrow_mut().push(d)), Ok(10.0));
        assert_eq!(*writes.borrow(), 0, "a repeated admission writes nothing");
        assert!(slept.borrow().is_empty());
        assert_eq!(
            *holding.calls.borrow(),
            ["account", "held", "max"],
            "reads only, one held read"
        );

        // Another leverage and no position: ONE write, then the first poll read sees it.
        let other = Scripted::new(vec![flat(2.0), flat(10.0)]);
        assert_eq!(settle(&other, 10.0, write_ok, |d| slept.borrow_mut().push(d)), Ok(10.0));
        assert_eq!(*writes.borrow(), 1);
        assert!(slept.borrow().is_empty(), "the first poll read is immediate");

        // The write lands on the third poll read (two sleeps).
        let slow = Scripted::new(vec![flat(2.0), flat(2.0), flat(2.0), flat(10.0)]);
        assert_eq!(settle(&slow, 10.0, write_ok, |d| slept.borrow_mut().push(d)), Ok(10.0));
        assert_eq!(*writes.borrow(), 2);
        assert_eq!(*slept.borrow(), [Duration::from_millis(250), Duration::from_millis(500)]);

        // The write is accepted but the venue never holds it: an error naming both figures, one write.
        slept.borrow_mut().clear();
        let never = Scripted::new(vec![flat(2.0)]);
        let why = settle(&never, 10.0, write_ok, |d| slept.borrow_mut().push(d)).unwrap_err();
        assert!(why.contains("still holds 2x") && why.contains("10x"), "{why}");
        assert!(why.contains("just now") && !why.contains("at the start"), "{why}");
        assert_eq!(*writes.borrow(), 3, "one write, never a retry");
        assert_eq!(slept.borrow().iter().sum::<Duration>(), Duration::from_millis(5000));

        // The write itself fails: nothing is polled, the venue's words are kept.
        let fails = Scripted::new(vec![flat(2.0)]);
        let why = settle(
            &fails,
            10.0,
            || Err(LeverageWriteError::Refused("110017 key has no trade permission".into())),
            |_| panic!("no poll after a failed write"),
        )
        .unwrap_err();
        assert_eq!(why, "the venue was not written: 110017 key has no trade permission");
        assert_eq!(fails.calls.borrow().iter().filter(|c| **c == "held").count(), 1);
    }

    /// **A write whose outcome is unknown (a timeout, a 5xx) is read back, never reported as refused**: a POST
    /// that landed is a success the venue is SEEN holding; one that did not is an error that says the outcome
    /// was unknown, after ONE write and the whole schedule; and the kind is the bridge's to decide.
    #[test]
    fn an_ambiguous_write_is_read_back_and_never_called_refused() {
        let writes = RefCell::new(0u32);
        let timed_out = || {
            *writes.borrow_mut() += 1;
            Err(LeverageWriteError::Ambiguous("POST timed out".into()))
        };
        let slept = RefCell::new(Vec::new());

        // The POST timed out but had landed: the first poll read sees it, so the answer is the figure.
        let landed = Scripted::new(vec![flat(2.0), flat(10.0)]);
        assert_eq!(settle(&landed, 10.0, timed_out, |d| slept.borrow_mut().push(d)), Ok(10.0));
        assert_eq!(*writes.borrow(), 1, "one write, no retry");
        assert!(slept.borrow().is_empty(), "the first poll read is immediate");

        // It had not: the schedule runs out and the error says the outcome was unknown (not "refused").
        let never = Scripted::new(vec![flat(2.0)]);
        let why = settle(&never, 10.0, timed_out, |d| slept.borrow_mut().push(d)).unwrap_err();
        assert!(why.contains("outcome is unknown") && why.contains("POST timed out"), "{why}");
        assert!(why.contains("still holds 2x") && why.contains("10x"), "{why}");
        assert!(!why.contains("was not written"), "an unknown outcome is not a refusal: {why}");
        assert_eq!(*writes.borrow(), 2, "one more write, never a retry inside");
        assert_eq!(slept.borrow().iter().sum::<Duration>(), Duration::from_millis(5000));

        // The two kinds read the same in Display, and the constructor files a call by the bridge's rule.
        assert_eq!(LeverageWriteError::of_venue_call(true, "x".into()).to_string(), "x");
        assert_eq!(
            LeverageWriteError::of_venue_call(true, "t".into()),
            LeverageWriteError::Ambiguous("t".into())
        );
        assert_eq!(
            LeverageWriteError::of_venue_call(false, "r".into()),
            LeverageWriteError::Refused("r".into())
        );
    }

    /// `settle` writes nothing for an account the probe refuses, whatever the refusal.
    #[test]
    fn settle_never_writes_for_a_refused_probe() {
        let never_write = || panic!("a refused probe must not write");
        let no_sleep = |_| panic!("a refused probe must not poll");

        let mut isolated = Scripted::new(vec![flat(2.0)]);
        isolated.account = Err("the account is in isolated margin".to_string());
        assert!(settle(&isolated, 10.0, never_write, no_sleep).is_err());

        let mut capped = Scripted::new(vec![flat(2.0)]);
        capped.max = Ok(Some(5.0));
        let why = settle(&capped, 10.0, never_write, no_sleep).unwrap_err();
        assert!(why.contains("above the venue's maximum of 5x"), "{why}");

        let why = settle(&Scripted::new(vec![open(3.0)]), 10.0, never_write, no_sleep).unwrap_err();
        assert!(why.contains("open position is held at 3x"), "{why}");

        let hedge = Scripted::new(vec![Err("hedge mode: 2 rows".to_string())]);
        assert_eq!(
            settle(&hedge, 10.0, never_write, no_sleep),
            Err("hedge mode: 2 rows".to_string())
        );

        // An open position ALREADY at the figure is a no-op, not a refusal and not a write.
        assert_eq!(settle(&Scripted::new(vec![open(10.0)]), 10.0, never_write, no_sleep), Ok(10.0));
    }

    #[test]
    fn leverage_figures_compare_within_a_nano() {
        assert!(same_leverage(10.0, 10.0));
        assert!(same_leverage(10.0, 10.0 + 1e-12));
        assert!(!same_leverage(10.0, 10.5));
        assert!(!same_leverage(f64::NAN, 10.0));
    }
}
