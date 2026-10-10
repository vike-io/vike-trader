//! The `coverage` rendering, and `gate`'s grammar: the criterion that judges a coverage.

use super::*;

// ---- the `coverage` rendering ----

/// One instrument with a real disagreement (`trade` on every day, `depth` missing one) and one
/// that is complete — the two dispositions the report separates.
fn coverage_rows() -> Vec<InstrumentRow> {
    vec![
        InstrumentRow {
            venue: "binance".to_string(),
            name: "BTCUSDT".to_string(),
            grouped: false,
            kinds: vec![
                KindRow { kind: "trade".to_string(), days: 3 },
                KindRow { kind: "depth".to_string(), days: 2 },
            ],
            spanned_days: 3,
            partial: vec![PartialRow {
                day: 2,
                start_ms: 172_800_000,
                missing: vec!["depth".to_string()],
            }],
        },
        InstrumentRow {
            venue: "polymarket".to_string(),
            name: "fam".to_string(),
            grouped: true,
            kinds: vec![KindRow { kind: "trade".to_string(), days: 2 }],
            spanned_days: 2,
            partial: Vec::new(),
        },
    ]
}

#[test]
fn coverage_lines_line_the_kinds_up_and_detail_only_the_partial_days() {
    let lines = coverage_lines(&coverage_rows(), 2, false);
    assert!(lines[0].contains("INSTRUMENT") && lines[0].contains("PARTIAL"), "{}", lines[0]);
    assert!(lines[1].contains("trade:3, depth:2"), "{}", lines[1]);
    assert!(lines[1].ends_with("  1"), "the partial COUNT is on the row: {}", lines[1]);
    assert_eq!(lines[2], "      1970-01-03  missing: depth");
    // The complete instrument gets its row and NO detail lines — a "nothing missing" line
    // under every healthy instrument is how a report teaches an operator to skim past it.
    assert!(lines[3].contains("fam") && lines[3].ends_with("  -"), "{}", lines[3]);
    assert_eq!(lines.last().unwrap(), "2 instruments · 1 with partial days");
}

/// The human table CAPS the day detail and says how many it withheld; the document does not
/// cap at all (see [`MAX_PARTIAL_DAYS_SHOWN`]).
#[test]
fn the_human_table_caps_the_partial_days_while_the_document_carries_them_all() {
    let mut rows = coverage_rows();
    rows[0].partial = (0..MAX_PARTIAL_DAYS_SHOWN as i64 + 2)
        .map(|d| PartialRow {
            day: d,
            start_ms: d * 86_400_000,
            missing: vec!["depth".to_string()],
        })
        .collect();

    let lines = coverage_lines(&rows, 2, false);
    let detail = lines.iter().filter(|l| l.contains("missing:")).count();
    assert_eq!(detail, MAX_PARTIAL_DAYS_SHOWN, "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("… and 2 more partial days")), "{lines:?}");

    let args = parse_of(&["hist", "coverage", "--json"]).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&coverage_json(&args, &rows, 2)).unwrap();
    assert_eq!(
        doc["instruments"][0]["partial_days"].as_array().unwrap().len(),
        MAX_PARTIAL_DAYS_SHOWN + 2,
        "a truncated array would be a wrong answer, not a long one"
    );
}

/// The document's `complete` flag is computed from the same list it ships, and a partial day
/// carries its day INDEX, its epoch-ms midnight and the rendered date — the index is what the
/// store indexes by, the ms is what everything else derives from.
#[test]
fn coverage_json_carries_the_verdict_and_both_spellings_of_a_day() {
    let args = parse_of(&["hist", "coverage", "--partial-only", "--json"]).unwrap();
    let doc: serde_json::Value =
        serde_json::from_str(&coverage_json(&args, &coverage_rows(), 7)).unwrap();

    assert_eq!(doc["subcommand"], "coverage");
    assert_eq!(doc["partial_only"], true);
    assert_eq!(doc["instruments_reported"], 7);
    assert_eq!(doc["count"], 2);

    let partial = &doc["instruments"][0];
    assert_eq!(partial["complete"], false);
    assert_eq!(partial["venue"], "binance");
    assert_eq!(partial["spanned_days"], 3);
    assert_eq!(partial["kinds"][0], serde_json::json!({ "kind": "trade", "days": 3 }));
    assert_eq!(partial["partial_days"][0]["day"], 2);
    assert_eq!(partial["partial_days"][0]["start_ms"], 172_800_000);
    assert_eq!(partial["partial_days"][0]["date"], "1970-01-03");
    assert_eq!(partial["partial_days"][0]["missing_kinds"][0], "depth");

    let complete = &doc["instruments"][1];
    assert_eq!(complete["complete"], true);
    assert_eq!(complete["grouped"], true, "a grouped instrument says so");
    assert!(complete["partial_days"].as_array().unwrap().is_empty());
}

/// `gate`'s whole line, resolved: the spec becomes a SELECTOR, the duration becomes
/// milliseconds, and the kind roster is never empty. Everything downstream is then a pure fold
/// with no parse left in it — see [`GateArgs`].
#[test]
fn a_gate_line_resolves_its_spec_its_duration_and_its_kind_roster() {
    let a = parse_of(&[
        "hist",
        "gate",
        "binance:BTCUSDT:1h",
        "--require-days",
        "365",
        "--max-gap",
        "4h",
    ])
    .expect("a well-formed gate");
    let g = a.gate.as_ref().expect("a GateArgs");
    assert_eq!(g.spec.text(), "binance:BTCUSDT:1h");
    assert_eq!(g.require_days, 365);
    assert_eq!(g.max_gap_ms, Some(4 * 3_600_000));
    assert_eq!(g.kinds, vec!["bar".to_string()], "defaulted, never empty");
    assert_eq!(a.addr, DEFAULT_ADDR, "…and it is a READ verb, so the addr is resolved");
    assert!(a.spec.is_none(), "the spec is MOVED into GateArgs, never left in both");

    // Repeatable, deduped, and the default is replaced rather than added to.
    let a = parse_of(&[
        "hist",
        "gate",
        "polymarket:@election-2026",
        "--require-days",
        "7",
        "--require-kind",
        "book",
        "--require-kind",
        "trade",
        "--require-kind",
        "book",
    ])
    .expect("a grouped gate");
    let g = a.gate.as_ref().expect("a GateArgs");
    assert_eq!(g.kinds, vec!["book".to_string(), "trade".to_string()]);
    assert_eq!(g.max_gap_ms, None, "the holes were not asked about");
    assert!(g.spec.grouped, "the @ reached the selector: {:?}", g.spec);
}

/// The two refusals that keep a gate from asserting nothing: no spec, and no criterion. Each
/// names what to type instead, so the refusal is never a dead end.
///
/// ⚠ **The criterion refusal used to advertise `--require-days 1` as "the PRESENCE-only
/// spelling", and that label was FALSE.** `judge_days` compares `span_ms >= 86_400_000`, so a
/// series that exists and is six hours old breaches it — an operator who wrote the advertised
/// line into an `ExecStartPre=` had a unit refusing to start over exactly the tape it had just
/// fetched. There is no presence-only spelling, and the message says so rather than implying
/// one.
#[test]
fn a_gate_with_no_subject_or_no_criterion_is_refused_at_the_door() {
    let e = parse_of(&["hist", "gate", "--require-days", "30"]).expect_err("no spec");
    assert!(e.contains("gate needs a spec"), "{e}");
    assert!(e.contains("VENUE:@GROUP"), "…and names the grouped spelling too: {e}");

    let e = parse_of(&["hist", "gate", "binance:BTCUSDT:1h"]).expect_err("no criterion");
    assert!(e.contains("--require-days"), "{e}");
    assert!(e.contains("checked nothing"), "{e}");
    assert!(e.contains("no PRESENCE-only spelling"), "the label that was false is gone: {e}");
    assert!(e.contains("`--require-days 1`"), "…and the narrowest gate is still named: {e}");
    assert!(e.contains("WHOLE DAY"), "…with what it actually asserts: {e}");
}

/// `--require-kind` naming ACCOUNT data is refused in the SAME words `ls --kind` is refused in
/// — one rule, one sentence. The control is the second half: the market funding RATE is not an
/// account kind and still parses.
///
/// ⚠ **The control USED TO BE UNABLE TO FAIL.** It spelled `binance:BTCUSDT:funding` and
/// `--require-kind bar` — putting the word in the spec's INTERVAL slot, which this crate
/// deliberately does not validate, and passing the flag the DEFAULT value that the first half
/// of this file already proves parses. Flip `vike_model::plane_of` so `funding`
/// classifies as `StorePlane::Account` and that spelling stays green while the boundary it
/// claims to guard has moved. The word now reaches `--require-kind` itself, which is the only
/// place the refusal reads.
#[test]
fn a_required_kind_naming_account_data_is_refused_in_the_readers_own_words() {
    let e = parse_of(&[
        "hist",
        "gate",
        "binance:BTCUSDT:1h",
        "--require-days",
        "1",
        "--require-kind",
        "exec_fill",
    ])
    .expect_err("account data");
    assert!(e.contains("ACCOUNT data"), "{e}");
    assert!(e.contains("vike-cli account"), "…and the plane that will serve it: {e}");
    assert_eq!(
        e,
        refuse_an_account_kind_on_a_read("exec_fill").expect_err("the shared sentence"),
        "the two flags must meet ONE sentence"
    );

    // The CONTROL, through the same flag the refusal reads. `funding` is the MARKET rate and
    // must pass `refuse_an_account_kind_on_a_read`; the spec carries NO interval, because the
    // market rate lives in the `bar` kind under `interval=funding` and a third part here would
    // meet `refuse_a_kind_the_spec_can_never_select` for a different reason entirely.
    assert!(
        refuse_an_account_kind_on_a_read("funding").is_ok(),
        "the MARKET funding rate is not account data — `docs/decisions/0080` separated the \
             two names so this can be asserted at all"
    );
    let ok = parse_of(&[
        "hist",
        "gate",
        "binance:BTCUSDT",
        "--require-days",
        "1",
        "--require-kind",
        "funding",
    ]);
    let g = ok.as_ref().expect("the market funding RATE parses as a criterion").gate.as_ref();
    assert_eq!(
        g.expect("a GateArgs").kinds,
        vec!["funding".to_string()],
        "…and it reaches the criterion roster rather than being swallowed: {ok:?}"
    );
}

/// A `--require-kind` the SPEC can never select is refused HERE, before a socket opens — the
/// wiring of [`gate::refuse_a_kind_the_spec_can_never_select`], whose own unit tests carry the
/// argument and the anti-vacuity control.
///
/// ⚠ The order matters and this is where it is asserted: the refusal runs AFTER the default
/// kind is folded in, so a bare `gate binance:BTCUSDT:1h` — whose roster is `[bar]` — is not
/// refused by the check meant for the kind an operator NAMED.
#[test]
fn a_required_kind_the_spec_can_never_select_is_refused_before_a_socket_opens() {
    let e = parse_of(&[
        "hist",
        "gate",
        "binance:BTCUSDT:1h",
        "--require-days",
        "1",
        "--require-kind",
        "bar",
        "--require-kind",
        "trade",
    ])
    .expect_err("an interval-bearing spec cannot select a trade series");
    assert!(e.contains("--require-kind trade"), "{e}");
    assert!(e.contains("`binance:BTCUSDT` gates"), "…and the spec that would work: {e}");

    // The two controls: the default roster under the same spec, and the same roster under a
    // spec that named no step.
    assert!(
        parse_of(&["hist", "gate", "binance:BTCUSDT:1h", "--require-days", "1"]).is_ok(),
        "the DEFAULT roster is the one kind a third part can select"
    );
    assert!(
        parse_of(&[
            "hist",
            "gate",
            "binance:BTCUSDT",
            "--require-days",
            "1",
            "--require-kind",
            "trade",
        ])
        .is_ok(),
        "…and with no step named, a tick kind is exactly what the spec reaches"
    );
}

/// An EMPTY `--require-kind` is refused rather than collapsing into the default. It is
/// reachable from a script (`--require-kind="$K"` with `K` unset), and silently gating `bar`
/// where the operator meant a variable's value is a green over the wrong tape.
#[test]
fn a_blank_required_kind_is_refused_rather_than_defaulted() {
    let e = parse_of(&[
        "hist",
        "gate",
        "binance:BTCUSDT:1h",
        "--require-days",
        "1",
        "--require-kind",
        "",
    ])
    .expect_err("a blank kind");
    assert!(e.contains("EMPTY value"), "{e}");
    assert!(e.contains("omit the flag"), "…and what to do instead: {e}");
}

/// Every flag that does not apply to `gate` is refused BY NAME, and `--kind` — the one an
/// operator reaches for first — names the criterion flag that replaced it.
#[test]
fn gate_refuses_the_listing_flags_and_names_what_replaced_the_kind_filter() {
    let line = |extra: &[&str]| {
        let mut v = vec!["hist", "gate", "binance:BTCUSDT:1h", "--require-days", "1"];
        v.extend_from_slice(extra);
        parse_of(&v)
    };
    assert!(line(&[]).is_ok(), "the negative control: the bare line parses");
    for (extra, needle) in [
        (vec!["--kind", "bar"], "--require-kind"),
        (vec!["--venue", "binance"], "EXACTLY"),
        (vec!["--name", "BTC"], "EXACTLY"),
        (vec!["--class"], "data hist ls --class"),
        (vec!["--partial-only"], "verdict"),
        (vec!["--store", "/srv/hist"], "--addr"),
        (vec!["--engine", "/bin/backtest"], "--addr"),
        (vec!["--days", "30"], "bounds a FETCH window"),
        (vec!["--from", "0"], "bounds a FETCH window"),
        (vec!["--out", "x.parquet"], "only `export` writes one"),
        (vec!["--symbol", "S"], "belongs to `rm` or `repair`"),
        (vec!["--produced-by", "p:"], "only `rm` deletes"),
        (vec!["--source", "demo"], "`fetch`'s axis"),
        (vec!["--gaps"], "data hist gaps"),
    ] {
        let e = line(&extra).expect_err(&format!("{extra:?} must be refused"));
        assert!(e.contains(needle), "{extra:?} must say {needle:?}: {e}");
    }
}

/// ...and the mirror: every CRITERION flag is refused by name on the verbs that judge nothing,
/// with the verb that does judge in the message.
#[test]
fn a_criterion_flag_on_a_rendering_verb_is_refused_and_names_the_gate() {
    for sub in ["ls", "gaps", "coverage", "health", "universe", "fetch", "export"] {
        for extra in
            [vec!["--require-days", "30"], vec!["--max-gap", "1d"], vec!["--require-kind", "bar"]]
        {
            let mut v = vec!["hist", sub];
            v.extend_from_slice(&extra);
            let e = parse_of(&v).expect_err(&format!("{sub} {extra:?}"));
            assert!(e.contains(extra[0]), "{sub} must name the flag: {e}");
            assert!(e.contains("data hist gate"), "{sub} must name the gate: {e}");
        }
    }
}

/// A malformed `--max-gap` is refused HERE, before a socket is opened, because nothing is
/// forwarded: this flag is consumed in this process, so the far side would never see it.
#[test]
fn a_max_gap_that_is_not_fixed_time_is_refused_before_anything_is_dialled() {
    let line = |v: &str| {
        parse_of(&["hist", "gate", "binance:BTCUSDT:1h", "--require-days", "1", "--max-gap", v])
    };
    assert!(line("1d").is_ok(), "the negative control");
    assert!(line("500bars").expect_err("bars").contains("BAR COUNT"));
    assert!(line("3mo").expect_err("months").contains("CALENDAR"));
    assert!(line("").expect_err("blank").contains("--max-gap"));
}
