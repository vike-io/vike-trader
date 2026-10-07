use super::*;

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
        match f.value {
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

/// A bare switch cannot be [`Repeat::LastWins`]: there is no earlier value to overwrite.
#[test]
fn a_bare_switch_is_idempotent() {
    for f in FLAGS.iter() {
        if f.value == Value::None {
            assert_eq!(
                f.repeat,
                Repeat::Idempotent,
                "{} is a bare switch, so its repeat cannot be {:?}",
                f.long,
                f.repeat
            );
        }
    }
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
            assert!(!f.applies_to.is_empty(), "{} ships and is accepted nowhere", f.long);
        }
    }
}

/// Every sub-verb a row names is a real one.
#[test]
fn every_named_sub_verb_is_real() {
    for f in FLAGS.iter() {
        for v in f.applies_to.iter().chain(f.refusal_reaches.iter()) {
            assert!(
                SUB_VERBS.contains(v),
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

/// ⚠ **The two `[data]` rosters are HAND COPIES, and this holds each one equal to the
/// validator an operator actually meets.**
///
/// `gap_dispositions` and `universe_modes` shipped claiming `derived_from` a
/// `crates/vike-backtest/` symbol this crate cannot name, which silenced
/// [`an_underived_roster_admits_it`] — the only guard over a roster nothing derives — while
/// nothing compared the members to anything at all. Three copies of each three-member roster
/// could then drift in silence: the engine's match arms, `crate::cmd::data`'s validator consts,
/// and these published rows. Both rows now ADMIT the copy, and this is the leg the admission
/// promises.
///
/// **It compares by RENDERING the refusal**, because that message prints the const joined by
/// `" | "` — so extracting that segment and comparing it as a whole string pins membership,
/// ORDER and LENGTH at once. Asking whether the message merely CONTAINS the row would pass a
/// row that is short, since a prefix of a longer roster is a substring of it, and a short
/// published roster is exactly the `all_subcommands` failure this table has already had.
///
/// ⚠ The ENGINE direction is NOT covered here and both admissions say so: a fourth disposition
/// added to `DataCfg::on_gap` leaves the CLI refusing a value the profile loader accepts, and
/// catching that needs the dev-dependency that only the `tests/` tree reaches.
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
        "the `gap_dispositions` row and crates/vike-cli/src/cmd/data.rs's ON_GAP_VALUES must \
             agree in MEMBERSHIP and ORDER — this row is published in cli.json, and the refusal \
             renders that const"
    );

    let universe = row("universe_modes");
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
        "the `universe_modes` row and crates/vike-cli/src/cmd/data.rs's UNIVERSE_VALUES must \
             agree in MEMBERSHIP and ORDER"
    );
}

/// ⚠ The match rule must be one this export defines — `open_tail` above all, because a roster
/// carrying it may not be rendered as an exhaustive list.
#[test]
fn match_rules_are_known() {
    for r in ROSTERS.iter() {
        assert!(
            matches!(r.match_rule, "exact" | "ascii_ci" | "open_tail"),
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
        assert!(
            matches!(f.validated_by, "cli" | "engine" | "none"),
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
        assert!(
            matches!(f.default_kind, "schema" | "implied" | "far_side" | "none"),
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
            assert!(!r.message.is_empty(), "{} carries an empty refusal for {:?}", f.long, r.on);
            assert!(!r.on.is_empty(), "{} carries a refusal that names no subject", f.long);
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
/// disagreed with `crates/vike-cli/src/cmd/runs/show.rs`'s `refuse_an_unbuilt_renderer`.
/// Both existing end-to-end tests survived the divergence because each asserts one word
/// (`renderer`, `tearsheet`) that the new text still contains.
///
/// Each message lives in this file TWICE — as the flag row's own refusal and again inside
/// the `unbuilt_renderers` roster, prefixed `<flag> :: ` — so this checks both copies. It
/// can be a plain unit test rather than a cross-crate gate for one reason: the producer is
/// `pub(crate)` in this same crate, so the test CALLS it instead of quoting it.
#[test]
fn the_unbuilt_renderer_messages_are_the_ones_the_binary_prints() {
    use crate::cmd::runs::show::{UNBUILT_RENDERERS, refuse_an_unbuilt_renderer};
    let roster = ROSTERS
        .iter()
        .find(|r| r.id == "unbuilt_renderers")
        .expect("the `unbuilt_renderers` roster row exists");
    for flag in UNBUILT_RENDERERS {
        let real = refuse_an_unbuilt_renderer(flag);
        let row = FLAGS.iter().find(|f| f.long == flag).expect("every refused flag has a row");
        let published = row
            .refusals
            .iter()
            .find(|r| r.message.starts_with(flag))
            .unwrap_or_else(|| panic!("{flag} publishes no refusal naming itself"));
        assert_eq!(
            published.message, real,
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
            if cites_withheld(r.message) {
                bad.push(format!("{} refusal on {}: {}", f.long, r.on, r.message));
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
/// Compares the FLAG and ROSTER arrays rather than the whole document, because those are the
/// parts a later stage renders from; the envelope is pinned by the test above.
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
    assert_eq!(
        a.get("flags"),
        b.get("flags"),
        "the render and the committed fixture disagree about the flag table — re-render the fixture, or fix the table"
    );
    assert_eq!(
        a.get("rosters"),
        b.get("rosters"),
        "the render and the committed fixture disagree about the rosters"
    );
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
/// copy). `crates/vike-backtest/src/profile_surface_tests.rs`'s `bless_the_committed_fixture` is the
/// twin, for the sibling asset, and exists for the stronger version of the same reason: that
/// asset has no publication verb at all.
#[test]
#[ignore = "writes into the source tree — run it deliberately, then read the diff"]
fn bless_the_committed_fixture() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cli.json");
    let files = rendered_files();
    let rendered = files.get(CLI_JSON).expect("cli.json is rendered");
    std::fs::write(path, rendered).unwrap_or_else(|e| panic!("could not write {path}: {e}"));
    eprintln!("wrote {} bytes to {path}", rendered.len());
}
