use super::*;

/// The runbook's lines parse, and `add` writes its date as a row holds it.
#[test]
fn the_runbook_lines_parse() {
    let line = [
        "add",
        "--venue",
        "dukascopy",
        "--symbol",
        "AAPLUSUSD",
        "--date",
        "2020-8-31",
        "--ratio",
        "4",
    ];
    let add = parse(&line).expect("add");
    let SplitsVerb::Add(flags) = &add else { panic!("{add:?}") };
    assert_eq!(judge(&add), Ok(()));
    let split = split_of(flags).expect("a split");
    assert_eq!(
        (split.venue.as_str(), split.symbol.as_str(), split.date.as_str(), split.ratio),
        ("dukascopy", "AAPLUSUSD", "2020-08-31", 4.0)
    );

    let rm = ["rm", "--venue", "dukascopy", "--symbol", "AAPLUSUSD", "--date", "2020-08-31"];
    assert!(matches!(parse(&rm), Ok(SplitsVerb::Rm(_))));
    let rename = ["rename", "--venue", "dukascopy", "--ticker", "FB", "--to", "META"];
    assert!(matches!(parse(&rename), Ok(SplitsVerb::Rename(_))));
    assert!(parse(&["rename", "--venue", "dukascopy", "--ticker", "FB"]).is_err(), "--to required");
    assert!(matches!(parse(&["ls"]), Ok(SplitsVerb::Ls(_))));
    assert!(matches!(parse(&["ls", "--venue", "dukascopy"]), Ok(SplitsVerb::Ls(_))));
}

/// What is no split is refused before anything is written; every flag of `add` is required.
#[test]
fn what_is_no_split_is_refused_at_the_door() {
    for (date, ratio) in
        [("2020-08-31", "1"), ("2020-08-31", "0"), ("2020-02-30", "4"), ("31.08.2020", "4")]
    {
        let line = [
            "add",
            "--venue",
            "dukascopy",
            "--symbol",
            "AAPLUSUSD",
            "--date",
            date,
            "--ratio",
            ratio,
        ];
        let verb = parse(&line).expect("the grammar takes it");
        assert!(judge(&verb).is_err(), "{date} {ratio}");
    }
    assert!(
        parse(&["add", "--venue", "dukascopy", "--symbol", "AAPLUSUSD", "--date", "2020-08-31"])
            .is_err()
    );
    let rm = ["rm", "--venue", "dukascopy", "--symbol", "AAPLUSUSD", "--date", "yesterday"];
    assert!(judge(&parse(&rm).expect("parses")).is_err());
}

/// What the datahub's watch found: a split on one venue, a reverse split on another.
fn open_warnings() -> Vec<SplitWarning> {
    let warning = |venue: &str, symbol: &str, new_shares, old_shares, before, after| SplitWarning {
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        date: "2026-11-20".to_string(),
        new_shares,
        old_shares,
        price_before: before,
        price_after: after,
    };
    vec![
        warning("dukascopy", "NVDA.CFD", 10, 1, 1400.0, 140.0),
        warning("ig", "GE.CFD", 1, 8, 12.0, 96.0),
    ]
}

fn warnings_flags(venue: Option<&str>, addr: &str) -> SplitsWarningsFlags {
    SplitsWarningsFlags { venue: venue.map(str::to_string), addr: Some(addr.to_string()) }
}

/// Each open warning prints as the desktop's one sentence (`SplitWarning::message`), `--venue` narrows them, and none
/// says so in words.
#[test]
fn the_warnings_print_in_the_desktops_words() {
    assert!(matches!(parse(&["warnings"]), Ok(SplitsVerb::Warnings(_))));
    assert!(matches!(
        parse(&["warnings", "--venue", "ig", "--addr", "127.0.0.1:1"]),
        Ok(SplitsVerb::Warnings(_))
    ));
    let warnings = open_warnings();
    let all = warning_lines(&warnings, None);
    assert_eq!(all, [warnings[0].message(), warnings[1].message()]);
    assert!(
        all[0].starts_with("NVDA.CFD: looks like a 10-for-1 split on 2026-11-20"),
        "{}",
        all[0]
    );
    assert!(all[0].ends_with("run vike-cli data catalog refresh --venue dukascopy"), "{}", all[0]);
    assert_eq!(warning_lines(&warnings, Some("ig")), [warnings[1].message()]);
    assert_eq!(warning_lines(&warnings, Some("oanda")), ["no open split warnings"]);
    assert_eq!(warning_lines(&[], None), ["no open split warnings"]);
}

/// Against a datahub that serves the read: one `SplitWarnings` request, and no settings directory is needed; an OLDER
/// one is refused in the desktop's words, naming the CLI, with nothing sent; a REAL one with nothing watched answers
/// none; an unreachable one is the connect rung.
#[test]
fn the_verb_asks_the_datahub_and_refuses_an_older_one() {
    use vike_datahub_client::proto::{Request, Response};
    use vike_datahub_client::test_support::spawn_fake_peer;

    let (addr, received, _) = spawn_fake_peer(vec![FEATURE_SPLIT_WARNINGS.to_string()], |r| {
        matches!(r, Request::SplitWarnings).then(|| Response::SplitWarnings(open_warnings()))
    });
    let verb = SplitsVerb::Warnings(warnings_flags(Some("dukascopy"), &addr.to_string()));
    let lines = execute(verb, None, None, None).expect("answered, with no settings directory");
    assert_eq!(lines, [open_warnings()[0].message()]);
    assert_eq!(received.recv().expect("the fake reports").len(), 1);

    let (addr, received, _) = spawn_fake_peer(vec!["load_bars".to_string()], |_| None);
    let err = warnings(&warnings_flags(None, &addr.to_string()), None, None).expect_err("older");
    assert_eq!(err.exit, crate::exit::Exit::Refused);
    assert!(err.message.starts_with("this datahub is older than the CLI"), "{}", err.message);
    assert!(err.message.contains(FEATURE_SPLIT_WARNINGS), "{}", err.message);
    assert!(received.recv().expect("the fake reports").is_empty(), "nothing was sent");

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let real = listener.local_addr().expect("addr").to_string();
    let store: std::sync::Arc<dyn vike_data::HistoryStore + Send + Sync> =
        std::sync::Arc::new(vike_data::HistoryStoreMemory::new());
    std::thread::spawn(move || {
        let _ = vike_datahub::serve(listener, store);
    });
    let lines = warnings(&warnings_flags(None, &real), None, None).expect("a real datahub answers");
    assert_eq!(lines, ["no open split warnings"]);

    let err =
        warnings(&warnings_flags(None, "127.0.0.1:1"), None, None).expect_err("nothing listens");
    assert_eq!(err.exit, crate::exit::Exit::Connect);
}
