//! The `data realtime record` grammar — the verb roster, the subscription spec and the one `parse`.

use super::{Args, COMMAND, DEFAULT_ADDR, RESERVED_GRAIN_FLAG, Render, Spec, Verb, What};
use crate::cmd::args::{Flags, help_requested, no_value};

// ─── the vocabulary ──────────────────────────────────────────────────────────────────────────────

/// Every verb, in the order [`usage`] lists them — and the roster the refusals RENDER rather than
/// restate, `crate::cmd::data::realtime`'s `VERBS` rule one rung down.
pub(super) const VERBS: &[Verb] = &[Verb::Ls, Verb::Add, Verb::Rm];

/// The roster as a refusal renders it — one spelling, used by both the missing-verb and the
/// unknown-verb messages.
pub(super) fn verb_roster() -> String {
    VERBS.iter().map(|v| v.as_str()).collect::<Vec<_>>().join(" | ")
}

/// The word `record` does NOT have, and what to say when it is typed. `--lane` is the one an
/// operator reaches for and it is the one that is unavailable — see this module's doc, ruling 1.
fn grain_refusal(flag: &str) -> String {
    format!(
        "`{flag}` is not part of the `record` grammar, and that is a fact about this tree rather \
         than a gap in this verb: the recorder chooses its stream set ONCE for the whole PROCESS \
         (`Stream::ALL`), so there is no per-stream selection two grains up to expose. The noun \
         here is a SUBSCRIPTION — a venue plus a family or a symbol plus a backfill. If that grain \
         is ever wanted the reserved word is `{RESERVED_GRAIN_FLAG}`, never `--lane`, which in \
         this same group already names a live LOSS CONTRACT (`data realtime watch --lane`)"
    )
}

/// The `--addr` refusal — ruling 3, in the shape this group already uses for a designed-and-unbuilt
/// thing: named, with what it is waiting on, never "unknown option".
fn remote_refusal() -> String {
    format!(
        "`--addr` is designed and not built on `{COMMAND}`: this verb edits the SUBSCRIPTION ROWS \
         in THIS box's settings database, and reaching another box's rows needs wire verbs \
         `vike_datahub_client::proto::Request` has no arm of \
         (docs/decisions/0081-a-recorded-subscription-is-a-write-verb.md classifies them Write, so \
         they will need node keys). Run this verb ON the box whose daemon records. ⚠ The address \
         is still USED — the venue check dials the configured datahub read-only — it simply cannot \
         be redirected by this flag yet"
    )
}

// ─── the spec ────────────────────────────────────────────────────────────────────────────────────

/// Parse this group's positional.
///
/// ⚠ **`@` MEANS A FAMILY HERE, and one module up it deliberately means nothing.**
/// `crate::cmd::data::realtime`'s `parse_key` reads the second part as a SYMBOL whatever it starts
/// with, because hyperliquid spells real instruments that way (`@107`) and that verb hands the
/// symbol to a venue VERBATIM. This grammar is the opposite: `family` is a COLUMN on the row being
/// written, so the marker has to be readable. The two parsers therefore cannot be shared, and that
/// is a property of the two nouns rather than duplication to be cleaned up — a subscription is a
/// stored selection and a live key is a wire address.
///
/// ⚠ The VENUE is not validated against a roster here. The recordable set is a per-BUILD fact about
/// a remote process ([`probe_venue`] is what asks it), and a roster in this binary would be the
/// thing the surface design's §7.1 says this grammar must not carry.
pub(super) fn parse_spec(raw: &str) -> Result<Spec, String> {
    let parts: Vec<&str> = raw.split(':').collect();
    if parts.len() == 3 {
        return Err(format!(
            "'{raw}' names a SERIES, not a subscription — the third part is a bar INTERVAL, and a \
             recorder subscription has none: it records the venue's own streams rather than a \
             resampled grid. `{COMMAND} add VENUE:SYMBOL` or `{COMMAND} add VENUE:@FAMILY`"
        ));
    }
    if parts.len() != 2 || parts.iter().any(|p| p.trim().is_empty()) {
        return Err(format!(
            "'{raw}' is not VENUE:SYMBOL or VENUE:@FAMILY — two non-empty parts, e.g. \
             binance:BTCUSDT.P or polymarket:@btc-updown-5m. The `@` marks a whole market FAMILY, \
             recorded as ONE grouped series"
        ));
    }
    let venue = parts[0].trim();
    check_token("the venue", venue)?;
    let second = parts[1].trim();
    let what = match second.strip_prefix('@') {
        Some(family) => {
            if family.is_empty() {
                return Err(format!(
                    "'{raw}' names an EMPTY family — `@` is the marker and the family name follows \
                     it, e.g. polymarket:@btc-updown-5m"
                ));
            }
            check_token("the family", family)?;
            What::Family(family.to_string())
        }
        None => {
            check_token("the symbol", second)?;
            What::Symbol(second.to_string())
        }
    };
    Ok(Spec { venue: venue.to_string(), what })
}

/// The one shape check every part of a spec gets: no whitespace, no control characters, no comma.
///
/// ⚠ Narrow ON PURPOSE. A venue's own symbol spelling is the venue's business and this row is
/// handed to `vike_datahub::recording::build_recording_feed` verbatim, so anything that could be a
/// real spelling is admitted. What is refused is the two shapes that are always a typo here: a token
/// with whitespace in it (a shell quoting mistake) and a token with a COMMA in it (a TOML array
/// separator typed INSIDE one element — `add binance:BTCUSDT,ETHUSDT` is two subscriptions, and
/// storing it as one symbol would record a market that does not exist while reading correctly in
/// `ls`).
fn check_token(what: &str, token: &str) -> Result<(), String> {
    if token.is_empty() {
        return Err(format!("{what} is empty"));
    }
    if let Some(bad) = token.chars().find(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!(
            "{what} ('{token}') contains {bad:?}, which no venue spells — that is a shell quoting \
             mistake rather than a name"
        ));
    }
    if token.contains(',') {
        return Err(format!(
            "{what} ('{token}') contains a comma. A subscription names ONE symbol or ONE family; \
             two symbols are two `add` runs, and storing the pair as one name would record a \
             market that does not exist while reading correctly in `{COMMAND} ls`"
        ));
    }
    Ok(())
}

/// `--backfill`'s value — the three words the schema's own CHECK admits
/// (`backfill IS NULL OR backfill IN ('venue', 'archive', 'off')`).
fn parse_backfill(value: &str) -> Result<String, String> {
    match value {
        "venue" | "archive" | "off" => Ok(value.to_string()),
        "" => {
            Err("--backfill was given an EMPTY value. Name `venue`, `archive` or `off`, or omit \
                   the flag to file no key at all — an ABSENT key is not the same as `off`, and \
                   the row stores the difference"
                .to_string())
        }
        other => Err(format!(
            "unknown `--backfill {other}` (venue | archive | off). The value is stored verbatim \
             and the store's own CHECK admits exactly those three, so a fourth word would be \
             refused by SQLite rather than by this message"
        )),
    }
}

// ─── the parsed line ─────────────────────────────────────────────────────────────────────────────

/// Parse this group's argv tail (everything after `record`). PURE — no I/O, no socket, no store.
///
/// ⚠ Every flag is accepted by the ONE loop below and refused PER VERB afterwards, which is the
/// discipline `crate::cmd::data` and `crate::cmd::data::realtime` both follow: an inapplicable flag
/// is named in a message saying which verb it DOES belong to, where an unknown-option error would
/// tell an operator the flag does not exist — which is false.
pub(super) fn parse(argv: &[String], configured_addr: Option<&str>) -> Result<Args, String> {
    let Some(first) = argv.first() else {
        return Err(format!("`{COMMAND}` needs a verb ({})", verb_roster()));
    };
    if matches!(first.as_str(), "-h" | "--help" | "help") {
        return help_requested();
    }
    let verb = match first.as_str() {
        "ls" => Verb::Ls,
        "add" => Verb::Add,
        "rm" => Verb::Rm,
        // The verb §7 named and the owner deleted, refused BY NAME rather than as unknown: its
        // columns are folded into `ls`, and the word is RESERVED because the real `status` —
        // configured and producing NOTHING — can only come from the daemon's in-process liveness,
        // which is a decision rather than an implementation detail.
        "status" => {
            return Err(format!(
                "`{COMMAND} status` does not exist, and its columns are on `{COMMAND} ls`: the \
                 rows are what this box is CONFIGURED to record, and a row is all the store can \
                 answer for. The other question — configured and producing NOTHING — needs the \
                 daemon's in-process liveness, which no wire verb serves; `data realtime status` \
                 is one token away and is an ADVERTISEMENT of the live market-data plane rather \
                 than a probe of either"
            ));
        }
        other => {
            return Err(format!("unknown `{COMMAND}` verb '{other}' ({})", verb_roster()));
        }
    };

    let mut positional: Option<String> = None;
    let mut profile: Option<String> = None;
    let mut profiles = false;
    let mut backfill: Option<String> = None;
    let mut note: Option<String> = None;
    let mut ord: Option<i64> = None;
    let mut dry_run = false;
    let mut addr_given = false;
    let mut render: Option<Render> = None;
    let mut json_flag = false;

    let mut flags = Flags::new(argv[1..].iter().cloned());
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            "--profile" => profile = Some(flags.value(&flag, inline)?),
            "--profiles" => {
                no_value(&flag, inline)?;
                profiles = true;
            }
            "--backfill" => backfill = Some(parse_backfill(&flags.value(&flag, inline)?)?),
            "--note" => note = Some(flags.value(&flag, inline)?),
            "--ord" => ord = Some(parse_ord(&flags.value(&flag, inline)?)?),
            "--dry-run" => {
                no_value(&flag, inline)?;
                dry_run = true;
            }
            "--format" => render = Some(parse_render(&flags.value(&flag, inline)?)?),
            "--json" => {
                no_value(&flag, inline)?;
                json_flag = true;
            }
            // ⚠ CONSUMED and then refused, rather than left to the unknown-option catch-all: the
            // flag is real, it is the established route switch on this plane, and it is the half
            // of this design that is not built. Its VALUE is read first so a trailing `--addr`
            // with nothing after it still reports the ordinary dangling-flag error.
            "--addr" => {
                flags.value(&flag, inline)?;
                addr_given = true;
            }
            // The word an operator will reach for, answered with why this grain does not exist.
            "--lane" | "--stream" => return Err(grain_refusal(&flag)),
            "-h" | "--help" => return help_requested(),
            other if other.starts_with("--") => return Err(format!("unknown option '{other}'")),
            token => {
                // ⚠ REASSEMBLED, because `Flags::next_flag` splits EVERY token on its first `=` —
                // right for a FLAG and wrong for a positional. `crate::cmd::data::source` hit this
                // and its comment carries what it cost: binding the head and dropping the tail is
                // SILENT.
                let spec = match &inline {
                    Some(rest) => format!("{token}={rest}"),
                    None => token.to_string(),
                };
                match &positional {
                    None => positional = Some(spec),
                    Some(already) => {
                        return Err(format!(
                            "unexpected extra argument '{spec}' (the spec is already \
                             '{already}'); `{}` takes ONE subscription — a second is a second run",
                            verb.as_str()
                        ));
                    }
                }
            }
        }
    }

    if addr_given {
        return Err(remote_refusal());
    }

    // ⚠ `--json` IS `--format json`, so the two can disagree in exactly one way and the
    // contradiction is REFUSED rather than resolved — the rule both sibling groups follow.
    let render = match (render, json_flag) {
        (Some(Render::Table), true) => {
            return Err("--json and --format table ask for two different renderings. `--json` IS \
                        `--format json` — pass one"
                .to_string());
        }
        (Some(r), _) => Some(r),
        (None, true) => Some(Render::Json),
        (None, false) => None,
    };

    let spec = match (verb, positional) {
        (Verb::Ls, Some(raw)) => {
            return Err(format!(
                "`{COMMAND} ls` takes no positional argument ('{raw}') — it prints the whole \
                 subscription set, which is the only thing the rows can answer for. To choose a \
                 PROFILE, `--profile NAME`"
            ));
        }
        (Verb::Ls, None) => None,
        (_, Some(raw)) => Some(parse_spec(&raw)?),
        (v, None) => {
            return Err(format!(
                "`{COMMAND} {}` needs a subscription: VENUE:SYMBOL or VENUE:@FAMILY (e.g. \
                 binance:BTCUSDT.P, polymarket:@btc-updown-5m). `{COMMAND} ls` is what is stored \
                 today",
                v.as_str()
            ));
        }
    };

    for (flag, given, belongs) in [
        ("--profiles", profiles, Verb::Ls),
        ("--backfill", backfill.is_some(), Verb::Add),
        ("--note", note.is_some(), Verb::Add),
        ("--ord", ord.is_some(), Verb::Rm),
    ] {
        if given && verb != belongs {
            return Err(format!(
                "{flag} does not apply to `{}` — it belongs to `{COMMAND} {}`",
                verb.as_str(),
                belongs.as_str()
            ));
        }
    }
    if dry_run && !verb.writes() {
        return Err(format!(
            "--dry-run does not apply to `{}`, which writes nothing in the first place",
            verb.as_str()
        ));
    }
    if render.is_some() && verb.writes() {
        return Err(format!(
            "--format/--json do not apply to `{}`: it reports what it WROTE, and that report is \
             prose an operator has to read — including the disclosure that a row takes effect at \
             the daemon's next restart. `{COMMAND} ls --format json` is the machine form of the \
             subscription set",
            verb.as_str()
        ));
    }
    if profiles && profile.is_some() {
        return Err("--profiles and --profile ask opposite questions: one LISTS the profiles and \
                    the other SELECTS one. Pass one"
            .to_string());
    }

    Ok(Args {
        verb,
        spec,
        profile,
        profiles,
        backfill,
        note,
        ord,
        dry_run,
        render,
        // A BLANK configured rung is skipped rather than honoured, the same rule the two sibling
        // parsers apply: an `Environment=` line that set nothing must not aim this at an empty
        // address.
        addr: configured_addr
            .filter(|s| !s.trim().is_empty())
            .map_or_else(|| DEFAULT_ADDR.to_string(), str::to_string),
    })
}

/// `--ord`'s value: which candidate row, when a SPEC matches more than one.
fn parse_ord(raw: &str) -> Result<i64, String> {
    let n: i64 = raw
        .trim()
        .parse()
        .map_err(|_| format!("--ord {raw:?} is not a whole number — it names a row's `ord`"))?;
    if n < 0 {
        return Err(format!(
            "--ord {raw:?} is negative, and no stored row carries a negative ord — `{COMMAND} ls` \
             prints the ords that exist"
        ));
    }
    Ok(n)
}

/// Parse a `--format` value into the axis.
fn parse_render(value: &str) -> Result<Render, String> {
    match value {
        "table" => Ok(Render::Table),
        "json" => Ok(Render::Json),
        "" => Err("--format was given an EMPTY value. Name `table` or `json`.".to_string()),
        "jsonl" => {
            Err("`--format jsonl` does not apply here: this verb answers ONE question about \
                        ONE box's stored subscription set, and the rows are part of that answer \
                        rather than a stream of their own. `--format json` is the machine form"
                .to_string())
        }
        other => Err(format!("unknown `--format {other}` (table | json)")),
    }
}
