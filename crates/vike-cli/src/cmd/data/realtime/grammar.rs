//! The `data realtime` grammar — the verb, lane, format and bound words, and the one `parse`.

use std::time::Duration;

use vike_datahub_client::market::{MD_DEPTH_LEVELS_CEILING, MdLane, validate_md_symbol};

use super::render::render_word;
use super::{Args, Bound, DEFAULT_ADDR, Key, Render, Verb};
use crate::cmd::args::{Flags, help_requested, no_value};

// ─── the vocabulary ──────────────────────────────────────────────────────────────────────────────

/// Every verb, in the order [`usage`] lists them — and the roster the refusals RENDER rather than
/// restate. `crate::cmd::data`'s `SUBCOMMANDS` exists for the same reason and carries the incident:
/// the one message whose whole job is to name a roster named it short.
pub(super) const VERBS: &[Verb] = &[Verb::Watch, Verb::Status];

/// The SUB-GROUPS of `data realtime` — routed above [`parse`] by [`run`], exactly as
/// `crate::cmd::data` routes this group, so no sub-group word reaches that parser at all.
///
/// ⚠ They are NOT [`Verb`] variants and they still have to be in the ROSTER, which is the whole
/// reason this const exists rather than the routing being left implicit: the missing-verb and
/// unknown-verb refusals are both RENDERED from [`verb_roster`], so a sub-group absent from it is
/// one an operator can neither discover nor be told about while it parses perfectly well. That is
/// the incident [`VERBS`]' own doc carries, wearing a different hat.
pub(super) const SUBGROUPS: &[&str] = &["record"];

/// The roster as a refusal renders it — one spelling, used by both the missing-verb and the
/// unknown-verb messages, and it carries [`SUBGROUPS`] as well as [`VERBS`].
fn verb_roster() -> String {
    VERBS
        .iter()
        .map(|v| v.as_str())
        .chain(SUBGROUPS.iter().copied())
        .collect::<Vec<_>>()
        .join(" | ")
}

/// Every lane this wire serves, in the order [`usage`] and the refusals name them.
///
/// ⚠ **The WORD an operator types is the WIRE's own lane label** —
/// [`MdLane::feed_stream_label`] — rather than a spelling minted here, so this verb cannot offer a
/// vocabulary the wire does not answer to and a fourth lane cannot arrive unreachable from the CLI.
/// The coupling is deliberate and it is GATED rather than silent: that function's own doc warns it
/// is string-keyed between two independent venue producers, so
/// `the_lane_words_are_the_wires_own_labels` pins all three words and reddens if a producer ever
/// moves one — at which point an author decides whether the OPERATOR's word moves with it.
///
/// The array carries only the ORDER. A new [`MdLane`] variant absent from it would be absent from
/// this verb with nothing red — stable Rust cannot enumerate an enum's variants — so the residual is
/// held the only way it can be, by the no-`_` match in
/// `every_lane_is_reachable_by_the_word_it_advertises` below. That is the same residual
/// `crate::cmd::data::source`'s `SOURCES` declares and the same backstop
/// `vike_datahub_client::market`'s own suite uses.
pub(super) const LANES: &[MdLane] = &[MdLane::Depth, MdLane::Book, MdLane::Trades];

/// The lanes that are asked for and do not exist, each with the argument for WHY — never an
/// "unknown value", which would send an operator to check a spelling they got right.
///
/// ⚠ `quotes` is the row §8.3 demands and the reason is a CONTRACT rather than a gap:
/// `vike_model::strategy`'s quote sink is fed by nothing on this plane
/// ([`MdFrame`]'s own doc: *"there is no `Quotes` lane and no `Bars` lane"*), and mapping the word
/// onto `depth` would hand back a lane with the opposite loss contract under the name that was
/// asked for.
const UNSERVED_LANES: &[(&str, &str)] = &[
    (
        "quotes",
        "this wire serves no quotes lane, and `depth` is NOT a substitute for one: a superseded \
         depth frame is not a loss (the lane conflates by contract) while a dropped print is, and \
         the wire discloses that difference as a tape gap. Asking for quotes and being handed \
         depth would hide exactly the thing you asked to see",
    ),
    (
        "bars",
        "this wire carries no bar lane — a bar is a WINDOW, and a window that has closed is history: \
         `data hist fetch` gets it and `data hist ls` says what is already there",
    ),
];

/// The lane roster as a refusal renders it — DERIVED from [`LANES`], never typed.
pub(super) fn lane_roster() -> String {
    LANES.iter().map(|l| l.feed_stream_label()).collect::<Vec<_>>().join(" | ")
}

/// `--lane`'s value, resolved against the wire's own lane labels.
pub(super) fn parse_lane(value: &str) -> Result<MdLane, String> {
    if let Some(lane) = LANES.iter().copied().find(|l| l.feed_stream_label() == value) {
        return Ok(lane);
    }
    if value.is_empty() {
        return Err(format!("--lane was given an EMPTY value. Name one of: {}", lane_roster()));
    }
    if let Some((_, why)) = UNSERVED_LANES.iter().find(|(name, _)| *name == value) {
        return Err(format!(
            "`--lane {value}` is not a lane this wire serves: {why}. The lanes it does serve: {}",
            lane_roster()
        ));
    }
    Err(format!("unknown `--lane {value}` ({})", lane_roster()))
}

/// The formats this group names in a refusal but does not serve, and what each is waiting on — the
/// same choice `crate::cmd::data`'s `UNBUILT_FORMATS` makes, for the same reason.
///
/// ⚠ **A FUNCTION rather than a `const`, for [`usage`]'s reason**: the `csv` row's answer names
/// WHICH verb emits rows, and that fact belongs to the plane rather than to this group —
/// [`super::ROW_VERB`]. It was typed here as `(P4)` while the sibling roster one module over typed
/// `(P2)`, about the same verb, and the two shipped disagreeing. A `&'static str` cannot
/// interpolate, so the roster renders instead.
///
/// ⚠ **That const lost its PHASE MARKER when `data hist get` shipped, and this row's sentence moved
/// with it.** It read "the verb that emits them is `data hist get` (P4)", which sent a reader to a
/// phase table; the verb exists, so the row now says what it does and does not serve.
///
/// ⚠ **…and this row ended "`csv` is still served by nobody", which stopped being true when
/// `data hist export --addr --format csv` shipped.** So does the row's own answer: `csv` is a FILE
/// format now, written by a verb, and the sentence names it. What is unchanged is the refusal
/// HERE — a live frame is still not a row this group writes to a file — which is why the row stays
/// rather than the value being admitted.
fn unbuilt_renders() -> [(&'static str, String); 2] {
    [
        (
            "csv",
            format!(
                "the spreadsheet form of a ROW, and a frame is not a row — {} is the verb that \
                 emits rows to stdout, and it serves `jsonl` rather than `csv`; {} is the verb \
                 that writes a csv FILE, out of a store rather than off a live wire",
                super::ROW_VERB,
                super::FILE_VERB
            ),
        ),
        ("parquet", "a FILE format; a live stream has no schema to declare up front".to_string()),
    ]
}

/// Parse a `--format` value into the axis. Which verb may use which is [`parse`]'s to refuse.
pub(super) fn parse_render(value: &str) -> Result<Render, String> {
    match value {
        "table" => Ok(Render::Table),
        "jsonl" => Ok(Render::Jsonl),
        "json" => Ok(Render::Json),
        "" => Err("--format was given an EMPTY value. Name `table`, `jsonl` (watch) or `json` \
                   (status)."
            .to_string()),
        other => {
            // Bound before the `if let`, so the rendered roster outlives the borrow of the row's
            // reason under every edition's temporary-scope rule rather than under one of them.
            let unbuilt = unbuilt_renders();
            if let Some((_, why)) = unbuilt.iter().find(|(name, _)| *name == other) {
                return Err(format!(
                    "`--format {other}` is not served here: {why}. This group renders `table`, \
                     `jsonl` (watch) and `json` (status)."
                ));
            }
            Err(format!(
                "unknown `--format {other}` (table | jsonl on `watch`; table | json on `status`)"
            ))
        }
    }
}

/// The rendering a line gets when it names none. ONE seam for both verbs, and they answer
/// DIFFERENTLY — which is the part worth reading rather than the rule itself.
///
/// * **`watch` follows the DESTINATION.** §8.3: *"`--format jsonl` is the default for a non-tty,
///   `table` for a tty"*. A file is not a tty by construction, so `--out` takes the machine form
///   too, which is what makes `| jq` and `--out FILE` agree without the operator saying so twice.
/// * **`status` follows the PLANE.** `data hist ls`, `data catalog ls` and `data source ls` all
///   render `table` whatever they are piped into, and a document verb that broke ranks would be the
///   one place in `data` where `| less` answered in JSON.
///
/// ⚠ **That split is a CORRECTION, and the thing it fixed was a test reading its own answer
/// wrong.** With the destination rule applied to both, `status` in any pipeline — which is every
/// test in `crates/vike-cli/tests/data_cli.rs`, since a spawned child's stdout is never a terminal —
/// printed a JSON document, and the table assertions written against it passed on `"binance"` and
/// `"advertised"` appearing inside that document's own keys. The rule now scopes to the verb §8.3
/// states it for, and `the_table_answer_is_a_table_and_not_a_document` is what holds it.
pub(super) fn default_render(verb: Verb, to_a_file: bool, stdout_is_a_terminal: bool) -> Render {
    match verb {
        Verb::Watch if to_a_file || !stdout_is_a_terminal => Render::Jsonl,
        Verb::Watch | Verb::Status => Render::Table,
    }
}

// ─── the spec ────────────────────────────────────────────────────────────────────────────────────

/// Parse `watch`'s positional.
///
/// ⚠ **`@` is NOT interpreted BY THIS VERB, and §7.1's `VENUE:@GROUP` form has no meaning on it.**
/// A GROUPED series is a STORE shape — one part holding many symbols, told apart by a row-level
/// column — while this wire's key is `(venue, symbol, lane)` and the symbol is handed to the venue
/// VERBATIM. Worse than meaningless, reading `@` as a marker would be WRONG here: hyperliquid
/// spells real instruments that way (`@107`), which `vike_datahub_client::market`'s own
/// symbol-bound derivation cites as a four-byte symbol. So the second part is a symbol whatever it
/// starts with.
///
/// ⚠ **This doc spoke for the whole GROUP and it may not**, which is a correction rather than a
/// rewording: `super::record`'s `parse_spec` reads the SAME character as a FAMILY marker, because
/// `family` is a COLUMN on the row it writes and the marker has to be readable there. One group,
/// two parsers, opposite answers — deliberately, and it is a property of the two nouns rather than
/// duplication to be cleaned up. The rule that survives is per-VERB: `watch` never interprets `@`.
///
/// ⚠ The VENUE is not validated, deliberately — the reachable set is a property of a remote process
/// this crate cannot see at parse time, which is the rule `crate::cmd::data`'s module doc already
/// states for the whole plane. An unknown or unserved venue comes back as a typed
/// [`MdRefusal`] naming which of the two it was, and `data realtime status` is the LOCAL way to see
/// the answer before typing it.
///
/// The SYMBOL is validated, and that is not a contradiction: [`validate_md_symbol`] is the same
/// function the server's own door calls, so this side guards no rule the far side does not know. The
/// only thing being bought is the RUNG — a malformed symbol is a usage error before a socket opens
/// rather than after.
pub(super) fn parse_key(spec: &str) -> Result<Key, String> {
    let parts: Vec<&str> = spec.split(':').collect();
    if parts.len() == 3 {
        return Err(format!(
            "'{spec}' names a SERIES, not a live key — the third part is a bar INTERVAL and this \
             wire carries no bar lane. `data realtime watch VENUE:SYMBOL --lane depth|book|trades`; \
             an interval belongs to the `data hist` verbs"
        ));
    }
    if parts.len() != 2 || parts.iter().any(|p| p.trim().is_empty()) {
        return Err(format!(
            "'{spec}' is not VENUE:SYMBOL — two non-empty parts, e.g. binance:BTCUSDT. The symbol \
             is the VENUE's own spelling and is passed to it verbatim"
        ));
    }
    // ⚠ The message is the validator's OWN and is not re-worded: one refusal, one wording, whichever
    // end reaches it first. It deliberately never echoes the symbol back — that field is the
    // unbounded one, and the validator's doc argues why an error quoting it moves the cost rather
    // than refusing it.
    validate_md_symbol(parts[1]).map_err(|why| format!("the SPEC's symbol: {why}"))?;
    Ok(Key { venue: parts[0].to_string(), symbol: parts[1].to_string() })
}

// ─── the bound ───────────────────────────────────────────────────────────────────────────────────

/// `--for`'s grammar: a count and one of `s` | `m` | `h`.
///
/// ⚠ **This is NOT `vike_model::time::parse_span` and could not be**, which is worth stating because
/// this workspace's standing rule is to reuse that grammar (`crate::cmd::data::gate`'s `--max-gap`
/// does). That one has **no seconds at all**, by design — its own doc: *"there is no `s` — a
/// walk-forward window measured in seconds is not a thing this grammar admits"* — and §8.3's own
/// example is `--for 30s`. A live stream is the one thing in this tree measured in seconds, so the
/// choice is a narrower grammar here or a wider one everywhere; widening `parse_span` would widen
/// every walk-forward window and every gap tolerance to buy one flag.
///
/// It is narrower at the TOP end too, and by name: `d` and above are refused with what they mean
/// here rather than accepted, because a stream that runs for days is a RECORDING — which is
/// [`super::record`], BUILT since 2026-09-22 — and `--unbounded` is the other answer.
///
/// ⚠ This doc and both refusals below said `record` was "not built" / "§11.1 describes"; the verb
/// exists, so they now name it as a thing to RUN rather than as a phase plan to read.
pub(super) fn parse_for(raw: &str) -> Result<Duration, String> {
    const WANT: &str = "want 30s | 5m | 2h";
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(format!("--for was given an EMPTY value ({WANT})"));
    }
    // Checked on the RAW string, before the lowercase below can hide it — `vike_model::time`'s own
    // rule, kept for the same reason: `m` already means minutes everywhere in this tree, and case as
    // the only distinguisher is how a `1M`/`1m` bug happens.
    if trimmed.ends_with('M') {
        return Err(format!(
            "--for {raw:?}: `M` is not a unit here — `m` is minutes ({WANT}). A stream measured in \
             months is `--unbounded`, or `data realtime record add`, which makes the box keep it"
        ));
    }
    let lower = trimmed.to_ascii_lowercase();
    let digits = lower.len() - lower.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let (num, unit) = lower.split_at(digits);
    if num.is_empty() {
        return Err(format!("--for {raw:?} has no count ({WANT})"));
    }
    if unit.is_empty() {
        return Err(format!(
            "--for {raw:?} has no unit — a bare number is refused so it cannot silently mean \
             {num}s or {num}m ({WANT})"
        ));
    }
    let n: u64 = num.parse().map_err(|_| format!("--for {raw:?}: count does not fit ({WANT})"))?;
    if n == 0 {
        return Err(format!(
            "--for {raw:?} is zero-length — a stream of no time is what NOT running it gives you \
             ({WANT})"
        ));
    }
    let secs = |mult: u64| {
        n.checked_mul(mult)
            .map(Duration::from_secs)
            .ok_or_else(|| format!("--for {raw:?}: count does not fit ({WANT})"))
    };
    match unit {
        "s" => secs(1),
        "m" => secs(60),
        "h" => secs(3_600),
        "d" | "w" | "mo" | "y" => Err(format!(
            "--for {raw:?} is longer than a WATCH: a stream held for days is a RECORDING, which is \
             what `data realtime record add` is for — it writes a subscription row the recording \
             daemon mounts, so the tape outlives this terminal. For an open-ended stream into THIS \
             terminal, `--unbounded` ({WANT})"
        )),
        "bars" => Err(format!(
            "--for {raw:?} is a BAR COUNT, and this wire carries no bar lane — there is nothing for \
             a bar to count here. Bound the stream in time ({WANT}) or in frames (--events N)"
        )),
        other => Err(format!("--for {raw:?}: unknown unit {other:?} ({WANT})")),
    }
}

/// `--events`' value: how many DATA frames end the stream.
fn parse_events(raw: &str) -> Result<usize, String> {
    let n: usize = raw
        .trim()
        .parse()
        .map_err(|_| format!("--events {raw:?} is not a whole number of frames"))?;
    if n == 0 {
        return Err(
            "--events 0 asks for a stream of no frames, which is what NOT running this verb gives \
             you. Name the frames you actually want, or `--unbounded`"
                .to_string(),
        );
    }
    Ok(n)
}

/// `--depth`'s value: levels per side.
///
/// ⚠ **The ONLY bound this side applies is the WIRE'S OWN FIELD, and the refusal used to advertise
/// a different one.** Every unparseable value was refused with `(1..={MD_DEPTH_LEVELS_CEILING})` —
/// a range this function has never enforced and which [`usage`]'s `--depth` row says, four lines
/// away, is never enforced at all ("a request above the ceiling is CLAMPED AND ACCEPTED, never
/// refused"). So `--depth 500` rode to the wire in silence while `--depth 70000` came back "not a
/// whole number of levels a side (1..=200)", which was false twice over: 70000 IS a whole number,
/// and 200 is not a bound anything here applies. An operator read the range and believed 201 would
/// be refused; nothing said otherwise when it was not.
///
/// ONE rule now, and the message, [`usage`] and the code all render it: **this side refuses only
/// what cannot be SENT.** [`MdSpec::depth_levels`] is a `u16`, so that boundary is `u16::MAX` —
/// a TRANSPORT fact, not a policy — and everything below it is the server's to clamp.
pub(super) fn parse_depth(raw: &str) -> Result<u16, String> {
    let trimmed = raw.trim();
    let n: u16 = trimmed.parse().map_err(|_| {
        // ⚠ Two different mistakes wear one `ParseIntError` and they need OPPOSITE answers: a typo
        // is a spelling question, while a number that does not fit is a bound the operator has to
        // be told the SIZE of — and telling them the wrong size is what this function did.
        if !trimmed.is_empty() && trimmed.chars().all(|c| c.is_ascii_digit()) {
            format!(
                "--depth {raw:?} does not FIT the wire's own field: levels a side ride as a u16, \
                 so {} is the largest number that can be sent at all. That is a TRANSPORT bound \
                 and NOT a ceiling this binary applies — anything below it is carried verbatim, \
                 the server clamps to what it serves (this wire's ceiling is \
                 {MD_DEPTH_LEVELS_CEILING}), and the number it served is disclosed before the \
                 first frame",
                u16::MAX
            )
        } else {
            format!("--depth {raw:?} is not a whole number of levels a side")
        }
    })?;
    if n == 0 {
        return Err(
            "--depth 0 asks for an empty ladder. Omit the flag for the wire's default, or name the \
             levels you want"
                .to_string(),
        );
    }
    // ⚠ A value ABOVE the ceiling is NOT refused here, and that is the point of the whole
    // clamp-is-an-acceptance rule: the SERVER decides, and what it served comes back in
    // `MdSubscribed.accepted`. Refusing locally would put a second ceiling in this binary that a
    // server could not lower — and one this binary could not raise for a server that had.
    Ok(n)
}

// ─── the parsed line ─────────────────────────────────────────────────────────────────────────────

/// Parse this group's argv tail (everything after the group word). PURE — no I/O, no socket.
///
/// ⚠ Every flag is accepted by the ONE loop below and refused PER VERB afterwards, rather than being
/// routed by a per-verb match. That ordering is `crate::cmd::data`'s and it buys the same thing: an
/// inapplicable flag is named in a message that says which verb it DOES belong to, where an
/// unknown-option error would tell an operator the flag does not exist — which is false, and sends
/// them looking in the wrong place.
pub(super) fn parse(argv: &[String], configured_addr: Option<&str>) -> Result<Args, String> {
    let Some(first) = argv.first() else {
        return Err(format!("`data realtime` needs a verb ({})", verb_roster()));
    };
    if matches!(first.as_str(), "-h" | "--help" | "help") {
        return help_requested();
    }
    let verb = match first.as_str() {
        "watch" => Verb::Watch,
        "status" => Verb::Status,
        // ⚠ `record` never reaches this match — [`run`] routes it to `super::record` above this
        // parser, the way `crate::cmd::data` routes this group. It IS in [`verb_roster`] through
        // [`SUBGROUPS`], so both refusals below still name it.
        //
        // ⚠ This arm used to REFUSE it, saying "what the BOX persists is a file on the server's
        // own machine, read once at mount". That sentence was struck by this module's own doc ~500
        // lines above before the verb existed, and the pin on it checked only for "designed and
        // not built" and "§11.1" — so the stale clause was invisible to CI for as long as it
        // stood. The verb is built now and the arm is gone with it.
        other => {
            return Err(format!("unknown `data realtime` verb '{other}' ({})", verb_roster()));
        }
    };

    let mut positional: Option<String> = None;
    let mut lane: Option<MdLane> = None;
    let mut depth: Option<u16> = None;
    let mut duration: Option<Duration> = None;
    let mut events: Option<usize> = None;
    let mut unbounded = false;
    let mut out: Option<String> = None;
    let mut addr: Option<String> = None;
    let mut render: Option<Render> = None;
    let mut json_flag = false;

    let mut flags = Flags::new(argv[1..].iter().cloned());
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--lane" => lane = Some(parse_lane(&flags.value(&flag, inline)?)?),
            "--depth" => depth = Some(parse_depth(&flags.value(&flag, inline)?)?),
            "--for" => duration = Some(parse_for(&flags.value(&flag, inline)?)?),
            "--events" => events = Some(parse_events(&flags.value(&flag, inline)?)?),
            "--unbounded" => {
                no_value(&flag, inline)?;
                unbounded = true;
            }
            "--out" => out = Some(flags.value(&flag, inline)?),
            "--addr" => addr = Some(flags.value(&flag, inline)?),
            "--format" => render = Some(parse_render(&flags.value(&flag, inline)?)?),
            "--json" => {
                no_value(&flag, inline)?;
                json_flag = true;
            }
            "-h" | "--help" => return help_requested(),
            // The `--` rule `crate::cmd::args`'s `is_flag_token` spells for this whole crate.
            other if other.starts_with("--") => return Err(format!("unknown option '{other}'")),
            token => {
                // ⚠ **REASSEMBLED, because `Flags::next_flag` splits EVERY token on its first
                // `=`** — right for a FLAG and wrong for a positional. `crate::cmd::data::source`
                // hit this and its own comment carries what it cost: binding the head and dropping
                // the tail is SILENT, and the group whose job is that two spellings of one value
                // agree was the one that had two answers for it. No venue in the roster spells an
                // `=` today, and a symbol is handed to the venue VERBATIM — so the one thing this
                // parser may not do is decide which half of one was meant.
                let spec = match &inline {
                    Some(rest) => format!("{token}={rest}"),
                    None => token.to_string(),
                };
                match &positional {
                    None => positional = Some(spec),
                    Some(already) => {
                        return Err(format!(
                            "unexpected extra argument '{spec}' (the spec is already \
                             '{already}'); `watch` takes ONE key — a second subscription is a \
                             second run"
                        ));
                    }
                }
            }
        }
    }

    // ⚠ `--json` IS `--format json`, so the two can disagree in exactly one way and the
    // contradiction is REFUSED rather than resolved — the rule both sibling groups already follow.
    // On `watch` the shorthand is refused outright a few lines down, so this only ever decides for
    // `status`.
    let render = match (render, json_flag) {
        (Some(r), true) if r != Render::Json => {
            return Err(format!(
                "--json and --format {} ask for two different renderings. `--json` IS \
                 `--format json` — pass one",
                render_word(r)
            ));
        }
        (Some(r), _) => Some(r),
        (None, true) => Some(Render::Json),
        (None, false) => None,
    };

    let key = match (verb, positional) {
        (Verb::Watch, Some(spec)) => Some(parse_key(&spec)?),
        (Verb::Watch, None) => {
            return Err(
                "`data realtime watch` needs a key: VENUE:SYMBOL (e.g. binance:BTCUSDT). It is the \
                 VENUE's own spelling — `data catalog ls --venue V` is the list"
                    .to_string(),
            );
        }
        (v, Some(spec)) => {
            return Err(format!(
                "`{}` takes no positional argument ('{spec}') — it answers about the SERVER, not \
                 about one key. One key's frames are `data realtime watch {spec} --lane L`",
                v.as_str()
            ));
        }
        (_, None) => None,
    };

    match verb {
        Verb::Watch => {
            if lane.is_none() {
                return Err(format!(
                    "`watch` needs --lane ({}): the lane is not a detail, it is the LOSS CONTRACT. \
                     `depth` conflates — a superseded frame is not a loss — and `book` is lossless, \
                     so no default could be right for both",
                    lane_roster()
                ));
            }
            if depth.is_some() && lane == Some(MdLane::Trades) {
                return Err(
                    "--depth does not apply to `--lane trades`: a trade print has no levels, and \
                     the wire IGNORES the field on that lane rather than refusing it — so a number \
                     here would be reported back as though it had done something"
                        .to_string(),
                );
            }
            if unbounded && (duration.is_some() || events.is_some()) {
                return Err(
                    "--unbounded contradicts --for/--events: one says stop and the other says \
                     never. Pass the bound you meant"
                        .to_string(),
                );
            }
            if !unbounded && duration.is_none() && events.is_none() {
                return Err("`watch` is BOUNDED by default: pass --for DURATION (30s | 5m | 2h), \
                     --events N, or both — whichever lands first ends the stream. An unbounded \
                     stream is available and has to be ASKED for: --unbounded. A verb that never \
                     returns is one that cannot go in a pipeline"
                    .to_string());
            }
            // ⚠ The SHORTHAND is answered first, deliberately: the block above resolves `--json`
            // into `Some(Render::Json)`, so checking the resolved axis first would answer an
            // operator who typed `--json` with a sentence about a flag they did not type.
            if json_flag {
                return Err(
                    "--json does not apply to `watch` — it is the shorthand for `--format json`, \
                     and a stream is a SEQUENCE rather than one document. Pass `--format jsonl`"
                        .to_string(),
                );
            }
            if render == Some(Render::Json) {
                return Err(
                    "`--format json` does not apply to `watch`: a stream is a SEQUENCE of frames \
                     arriving over time, and one JSON document cannot be written until the last of \
                     them has. `--format jsonl` is the machine form — one object per frame, one \
                     line each"
                        .to_string(),
                );
            }
        }
        Verb::Status => {
            for (flag, given) in [
                ("--lane", lane.is_some()),
                ("--depth", depth.is_some()),
                ("--for", duration.is_some()),
                ("--events", events.is_some()),
                ("--unbounded", unbounded),
                ("--out", out.is_some()),
            ] {
                if given {
                    return Err(format!(
                        "{flag} does not apply to `status` — it bounds or shapes a STREAM, and this \
                         verb reads the handshake that has already happened. The stream is `data \
                         realtime watch`"
                    ));
                }
            }
            if render == Some(Render::Jsonl) {
                return Err(
                    "`--format jsonl` does not apply to `status`: this verb answers ONE question \
                     about ONE server, and the per-venue rows are part of that answer rather than a \
                     stream of their own. `--format json` is the machine form"
                        .to_string(),
                );
            }
        }
    }

    Ok(Args {
        verb,
        key,
        lane,
        depth,
        bound: if unbounded { Bound::Unbounded } else { Bound::First { events, duration } },
        out,
        render,
        addr: addr
            .or_else(|| {
                // A BLANK rung is skipped rather than honoured, the same rule `crate::cmd::data`'s
                // `parse` applies: an `Environment=` line that set nothing must not aim this at an
                // empty address.
                configured_addr.filter(|s| !s.trim().is_empty()).map(str::to_string)
            })
            .unwrap_or_else(|| DEFAULT_ADDR.to_string()),
    })
}
