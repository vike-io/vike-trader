//! The mutation self-tests: the harness can fail, and the shapes it cannot see are measured.

use vike_model::params::Params;
use vike_strategies_check::takers;

use super::market::Feed;
use super::roster::{BUY_HOLD, DCA_ACCUMULATE, GRID, PAIRS_ZSCORE, source};
use super::{PROBES, calls, gated, params, params_for, table, trace};

/// The file's `refuse` export (the deleted registry's `unarmable_params`): the whole-table refusal
/// a mount asks before any strategy exists (`crates/vike-rust-strategy/src/host.rs`'s `refusal`).
/// `None` for a roster file with no such export, as the registry answered for every other name.
fn unarmable_params(file: &str, params: &Params) -> Option<String> {
    match file {
        GRID => takers::grid::refuse(params),
        DCA_ACCUMULATE => takers::dca_accumulate::refuse(params),
        _ => None,
    }
}

/// The harness itself must be able to fail: the trace distinguishes what it must and is not a
/// constant, and the two named instances of the class really are inert (their `needs` are not
/// decoration).
#[test]
fn the_harness_can_actually_fail() {
    let t =
        |file: &str, src: &str| trace(file, &toml::from_str::<Params>(src).unwrap(), Feed::Plain);
    // NOT a constant: the same strategy at two sizes traces differently...
    assert_ne!(t(BUY_HOLD, "size = 1.0"), t(BUY_HOLD, "size = 2.0"));
    // ...and at the SAME params it is deterministic, or every inequality above would be noise.
    assert_eq!(t(GRID, "step = 0.02"), t(GRID, "step = 0.02"));
    // ...and non-empty, or equality would hold for the emptiest reason there is.
    assert!(t(GRID, "step = 0.02").len() > 10);

    // THE 4th INSTANCE, measured: a fixed anchor price on a first-price grid changes nothing.
    assert_eq!(
        t(GRID, "anchor_price = 0.4"),
        t(GRID, "anchor_price = 0.9"),
        "the reported finding: `anchor_price` is read only in `anchor_at`'s AnchorMode::Fixed arm"
    );
    assert!(gated(GRID, "anchor_price"), "...so its header MUST declare a `needs`");
    // THE 5th: `tick` outside a bounded-01 market.
    assert_eq!(t(GRID, "tick = 0.001"), t(GRID, "tick = 0.45"));
    assert!(gated(GRID, "tick"));
    // ...and both come alive once armed, which is what makes the `needs` claims rather than excuses.
    assert_ne!(
        t(GRID, "anchor = \"fixed\"\nanchor_price = 0.4"),
        t(GRID, "anchor = \"fixed\"\nanchor_price = 0.9")
    );
    assert_ne!(
        t(GRID, "bounded01 = true\nstep = 0.05\ntick = 0.001"),
        t(GRID, "bounded01 = true\nstep = 0.05\ntick = 0.45")
    );
    // ⚠ ...and the step is why the line above carries one: at the default `step = 1.0` EVERY rung
    // of a 0..1 grid is off the board, so the `tick` row would look proven while proving nothing.
    assert_eq!(
        t(GRID, "bounded01 = true\ntick = 0.001"),
        t(GRID, "bounded01 = true\ntick = 0.45"),
        "a bounded-01 grid at the default step rests nothing, so `tick` cannot show there"
    );
}

/// The source scanner in `crates/vike-strategies-check/tests/param_keys_gate.rs` CANNOT see this
/// class, asserted rather than said: every LOOKUP SITE is an unconditional statement inside
/// `from_params` (`Grid::from_params` reads `anchor_price` in the same straight-line struct literal
/// as `band`), and only `Grid::anchor_at`, with no `params` in scope, decides whether the value is
/// looked at.
#[test]
fn the_lookup_sites_of_this_class_are_unconditional() {
    let src = source(GRID);
    // The gated key and an ungated one sit at the SAME syntactic depth in one initializer.
    assert!(
        src.contains("anchor_price: f(\"anchor_price\").unwrap_or(d.anchor_price),"),
        "the gated key's lookup has moved — re-check whether a source scan could now see it"
    );
    assert!(
        src.contains("band: f(\"band\").unwrap_or(d.band),"),
        "the ungated key's lookup has moved"
    );
    // ...and the CONDITION is in another function, over a FIELD, with no params table in scope.
    assert!(
        src.contains("AnchorMode::Fixed => self.anchor_price,"),
        "the branch that decides whether `anchor_price` is ever read has moved out of \
         `Grid::anchor_at`"
    );
}

/// The residual, DEMONSTRATED: each block runs the case it describes, so the blind spot can be
/// neither quietly widened nor quietly closed. (1) inertness that depends on the FEED, (2)
/// inertness only at a NON-BASE combination, (3) a DEAD CONFIGURATION — a table under which the
/// strategy places no order at all, invisible to both directions, kept as a measured ledger.
#[test]
fn the_shapes_this_harness_cannot_see() {
    let with = |file: &str, src: &str, feed: Feed| {
        trace(file, &toml::from_str::<Params>(src).unwrap(), feed)
    };

    // (1) MARKET-dependent inertness. `PairsZScore` reads `funding_a`/`funding_b` only as the
    //     FALLBACK for a bar with no funding (`self.last_funding_a.unwrap_or(self.funding_a)`), so
    //     on a funding-BEARING feed both are inert — no params predicate can express that.
    let pairs = |funding_a: &str, feed: Feed| {
        trace(PAIRS_ZSCORE, &params(PAIRS_ZSCORE, &format!("funding_a = {funding_a}\n")), feed)
    };
    assert_ne!(
        pairs("0.0", Feed::Plain),
        pairs("900.0", Feed::Plain),
        "the harness's own feed leaves `funding_a` live — that is what direction 2 measures"
    );
    assert_eq!(
        pairs("0.0", Feed::FundedPairLegs),
        pairs("900.0", Feed::FundedPairLegs),
        "...and on a funding-bearing feed the SAME key is inert, with no params-table gate able to \
         say so. Declared blind spot: a header `needs` is a predicate over the params, so \
         inertness that depends on the FEED is out of its reach by construction."
    );
    assert!(
        !gated(PAIRS_ZSCORE, "funding_a"),
        "no `needs` is demanded for it, and none could be written"
    );

    // (2) Inertness only at a NON-BASE combination (direction 2 measures at the base table).
    //     `grid`'s `band`: with `bounded01` on, a band wider than the 0..1 market is swallowed
    //     whole by the wall clamps in `Grid::arm`.
    assert_ne!(
        with(GRID, "band = 0.02", Feed::Plain),
        with(GRID, "band = 9.0", Feed::Plain),
        "at the base table `band` is live"
    );
    assert_eq!(
        with(GRID, "bounded01 = true\nband = 9.0", Feed::Plain),
        with(GRID, "bounded01 = true\nband = 99.0", Feed::Plain),
        "...and two bands that both exceed the 0..1 walls are indistinguishable. Declared blind \
         spot: the probe is one base table per file, not the cross-product of every key."
    );
    assert!(!gated(GRID, "band"));

    // (3) A DEAD CONFIGURATION — the whole strategy silently places no order, so both values of
    //     EVERY key are equally dead and neither direction can fail. A dead table, not a dead key.
    //
    //     Each row is MEASURED (zero calls, against a base table that trades). ⚠ A row that stops
    //     being zero means the strategy was fixed; deleting the row is that fix's other half.
    //
    //     ⚠ Each row is also REFUSED AT LOAD, and asserts it: the file's `refuse` asks the ladder
    //     builders whether any anchor these params permit rests a rung, and a mount is refused
    //     when none does. A measurement here, a gate one layer up; a row that loses either half
    //     has stopped being true.
    const DEAD: &[(&str, &str, &str)] = &[
        (
            GRID,
            "rungs = 0",
            "the degenerate ladder: `Grid::legs_at` returns at `rungs == 0 || size <= 0.0 || \
             step <= 0.0`, so nothing is ever rested",
        ),
        (DCA_ACCUMULATE, "rungs = 0", "...and `DcaAccumulate::arm`'s identical guard"),
        (
            DCA_ACCUMULATE,
            "anchor = \"fixed\"",
            "a FIXED anchor at its compiled default `anchor_price = 0`: every rung prices at \
             `0 - side * k * step` <= 0 and is skipped, while `drive` has already stamped \
             `anchor = Some(0.0)` so it never re-arms. An operator who armed the anchor MODE and \
             left the price unset gets a mount that can never trade",
        ),
        (
            GRID,
            "bounded01 = true",
            "a 0..1 grid at the compiled default `step = 1.0`: every rung is off the board — \
             `the_harness_can_actually_fail` leans on the same fact from the other side",
        ),
    ];
    for (file, src, why) in DEAD {
        assert!(
            calls(file, &params(file, "")) > 0,
            "{file}'s base table must trade, or the contrast below is empty"
        );
        let table: Params = toml::from_str(src).unwrap();
        assert_eq!(
            calls(file, &table),
            0,
            "`{file}` with `{src}` placed NO order at all when this row was measured ({why}) and \
             now places some — the strategy was fixed, so delete this row"
        );
        // ...and the OTHER half: not mountable (asserted on the predicate a mount consults).
        assert!(
            unarmable_params(file, &table).is_some(),
            "`{file}` with `{src}` rests nothing (measured, one line above) and its `refuse` \
             does NOT refuse it — the dead configuration is mountable again"
        );
        // ...and dead for EVERY key: two values of any probed key trace identically under it.
        for probe in PROBES.iter().filter(|p| p.file == *file) {
            let one = params_for(probe, probe.a, src);
            let other = params_for(probe, probe.b, src);
            if calls(file, &one) > 0 || calls(file, &other) > 0 {
                continue; // the probe's own value re-animates the table (`rungs = 4` over `rungs = 0`)
            }
            assert_eq!(
                trace(file, &one, Feed::Plain),
                trace(file, &other, Feed::Plain),
                "{file}'s `{}` must be inert under `{src}`",
                probe.key
            );
        }
        // ...including the ROUTE key (no probe row may name it), which routes at the base table.
        assert_ne!(
            with(file, "symbol = \"A\"", Feed::Plain),
            with(file, "symbol = \"Z\"", Feed::Plain),
            "{file}'s `symbol` really does route the orders at the base table"
        );
        assert_eq!(
            with(file, &format!("{src}\nsymbol = \"A\""), Feed::Plain),
            with(file, &format!("{src}\nsymbol = \"Z\""), Feed::Plain),
            "...and is inert under `{src}` like everything else"
        );
    }

    // (4) The direction the residual CANNOT hide in: a key inert by coincidence of this script
    //     fails direction 2 LOUDLY, so the residual costs a false alarm, never a silent miss.
    assert_eq!(
        with(BUY_HOLD, "size = 1.0", Feed::Plain),
        with(BUY_HOLD, "size = 1.0", Feed::Plain)
    );
}

/// The SOUNDNESS half of the load-time refusal, DRIVEN rather than argued: every table a file's
/// `refuse` rejects must place ZERO orders over the whole scripted market, at both scales.
///
/// ⚠ The direction that can do damage: too WEAK leaves a dead mount mountable (the `DEAD` ledger
/// pins that row by row); too STRONG rejects a profile that would have traded. So the tables are a
/// CROSS PRODUCT: whatever the predicate refuses, the harness must independently find silent.
///
/// The converse is deliberately NOT asserted, because it is false: a bounded grid whose step fits
/// only near a wall rests nothing at this script's mid-domain first price — a live configuration
/// on the wrong market, and refusing it would be exactly the over-refusal above.
#[test]
fn nothing_the_refusal_rejects_would_have_traded() {
    /// Ladder fragments, combined pairwise: each knob at a value that kills the ladder and one that
    /// keeps it alive.
    const FRAGMENTS: &[&str] = &[
        "",
        "rungs = 0",
        "rungs = -5",
        "rungs = 4",
        "size = 0.0",
        "size = 2.0",
        "step = 0.0",
        "step = 0.05",
        "step = 1.0",
        "anchor = \"fixed\"",
        "anchor = \"fixed\"\nanchor_price = 0.5",
        "bounded01 = true",
        "bounded01 = true\ntick = 0.02",
        "side = \"short\"",
    ];
    let merged = |a: &str, b: &str| {
        let mut t = table(a);
        for (k, v) in table(b) {
            t.insert(k, v);
        }
        t
    };
    let (mut refused, mut armable) = (0usize, 0usize);
    for file in [GRID, DCA_ACCUMULATE] {
        for a in FRAGMENTS {
            for b in FRAGMENTS {
                let params = merged(a, b);
                if unarmable_params(file, &params).is_none() {
                    armable += 1;
                    continue;
                }
                refused += 1;
                assert_eq!(
                    calls(file, &params),
                    0,
                    "`{file}` with `{a}` + `{b}` is REFUSED at load, and yet it placed orders over \
                     the scripted market — the refusal is rejecting a mount that would have traded"
                );
            }
        }
    }
    // Both halves non-empty, or the assertion above holds for the emptiest possible reason.
    assert!(refused > 20, "only {refused} tables were refused — the sweep has gone nearly vacuous");
    assert!(armable > 20, "only {armable} tables survived — the refusal is rejecting everything");
}
