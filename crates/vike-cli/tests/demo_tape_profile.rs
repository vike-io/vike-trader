//! The shipped example profile and the demo tape must describe THE SAME SLICE.
//!
//! # Why this file exists
//!
//! `vike-cli init` writes `user_data/profiles/backtest.toml`, and `backtest --seed-demo` writes
//! `vike_data::demo`'s tape. They are the two halves of one promise — "a fresh install can run a
//! backtest" — and they live in different crates, with the slice spelled as a UTC hour label in one
//! and as epoch milliseconds in the other. Nothing but this file makes them agree.
//!
//! The failure they are held against is not a crash. A profile naming a slice the store does not
//! hold produces a run that COMPLETES and reports no trades, which is indistinguishable from a
//! strategy that never fired — the exact confusion the old profile's `⚠ Change this to a slice your
//! store actually holds` comment admitted and could not fix. So the drift this catches is silent by
//! construction, which is what earns it a gate rather than a comment.
//!
//! ⚠ It converts the hour labels with `vike_model::time`, the SAME functions
//! `vike_backtest::harness::profile`'s `parse_ts` uses. A second date implementation here would
//! agree with the profile loader only by luck, and would fail in the one direction that matters:
//! passing while the real loader reads a different instant.

use vike_cli::cmd::init::content::PROFILE_BACKTEST;
use vike_data::demo::{DEMO_SLICES, DEMO_VENUE};

/// Epoch milliseconds for a `YYYY-MM-DDTHH` label, through the profile loader's own conversion.
fn label_to_ms(label: &str) -> i64 {
    let (y, m, d, h) = vike_model::time::parse_hour_label(label)
        .unwrap_or_else(|| panic!("the shipped profile carries {label:?}, which is not a YYYY-MM-DDTHH hour label — the profile loader would refuse it"));
    vike_model::time::days_from_civil(y, m, d) * 86_400_000 + i64::from(h) * 3_600_000
}

/// The profile's `[data]` table, as the four fields this gate compares.
struct ProfileSlice {
    venue: String,
    symbols: Vec<String>,
    interval: String,
    from_ms: i64,
    to_ms: i64,
}

fn shipped_profile() -> ProfileSlice {
    let doc: toml::Value =
        toml::from_str(PROFILE_BACKTEST).expect("the shipped profile must be valid TOML");
    let data = doc.get("data").expect("the shipped profile must carry a [data] table");
    let s = |k: &str| {
        data.get(k)
            .and_then(toml::Value::as_str)
            .unwrap_or_else(|| panic!("[data].{k} is missing or not a string"))
            .to_string()
    };
    ProfileSlice {
        venue: s("venue"),
        symbols: data
            .get("symbols")
            .and_then(toml::Value::as_array)
            .expect("[data].symbols must be an array")
            .iter()
            .map(|v| v.as_str().expect("a symbol must be a string").to_string())
            .collect(),
        interval: s("interval"),
        from_ms: label_to_ms(&s("from")),
        to_ms: label_to_ms(&s("to")),
    }
}

/// THE gate. Every field the profile names must be one the demo tape actually wrote.
#[test]
fn the_shipped_profile_names_a_slice_the_demo_tape_holds() {
    let p = shipped_profile();

    assert_eq!(
        p.venue, DEMO_VENUE,
        "the shipped profile's venue is {:?} but the demo tape is written under {DEMO_VENUE:?}. \
         `backtest --seed-demo` then leaves that profile with an empty slice, which runs to \
         completion and reports no trades — the failure this gate exists to make loud.",
        p.venue
    );

    let matching: Vec<_> = DEMO_SLICES.iter().filter(|s| s.interval == p.interval).collect();
    assert!(
        !matching.is_empty(),
        "the shipped profile asks for interval {:?}; the demo tape holds only {:?}",
        p.interval,
        DEMO_SLICES.iter().map(|s| s.interval).collect::<Vec<_>>()
    );

    for symbol in &p.symbols {
        let slice = matching.iter().find(|s| s.symbol == symbol).unwrap_or_else(|| {
            panic!(
                "the shipped profile asks for {symbol}/{}, which the demo tape does not hold",
                p.interval
            )
        });
        assert!(
            p.from_ms >= slice.from_ms && p.to_ms <= slice.to_ms,
            "the shipped profile's window [{}, {}) is not inside the demo tape's [{}, {}) for \
             {symbol}/{}. A profile reaching past the tape is not an error at run time — the \
             uncovered part is simply absent from the result.",
            p.from_ms,
            p.to_ms,
            slice.from_ms,
            slice.to_ms,
            p.interval
        );
    }
}

/// The window must be big enough to be worth running. A profile technically inside the tape but
/// spanning three bars would pass the gate above and still hand a new user an empty-looking report,
/// because a moving-average strategy needs a warm-up before it trades at all.
#[test]
fn the_shipped_window_is_long_enough_for_a_strategy_to_warm_up_and_trade() {
    let p = shipped_profile();
    let slice =
        DEMO_SLICES.iter().find(|s| s.interval == p.interval).expect("checked by the gate above");
    let bars = (p.to_ms - p.from_ms) / slice.step_ms;
    // The shipped `sma_cross` params are fast=10 / slow=30, so 30 bars is pure warm-up. 500 is an
    // arbitrary floor with the right shape: an order of magnitude past the slowest shipped window.
    assert!(
        bars >= 500,
        "the shipped profile covers only {bars} bars of {}; the shipped sma_cross needs 30 for \
         warm-up alone, so a report from this window would look empty for a reason that has \
         nothing to do with the strategy",
        p.interval
    );
}

/// The profile must SAY the tape is synthetic. This is the one assertion here that is about
/// honesty rather than mechanism: the whole reason the tape is written under its own venue is that
/// a backtest result computed on invented prices is indistinguishable from a real one once it
/// leaves the terminal, and the file the user reads is where that has to be stated.
#[test]
fn the_shipped_profile_says_the_demo_tape_is_not_market_data() {
    let text = PROFILE_BACKTEST;
    assert!(
        text.contains("NOT market data"),
        "the shipped profile no longer states that the demo tape is synthetic. A user who takes \
         it for real data draws conclusions from a closed-form curve."
    );
    assert!(
        text.contains("--seed-demo"),
        "the shipped profile no longer names the command that fills the slice it asks for, so a \
         reader who hits an empty run has nothing to follow."
    );
}
