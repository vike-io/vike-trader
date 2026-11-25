use super::*;
use vike_analytics::results::{DAILY_PERIODS_PER_YEAR, DEFAULT_PERIODS_PER_YEAR};

/// Build a minimal BAR profile at `interval`. Only the two fields `periods_per_year` reads
/// are meaningful; the rest is the smallest thing that parses.
fn bar_profile(interval: &str) -> ProfileBacktest {
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
file = "trend/buy_hold.rs"
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
fn profile_with_engine(extra: &str) -> ProfileBacktest {
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
file = "trend/buy_hold.rs"
"#
    );
    toml::from_str(&toml).expect("fixture profile must parse")
}

/// Parse a profile whose `[strategy.params]` holds `params`.
fn profile_with_params(params: &str) -> ProfileBacktest {
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
[strategy]
file = "trend/buy_hold.rs"
[strategy.params]
{params}
"#
    );
    toml::from_str(&toml).expect("fixture profile must parse")
}

/// The words of one stamp: `(fees, slippage, funding)`.
fn words(s: &RealismStamp) -> (&str, &str, &str) {
    let c = s.charged.as_ref().expect("every stamp this producer writes carries its words");
    (c.fees.as_str(), c.slippage.as_str(), c.funding.as_str())
}

/// ⚠ **THE DEFECT, as a test.** A profile that names no cost and a strategy that states none are charged
/// none (the owner's ruling of 2026-11-18: no venue table, `crate::run::costs`), and the stamp says so
/// in words and in keys, recorded though the file never wrote them.
#[test]
fn a_profile_that_names_no_cost_is_stamped_free() {
    let s = realism_stamp(&profile_with_engine(""));
    assert_eq!(words(&s), ("none", "none", "not applicable"));
    assert_eq!(s.charged_nothing.as_deref(), Some("every fill was free: fees none, slippage none"));
    assert_eq!(s.digest(), "fees: none; slippage: none; funding: not applicable");
    assert_eq!(s.get("strategy.params.commission"), Some("none"));
    assert_eq!(s.get("strategy.params.slippage"), Some("none"));
    assert_eq!(s.get("engine.margin_call"), Some("(absent)"));
    assert_eq!(s.get("engine.whole_entry_fee"), Some("false"));
    let json = serde_json::to_value(&s).unwrap();
    assert_eq!(json["frictionless"], "every fill was free: fees none, slippage none");
    assert_eq!(json["charged"]["fees"], "none");
}

/// A commission param is said in its unit, and whose; a percent with no unit named is the default unit.
#[test]
fn a_commission_param_is_stamped_in_its_unit() {
    for (params, said) in [
        ("commission = 0.05", "0.05% of the fill's value (strategy.params)"),
        ("commission = 1.5\ncommission_unit = \"per_order\"", "1.5 per order (strategy.params)"),
        (
            "commission = 0.005\ncommission_unit = \"per_contract\"",
            "0.005 per contract (strategy.params)",
        ),
        (
            "commission = 0.072\ncommission_unit = \"probability_scaled\"",
            "0.072 x p(1-p) a share, taker (strategy.params)",
        ),
    ] {
        let s = realism_stamp(&profile_with_params(params));
        assert_eq!(words(&s).0, said, "{params}");
        assert_eq!(s.charged_nothing, None, "{params}");
    }
}

/// Any ONE armed cost channel ends the charged-nothing reason, and the slippage term names it.
#[test]
fn any_one_armed_cost_channel_ends_the_charged_nothing_reason() {
    let slipped = realism_stamp(&profile_with_params("slippage = 0.01"));
    assert_eq!(words(&slipped).1, "0.01% of the price a fill (strategy.params)");
    assert_eq!(slipped.charged_nothing, None);
    assert_eq!(slipped.get("strategy.params.slippage"), Some("0.01% of the price a fill"));
    let ticked = realism_stamp(&profile_with_params(
        "slippage = 2\nslippage_unit = \"ticks\"\nslippage_tick = 0.5",
    ));
    assert_eq!(words(&ticked).1, "2 ticks of 0.5 a fill (strategy.params)");
    assert_eq!(ticked.charged_nothing, None);
    // A zero amount charges nothing: the run stays free.
    for none in [
        "slippage = 0",
        "slippage = 0\nslippage_unit = \"ticks\"\nslippage_tick = 0.01",
        "commission = 0\ncommission_unit = \"per_order\"",
    ] {
        let s = realism_stamp(&profile_with_params(none));
        assert_eq!((words(&s).0, words(&s).1), ("none", "none"), "{none}");
        assert!(s.charged_nothing.is_some(), "{none}");
    }
    assert_eq!(realism_stamp(&profile_with_params("commission = 0.04")).charged_nothing, None);
}

/// Funding in words: `not applicable` for a spot pair, `charged` for a perpetual that says `true`, `no
/// funding history` when the RUN reported the store held none, `off` when the profile did not turn it
/// on (absent is off since 2026-11-21) or turned it off.
#[test]
fn the_funding_term_says_what_the_run_did() {
    let perp = |extra: &str| -> ProfileBacktest {
        toml::from_str(&format!(
            "name = \"f\"\n[data]\nkind = \"bar\"\ninterval = \"1h\"\nfrom = \"0\"\nto = \"1\"\n\
             venue = \"bybit\"\nsymbols = [\"BTCUSDT.P\"]\n[engine]\ncash = 1000.0\n{extra}\n\
             [strategy]\nfile = \"trend/buy_hold.rs\"\n"
        ))
        .expect("fixture profile must parse")
    };
    let absent = perp("");
    assert_eq!(words(&realism_stamp(&absent)).2, "off (engine.attach_funding absent)");
    let p = perp("attach_funding = true");
    assert_eq!(words(&realism_stamp(&p)), ("none", "none", "charged"));
    let missing = TradingRecord {
        data_warnings: vec![format!(
            "BTCUSDT.P: a perpetual, and {} for it in this window, so this report charged it NO \
             FUNDING",
            super::super::NO_FUNDING_HISTORY
        )],
        ..TradingRecord::default()
    };
    assert_eq!(words(&realism_stamp_of_run(&p, &missing)).2, "no funding history (BTCUSDT.P)");
    assert_eq!(words(&realism_stamp_of_run(&p, &TradingRecord::default())).2, "charged");
    let off = perp("attach_funding = false");
    assert_eq!(words(&realism_stamp(&off)).2, "off (engine.attach_funding = false)");
}

/// **A run's report is stamped with the profile's name, the run's realism stamp and its strategy
/// file**, whatever name the report came with: the shared unstamped one (`None`) and a named one end
/// the same, and the stamp is the RUN's (its funding term reads the run's warnings).
#[test]
fn a_run_report_takes_the_profiles_name_the_runs_stamp_and_its_strategy_file() {
    let p = bar_profile("1h");
    let run = TradingRecord::default();
    let report = |name: Option<String>| {
        stamp_run_report(&p, &run, TradingResults::from_record(name, &run, periods_per_year(&p)))
    };
    let stamped = report(None);
    assert_eq!(stamped.name.as_deref(), Some("ppy"));
    assert_eq!(stamped.realism, Some(realism_stamp_of_run(&p, &run)));
    assert_eq!(stamped.strategy_file, "trend/buy_hold.rs");
    assert_eq!(
        (stamped.strategy_sha256.as_str(), stamped.strategy_sdk),
        ("", None),
        "no door read it"
    );
    let renamed = report(Some("another".into()));
    assert_eq!(renamed.name, stamped.name, "the profile's name wins");
}

/// ⚠ **A sub-basis-point rate must not render as free.** A fixed `{:.4}` would print a 0.2bp
/// commission as `0.0000`, which is exactly the conflation the stamp exists to prevent.
#[test]
fn a_sub_basis_point_rate_survives_the_rendering() {
    let s = realism_stamp(&profile_with_params("commission = 0.002"));
    assert_eq!(s.get("strategy.params.commission"), Some("0.002% of the fill's value"));
    assert_eq!(s.charged_nothing, None);
}

/// The comparability question the stamp exists for: two runs whose only difference is the commission
/// diverge on exactly that key, by the TOML path an operator would edit.
#[test]
fn two_runs_differing_only_in_fee_diverge_on_exactly_that_key() {
    let free = realism_stamp(&profile_with_engine(""));
    let costed = realism_stamp(&profile_with_params("commission = 0.04"));

    let d = free.divergence(&costed);
    assert_eq!(d.len(), 1, "expected one divergence, got {d:?}");
    assert_eq!(d[0].key, "strategy.params.commission");
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

/// Every `pub` field `ProfileEngine` declares, read out of the source that declares it.
///
/// ⚠ A TEXT SCAN, and deliberately the same one
/// `crates/vike-backtest/tests/engine_cfg_reaches_the_engine.rs`'s `declared_fields` performs for
/// the ENGINE side of this problem: Rust cannot iterate a struct's fields, and a derive macro
/// to make it possible is a dependency this crate will not take for one test. `include_str!`
/// embeds the source at COMPILE time, so this cannot read a stale copy.
///
/// Two residuals, both inherited from that helper. Only `pub ` fields are seen — a private
/// `ProfileEngine` field would still deserialize from TOML and would be invisible here rather than
/// merely unchecked. And the block is delimited by the first column-0 `}` after the
/// declaration rather than by a parse, which is sound only because nothing inside that
/// declaration starts a line with a brace.
fn declared_engine_fields() -> Vec<String> {
    const PROFILE_SOURCE: &str = include_str!("profile/engine_config.rs");
    let start =
        PROFILE_SOURCE.find("pub struct ProfileEngine {").expect("ProfileEngine declaration");
    let body = &PROFILE_SOURCE[start..];
    let end = body.find("\n}").expect("end of ProfileEngine");
    // `skip(1)` drops the declaration line itself, which would otherwise survive the `pub `
    // filter as a bogus field named `struct ProfileEngine {`.
    body[..end]
        .lines()
        .skip(1)
        .filter_map(|l| l.trim().strip_prefix("pub "))
        .filter_map(|l| l.split(':').next())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// ⚠ **Every field `ProfileEngine` declares owes exactly one `engine.<field>` key** — the assertion
/// the first version of this test claimed to make and could not.
///
/// That version pinned `s.values.len()` against a literal `41`, under the doc "a knob added to
/// `ProfileEngine` and not to `realism_stamp` reddens here with the instruction rather than
/// silently going unrecorded". A length pin cannot see that direction: a field added with no
/// `put` leaves the length at 41 and the assert holds. It reddened only in the OPPOSITE case,
/// when somebody had already added the line — a change-detector sold as a completeness gate,
/// which is the class this tree corrects rather than tolerates. Deriving one side is the shape
/// that works here as elsewhere, so the field list comes from [`declared_engine_fields`] and
/// only the KEYS are local.
///
/// Why a BARE profile makes the check total, and why there is deliberately NO exemption table:
/// an absent `[engine.*]` sub-table stamps as `(absent)` rather than being skipped (the
/// argument is at `realism_stamp`'s opt-in sub-tables), so a struct-typed field owes a key
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
    // vacuous and this test green. `ProfileEngine` declared 35 `pub` fields when this was written
    // and fields are only ever added, so a handful means the scan broke rather than the struct
    // shrinking.
    assert!(fields.len() > 30, "the scan found only {} fields — it is broken", fields.len());
    for field in fields {
        let key = format!("engine.{field}");
        assert!(
            s.get(&key).is_some(),
            "`ProfileEngine::{field}` is declared and `realism_stamp` records no {key:?}. Add a \
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
/// stamp is a deliberately chosen SUBSET of `ProfileData` (the cost-relevant keys — the argument is
/// at `realism_stamp`'s `let d: &ProfileData` block), so nothing derives it; and a `put` whose key
/// DUPLICATES another is silently collapsed by `RealismStamp::new`'s `BTreeMap`, which shows up
/// here as a count that fell.
#[test]
fn a_bare_profile_stamps_a_pinned_key_count() {
    let s = realism_stamp(&profile_with_engine(""));
    assert_eq!(
        s.values.len(),
        50,
        "the stamp's key set changed. If you ADDED an `[engine]`/`[data]` line, bump this \
             number; if you removed one, drop it. If the count FELL without a line being removed, \
             two `put` calls now share a key and the map collapsed them.\nkeys: {:?}",
        s.values.keys().collect::<Vec<_>>()
    );
}
