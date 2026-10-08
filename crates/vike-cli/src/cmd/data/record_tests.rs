use std::collections::BTreeMap;

use vike_secrets::profile_store::{ProfileRow, RecorderRow};

use super::*;

fn parse_of(argv: &[&str]) -> Result<Args, String> {
    let owned: Vec<String> = argv.iter().map(|s| (*s).to_string()).collect();
    parse(&owned, None)
}

fn sub(ord: i64, venue: &str, family: Option<&str>, symbols: Option<&[&str]>) -> SubscriptionRow {
    SubscriptionRow {
        ord,
        venue: venue.to_string(),
        family: family.map(str::to_string),
        symbols: symbols
            .map(|s| toml_string_array(&s.iter().map(|x| (*x).to_string()).collect::<Vec<_>>())),
        backfill: None,
        note: None,
    }
}

fn profile(name: &str, active: bool, subs: Vec<SubscriptionRow>) -> StoredProfile {
    StoredProfile {
        row: ProfileRow { name: name.to_string(), kind: ProfileKind::Recorder, active, note: None },
        mounts: Vec::new(),
        params: BTreeMap::new(),
        settings: BTreeMap::new(),
        recorder: Some(RecorderBody {
            row: RecorderRow {
                store: "market_data/hist".to_string(),
                interval_secs: None,
                min_parts: None,
                target_mb: None,
                max_merge_rows: None,
                retention_days: None,
                alert_webhooks: None,
                alert_repeat_secs: None,
                alert_series_prefix: None,
                note: None,
            },
            subscriptions: subs,
        }),
    }
}

fn add_args(spec: &str) -> Args {
    let mut a = parse_of(&["add", spec]).expect("a well-formed add line");
    a.addr = "127.0.0.1:1".to_string();
    a
}

/// **`@` MEANS A FAMILY HERE and means nothing one module up.** The two parsers answer the same
/// token differently ON PURPOSE — see [`parse_spec`]'s doc — so this pins BOTH readings rather
/// than only the one this file implements.
#[test]
fn the_at_marker_is_a_family_here_and_a_symbol_one_module_up() {
    assert_eq!(
        parse_spec("polymarket:@btc-updown-5m"),
        Ok(Spec {
            venue: "polymarket".to_string(),
            what: What::Family("btc-updown-5m".to_string()),
        })
    );
    assert_eq!(
        parse_spec("binance:BTCUSDT.P"),
        Ok(Spec { venue: "binance".to_string(), what: What::Symbol("BTCUSDT.P".to_string()) })
    );
    // The hyperliquid spelling the sibling parser exists to protect: `@107` is a real
    // instrument THERE and is a FAMILY here, which is the disagreement worth pinning.
    assert_eq!(
        parse_spec("hyperliquid:@107"),
        Ok(Spec { venue: "hyperliquid".to_string(), what: What::Family("107".to_string()) })
    );
    let empty = parse_spec("hyperliquid:@").expect_err("a bare marker names nothing");
    assert!(empty.contains("EMPTY family"), "{empty}");
}

/// A three-part spec is a SERIES and is refused with where an interval belongs; the two typo
/// shapes a symbol may never have are refused by name.
#[test]
fn the_spec_grammar_refuses_a_series_and_the_two_typo_shapes() {
    let series = parse_spec("binance:BTCUSDT:1h").expect_err("a series is not a subscription");
    assert!(series.contains("INTERVAL"), "{series}");
    let comma = parse_spec("binance:BTCUSDT,ETHUSDT").expect_err("a comma is a typo here");
    assert!(comma.contains("comma"), "{comma}");
    assert!(comma.contains("two `add` runs"), "…and says what to do instead: {comma}");
    let space = parse_spec("binance:BTC USDT").expect_err("whitespace is a quoting mistake");
    assert!(space.contains("shell quoting"), "{space}");
    let one = parse_spec("binance").expect_err("one part is not a spec");
    assert!(one.contains("VENUE:@FAMILY"), "{one}");
}

/// **`--lane` is REFUSED BY NAME, and the reserved word is named in the refusal.** Ruling 1 is
/// the one an operator is most likely to trip over, because the word means something else on
/// the sibling verb in this same group.
#[test]
fn the_grain_flags_are_refused_by_name_and_the_reserved_word_is_stated() {
    for flag in ["--lane", "--stream"] {
        let why = parse_of(&["add", "binance:BTCUSDT", flag, "trades"])
            .expect_err("this grain does not exist");
        assert!(why.contains(RESERVED_GRAIN_FLAG), "it must name the reserved word: {why}");
        assert!(why.contains("Stream::ALL"), "…and the measurement behind it: {why}");
        assert!(!why.contains("unknown option"), "a real word is not an unknown one: {why}");
    }
}

/// `--addr` is ACCEPTED and refused by name — ruling 3 — and its value is consumed, so the flag
/// cannot swallow the next token and report a different error.
#[test]
fn addr_is_accepted_and_refused_by_name_on_every_verb() {
    for argv in [
        vec!["ls", "--addr", "1.2.3.4:9"],
        vec!["add", "binance:BTCUSDT", "--addr", "1.2.3.4:9"],
        vec!["rm", "binance:BTCUSDT", "--addr=1.2.3.4:9"],
    ] {
        let why = parse_of(&argv).expect_err("the remote half is not built");
        assert!(why.contains("designed and not built"), "{argv:?}: {why}");
        assert!(why.contains("0081"), "…and points at the ruling: {why}");
    }
    // A dangling `--addr` is still the ordinary dangling-flag error rather than this one.
    let dangling = parse_of(&["ls", "--addr"]).expect_err("no value");
    assert!(dangling.contains("requires a value"), "{dangling}");
}

/// Each verb's own flags are refused on the others BY NAME, saying where they belong.
#[test]
fn a_flag_on_the_wrong_verb_names_the_verb_it_belongs_to() {
    for (argv, flag, belongs) in [
        (vec!["add", "b:S", "--profiles"], "--profiles", "ls"),
        (vec!["ls", "--backfill", "off"], "--backfill", "add"),
        (vec!["ls", "--note", "x"], "--note", "add"),
        (vec!["add", "b:S", "--ord", "1"], "--ord", "rm"),
    ] {
        let why = parse_of(&argv).expect_err("an inapplicable flag");
        assert!(why.contains(flag), "{argv:?}: {why}");
        assert!(why.contains(belongs), "{argv:?}: it must name where it belongs: {why}");
    }
    let dry = parse_of(&["ls", "--dry-run"]).expect_err("ls writes nothing");
    assert!(dry.contains("writes nothing"), "{dry}");
    let fmt = parse_of(&["add", "b:S", "--json"]).expect_err("a write renders no document");
    assert!(fmt.contains("ls --format json"), "…and names the machine form: {fmt}");
}

/// The verb roster is RENDERED by both the missing-verb and unknown-verb refusals, and the
/// deleted `status` verb is named rather than left to the unknown arm.
#[test]
fn the_roster_is_rendered_and_the_deleted_verb_is_named() {
    let none = parse(&[], None).expect_err("a verb is required");
    for v in VERBS {
        assert!(none.contains(v.as_str()), "the roster must name {}: {none}", v.as_str());
    }
    let unknown = parse_of(&["frobnicate"]).expect_err("not a verb");
    assert!(unknown.contains("unknown"), "{unknown}");
    let status = parse_of(&["status"]).expect_err("there is no status verb");
    assert!(status.contains("columns are on"), "{status}");
    assert!(!status.contains("unknown"), "a deleted verb is not an unknown one: {status}");
}

/// The usage page leaves no placeholder unexpanded and documents every verb.
#[test]
fn the_usage_page_expands_and_names_every_verb() {
    let page = usage();
    assert!(!page.contains('{'), "an unsubstituted placeholder survived: {page}");
    for v in VERBS {
        assert!(
            page.lines().any(|l| l.starts_with(&format!("  {}", v.as_str()))),
            "`{}` has no block of its own: {page}",
            v.as_str()
        );
    }
    assert!(page.contains("NEXT RESTART"), "ruling 2 is the thing to read here: {page}");
}

/// **Ruling 4's three noes are three DIFFERENT messages**, because they name three different
/// next commands — and the first splits again on whether the database file is even there.
#[test]
fn each_no_names_its_own_next_command() {
    let db = std::path::Path::new("/nope/settings/db/vike.db");
    let none = resolve_target(db, &Profiles::none(), None).expect_err("no store");
    assert!(none.msg.contains("secrets migrate"), "{}", none.msg);

    let empty = Profiles::from_rows(Vec::new());
    let stored = resolve_target(db, &empty, None).expect_err("no recorder profile");
    assert!(stored.msg.contains("config mirror --recorder"), "{}", stored.msg);
    assert!(!stored.msg.contains("secrets migrate"), "a DIFFERENT no: {}", stored.msg);

    let inactive = Profiles::from_rows(vec![profile("a", false, Vec::new())]);
    let none_active = resolve_target(db, &inactive, None).expect_err("none selected");
    assert!(none_active.msg.contains("--profile NAME"), "{}", none_active.msg);
    assert!(none_active.msg.contains("Recorder profiles in this store: a."), "{}", none_active.msg);

    // …and the active row is what a bare line resolves to.
    let active =
        Profiles::from_rows(vec![profile("a", false, Vec::new()), profile("b", true, Vec::new())]);
    let got = resolve_target(db, &active, None).expect("the active row answers");
    assert_eq!(got.row.name, "b");
    // A NAMED profile overrides it.
    let named = resolve_target(db, &active, Some("a")).expect("a named profile answers");
    assert_eq!(named.row.name, "a");
    let missing = resolve_target(db, &active, Some("zzz")).expect_err("no such profile");
    assert!(missing.msg.contains("Recorder profiles in this store: a, b."), "{}", missing.msg);
}

/// **THE NOTE SURVIVES.** `render_recorder_toml` never emits the `note` column, so a
/// read-modify-write that went out through the rendered document would drop every note on the
/// profile. This pins that the planned body carries them, which is the property the whole
/// row-based write path exists for.
#[test]
fn a_planned_write_carries_every_note_the_toml_rendering_would_drop() {
    let mut existing = sub(0, "polymarket", Some("btc-updown-5m"), None);
    existing.note = Some("the family the CI box has recorded since June".to_string());
    let target = profile("default", true, vec![existing.clone()]);
    let mut args = add_args("binance:BTCUSDT.P");
    args.note = Some("added by hand".to_string());
    let plan = plan_write(&args, args.spec.as_ref().unwrap(), &target).expect("a legal add");
    let body = plan.stored.recorder.as_ref().expect("the body rides through");
    assert_eq!(body.subscriptions[0].note.as_deref(), existing.note.as_deref());
    assert_eq!(body.subscriptions[1].note.as_deref(), Some("added by hand"));
    // ...and the rendering genuinely does NOT carry it, which is why the write may not go
    // through it. This half is what makes the assertion above a fact rather than a habit.
    assert!(
        !render_recorder_toml(body).contains("added by hand"),
        "if the renderer ever learns `note`, this test's REASON has changed"
    );
}

/// A duplicate `add` is refused by name, and a SECOND family row on one venue is refused with
/// the index that would otherwise refuse it inside the write transaction.
#[test]
fn add_refuses_a_duplicate_and_a_second_family_on_one_venue() {
    let target = profile(
        "default",
        true,
        vec![
            sub(0, "polymarket", Some("btc-updown-5m"), None),
            sub(1, "binance", None, Some(&["BTCUSDT.P"])),
        ],
    );
    let args = add_args("binance:BTCUSDT.P");
    let why = plan_write(&args, args.spec.as_ref().unwrap(), &target).expect_err("a duplicate");
    assert!(why.msg.contains("already a subscription"), "{}", why.msg);
    assert!(why.msg.contains("ord 1"), "…naming which row: {}", why.msg);

    let args = add_args("polymarket:@eth-updown-5m");
    let why = plan_write(&args, args.spec.as_ref().unwrap(), &target).expect_err("two families");
    assert!(why.msg.contains("subscription_one_family_per_venue"), "{}", why.msg);
    assert!(why.msg.contains("NOTHING was written"), "{}", why.msg);
}

/// **Ruling 5: an ambiguous `rm` REFUSES and prints every candidate's `ord`**, and `--ord N`
/// resolves it. Two symbols-based rows on one venue are legal, which is the whole reason.
#[test]
fn rm_refuses_an_ambiguous_match_and_ord_resolves_it() {
    let mut rows = vec![
        sub(0, "binance", None, Some(&["BTCUSDT.P"])),
        sub(1, "binance", None, Some(&["BTCUSDT.P"])),
    ];
    let spec = parse_spec("binance:BTCUSDT.P").expect("a spec");
    let why = remove_one(&mut rows, &spec, None, "default").expect_err("two candidates");
    assert!(why.msg.contains("matches 2 subscriptions"), "{}", why.msg);
    assert!(why.msg.contains("--ord N"), "…and how to pick: {}", why.msg);
    assert!(why.msg.contains("ord 0") && why.msg.contains("ord 1"), "both: {}", why.msg);
    assert_eq!(rows.len(), 2, "NOTHING was removed");

    let removed = remove_one(&mut rows, &spec, Some(1), "default").expect("--ord picks");
    assert_eq!(removed.ord, 1);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].ord, 0, "the OTHER row survives");
}

/// A symbol that is one of SEVERAL on a row is a NEAR MISS, never a match — removing the whole
/// row would be a silent over-removal, and the refusal says which row it saw.
#[test]
fn a_symbol_inside_a_multi_symbol_row_is_reported_not_removed() {
    let mut rows = vec![sub(0, "binance", None, Some(&["BTCUSDT.P", "ETHUSDT.P"]))];
    let spec = parse_spec("binance:ETHUSDT.P").expect("a spec");
    let why = remove_one(&mut rows, &spec, None, "default").expect_err("a near miss");
    assert!(why.msg.contains("names other symbols too"), "{}", why.msg);
    assert!(why.msg.contains("ord 0"), "…naming the row: {}", why.msg);
    assert_eq!(rows.len(), 1, "NOTHING was removed");
}

/// A family `rm` removes the family row and leaves the symbols rows alone.
#[test]
fn rm_removes_the_row_a_family_spec_names() {
    let mut rows = vec![
        sub(0, "polymarket", Some("btc-updown-5m"), None),
        sub(1, "polymarket", None, Some(&["0xdead"])),
    ];
    let spec = parse_spec("polymarket:@btc-updown-5m").expect("a spec");
    let removed = remove_one(&mut rows, &spec, None, "default").expect("one match");
    assert_eq!(removed.ord, 0);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].ord, 1);

    let spec = parse_spec("polymarket:@nope").expect("a spec");
    let why = remove_one(&mut rows, &spec, None, "default").expect_err("no match");
    assert!(why.msg.contains("no subscription"), "{}", why.msg);
}

/// **The three-row degrade table of the venue check**, as the pure half of it: the reader.
/// An EMPTY advertisement is "cannot say" and not "records nothing", which is what makes this
/// client forward-compatible with a datahub that predates the advertisement.
#[test]
fn an_empty_advertisement_says_nothing_rather_than_saying_no() {
    assert!(advertised_rec_venues(&[]).is_empty());
    assert!(
        advertised_rec_venues(&["market_data".to_string(), "md_venue=binance".to_string()])
            .is_empty(),
        "an md_venue entry describes the LIVE plane and must not be read as a recordable one"
    );
    assert_eq!(
        advertised_rec_venues(&[
            "rec_venue=binance".to_string(),
            "rec_venue= polymarket ".to_string(),
            "rec_venue=".to_string(),
        ]),
        vec!["binance".to_string(), "polymarket".to_string()],
        "trimmed, and an EMPTY value dropped"
    );
    let refusal = unrecordable_refusal("okx", &["binance".to_string()]);
    assert!(refusal.contains("`okx`"), "{refusal}");
    assert!(refusal.contains("NEXT RESTART"), "it must state the consequence: {refusal}");
}

/// The `symbols` column is TOML TEXT, and a column that does not parse is REPORTED rather than
/// read as empty — an unparseable row is a row `rm` could otherwise never reach.
#[test]
fn the_symbols_column_is_toml_text_and_a_broken_one_is_reported() {
    assert_eq!(parse_symbols("[\"A\", \"B\"]"), Ok(vec!["A".to_string(), "B".to_string()]));
    assert_eq!(parse_symbols("[]"), Ok(Vec::new()));
    assert!(parse_symbols("[\"A\"").is_err(), "an unbalanced array is an error");
    assert!(parse_symbols("[1]").is_err(), "a non-string element is an error");
    let mut rows = vec![sub(0, "binance", None, None)];
    rows[0].symbols = Some("[\"A\"".to_string());
    let spec = parse_spec("binance:A").expect("a spec");
    let why = remove_one(&mut rows, &spec, None, "default").expect_err("an unreadable row");
    assert!(why.msg.contains("cannot be matched"), "{}", why.msg);
}

/// The `ls` renderings: the table leads with `source:` and states ruling 2, and the JSON form
/// carries `symbols` as an ARRAY rather than the stored text.
#[test]
fn ls_renders_the_source_the_rows_and_the_restart_disclosure() {
    let target = profile(
        "default",
        true,
        vec![
            sub(0, "polymarket", Some("btc-updown-5m"), None),
            sub(1, "binance", None, Some(&["BTCUSDT.P"])),
        ],
    );
    let table = render_subscriptions("/s/db/vike.db", &target, Render::Table);
    assert!(table.starts_with("source: /s/db/vike.db"), "{table}");
    assert!(table.contains("profile: default (active)"), "{table}");
    assert!(table.contains("store: market_data/hist"), "{table}");
    assert!(table.contains("family btc-updown-5m"), "{table}");
    assert!(table.contains("NEXT RESTART"), "{table}");

    let doc: serde_json::Value =
        serde_json::from_str(&render_subscriptions("/s/db/vike.db", &target, Render::Json))
            .expect("the json form is one document");
    assert_eq!(doc["profile"], "default");
    assert_eq!(doc["subscriptions"][1]["symbols"][0], "BTCUSDT.P");
    assert!(doc["subscriptions"][0]["symbols"].is_null(), "a family row has no symbols");

    let listing =
        render_profiles("/s/db/vike.db", &Profiles::from_rows(vec![target]), Render::Table);
    assert!(listing.contains("default"), "{listing}");
    assert!(listing.contains("yes"), "the active column: {listing}");
}
