use super::*;
use std::assert_matches;

/// Every flag name is unique. A duplicate renders twice and the second silently wins.
#[test]
fn flag_names_are_unique() {
    let mut seen = std::collections::BTreeSet::new();
    for f in FLAGS.iter() {
        assert!(seen.insert(f.long), "{} appears twice in FLAGS", f.long);
    }
}

/// Every flag name is spelled as a long flag.
#[test]
fn flag_names_are_long_flags() {
    for f in FLAGS.iter() {
        assert!(f.long.starts_with("--"), "{} is not spelled as a long flag", f.long);
    }
}

/// ⚠ A flag that takes a value must carry a SAMPLE, and a bare switch must not.
///
/// The sample is what retires the two hand-written arity tables this export exists to replace,
/// so a row with no sample cannot drive them and the retirement stalls on exactly that row.
#[test]
fn arity_and_sample_agree() {
    for f in FLAGS.iter() {
        match f.value() {
            Value::Required => {
                let ok = match f.sample {
                    Some(s) => !s.is_empty(),
                    None => false,
                };
                assert!(ok, "{} takes a value and carries no sample", f.long);
            }
            Value::None => {
                let ok = match f.sample {
                    Some(s) => s.is_empty(),
                    None => true,
                };
                assert!(ok, "{} is a bare switch and carries the sample {:?}", f.long, f.sample);
            }
        }
    }
}

/// ⚠ **Two-way, in the shape of Kraken's `every_command_is_classified_exactly_once`:** every flag the
/// plane's `clap` command declares, hidden ones included, has exactly one row, and every row names a
/// real flag. `--help` and `--version` are the flags `clap` adds when it builds the command.
#[test]
fn every_flag_of_the_plane_is_classified_exactly_once() {
    let plane = crate::cmd::backtest::command();
    let mut declared: std::collections::BTreeSet<String> = plane
        .get_arguments()
        .chain(plane.get_subcommands().flat_map(clap::Command::get_arguments))
        .filter_map(|arg| arg.get_long())
        .map(|long| format!("--{long}"))
        .collect();
    declared.insert("--help".to_string());
    declared.insert("--version".to_string());
    let rows: Vec<String> = FLAGS.iter().map(|f| f.long.to_string()).collect();
    let classified: std::collections::BTreeSet<String> = rows.iter().cloned().collect();
    assert_eq!(rows.len(), classified.len(), "a flag has two rows");
    assert_eq!(
        declared.difference(&classified).collect::<Vec<_>>(),
        Vec::<&String>::new(),
        "these flags are declared on the command and have no row in `FLAGS`"
    );
    assert_eq!(
        classified.difference(&declared).collect::<Vec<_>>(),
        Vec::<&String>::new(),
        "these rows of `FLAGS` name no flag of the command"
    );
}

/// A row carries a `value_name` only where the published spelling differs from the `clap` field's;
/// a copy of the field's own would rot the day the field changes.
#[test]
fn a_row_spells_a_value_name_only_where_clap_spells_it_differently() {
    for f in FLAGS.iter() {
        if let Some(name) = f.value_name {
            assert_ne!(
                Some(name.to_string()),
                f.clap_value_name(),
                "{}: drop this `value_name`, the field declares the same one",
                f.long
            );
        }
    }
}

/// A bare switch has no earlier value to refuse or keep.
#[test]
fn a_bare_switch_is_idempotent() {
    for f in FLAGS.iter() {
        if f.value() == Value::None {
            assert_eq!(
                f.repeat(),
                Repeat::Idempotent,
                "{} is a bare switch, so its repeat cannot be {:?}",
                f.long,
                f.repeat()
            );
        }
    }
}

/// `cli.json`'s `commands` covers every verb `--help` lists, and each is walked rather than left as
/// the root's unit row (`commands`'s `whole_command`).
#[test]
fn every_verb_is_walked() {
    let files = rendered_files();
    let doc: serde_json::Value =
        serde_json::from_str(files.get(CLI_JSON).expect("cli.json is rendered")).expect("JSON");
    let rows = doc["commands"].as_array().expect("commands is an array");
    for (verb, _) in crate::COMMANDS {
        let under: Vec<&serde_json::Value> =
            rows.iter().filter(|row| row["path"][0] == *verb).collect();
        assert!(!under.is_empty(), "`commands` has no row for the verb `{verb}`");
        let walked = under.len() > 1 || !under[0]["args"].as_array().expect("args").is_empty();
        assert!(walked, "`{verb}` is left as the root's unit row");
    }
    for row in rows {
        for arg in row["args"].as_array().expect("args") {
            assert!(arg["name"].is_string(), "an argument with no name: {arg}");
            assert!(arg.get("help").is_some(), "an argument with no help field: {arg}");
        }
    }
    assert!(
        !rows.iter().any(|row| ["sweep", "walkforward", "study"]
            .iter()
            .any(|retired| row["path"][0] == *retired)),
        "a retired verb is published: help must not advertise a verb that refuses"
    );
}

/// ⚠ **An unbuilt flag accepts NOWHERE**, and must say where its refusal reaches instead.
///
/// Written as a test because the gathered data had these the other way round: the four unbuilt
/// renderers listed all eight reading sub-verbs in `applies_to`, which records where the REFUSAL
/// reaches. A consumer reading that field as "accepts" would have published four flags that
/// accept nothing as accepted everywhere.
#[test]
fn an_unbuilt_flag_accepts_nowhere_and_says_where_its_refusal_reaches() {
    for f in FLAGS.iter() {
        if f.status == Status::Unbuilt {
            assert!(
                f.applies_to.is_empty(),
                "unbuilt {} claims to be accepted on {:?}",
                f.long,
                f.applies_to
            );
            assert!(
                !f.refusal_reaches.is_empty(),
                "unbuilt {} says nowhere its refusal reaches",
                f.long
            );
        }
    }
}

/// A flag that ships is accepted somewhere.
#[test]
fn a_shipping_flag_is_accepted_somewhere() {
    for f in FLAGS.iter() {
        if f.status == Status::Ships {
            assert!(!f.accepted_on().is_empty(), "{} ships and is accepted nowhere", f.long);
        }
    }
}

/// `--help` is published on every sub-verb, because `clap` adds it to each: its row said eight
/// reading verbs and was five short (`run`, `templates`, `script-api`, `script-check`, `prune`).
#[test]
fn help_is_accepted_on_every_sub_verb() {
    let help = FLAGS.iter().find(|f| f.long == "--help").expect("the `--help` row");
    assert_eq!(help.accepted_on(), sub_verbs());
}

/// `--version` is published on every sub-verb, because `vike_utility::cli`'s `propagate_version`
/// gives each one: the `--help` row published "unknown argument: --version", and no row said it.
#[test]
fn version_is_accepted_on_every_sub_verb() {
    let version = FLAGS.iter().find(|f| f.long == "--version").expect("the `--version` row");
    assert_eq!(version.accepted_on(), sub_verbs());
}

/// Every sub-verb a row names is a real one.
#[test]
fn every_named_sub_verb_is_real() {
    for f in FLAGS.iter() {
        for v in f.applies_to.iter().chain(f.refusal_reaches.iter()) {
            assert!(
                sub_verbs().iter().any(|sub| sub == v),
                "{} names the sub-verb {:?}, which does not exist",
                f.long,
                v
            );
        }
    }
}

/// Every `roster_id` a flag names resolves to a declared roster.
#[test]
fn every_named_roster_resolves() {
    for f in FLAGS.iter() {
        if let Some(id) = f.roster_id {
            assert!(
                ROSTERS.iter().any(|r| r.id == id),
                "{} names the roster {:?}, which is not in ROSTERS",
                f.long,
                id
            );
        }
    }
}

/// Roster ids are unique.
#[test]
fn roster_ids_are_unique() {
    let mut seen = std::collections::BTreeSet::new();
    for r in ROSTERS.iter() {
        assert!(seen.insert(r.id), "roster {} is declared twice", r.id);
    }
}

/// ⚠ A roster nothing derives must ADMIT that in writing, so an ungated hand copy is visible
/// rather than assumed away.
#[test]
fn an_underived_roster_admits_it() {
    for r in ROSTERS.iter() {
        if r.derived_from.is_none() {
            let ok = match r.admission {
                Some(a) => !a.is_empty(),
                None => false,
            };
            assert!(ok, "roster {} is derived from nothing and admits nothing", r.id);
        }
    }
}

/// ⚠ **The two `[data]` rosters are DERIVED from the engine's own arrays, and this holds each one
/// equal to the validator an operator actually meets.**
///
/// `gap_dispositions` and `universe_modes` were HAND COPIES of `OnGap::NAMES` and
/// `UniverseMode::NAMES` while those lived in an engine crate this crate cannot link; they sit in
/// `vike_model::run_request::gap` now, so each row's `members` IS the array (not a literal that
/// merely equals it), and `derived_from` says so. Three things are held here:
///
/// 1. the row IS the array, and says it is derived from it — a literal planted back in the row
///    fails the first assertions;
/// 2. every member passes the CLI validator, in upper case too (the rows claim `ascii_ci`);
/// 3. the validator's REFUSAL renders the set the row publishes.
///
/// **(3) compares by RENDERING the refusal**, because that message prints the roster joined by
/// `" | "` — so extracting that segment and comparing it as a whole string pins membership,
/// ORDER and LENGTH at once. Asking whether the message merely CONTAINS the row would pass a
/// row that is short, since a prefix of a longer roster is a substring of it, and a short
/// published roster is exactly the `all_subcommands` failure this table has already had.
///
/// The ENGINE direction (the profile loader resolves every member) is
/// `crates/vike-cli/tests/cli_verbs/backtest_flags_schema.rs`'s
/// `every_data_roster_member_is_a_value_the_profile_loader_resolves`, where the vike-backtest
/// dev-dependency reaches.
#[test]
fn the_data_rosters_are_exactly_what_the_cli_validator_accepts() {
    // The segment of a refusal between "not one of " and the end of that sentence IS the
    // const, rendered. Both messages are built as `"… is not one of {}. <explanation>"`.
    let rendered_roster = |msg: &str| -> String {
        let tail = msg
            .split_once("not one of ")
            .unwrap_or_else(|| panic!("the refusal must name the set: {msg}"))
            .1;
        tail.split_once(". ")
            .unwrap_or_else(|| panic!("the set, then the sentence that explains it: {msg}"))
            .0
            .to_string()
    };
    let row = |id: &str| {
        ROSTERS
            .iter()
            .find(|r| r.id == id)
            .unwrap_or_else(|| panic!("the `{id}` roster row exists"))
    };

    let gaps = row("gap_dispositions");
    assert_eq!(
        gaps.members,
        &vike_model::run_request::gap::OnGap::NAMES[..],
        "gap_dispositions must publish `OnGap::NAMES` itself"
    );
    assert_eq!(gaps.derived_from, Some("vike_model::run_request::gap::OnGap::NAMES"));
    for m in gaps.members {
        assert!(
            crate::cmd::data::on_gap_override(m).is_ok(),
            "gap_dispositions publishes {m:?} and `--on-gap` refuses it"
        );
        // The row claims `ascii_ci`, so the upper-case spelling must be accepted too.
        assert!(
            crate::cmd::data::on_gap_override(&m.to_ascii_uppercase()).is_ok(),
            "gap_dispositions claims match_rule {:?} and `--on-gap` refuses {:?}",
            gaps.match_rule,
            m.to_ascii_uppercase()
        );
    }
    let e = crate::cmd::data::on_gap_override("nope").expect_err("not a disposition");
    assert_eq!(
        rendered_roster(&e),
        gaps.members.join(" | "),
        "the `gap_dispositions` row and the refusal crates/vike-cli/src/cmd/data.rs's \
             `on_gap_override` renders must agree in MEMBERSHIP and ORDER — this row is published \
             in cli.json"
    );

    let universe = row("universe_modes");
    assert_eq!(
        universe.members,
        &vike_model::run_request::gap::UniverseMode::NAMES[..],
        "universe_modes must publish `UniverseMode::NAMES` itself"
    );
    assert_eq!(universe.derived_from, Some("vike_model::run_request::gap::UniverseMode::NAMES"));
    for m in universe.members {
        assert!(
            crate::cmd::data::universe_override(m).is_ok(),
            "universe_modes publishes {m:?} and `--universe` refuses it"
        );
        assert!(
            crate::cmd::data::universe_override(&m.to_ascii_uppercase()).is_ok(),
            "universe_modes claims match_rule {:?} and `--universe` refuses {:?}",
            universe.match_rule,
            m.to_ascii_uppercase()
        );
    }
    let e = crate::cmd::data::universe_override("nope").expect_err("not a membership rule");
    assert_eq!(
        rendered_roster(&e),
        universe.members.join(" | "),
        "the `universe_modes` row and the refusal crates/vike-cli/src/cmd/data.rs's \
             `universe_override` renders must agree in MEMBERSHIP and ORDER"
    );
}

/// `selector_doc_table_forms` is the one roster whose source is PROSE: the `//!` table at the head of
/// `crates/vike-cli/src/cmd/backtest/read/selector.rs`, which no code reads at run time. So its members stay
/// written here, and this holds each one's spelling (the example after `e.g.`, or the member itself)
/// to that table's rows in order, read at RUN time like `selector_tests.rs`'s
/// `the_doc_table_and_the_refusal_agree` (the mirror may withhold a file, so never `include_str!`).
#[test]
fn the_selector_doc_table_roster_is_the_module_doc_table() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/cmd/backtest/read/selector.rs");
    let src = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let table: Vec<&str> = src
        .lines()
        .filter_map(|line| line.strip_prefix("//! | `"))
        .filter_map(|row| row.split_once('`').map(|(spelling, _)| spelling))
        .collect();
    let row = ROSTERS
        .iter()
        .find(|r| r.id == "selector_doc_table_forms")
        .expect("the `selector_doc_table_forms` roster row exists");
    let spelled: Vec<&str> = row
        .members
        .iter()
        .map(|member| member.rsplit_once("e.g. ").map_or(*member, |(_, example)| example))
        .collect();
    assert_eq!(spelled, table, "the roster and the selector module's `//!` table disagree");
}

/// ⚠ The match rule must be one this export defines — `open_tail` above all, because a roster
/// carrying it may not be rendered as an exhaustive list.
#[test]
fn match_rules_are_known() {
    for r in ROSTERS.iter() {
        assert_matches!(
            r.match_rule,
            "exact" | "ascii_ci" | "open_tail",
            "roster {} carries the unknown match rule {:?}",
            r.id,
            r.match_rule
        );
    }
}

/// `validated_by` is one of the three values a renderer knows.
#[test]
fn validated_by_is_known() {
    for f in FLAGS.iter() {
        assert_matches!(
            f.validated_by,
            "cli" | "engine" | "none",
            "{} carries the unknown validated_by {:?}",
            f.long,
            f.validated_by
        );
    }
}

/// `default.kind` is one of the four precedence classes, `far_side` included.
#[test]
fn default_kinds_are_known() {
    for f in FLAGS.iter() {
        assert_matches!(
            f.default_kind,
            "schema" | "implied" | "far_side" | "none",
            "{} carries the unknown default kind {:?}",
            f.long,
            f.default_kind
        );
    }
}

/// ⚠ A CONDITIONAL default must name the flag it is conditional on, and that flag must exist.
///
/// Stated because the flat rendering is the dangerous one: `--strategy`'s implied `rhai` applies
/// only under `--script`, and a table printing it unconditionally publishes a default the CLI
/// supplies half the time.
#[test]
fn a_conditional_default_names_a_real_flag() {
    for f in FLAGS.iter() {
        if let Some(on) = f.default_conditional_on {
            assert!(
                FLAGS.iter().any(|o| o.long == on),
                "{}'s default is conditional on {:?}, which is not a flag",
                f.long,
                on
            );
        }
    }
}

/// Whether `symbol::field` is in `file`: the struct (or the enum variant) is there and declares the
/// field. `field` is `None` for a hand-built argument set, whose citation names the builder.
fn citation_resolves(file: &str, symbol: &str, field: Option<&str>) -> Result<(), String> {
    let path = format!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../{}"), file);
    let src = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
    let (item, member) = match symbol.split_once("::") {
        Some((item, member)) => (item, Some(member)),
        None => (symbol, None),
    };
    if member == Some("augment_args") {
        return if src.contains(&format!("impl clap::Args for {item} {{")) {
            Ok(())
        } else {
            Err(format!("{file} builds no arguments for `{item}`"))
        };
    }
    let start = ["struct ", "enum "]
        .iter()
        .find_map(|keyword| src.find(&format!("{keyword}{item} {{")))
        .ok_or_else(|| format!("{file} declares no `{item}`"))?;
    let mut body = &src[start..src[start..].find("\n}\n").map_or(src.len(), |end| start + end)];
    if let Some(variant) = member {
        let at = body.find(&format!("    {variant} {{")).ok_or_else(|| format!("no `{symbol}`"))?;
        body = &body[at..body[at..].find("\n    },").map_or(body.len(), |end| at + end)];
    }
    match field {
        Some(field) if !body.contains(&format!(" {field}: ")) => {
            Err(format!("`{symbol}` in {file} declares no field `{field}`"))
        }
        _ => Ok(()),
    }
}

/// Every generated `evidence` resolves against the source, and every row of `HOMES` is used, so the
/// table cannot keep a stale row. A renamed field needs no row (the id IS the field's name); a struct
/// moved or renamed fails here, and the planted misses prove the check bites.
#[test]
fn every_generated_citation_resolves() {
    let tree = commands::whole_command();
    let mut used = std::collections::BTreeSet::new();
    let mut wrong: Vec<String> = Vec::new();
    for arg in vike_utility::cli::describe(&tree) {
        let (file, symbol, field) = commands::citation(&tree, &arg);
        used.insert((file, symbol));
        if let Err(why) = citation_resolves(file, symbol, field.as_deref()) {
            wrong.push(why);
        }
    }
    assert!(wrong.is_empty(), "these citations do not resolve:\n  {}", wrong.join("\n  "));
    for (scope, file, symbol) in commands::HOMES {
        assert!(used.contains(&(*file, *symbol)), "HOMES row ({scope}, {symbol}) cites nothing");
    }
    let run = "crates/vike-cli/src/cmd/backtest/run_args.rs";
    assert!(citation_resolves(run, "RunCommand", Some("profile")).is_ok());
    assert!(citation_resolves(run, "RunCommand", Some("no_such_field")).is_err());
    assert!(citation_resolves(run, "NoSuchCmd", None).is_err());
    let gatewayhub = "crates/vike-cli/src/cmd/gatewayhub.rs";
    assert!(citation_resolves(gatewayhub, "Args::Setup", Some("rotate")).is_ok());
    assert!(citation_resolves(gatewayhub, "Args::Setup", Some("no_such_field")).is_err());
}

/// Every row says where it was read from.
#[test]
fn every_row_carries_evidence() {
    for f in FLAGS.iter() {
        assert!(!f.evidence.is_empty(), "{} carries no evidence", f.long);
    }
    for r in ROSTERS.iter() {
        assert!(!r.evidence.is_empty(), "roster {} carries no evidence", r.id);
    }
}

/// ⚠ **Citations are by SYMBOL, never by line.**
///
/// A `path:NNN` rots silently — which is why this tree's citation gate rejects one outright —
/// and this table would carry that rot into a PUBLISHED asset, where nothing in this repo scans
/// it.
#[test]
fn no_evidence_cites_a_line_number() {
    fn cites_a_line(s: &str) -> bool {
        s.split_whitespace().any(|w| {
            let Some((head, tail)) = w.rsplit_once(':') else { return false };
            head.ends_with(".rs") && !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit())
        })
    }
    for f in FLAGS.iter() {
        assert!(!cites_a_line(f.evidence), "{} cites a line number: {}", f.long, f.evidence);
    }
    for r in ROSTERS.iter() {
        assert!(!cites_a_line(r.evidence), "roster {} cites a line number: {}", r.id, r.evidence);
    }
}

/// A refusal carries a message.
#[test]
fn every_refusal_carries_a_message() {
    for f in FLAGS.iter() {
        for r in f.refusals.iter() {
            assert!(!r.message().is_empty(), "{} carries an empty refusal for {:?}", f.long, r.on);
            assert!(!r.on.is_empty(), "{} carries a refusal that names no subject", f.long);
        }
    }
}

/// **Every flag refusal this crate raises is published**, so `cli.json` is complete by TYPE: a new
/// variant of `crate::refusals::FlagRefusal` does not pass without a row. A carrier forwards words
/// another surface owns, and those words are what the rows publish.
#[test]
fn every_flag_refusal_this_crate_raises_is_published() {
    use crate::refusals::FlagRefusalId;
    const CARRIERS: [FlagRefusalId; 2] = [FlagRefusalId::MarkName, FlagRefusalId::PresetRefused];
    let published: Vec<FlagRefusalId> =
        FLAGS.iter().flat_map(|f| f.refusals.iter().filter_map(Refusal::id)).collect();
    for id in FlagRefusalId::ALL {
        assert_ne!(
            published.contains(&id),
            CARRIERS.contains(&id),
            "{} must be published by exactly the rows that raise it (a carrier by none)",
            id.word()
        );
    }
}

/// A refusal whose `on` LISTS reading sub-verbs names none that accepts the flag, and the flag's rows
/// together name every reading verb that refuses it (a list, or a row of its own: `--cash` on
/// `params`). `prune` was left out of nineteen lists when it shipped, and no test noticed.
#[test]
fn a_listed_reading_verb_roster_names_exactly_the_verbs_that_refuse() {
    let reading: Vec<String> = sub_verbs().into_iter().filter(|verb| verb != "run").collect();
    let is_reading = |word: &str| reading.iter().any(|verb| verb == word);
    for f in FLAGS.iter() {
        let mut named: Vec<&str> = Vec::new();
        let mut listed = false;
        for r in f.refusals.iter() {
            let items: Vec<&str> = r.on.split(", ").collect();
            if items.len() > 1 && items.iter().all(|item| is_reading(item)) {
                listed = true;
                for item in &items {
                    assert!(
                        !f.applies_to.contains(item),
                        "{} lists `{item}`, which accepts it",
                        f.long
                    );
                }
                named.extend(items);
            } else if let Some(word) = r.on.split([' ', ',']).next().filter(|word| is_reading(word))
            {
                named.push(word);
            }
        }
        for verb in reading.iter().filter(|verb| listed && !f.applies_to.contains(&verb.as_str())) {
            assert!(named.contains(&verb.as_str()), "{}'s refusals leave out `{verb}`", f.long);
        }
    }
}

/// A row fills only holes its variant's template has, so a reworded template cannot leave a
/// value published for a hole that is gone.
#[test]
fn every_filled_hole_is_a_hole_of_its_template() {
    for f in FLAGS.iter() {
        for r in f.refusals.iter() {
            let RefusalText::Raised(id, fills) = r.text else { continue };
            for (hole, _) in fills {
                let template = id.template();
                assert!(
                    template.contains(&format!("{{{hole}}}"))
                        || template.contains(&format!("{{{hole}:?}}")),
                    "{} fills `{hole}`, which {} does not have: {template}",
                    f.long,
                    id.word()
                );
            }
        }
    }
}

/// ⚠ **The published refusal is the one the binary PRINTS — and three of the four were
/// not.**
///
/// [`Refusal::message`]'s own doc says "The message VERBATIM", and nothing held it to
/// anything. [`every_refusal_carries_a_message`] above asks only whether a message EXISTS,
/// and `the_render_equals_the_committed_fixture` compares this table to the fixture it
/// RENDERS — so the table and the published asset agreed with each other while all three
/// disagreed with `crates/vike-cli/src/cmd/backtest/read/show.rs`'s `refuse_an_unbuilt_renderer`.
/// Both existing end-to-end tests survived the divergence because each asserts one word
/// (`renderer`, `tearsheet`) that the new text still contains.
///
/// Each message lives in this file TWICE — as the flag row's own refusal and again inside
/// the `unbuilt_renderers` roster, prefixed `<flag> :: ` — so this checks both copies. It
/// can be a plain unit test rather than a cross-crate gate for one reason: the producer is
/// `pub(crate)` in this same crate, so the test CALLS it instead of quoting it.
#[test]
fn the_unbuilt_renderer_messages_are_the_ones_the_binary_prints() {
    use crate::cmd::backtest::read::show::{UNBUILT_RENDERERS, refuse_an_unbuilt_renderer};
    let roster = ROSTERS
        .iter()
        .find(|r| r.id == "unbuilt_renderers")
        .expect("the `unbuilt_renderers` roster row exists");
    for flag in UNBUILT_RENDERERS {
        let real = refuse_an_unbuilt_renderer(flag).to_string();
        let row = FLAGS.iter().find(|f| f.long == flag).expect("every refused flag has a row");
        let published = row
            .refusals
            .iter()
            .find(|r| r.message().starts_with(flag))
            .unwrap_or_else(|| panic!("{flag} publishes no refusal naming itself"));
        assert_eq!(
            published.message(),
            real,
            "{flag}: the published copy is STALE — re-copy it from \
                 `refuse_an_unbuilt_renderer`, which is what the binary prints"
        );
        let want = format!("{flag} :: {real}");
        assert!(
            roster.refused_members.contains(&want.as_str()),
            "the `unbuilt_renderers` roster carries a stale copy for {flag}"
        );
    }
}

/// ⚠ **No row may cite a path the public mirror withholds.**
///
/// This table is PUBLISHED — it ships as a release asset and the documentation site renders its
/// prose onto a public page. `scripts/`, `docs/`, `.github/`, `content/`, the `justfile` and
/// every `CLAUDE.md` are excluded from the mirror by `scripts/publish_mirror.sh`'s `DENY`, so a
/// citation naming one of them renders as a link into nothing for every reader outside this
/// repository.
///
/// Found by reading the fixture against the docs site's own `citations.test.mjs`, which treats
/// exactly these prefixes as withheld and fails its build on one — so without this test the
/// first symptom would have been a RED BUILD IN ANOTHER REPOSITORY, caused by a string in this
/// one. `rosters[top_level_commands].derived_from` cited `scripts/gen_skills.sh` and was the
/// single instance.
///
/// ⚠ `crates/` paths are fine and deliberately not matched: the mirror publishes the source
/// tree, so those citations resolve for a public reader. The rule is about the trees that are
/// held back, not about citing files at all.
#[test]
fn no_row_cites_a_path_the_mirror_withholds() {
    const WITHHELD: [&str; 5] = ["scripts/", "docs/", ".github/", "content/", "justfile"];
    let cites_withheld = |s: &str| {
        WITHHELD.iter().any(|w| {
            s.match_indices(w).any(|(i, _)| {
                // Only a CITATION counts — the prefix inside a backtick span. Prose that happens
                // to contain the word "docs/" in running text is not a link anyone follows.
                s[..i].rfind('`').is_some_and(|b| !s[b + 1..i].contains('`'))
            })
        })
    };
    let mut bad: Vec<String> = Vec::new();
    for f in FLAGS.iter() {
        for (what, s) in [
            ("evidence", f.evidence),
            ("short", f.short),
            ("reason", f.default_reason.unwrap_or("")),
        ] {
            if cites_withheld(s) {
                bad.push(format!("{} {what}: {s}", f.long));
            }
        }
        for r in f.refusals.iter() {
            if cites_withheld(&r.message()) {
                bad.push(format!("{} refusal on {}: {}", f.long, r.on, r.message()));
            }
        }
    }
    for r in ROSTERS.iter() {
        for (what, s) in [
            ("evidence", r.evidence),
            ("derived_from", r.derived_from.unwrap_or("")),
            ("admission", r.admission.unwrap_or("")),
        ] {
            if cites_withheld(s) {
                bad.push(format!("roster {} {what}: {s}", r.id));
            }
        }
    }
    let tree = commands::whole_command();
    for row in vike_utility::cli::commands(&tree) {
        if cites_withheld(&row.about) {
            bad.push(format!("`{}` about: {}", row.path.join(" "), row.about));
        }
    }
    for arg in vike_utility::cli::describe(&tree) {
        if cites_withheld(&arg.help) {
            bad.push(format!("`{} --{}` help: {}", arg.command.join(" "), arg.name, arg.help));
        }
    }
    assert!(
        bad.is_empty(),
        "these rows cite a path the public mirror withholds, and this table is published:\n  {}\n\
             Reword the citation — name the thing rather than its path — or the docs site renders a \
             dead link and its own citation gate fails a build in another repository.",
        bad.join("\n  ")
    );
}

/// The rendered document is valid JSON carrying the sets this export promises.
#[test]
fn the_rendered_document_is_well_formed() {
    let files = rendered_files();
    let raw = files.get(CLI_JSON).expect("cli.json is rendered");
    let doc: serde_json::Value = serde_json::from_str(raw).expect("cli.json is valid JSON");
    assert_eq!(doc["schema_version"], SCHEMA_VERSION);
    assert_eq!(doc["plane"], PLANE);
    assert_eq!(doc["flags"].as_array().map(Vec::len), Some(FLAGS.len()));
    assert_eq!(doc["rosters"].as_array().map(Vec::len), Some(ROSTERS.len()));
    assert!(raw.ends_with('\n'), "a published text asset ends with a newline");
}

/// ⚠ **The committed fixture is the frozen schema, and the render must still equal it.**
///
/// Read at RUN TIME rather than through `include_str!`, deliberately: the fixture ships to the
/// public mirror, and an `include_str!` of a path the mirror could ever withhold makes the whole
/// mirror fail to COMPILE. That has happened twice in this tree, which is why the run-time read
/// is the idiom here.
///
/// Compares every top-level key the FIXTURE carries, not the whole render: an old reader of the
/// released file reads those keys, so what it got is pinned value for value, and a key added to the
/// render (ruling 10: added to, never renamed or dropped) is one that reader ignores. `commands` is
/// that key: it is derived from the `clap` tree the binary compiles, so no hand copy of it can drift
/// and committing it would only make every help-text edit re-bless a fixture.
#[test]
fn the_render_equals_the_committed_fixture() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cli.json");
    let committed = std::fs::read_to_string(path).unwrap_or_else(|e| {
            panic!("the committed fixture is missing at {path} ({e}) — it is the frozen schema this table is written against")
        });
    let files = rendered_files();
    let rendered = files.get(CLI_JSON).expect("cli.json is rendered");
    let a: serde_json::Value =
        serde_json::from_str(&committed).expect("the committed fixture is valid JSON");
    let b: serde_json::Value = serde_json::from_str(rendered).expect("the render is valid JSON");
    let fixture = a.as_object().expect("the fixture is one JSON object");
    assert!(fixture.contains_key("flags") && fixture.contains_key("rosters"));
    for (key, want) in fixture {
        assert_eq!(
            Some(want),
            b.get(key),
            "the render and the committed fixture disagree about `{key}` — re-render the fixture, or fix the table"
        );
    }
}

/// ⚠ **Ruling 10: a field of the released file is never renamed or dropped, only added to.** The test
/// above compares values, and re-blessing the fixture would carry a renamed field straight past it, so
/// the NAMES an old reader reads are held here, against the render itself.
#[test]
fn every_field_an_old_reader_reads_is_still_published() {
    const TOP_LEVEL: [&str; 7] = [
        "flags",
        "grammar",
        "plane",
        "rosters",
        "schema_version",
        "sub_verb_required",
        "sub_verbs",
    ];
    const FLAG: [&str; 17] = [
        "applies_to",
        "default",
        "evidence",
        "long",
        "mode_scope",
        "profile_key",
        "refusal_reaches",
        "refusals",
        "repeat",
        "roster_id",
        "sample",
        "shape",
        "short",
        "status",
        "validated_by",
        "value",
        "value_name",
    ];
    const DEFAULT: [&str; 4] = ["conditional_on", "kind", "reason", "value"];
    const REFUSAL: [&str; 3] = ["id", "message", "on"];
    const ROSTER: [&str; 7] =
        ["admission", "derived_from", "evidence", "id", "match", "members", "refused_members"];
    const GRAMMAR: [&str; 4] =
        ["help_exit", "help_stream", "split_on_first_equals_only", "value_forms"];

    let files = rendered_files();
    let doc: serde_json::Value =
        serde_json::from_str(files.get(CLI_JSON).expect("cli.json is rendered")).expect("JSON");
    let carries = |what: &str, row: &serde_json::Value, fields: &[&str]| {
        for field in fields {
            assert!(row.get(field).is_some(), "{what} no longer publishes `{field}`: {row}");
        }
    };
    carries("the document", &doc, &TOP_LEVEL);
    carries("`grammar`", &doc["grammar"], &GRAMMAR);
    for flag in doc["flags"].as_array().expect("flags is an array") {
        carries("a flag row", flag, &FLAG);
        carries("a flag's `default`", &flag["default"], &DEFAULT);
        for refusal in flag["refusals"].as_array().expect("refusals is an array") {
            carries("a refusal", refusal, &REFUSAL);
        }
    }
    for roster in doc["rosters"].as_array().expect("rosters is an array") {
        carries("a roster row", roster, &ROSTER);
    }
}

/// **THE FIXTURE WRITER** — re-render `tests/fixtures/cli.json` from the tables, in place.
///
/// `#[ignore]`d, so it never runs in CI or in `just test`; run it deliberately when
/// [`the_render_equals_the_committed_fixture`] fails because the TABLES legitimately grew:
///
/// ```sh
/// cargo test -p vike-cli --lib surface::surface_tests::bless_the_committed_fixture -- --ignored
/// ```
///
/// Then read the diff before committing it — the comparison test exists to make a table change
/// VISIBLE, and a writer run without reading its output turns that gate into a rubber stamp.
///
/// ⚠ **This does not duplicate `vike-cli surface --out`, and the difference is the point.**
/// That verb is the PUBLICATION writer: it renders every asset into a directory a caller names,
/// for the docs site and the release. This writes ONE file, at the one path the test above
/// reads, derived from `CARGO_MANIFEST_DIR` — so blessing the fixture needs no built binary, no
/// remembered path and no `cargo run` (which the dev box refuses outright: the workspace does
/// not compile there, so a bless would otherwise have to go through a remote lane and a file
/// copy). `crates/vike-backtest/src/run/profile_surface_tests.rs`'s `bless_the_committed_fixture` is the
/// twin, for the sibling asset, and exists for the stronger version of the same reason: that
/// asset has no publication verb at all.
#[test]
#[ignore = "writes into the source tree — run it deliberately, then read the diff"]
fn bless_the_committed_fixture() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cli.json");
    let files = rendered_files();
    let mut doc: serde_json::Value =
        serde_json::from_str(files.get(CLI_JSON).expect("cli.json is rendered")).expect("JSON");
    doc.as_object_mut().expect("one JSON object").remove("commands");
    let mut rendered = serde_json::to_string_pretty(&doc).expect("plain data");
    rendered.push('\n');
    std::fs::write(path, &rendered).unwrap_or_else(|e| panic!("could not write {path}: {e}"));
    eprintln!("wrote {} bytes to {path}", rendered.len());
}
