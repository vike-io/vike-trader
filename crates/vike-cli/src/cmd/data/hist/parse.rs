//! `parse`: the `hist` group's ONE flag loop and the refusals its verbs share, before each grammar.
use super::refuse::{refuse_foreign_flags, refuse_the_remote_route_on_repair, store_refusal};
use super::{
    Args, Filter, RETIRED_SPELLINGS, SUBCOMMANDS, Sub, export, get, parse_cancel, parse_coverage,
    parse_export, parse_fetch, parse_gaps, parse_gate, parse_get, parse_health, parse_import,
    parse_list, parse_repair, parse_rm, parse_running, parse_universe,
};
use crate::cmd::args::{Flags, help_requested, no_value};
use crate::cmd::data::shared::{
    DEFAULT_ADDR, Format, ROW_VERB, Source, parse_format, parse_source,
};

/// Parse `data`'s own argv tail (everything after the verb). PURE — no I/O, no spawn.
///
/// ⚠ Every flag is accepted by the ONE loop below and then refused per-subcommand by
/// [`refuse_foreign_flags`], rather than being routed by a per-subcommand match. That ordering is
/// what lets an inapplicable flag be named in a message that says which subcommand it DOES belong
/// to; an unknown-option error would tell an operator the flag does not exist, which is false and
/// sends them looking in the wrong place.
pub(crate) fn parse(
    mut it: impl Iterator<Item = String>,
    configured_addr: Option<&str>,
) -> Result<Args, String> {
    let Some(first) = it.next() else {
        // ⚠ DERIVED from [`SUBCOMMANDS`], never re-typed. The hand-written copy this replaced
        // omitted `rm` — a subcommand that had shipped months earlier — so the one message whose
        // whole job is to name the roster named it short, on the verb that DELETES.
        return Err(format!(
            "a subcommand is required ({})",
            SUBCOMMANDS.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" | ")
        ));
    };
    if matches!(first.as_str(), "-h" | "--help" | "help") {
        return help_requested();
    }
    // ⚠ THE GROUP LAYER. `data` is a PLANE and its verbs live in groups — the surface design's §7.
    // This function is `hist`'s parser and ONLY `hist`'s: [`run`] routes `catalog`, `realtime` and
    // `source` to their own modules above here, so no other group word reaches this match.
    //
    // ⚠ This carried a refusal arm for `realtime` — the last group that was designed and unbuilt —
    // and it went with that group's verbs, along with the `unbuilt_group_message` it called. A
    // function no caller reaches is `-D dead-code`, and papering over that with an `allow` would
    // have left a second spelling of a sentence nothing produces lying in wait. The history is in
    // `crates/vike-cli/tests/data_cli/gate.rs`'s
    // `a_group_that_answers_never_reads_as_designed_but_unbuilt`, which is what survives of it.
    //
    // The sub-verb is always REQUIRED, as it is on `backtest`: there is no bare `vike-cli data …`.
    let verb = match first.as_str() {
        "hist" => it.next().ok_or_else(|| {
            format!(
                "`data hist` needs a verb ({})",
                SUBCOMMANDS.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" | ")
            )
        })?,
        // A bare verb is the PRE-GROUP spelling. Refused by NAME with its replacement, never
        // silently accepted: a deprecation that keeps working is one nobody migrates off, and a
        // deprecation that fails without naming its replacement is one that costs a support round
        // trip. See [`RETIRED_SPELLINGS`].
        other => {
            if let Some((_, now)) = RETIRED_SPELLINGS.iter().find(|(was, _)| *was == other) {
                return Err(format!(
                    "`data {other}` moved: it is `{now}` now. Every `data` verb lives in a GROUP \
                     (hist | realtime | catalog | source) — the plane got too wide to be flat."
                ));
            }
            return Err(format!(
                "unknown `data` group '{other}' (hist | realtime | catalog | source)"
            ));
        }
    };
    let sub = match verb.as_str() {
        "fetch" => Sub::Fetch,
        "running" => Sub::Running,
        "cancel" => Sub::Cancel,
        "import" => Sub::Import,
        "export" => Sub::Export,
        "get" => Sub::Get,
        "ls" => Sub::List,
        "gaps" => Sub::Gaps,
        "coverage" => Sub::Coverage,
        "health" => Sub::TapeHealth,
        "universe" => Sub::Universe,
        "gate" => Sub::Gate,
        "rm" => Sub::Rm,
        "repair" => Sub::Repair,
        "-h" | "--help" | "help" => return help_requested(),
        // The verbs that were RENAMED or ABSORBED, caught here so the message can name the new
        // spelling rather than listing the roster and leaving the reader to spot the difference.
        //
        // ⚠ `fetch-starter` and `seed-demo` are in this list even though they are also in
        // [`RETIRED_SPELLINGS`], and the duplication is the point: that table catches the FLAT
        // pre-group form (`data seed-demo`), while this arm catches the operator who learned the
        // group split and typed `data hist seed-demo`. Both are plausible, and "unknown verb" is
        // the wrong answer to either.
        "list" => return Err("`data hist list` is `data hist ls` now".to_string()),
        "tape-health" => {
            return Err("`data hist tape-health` is `data hist health` now".to_string());
        }
        "fetch-starter" => {
            return Err(
                "`fetch-starter` is not a verb any more — the SOURCE is an axis on `fetch` now: \
                 `data hist fetch --source starter`"
                    .to_string(),
            );
        }
        "seed-demo" => {
            return Err(
                "`seed-demo` is not a verb any more — the SOURCE is an axis on `fetch` now: \
                 `data hist fetch --source demo`"
                    .to_string(),
            );
        }
        other => return Err(format!("unknown `data hist` verb '{other}'")),
    };

    let mut spec: Option<String> = None;
    let mut source: Option<Source> = None;
    let mut days: Option<String> = None;
    let mut from: Option<String> = None;
    let mut to: Option<String> = None;
    let mut store: Option<String> = None;
    let mut engine: Option<String> = None;
    let mut addr: Option<String> = None;
    let mut filter = Filter::default();
    // The RETIRED flag, tracked only so the refusal below can name what replaced it. The `gaps`
    // the rest of this function reasons about is derived from the VERB, one line down.
    let mut gaps_flag = false;
    let mut class = false;
    let mut partial_only = false;
    let mut json_flag = false;
    // ⚠ `--format` is the ONE valued flag kept RAW through the loop, because WHICH parser reads it
    // is a property of the VERB: [`Sub::Get`] emits rows and reaches [`get::parse_render`], every
    // other verb here emits a catalog or a report and reaches [`parse_format`]. Parsing eagerly
    // would have meant one roster answering for both, which is exactly how
    // `data catalog ls --format jsonl` would have become valid for a verb that emits no rows.
    let mut format_raw: Option<String> = None;
    let mut limit: Option<String> = None;
    // `export --addr`'s walk step, kept RAW for the reason its match arm states.
    let mut window_raw: Option<String> = None;
    let mut symbol: Option<String> = None;
    let mut group: Option<String> = None;
    let mut interval: Option<String> = None;
    let mut produced_by: Option<String> = None;
    let mut out: Option<String> = None;
    let mut dry_run = false;
    let mut yes = false;
    // `gate`'s three. Each is kept RAW here and resolved in the `Sub::Gate` arm, so the refusals
    // below can name a flag that was given without this loop having to know which verb it belongs
    // to — the ordering [`parse`]'s own doc argues for.
    let mut require_days: Option<String> = None;
    let mut max_gap: Option<String> = None;
    let mut require_kinds: Vec<String> = Vec::new();
    // `import`'s own. Its SECOND positional (the dataset — the first, the format, rides `spec`)
    // and its two flags, each refused by name on every other verb below.
    let mut dataset: Option<String> = None;
    let mut bars: Option<String> = None;
    let mut verify = false;

    let mut flags = Flags::new(it);
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--symbol" => symbol = Some(flags.value(&flag, inline)?),
            "--group" => group = Some(flags.value(&flag, inline)?),
            "--interval" => interval = Some(flags.value(&flag, inline)?),
            "--produced-by" => produced_by = Some(flags.value(&flag, inline)?),
            "--dry-run" => {
                no_value(&flag, inline)?;
                dry_run = true;
            }
            "--yes" => {
                no_value(&flag, inline)?;
                yes = true;
            }
            "--verify" => {
                no_value(&flag, inline)?;
                verify = true;
            }
            "--bars" => bars = Some(flags.value(&flag, inline)?),
            "--source" => source = Some(parse_source(&flags.value(&flag, inline)?)?),
            "--days" => days = Some(flags.value(&flag, inline)?),
            "--from" => from = Some(flags.value(&flag, inline)?),
            "--to" => to = Some(flags.value(&flag, inline)?),
            // ⚠ REFUSED BY NAME on every verb that READS history — `export` since 2026-09-26, on
            // BOTH its routes, and every [`Sub::is_read`] verb — before its value is read, so a
            // trailing `--store` with no directory meets the same sentence rather than "requires a
            // value", which would ask for a path that is then refused. Decision 0084's amendment
            // closed the local READ door, and `export` was the one reader it had left open: the
            // engine it spawns reads through a datahub now, so the directory this flag used to name
            // is served by a key-less one started on it. Never "unknown option": somebody who typed
            // it believes a directory will be read. The WRITERS below keep it. [`store_refusal`]
            // is the sentence, and why it is ONE sentence.
            "--store" if sub == Sub::Export || sub.is_read() => return Err(store_refusal(sub)),
            "--store" => store = Some(flags.value(&flag, inline)?),
            "--engine" => engine = Some(flags.value(&flag, inline)?),
            "--out" => out = Some(flags.value(&flag, inline)?),
            "--addr" => addr = Some(flags.value(&flag, inline)?),
            "--kind" => filter.kind = Some(flags.value(&flag, inline)?),
            "--venue" => filter.venue = Some(flags.value(&flag, inline)?),
            "--name" => filter.name = Some(flags.value(&flag, inline)?),
            "--require-days" => require_days = Some(flags.value(&flag, inline)?),
            "--max-gap" => max_gap = Some(flags.value(&flag, inline)?),
            // ⚠ The ONE repeatable flag in this verb. It PUSHES rather than replaces, because its
            // whole job is to declare a SET — "this run needs bars and a trade tape" is one gate,
            // not two, and a last-wins flag would silently assert half of what was typed.
            "--require-kind" => require_kinds.push(flags.value(&flag, inline)?),
            // ⚠ STILL RECOGNISED, and refused below by name rather than dropped from the match.
            // Falling through to `unknown option '--gaps'` would tell an operator the flag does
            // not exist — which sends them to check their spelling instead of to the verb that
            // replaced it. Same rule as [`RETIRED_SPELLINGS`], one layer down.
            "--gaps" => {
                no_value(&flag, inline)?;
                gaps_flag = true;
            }
            "--class" => {
                no_value(&flag, inline)?;
                class = true;
            }
            "--partial-only" => {
                no_value(&flag, inline)?;
                partial_only = true;
            }
            // ⚠ The SHORTHAND, kept because it is the workspace convention - see [`Format`]. It
            // sets the same axis the long flag sets, so nothing downstream reads two fields.
            "--json" => {
                no_value(&flag, inline)?;
                json_flag = true;
            }
            "--format" => format_raw = Some(flags.value(&flag, inline)?),
            "--limit" => limit = Some(flags.value(&flag, inline)?),
            // ⚠ Kept RAW here for `--format`'s reason inverted: this one has exactly ONE reader
            // ([`export::parse_window_step`]) but its refusals NAME the kind, which is not resolved
            // until the `Sub::Export` arm. Parsing it in the loop would mean a message that could
            // not say which lane's step was being asked for.
            export::WINDOW_FLAG => window_raw = Some(flags.value(&flag, inline)?),
            "-h" | "--help" => return help_requested(),
            // ⚠ Judged on the `--` prefix, the same rule `crate::cmd::args`'s `is_flag_token`
            // spells for every valued flag in this crate: a token beginning with `--` is a FLAG,
            // so an unrecognised one is a usage error rather than something to read as a spec.
            other if other.starts_with("--") => return Err(format!("unknown option '{other}'")),
            // The one POSITIONAL in this verb: the spec. Anything after the first is a mistake
            // worth naming — a second bare word is nearly always a shell-quoting accident, and
            // silently ignoring it would fetch a series the operator did not ask for.
            // ⚠ The tail NAMES THE VERB rather than saying "one series per fetch", which is what it
            // said while `fetch`/`export` were the only spec-takers. [`Sub::takes_a_spec`] admits
            // [`Sub::Gate`] now, and that verb fetches nothing — an operator who typed `gate` and
            // was refused in the name of a fetch has to work out which of the two sentences is
            // about them.
            // ⚠ `import` is the one verb with TWO positionals — the FORMAT (carried in `spec` until
            // its arm moves it out) and the DATASET — and its third is refused in its own words:
            // one command imports one dataset.
            positional => match (&spec, &dataset) {
                (None, _) => spec = Some(positional.to_string()),
                (Some(_), None) if sub == Sub::Import => dataset = Some(positional.to_string()),
                (Some(first), Some(already)) => {
                    return Err(format!(
                        "unexpected extra argument '{positional}': `import` takes TWO — the FORMAT \
                         ('{first}') and ONE dataset ('{already}'). Import another dataset with \
                         another command"
                    ));
                }
                (Some(already), None) => {
                    return Err(format!(
                        "unexpected extra argument '{positional}' (the spec is already \
                         '{already}'); `{}` takes ONE series",
                        sub.as_str()
                    ));
                }
            },
        }
    }

    // ⚠ `--gaps` is a VERB now, and the refusal is UNIVERSAL rather than per-subcommand — including
    // on `ls`, the one verb that used to take it. Five arms below used to refuse it each with a
    // reason of their own (a coverage row is UTC days, a health finding is presence, a universe row
    // is membership); every one of those reasons survives in this message's second half, and none
    // of them was ever the reason it is refused on `ls`. Keeping five sites would have meant one of
    // them going stale silently the day a sixth verb landed.
    if gaps_flag {
        return Err(format!(
            "`--gaps` is not a flag any more — it is a VERB. `vike-cli data hist gaps` takes the \
             same --kind/--venue/--name filters `ls` takes and answers what is MISSING inside each \
             matched series. You typed it on `{}`, which answers a different question — a gap \
             range is epoch-ms, a coverage day is a UTC-day index, and a health finding is about \
             what is PRESENT",
            sub.as_str()
        ));
    }
    // ONE axis, two spellings, and the CONTRADICTION is refused rather than resolved. `--json` is
    // the shorthand for `--format json` ([`Format`] carries why it survives), so a line carrying
    // both can only disagree by naming some OTHER rendering. Picking a winner there would silently
    // discard half of what the operator typed, which is the rule `--days` vs `--from`/`--to`
    // already follows one arm down.
    //
    // ⚠ It is refused on the RAW value, ABOVE the two parsers, so both of them inherit it and the
    // `table` sentence is spelled once for the whole group — `catalog`'s
    // `the_two_groups_refuse_the_json_format_contradiction_in_the_same_words` holds this literal
    // and that group's equal, word for word.
    //
    // ⚠ **`jsonl` is a SECOND way to disagree and it arrived with `get`.** It gets its own sentence
    // rather than a widened `table` one, because the reason differs and the reason is what an
    // operator acts on: `table` and `json` are two renderings of ONE document, while `jsonl` is not
    // a document at all. Every other value — `csv`, `parquet`, a typo — is refused by the format
    // parser below whether or not `--json` was given, so there is nothing for this to say about it.
    //
    // ⚠ **…and that arm is [`Sub::Get`]'s ALONE, unlike the `table` one above it.** On every other
    // verb `jsonl` is in exactly the class the sentence above describes: [`parse_format`] refuses
    // it there with or without `--json`, naming [`ROW_VERB`] as the verb that serves it. Firing
    // this arm on `ls` replaced that with a message describing GET's document and ending "Pass
    // one" — advice that is FALSE there, because dropping `--json` leaves a `--format jsonl` the
    // verb still refuses. It also split the two groups: `data catalog ls --json --format jsonl`
    // answers with the [`ROW_VERB`] message, because `catalog`'s parser reads `--format` eagerly
    // and its contradiction check never sees a `jsonl` at all. One question, one answer.
    //
    // ⚠ **[`Sub::Export`] is EXEMPT from the `table` arm, and it is the one verb that must be.**
    // On `export` `--format` names the FILE `--out` receives (§7's own grammar), so `--json
    // --format table` is not two spellings of one rendering — it is a terminal flag beside a file
    // flag, and "`--json` IS `--format json` — pass one" would be advice to drop a flag that is
    // not the problem. [`export::parse_wire`] and [`export::refuse_a_wire_on_the_local_route`]
    // give the AXIS correction instead, each on its own route.
    match (format_raw.as_deref(), json_flag) {
        (Some("table"), true) if sub != Sub::Export => {
            return Err("--json and --format table ask for two different renderings. `--json` IS \
                 `--format json` — pass one"
                .to_string());
        }
        (Some("jsonl"), true) if sub == Sub::Get => {
            return Err(
                "--json and --format jsonl are not two spellings of one thing. `--json` IS \
                 `--format json`: ONE document, with the rows in a field beside the counts. \
                 `jsonl` is a SEQUENCE — one object per row and nothing else on stdout, for a \
                 pipeline. Pass one."
                    .to_string(),
            );
        }
        _ => {}
    }
    // WHICH parser reads the value is the VERB's property — see [`format_raw`]'s declaration. Only
    // one of these two is ever `Some`, and each arm's `None` means "this verb does not use that
    // axis" rather than "nothing was given".
    let get_render = match sub {
        Sub::Get => Some(get::render_for(format_raw.as_deref(), json_flag)?),
        _ => None,
    };
    // ⚠ **THREE parsers now, not two, and [`Sub::Export`]'s is the one that reads a different
    // AXIS.** `get`'s reads a terminal rendering and this file's own [`parse_format`] reads a
    // catalog rendering; `export`'s reads the FILE FORMAT `--out` receives. That is §7's grammar
    // rather than a widening: `export SPEC --out FILE [--format parquet|csv|jsonl]` names no
    // terminal form at all. The consequence a reader needs is that `--format json` on `export`
    // used to parse HERE and mean the report document — it is now refused by name with the axis
    // correction, and `--json` (which is what it always was) is untouched.
    let format = match sub {
        Sub::Get | Sub::Export => None,
        _ => format_raw.as_deref().map(parse_format).transpose()?,
    };

    // ⚠ ONE downstream field, deliberately. Sixty-odd sites read [`Args::json`]; a second field
    // saying the same thing in different words is how the two come to disagree. For `get` it is a
    // PROJECTION of [`GetArgs::render`] rather than a second decision — that verb has three
    // renderings and this field has two states, so the projection is computed here, once, and
    // `execute_get` reads the render instead.
    let json = match get_render {
        Some(r) => r == get::Render::Json,
        None => match (format, json_flag) {
            (Some(f), _) => f == Format::Json,
            (None, given) => given,
        },
    };

    // DERIVED from the verb, never from a flag: [`Sub::Gaps`] IS the probe, so nothing downstream
    // has to ask both questions.
    let gaps = sub == Sub::Gaps;

    // `--limit` belongs to `get` and to nothing else, for [`refuse_foreign_flags`]'s reason. It
    // bounds ROWS, and `get` is the only verb of this plane that emits any: a `--limit` on `ls`
    // looks like it would shorten a listing and would in fact be a flag with nothing to bound,
    // which is worse than a flag that does not exist.
    //
    // ⚠ [`ROW_VERB`] is RENDERED, not re-typed. This site hand-typed `vike-cli data hist get SPEC`
    // in the same PR that introduced the const to stop exactly that — and the two spellings had
    // already diverged by a trailing `SPEC` on the day they were written. A third copy is how the
    // `(P2)` vs `(P4)` drift the const exists for comes back.
    if sub != Sub::Get {
        refuse_foreign_flags(
            sub,
            &[("--limit", limit.is_some())],
            &format!(
                "that flag bounds how many ROWS are printed, and the only verb here that emits \
                 rows is {ROW_VERB}. Every other verb answers about a store rather than with its \
                 contents — narrow a LISTING with --kind/--venue/--name instead"
            ),
        )?;
    }

    // `--out` belongs to `export` and to nothing else, for [`refuse_foreign_flags`]'s reason: an
    // operator who typed it on a `fetch` is not looking for "unknown option" — that flag exists,
    // and they want the verb that takes it.
    if sub != Sub::Export {
        refuse_foreign_flags(
            sub,
            &[("--out", out.is_some())],
            "that flag names the FILE an `export` writes, and only `export` writes one",
        )?;
    }

    // `--window` belongs to `export`'s REMOTE route alone, and it is refused in TWO places with
    // TWO different sentences — here for every other verb, and in the `Sub::Export` arm for the
    // local route. One message could not serve both: elsewhere the fact is that nothing walks a
    // wire, while on the local route the walk is the thing that does not happen.
    if sub != Sub::Export {
        refuse_foreign_flags(
            sub,
            &[(export::WINDOW_FLAG, window_raw.is_some())],
            "that flag sets the step of a WINDOWED WALK, and the only verb here that walks one is \
             `vike-cli data hist export SPEC --out FILE --addr H:P`, which splits a bulk range \
             into frame-sized requests. Every other verb answers in one request",
        )?;
    }

    // `import`'s two flags belong to `import` and to nothing else — refused BY NAME everywhere, in
    // one place, for [`refuse_foreign_flags`]'s reason: an operator who typed `--bars` on a fetch
    // wants the verb that takes it, not "unknown option".
    if sub != Sub::Import {
        refuse_foreign_flags(
            sub,
            &[("--bars", bars.is_some()), ("--verify", verify)],
            "that flag belongs to `vike-cli data hist import`, which reads a vendor ARCHIVE on the \
             datahub's own box: --bars names the bar intervals derived per imported day, and \
             --verify decodes the archive without writing. A verb here that takes a bar step takes \
             it from its spec",
        )?;
    }

    // `gate`'s three CRITERION flags belong to `gate` and to nothing else — refused BY NAME
    // everywhere, in one place. An operator who typed one on `ls` is not looking for "unknown
    // option": they want the verb whose answer is a verdict rather than a table, and the message
    // names it.
    if sub != Sub::Gate {
        refuse_foreign_flags(
            sub,
            &[
                ("--require-days", require_days.is_some()),
                ("--max-gap", max_gap.is_some()),
                ("--require-kind", !require_kinds.is_empty()),
            ],
            "that flag declares a CRITERION, and the only verb here that judges one is \
             `vike-cli data hist gate SPEC --require-days N` — whose product is an exit code a CI \
             step branches on. Every other verb RENDERS a store and leaves the decision to you",
        )?;
    }

    // The SELECTOR flags belong to the two subcommands that name a series by identity — `rm` and
    // `repair` — and to nothing else. Refused BY NAME everywhere else, in one place.
    //
    // ⚠ `repair` joined this set rather than getting refusals of its own, and the naming of the
    // set changed with it: these flags are not "rm's flags", they are how a series is ADDRESSED
    // when the store's own enumeration is not the way in. For `rm` that is because a wildcard is
    // wanted; for `repair` it is because the broken series may not be enumerable at all.
    //
    // ⚠ `import` is exempt here and refuses the three identity flags in its OWN arm below, because
    // this sentence would be false there: an import does name its series exactly, by its two
    // positionals, and "narrow a LISTING" is advice for a verb it is not.
    if !matches!(sub, Sub::Rm | Sub::Repair | Sub::Import) {
        refuse_foreign_flags(
            sub,
            &[
                ("--symbol", symbol.is_some()),
                ("--group", group.is_some()),
                ("--interval", interval.is_some()),
            ],
            "that flag names a series by IDENTITY and belongs to `rm` or `repair`. To narrow a \
             LISTING use --kind/--venue/--name, which are substring filters over what a datahub \
             already sent",
        )?;
    }
    // The PLAN-THEN-CONFIRM pair. It rode the identity refusal above, with that refusal's sentence,
    // until `import` became a third verb that takes it: "names a series by IDENTITY" was never
    // true of a confirmation flag, and naming two of its three verbs would send an operator who
    // meant `import` to the wrong one.
    if !matches!(sub, Sub::Rm | Sub::Repair | Sub::Import) {
        refuse_foreign_flags(
            sub,
            &[("--dry-run", dry_run), ("--yes", yes)],
            "that flag rehearses or confirms a WRITE that is shown as a plan first, and the verbs \
             that plan one take it: `rm`, `repair` and `import`",
        )?;
    }
    // `--produced-by` belongs to `rm` ALONE, `repair` included — and the reason is worth the extra
    // refusal rather than a shared row above. It is a provenance ASSERTION over the commit keys of
    // everything a selector matched, and it exists because a DELETE by name is not safe enough. A
    // rebuild asserts nothing and deletes nothing: it re-derives an index from parts it reads, so
    // there is no act for a provenance check to stand in front of. Accepting it here would be a
    // flag that looks like a guard and guards nothing.
    if sub != Sub::Rm {
        refuse_foreign_flags(
            sub,
            &[("--produced-by", produced_by.is_some())],
            "that flag ASSERTS the commit-key provenance of everything about to be DELETED, and \
             only `rm` deletes. A `repair` reads parts and rewrites an index — it removes no row, \
             so there is nothing for a provenance assertion to guard",
        )?;
    }

    // The half-crossing refusals, spelled once for both directions. A read verb may not carry a
    // store-side flag and a write verb may not carry a datahub-side one — see [`Sub::is_read`] and
    // the module doc's opening for why "ignore it" was never an option here.
    if sub.is_read() {
        // The ENGINE flag, refused on every read subcommand: this process cannot open a hist store
        // at all, and a read verb spawns nothing.
        // ⚠ `--store` is not in this list any more, and cannot reach it: the flag loop refuses it on
        // every read verb before its value is read, with [`store_refusal`]'s sentence — the one
        // every history reader prints. It rode this list, with a hand-spelled copy of that
        // sentence's replacement command in the tail, until 2026-09-26.
        refuse_foreign_flags(
            sub,
            &[("--engine", engine.is_some())],
            "that flag names the engine that writes a hist store on THIS machine, and the read \
             verbs spawn nothing: they read the store a running vike-datahub already has open — \
             reach it with --addr",
        )?;
        // ⚠ The WINDOW is refused on the read subcommands whose answer is a whole-series fold, and
        // ACCEPTED on `universe`, whose question IS a window — [`Sub::refuses_a_window`] is where
        // that split is decided and argued. `--days` is refused on `universe` too, separately, in
        // its own arm below: it counts back from NOW, which makes a membership window answer a
        // different question every time it is run.
        if sub.refuses_a_window() {
            refuse_foreign_flags(
                sub,
                &[("--days", days.is_some()), ("--from", from.is_some()), ("--to", to.is_some())],
                "that flag bounds a FETCH window, and this verb folds each series' WHOLE recorded \
                 span — a bound could only narrow the rendering, never the question. \
                 `vike-cli data hist universe --from … --to …` is the read verb whose question IS a \
                 window",
            )?;
        }
        // ⚠ Keyed on [`Sub::takes_a_spec`] rather than on `is_read`, because `gate` is a READ verb
        // that takes one — see that predicate's doc for why the identity argument below survives
        // it unchanged.
        if let Some(extra) = &spec
            && !sub.takes_a_spec()
        {
            return Err(format!(
                "'{extra}': `{}` takes no VENUE:SYMBOL:INTERVAL spec — a stored series is \
                 (kind, venue, symbol-or-group, interval?), which no colon-string can spell. \
                 Narrow the listing with --kind/--venue/--name instead",
                sub.as_str()
            ));
        }
    } else if sub == Sub::Rm {
        // `rm` keeps `--addr` (its REMOTE route) and `--kind`/`--venue` (its SELECTOR, not a
        // substring filter). What it refuses is the read half's browse aids and the fetch half's
        // window, each with the reason it does not apply.
        refuse_foreign_flags(
            sub,
            &[
                ("--name", filter.name.is_some()),
                ("--class", class),
                ("--partial-only", partial_only),
            ],
            "that flag narrows or annotates a LISTING. `rm` selects by EXACT identity — --symbol \
             or --group, never a substring — because a substring match is not a thing to delete by",
        )?;
        refuse_foreign_flags(
            sub,
            &[("--days", days.is_some()), ("--from", from.is_some()), ("--to", to.is_some())],
            "that flag bounds a FETCH window. `rm` removes whole series, never a time range — a \
             partial delete is a different feature with a different plan",
        )?;
        // ⚠ The two ROUTES are exclusive, and this is where that is decided. `--addr` names a
        // datahub that already has a store open; `--store`/`--engine` name a store and a binary on
        // THIS machine. Both at once has two readable meanings and no obviously right one, and
        // either choice would silently discard half of what the operator typed — the same rule
        // `window_from` applies to `--days` vs `--from`/`--to`.
        if addr.is_some() && (store.is_some() || engine.is_some()) {
            return Err(
                "--addr and --store/--engine are two DIFFERENT stores: --addr asks a running \
                 vike-datahub about the store THAT process opened, while --store names one on this \
                 machine for the engine to open. Pass one."
                    .to_string(),
            );
        }
        if let Some(extra) = &spec {
            return Err(format!(
                "'{extra}': `rm` takes no VENUE:SYMBOL:INTERVAL spec — a stored series is \
                 (kind, venue, symbol-or-group, interval?), and naming it positionally cannot \
                 express a GROUPED series (whose symbol is empty) or a wildcarded dimension. Use \
                 --kind/--venue/--symbol|--group/--interval"
            ));
        }
    } else if sub == Sub::Repair {
        // `repair` keeps `--kind`/`--venue` (its SELECTOR, not a substring filter) and the
        // store-side flags. It refuses the read half's browse aids and the fetch half's window for
        // `rm`'s reasons, and `--addr` for one of its own.
        refuse_foreign_flags(
            sub,
            &[
                ("--name", filter.name.is_some()),
                ("--class", class),
                ("--partial-only", partial_only),
            ],
            "that flag narrows or annotates a LISTING. `repair` names ONE series by EXACT identity \
             — --symbol or --group, never a substring — because the series it repairs may be one \
             no listing can show you",
        )?;
        refuse_foreign_flags(
            sub,
            &[("--days", days.is_some()), ("--from", from.is_some()), ("--to", to.is_some())],
            "that flag bounds a FETCH window. `repair` rebuilds a whole series' index from the \
             parts on disk — there is no time range to rebuild",
        )?;
        if addr.is_some() {
            return Err(refuse_the_remote_route_on_repair());
        }
        if let Some(extra) = &spec {
            return Err(format!(
                "'{extra}': `repair` takes no VENUE:SYMBOL:INTERVAL spec — a stored series is \
                 (kind, venue, symbol-or-group, interval?), which no colon-string can spell (it \
                 carries no kind, and a GROUPED series has no symbol at all). Use \
                 --kind/--venue/--symbol|--group/--interval"
            ));
        }
    } else if matches!(sub, Sub::Running | Sub::Cancel) {
        // ⚠ **THE RUNNING-FETCH DOOR, and every refusal here says what IS true of it** — which is
        // why these two are not [`Sub::is_read`] verbs (see [`Sub::Running`]): they read no store,
        // take no filter and start no fetch. They ask a datahub about the fetches it is RUNNING,
        // and `--addr` is the one flag that means anything to them.
        refuse_foreign_flags(
            sub,
            &[("--store", store.is_some()), ("--engine", engine.is_some())],
            "those flags name a LOCAL store and the engine that opens it, and this verb touches no \
             store at all: it asks a datahub about the fetches it is RUNNING. Use --addr \
             HOST:PORT (default 127.0.0.1:7878) to say WHICH datahub",
        )?;
        refuse_foreign_flags(
            sub,
            &[("--source", source.is_some())],
            "that flag chooses where a FETCH gets its rows, and this verb starts no fetch: it looks \
             at (or stops) the ones a datahub is already running",
        )?;
        refuse_foreign_flags(
            sub,
            &[("--days", days.is_some()), ("--from", from.is_some()), ("--to", to.is_some())],
            "that flag bounds a FETCH window, and a running fetch already has one — `running` \
             prints it, and `cancel` names a fetch by its SERIES and stops every running request \
             on it, whatever its window",
        )?;
        refuse_foreign_flags(
            sub,
            &[
                ("--kind", filter.kind.is_some()),
                ("--venue", filter.venue.is_some()),
                ("--name", filter.name.is_some()),
                ("--class", class),
                ("--partial-only", partial_only),
            ],
            "that flag narrows or annotates a LISTING of the store, and this verb reads no store: \
             `running` lists every fetch the datahub is serving, and `cancel` names ONE series \
             exactly, by the spec it was fetched with",
        )?;
    } else if sub == Sub::Import {
        // ⚠ **THE IMPORT DOOR, and every refusal here says what is true of it** — the
        // running-door arm's rule. An import writes the store a DATAHUB has open, from files on
        // THAT box; it names its series by its two positionals and its days by --from/--to, so
        // every flag below would be a second, contradicting way to say one of those things.
        refuse_foreign_flags(
            sub,
            &[("--store", store.is_some()), ("--engine", engine.is_some())],
            "those flags name a store on THIS machine and the engine that opens it, and an import \
             has no local route: the DATAHUB reads the archive on its OWN box \
             (<project>/market_data/imports/) into the store IT serves. Use --addr HOST:PORT \
             (default 127.0.0.1:7878) to say WHICH datahub — the one whose box holds the files",
        )?;
        refuse_foreign_flags(
            sub,
            &[("--source", source.is_some())],
            "that flag chooses where a FETCH gets its rows, and an import's source is its FORMAT — \
             the first argument, one of the formats the datahub advertises",
        )?;
        refuse_foreign_flags(
            sub,
            &[("--days", days.is_some())],
            "that flag counts back from NOW, and an import's window is the ARCHIVE's own days: \
             name them with --from/--to (inclusive UTC days, YYYY-MM-DD), or omit both for the \
             whole dataset",
        )?;
        refuse_foreign_flags(
            sub,
            &[
                ("--kind", filter.kind.is_some()),
                ("--venue", filter.venue.is_some()),
                ("--name", filter.name.is_some()),
                ("--class", class),
                ("--partial-only", partial_only),
            ],
            "that flag narrows or annotates a LISTING, and an import names what it writes exactly: \
             the FORMAT decides the venue and the kinds, and the DATASET is the symbol",
        )?;
        refuse_foreign_flags(
            sub,
            &[
                ("--symbol", symbol.is_some()),
                ("--group", group.is_some()),
                ("--interval", interval.is_some()),
            ],
            "an import's series is named by its two arguments — the DATASET is the symbol and the \
             FORMAT decides the venue — and the bar intervals it derives by --bars",
        )?;
    } else if sub == Sub::Export {
        // ⚠ **`export` LEFT the blanket write-half refusal below for TWO of its six rows, and only
        // `export` did.** `--addr` is this verb's ROUTE SWITCH now and `--kind` is its remote
        // route's row shape, so refusing them here with "that belongs to the READ half" would be
        // false about the verb the operator is actually running — and would make the two flags
        // unreachable, which is how this pair sat until 2026-09-22. The four that remain are
        // LISTING aids and are as foreign to an export as they are to a fetch, so they keep the
        // sentence they had. ⚠ The two departed rows are NOT ignored anywhere: `--addr` decides
        // the route in the `Sub::Export` arm below, and `--kind` is either parsed there (remote) or
        // refused there BY NAME with the flag that reaches the route it means (local).
        refuse_foreign_flags(
            sub,
            &[
                ("--venue", filter.venue.is_some()),
                ("--name", filter.name.is_some()),
                ("--class", class),
                ("--partial-only", partial_only),
            ],
            "that flag narrows or annotates a LISTING, and an export names ONE series. To find \
             the series you meant, `vike-cli data hist ls` takes all four",
        )?;
    } else {
        // ⚠ `Sub::Fetch` is the only subcommand left, and `--addr` is deliberately NOT in this
        // list any more — a VENUE fetch keeps it (that source's own route to a datahub, the same
        // shape `export` left its own blanket refusal for), while `--source starter|demo` refuses
        // it for a different reason. Both are checked in the per-source arm below, once the
        // source is at hand, rather than blanket-refused here where the two could not be told
        // apart — which is what let a VENUE fetch's OWN `--addr` get refused as foreign until now.
        refuse_foreign_flags(
            sub,
            &[
                ("--kind", filter.kind.is_some()),
                ("--venue", filter.venue.is_some()),
                ("--name", filter.name.is_some()),
                ("--class", class),
                ("--partial-only", partial_only),
            ],
            "that flag belongs to the READ half (`ls`/`gaps`/`coverage`), which asks a running \
             vike-datahub about a store — while `fetch --source starter|demo` drives the engine \
             against a store on this machine, named with --store",
        )?;
    }

    // The per-subcommand half of the grammar. Only `fetch` produces a spec and a window; the other
    // arms are refusals (and `rm`'s own construction), so the ONE `Args` below cannot drift between
    // subcommands the way five hand-written literals would.
    let mut rm = None;
    let mut repair = None;
    let mut export_range = None;
    // `Some` ONLY on `export`'s remote route — see [`Args::export`] for why that is the selector
    // rather than a boolean beside `export_range`.
    let mut export_args: Option<export::Plan> = None;
    let mut universe_window = None;
    let mut gate_args = None;
    let mut get_args = None;
    let mut import_args = None;
    let (spec, window) = match sub {
        Sub::Fetch => {
            parse_fetch::parse(sub, source, spec, days, from, to, &store, &engine, &addr)?
        }
        // ⚠ `--source` is FETCH's axis and nothing else's, refused elsewhere rather than ignored.
        // Every other verb either reads the store that is already there (`ls`, `coverage`,
        // `health`, `universe`, `export`) or edits it (`rm`, `repair`) — none of them CHOOSES where
        // rows come from, so a `--source` on one names a decision that verb does not make.
        _ if source.is_some() => {
            return Err(format!(
                "--source does not apply to `{}` — it is `fetch`'s axis: WHERE rows come from. \
                 This verb works on the store that is already there.",
                sub.as_str()
            ));
        }
        Sub::Running => parse_running::parse(spec)?,
        Sub::Cancel => parse_cancel::parse(spec)?,
        Sub::Import => {
            let (spec, window, parsed) =
                parse_import::parse(spec, dataset, from, to, bars, dry_run, verify, yes)?;
            import_args = parsed;
            (spec, window)
        }
        Sub::Export => {
            let (spec, window, range, plan) = parse_export::parse(
                sub, spec, &out, days, &addr, &engine, &filter, format_raw, from, to, window_raw,
            )?;
            export_range = range;
            export_args = plan;
            (spec, window)
        }
        Sub::Get => {
            let (spec, window, parsed) = parse_get::parse(
                sub,
                &filter,
                class,
                partial_only,
                spec,
                days,
                from,
                to,
                limit,
                get_render,
            )?;
            get_args = parsed;
            (spec, window)
        }
        Sub::List => parse_list::parse(sub, partial_only)?,
        Sub::Gaps => parse_gaps::parse(sub, partial_only, class)?,
        Sub::Coverage => parse_coverage::parse(sub, &filter, class)?,
        Sub::TapeHealth => parse_health::parse(sub, partial_only, class)?,
        Sub::Universe => {
            let (spec, window, parsed) =
                parse_universe::parse(sub, partial_only, class, days, from, to)?;
            universe_window = parsed;
            (spec, window)
        }
        Sub::Gate => {
            let (spec, window, parsed) = parse_gate::parse(
                sub,
                &filter,
                class,
                partial_only,
                spec,
                require_days,
                max_gap,
                require_kinds,
            )?;
            gate_args = parsed;
            (spec, window)
        }
        Sub::Rm => {
            let (spec, window, parsed) = parse_rm::parse(
                &mut filter,
                symbol,
                group,
                interval,
                produced_by,
                &addr,
                dry_run,
                yes,
            )?;
            rm = parsed;
            (spec, window)
        }
        Sub::Repair => {
            let (spec, window, parsed) =
                parse_repair::parse(&mut filter, symbol, group, interval, dry_run, yes)?;
            repair = parsed;
            (spec, window)
        }
    };

    Ok(Args {
        sub,
        // Defaulted here rather than at the flag, so every subcommand carries a source and only
        // `fetch` consults it — the axis has one home, not an Option threaded through.
        source: source.unwrap_or(Source::Venue),
        spec,
        window,
        export_range,
        universe_window,
        out,
        store,
        engine,
        // ⚠ THE TWO ARE DELIBERATELY NOT THE SAME QUESTION, and collapsing them would change
        // what `rm` DELETES. `addr_given` is "the operator asked for the remote route ON THIS LINE"
        // — `execute_rm` turns on it — so a configured `config.datahub_addr` must NOT set it: a box
        // that merely names where its datahub lives has not thereby asked for every `data hist rm` to be
        // executed against that datahub instead of the local store. The setting answers WHERE to
        // dial, never WHETHER to.
        addr_given: addr.is_some(),
        addr: addr
            .or_else(|| {
                // A BLANK rung is skipped rather than honoured, the same rule the compute ladder
                // applies: an `Environment=` line that set nothing must not aim this at an empty
                // address.
                configured_addr.filter(|s| !s.trim().is_empty()).map(str::to_string)
            })
            .unwrap_or_else(|| DEFAULT_ADDR.to_string()),
        filter,
        gaps,
        class,
        partial_only,
        json,
        rm,
        repair,
        gate: gate_args,
        get: get_args,
        export: export_args,
        import: import_args,
    })
}
