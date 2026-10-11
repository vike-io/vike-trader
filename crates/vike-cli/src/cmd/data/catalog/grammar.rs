//! `data catalog`'s grammar: the usage page rendered from its declarations, and the parser.

use vike_datahub_client::catalog::validate_catalog_venue;
use vike_model::AssetClass;

use crate::cmd::args::{Flags, help_requested, no_value};

use super::{Args, DEFAULT_ADDR, Format, InstrumentRef, VERBS, Verb, parse_format};

/// The roster as a refusal renders it — one spelling, used by both the missing-verb and the
/// unknown-verb messages.
fn verb_roster() -> String {
    VERBS.iter().map(|v| v.as_str()).collect::<Vec<_>>().join(" | ")
}

/// The asset-class vocabulary as `--class`'s refusal and [`usage`] render it.
///
/// DERIVED from `vike_model::AssetClass::SQL_WORDS`, never typed: there are eleven variants, the
/// taxonomy has already MOVED crates once (`docs/decisions/0061` was accepted while it still lived
/// in `vike-catalog`), and a hand-copied list in a usage string is exactly the shape this
/// repository has watched rot.
pub(super) fn class_roster() -> String {
    AssetClass::SQL_WORDS.join(" | ")
}

/// Every flag spelling [`parse`] accepts, written ONCE each.
///
/// ⚠ **A `const` rather than a literal at the match arm, and it is load-bearing.** A flag is named
/// in exactly two places — the parser's arm and its usage row — and while those are two literals
/// the page can lose a flag the parser still takes, or keep one whose arm was deleted, with
/// nothing to notice. A `&'static str` const is a legal MATCH PATTERN, so the arm and the row
/// become one declaration seen twice.
pub(super) const FLAG_VENUE: &str = "--venue";
pub(super) const FLAG_CLASS: &str = "--class";
pub(super) const FLAG_SEARCH: &str = "--search";
pub(super) const FLAG_ADDR: &str = "--addr";
pub(super) const FLAG_FORMAT: &str = "--format";
const FLAG_JSON: &str = "--json";
const FLAG_HELP_SHORT: &str = "-h";
const FLAG_HELP_LONG: &str = "--help";

/// One row of [`usage`]'s `options:` block, and the declaration of one flag.
pub(super) struct FlagDoc {
    /// Every spelling [`parse`] accepts for this row, from the consts above — never a second
    /// literal. More than one only for `-h, --help`, which is one flag with two names.
    pub(super) spellings: &'static [&'static str],
    /// The value placeholder the page renders after the spelling (`V`, `H:P`), EMPTY for a flag
    /// that takes none. It is also what `every_declared_flag_is_accepted_by_the_parser` reads to
    /// decide whether to feed the probe a value.
    pub(super) arg: &'static str,
    /// The help, pre-wrapped to this page's width; the first line goes beside the label. Carries
    /// [`expand`]'s tokens rather than interpolating — see [`Verb::usage_block`].
    pub(super) help: &'static [&'static str],
}

impl FlagDoc {
    /// The label the page puts in its left column — the spellings joined, plus the placeholder.
    pub(super) fn label(&self) -> String {
        let spellings = self.spellings.join(", ");
        if self.arg.is_empty() { spellings } else { format!("{spellings} {}", self.arg) }
    }
}

/// Every flag this group's grammar carries, in the order [`usage`] lists them.
pub(super) const FLAGS: &[FlagDoc] = &[
    FlagDoc {
        spellings: &[FLAG_VENUE],
        arg: "V",
        help: &[
            "ls/refresh: the venue to ask, as its roster slug (binance, okx,",
            "polymarket). REQUIRED there, and its SHAPE is refused here rather than at",
            "the server. Refused on `show`, which names its venue in the VENUE:SYMBOL",
            "positional, and on `venues`, whose answer IS the roster",
        ],
    },
    FlagDoc {
        spellings: &[FLAG_CLASS],
        arg: "C",
        help: &["ls: keep only instruments of this asset class, case-insensitively", "({classes})"],
    },
    FlagDoc {
        spellings: &[FLAG_SEARCH],
        arg: "T",
        help: &[
            "ls: keep only instruments whose symbol, base, quote or description",
            "CONTAINS T, case-insensitively. A browse aid over the answer that already",
            "arrived — it never makes the server or the venue do less work. An EMPTY T",
            "is refused: it would keep every row while claiming a filter ran",
        ],
    },
    FlagDoc {
        spellings: &[FLAG_ADDR],
        arg: "H:P",
        help: &[
            "every verb: the datahub to ask (default {default_addr}). It binds",
            "localhost, so reach a remote one over `ssh -L 7878:localhost:7878`. On",
            "`venues` an unreachable one is REPORTED rather than fatal",
        ],
    },
    FlagDoc {
        spellings: &[FLAG_FORMAT],
        arg: "F",
        help: &[
            "HOW the answer is rendered: `table` (the default) or `json`. `--json` is",
            "its shorthand and the two are refused together only when they DISAGREE.",
            "`jsonl` is refused HERE and served by `data hist get`, the verb that emits",
            "ROWS — a catalog is one line per instrument, not a row. `csv`/`parquet`",
            "are FILE formats, written by `data hist export --out FILE`; each is refused",
            "by name with the verb that writes it, never as a spelling mistake",
        ],
    },
    FlagDoc {
        spellings: &[FLAG_JSON],
        arg: "",
        help: &[
            "shorthand for --format json: one JSON document on stdout. For `ls` the",
            "instruments with their raw grid numbers, under an `outcome` token that",
            "separates an EMPTY listing from an unlistable venue. For `show` the grid",
            "as recorded, with the class carried as the model's own stored word. For",
            "`venues` the build's rows and the server's answer as two SEPARATE objects,",
            "plus the skew between them",
        ],
    },
    FlagDoc { spellings: &[FLAG_HELP_SHORT, FLAG_HELP_LONG], arg: "", help: &["this message"] },
];

/// The two runtime facts this page carries, expanded on the way out.
///
/// ⚠ **Tokens rather than `format!`**, because the blocks they live in are `&'static str`: the
/// `--class` vocabulary is [`class_roster`]'s to answer (eleven words whose taxonomy has already
/// MOVED crates once) and the address is [`DEFAULT_ADDR`]'s. A copy of either typed into this page
/// is exactly the shape this repository has watched rot, and
/// `the_usage_leaves_no_placeholder_unexpanded` reddens on a token nothing here substitutes.
pub(super) fn expand(line: &str) -> String {
    line.replace("{classes}", &class_roster()).replace("{default_addr}", DEFAULT_ADDR)
}

/// The label column of a usage block, in `crate::cmd::data`'s [`super::USAGE`] style: two spaces,
/// the label padded to `width`, the entry's first line beside it and the rest under it.
fn usage_entry(label: &str, width: usize, lines: &[&str]) -> Vec<String> {
    let indent = 2 + width;
    lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            if i == 0 {
                format!("  {label:<width$}{}", expand(line))
            } else {
                format!("{:indent$}{}", "", expand(line))
            }
        })
        .collect()
}

/// This group's usage page.
///
/// ⚠ A FUNCTION rather than a `const`, and it is RENDERED from the declarations rather than typed:
/// one block per [`VERBS`] row through [`Verb::usage_block`], one row per [`FLAGS`] entry. A page
/// typed as one literal could — and did — lose a verb's whole paragraph with nothing red, because
/// every verb name also occurs in the surrounding prose.
pub(super) fn usage() -> String {
    // The label column widths, MEASURED off the page this rendering replaced rather than chosen,
    // so the rows land where they already did: the longest verb is `refresh` (7) plus two spaces,
    // and the longest flag label is `-h, --help` (10) plus two.
    const VERB_COL: usize = 9;
    const FLAG_COL: usize = 12;
    const PREAMBLE: &str = "\
usage: vike-cli data catalog <verb> [options]

WHAT IS ADDRESSABLE. `data hist` answers about a store; this group answers about the
INSTRUMENTS themselves — what a venue lists, what one instrument's grid is, and what
this build and the datahub that answered can each actually do, per venue.";

    let mut lines: Vec<String> = PREAMBLE.lines().map(str::to_string).collect();
    lines.push(String::new());
    for v in VERBS {
        lines.extend(usage_entry(v.as_str(), VERB_COL, v.usage_block()));
    }
    lines.push(String::new());
    lines.push("options:".to_string());
    for f in FLAGS {
        lines.extend(usage_entry(&f.label(), FLAG_COL, f.help));
    }
    lines.join("\n")
}

/// Parse this group's argv tail (everything after the group word). PURE — no I/O, no socket.
///
/// ⚠ Every flag is accepted by the ONE loop below and refused PER VERB afterwards, rather than
/// being routed by a per-verb match. That ordering is `crate::cmd::data`'s `parse`'s and it buys
/// the same thing: an inapplicable flag is named in a message that says which verb it DOES belong
/// to, where an unknown-option error would tell an operator the flag does not exist — which is
/// false, and sends them looking in the wrong place.
pub(super) fn parse(argv: &[String], configured_addr: Option<&str>) -> Result<Args, String> {
    let Some(first) = argv.first() else {
        return Err(format!("a verb is required ({})", verb_roster()));
    };
    if matches!(first.as_str(), "-h" | "--help" | "help") {
        return help_requested();
    }
    let verb = match first.as_str() {
        "ls" => Verb::Ls,
        "show" => Verb::Show,
        "refresh" => Verb::Refresh,
        "venues" => Verb::Venues,
        // The spelling an operator arrives with, because the sibling group renamed the same verb:
        // `data list` became `data hist ls`, so `data catalog list` is what a reader who learned
        // that rename types next. Naming the replacement costs one arm and saves a support round
        // trip — `crate::cmd::data`'s `RETIRED_SPELLINGS` makes the same trade one group over.
        "list" => return Err("`data catalog list` is `data catalog ls`".to_string()),
        other => return Err(format!("unknown `data catalog` verb '{other}' ({})", verb_roster())),
    };

    let mut venue: Option<String> = None;
    let mut positional: Option<String> = None;
    let mut class: Option<AssetClass> = None;
    let mut search: Option<String> = None;
    let mut addr: Option<String> = None;
    let mut json_flag = false;
    let mut format: Option<Format> = None;

    let mut flags = Flags::new(argv[1..].iter().cloned());
    while let Some((flag, inline)) = flags.next_flag() {
        match flag.as_str() {
            // ⚠ Every arm names a [`FLAGS`] const rather than a literal — see [`FLAG_VENUE`] for
            // why the parser and the usage page have to be one declaration.
            FLAG_VENUE => venue = Some(flags.value(&flag, inline)?),
            FLAG_CLASS => class = Some(parse_class(&flags.value(&flag, inline)?)?),
            FLAG_SEARCH => search = Some(parse_search(flags.value(&flag, inline)?)?),
            FLAG_ADDR => addr = Some(flags.value(&flag, inline)?),
            FLAG_FORMAT => format = Some(parse_format(&flags.value(&flag, inline)?)?),
            FLAG_JSON => {
                no_value(&flag, inline)?;
                json_flag = true;
            }
            FLAG_HELP_SHORT | FLAG_HELP_LONG => return help_requested(),
            other if other.starts_with("--") => return Err(format!("unknown option '{other}'")),
            _ => match positional {
                // Named BOTH, for `crate::cmd::data`'s reason: an operator who typed two
                // instruments cannot tell which one this parser kept unless the refusal says.
                Some(had) => {
                    return Err(format!(
                        "two positional arguments ('{had}' and '{flag}') — `data catalog show` \
                         takes exactly one VENUE:SYMBOL"
                    ));
                }
                None => positional = Some(flag),
            },
        }
    }

    // ⚠ THE CONTRADICTION IS REFUSED RATHER THAN RESOLVED, and the sentence is deliberately the
    // one `crate::cmd::data`'s `parse` already gives: `--json` IS `--format json`, so the two can
    // disagree in exactly one way. Picking a winner would silently discard half of what the
    // operator typed. The two spellings are held EQUAL by
    // `the_two_groups_refuse_the_json_format_contradiction_in_the_same_words`, which is what stops
    // one group's wording drifting away from the other's without a shared const to import.
    let json = match (format, json_flag) {
        (Some(Format::Table), true) => {
            return Err("--json and --format table ask for two different renderings. `--json` IS \
                 `--format json` — pass one"
                .to_string());
        }
        (Some(f), _) => f == Format::Json,
        (None, given) => given,
    };

    let instrument = match (verb, positional) {
        (Verb::Show, Some(spec)) => Some(parse_instrument(&spec)?),
        (Verb::Show, None) => {
            return Err(
                "`data catalog show` needs an instrument: VENUE:SYMBOL (e.g. binance:BTCUSDT)"
                    .to_string(),
            );
        }
        (v, Some(spec)) => {
            return Err(format!(
                "`{}` takes no positional argument ('{spec}') — an instrument is `data catalog \
                 show VENUE:SYMBOL`",
                v.as_str()
            ));
        }
        (_, None) => None,
    };

    match verb {
        Verb::Ls | Verb::Refresh => {
            let Some(slug) = venue.as_deref() else {
                return Err(format!(
                    "`{}` needs --venue: a catalog is per VENUE, and there is no roster-wide \
                     listing on this wire. `data catalog venues` is the roster",
                    verb.as_str()
                ));
            };
            // ⚠ **The SHAPE is refused HERE, by this binary, before a socket is opened.**
            // `vike_datahub_client::catalog::validate_catalog_venue` is the same function the
            // server's own door calls, and that module's doc says the client copy exists "for the
            // message and never for the enforcement" — i.e. to be called exactly here. Until it
            // was, `--venue BINANCE` (or `bin@nce`, or the empty string a shell variable expanded
            // to) opened a connection and the operator's diagnosis depended on who was on the
            // port: with no datahub up, exit 3 `cannot connect to datahub at …` — a connect-class
            // rung a WRAPPER RETRIES — and against a server with no catalog lane, the `does not
            // advertise venue_catalog` message. Neither ever mentioned the slug.
            //
            // The sentence is the client's own rather than one minted here, for
            // [`refuse_an_unlistable_venue`]'s reason: one refusal, one wording, wherever it is
            // reached from. ⚠ It deliberately never echoes the value back — that validator's doc
            // argues why, and prefixing the flag name is all this side adds.
            //
            // ⚠ `show`'s venue is NOT put through this: that half of the VENUE:SYMBOL positional
            // reaches `properties_as_of`, a different wire verb with its own door, and coercing it
            // through the CATALOG validator would refuse a spelling that verb serves.
            validate_catalog_venue(slug).map_err(|why| format!("`--venue`: {why}"))?;
        }
        Verb::Show => {
            if venue.is_some() {
                return Err(
                    "--venue does not apply to `show` — the venue is the FIRST half of the \
                     VENUE:SYMBOL positional, and a second spelling of it could disagree with it"
                        .to_string(),
                );
            }
        }
        Verb::Venues => {
            if venue.is_some() {
                return Err(
                    "--venue does not apply to `venues` — the whole roster IS this verb's answer. \
                     One venue's instruments are `data catalog ls --venue V`"
                        .to_string(),
                );
            }
        }
    }

    if verb != Verb::Ls {
        for (flag, given) in [("--class", class.is_some()), ("--search", search.is_some())] {
            if given {
                return Err(format!(
                    "{flag} does not apply to `{}` — it narrows a LISTING, which is `data catalog \
                     ls --venue V`",
                    verb.as_str()
                ));
            }
        }
    }

    Ok(Args {
        verb,
        venue,
        instrument,
        class,
        search,
        addr: addr
            .or_else(|| {
                // A BLANK rung is skipped rather than honoured, the same rule
                // `crate::cmd::data`'s `parse` applies: an `Environment=` line that set nothing
                // must not aim this at an empty address.
                configured_addr.filter(|s| !s.trim().is_empty()).map(str::to_string)
            })
            .unwrap_or_else(|| DEFAULT_ADDR.to_string()),
        json,
    })
}

/// `--class`'s value, resolved against the model's own stored words, case-insensitively.
///
/// ⚠ **The vocabulary is `vike_model::AssetClass`'s, not this file's**, and the comparison is over
/// `AssetClass::sql_word` — the same word the store persists and the same word
/// `crate::cmd::data`'s `class_cell` renders. A second spelling minted here (`crypto-perp`,
/// `perp`) would be a private vocabulary an operator could not carry between the two groups.
fn parse_class(value: &str) -> Result<AssetClass, String> {
    if value.is_empty() {
        return Err(format!("--class was given an EMPTY value. Name one of: {}", class_roster()));
    }
    AssetClass::ALL
        .iter()
        .copied()
        .find(|c| c.sql_word().eq_ignore_ascii_case(value))
        .ok_or_else(|| format!("unknown `--class {value}` ({})", class_roster()))
}

/// `--search`'s value, refused EMPTY by name — the rule [`parse_class`] and
/// `crate::cmd::data`'s `parse_format` already state for their own values, reached at last by the
/// third narrowing flag.
///
/// ⚠ **An empty needle is not a narrow filter, it is NO filter wearing one.** `contains("")` is
/// true for every row, so the whole listing renders under a summary that says a filter selected it
/// (`20000 of 20000 instruments`), the document echoes `"search": ""` as though one had run, and a
/// TRUNCATED listing additionally earns [`truncation_warning`]'s ⚠ — a warning that a filter may
/// be hiding the tail, for a filter that selected nothing out. The shape that produces one is a
/// shell variable that expanded to nothing, which is exactly the case an operator cannot see in
/// their own scrollback.
///
/// WHITESPACE is not refused, deliberately: a description genuinely contains spaces, so `--search
/// " perp"` is a needle rather than an accident. Only the value that matches everything is.
fn parse_search(value: String) -> Result<String, String> {
    if value.is_empty() {
        return Err(
            "--search was given an EMPTY value, which keeps every row rather than narrowing \
             anything. Name the text to look for, or drop the flag."
                .to_string(),
        );
    }
    Ok(value)
}

/// The `VENUE:SYMBOL` positional's SHAPE — two non-empty parts and nothing else.
///
/// ⚠ **A three-part spelling is refused BY NAME rather than as a shape error**, because it is not
/// a typo: `VENUE:SYMBOL:INTERVAL` is the grammar `crate::cmd::data`'s `check_spec` enforces one
/// group over, and an operator arriving from `data hist fetch` will type it. An interval is a
/// property of a SERIES; an instrument has none, so the refusal says which verb wants the third
/// part rather than telling them their line is malformed.
///
/// Neither half is validated further, for the reason `crate::cmd::data`'s module doc gives: the
/// reachable set is a property of a remote process this crate cannot see at parse time.
fn parse_instrument(spec: &str) -> Result<InstrumentRef, String> {
    let parts: Vec<&str> = spec.split(':').collect();
    if parts.len() == 3 {
        return Err(format!(
            "'{spec}' names a SERIES, not an instrument — the third part is a bar INTERVAL and an \
             instrument has none. `data catalog show VENUE:SYMBOL`; the interval belongs to the \
             `data hist` verbs"
        ));
    }
    if parts.len() != 2 || parts.iter().any(|p| p.trim().is_empty()) {
        return Err(format!(
            "'{spec}' is not VENUE:SYMBOL — two non-empty parts, e.g. binance:BTCUSDT"
        ));
    }
    Ok(InstrumentRef { venue: parts[0].to_string(), symbol: parts[1].to_string() })
}
