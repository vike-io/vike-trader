//! What a mount of a taker file says, moved here from the daemon's two pinned tests
//! (`a_ladder_that_can_never_rest_a_rung_is_refused`, `the_effective_params_line_reports_what_mounted`
//! in `crates/vike-tradehub/src/config/tests/strategy_params.rs`), which since plan 2b prove only the
//! daemon's half (a file's refusal reaches the caller verbatim, its echo rows render `k=v`). The
//! words come from the files' own `refuse` and `echo`; the strings are the pinned tests' own.

use vike_model::params::Params;

use crate::{buy_hold, dca_accumulate, grid, momentum};

fn params(text: &str) -> Params {
    toml::from_str(text).unwrap_or_else(|e| panic!("{text}: {e}"))
}

fn line(echo: fn(&Params) -> Vec<(&'static str, String)>, text: &str) -> String {
    echo(&params(text)).iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ")
}

type Refuse = fn(&Params) -> Option<String>;

/// A params table that describes NO order at all is refused, carrying the resolution; the
/// near-misses and the empty table are not.
#[test]
fn a_ladder_that_can_never_rest_a_rung_is_refused() {
    let dca: Refuse = dca_accumulate::refuse;
    let grid: Refuse = grid::refuse;
    // (file, its refuse, params, the resolution the message must show)
    for (name, refuse, text, shown) in [
        ("dca_accumulate", dca, "anchor = \"fixed\"", "anchor_price=0"),
        ("grid", grid, "bounded01 = true", "step=1"),
        ("grid", grid, "rungs = 0", "rungs=0"),
        ("grid", grid, "rungs = -5", "rungs=0"),
        ("dca_accumulate", dca, "size = 0.0", "size=0"),
    ] {
        let err = refuse(&params(text)).unwrap_or_else(|| panic!("{name}/{text} must be refused"));
        assert!(err.contains("NO rung"), "{name}/{text}: names what is wrong: {err}");
        assert!(err.contains("never place an order"), "{name}/{text}: {err}");
        assert!(err.contains(shown), "{name}/{text}: carries the resolution: {err}");
        assert!(
            !err.contains("wrong TYPE") && !err.contains("does not read"),
            "{name}/{text}: this is the case the key checks PASS: {err}"
        );
    }
    for (name, refuse, text) in [
        ("dca_accumulate", dca, "anchor = \"fixed\"\nside = \"short\"\nstep = 0.05"),
        ("grid", grid, "bounded01 = true\nstep = 0.05"),
        ("dca_accumulate", dca, "anchor = \"fixed\"\nanchor_price = 40.0"),
        ("grid", grid, "rungs = 4\nstep = 0.5"),
        ("dca_accumulate", dca, "rungs = 4\nstep = 0.5"),
        ("grid", grid, ""),
        ("dca_accumulate", dca, ""),
    ] {
        assert_eq!(refuse(&params(text)), None, "`{name}` / `{text}` must still mount");
    }
}

/// The echo reports the RESOLVED knobs, the whole set, and the resolution rather than the input.
#[test]
fn the_effective_params_line_reports_what_mounted() {
    assert_eq!(
        line(grid::echo, "step = 0.5"),
        "anchor=first anchor_price=0 step=0.5 rungs=3 size=1 band=10 bounded01=false \
         tick=0.001 symbol=(from the feed)"
    );
    assert_eq!(line(buy_hold::echo, ""), "size=1 symbol=(from the feed)");
    let l = line(grid::echo, "anchor = \"fixd\"\nanchor_price = 42.0");
    assert!(l.contains("anchor=first"), "{l}");
    let l = line(dca_accumulate::echo, "side = \"shrot\"");
    assert!(l.contains("side=long"), "{l}");
    let l = line(momentum::echo, "qty = 2.0");
    assert!(l.contains("venue=sim"), "a live mount under the tag `sim`, and it says so: {l}");
    assert!(l.contains("tp=(unarmed)"), "an un-armed barrier leg says so: {l}");
    let l = line(grid::echo, "size = 2.0");
    assert!(l.contains("size=2"), "{l}");
}
