//! The mutation self-tests: the harness can fail, and the shapes it cannot see are measured.

use toml::Value;

use vike_strategy::{param_gate, unarmable_params};

use super::market::Feed;
use super::{PROBES, calls, params, params_for, table, trace};

/// The harness itself must be able to fail: the trace distinguishes what it must and is not a
/// constant, and the two named instances of the class really are inert (their rows are not
/// decoration).
#[test]
fn the_harness_can_actually_fail() {
    let t =
        |name: &str, src: &str| trace(name, &toml::from_str::<Value>(src).unwrap(), Feed::Plain);
    // NOT a constant: the same strategy at two sizes traces differently...
    assert_ne!(t("buy_hold", "size = 1.0"), t("buy_hold", "size = 2.0"));
    // ...and at the SAME params it is deterministic, or every inequality above would be noise.
    assert_eq!(t("grid", "step = 0.02"), t("grid", "step = 0.02"));
    // ...and non-empty, or equality would hold for the emptiest reason there is.
    assert!(t("grid", "step = 0.02").len() > 10);

    // THE 4th INSTANCE, measured: a fixed anchor price on a first-price grid changes nothing.
    assert_eq!(
        t("grid", "anchor_price = 0.4"),
        t("grid", "anchor_price = 0.9"),
        "the reported finding: `anchor_price` is read only in `arm`'s AnchorMode::Fixed arm"
    );
    assert!(param_gate("grid", "anchor_price").is_some(), "...so it MUST carry a gate row");
    // THE 5th: `tick` outside a bounded-01 market.
    assert_eq!(t("grid", "tick = 0.001"), t("grid", "tick = 0.45"));
    assert!(param_gate("grid", "tick").is_some());
    // ...and both come alive once armed, which is what makes the rows claims rather than excuses.
    assert_ne!(
        t("grid", "anchor = \"fixed\"\nanchor_price = 0.4"),
        t("grid", "anchor = \"fixed\"\nanchor_price = 0.9")
    );
    assert_ne!(
        t("grid", "bounded01 = true\nstep = 0.05\ntick = 0.001"),
        t("grid", "bounded01 = true\nstep = 0.05\ntick = 0.45")
    );
    // ⚠ ...and the step is why the line above carries one: at the default `step = 1.0` EVERY rung
    // of a 0..1 grid is off the board, so the `tick` row would look proven while proving nothing.
    assert_eq!(
        t("grid", "bounded01 = true\ntick = 0.001"),
        t("grid", "bounded01 = true\ntick = 0.45"),
        "a bounded-01 grid at the default step rests nothing, so `tick` cannot show there"
    );
}

/// The source scanner in `crates/vike-strategy/tests/param_keys_gate.rs` CANNOT see this class,
/// asserted rather than said: every LOOKUP SITE is an unconditional statement inside `from_params`
/// (`Grid::from_params` reads `anchor_price` in the same straight-line struct literal as `band`),
/// and only `Grid::anchor_at`, with no `params` in scope, decides whether the value is looked at.
#[test]
fn the_lookup_sites_of_this_class_are_unconditional() {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/strategies/grid_dca.rs"),
    )
    .expect("grid_dca.rs");
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
         `Grid::anchor_at` / `DcaAccumulate::arm`"
    );
}

/// The residual, DEMONSTRATED: each block runs the case it describes, so the blind spot can be
/// neither quietly widened nor quietly closed. (1) inertness that depends on the FEED, (2)
/// inertness only at a NON-BASE combination, (3) a DEAD CONFIGURATION — a table under which the
/// strategy places no order at all, invisible to both directions, kept as a measured ledger.
#[test]
fn the_shapes_this_harness_cannot_see() {
    let with = |name: &str, src: &str, feed: Feed| {
        trace(name, &toml::from_str::<Value>(src).unwrap(), feed)
    };

    // (1) MARKET-dependent inertness. `PairsZScore` reads `funding_a`/`funding_b` only as the
    //     FALLBACK for a bar with no funding (`self.last_funding_a.unwrap_or(self.funding_a)`), so
    //     on a funding-BEARING feed both are inert — no params predicate can express that.
    let pairs = |funding_a: &str, feed: Feed| {
        trace("pairs_zscore", &params("pairs_zscore", &format!("funding_a = {funding_a}\n")), feed)
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
         say so. Declared blind spot: a `PARAM_GATES` row is a predicate over the params, so \
         inertness that depends on the FEED is out of its reach by construction."
    );
    assert!(
        param_gate("pairs_zscore", "funding_a").is_none(),
        "no gate row is demanded for it, and none could be written"
    );

    // (2) Inertness only at a NON-BASE combination (direction 2 measures at the base table).
    //     `grid`'s `band`: with `bounded01` on, a band wider than the 0..1 market is swallowed
    //     whole by the wall clamps in `Grid::arm`.
    assert_ne!(
        with("grid", "band = 0.02", Feed::Plain),
        with("grid", "band = 9.0", Feed::Plain),
        "at the base table `band` is live"
    );
    assert_eq!(
        with("grid", "bounded01 = true\nband = 9.0", Feed::Plain),
        with("grid", "bounded01 = true\nband = 99.0", Feed::Plain),
        "...and two bands that both exceed the 0..1 walls are indistinguishable. Declared blind \
         spot: the probe is one base table per strategy, not the cross-product of every key."
    );
    assert!(param_gate("grid", "band").is_none());

    // (3) A DEAD CONFIGURATION — the whole strategy silently places no order, so both values of
    //     EVERY key are equally dead and neither direction can fail. A dead table, not a dead key.
    //
    //     Each row is MEASURED (zero calls, against a base table that trades). ⚠ A row that stops
    //     being zero means the strategy was fixed; deleting the row is that fix's other half.
    //
    //     ⚠ Each row is also REFUSED AT LOAD, and asserts it: `unarmable_params` asks the ladder
    //     builders whether any anchor these params permit rests a rung, and the daemon refuses the
    //     profile when none does. A measurement here, a gate one layer up; a row that loses either
    //     half has stopped being true. (Why no mount-line `(inert: …)` marking:
    //     `vike_strategy::PARAM_GATES`' doc.)
    const DEAD: &[(&str, &str, &str)] = &[
        (
            "grid",
            "rungs = 0",
            "the degenerate ladder: `Grid::arm` returns at `rungs == 0 || size <= 0.0 || \
             step <= 0.0`, so nothing is ever rested",
        ),
        ("dca_accumulate", "rungs = 0", "...and `DcaAccumulate::arm`'s identical guard"),
        (
            "dca_accumulate",
            "anchor = \"fixed\"",
            "a FIXED anchor at its compiled default `anchor_price = 0`: every rung prices at \
             `0 - side * k * step` <= 0 and is skipped, while `drive` has already stamped \
             `anchor = Some(0.0)` so it never re-arms. An operator who armed the anchor MODE and \
             left the price unset gets a mount that can never trade",
        ),
        (
            "grid",
            "bounded01 = true",
            "a 0..1 grid at the compiled default `step = 1.0`: every rung is off the board — \
             `the_harness_can_actually_fail` leans on the same fact from the other side",
        ),
    ];
    for (name, src, why) in DEAD {
        assert!(
            calls(name, &params(name, "")) > 0,
            "{name}'s base table must trade, or the contrast below is empty"
        );
        let table: Value = toml::from_str(src).unwrap();
        assert_eq!(
            calls(name, &table),
            0,
            "`{name}` with `{src}` placed NO order at all when this row was measured ({why}) and \
             now places some — the strategy was fixed, so delete this row"
        );
        // ...and the OTHER half: not mountable (asserted on the predicate the daemon consults).
        assert!(
            unarmable_params(name, &table).is_some(),
            "`{name}` with `{src}` rests nothing (measured, one line above) and \
             `unarmable_params` does NOT refuse it — the dead configuration is mountable again"
        );
        // ...and dead for EVERY key: two values of any probed key trace identically under it.
        for probe in PROBES.iter().filter(|p| p.strategy == *name) {
            let one = params_for(probe, probe.a, src);
            let other = params_for(probe, probe.b, src);
            if calls(name, &one) > 0 || calls(name, &other) > 0 {
                continue; // the probe's own value re-animates the table (`rungs = 4` over `rungs = 0`)
            }
            assert_eq!(
                trace(name, &one, Feed::Plain),
                trace(name, &other, Feed::Plain),
                "{name}'s `{}` must be inert under `{src}`",
                probe.key
            );
        }
        // ...including the ROUTE key (no probe row may name it), which routes at the base table.
        assert_ne!(
            with(name, "symbol = \"A\"", Feed::Plain),
            with(name, "symbol = \"Z\"", Feed::Plain),
            "{name}'s `symbol` really does route the orders at the base table"
        );
        assert_eq!(
            with(name, &format!("{src}\nsymbol = \"A\""), Feed::Plain),
            with(name, &format!("{src}\nsymbol = \"Z\""), Feed::Plain),
            "...and is inert under `{src}` like everything else"
        );
    }

    // (4) The direction the residual CANNOT hide in: a key inert by coincidence of this script
    //     fails direction 2 LOUDLY, so the residual costs a false alarm, never a silent miss.
    assert_eq!(
        with("buy_hold", "size = 1.0", Feed::Plain),
        with("buy_hold", "size = 1.0", Feed::Plain)
    );
}

/// The SOUNDNESS half of the load-time refusal, DRIVEN rather than argued: every table
/// [`vike_strategy::unarmable_params`] rejects must place ZERO orders over the whole scripted
/// market, at both scales.
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
        Value::Table(t)
    };
    let (mut refused, mut armable) = (0usize, 0usize);
    for name in ["grid", "dca_accumulate"] {
        for a in FRAGMENTS {
            for b in FRAGMENTS {
                let params = merged(a, b);
                if unarmable_params(name, &params).is_none() {
                    armable += 1;
                    continue;
                }
                refused += 1;
                assert_eq!(
                    calls(name, &params),
                    0,
                    "`{name}` with `{a}` + `{b}` is REFUSED at load, and yet it placed orders over \
                     the scripted market — the refusal is rejecting a mount that would have traded"
                );
            }
        }
    }
    // Both halves non-empty, or the assertion above holds for the emptiest possible reason.
    assert!(refused > 20, "only {refused} tables were refused — the sweep has gone nearly vacuous");
    assert!(armable > 20, "only {armable} tables survived — the refusal is rejecting everything");
}
