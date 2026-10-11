use super::*;

fn parse_of(args: &[&str]) -> Result<Args, String> {
    parse(args.iter().map(|s| s.to_string()))
}

#[test]
fn options_parse_in_both_flag_forms() {
    assert_eq!(parse_of(&[]).unwrap(), Args { name: None, category: None, json: false });
    assert_eq!(
        parse_of(&["--category", "Momentum"]).unwrap().category.as_deref(),
        Some("Momentum")
    );
    assert_eq!(parse_of(&["--category=Momentum"]).unwrap().category.as_deref(), Some("Momentum"));
    assert_eq!(parse_of(&["--name", "sma"]).unwrap().name.as_deref(), Some("sma"));
    assert!(parse_of(&["--json"]).unwrap().json);
    assert!(parse_of(&["--nope"]).unwrap_err().contains("unknown option"));
    assert!(parse_of(&["--json=1"]).unwrap_err().contains("takes no value"));
    assert_eq!(parse_of(&["--help"]).unwrap_err(), "help requested");
    // Two different questions: answering one and dropping the other is how a listing lies.
    assert!(
        parse_of(&["--name", "sma", "--category", "Overlap"]).unwrap_err().contains("pass one")
    );
}

/// ⚠ The property this whole surface exists for, asserted in BOTH directions against the same
/// const the host binds from — so this listing cannot go stale while the docs claim otherwise.
///
/// Direction 2 is the one a `filter` hides: a name in `RHAI_INDICATORS` with no registry entry
/// is still CALLABLE (the host registered it) yet would never appear in any roster, so a user
/// would be told it does not exist.
#[test]
fn the_listing_is_exactly_the_host_bound_set() {
    let mut printed: Vec<&str> = bound_metas().iter().map(|m| m.name).collect();
    // The callable set is now the UNION of two spellings, and the listing must be exactly it:
    // a bare name (`sma`, `macd`), or at least one per-line accessor (`bollinger_mid`). The
    // assertion is unchanged in spirit — no uncallable suggestion, no callable name nobody can
    // discover — but "callable" stopped meaning "bare name" when per-line accessors landed.
    let mut bound: Vec<&str> = vike_indicators::registry()
        .iter()
        .filter(|m| {
            vike_script::RHAI_INDICATORS.contains(&m.name)
                || !vike_script::line_accessors(m.name).is_empty()
        })
        .map(|m| m.name)
        .collect();
    printed.sort_unstable();
    bound.sort_unstable();
    assert_eq!(
        printed, bound,
        "the printed roster must be exactly what the Rhai host binds — a name in one and not \
             the other is either an uncallable suggestion or a callable name nobody can discover"
    );
    assert!(!printed.is_empty(), "a build that binds nothing would make this test vacuous");

    // ...and the union is a STRICT superset of the bare names, or the accessor arm above is
    // dead code dressed as a guarantee. `bollinger` is the witness: listed, and no bare call.
    assert!(
        printed.len() > vike_script::RHAI_INDICATORS.len(),
        "no indicator is reachable ONLY through a line accessor, so this test proves nothing \
             about them"
    );
    assert!(bound_metas().iter().any(|m| m.name == "bollinger"), "bollinger must be listed");
    assert!(!vike_script::RHAI_INDICATORS.contains(&"bollinger"), "...with no bare call");
    assert!(!vike_script::line_accessors("bollinger").is_empty(), "...but with accessors");
}

/// Every bound name reaches the page a user reads, with its call form and its label — and each
/// category is ONE block, not a header repeated wherever the registry's source order happened
/// to interleave two families.
#[test]
fn the_rendered_listing_names_every_bound_indicator_once_per_category_block() {
    let rows = bound_metas();
    let text = render(&rows, &[]);
    for m in &rows {
        assert!(text.contains(&call_form(m)), "{} is missing its call form", m.name);
        assert!(text.contains(m.pretty), "{} is missing its label", m.name);
    }
    // The count is COMPUTED, here and in the listing — never written into prose.
    assert!(text.contains(&format!("{} callable", rows.len())));

    let headers: Vec<&str> =
        text.lines().filter(|l| !l.is_empty() && !l.starts_with(' ')).collect();
    let mut unique = headers.clone();
    unique.sort_unstable();
    unique.dedup();
    // The footer is the one un-indented line that is not a category header.
    assert_eq!(headers.len(), unique.len(), "a category header is printed twice: {headers:?}");
}

/// A category filter answers with that category and nothing else; an unknown one names the
/// categories that exist rather than printing an empty list.
#[test]
fn a_category_filter_narrows_and_an_unknown_one_explains_itself() {
    let all = bound_metas();
    let first = category_name(all[0]);
    let hit = filter_by_category(&all, &first.to_lowercase()).expect("case-insensitive");
    assert!(!hit.is_empty());
    assert!(hit.iter().all(|m| category_name(m) == first));
    assert!(hit.len() <= all.len());

    let Err(err) = filter_by_category(&all, "not-a-category") else {
        panic!("an unknown category must be an error, never an empty listing");
    };
    assert!(err.contains("not-a-category"), "{err}");
    assert!(err.contains(&first), "the error must name the categories that DO exist: {err}");
}

/// ⚠ The two ways a name can miss are different errors, because they have different repairs: a
/// typo is fixed by spelling, a HELD-BACK indicator by picking a different one (there is
/// nothing to fix). Collapsing them into one message is how somebody spends an afternoon
/// re-checking a spelling that was right.
#[test]
fn an_unbound_name_and_an_unknown_name_are_different_errors() {
    let bound = bound_metas();
    let name = bound[0].name;
    assert_eq!(find_bound(name).unwrap().name, name);

    let Err(unknown) = find_bound("sma_typo") else {
        panic!("test premise: `sma_typo` must not be an indicator anywhere");
    };
    assert!(unknown.contains("no indicator named"), "{unknown}");

    // A registry indicator this build does NOT bind — derived, never a hard-coded name, since
    // which ones are held back is the host's decision and can change. The message must carry
    // the HOST'S OWN reason (`vike_script::unbound_reason`), not a paraphrase of it.
    if let Some(m) = vike_indicators::registry().iter().find(|m| !vike_script::is_callable(m.name))
    {
        let Err(msg) = find_bound(m.name) else {
            panic!("{} is callable by no spelling, so it must not resolve as bound", m.name);
        };
        assert!(msg.contains("cannot call"), "{msg}");
        let why = vike_script::unbound_reason(m.name)
            .expect("a held-back registry indicator must carry a reason");
        assert!(msg.contains(why), "the error must quote the host's own reason: {msg}");
    }
}

/// The machine rows carry what a caller cannot otherwise know — how many parameters an
/// indicator takes, what they default to, and which value comes back.
#[test]
fn the_machine_rows_carry_params_and_outputs() {
    for m in bound_metas() {
        let row = detail_row(m);
        assert_eq!(row["name"], m.name);
        assert_eq!(row["category"], category_name(m));
        assert_eq!(row["params"].as_array().unwrap().len(), m.params.len());
        assert_eq!(row["outputs"].as_array().unwrap().len(), m.outputs.len());
        // The compact row is the strict subset a roster needs, and nothing more.
        let compact = compact_row(m);
        assert_eq!(compact["name"], m.name);
        assert!(compact.get("params").is_none(), "the compact row must stay compact");
    }
}

#[test]
fn a_whole_number_default_prints_without_a_trailing_zero() {
    assert_eq!(fmt_num(20.0), "20");
    assert_eq!(fmt_num(2.5), "2.5");
}

// ── the user's own indicators ───────────────────────────────────────────────────────────────
//
// ⚠ These drive the renderers with SYNTHETIC rows rather than installing into the process. The
// installed set is a `OnceLock` shared by every test in this binary, so one install would leak
// into every sibling and make results depend on execution order. That the renderers are fed by
// the REAL `vike_script::installed_user_indicators` is proved end-to-end, through the shipped
// binary and a real `.rhai` file on disk, by
// `crates/vike-cli/tests/user_indicators_cli.rs` — without which every assertion here would
// still pass with `user_rows()` hard-wired to return nothing.

fn synthetic() -> Vec<UserRow> {
    vec![
        UserRow {
            name: "my_ema",
            params: vec![("period", 20.0), ("scale", 1.5)],
            accessors: vec![],
            bare_call: true,
        },
        UserRow { name: "my_flag", params: vec![], accessors: vec![], bare_call: true },
    ]
}

/// A user file that declared `fn outputs() { ["upper", "mid", "lower"] }` — three lines, line 0
/// NOT the namesake, so the bare name does not bind. The shape `vike_script`'s
/// `user_bare_name_binds` produces, spelled here as a row because these tests must not install
/// into the process (see the note above).
fn synthetic_band() -> UserRow {
    UserRow {
        name: "my_bands",
        params: vec![("width", 2.0)],
        accessors: ["upper", "mid", "lower"]
            .into_iter()
            .map(|l| (l.to_string(), format!("my_bands_{l}")))
            .collect(),
        bare_call: false,
    }
}

/// The author's own indicators reach the page, with the call form their `param()` declarations
/// imply — and they are in a block of their OWN, never mixed into a built-in category.
///
/// Non-vacuous: with the user block removed, neither name appears in the text at all.
#[test]
fn the_users_own_indicators_are_listed_in_their_own_block() {
    let text = render(&bound_metas(), &synthetic());
    assert!(text.contains(USER_BLOCK_HEADING), "the block must be headed: {text}");
    // The knobs are SPELLED, or the listing tells the author their own indicator takes no
    // arguments — which is the one fact about it they are surest of, printed wrong.
    assert!(text.contains("my_ema(period=20, scale=1.5)"), "{text}");
    // ...and a file with no `param()` still renders as a CALL, parentheses included.
    assert!(text.contains("my_flag()"), "{text}");

    // The block is BELOW every category block: the built-ins are what a reader is usually
    // looking for, and a two-row block at the top would push the catalog off the screen.
    let heading_at = text.find(USER_BLOCK_HEADING).unwrap();
    let last_builtin = bound_metas().last().map(|m| text.find(&call_form(m)).unwrap()).unwrap();
    assert!(heading_at > last_builtin, "the user block must come last: {text}");

    // The footer counts BOTH and says which is which — the numbers are computed, never prose.
    let n = bound_metas().len();
    assert!(
        text.contains(&format!("{} callable in this build ({n} built in, 2 of your own)", n + 2)),
        "{text}"
    );
}

/// With nothing installed the output is byte-identical to what it always was — no empty
/// heading, no "0 of your own" noise on every fresh install.
///
/// Non-vacuous: a `render` that emitted the heading unconditionally, or that always spelled the
/// split footer, fails here while the test above still passes.
#[test]
fn no_user_indicators_means_no_block_and_the_original_footer() {
    let text = render(&bound_metas(), &[]);
    assert!(!text.contains(USER_BLOCK_HEADING), "an empty block must not be headed: {text}");
    assert!(!text.contains("of your own"), "{text}");
    assert!(text.contains(&format!("{} callable in this build.", bound_metas().len())));
    assert!(render_user_rows(&[]).is_empty());
}

/// ⚠ The machine shape must make the distinction WITHOUT a reader noticing a label: a separate
/// key, and no `category` field on a row that has no category.
///
/// Non-vacuous: folding the user rows into `indicators` (or giving them a `category`) fails
/// both halves, and a `--json` consumer would then file an author's own file under a family it
/// is not in.
#[test]
fn the_machine_rows_keep_the_two_kinds_apart() {
    let rows = synthetic();
    let user = user_detail_row(&rows[0]);
    assert_eq!(user["name"], "my_ema");
    assert_eq!(user["source"], "user_data/indicators");
    assert!(user.get("category").is_none(), "a user indicator has no category: {user}");
    assert!(user.get("outputs").is_none(), "no empty promise of a field: {user}");
    let params = user["params"].as_array().expect("its own knobs");
    assert_eq!(params.len(), 2);
    assert_eq!(params[0]["name"], "period");
    assert_eq!(params[0]["default"], 20.0);
    // ...and a built-in row is untouched by any of this.
    let builtin = detail_row(bound_metas()[0]);
    assert!(builtin.get("category").is_some() && builtin.get("source").is_none());
    // A file with no knobs still carries the key, as an empty array — a consumer must not have
    // to test for a key's presence before reading it.
    assert_eq!(user_detail_row(&rows[1])["params"].as_array().map(Vec::len), Some(0));
}

/// ⚠ **A user BAND indicator must never be printed under its bare name.** `my_bands()` does not
/// resolve — line 0 is the upper band, the same refusal `bollinger` carries — so printing it
/// would hand the author a call that raises on every bar and self-disables the strategy, which
/// is the exact silent failure this command's narrowing exists to prevent. The three spellings
/// that DO resolve are printed instead, each naming its line.
///
/// Non-vacuous in both directions: `my_ema`, whose bare name binds, is in the same block and is
/// still printed bare with no `->` column, so this cannot pass by dropping every bare name; and
/// the knobs are spelled on the accessor rows, so it cannot pass by printing bare accessors.
#[test]
fn a_user_band_indicator_is_printed_line_by_line_and_never_under_its_bare_name() {
    let mut rows = synthetic();
    rows.push(synthetic_band());
    // The block ALONE, so the `->` count below measures these rows and not the built-in block,
    // which prints the same column for its own off-namesake lines.
    let text = render_user_rows(&rows);

    assert!(
        !text.contains("my_bands(width=2)"),
        "a call that raises on every bar must not be printed: {text}"
    );
    for line in ["upper", "mid", "lower"] {
        assert!(
            text.contains(&format!("my_bands_{line}(width=2)")),
            "the {line} band must be one call away, with the knob: {text}"
        );
    }
    // ...and every one of those rows — and ONLY those — names the line it returns. The bare
    // names get no column: they bind only when line 0 is named after the file, so the call
    // already says which number it is.
    assert_eq!(text.matches("-> ").count(), 3, "{text}");
    assert!(text.contains("my_ema(period=20, scale=1.5)\n"), "a bare row ends there: {text}");
    assert!(text.contains("my_flag()\n"), "{text}");
}

/// The machine row carries the same two facts [`detail_row`] carries for a built-in, because a
/// user file declaring `fn outputs()` has the same two-families-of-name shape. Without them a
/// consumer reads `name` and types a call that raises on every bar.
///
/// Non-vacuous: the single-output row in the same test carries `bare_call: true` and an EMPTY
/// accessor list, so a row hard-wired either way fails one half.
#[test]
fn the_machine_row_says_which_spellings_of_a_user_indicator_resolve() {
    let band = user_detail_row(&synthetic_band());
    assert_eq!(band["bare_call"].as_bool(), Some(false), "{band}");
    let accessors = band["accessors"].as_array().expect("its line accessors");
    assert_eq!(accessors.len(), 3, "{band}");
    assert_eq!(accessors[1]["line"], "mid");
    assert_eq!(accessors[1]["call"], "my_bands_mid");

    // The ordinary shape: the bare name resolves and there is no per-line spelling at all —
    // the same pair a single-output BUILT-IN's row carries.
    let plain = user_detail_row(&synthetic()[0]);
    assert_eq!(plain["bare_call"].as_bool(), Some(true), "{plain}");
    assert_eq!(plain["accessors"].as_array().map(Vec::len), Some(0), "{plain}");
}
