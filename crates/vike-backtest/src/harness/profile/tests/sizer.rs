//! The `[engine.sizer]` table: every sizer kind, nested bases, and the depth limit.

use super::*;

// --- Task 12: [engine.sizer] ---------------------------------------------------------------

#[test]
fn no_sizer_is_the_default() {
    let p = BacktestProfile::from_toml_str(BAR_TOML).expect("parses");
    assert!(p.engine.sizer.is_none());
}

#[test]
fn a_sizer_reaches_the_profile_and_builds() {
    let toml = BAR_TOML
        .replace("fee_rate = 0.001", "fee_rate = 0.001\n\n[engine.sizer]\nkind = \"pass_through\"");
    let p = BacktestProfile::from_toml_str(&toml).expect("parses");
    let cfg = p.engine.sizer.as_ref().expect("sizer present");
    assert!(cfg.build().is_ok());
}

#[test]
fn an_unknown_sizer_kind_is_rejected() {
    let toml = BAR_TOML
        .replace("fee_rate = 0.001", "fee_rate = 0.001\n\n[engine.sizer]\nkind = \"telepathy\"");
    match BacktestProfile::from_toml_str(&toml) {
        Err(HarnessError::Validation(m)) => assert!(m.contains("telepathy"), "{m}"),
        Ok(p) => match p.engine.sizer.as_ref().expect("present").build() {
            Err(HarnessError::Validation(m)) => assert!(m.contains("telepathy"), "{m}"),
            Err(other) => panic!("expected a validation error, got {other:?}"),
            // `Box<dyn PositionSizer>` doesn't implement `Debug`, so this arm can't be
            // printed — the assertion itself is the diagnostic.
            Ok(_) => panic!("expected build() to reject an unknown kind"),
        },
        Err(other) => panic!("expected a validation error, got {other:?}"),
    }
}

/// Every scalar (non-wrapping) sizer kind builds given its own required knob.
#[test]
fn every_scalar_sizer_kind_builds() {
    for (kind, extra) in [
        ("fixed_dollar", "amount = 1000.0"),
        ("fixed_shares", "shares = 10.0"),
        ("pct_equity", "pct = 0.1"),
        ("pct_volatility", "pct = 0.1"),
        ("max_risk_pct", "pct = 0.02"),
    ] {
        let toml = BAR_TOML.replace(
            "fee_rate = 0.001",
            &format!("fee_rate = 0.001\n\n[engine.sizer]\nkind = \"{kind}\"\n{extra}"),
        );
        let p = BacktestProfile::from_toml_str(&toml)
            .unwrap_or_else(|e| panic!("{kind} should parse: {e:?}"));
        p.engine
            .sizer
            .as_ref()
            .unwrap()
            .build()
            .unwrap_or_else(|e| panic!("{kind} should build: {e:?}"));
    }
}

#[test]
fn a_scalar_sizer_kind_missing_its_knob_is_rejected() {
    let toml = BAR_TOML
        .replace("fee_rate = 0.001", "fee_rate = 0.001\n\n[engine.sizer]\nkind = \"fixed_dollar\"");
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => assert!(m.contains("engine.sizer.amount"), "{m}"),
        other => panic!("expected a validation error, got {other:?}"),
    }
}

#[test]
fn a_sizer_portfolio_heat_missing_its_base_is_rejected() {
    let toml = BAR_TOML.replace(
        "fee_rate = 0.001",
        "fee_rate = 0.001\n\n[engine.sizer]\nkind = \"portfolio_heat\"\nmax_heat = 0.1",
    );
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => assert!(m.contains("[engine.sizer.base]"), "{m}"),
        other => panic!("expected a validation error, got {other:?}"),
    }
}

#[test]
fn a_sizer_drawdown_throttle_missing_its_base_is_rejected() {
    let toml = BAR_TOML.replace(
        "fee_rate = 0.001",
        "fee_rate = 0.001\n\n[engine.sizer]\nkind = \"drawdown_throttle\"\n\
             sensitivity = 0.5\nfloor = 0.2",
    );
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => assert!(m.contains("[engine.sizer.base]"), "{m}"),
        other => panic!("expected a validation error, got {other:?}"),
    }
}

/// Only the two WRAPPING kinds read `SizerCfg::base`. Under a scalar kind the table is read by
/// nobody, so the run installs ONE sizer while the operator reads the profile as a chain —
/// the same silent-no-op class every other rule in this validator refuses.
#[test]
fn a_base_under_a_non_wrapping_sizer_kind_is_rejected() {
    let toml = BAR_TOML.replace(
        "fee_rate = 0.001",
        "fee_rate = 0.001\n\n[engine.sizer]\nkind = \"fixed_dollar\"\namount = 1000.0\n\n\
             [engine.sizer.base]\nkind = \"pass_through\"",
    );
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => {
            assert!(m.contains("fixed_dollar"), "{m}");
            assert!(m.contains("[engine.sizer.base]"), "{m}");
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
}

/// The same refusal one level down, so the rule is not merely a top-level check: a base under
/// a scalar kind nested inside a legitimate wrapper is refused, and the message names the
/// nested path rather than the outer one.
#[test]
fn a_base_under_a_nested_non_wrapping_sizer_kind_is_rejected() {
    let toml = BAR_TOML.replace(
        "fee_rate = 0.001",
        "fee_rate = 0.001\n\n[engine.sizer]\nkind = \"portfolio_heat\"\nmax_heat = 0.1\n\n\
             [engine.sizer.base]\nkind = \"fixed_dollar\"\namount = 1000.0\n\n\
             [engine.sizer.base.base]\nkind = \"pass_through\"",
    );
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => {
            assert!(m.contains("[engine.sizer.base.base]"), "{m}");
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
}

#[test]
fn a_sizer_portfolio_heat_builds_with_a_nested_base() {
    let toml = BAR_TOML.replace(
        "fee_rate = 0.001",
        "fee_rate = 0.001\n\n[engine.sizer]\nkind = \"portfolio_heat\"\nmax_heat = 0.1\n\
             [engine.sizer.base]\nkind = \"fixed_dollar\"\namount = 1000.0",
    );
    let p = BacktestProfile::from_toml_str(&toml).expect("parses");
    assert!(p.engine.sizer.as_ref().unwrap().build().is_ok());
}

#[test]
fn a_sizer_drawdown_throttle_builds_with_a_nested_base() {
    let toml = BAR_TOML.replace(
        "fee_rate = 0.001",
        "fee_rate = 0.001\n\n[engine.sizer]\nkind = \"drawdown_throttle\"\n\
             sensitivity = 0.5\nfloor = 0.2\n\
             [engine.sizer.base]\nkind = \"fixed_dollar\"\namount = 1000.0",
    );
    let p = BacktestProfile::from_toml_str(&toml).expect("parses");
    assert!(p.engine.sizer.as_ref().unwrap().build().is_ok());
}

/// A chain of exactly `depth` `SizerCfg` nodes: `depth - 1` `"portfolio_heat"` wrappers ending
/// in one terminal `"pass_through"` leaf — so the whole chain always BUILDS (not just parses)
/// whenever `depth <= MAX_SIZER_DEPTH`. `depth` counts the same way `SizerCfg::build_at_depth`
/// does: the top-level `[engine.sizer]` table is depth 1.
fn nested_sizer_toml(depth: usize) -> String {
    assert!(depth >= 1);
    let mut path = "engine.sizer".to_string();
    let mut out = String::new();
    for level in 1..=depth {
        out.push_str(&format!("[{path}]\n"));
        if level < depth {
            out.push_str("kind = \"portfolio_heat\"\nmax_heat = 0.5\n");
            path.push_str(".base");
        } else {
            out.push_str("kind = \"pass_through\"\n");
        }
    }
    out
}

#[test]
fn a_sizer_chain_at_the_depth_limit_builds() {
    let toml = format!("{BAR_TOML}\n{}", nested_sizer_toml(MAX_SIZER_DEPTH));
    let p = BacktestProfile::from_toml_str(&toml).expect("parses and validates");
    assert!(p.engine.sizer.as_ref().unwrap().build().is_ok());
}

#[test]
fn a_sizer_chain_past_the_depth_limit_is_rejected() {
    let toml = format!("{BAR_TOML}\n{}", nested_sizer_toml(MAX_SIZER_DEPTH + 1));
    match BacktestProfile::from_toml_str(&toml).unwrap_err() {
        HarnessError::Validation(m) => {
            assert!(m.contains("MAX_SIZER_DEPTH"), "{m}");
            assert!(m.contains(&(MAX_SIZER_DEPTH + 1).to_string()), "{m}");
        }
        other => panic!("expected a validation error, got {other:?}"),
    }
}

/// Manual measurement, not a CI regression test: does deserializing a `[engine.sizer]` chain
/// far past `MAX_SIZER_DEPTH` blow the stack DURING PARSING — before `SizerCfg::build`'s own
/// bound ever gets a chance to run — or does `toml`'s recursive descent handle a deep chain
/// fine, in which case `build` (called from `validate`) catches it cleanly?
///
/// Escalates depth and times each `from_toml_str` call, bailing out (and always failing, via
/// `panic!`, so nextest prints the captured measurements regardless of outcome) once a level
/// is already slow — no point re-proving a hang at a bigger number once one is observed.
/// `#[ignore]`d because the answer at the top of this escalation is a MULTI-MINUTE HANG (see
/// the Task 12 fix-round report — a first flat run at depth 50,000 hit nextest's own slow-test
/// timeout at ~244s with no result either way), which would make the default suite
/// unusable if this ran by default. Run explicitly:
/// `cargo nextest run -p vike-backtest --run-ignored ignored-only sizer_depth_probe`.
#[test]
#[ignore]
fn sizer_depth_probe() {
    use std::fmt::Write as _;
    use std::time::Instant;
    let mut report = String::new();
    for depth in [10usize, 20, 30, 50, 80, 100, 500, 1_000, 2_000, 5_000, 10_000, 20_000, 50_000] {
        let toml = format!("{BAR_TOML}\n{}", nested_sizer_toml(depth));
        let start = Instant::now();
        let result = BacktestProfile::from_toml_str(&toml);
        let elapsed = start.elapsed();
        let outcome = match &result {
            Ok(_) => "Ok (build's own MAX_SIZER_DEPTH check should have refused this — bug if \
                          so)"
            .to_string(),
            Err(e) => format!("Err({e:?})"),
        };
        let _ = writeln!(report, "depth={depth:>6} elapsed={elapsed:>10.2?} outcome={outcome}");
        // Growth here is not linear (see the report) — once one level is already slow, a
        // bigger one will not finish inside this test's own runtime budget.
        if elapsed.as_secs() >= 3 {
            let _ = writeln!(report, "(stopping escalation: depth={depth} was already slow)");
            break;
        }
    }
    panic!("sizer_depth_probe measurements (this failure is expected — see doc):\n{report}");
}
