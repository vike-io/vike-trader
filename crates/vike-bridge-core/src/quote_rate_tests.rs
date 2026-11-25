use std::time::Duration;

use super::*;

const HOUR: Duration = Duration::from_secs(3600);

fn memory(reuse: Duration, failed: Duration, remember_no_pair: bool) -> QuoteRateMemory<f64> {
    QuoteRateMemory::new(QuoteRateMemoryConfig { reuse, failed, remember_no_pair })
}

fn unavailable() -> Result<f64, QuoteRateError> {
    Err(QuoteRateError::Unavailable("timed out".into()))
}

/// A rate is handed out inside `reuse` and not after; a failure inside `failed` and not after; the
/// key is the trimmed upper-cased currency; another currency is never answered.
#[test]
fn a_kept_answer_lives_for_its_own_window_only() {
    let kept = memory(HOUR, HOUR, true);
    assert_eq!(kept.answer("JPY"), None, "nothing told yet");
    kept.note(" jpy", &Ok(0.0066));
    assert_eq!(kept.answer("JPY"), Some(Ok(0.0066)));
    assert_eq!(kept.answer("GBP"), None, "another currency asks the venue");
    kept.note("GBP", &unavailable());
    assert_eq!(kept.answer("gbp "), Some(unavailable()));

    let aged = memory(Duration::ZERO, Duration::ZERO, true);
    aged.note("JPY", &Ok(0.0066));
    aged.note("GBP", &unavailable());
    assert_eq!(aged.answer("JPY"), None, "a rate past `reuse` is asked again");
    assert_eq!(aged.answer("GBP"), None, "a failure past `failed` is asked again");
    let (_, last) = aged.last_rate("JPY").expect("whatever its age");
    assert_eq!(last, 0.0066);
}

/// The two windows are separate: a long `failed` with a short `reuse` keeps the failure and not the
/// rate, and a rate read clears the currency's failure at once.
#[test]
fn a_rate_read_clears_the_failure_and_the_windows_are_separate() {
    let kept = memory(Duration::ZERO, HOUR, true);
    kept.note("EUR", &unavailable());
    assert_eq!(kept.answer("EUR"), Some(unavailable()));
    kept.note("EUR", &Ok(1.08));
    assert_eq!(kept.recent_failure("EUR"), None, "an answer clears the failure");
    assert_eq!(kept.answer("EUR"), None, "and a zero `reuse` hands no rate out again");
    kept.note("EUR", &unavailable());
    kept.forget_failure("eur");
    assert_eq!(kept.answer("EUR"), None, "forgotten without a rate");
}

/// `remember_no_pair` decides the verdict alone: kept like any failure when on; when off, not kept
/// and an earlier failure of the currency left as it was.
#[test]
fn a_no_pair_verdict_is_kept_only_when_the_venue_says_so() {
    let no_pair = || Err(QuoteRateError::NoPair("no single pair".into()));
    let remembers = memory(HOUR, HOUR, true);
    remembers.note("SEK", &no_pair());
    assert_eq!(remembers.answer("SEK"), Some(no_pair()));

    let forgets = memory(HOUR, HOUR, false);
    forgets.note("SEK", &no_pair());
    assert_eq!(forgets.answer("SEK"), None, "a verdict is not kept");
    forgets.note("SEK", &unavailable());
    forgets.note("SEK", &no_pair());
    assert_eq!(forgets.answer("SEK"), Some(unavailable()), "nor does it clear a failure");
    let bad = Err(QuoteRateError::BadQuote("crossed".into()));
    forgets.note("SEK", &bad);
    assert_eq!(forgets.answer("SEK"), Some(bad), "every other failure is kept");
}
