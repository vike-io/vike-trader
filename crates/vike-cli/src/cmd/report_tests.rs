use super::*;
// ⚠ Imported HERE rather than at the top of the file: nothing outside these tests reads the
// roster (the PARSER reaches it through `Period::parse`, which owns the refusal), so a
// top-level import would be an unused one — and this crate's clippy lane runs `-D warnings`.
use crate::cmd::report::schema::PERIODS;
use vike_model::accounts::account_keys::AccountLabel;

fn parsed(args: &[&str]) -> Result<Args, Stop> {
    parse(args.iter().map(|s| s.to_string()))
}

/// `--cash`'s help states the report seed vike-analytics holds, in the bytes it always printed (B5.05).
#[test]
fn the_cash_help_states_the_report_seed_as_it_always_read() {
    assert_eq!(
        cash_help(),
        "the account's starting capital, the realized curve's seed, which scales the return ratios \
         (default 10000; an account or a forward run)"
    );
}

/// The one source a line names.
fn source(args: &[&str]) -> Source {
    match parsed(args).unwrap().request {
        Request::One(source) => source,
        other => panic!("{args:?} is not one source: {other:?}"),
    }
}

// ---- the RETIRED spellings (owner, 2026-11-18) ----

/// **`--node` is refused in every form, with the ONE line both journal doors share**, before any
/// other rule: whatever else the line carries, the answer is where the live report went. The
/// refusal is a parse error, so nothing is dialed and no key is read.
#[test]
fn the_node_door_is_refused_naming_the_account_spelling() {
    for line in [
        vec!["--node", "<host>:9200"],
        vec!["--node=<host>:9200", "--cash=25000", "--json"],
        vec!["--node", "n:1", "--breakdown", "day"],
        vec!["run:@last", "--node", "n:1"],
        vec!["account:binance/MAIN", "--node", "n:1"],
        vec!["--node", "n:1", "--schema"],
        vec!["--node", "n:1", "--cash", "lots"],
    ] {
        let err = parsed(&line).unwrap_err();
        assert!(!err.is_help(), "{line:?} is a refusal");
        assert!(err.contains(JOURNAL_DOORS_RETIRED), "{line:?}: {err}");
    }
    assert!(JOURNAL_DOORS_RETIRED.contains("report account:<venue>/<LABEL>"));
    assert!(!JOURNAL_DOORS_RETIRED.contains('\n'), "one line");
}

/// **A bare selector is refused naming the spelling that works, with the operator's own selector
/// in it** — every form the stored grammar takes, `@` forms and an `=` included.
#[test]
fn a_bare_selector_is_refused_naming_its_run_spelling() {
    for selector in ["1756080000-abcd-0", "1756080", "@last", "@last:backtest", "@mark=v2", "help"]
    {
        let err = parsed(&[selector]).unwrap_err();
        assert!(!err.is_help(), "{selector} is a refusal, never a help word");
        assert!(
            err.contains(&format!(
                "a bare run selector is retired: spell it `vike-cli report run:{selector}`"
            )),
            "{selector}: {err}"
        );
    }
}

/// `run:` with nothing after it would be a prefix of EVERY run id, so a one-run project would
/// answer about that run silently. It is refused by name.
#[test]
fn an_empty_run_selector_is_refused() {
    let err = parsed(&["run:"]).unwrap_err();
    assert!(err.contains("`run:` names no run"), "{err}");
}

// ---- the grammar: numbers and flags ----

#[test]
fn json_is_a_bare_boolean_and_an_inline_value_is_a_usage_error() {
    let err = parsed(&["run:@last", "--json=1"]).unwrap_err();
    assert!(err.contains("--json"), "{err}");
}

/// A number that does not parse is refused by NAME — and so is one that parses to a non-finite
/// value, because JSON writes it as `null` and a renderer would then apply its own default while
/// the operator believed they had set one.
#[test]
fn a_bad_number_is_refused_by_name_and_a_non_finite_one_counts_as_bad() {
    let err = parsed(&["run:@last", "--cash", "lots"]).unwrap_err();
    assert!(err.contains("--cash"), "{err}");
    // (`-inf` is a single-dash word that is no number, so the shared reader refuses it before this
    // verb sees it — still refused.)
    assert!(parsed(&["run:@last", "--periods-per-year", "-inf"]).is_err());
    for bad in ["nan", "inf"] {
        let err = parsed(&["run:@last", "--periods-per-year", bad]).unwrap_err();
        assert!(err.contains("finite"), "{bad}: {err}");
    }
}

#[test]
fn help_short_circuits_even_without_a_source() {
    for spelling in ["-h", "--help"] {
        assert!(parsed(&[spelling]).unwrap_err().is_help(), "{spelling} is a success");
    }
}

#[test]
fn an_unknown_flag_is_rejected_by_name() {
    let err = parsed(&["run:@last", "--format", "csv"]).unwrap_err();
    assert!(err.contains("--format"), "{err}");
}

// ---- the grammar: the STORED source ----

/// `run:<selector>` is the stored source, and the selector after `run:` takes the whole selector
/// grammar — including the `@` forms, which a naive parser would mistake for something else.
#[test]
fn a_run_positional_is_the_stored_run_selector() {
    for selector in ["1756080000-abcd-0", "1756080", "@last", "@last:backtest", "@baseline"] {
        assert_eq!(
            source(&[&format!("run:{selector}")]),
            Source::Run(selector.to_string()),
            "{selector}"
        );
    }
    let a = parsed(&["run:@last", "--breakdown", "month", "--json"]).unwrap();
    assert_eq!(a.breakdown, Some(Period::Month));
    assert!(a.json);
    // …and the HTML page of a stored run.
    let a = parsed(&["run:@last", "--html", "out.html"]).unwrap();
    assert_eq!(a.html.as_deref(), Some(std::path::Path::new("out.html")));
}

/// ⚠ A selector carrying an `=` survives. The shared flag iterator splits every argument on the
/// first `=`, so a positional has to be re-joined or a selector is silently TRUNCATED — which
/// would resolve to a different run, or to none, with nothing saying why.
#[test]
fn a_selector_containing_an_equals_is_put_back_together() {
    assert_eq!(source(&["run:@mark=v2"]), Source::Run("@mark=v2".to_string()));
}

/// `--breakdown`'s value is checked against the PUBLISHED roster, so the parser and
/// `crate::cmd::report::schema::json_schema`'s `enum` cannot name different sets.
#[test]
fn a_breakdown_period_is_checked_against_the_published_roster() {
    let err = parsed(&["run:@last", "--breakdown", "fortnight"]).unwrap_err();
    for p in PERIODS {
        assert!(err.contains(p), "the refusal must name `{p}`: {err}");
    }
    assert_eq!(parsed(&["run:@last", "--breakdown=day"]).unwrap().breakdown, Some(Period::Day));
}

/// Two sources without `--compare` is a refusal that points at it — an operator who typed two
/// wants a comparison, and a comparison is asked for, never implied.
#[test]
fn two_selectors_are_refused_and_point_at_the_verb_that_takes_two() {
    let err = parsed(&["run:a-1-0", "run:b-1-0"]).unwrap_err();
    assert!(err.contains("ONE source"), "{err}");
    assert!(err.contains("report --compare <A> <B>"), "{err}");
}

// ---- the grammar: `--compare` ----

/// `--compare` holds exactly two sources, A first; `--schema --compare` is the comparison's shape.
#[test]
fn compare_holds_two_sources_in_order() {
    let a = parsed(&["--compare", "run:a-1-0", "account:binance/MAIN"]).unwrap();
    let Request::Compare(Source::Run(first), Source::Account(second)) = a.request else {
        panic!("not a comparison of a run and an account")
    };
    assert_eq!((first.as_str(), second.venue.as_str()), ("a-1-0", "binance"));
    assert!(matches!(
        parsed(&["run:a-1-0", "--compare", "run:b-1-0"]).unwrap().request,
        Request::Compare(Source::Run(_), Source::Run(_))
    ));
    for line in [
        vec!["--compare", "run:a-1-0"],
        vec!["--compare"],
        vec!["--compare", "run:a", "run:b", "run:c"],
    ] {
        let err = parsed(&line).unwrap_err();
        assert!(err.contains("`--compare` holds TWO sources"), "{line:?}: {err}");
    }
    assert_eq!(
        parsed(&["--schema", "--compare"]).unwrap().request,
        Request::Schema { compare: true }
    );
}

/// Under `--compare` an account's knobs need an account side, and ONE source's knobs have none.
#[test]
fn compare_takes_the_flags_its_sides_can_use() {
    let both = ["--compare", "run:a-1-0", "account:binance/MAIN"];
    let line = |extra: &[&str]| -> Vec<String> {
        both.iter().chain(extra).map(|s| s.to_string()).collect()
    };
    for extra in [
        &["--from", "2026-11-01"][..],
        &["--cash", "5000"],
        &["--mark-to-market", "--interval", "5m"],
        &["--without-foreign"],
        &["--json"],
        &["--html", "c.html"],
    ] {
        assert!(parse(line(extra).into_iter()).is_ok(), "{extra:?}");
    }
    let err = parse(line(&["--breakdown", "day"]).into_iter()).unwrap_err();
    assert!(err.contains("--breakdown buckets ONE stored run's curve"), "{err}");
    let err = parse(line(&["--name", "x"]).into_iter()).unwrap_err();
    assert!(err.contains("--name heads ONE account's report"), "{err}");
    // A run may be a FORWARD run, which takes `--cash`: refused once both are read as backtests
    // (`a_live_knob_is_refused_on_a_stored_run_and_taken_by_a_forward_one`).
    assert!(parsed(&["--compare", "run:a-1-0", "run:b-1-0", "--cash", "5000"]).is_ok());
    let err = parsed(&["--compare", "run:a-1-0", "run:b-1-0", "--from", "5"]).unwrap_err();
    assert!(err.contains("--from"), "{err}");
}

// ---- the grammar: the ACCOUNT source ----

/// `account:<venue>/<LABEL>` is `crate::cmd::trade::selector`'s grammar: a bare venue and
/// `<venue>/DEFAULT` are the unlabelled account, a route key and an empty book are refused in its
/// own words.
#[test]
fn an_account_positional_is_a_book() {
    let Source::Account(book) = source(&["account:binance/MAIN"]) else { panic!("not an account") };
    assert_eq!((book.venue.as_str(), book.label.text()), ("binance", Some("MAIN")));
    for unlabelled in ["account:binance", "account:binance/DEFAULT"] {
        let Source::Account(book) = source(&[unlabelled]) else { panic!("{unlabelled}") };
        assert_eq!(book.label, AccountLabel::default(), "{unlabelled}");
    }
    assert!(parsed(&["account:"]).unwrap_err().contains("`account:` names no account"));
    assert!(parsed(&["account:binance#ALT"]).unwrap_err().contains("ROUTE KEY"));
}

/// `--from`/`--to` are the workspace's one date parse: epoch-ms verbatim, a date its UTC day's
/// start, and an END named as a date reaches to that day's last millisecond. A window that holds
/// nothing is refused: it would read like an account that never traded.
#[test]
fn the_window_reads_dates_and_instants() {
    let a = parsed(&["account:binance", "--from", "2026-11-01", "--to", "2026-11-18"]).unwrap();
    let day = vike_model::MS_PER_DAY;
    let nov1 = vike_model::parse_date_label("2026-11-01").unwrap();
    let nov18 = vike_model::parse_date_label("2026-11-18").unwrap();
    assert_eq!((a.from, a.to), (Some(nov1), Some(nov18 + day - 1)));
    let a = parsed(&["account:binance", "--from", "5", "--to", "9"]).unwrap();
    assert_eq!((a.from, a.to), (Some(5), Some(9)), "epoch-ms is taken verbatim");
    assert!(
        parsed(&["account:binance", "--from", "9", "--to", "5"]).unwrap_err().contains("after")
    );
    assert!(parsed(&["account:binance", "--from", "soon"]).unwrap_err().contains("--from"));
}

/// Every account knob parses on an account.
#[test]
fn an_account_takes_its_knobs() {
    let a = parsed(&[
        "account:binance/MAIN",
        "--cash",
        "5000",
        "--periods-per-year",
        "365",
        "--mark-to-market",
        "--interval",
        "5m",
        "--without-foreign",
        "--name",
        "mine",
        "--html",
        "a.html",
    ])
    .unwrap();
    assert_eq!((a.cash, a.periods_per_year), (Some(5000.0), Some(365.0)));
    assert!(a.mark_to_market && a.without_foreign);
    assert_eq!((a.interval.as_deref(), a.name.as_deref()), (Some("5m"), Some("mine")));
}

// ---- the SOURCE rules ----

/// ⚠ **No source is a refusal that names the sources, and no source may become a default.**
/// Defaulting to `@last` would report on whatever happened to run most recently, which is the
/// silent precedence the selector grammar refuses everywhere else.
#[test]
fn no_source_names_both_rather_than_defaulting_to_one() {
    let err = parsed(&["--json"]).unwrap_err();
    assert!(err.contains("run:<run>"), "{err}");
    assert!(err.contains("account:<venue>/<LABEL>"), "{err}");
    assert!(err.contains("--schema"), "{err}");
}

/// ⚠ **Two sources is a refusal, never a precedence.** An operator who typed both believes both
/// matter; answering about one silently would let `--schema` win over a real run.
#[test]
fn naming_two_sources_is_refused_with_both_named() {
    let err = parsed(&["run:@last", "--schema"]).unwrap_err();
    assert!(err.contains("run:<run>") && err.contains("--schema"), "{err}");
    assert!(err.contains("ONE source"), "{err}");
    assert!(err.contains("no precedence"), "{err}");
    let err = parsed(&["account:binance", "--schema"]).unwrap_err();
    assert!(err.contains("account:<venue>/<LABEL>") && err.contains("--schema"), "{err}");
}

/// The stored backtest [`runs`] holds.
const BACKTEST: &str = "1756080000-abcd-0";

/// A runs root holding one stored backtest ([`BACKTEST`]) and one forward run of `binance/MAIN`, whose id
/// is returned.
fn runs() -> (tempfile::TempDir, String) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join(BACKTEST);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("manifest.json"),
        r#"{"schema":1,"run_id":"1756080000-abcd-0","kind":"backtest","produced_by":"backtest","started_at":"2025-08-25T00:00:00Z","finished_at":"2025-08-25T00:01:00Z","git_sha":null,"config":{"path":null,"name":null},"detail":null}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("report.json"),
        r#"{"name":"demo","final_equity":10500.0,"total_return":0.05,"n_trades":3,
            "win_rate":0.66,"sharpe":1.25,"max_drawdown":0.031,"profit_factor":2.0}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("series.json"),
        r#"{"schema":1,"equity":[10000.0,10500.0],"equity_timestamp":[1756080000000,1756166400000],"stride":1,"source_len":2}"#,
    )
    .unwrap();
    std::fs::write(dir.join("config.toml"), "[data]\nkind = \"bar\"\ninterval = \"1d\"\n").unwrap();
    let forward = vike_runs::ForwardRun {
        schema: vike_runs::FORWARD_RUN_SCHEMA,
        mount_id: "m-1".to_string(),
        venue: "binance".to_string(),
        symbol: "BTCUSDT".to_string(),
        interval: "1m".to_string(),
        account: "binance/MAIN".to_string(),
        strategy_file: "trend.rhai".to_string(),
        strategy_sha256: "0".repeat(64),
        params: Default::default(),
        reference_run: None,
        started_ms: 1_757_000_000_000,
    };
    let id = vike_runs::write_forward_run(tmp.path(), &forward, None).unwrap().run_id;
    (tmp, id)
}

/// The whole verb over `runs` with no node and no datahub: a line a live side reaches ends at the
/// unset `config.node_addr`, so whatever refuses before that refused the line.
fn answer(runs: &std::path::Path, line: &[&str]) -> CommandResult<String> {
    let a = parsed(line).unwrap();
    let ctx = Context {
        runs_root: Some(runs.to_path_buf()),
        marks_root: None,
        datahub_addr: None,
        datahub_keys: &|| None,
        node_addr: None,
        hub_keys: &HubKeyring::default,
        now_ms: 1_758_000_000_000,
    };
    match &a.request {
        Request::One(source) => one(&ctx, &a, source),
        Request::Compare(x, y) => comparison::run_compare(&ctx, &a, x, y),
        other => panic!("{line:?} is not a source: {other:?}"),
    }
}

/// ⚠ **The refusal that matters most.** `--cash` and `--periods-per-year` RESCALE every return
/// ratio a live report computes; a stored run recomputes none of them. Silently dropping either
/// would hand somebody ratios they believe are scaled to their account — so both are refused by
/// name on a stored backtest, with what to do instead. A FORWARD run is folded as an account is, so
/// it TAKES them (the reviewer's seed finding: it could not, and always folded from the default).
#[test]
fn a_live_knob_is_refused_on_a_stored_run_and_taken_by_a_forward_one() {
    let (tmp, forward) = runs();
    let stored = format!("run:{BACKTEST}");
    let err = answer(tmp.path(), &[&stored, "--cash", "25000"]).unwrap_err();
    assert_eq!(err.exit, crate::exit::Exit::Usage, "{}", err.message);
    assert!(err.message.contains("--cash is a LIVE"), "{}", err.message);
    assert!(err.message.contains("verbatim"), "what a stored run does instead: {}", err.message);
    assert!(err.message.contains("account:<venue>/<LABEL>"), "who takes it: {}", err.message);
    assert!(err.message.contains("1756080000-abcd-0 is a stored backtest"), "{}", err.message);

    let err = answer(tmp.path(), &[&stored, "--periods-per-year", "365"]).unwrap_err();
    assert!(err.message.contains("--periods-per-year is a LIVE"), "{}", err.message);
    assert!(err.message.contains("annualized when it RAN"), "{}", err.message);
    for (flag, extra) in
        [("--mark-to-market", None), ("--without-foreign", None), ("--name", Some("x"))]
    {
        let line: Vec<&str> = [stored.as_str(), flag].into_iter().chain(extra).collect();
        let err = answer(tmp.path(), &line).unwrap_err();
        assert!(err.message.starts_with(flag), "{line:?}: {}", err.message);
        assert!(err.message.contains("account:<venue>/<LABEL>"), "{line:?}: {}", err.message);
    }
    let err = answer(tmp.path(), &["--compare", &stored, &stored, "--cash", "1"]).unwrap_err();
    assert!(err.message.contains("--cash is a LIVE"), "{}", err.message);
    assert!(err.message.contains("are stored backtests"), "{}", err.message);

    // The forward run takes every knob and reaches its live side (here: no node to ask).
    let forward = format!("run:{forward}");
    let knobs = ["--cash", "25000", "--periods-per-year", "365", "--without-foreign"];
    for line in [
        [&[forward.as_str()][..], &knobs].concat(),
        [&["--compare", stored.as_str(), forward.as_str()][..], &knobs].concat(),
    ] {
        let err = answer(tmp.path(), &line).unwrap_err();
        assert!(err.message.contains("config.node_addr"), "{line:?}: {}", err.message);
    }
}

/// A stored side says its starting capital too, its stored curve's first equity: in the text and in
/// the document's side notes.
#[test]
fn a_stored_side_says_its_starting_capital() {
    let (tmp, _) = runs();
    let stored = format!("run:{BACKTEST}");
    let said = "starting capital 10000 (its stored curve's first equity)";
    let text = answer(tmp.path(), &["--compare", &stored, &stored]).unwrap();
    assert!(text.contains(&format!("a: {said}\n")), "{text}");
    let json = answer(tmp.path(), &["--compare", &stored, &stored, "--json"]).unwrap();
    let doc: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(doc["b"]["notes"], serde_json::json!([said]), "{doc}");
}

/// The window knobs are refused on a run by name, and `--breakdown` on an account: each would
/// otherwise change nothing while the operator believes it did.
#[test]
fn each_knob_is_refused_where_it_cannot_apply() {
    for (line, flag) in [
        (vec!["run:@last", "--from", "2026-11-01"], "--from"),
        (vec!["run:@last", "--to", "5"], "--to"),
    ] {
        let err = parsed(&line).unwrap_err();
        assert!(err.contains(flag) && err.contains("account:<venue>/<LABEL>"), "{line:?}: {err}");
    }
    let err = parsed(&["account:binance", "--breakdown", "day"]).unwrap_err();
    assert!(err.contains("--breakdown") && err.contains("series.json"), "{err}");
    let err = parsed(&["account:binance", "--json", "--html", "a.html"]).unwrap_err();
    assert!(err.contains("--json and --html"), "{err}");
    let err = parsed(&["account:binance", "--interval", "5m"]).unwrap_err();
    assert!(err.contains("--mark-to-market was not given"), "{err}");
}

/// `--schema` answers about the DOCUMENT, so every other flag on the line was written in the
/// belief that it would change the output and none of them can. An inert flag is the defect
/// this workspace refuses; a named refusal is the cure.
#[test]
fn schema_refuses_every_flag_that_cannot_change_it() {
    for line in [
        vec!["--schema", "--json"],
        vec!["--schema", "--cash", "1"],
        vec!["--schema", "--periods-per-year", "365"],
        vec!["--schema", "--breakdown", "day"],
        vec!["--schema", "--from", "5"],
        vec!["--schema", "--html", "a.html"],
    ] {
        let err = parsed(&line).unwrap_err();
        assert!(err.contains("--schema"), "{line:?}: {err}");
        assert!(err.contains("one form"), "{line:?}: {err}");
    }
    // …and on its own it parses to the schema request and nothing else.
    assert_eq!(parsed(&["--schema"]).unwrap().request, Request::Schema { compare: false });
}

/// The schema really is printable with no run, no node and no key — the property that makes it
/// the one surface this verb has on a box with none of those.
#[test]
fn the_schema_prints_without_a_run_a_node_or_a_key() {
    let text = schema_text();
    let doc: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
    assert_eq!(doc["type"], serde_json::json!("object"));
    assert!(doc["properties"]["schema"]["const"].is_number(), "{doc}");
}

/// ⚠ The usage text must advertise both sources in their ONE spelling and the schema, and must
/// NOT advertise the retired `--node`: help that names a door which refuses is the defect
/// `crate::RETIRED_COMMANDS`' rule refuses for whole verbs.
#[test]
fn the_usage_advertises_the_run_spelling_and_hides_the_retired_door() {
    let usage = args::long_help::<Cli>(BIN);
    assert!(usage.contains("Usage:"), "help_cli.rs asserts this token on the shipped binary");
    for token in
        ["SOURCE", "run:", "account:", "--schema", "--breakdown", "@last", "--from", "--compare"]
    {
        assert!(usage.contains(token), "the usage must advertise `{token}`");
    }
    // (The prose names `--node` as RETIRED; what must not appear is its option row.)
    assert!(!usage.contains("--node <"), "the retired door is not an option row: {usage}");
    for period in PERIODS {
        assert!(usage.contains(period), "…and every --breakdown period: `{period}`");
    }
    // ⚠ The DECIMATION rule is in the usage, not only in the code: an operator reading `-h` is
    // the person who will otherwise be surprised by a refusal, and the refusal is the feature.
    assert!(usage.contains("MAX_EQUITY_SAMPLES"), "the cap that causes the refusal is named");
    assert!(usage.contains("DECIMATED"), "…and the word the refusal itself uses");
}
