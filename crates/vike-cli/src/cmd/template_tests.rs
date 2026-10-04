use super::template::{TEMPLATE_HEADER, template_body};
use super::{Sub, parse};
use vike_model::venues::VENUES;

fn parse_of(argv: &[&str]) -> Result<super::Args, String> {
    parse(argv.iter().map(|s| (*s).to_string()))
}

#[test]
fn template_is_a_subcommand_and_takes_venue() {
    assert_eq!(parse_of(&["template"]).unwrap().sub, Sub::Template);
    assert_eq!(parse_of(&["template", "--venue", "okx"]).unwrap().venue.as_deref(), Some("okx"));
}

/// `--venue` on an inspecting subcommand is REFUSED rather than ignored — a silently dropped
/// filter lets somebody believe they scoped an output that was never scoped.
#[test]
fn venue_is_refused_on_list_and_path() {
    for sub in ["list", "path"] {
        let err = parse_of(&[sub, "--venue", "okx"]).unwrap_err();
        assert!(err.contains("--venue"), "{sub}: {err}");
    }
}

/// EVERY roster venue appears, so a venue added to `VENUES` cannot silently miss the template.
/// This is the completeness property the capability-map playbook asks of any per-venue output.
#[test]
fn every_roster_venue_is_emitted() {
    let body = template_body(None).unwrap();
    for v in VENUES {
        assert!(body.contains(&format!("# ── {v} ──")), "no section for {v}");
        assert!(
            body.contains(&format!("{}_LIVE_API_KEY=", v.to_uppercase())),
            "no LIVE key row for {v}"
        );
    }
}

/// The emitted grid carries NO values — it is routinely redirected into a real store, and a
/// stray value would be a credential written by us.
#[test]
fn no_row_carries_a_value() {
    let body = template_body(None).unwrap();
    for line in body.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
        assert!(line.ends_with('='), "row carries a value: {line}");
        assert_eq!(line.matches('=').count(), 1, "row has more than one '=': {line}");
    }
}

/// The legacy `MAINNET` tier is READ by the loader but must not be TAUGHT here.
#[test]
fn the_legacy_tier_is_not_emitted() {
    let body = template_body(None).unwrap();
    assert!(!body.contains("_MAINNET_"), "template teaches the deprecated MAINNET tier");
}

/// The two warnings a first-time reader most needs: that the grid is incomplete for the FX
/// venues, and that a filled key arms nothing without the policy ceiling.
#[test]
fn the_header_carries_both_load_bearing_warnings() {
    assert!(TEMPLATE_HEADER.contains("BESPOKE"), "no incompleteness warning");
    assert!(TEMPLATE_HEADER.contains("policy.venues"), "no arming-ceiling warning");
}

#[test]
fn one_venue_filter_emits_only_that_venue() {
    let body = template_body(Some("okx")).unwrap();
    assert!(body.contains("OKX_LIVE_API_KEY="));
    assert!(!body.contains("BINANCE_LIVE_API_KEY="));
}

/// An unknown venue ERRORS and names the roster. An empty emission would be redirected over a
/// store and look like "this venue needs nothing".
#[test]
fn an_unknown_venue_errors_and_names_the_roster() {
    let err = template_body(Some("kraken_futures_x")).unwrap_err();
    assert!(err.contains("unknown venue"), "{err}");
    assert!(err.contains(VENUES[0]), "error should list the roster: {err}");
}

#[test]
fn the_venue_filter_is_case_insensitive() {
    assert!(template_body(Some("OKX")).unwrap().contains("OKX_LIVE_API_KEY="));
}
