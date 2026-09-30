use super::*;
use vike_analytics::report::DAILY_PERIODS_PER_YEAR;

/// Build a minimal BAR profile at `interval`. Only the two fields `periods_per_year` reads
/// are meaningful; the rest is the smallest thing that parses.
fn bar_profile(interval: &str) -> BacktestProfile {
    let toml = format!(
        r#"
name = "ppy"
[data]
kind = "bar"
interval = "{interval}"
from = "0"
to = "1"
venue = "binance"
symbols = ["BTCUSDT"]
[engine]
cash = 1000.0
[strategy]
name = "buy_hold"
"#
    );
    toml::from_str(&toml).expect("fixture profile must parse")
}

/// The anchor does not move: a daily run reports exactly what it always did.
#[test]
fn a_daily_bar_profile_still_returns_the_daily_constant_exactly() {
    assert_eq!(
        periods_per_year(&bar_profile("1d")).to_bits(),
        DAILY_PERIODS_PER_YEAR.to_bits(),
        "the 1d anchor must be bit-identical to its pre-fix value"
    );
}

/// The bug this function exists to prevent: an intraday interval annualized as though daily.
#[test]
fn intraday_intervals_scale_off_the_daily_anchor_instead_of_collapsing_onto_it() {
    // 1440 one-minute bars per day, 24 one-hour bars per day.
    assert_eq!(periods_per_year(&bar_profile("1m")), 252.0 * 1440.0);
    assert_eq!(periods_per_year(&bar_profile("1h")), 252.0 * 24.0);
    assert_eq!(periods_per_year(&bar_profile("5m")), 252.0 * 288.0);

    // The regression itself: 1m must NOT equal the daily constant.
    assert_ne!(
        periods_per_year(&bar_profile("1m")),
        DAILY_PERIODS_PER_YEAR,
        "a 1m run annualized at 252 understates Sharpe by sqrt(1440)"
    );
}

/// Sharpe scales by sqrt(periods_per_year), so this pins the SIZE of the correction the fix
/// applies — the number quoted in the doc comment above.
#[test]
fn the_one_minute_correction_is_sqrt_1440() {
    let ratio = periods_per_year(&bar_profile("1m")) / DAILY_PERIODS_PER_YEAR;
    assert_eq!(ratio, 1440.0);
    assert!(
        (ratio.sqrt() - 37.947).abs() < 0.001,
        "sharpe was understated by ~37.9x, got {}",
        ratio.sqrt()
    );
}

/// Longer-than-daily intervals scale DOWN off the same anchor — the arithmetic is not
/// intraday-only.
#[test]
fn a_weekly_interval_scales_down_off_the_same_anchor() {
    assert_eq!(periods_per_year(&bar_profile("7d")), 252.0 / 7.0);
}

/// An unparseable interval must fall back, never fabricate a scale from a bad parse.
#[test]
fn an_unparseable_interval_falls_back_rather_than_fabricating_a_scale() {
    for bad in ["", "m", "1x", "xm", "-1m"] {
        assert_eq!(
            periods_per_year(&bar_profile(bad)),
            DEFAULT_PERIODS_PER_YEAR,
            "interval {bad:?} must fall back"
        );
    }
}

// --- the realism stamp ---------------------------------------------------------------------

/// Parse a profile from a body appended under `[engine]`.
fn profile_with_engine(extra: &str) -> BacktestProfile {
    let toml = format!(
        r#"
name = "realism"
[data]
kind = "bar"
interval = "1d"
from = "0"
to = "1"
venue = "binance"
symbols = ["BTCUSDT"]
[engine]
cash = 1000.0
{extra}
[strategy]
name = "buy_hold"
"#
    );
    toml::from_str(&toml).expect("fixture profile must parse")
}

/// ⚠ **THE DEFECT, as a test.** A profile that mentions neither `fee_rate` nor `slippage` is
/// describing a market where trading is free, and the resolved numbers say so — which is what a
/// report had no way to tell anybody.
#[test]
fn a_profile_that_mentions_no_cost_at_all_stamps_as_frictionless() {
    let s = realism_stamp(&profile_with_engine(""));
    let why = s.frictionless.as_deref().expect("a costless profile must be flagged");
    // The reason names all four channels, because "frictionless" alone leaves the operator
    // guessing which knob they forgot.
    for needle in ["fee_rate", "[engine.fee]", "slippage", "[engine.impact]"] {
        assert!(why.contains(needle), "the reason must name {needle}: {why}");
    }
    // ...and the values are RECORDED even though the file never wrote them. This is the half
    // "record the resolved configuration" buys over "record the file".
    assert_eq!(s.get("engine.fee_rate"), Some("0"));
    assert_eq!(s.get("engine.slippage"), Some("0"));
    assert_eq!(s.get("engine.fee"), Some("(absent)"));
    assert_eq!(s.get("engine.impact"), Some("(absent)"));
}

/// Any ONE armed cost channel ends the verdict — four independent channels, and the run has
/// priced something as soon as one of them is on.
#[test]
fn any_one_armed_cost_channel_ends_the_frictionless_verdict() {
    assert!(realism_stamp(&profile_with_engine("fee_rate = 0.0004")).frictionless.is_none());
    assert!(realism_stamp(&profile_with_engine("slippage = 0.0001")).frictionless.is_none());
    assert!(
        realism_stamp(&profile_with_engine("[engine.fee]\nkind = \"flat\"")).frictionless.is_none(),
        "a fee SCHEDULE supersedes the flat rate and is a cost channel of its own"
    );
    assert!(
        realism_stamp(&profile_with_engine("[engine.impact]\nmodel = \"linear\""))
            .frictionless
            .is_none()
    );
}

/// ⚠ **A sub-basis-point rate must not render as free.** A fixed `{:.4}` would print a 0.2bp
/// maker rebate as `0.0000`, which is exactly the conflation the stamp exists to prevent.
#[test]
fn a_sub_basis_point_rate_survives_the_rendering() {
    let s = realism_stamp(&profile_with_engine("fee_rate = 0.00002"));
    assert_eq!(s.get("engine.fee_rate"), Some("0.00002"));
    assert!(s.frictionless.is_none());
}

/// The comparability question the stamp exists for: two runs whose only difference is the fee
/// diverge on exactly that key, by the TOML path an operator would edit.
#[test]
fn two_runs_differing_only_in_fee_diverge_on_exactly_that_key() {
    let free = realism_stamp(&profile_with_engine(""));
    let costed = realism_stamp(&profile_with_engine("fee_rate = 0.0004"));

    let d = free.divergence(&costed);
    assert_eq!(d.len(), 1, "expected one divergence, got {d:?}");
    assert_eq!(d[0].key, "engine.fee_rate");
}

/// An absent optional records as `-`, never as `0`. `RealismStamp` compares values as strings,
/// so a zero would make "this knob was off" and "this knob was set to zero" one answer.
#[test]
fn an_absent_optional_records_as_a_dash_rather_than_a_zero() {
    let s = realism_stamp(&profile_with_engine(""));
    assert_eq!(s.get("engine.volume_limit"), Some("-"));
    assert_eq!(s.get("engine.leverage"), Some("-"));
    assert_eq!(s.get("data.warmup"), Some("-"));
    assert_eq!(s.get("data.detail_interval"), Some("-"));

    let set = realism_stamp(&profile_with_engine("volume_limit = 0.0"));
    assert_eq!(
        set.get("engine.volume_limit"),
        Some("0"),
        "a volume_limit of ZERO is a real (and drastic) setting and must not read as unset"
    );
}

/// Batch A's two `[data]` keys and the resolved universe are on the stamp: a run priced
/// identically over a different tape, or with a different warm-up, is not a comparable result.
#[test]
fn the_data_keys_that_change_the_fills_are_on_the_stamp() {
    let s = realism_stamp(&profile_with_engine(""));
    assert_eq!(s.get("data.kind"), Some("bar"));
    assert_eq!(s.get("data.interval"), Some("1d"));
    assert_eq!(s.get("data.series"), Some("binance:BTCUSDT"));
}

/// The whole point of `[engine.fee]`'s ten-key surface: an ARMED schedule records every one of
/// its resolved keys, so two runs on the same `kind` but different rates are still told apart.
#[test]
fn an_armed_fee_schedule_records_its_whole_resolved_surface() {
    let s = realism_stamp(&profile_with_engine(
        "[engine.fee]\nkind = \"flat\"\ntaker_rate = 0.0004\nmaker_rate = 0.0002",
    ));
    assert_eq!(s.get("engine.fee.kind"), Some("flat"));
    assert_eq!(s.get("engine.fee.taker_rate"), Some("0.0004"));
    assert_eq!(s.get("engine.fee.maker_rate"), Some("0.0002"));
    // The keys nobody wrote are recorded at their resolved defaults, which is the property
    // that makes a diff between two schedules complete.
    assert_eq!(s.get("engine.fee.maker_rebate_share"), Some("0"));
    assert_eq!(s.get("engine.fee.per_share"), Some("-"));
    assert_eq!(s.get("engine.fee.venue"), Some("-"));
    // ...and the table-absent sentinel is gone, so a diff against a no-schedule run reports
    // the schedule rather than agreeing with it.
    assert_eq!(s.get("engine.fee"), None);
}

/// Every `pub` field `EngineCfg` declares, read out of the source that declares it.
///
/// ⚠ A TEXT SCAN, and deliberately the same one
/// `crates/vike-ops/tests/engine_cfg_reaches_the_engine.rs`'s `declared_fields` performs for
/// the ENGINE side of this problem: Rust cannot iterate a struct's fields, and a derive macro
/// to make it possible is a dependency this crate will not take for one test. `include_str!`
/// embeds the source at COMPILE time, so this cannot read a stale copy, and
/// `crate::profile_surface` already reads the same file the same way for the schema export.
///
/// Two residuals, both inherited from that helper. Only `pub ` fields are seen — a private
/// `EngineCfg` field would still deserialize from TOML and would be invisible here rather than
/// merely unchecked. And the block is delimited by the first column-0 `}` after the
/// declaration rather than by a parse, which is sound only because nothing inside that
/// declaration starts a line with a brace.
fn declared_engine_fields() -> Vec<String> {
    const PROFILE_SRC: &str = include_str!("profile.rs");
    let start = PROFILE_SRC.find("pub struct EngineCfg {").expect("EngineCfg declaration");
    let body = &PROFILE_SRC[start..];
    let end = body.find("\n}").expect("end of EngineCfg");
    // `skip(1)` drops the declaration line itself, which would otherwise survive the `pub `
    // filter as a bogus field named `struct EngineCfg {`.
    body[..end]
        .lines()
        .skip(1)
        .filter_map(|l| l.trim().strip_prefix("pub "))
        .filter_map(|l| l.split(':').next())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// ⚠ **Every field `EngineCfg` declares owes exactly one `engine.<field>` key** — the assertion
/// the first version of this test claimed to make and could not.
///
/// That version pinned `s.values.len()` against a literal `41`, under the doc "a knob added to
/// `EngineCfg` and not to `realism_stamp` reddens here with the instruction rather than
/// silently going unrecorded". A length pin cannot see that direction: a field added with no
/// `put` leaves the length at 41 and the assert holds. It reddened only in the OPPOSITE case,
/// when somebody had already added the line — a change-detector sold as a completeness gate,
/// which is the class this tree corrects rather than tolerates. Deriving one side is the shape
/// that works here as elsewhere, so the field list comes from [`declared_engine_fields`] and
/// only the KEYS are local.
///
/// Why a BARE profile makes the check total, and why there is deliberately NO exemption table:
/// an absent `[engine.*]` sub-table stamps as `(absent)` rather than being skipped (the
/// argument is at `realism_stamp`'s `match &e.fee` arm), so a struct-typed field owes a key
/// here exactly as a scalar one does. A field this test cannot find is therefore a cost knob no
/// operator can compare — the defect, not an exception to it.
///
/// ⚠ It found one the moment it was written. `engine.decide` was declared, resolved, implied
/// `cash_gate` and disabled the granular sub-bar lane, and reached no stamp.
#[test]
fn a_bare_profile_stamps_every_declared_engine_field() {
    let s = realism_stamp(&profile_with_engine(""));
    let fields = declared_engine_fields();
    // A FLOOR, not a pin: a scan that silently returned nothing would make the loop below
    // vacuous and this test green. `EngineCfg` declared 35 `pub` fields when this was written
    // and fields are only ever added, so a handful means the scan broke rather than the struct
    // shrinking.
    assert!(fields.len() > 30, "the scan found only {} fields — it is broken", fields.len());
    for field in fields {
        let key = format!("engine.{field}");
        assert!(
            s.get(&key).is_some(),
            "`EngineCfg::{field}` is declared and `realism_stamp` records no {key:?}. Add a \
                 `put({key:?}, …)` line beside the knobs it belongs with; if it genuinely prices \
                 nothing, argue that at `realism_stamp`'s \"What it deliberately does NOT record\" \
                 section first. A knob missing from the stamp is a cost model nobody can \
                 compare.\nkeys: {:?}",
            s.values.keys().collect::<Vec<_>>()
        );
    }
}

/// ⚠ The key COUNT, kept as a change-detector and no longer sold as anything more. Two things
/// it still earns, neither of which the derived test above can see: the `data.*` half of the
/// stamp is a deliberately chosen SUBSET of `DataCfg` (the cost-relevant keys — the argument is
/// at `realism_stamp`'s `let d: &DataCfg` block), so nothing derives it; and a `put` whose key
/// DUPLICATES another is silently collapsed by `RealismStamp::new`'s `BTreeMap`, which shows up
/// here as a count that fell.
#[test]
fn a_bare_profile_stamps_a_pinned_key_count() {
    let s = realism_stamp(&profile_with_engine(""));
    assert_eq!(
        s.values.len(),
        42,
        "the stamp's key set changed. If you ADDED an `[engine]`/`[data]` line, bump this \
             number; if you removed one, drop it. If the count FELL without a line being removed, \
             two `put` calls now share a key and the map collapsed them.\nkeys: {:?}",
        s.values.keys().collect::<Vec<_>>()
    );
}
