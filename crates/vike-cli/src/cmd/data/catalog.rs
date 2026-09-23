//! `vike-cli data catalog` — WHAT IS ADDRESSABLE, the surface design's §7 third group.
//!
//! ⚠ **A GROUP owns its own grammar, and that is why this is a module rather than four more
//! `Sub` arms.** `crate::cmd::data`'s [`super::Args`] is one struct answering for the `hist`
//! group's eleven verbs, and every flag it carries is refused by name on the verbs it does not
//! belong to — a discipline that works because those eleven verbs share a vocabulary (a series, a
//! window, a store). This group does not share it: its noun is an INSTRUMENT the venue lists, not
//! a series the store holds, so folding it into that struct would make one type answer for two
//! command languages and every refusal in it ambiguous.
//!
//! # The four verbs
//!
//! | verb | wire | answers |
//! |---|---|---|
//! | `ls --venue V [--class C] [--search TEXT]` | `venue_catalog` | what this venue lists |
//! | `show VENUE:SYMBOL` | `properties_as_of` | tick size, lot size, asset class |
//! | `refresh --venue V` | `venue_catalog` | re-ask the venue, not the cache |
//! | `venues` | LOCAL + the handshake | THE CAPABILITY MATRIX (§8.1) |
//!
//! ⚠ `venues` is the one nobody in the competitive set ships, and the one that must NOT become a
//! fifth way to list venues. Its columns come from the LOCAL build and its last column from the
//! SERVER that answered, so it also shows a version skew — which is the failure an operator cannot
//! otherwise see.
//!
//! # ⚠ `ls` and `show` read TWO DIFFERENT SOURCES, and the verbs sit side by side
//!
//! `ls` asks the datahub to ask the VENUE: [`vike_datahub_client::DatahubClient::venue_catalog`]
//! is a live listing, and its instruments carry whatever grid the venue published in that same
//! answer. `show` asks the datahub about its own STORE:
//! [`vike_datahub_client::DatahubClient::properties_as_of`] reads the `kind=properties` tape a
//! recorder wrote, which can be older than the venue, can be absent entirely, and — for a venue
//! nothing has ever recorded — is absent for every symbol on it.
//!
//! So an instrument can appear in `ls` and have nothing for `show`, and the two are not in
//! disagreement: they answer about different things. Both renderings NAME their source, because
//! the one failure this pairing invites is reading a `show` absence as "the venue does not list
//! it". It is the same distinction `crate::cmd::data`'s `ClassProbe` draws one group over, and it
//! is why this module reuses that flag's as-of instant rather than minting a second one.
//!
//! # ⚠ `refresh` CANNOT bypass the server's memo, and says so rather than implying it
//!
//! [`vike_datahub_client::proto::Request::VenueCatalog`] carries no force bit — the memo and its
//! TTL are `crates/vike-datahub/src/catalog.rs`'s `CatalogLane`, whose admission this side cannot
//! reach. What the server DOES report is which arm answered
//! ([`CatalogOutcome::Listed`]'s `cached`), so this verb reports that verbatim: a `cached` answer
//! is told, in as many words, that NOTHING was re-asked. A verb whose name promises a fetch and
//! whose wire cannot make one has to spend its output on the difference, or it teaches an operator
//! that a stale symbol was just confirmed fresh.

use std::io;
use std::process::ExitCode;

use vike_datahub_client::catalog::{
    CatalogListing, CatalogOutcome, CatalogRefusal, validate_catalog_venue,
};
use vike_datahub_client::{
    DatahubClient, FEATURE_BACKFILL, FEATURE_VENUE_CATALOG, NodeKeys, Scope, advertised_md_venues,
};
use vike_model::AssetClass;
use vike_model::venue_caps::{LiveDataCaps, caps_for};

use crate::cmd::args::{Flags, exit_for_parse_error, help_requested, no_value};
use crate::exit::{CliError, CmdResult};

use super::{CLASS_AS_OF_TS, DEFAULT_ADDR, Format, col, connect, empty_note, parse_format};

/// What [`exit_for_parse_error`] and the failure line name this command. The GROUP is part of it:
/// `vike-cli data: …` for a line typed under `data catalog` would send a reader to the `hist`
/// group's usage, which is the page that does not contain the flag they got wrong.
const COMMAND: &str = "data catalog";

/// Which verb ran.
///
/// Adding one is FIVE edits and the COMPILER asks for four of them: an arm here, an arm in
/// [`Verb::as_str`], an arm in [`Verb::usage_block`] and an arm in [`execute`]. The fifth — a row
/// in [`VERBS`] — is the one nothing forces, and it is the one that costs the most: [`usage`] and
/// the missing-verb refusal are both RENDERED from that roster, so a verb absent from it is a verb
/// an operator can neither discover nor be told about, while still parsing.
///
/// ⚠ **This said FOUR edits and named `usage` as the unforced one, and that stopped being true
/// with [`Verb::usage_block`].** The usage page used to carry each verb's paragraph as a literal
/// inside one `format!`, so a verb could be documented nowhere while everything compiled and every
/// test stayed green — see
/// `the_usage_renders_a_block_for_every_verb_and_a_row_for_every_flag_it_declares` for the measured
/// reason the test that claimed to catch it could not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verb {
    /// `ls --venue V` — the venue's own instrument universe, filtered client-side.
    Ls,
    /// `show VENUE:SYMBOL` — one instrument's recorded grid, out of the STORE. See the module doc
    /// on why that is a different source from [`Verb::Ls`]'s.
    Show,
    /// `refresh --venue V` — the same wire call as [`Verb::Ls`], rendering the COUNTS rather than
    /// the rows, and reporting whether the server actually called the venue.
    Refresh,
    /// `venues` — THE CAPABILITY MATRIX. The one verb here that reaches no wire verb at all: its
    /// rows are this build's own declarations and its last column is the handshake the connection
    /// already performed.
    Venues,
}

/// Every verb, in the order [`usage`] lists them. It exists so the "a verb is required (…)"
/// refusal is DERIVED rather than typed — the failure `crate::cmd::data`'s `SUBCOMMANDS` was
/// introduced for, where the one message whose whole job is to name the roster named it short.
const VERBS: &[Verb] = &[Verb::Ls, Verb::Show, Verb::Refresh, Verb::Venues];

impl Verb {
    /// The name the operator typed, which is also what every refusal names it by and what the
    /// `--json` document carries as its `verb` field.
    fn as_str(self) -> &'static str {
        match self {
            Verb::Ls => "ls",
            Verb::Show => "show",
            Verb::Refresh => "refresh",
            Verb::Venues => "venues",
        }
    }

    /// This verb's whole block of [`usage`]: the first line goes BESIDE the verb name, the rest
    /// under it at the same indent.
    ///
    /// ⚠ **The completeness is the COMPILER's**, which is the point of the shape: [`usage`]
    /// renders one block per [`VERBS`] row through this match, so a verb with no documentation
    /// does not compile and a block cannot be deleted without deleting an arm. The page used to
    /// carry these paragraphs as literals inside one `format!` — deleting one left a verb
    /// undiscoverable from `--help` with nothing red anywhere, because every verb NAME also occurs
    /// in the page's prose.
    ///
    /// Lines carry [`expand`]'s tokens rather than interpolating: a `&'static str` cannot
    /// `format!`, and the two values that must not be typed here are exactly the two this
    /// repository has watched rot.
    fn usage_block(self) -> &'static [&'static str] {
        match self {
            Verb::Ls => &[
                "--venue V [--class C] [--search TEXT]",
                "the venue's own instrument universe, asked through the datahub. A venue that",
                "publishes NO bulk list, one that would cost the operator's own credentials,",
                "and one this server's build does not carry are three DIFFERENT answers and",
                "none of them renders as an empty list",
            ],
            Verb::Show => &[
                "VENUE:SYMBOL",
                "one instrument's recorded grid — tick size, lot step, the quantity and",
                "notional floors, and the ASSET CLASS a venue producer recorded. ⚠ This reads",
                "the datahub's STORE (its kind=properties tape), not the venue: an instrument",
                "`ls` lists may have nothing here, which means nothing has RECORDED it",
            ],
            Verb::Refresh => &[
                "--venue V",
                "the same question as `ls`, rendering the counts rather than the rows. ⚠ It",
                "cannot force a fetch — this wire carries no force bit — so it reports which",
                "arm answered: a server that replied from its own memo is SAID to have",
                "re-asked nothing",
            ],
            Verb::Venues => &[
                "THE CAPABILITY MATRIX. Per venue: the live lanes and the backfill kinds THIS",
                "BUILD declares, beside the venues the datahub that answered actually serves",
                "live market data for. The two halves are never merged into one verdict —",
                "reading which side said no is the whole point, and a version skew between a",
                "binary and its server is visible nowhere else. ⚠ It needs no datahub: with",
                "none reachable the local columns still answer and the server column says so",
            ],
        }
    }
}

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
fn class_roster() -> String {
    AssetClass::SQL_WORDS.join(" | ")
}

/// Every flag spelling [`parse`] accepts, written ONCE each.
///
/// ⚠ **A `const` rather than a literal at the match arm, and it is load-bearing.** A flag is named
/// in exactly two places — the parser's arm and its usage row — and while those are two literals
/// the page can lose a flag the parser still takes, or keep one whose arm was deleted, with
/// nothing to notice. A `&'static str` const is a legal MATCH PATTERN, so the arm and the row
/// become one declaration seen twice.
const FLAG_VENUE: &str = "--venue";
const FLAG_CLASS: &str = "--class";
const FLAG_SEARCH: &str = "--search";
const FLAG_ADDR: &str = "--addr";
const FLAG_FORMAT: &str = "--format";
const FLAG_JSON: &str = "--json";
const FLAG_HELP_SHORT: &str = "-h";
const FLAG_HELP_LONG: &str = "--help";

/// One row of [`usage`]'s `options:` block, and the declaration of one flag.
struct FlagDoc {
    /// Every spelling [`parse`] accepts for this row, from the consts above — never a second
    /// literal. More than one only for `-h, --help`, which is one flag with two names.
    spellings: &'static [&'static str],
    /// The value placeholder the page renders after the spelling (`V`, `H:P`), EMPTY for a flag
    /// that takes none. It is also what `every_declared_flag_is_accepted_by_the_parser` reads to
    /// decide whether to feed the probe a value.
    arg: &'static str,
    /// The help, pre-wrapped to this page's width; the first line goes beside the label. Carries
    /// [`expand`]'s tokens rather than interpolating — see [`Verb::usage_block`].
    help: &'static [&'static str],
}

impl FlagDoc {
    /// The label the page puts in its left column — the spellings joined, plus the placeholder.
    fn label(&self) -> String {
        let spellings = self.spellings.join(", ");
        if self.arg.is_empty() { spellings } else { format!("{spellings} {}", self.arg) }
    }
}

/// Every flag this group's grammar carries, in the order [`usage`] lists them.
const FLAGS: &[FlagDoc] = &[
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
fn expand(line: &str) -> String {
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
fn usage() -> String {
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

/// The `VENUE:SYMBOL` positional, parsed.
///
/// A struct rather than two `Option<String>`s on [`Args`], for the reason `crate::cmd::data`'s
/// `RmArgs` gives for its own: an `Args` that can hold half an instrument is an `Args` some future
/// arm will read one from.
#[derive(Debug, Clone, PartialEq, Eq)]
struct InstrumentRef {
    venue: String,
    symbol: String,
}

/// The parsed `data catalog …` line. PURE — the whole grammar is unit-tested below.
#[derive(Debug, PartialEq, Eq)]
struct Args {
    verb: Verb,
    /// `--venue V`. REQUIRED on `ls`/`refresh`, refused on the other two.
    venue: Option<String>,
    /// The `VENUE:SYMBOL` positional. `Some` only on `show`, where it is required.
    instrument: Option<InstrumentRef>,
    /// `--class C`, resolved to the model's own variant at PARSE time so no downstream site
    /// re-reads an operator's spelling.
    class: Option<AssetClass>,
    /// `--search TEXT`, verbatim; the comparison is case-insensitive at the filter.
    search: Option<String>,
    /// `--addr`, resolved through the same ladder `crate::cmd::data`'s `parse` uses: the flag,
    /// then the configured address, then [`DEFAULT_ADDR`].
    addr: String,
    /// `--json` / `--format json`. ONE field, deliberately — a second saying the same thing in
    /// different words is how the two come to disagree.
    json: bool,
}

/// Parse this group's argv tail (everything after the group word). PURE — no I/O, no socket.
///
/// ⚠ Every flag is accepted by the ONE loop below and refused PER VERB afterwards, rather than
/// being routed by a per-verb match. That ordering is `crate::cmd::data`'s `parse`'s and it buys
/// the same thing: an inapplicable flag is named in a message that says which verb it DOES belong
/// to, where an unknown-option error would tell an operator the flag does not exist — which is
/// false, and sends them looking in the wrong place.
fn parse(argv: &[String], configured_addr: Option<&str>) -> Result<Args, String> {
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

/// Run a `data catalog …` line. `argv` is everything AFTER the group word.
pub(super) fn run(
    argv: &[String],
    keys: Option<&NodeKeys>,
    configured_addr: Option<&str>,
) -> ExitCode {
    let args = match parse(argv, configured_addr) {
        Ok(a) => a,
        Err(msg) => return exit_for_parse_error(COMMAND, &usage(), &msg),
    };
    match execute(&args, keys) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vike-cli {COMMAND}: {}", e.msg);
            e.exit.into()
        }
    }
}

/// Route the parsed line. Nothing is shared between the arms but this dispatch and the exit
/// ladder — `venues` does not even open the same socket the other three require.
fn execute(args: &Args, keys: Option<&NodeKeys>) -> CmdResult<()> {
    match args.verb {
        Verb::Ls => execute_ls(args, keys),
        Verb::Show => execute_show(args, keys),
        Verb::Refresh => execute_refresh(args, keys),
        Verb::Venues => execute_venues(args, keys),
    }
}

// ─── `ls` and `refresh`: the venue's own list ───────────────────────────────────────────────────

/// One instrument, flattened out of the wire type on arrival.
///
/// ⚠ **It has to be flattened, and that is a property of this crate rather than a preference.**
/// `vike_catalog::Instrument` cannot be NAMED here — `vike-catalog` is not a dependency of
/// `crates/vike-cli/Cargo.toml` and adding it to reach one struct would pull the whole catalog
/// tree into a crate whose manifest argues edge by edge against exactly that. It is the same wall
/// `crate::cmd::data::tape_health`'s module doc records for `vike_data::TsRange`, and the same
/// answer: convert at the one site where type inference supplies the name, and carry plain fields
/// everywhere else.
///
/// ⚠ `class` is NOT an `Option`, unlike the store's. A catalog instrument always names a class;
/// [`execute_show`]'s source is `vike_model::SymbolProperties`, whose `asset_class` is an `Option`
/// precisely so "the venue told us" and "nobody said" stay distinguishable. The two verbs
/// therefore render the class differently and must.
#[derive(Debug, Clone, PartialEq)]
struct InstrumentRow {
    symbol: String,
    class: AssetClass,
    base: String,
    quote: String,
    /// `SymbolProperties::tick_size` as the venue published it in THIS listing.
    tick: f64,
    /// `SymbolProperties::step_size` — the lot step.
    lot: f64,
    description: String,
}

/// Does this row survive the client-side filters? Both are ANDed and an absent one matches
/// everything — the same browse-aid contract `crate::cmd::data`'s `Filter` states, and for the
/// same reason: nothing here reaches the wire, so a filter can never make the server or the venue
/// do less work.
fn keeps(row: &InstrumentRow, class: Option<AssetClass>, search: Option<&str>) -> bool {
    if matches!(class, Some(c) if row.class != c) {
        return false;
    }
    match search {
        None => true,
        Some(needle) => {
            let needle = needle.to_lowercase();
            [&row.symbol, &row.base, &row.quote, &row.description]
                .iter()
                .any(|field| field.to_lowercase().contains(&needle))
        }
    }
}

/// A grid number, or `-` when the venue published none.
///
/// ⚠ **`0` is the model's ABSENT, not a tick size.** `vike_model::SymbolProperties`' own doc states
/// the convention — *"every field is `Default`-zero-or-`None` because the whole convention is
/// absent-is-`0`"* — and `vike_catalog::Instrument`'s says the properties *"may be `Default` until
/// fetched"*. Printing `0` for a tick size would read as a fact about the instrument, which is the
/// blank-cell defect `crate::cmd::data`'s `class_cell` argues against one group over.
///
/// The comparison is `<= 0.0` rather than `== 0.0`: a negative grid is nonsense from any venue, so
/// folding it into the same cell says the same true thing and costs no float-equality test.
fn grid_cell(v: f64) -> String {
    if v <= 0.0 { "-".to_string() } else { v.to_string() }
}

/// `data catalog ls` — one `venue_catalog` round trip, filtered here, rendered here.
fn execute_ls(args: &Args, keys: Option<&NodeKeys>) -> CmdResult<()> {
    let listing = fetch_listing(args, keys)?;
    let CatalogOutcome::Listed { instruments, truncated, .. } = &listing.outcome else {
        // [`outcome_only_json`], not [`ls_json`]: a refused listing carries no `instruments` key
        // at all, and an empty array there would be the collapse [`outcome_json`] prevents.
        return refuse_an_unlistable_venue(args, &listing, outcome_only_json);
    };
    // The one site where inference supplies `vike_catalog::Instrument`'s name — see
    // [`InstrumentRow`] for why it may not be written.
    let all: Vec<InstrumentRow> = instruments
        .iter()
        .map(|i| InstrumentRow {
            symbol: i.raw_symbol.clone(),
            class: i.asset_class,
            base: i.base.clone(),
            quote: i.quote.clone(),
            tick: i.properties.tick_size,
            lot: i.properties.step_size,
            description: i.description.clone(),
        })
        .collect();
    let narrowed = args.class.is_some() || args.search.is_some();
    let rows: Vec<InstrumentRow> =
        all.into_iter().filter(|r| keeps(r, args.class, args.search.as_deref())).collect();

    if args.json {
        println!("{}", ls_json(args, &listing, &rows));
    } else {
        for line in ls_lines(&rows, instruments.len(), narrowed, *truncated, &listing.describe()) {
            println!("{line}");
        }
    }
    Ok(())
}

/// `data catalog refresh` — the same wire call, rendering the COUNTS and which arm answered.
///
/// ⚠ It ships no instrument rows deliberately. `ls` is the listing; this verb's whole product is
/// the answer to "did that actually reach the venue", and printing twenty thousand symbols under
/// it would bury the one line it exists for.
fn execute_refresh(args: &Args, keys: Option<&NodeKeys>) -> CmdResult<()> {
    let listing = fetch_listing(args, keys)?;
    let CatalogOutcome::Listed { instruments, cached, .. } = &listing.outcome else {
        // ⚠ **[`refresh_json`], not [`outcome_only_json`], and the difference is this verb's whole
        // product.** This arm used to emit the shared refusal document — the one `ls` emits — so a
        // `refresh --json` against an un-enumerable venue carried NO `reasked` key at all, while
        // [`refresh_json`]'s own ⚠ described a `reasked: null` document that nothing ever emitted.
        // A consumer of THIS verb branches on that field; making it absent on exactly the answers
        // where "did anything reach the venue" is most in doubt is the shape that teaches one to
        // treat a refusal as a `false`.
        return refuse_an_unlistable_venue(args, &listing, refresh_json);
    };
    if args.json {
        println!("{}", refresh_json(args, &listing));
    } else {
        for line in refresh_lines(instruments.len(), *cached, &listing.describe()) {
            println!("{line}");
        }
    }
    Ok(())
}

/// The one `venue_catalog` call both verbs make.
///
/// ⚠ The two failure classes are kept apart exactly as [`connect`] keeps them: a socket that could
/// not be reached is [`crate::exit::Exit::Connect`] — the rung a wrapper retries on — while a
/// SERVED error (the server does not advertise the lane, the venue slug was refused, the provider
/// itself failed) rides `?` onto [`crate::exit::Exit::Failed`], because the far side spoke.
/// ⚠ The venue is `unwrap_or_default`ed rather than `expect`ed, and the degenerate path is a clean
/// refusal rather than a panic: [`parse`] already requires `--venue` on both callers, and an empty
/// slug that somehow reached here is refused by
/// `vike_datahub_client::catalog::validate_catalog_venue` inside the client with a sentence naming
/// what a venue slug is. A renderer has no business aborting a run over a state its own parser
/// forbids.
fn fetch_listing(args: &Args, keys: Option<&NodeKeys>) -> CmdResult<CatalogListing> {
    let venue = args.venue.as_deref().unwrap_or_default();
    let mut client = connect(&args.addr, keys, Scope::Read)?;
    Ok(client.venue_catalog(venue)?)
}

/// The three NON-`Listed` outcomes, answered once for both verbs.
///
/// `document` is the verb's own `--json` renderer, passed in rather than chosen here: `ls` owes
/// [`outcome_only_json`] and `refresh` owes [`refresh_json`], whose `reasked` field is the one
/// thing a consumer reads that verb for. Both have the same signature, so the choice is a
/// function ITEM and costs nothing at the call site.
///
/// ⚠ **[`crate::exit::Exit::Empty`], not `Failed`, and not success either.** That rung exists so
/// that "the answer was nothing" and "nothing was evaluated" stop sharing a number, which is
/// precisely this distinction: a venue listed with zero instruments EXITS ZERO, while a venue that
/// could not be enumerated at all exits here. Collapsing them would put
/// `vike_datahub_client::catalog::CatalogOutcome`'s whole reason for being an enum back into a
/// pipeline's exit code — `data catalog ls --venue ig | wc -l` would read `0` for both.
///
/// It is not `Failed` because none of the three is a failure: `NotArmed` is the server's operator
/// doing what they wrote down, and both venue-shaped refusals are facts about the world. The
/// sentence is the WIRE's (`CatalogListing::describe`), never one assembled here, so the daemon,
/// the desktop and this CLI cannot describe one refusal three ways.
fn refuse_an_unlistable_venue(
    args: &Args,
    listing: &CatalogListing,
    document: fn(&Args, &CatalogListing) -> String,
) -> CmdResult<()> {
    if args.json {
        // stdout stays the document and nothing else; the sentence lands on stderr through
        // [`run`], which is the split `crate::cmd::data`'s module doc already states for `--json`.
        println!("{}", document(args, listing));
    }
    Err(CliError::empty(listing.describe()))
}

/// `ls`'s human rendering.
///
/// `listed` is what the SERVER sent, before the client-side filters; `narrowed` says whether any
/// filter ran. The pair is what lets [`empty_note`] tell "this venue lists nothing" apart from
/// "your filter selected nothing" — two opposite answers that a bare "nothing found" merges.
fn ls_lines(
    rows: &[InstrumentRow],
    listed: usize,
    narrowed: bool,
    truncated: bool,
    describe: &str,
) -> Vec<String> {
    let mut lines = Vec::new();
    if rows.is_empty() {
        lines.push(empty_note("instruments", listed, narrowed));
        lines.push(describe.to_string());
        lines.extend(truncation_warning(truncated, narrowed));
        return lines;
    }
    let sym_w = col("SYMBOL", rows.iter().map(|r| r.symbol.len()));
    let class_w = col("CLASS", rows.iter().map(|r| r.class.sql_word().len()));
    let base_w = col("BASE", rows.iter().map(|r| r.base.len()));
    let quote_w = col("QUOTE", rows.iter().map(|r| r.quote.len()));
    let tick_w = col("TICK", rows.iter().map(|r| grid_cell(r.tick).len()));
    let lot_w = col("LOT", rows.iter().map(|r| grid_cell(r.lot).len()));
    lines.push(format!(
        "{:<sym_w$}  {:<class_w$}  {:<base_w$}  {:<quote_w$}  {:>tick_w$}  {:>lot_w$}  DESCRIPTION",
        "SYMBOL", "CLASS", "BASE", "QUOTE", "TICK", "LOT"
    ));
    for r in rows {
        lines.push(format!(
            "{:<sym_w$}  {:<class_w$}  {:<base_w$}  {:<quote_w$}  {:>tick_w$}  {:>lot_w$}  {}",
            r.symbol,
            r.class.sql_word(),
            r.base,
            r.quote,
            grid_cell(r.tick),
            grid_cell(r.lot),
            r.description
        ));
    }
    lines.push(String::new());
    lines.push(if narrowed {
        format!("{} of {listed} instruments · {describe}", rows.len())
    } else {
        describe.to_string()
    });
    lines.extend(truncation_warning(truncated, narrowed));
    lines
}

/// The line a TRUNCATED listing owes a FILTERED reader, and that the wire's own sentence cannot
/// give.
///
/// `CatalogListing::describe` already says a listing was truncated. What it cannot know is that a
/// filter then ran on this side: the instrument somebody searched for may be in the tail the
/// server never sent, so an empty or short result under a filter is NOT evidence the venue does
/// not list it. Without a filter the wire's own sentence is the whole story and this adds nothing.
fn truncation_warning(truncated: bool, narrowed: bool) -> Vec<String> {
    if truncated && narrowed {
        vec![
            "⚠ the listing was TRUNCATED before this filter ran, so an instrument missing here \
             may be in the tail the server did not send rather than absent from the venue."
                .to_string(),
        ]
    } else {
        Vec::new()
    }
}

/// `refresh`'s human rendering — the wire's own sentence, plus the one fact it does not state
/// outright.
fn refresh_lines(count: usize, cached: bool, describe: &str) -> Vec<String> {
    let mut lines = vec![describe.to_string()];
    lines.push(if cached {
        // The honest half. See the module doc: this wire carries no force bit, so a `cached`
        // answer means this verb re-asked NOTHING and an operator must not read it as a fetch.
        "⚠ NOTHING was re-asked: the server answered from its own in-process memo, whose TTL is \
         that server's and which no flag on this wire can bypass. The count above is up to that \
         TTL old."
            .to_string()
    } else {
        format!("the server called the venue and now holds {count} instruments for it.")
    });
    lines
}

// ─── `show`: one instrument's recorded grid ─────────────────────────────────────────────────────

/// `data catalog show VENUE:SYMBOL` — one `properties_as_of` round trip against the datahub's own
/// store.
///
/// ⚠ **The as-of instant is `crate::cmd::data`'s [`CLASS_AS_OF_TS`], reused rather than re-chosen.**
/// That constant's doc argues the whole decision — a PIT tape answered at `i64::MAX` is "the
/// latest row on record", and every row of one instrument must ask the same question or two
/// renderings of it can disagree. `data hist ls --class` and this verb answer about the same tape,
/// so a second instant here would be a second answer to one question.
fn execute_show(args: &Args, keys: Option<&NodeKeys>) -> CmdResult<()> {
    // [`parse`] requires the positional on this verb, so the `else` is unreachable — spelled as a
    // usage refusal rather than a panic, for [`fetch_listing`]'s reason.
    let Some(target) = args.instrument.as_ref() else {
        return Err(CliError::usage(
            "`data catalog show` needs an instrument: VENUE:SYMBOL (e.g. binance:BTCUSDT)",
        ));
    };
    let mut client = connect(&args.addr, keys, Scope::Read)?;
    let props = client.properties_as_of(&target.venue, &target.symbol, CLASS_AS_OF_TS)?;
    let Some(props) = props else {
        if args.json {
            println!("{}", show_missing_json(args, target));
        }
        // ⚠ The EMPTY rung, for [`refuse_an_unlistable_venue`]'s reason: nothing was evaluated.
        // The sentence names the OTHER source deliberately — an absence here is a statement about
        // what has been RECORDED, and the venue may well list this instrument.
        return Err(CliError::empty(format!(
            "nothing has recorded an instrument grid for `{}:{}` in the store that datahub \
             opened, so there is nothing to show. That is a RECORDER fact, not a venue one — ask \
             the venue itself with `data catalog ls --venue {} --search {}`",
            target.venue, target.symbol, target.venue, target.symbol
        )));
    };
    if args.json {
        println!("{}", show_json(args, target, &props));
    } else {
        for line in show_lines(target, &props) {
            println!("{line}");
        }
    }
    Ok(())
}

/// `show`'s human rendering: a label/value block rather than a table, because there is one row and
/// a one-row table is a table nobody can read down.
fn show_lines(target: &InstrumentRef, props: &vike_model::SymbolProperties) -> Vec<String> {
    let class = match props.asset_class {
        Some(c) => c.sql_word(),
        // ⚠ The wiring signal, SAID rather than left blank — the identical verdict
        // `crate::cmd::data`'s `class_cell` renders for the same `None`: a grid was recorded and
        // the venue producer named no class for it.
        None => "unclassified",
    };
    let mut lines = vec![
        format!("{}:{}", target.venue, target.symbol),
        String::new(),
        format!("  asset class    {class}"),
        format!("  tick size      {}", grid_cell(props.tick_size)),
        format!("  lot step       {}", grid_cell(props.step_size)),
        format!("  min qty        {}", grid_cell(props.min_qty)),
        format!("  max qty        {}", grid_cell(props.max_qty)),
        format!("  min notional   {}", grid_cell(props.min_notional)),
        format!("  contract size  {}", grid_cell(props.contract_size)),
        String::new(),
        // The module doc's ⚠, at the one place a reader can act on it.
        "recorded in the datahub's kind=properties tape, latest row on record — NOT a live venue \
         query. `data catalog ls --venue <V>` asks the venue itself."
            .to_string(),
    ];
    if props.tick_size <= 0.0 || props.step_size <= 0.0 {
        lines.push(
            "⚠ the recorded grid names no tick size or no lot step. A row exists, so something \
             recorded it — a producer writing a DEFAULT grid is the shape that leaves these empty."
                .to_string(),
        );
    }
    lines
}

// ─── `venues`: THE CAPABILITY MATRIX ────────────────────────────────────────────────────────────

/// One venue's row of the matrix, as THIS BUILD declares it.
///
/// ⚠ **Both halves come from `vike_model::venue_caps`, and the backfill half is below a LAYER
/// WALL.** `crates/vike-cli/Cargo.toml` declares no `vike-backfill` edge and must not grow one —
/// that crate drags DataFusion and the whole collector tree into a binary whose identity is being
/// free of both, which CI's `light-consumers` lane exists to catch. The backfill columns are
/// reachable anyway because `vike_model::venue_caps::VenueCaps` carries `backfill_bars` /
/// `backfill_ticks`, and that file's own doc records them CROSS-PINNED to
/// `vike_backfill::caps::backfill_caps` by `venue_caps_cross_pin_the_backfill_table` in that
/// crate. So this is the same answer, held equal by a gate, on the right side of the wall.
#[derive(Debug, Clone, PartialEq, Eq)]
struct VenueRow {
    venue: &'static str,
    /// The live market-data lanes this build's adapter serves, in [`live_lanes`]' order.
    live: Vec<&'static str>,
    backfill_bars: bool,
    backfill_ticks: bool,
}

/// The lanes a declared [`LiveDataCaps`] serves, named.
///
/// ⚠ **The destructuring is the completeness gate and it is the COMPILER's**, not a test's: a
/// sixth lane added to `vike_model::venue_caps::LiveDataCaps` makes this pattern fail to compile,
/// because a struct pattern without `..` must bind every field. That is the same property
/// `vike_model::VENUES`' own roster test buys by walking the tree — a new thing cannot be silently
/// absent — bought here for free.
fn live_lanes(caps: &LiveDataCaps) -> Vec<&'static str> {
    let LiveDataCaps { bars, quotes, trades, book, depth } = *caps;
    [("bars", bars), ("quotes", quotes), ("trades", trades), ("book", book), ("depth", depth)]
        .into_iter()
        .filter(|(_, on)| *on)
        .map(|(name, _)| name)
        .collect()
}

/// The whole matrix, DERIVED from the canonical roster. Never a venue list typed here: every one
/// of those in this repository has rotted, and `vike_model::VENUES` is the roster every per-venue
/// table already iterates.
fn matrix() -> Vec<VenueRow> {
    vike_model::VENUES
        .iter()
        .map(|venue| {
            let caps = caps_for(venue);
            VenueRow {
                venue,
                live: live_lanes(&caps.live_data),
                backfill_bars: caps.backfill_bars,
                backfill_ticks: caps.backfill_ticks,
            }
        })
        .collect()
}

/// What the SERVER said, or why it could not be asked.
///
/// ⚠ An enum rather than a `Vec<String>` plus a flag: an unreachable server and a server that
/// advertised nothing are different facts, and a flat empty list would render them identically —
/// the same collapse `vike_datahub_client::catalog::CatalogOutcome` is an enum to prevent.
///
/// ⚠ **It had TWO variants and needed three.** A server that completed a `Welcome` and then
/// REFUSED the connection — a PROTO_VERSION skew, a mac it would not take, a keyed server with no
/// keys in this box's store — arrived as [`ServerView::Unreachable`], so the verb whose module doc
/// says it exists to surface a version skew rendered exactly that as an ABSENT datahub: `NOT
/// REACHED`, `?` in every LIVE FEED cell, and `"reachable": false` in the document. The server was
/// reached, answered, and said no, which is the "which side said no" distinction this whole verb
/// is built around. See [`ask_the_server`] for where the three states are told apart.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ServerView {
    /// The handshake completed; these are the features that `Welcome` carried.
    Answered(Vec<String>),
    /// The far side SPOKE and this connection did not survive it — see [`answered_and_refused`]
    /// for the exact set. Carries the client's own sentence, which names the cause.
    Refused(String),
    /// Nothing answered, so this build's columns are the whole answer. Carries the client's own
    /// sentence.
    Unreachable(String),
}

impl ServerView {
    /// The venues this server advertised a live market-data client for — the READ half of
    /// `vike_datahub_client::proto::md_venue_feature`, and the ONE genuinely per-venue fact a
    /// handshake carries.
    fn md_venues(&self) -> Vec<String> {
        match self {
            ServerView::Answered(f) => advertised_md_venues(f),
            // A refused connection advertised nothing this side may read: the `Welcome`'s features
            // are discarded with the connection, and treating a half-completed handshake as an
            // answer is the merge the third variant exists to prevent.
            ServerView::Refused(_) | ServerView::Unreachable(_) => Vec::new(),
        }
    }

    /// Whether this server advertised one named capability, or `None` when this side holds no
    /// advertisement it may read. `None` is not `false`: "it said no" and "there is no answer here"
    /// are different, and the whole verb is about not merging those.
    ///
    /// ⚠ `None` on [`ServerView::Refused`] is not the same fact as `None` on
    /// [`ServerView::Unreachable`], and only the second means nobody was asked: a refused
    /// connection may have carried a full `Welcome` that this side dropped —
    /// [`ServerView::md_venues`] is where that happens and why.
    fn serves(&self, feature: &str) -> Option<bool> {
        match self {
            ServerView::Answered(f) => Some(f.iter().any(|x| x == feature)),
            ServerView::Refused(_) | ServerView::Unreachable(_) => None,
        }
    }

    /// This view as a STABLE machine token, for the `--json` document.
    ///
    /// ⚠ **A token where the document shipped a `reachable` BOOLEAN**, and the boolean was not
    /// merely lossy but WRONG: a server that answered the handshake and refused the connection was
    /// reached, and `"reachable": false` told a consumer the opposite of what happened. Three
    /// states do not fit in a bool — the same argument [`outcome_json`] makes one verb over, and
    /// the same one `vike_datahub_client::catalog::CatalogOutcome` is an enum for.
    fn state(&self) -> &'static str {
        match self {
            ServerView::Answered(_) => "answered",
            ServerView::Refused(_) => "refused",
            ServerView::Unreachable(_) => "unreachable",
        }
    }
}

/// Which `io::ErrorKind`s mean the far side SPOKE.
///
/// `PermissionDenied` is the client's own classification of a served refusal — a denied mac, a
/// scope this key does not hold, or a server advertising `auth` against a box with no node keys in
/// its store. `InvalidData` is every way the answer was not one this client can proceed on: a
/// PROTO_VERSION mismatch (`DatahubClient`'s `check_proto_version`, wrapped at that kind), a
/// `Welcome` carrying no nonce on a keyed server, and a reply that is not a datahub's at all.
///
/// ⚠ The last one is why this is deliberately a claim about the CONNECTION rather than about the
/// peer's identity: a wrong service on the port lands in `InvalidData` too, and reporting *that*
/// as "reached and refused" is honest — something answered — where reporting it as an absent
/// datahub is not. Everything else (refused, timed out, unresolvable) is the socket, and is the
/// one case where nothing was said.
fn answered_and_refused(kind: io::ErrorKind) -> bool {
    matches!(kind, io::ErrorKind::PermissionDenied | io::ErrorKind::InvalidData)
}

/// `venues`' own handshake — the ONE place in this group that does not go through
/// `crate::cmd::data`'s [`connect`].
///
/// ⚠ **It must not, and that is deliberate rather than an oversight.** [`connect`] folds every
/// `DatahubClient::connect*` failure into one `CliError::connect` sentence, which is right for the
/// three verbs that need a working connection: there, the only actionable fact is that they have
/// no answer. This verb's entire product is the distinction, so it reads the `io::ErrorKind` the
/// client already classifies (see [`answered_and_refused`]) and keeps the two apart.
///
/// The message carried is the client's own, WITHOUT [`connect`]'s `cannot connect to datahub at
/// {addr}` prefix: [`venues_lines`] names the address on that same line, so the prefix printed it
/// twice — and asserted "cannot connect" about connections that had connected.
fn ask_the_server(addr: &str, keys: Option<&NodeKeys>) -> ServerView {
    let opened = match keys {
        Some(k) => DatahubClient::connect_authed(addr, k, Scope::Read),
        None => DatahubClient::connect(addr),
    };
    match opened {
        Ok(client) => ServerView::Answered(client.features().to_vec()),
        Err(e) if answered_and_refused(e.kind()) => ServerView::Refused(e.to_string()),
        Err(e) => ServerView::Unreachable(e.to_string()),
    }
}

/// The server-wide capabilities the matrix's own columns depend on, and what each one's absence
/// costs — one row per feature, each naming the VERB it gates.
///
/// ⚠ These are per-SERVER rather than per-venue, which is why they are a block under the table and
/// not two more columns: a cell repeated identically on every row reads as a per-venue fact, and
/// it is not one. `md_venue=` is the exception that proves it — that advertisement IS per venue,
/// so it is the column.
const SERVER_VERBS: &[(&str, &str)] = &[
    (
        FEATURE_BACKFILL,
        "`data hist fetch` — without it the BACKFILL column above names a capability this server \
         will not act on",
    ),
    (
        FEATURE_VENUE_CATALOG,
        "`data catalog ls` and `refresh` — without it this group cannot reach a venue's list at \
         all",
    ),
];

/// The difference between what this build declares and what the server that answered serves.
///
/// ⚠ **A DIFFERENCE, never a verdict.** §8.1's whole demand is that the two halves stay legible
/// apart: an operator must be able to read WHICH side said no. So both directions are carried by
/// name — a venue this build believes has a live feed that the server does not serve (a
/// `data realtime watch` that will be refused), and a venue the server serves that this build's
/// roster does not even name (this binary is OLDER than that server). Neither is folded into the
/// other and nothing here ANDs the two columns together.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Skew {
    /// Venues with a declared live feed in THIS build that the server did not advertise.
    declared_here_unserved_there: Vec<&'static str>,
    /// Venues the SERVER advertised that `vike_model::VENUES` does not carry.
    served_there_unknown_here: Vec<String>,
}

/// Compute the skew, or `None` unless an advertisement SURVIVED the handshake — in which case
/// there is no difference to state, only a missing half.
///
/// ⚠ **This said `None` when the server was never asked, and that covered [`ServerView::Refused`],
/// where something WAS asked and answered.** The distinction is [`ServerView::Answered`]'s alone: a
/// refused connection may well have carried a full `Welcome`, and this side discarded it
/// ([`ServerView::md_venues`] says so at the discard), so what is missing here is the far half of
/// the comparison rather than the question.
fn skew(rows: &[VenueRow], view: &ServerView) -> Option<Skew> {
    let ServerView::Answered(_) = view else { return None };
    let served = view.md_venues();
    let declared_here_unserved_there = rows
        .iter()
        .filter(|r| !r.live.is_empty() && !served.iter().any(|s| s == r.venue))
        .map(|r| r.venue)
        .collect();
    let served_there_unknown_here =
        served.iter().filter(|s| !rows.iter().any(|r| r.venue == s.as_str())).cloned().collect();
    Some(Skew { declared_here_unserved_there, served_there_unknown_here })
}

/// `data catalog venues` — the local matrix, plus whatever the datahub was willing to say.
///
/// ⚠ **An unreachable datahub is REPORTED, never fatal.** §8.1: the local columns are the bulk of
/// the answer, and a box with no datahub is exactly the box whose operator needs to know what this
/// build can do. It is also the one verb in this group that opens a socket it does not need.
fn execute_venues(args: &Args, keys: Option<&NodeKeys>) -> CmdResult<()> {
    let rows = matrix();
    let view = ask_the_server(&args.addr, keys);
    if args.json {
        println!("{}", venues_json(args, &rows, &view));
    } else {
        for line in venues_lines(&rows, &view, &args.addr) {
            println!("{line}");
        }
    }
    Ok(())
}

/// A row's live-lane cell, or `-` for a venue with no live feed at all.
fn lanes_cell(row: &VenueRow) -> String {
    if row.live.is_empty() { "-".to_string() } else { row.live.join(",") }
}

/// A row's backfill cell — the KINDS, not a boolean, because `bars` and `ticks` are different
/// answers to "what can I get for this venue without a vendor".
fn backfill_cell(row: &VenueRow) -> String {
    let kinds: Vec<&str> = [("bars", row.backfill_bars), ("ticks", row.backfill_ticks)]
        .into_iter()
        .filter(|(_, on)| *on)
        .map(|(name, _)| name)
        .collect();
    if kinds.is_empty() { "-".to_string() } else { kinds.join(",") }
}

/// The SERVER's cell for one venue: three states, and `?` is not `no`.
fn feed_cell(venue: &str, served: &[String], asked: bool) -> &'static str {
    if !asked {
        "?"
    } else if served.iter().any(|s| s == venue) {
        "served"
    } else {
        "not served"
    }
}

/// The matrix, rendered.
fn venues_lines(rows: &[VenueRow], view: &ServerView, addr: &str) -> Vec<String> {
    let served = view.md_venues();
    let asked = matches!(view, ServerView::Answered(_));
    let venue_w = col("VENUE", rows.iter().map(|r| r.venue.len()));
    let lanes_w = col("LIVE LANES", rows.iter().map(|r| lanes_cell(r).len()));
    let bf_w = col("BACKFILL", rows.iter().map(|r| backfill_cell(r).len()));
    let mut lines = vec![
        // ⚠ The provenance is in the HEADER, because that is what stops the last column being read
        // as a correction of the first two. They are three independent statements.
        format!(
            "{:<venue_w$}  {:<lanes_w$}  {:<bf_w$}  LIVE FEED",
            "VENUE", "LIVE LANES", "BACKFILL"
        ),
        format!(
            "{:<venue_w$}  {:<lanes_w$}  {:<bf_w$}  (that datahub)",
            "", "(this build)", "(this build)"
        ),
    ];
    for r in rows {
        lines.push(format!(
            "{:<venue_w$}  {:<lanes_w$}  {:<bf_w$}  {}",
            r.venue,
            lanes_cell(r),
            backfill_cell(r),
            feed_cell(r.venue, &served, asked)
        ));
    }
    lines.push(String::new());
    lines.push(format!(
        "this build: {} venues on the roster · {} with a live feed · {} that backfill anything",
        rows.len(),
        rows.iter().filter(|r| !r.live.is_empty()).count(),
        rows.iter().filter(|r| r.backfill_bars || r.backfill_ticks).count()
    ));
    match view {
        ServerView::Unreachable(why) => {
            lines.push(format!("the datahub at {addr}: NOT REACHED — {why}"));
            lines.push(
                "so the LIVE FEED column is `?` on every row: unasked, which is not the same as \
                 unserved. Everything above it is this build's own declaration and is unaffected."
                    .to_string(),
            );
        }
        // ⚠ Its OWN arm, and the ⚠ is the reason the third state exists: every one of these
        // used to print as `NOT REACHED`, which told the operator the box had no datahub when it
        // had one that answered and said no.
        //
        // ⚠ **The second line used to read "that server was reached and never asked", and that is
        // a claim about the FAR side which the commonest case contradicts.** A keyed datahub met
        // with no node keys completes `handshake()` — `Welcome.features`, `md_venue=` entries and
        // all — and `DatahubClient::connect` only then raises `PermissionDenied`, so the
        // advertisement existed and THIS side discarded it with the connection
        // ([`ServerView::md_venues`] says so at the discard). Telling an operator the server was
        // never asked sends them to look at the server for a decision this binary made.
        ServerView::Refused(why) => {
            lines.push(format!("the datahub at {addr}: REACHED, and it REFUSED — {why}"));
            lines.push(
                "so the LIVE FEED column is `?` on every row: nothing here read an advertisement \
                 it could stand behind. ⚠ On the commonest served refusal — a keyed datahub with \
                 no node keys in this box's store — that server DID advertise its live venues in \
                 the `Welcome` before refusing the connection, and THIS side dropped them with it \
                 rather than report half a handshake. So `?` is this binary's choice, not the \
                 server's silence. ⚠ This is NOT an absent datahub either — a PROTOCOL VERSION \
                 SKEW between this binary and that server lands here, as does a key it would not \
                 take, and each is a different thing to go and fix. Everything above it is this \
                 build's own declaration and is unaffected."
                    .to_string(),
            );
        }
        ServerView::Answered(_) => {
            lines.push(format!(
                "the datahub at {addr}: serves live market data for {} of them",
                served.iter().filter(|s| rows.iter().any(|r| r.venue == s.as_str())).count()
            ));
            for (feature, why) in SERVER_VERBS {
                let state = match view.serves(feature) {
                    Some(true) => "serves",
                    Some(false) => "does NOT serve",
                    None => "was not asked for",
                };
                lines.push(format!("  {state} `{feature}` — {why}"));
            }
            // ⚠ This arm is the only one that renders [`SERVER_VERBS`], and deliberately: an
            // unreached server said nothing about them either, and printing "does NOT serve" for a
            // server nobody asked would be the merge this verb exists to avoid.
        }
    }
    if let Some(s) = skew(rows, view) {
        if !s.declared_here_unserved_there.is_empty() {
            // ⚠ **This sentence used to end `a `data realtime watch` on one of them is refused by
            // the server, not by this binary`, and it named no route instead.** The reason it was
            // struck has since EXPIRED and the sentence is still right to leave out, which is worth
            // a reader's time because the two are different arguments:
            //
            // * THEN, the route did not exist — `crate::cmd::data`'s `parse` refused the whole
            //   group on its own usage rung, in this binary, before any socket.
            // * NOW it does (`crate::cmd::data::realtime`'s `watch` ships), and the struck half is
            //   STILL false — for a reason that survives the route: the skew set includes a server
            //   advertising NO market-data plane at all, and against one of those
            //   `vike_datahub_client::DatahubClient::md_subscribe` refuses LOCALLY on the
            //   capability, "nothing was sent". So "refused by the server, not by this binary" is
            //   exactly wrong in the case this fixture plants, and which side says no is the ONE
            //   thing this verb exists to keep legible.
            //
            // What is true needs no route at all, and is what it says. The test below
            // (`the_build_column_and_the_server_column_are_never_merged_into_one_verdict`) holds it.
            lines.push(format!(
                "⚠ SKEW — this build declares a live feed for {}, and that datahub advertises \
                 none. The lanes on the left are this binary's own adapters; whether a live \
                 subscription is SERVED is that server's answer, and for these venues it is no.",
                s.declared_here_unserved_there.join(", ")
            ));
        }
        if !s.served_there_unknown_here.is_empty() {
            lines.push(format!(
                "⚠ SKEW — that datahub serves live market data for {}, which this build's venue \
                 roster does not name: this binary is OLDER than that server.",
                s.served_there_unknown_here.join(", ")
            ));
        }
    }
    lines
}

// ─── the `--json` documents ─────────────────────────────────────────────────────────────────────

/// The fields every document in this group opens with.
///
/// ⚠ `verb` is DERIVED from [`Verb::as_str`], never typed as a literal. `crate::cmd::data`'s
/// `tape_health_json` carries the incident that rule comes from: four renderers each spelled their
/// verb as a literal, the group split renamed two of them, and the documents went on naming
/// spellings the parser refuses.
fn document_head(args: &Args) -> serde_json::Value {
    serde_json::json!({ "group": "catalog", "verb": args.verb.as_str(), "addr": args.addr })
}

/// One `CatalogOutcome`, as a STABLE machine token plus the numbers it carries.
///
/// ⚠ **`listed` is a number for `Listed` and `null` for everything else, and that is the whole
/// point.** An empty listing is `{"outcome":"listed","listed":0}` while an un-enumerable venue is
/// `{"outcome":"refused","listed":null}` — the type-level distinction
/// `vike_datahub_client::catalog`'s module doc exists to preserve, carried across the last hop to
/// a consumer rather than flattened on the way out.
fn outcome_json(outcome: &CatalogOutcome) -> serde_json::Value {
    match outcome {
        CatalogOutcome::Listed { instruments, truncated, cached } => serde_json::json!({
            "outcome": "listed",
            "listed": instruments.len(),
            "truncated": truncated,
            "cached": cached,
            "refusal": serde_json::Value::Null,
            "supported": serde_json::Value::Null,
        }),
        CatalogOutcome::NotArmed => serde_json::json!({
            "outcome": "not_armed",
            "listed": serde_json::Value::Null,
            "truncated": serde_json::Value::Null,
            "cached": serde_json::Value::Null,
            "refusal": serde_json::Value::Null,
            "supported": serde_json::Value::Null,
        }),
        CatalogOutcome::Refused(refusal) => {
            let (token, supported) = match refusal {
                CatalogRefusal::NoBulkList { .. } => ("no_bulk_list", serde_json::Value::Null),
                CatalogRefusal::NeedsCredentials => ("needs_credentials", serde_json::Value::Null),
                CatalogRefusal::NotServed { supported } => {
                    ("not_served", serde_json::json!(supported))
                }
            };
            serde_json::json!({
                "outcome": "refused",
                "listed": serde_json::Value::Null,
                "truncated": serde_json::Value::Null,
                "cached": serde_json::Value::Null,
                "refusal": token,
                "supported": supported,
            })
        }
    }
}

/// Merge `extra` into `head` — both are objects by construction, so this is total.
fn merged(mut head: serde_json::Value, extra: serde_json::Value) -> String {
    if let (Some(h), Some(e)) = (head.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            h.insert(k.clone(), v.clone());
        }
    }
    serde_json::to_string_pretty(&head)
        .expect("a tree of strings, numbers, bools and nulls; serialization is total")
}

/// `ls --json`: the outcome, the counts, and the rows that survived the filters.
///
/// ⚠ The `venue` field is `CatalogListing::venue` — the slug the SERVER echoed back — rather than
/// this side's `--venue` value. That type's own doc says it is echoed verbatim so a client holding
/// several in flight can match them up, and taking it from the answer rather than from the request
/// is what makes the document describe what arrived.
fn ls_json(args: &Args, listing: &CatalogListing, rows: &[InstrumentRow]) -> String {
    let instruments: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "symbol": r.symbol,
                // The model's own stored word, never a second spelling minted here.
                "asset_class": r.class.sql_word(),
                "base": r.base,
                "quote": r.quote,
                // ⚠ The RAW numbers, including the `0.0` the human table renders as `-`: a machine
                // reader asked for the grid in order to fold it, and a dash is this side's reading
                // rather than the datum.
                "tick_size": r.tick,
                "step_size": r.lot,
                "description": r.description,
            })
        })
        .collect();
    merged(
        document_head(args),
        serde_json::json!({
            "venue": listing.venue,
            "outcome_detail": outcome_json(&listing.outcome),
            "message": listing.describe(),
            "filter": { "class": args.class.map(AssetClass::sql_word), "search": args.search },
            "shown": instruments.len(),
            "instruments": instruments,
        }),
    )
}

/// `refresh --json`: the counts and — the field this verb exists for — whether anything was
/// actually re-asked.
///
/// ⚠ It answers for BOTH of [`execute_refresh`]'s exits, and that is a correction: the refusal
/// path used to print [`outcome_only_json`] instead, so the `reasked: null` document the ⚠ below
/// describes was emitted by nothing and the non-`Listed` arm was dead in production. `refresh
/// --json` now always carries the one field a consumer reads this verb for.
fn refresh_json(args: &Args, listing: &CatalogListing) -> String {
    let reasked = match &listing.outcome {
        CatalogOutcome::Listed { cached, .. } => serde_json::json!(!cached),
        _ => serde_json::Value::Null,
    };
    merged(
        document_head(args),
        serde_json::json!({
            "venue": listing.venue,
            "outcome_detail": outcome_json(&listing.outcome),
            "message": listing.describe(),
            // ⚠ NOT a synonym for `!cached`: it is `null` when no listing happened at all, so a
            // consumer cannot read a refusal as "did not re-ask" and retry forever.
            "reasked": reasked,
        }),
    )
}

/// The document a refused `ls`/`refresh` still owes a machine reader. It carries no `instruments`
/// key at all — an empty array there would be the very collapse [`outcome_json`] prevents.
fn outcome_only_json(args: &Args, listing: &CatalogListing) -> String {
    merged(
        document_head(args),
        serde_json::json!({
            "venue": listing.venue,
            "outcome_detail": outcome_json(&listing.outcome),
            "message": listing.describe(),
        }),
    )
}

/// `show --json`: the recorded grid, with the class as the model's own stored word and `null`
/// where the producer named none.
fn show_json(args: &Args, target: &InstrumentRef, props: &vike_model::SymbolProperties) -> String {
    merged(
        document_head(args),
        serde_json::json!({
            "venue": target.venue,
            "symbol": target.symbol,
            "recorded": true,
            "asset_class": props.asset_class.map(AssetClass::sql_word),
            "tick_size": props.tick_size,
            "step_size": props.step_size,
            "min_qty": props.min_qty,
            "max_qty": props.max_qty,
            "min_notional": props.min_notional,
            "contract_size": props.contract_size,
            // The SOURCE, in the document as well as in the table — see the module doc.
            "source": "datahub store, kind=properties, latest row on record",
        }),
    )
}

/// `show --json` when nothing was recorded. `recorded: false` with every grid field ABSENT rather
/// than zeroed: a zero here would be indistinguishable from a recorded default grid, which is a
/// real and different state this verb warns about separately.
fn show_missing_json(args: &Args, target: &InstrumentRef) -> String {
    merged(
        document_head(args),
        serde_json::json!({
            "venue": target.venue,
            "symbol": target.symbol,
            "recorded": false,
            "source": "datahub store, kind=properties, latest row on record",
        }),
    )
}

/// `venues --json`: two SEPARATE objects and the difference between them.
///
/// ⚠ There is deliberately no merged per-venue verdict. §8.1 wants the skew visible, and a
/// consumer that wants "can I watch binance live" ANDs `build` and `server` itself — having read,
/// in the document, which of the two said no.
///
/// ⚠ **`server.state` is a THREE-token field where this shipped `server.reachable`, a boolean.**
/// See [`ServerView::state`]: a server that answered the handshake and then refused the connection
/// is reached, and reporting `"reachable": false` for it was not lossy but false. The tokens are
/// `answered` / `refused` / `unreachable`, and every other field of this object is `null` on the
/// last two — including on `refused`, because a discarded handshake advertised nothing this side
/// may read.
fn venues_json(args: &Args, rows: &[VenueRow], view: &ServerView) -> String {
    let venues: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "venue": r.venue,
                "live_lanes": r.live,
                "backfill_bars": r.backfill_bars,
                "backfill_ticks": r.backfill_ticks,
            })
        })
        .collect();
    let server = match view {
        ServerView::Answered(features) => serde_json::json!({
            "state": view.state(),
            "error": serde_json::Value::Null,
            "features": features,
            "md_venues": view.md_venues(),
            "serves_backfill": view.serves(FEATURE_BACKFILL),
            "serves_venue_catalog": view.serves(FEATURE_VENUE_CATALOG),
        }),
        // The two NON-answers are ONE shape and differ only in their token and their sentence, so
        // the nulls are typed once: a second copy of them is how two documents of one verb come to
        // disagree about which keys a consumer may expect to be present.
        ServerView::Refused(why) | ServerView::Unreachable(why) => serde_json::json!({
            "state": view.state(),
            "error": why,
            // ⚠ `null`, not `[]`: this side holds no advertisement it may read, and an empty list
            // would say the SERVER advertised nothing — a different and much stronger claim.
            //
            // ⚠ **This read "nothing was asked", which is the one claim the `refused` half of this
            // shared arm contradicts** — the sentence deleted from `venues_lines`'s operator-facing
            // text and from [`ServerView::serves`]'s doc, left standing on the arm that BUILDS the
            // `refused` document. A keyed datahub met with no node keys completes `handshake()` and
            // only then refuses, so on `refused` the question was asked, the far side answered, and
            // THIS side dropped the `Welcome` with the connection ([`ServerView::md_venues`] is
            // where). Only [`ServerView::Unreachable`] means nobody was asked — which is why these
            // two share a SHAPE and not a reason.
            "features": serde_json::Value::Null,
            "md_venues": serde_json::Value::Null,
            "serves_backfill": serde_json::Value::Null,
            "serves_venue_catalog": serde_json::Value::Null,
        }),
    };
    let skew_doc = match skew(rows, view) {
        Some(s) => serde_json::json!({
            "declared_here_unserved_there": s.declared_here_unserved_there,
            "served_there_unknown_here": s.served_there_unknown_here,
        }),
        None => serde_json::Value::Null,
    };
    merged(
        document_head(args),
        serde_json::json!({ "build": { "venues": venues }, "server": server, "skew": skew_doc }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_string()).collect()
    }

    fn parse_of(args: &[&str]) -> Result<Args, String> {
        parse(&argv(args), None)
    }

    fn row(symbol: &str, class: AssetClass) -> InstrumentRow {
        InstrumentRow {
            symbol: symbol.to_string(),
            class,
            base: "BTC".to_string(),
            quote: "USDT".to_string(),
            tick: 0.01,
            lot: 0.001,
            description: "Bitcoin".to_string(),
        }
    }

    /// Every verb [`VERBS`] names is one [`parse`] accepts, under the grammar its own usage block
    /// states — so a roster row the parser refuses is caught here.
    ///
    /// ⚠ **This carried a second loop claiming "and every parsed verb is in the roster", and that
    /// loop could not fail.** It asserted `verb_roster().contains(v.as_str())` for `v in VERBS`,
    /// and [`verb_roster`] IS `VERBS` joined — a derivation compared against its own input. The
    /// direction it claimed is the one that matters (a verb [`parse`] accepts that the roster does
    /// not name is undiscoverable), and it is the one no test in this file can have: the parser's
    /// accepted set is a `match` over literals and nothing here can enumerate it. Deleted rather
    /// than left as a doc promising what it does not check. What narrows the gap instead is the
    /// same ratchet the flags have — every accepted verb reaches [`Verb`], and [`usage`] and every
    /// refusal render [`VERBS`], so a variant missing from the roster is documented and named
    /// nowhere at all rather than merely untested.
    #[test]
    fn every_verb_in_the_roster_parses_under_its_own_grammar() {
        for v in VERBS {
            let line: Vec<&str> = match v {
                Verb::Ls | Verb::Refresh => vec![v.as_str(), "--venue", "binance"],
                Verb::Show => vec![v.as_str(), "binance:BTCUSDT"],
                Verb::Venues => vec![v.as_str()],
            };
            let got = parse_of(&line).unwrap_or_else(|e| panic!("{} must parse: {e}", v.as_str()));
            assert_eq!(got.verb, *v);
        }
    }

    #[test]
    fn a_missing_verb_names_every_verb_that_exists() {
        let err = parse_of(&[]).expect_err("a verb is required");
        for v in VERBS {
            assert!(err.contains(v.as_str()), "the refusal must name `{}`: {err}", v.as_str());
        }
        // Anti-vacuity: the message is not merely long enough to contain any short word.
        assert!(!err.contains("frobnicate"), "{err}");
    }

    /// The rename an operator arrives with, because the sibling group made the same one.
    #[test]
    fn the_renamed_listing_verb_names_its_replacement_rather_than_the_roster() {
        let err = parse_of(&["list", "--venue", "binance"]).expect_err("`list` is not a verb");
        assert!(err.contains("`data catalog ls`"), "{err}");
        // ...and a genuinely unknown verb gets the OTHER message, so the assertion above is not
        // passing because everything says the same thing.
        let other = parse_of(&["frobnicate"]).expect_err("unknown");
        assert!(other.contains("unknown `data catalog` verb"), "{other}");
        assert!(!other.contains("`data catalog ls`"), "{other}");
    }

    /// `--venue` is the SUBJECT of two verbs and meaningless on the other two, and each refusal
    /// says which — never "unknown option", which would tell an operator the flag does not exist.
    #[test]
    fn venue_is_required_where_it_is_the_subject_and_refused_where_it_is_not() {
        for v in ["ls", "refresh"] {
            let err = parse_of(&[v]).expect_err("--venue is required");
            assert!(err.contains("--venue"), "{v}: {err}");
            assert!(err.contains("data catalog venues"), "{v} must name the roster verb: {err}");
        }
        let show = parse_of(&["show", "binance:BTCUSDT", "--venue", "binance"])
            .expect_err("--venue is refused on show");
        assert!(show.contains("VENUE:SYMBOL"), "{show}");
        let venues =
            parse_of(&["venues", "--venue", "binance"]).expect_err("--venue is refused on venues");
        assert!(venues.contains("roster"), "{venues}");
    }

    /// The non-`show` lines every refusal below is applied to, spelled once.
    fn other_verb_lines() -> Vec<Vec<&'static str>> {
        vec![
            vec!["ls", "--venue", "binance"],
            vec!["refresh", "--venue", "binance"],
            vec!["venues"],
        ]
    }

    #[test]
    fn the_instrument_positional_belongs_to_show_alone_and_every_refusal_says_why() {
        for base in other_verb_lines() {
            let mut line = base.clone();
            line.push("binance:BTCUSDT");
            let err = parse_of(&line).expect_err("a positional is refused");
            assert!(err.contains("data catalog show"), "{base:?}: {err}");
        }
        let missing = parse_of(&["show"]).expect_err("show needs one");
        assert!(missing.contains("VENUE:SYMBOL"), "{missing}");
        let two = parse_of(&["show", "binance:BTCUSDT", "okx:BTC-USDT"])
            .expect_err("two positionals are refused");
        assert!(two.contains("binance:BTCUSDT") && two.contains("okx:BTC-USDT"), "{two}");
    }

    /// The three-part spelling is the one an operator arrives with from `data hist fetch`, so it
    /// is refused by NAME rather than as a malformed line.
    #[test]
    fn a_series_spec_is_refused_as_a_series_rather_than_as_a_typo() {
        let err = parse_of(&["show", "binance:BTCUSDT:1h"]).expect_err("a series is not one");
        assert!(err.contains("INTERVAL"), "{err}");
        assert!(err.contains("data hist"), "the refusal must name the verbs that take one: {err}");
        // A genuinely malformed spelling gets the SHAPE message instead, so the assertion above
        // is not passing because every bad spec says the same thing.
        for bad in ["binance", "binance:", ":BTCUSDT", "binance: "] {
            let e = parse_of(&["show", bad]).expect_err("malformed");
            assert!(e.contains("two non-empty parts"), "{bad}: {e}");
            assert!(!e.contains("INTERVAL"), "{bad}: {e}");
        }
    }

    /// The class vocabulary is the MODEL's, matched case-insensitively, and the refusal renders
    /// the whole roster rather than a guess.
    #[test]
    fn the_class_filter_is_the_models_own_word_and_an_unknown_one_names_the_roster() {
        for class in AssetClass::ALL {
            let word = class.sql_word();
            let got = parse_of(&["ls", "--venue", "binance", "--class", word])
                .unwrap_or_else(|e| panic!("{word}: {e}"));
            assert_eq!(got.class, Some(*class));
            // ...and the same word shouted, because an operator types `CryptoPerp` and `cryptoperp`
            // interchangeably and neither is a different class.
            let shouted = parse_of(&["ls", "--venue", "binance", "--class", &word.to_uppercase()])
                .unwrap_or_else(|e| panic!("{word} upper: {e}"));
            assert_eq!(shouted.class, Some(*class));
        }
        let err = parse_of(&["ls", "--venue", "binance", "--class", "perp"])
            .expect_err("`perp` is not the vocabulary");
        for class in AssetClass::ALL {
            assert!(err.contains(class.sql_word()), "the refusal must name {}", class.sql_word());
        }
        let blank = parse_of(&["ls", "--venue", "binance", "--class", ""])
            .expect_err("an empty class names nothing");
        assert!(blank.contains("EMPTY"), "{blank}");
    }

    #[test]
    fn the_narrowing_flags_belong_to_the_listing_and_every_refusal_names_it() {
        let mut verbs = other_verb_lines();
        verbs.retain(|v| v[0] != "ls");
        verbs.push(vec!["show", "binance:BTCUSDT"]);
        for verb in verbs {
            for flag in [["--class", "CryptoSpot"], ["--search", "BTC"]] {
                let mut line = verb.clone();
                line.extend(flag.iter().copied());
                let err = parse_of(&line).expect_err("refused off `ls`");
                assert!(err.contains(flag[0]), "{verb:?} {flag:?}: {err}");
                assert!(err.contains("data catalog ls"), "{verb:?} {flag:?}: {err}");
            }
        }
        // The control: both flags are ACCEPTED on `ls`, so the loop above is not passing because
        // this parser refuses them everywhere.
        let ok =
            parse_of(&["ls", "--venue", "binance", "--class", "CryptoSpot", "--search", "BTC"])
                .expect("`ls` takes both");
        assert_eq!(ok.class, Some(AssetClass::CryptoSpot));
        assert_eq!(ok.search.as_deref(), Some("BTC"));
    }

    /// **A `--venue` this binary can refuse, refused by this binary.** The shape rules are
    /// `vike_datahub_client::catalog::validate_catalog_venue`'s — the SAME function the server's
    /// door calls — so the refusal an operator reads locally is the refusal the server would have
    /// given, and it arrives on the USAGE rung with no socket opened.
    ///
    /// ⚠ Until [`parse`] called it, each of these opened a connection and the operator's diagnosis
    /// depended on who was on the port: `cannot connect to datahub at …` (the CONNECT rung, which
    /// a wrapper retries) with no datahub up, or the `does not advertise venue_catalog` message
    /// against a server with no catalog lane. The slug itself was named on neither path.
    #[test]
    fn a_venue_this_binary_can_refuse_is_refused_before_a_socket_is_opened() {
        for verb in ["ls", "refresh"] {
            for bad in ["", "BINANCE", "bin@nce", "bi nance", "averyveryverylongvenueslug"] {
                let Err(err) = parse_of(&[verb, "--venue", bad]) else {
                    panic!("`{verb} --venue {bad}` must be refused before a socket is opened");
                };
                assert!(err.contains("--venue"), "the refusal names the flag: {err}");
                // ...and the sentence is the CLIENT's own, so one refusal is worded one way
                // wherever it is reached from. This is the exact text the server would return.
                let wire = validate_catalog_venue(bad).expect_err("the client refuses it too");
                assert!(err.contains(&wire), "the wording must be the wire's: {err}");
            }
        }
        // The control, in both directions: a real roster slug parses, and the refusal above is not
        // passing because every `--venue` is refused.
        for venue in vike_model::VENUES {
            let got = parse_of(&["ls", "--venue", *venue])
                .unwrap_or_else(|e| panic!("`{venue}` is on the roster and must parse: {e}"));
            assert_eq!(got.venue.as_deref(), Some(*venue));
        }
        // ⚠ ...and `show`'s venue is deliberately NOT put through this validator: that half of the
        // positional reaches `properties_as_of`, a different verb with its own door.
        let shouted = parse_of(&["show", "BINANCE:BTCUSDT"]).expect("`show` validates no venue");
        assert_eq!(shouted.instrument.expect("an instrument").venue, "BINANCE");
    }

    /// `--search ""` is refused by NAME, like its two sibling narrowing/rendering flags — see
    /// [`parse_search`] for what an empty needle does to a listing that is not refused.
    #[test]
    fn an_empty_search_is_refused_the_way_an_empty_class_and_an_empty_format_are() {
        let err = parse_of(&["ls", "--venue", "binance", "--search", ""])
            .expect_err("an empty needle narrows nothing");
        assert!(err.contains("--search"), "{err}");
        assert!(err.contains("EMPTY"), "{err}");
        // The three flags that take a value all answer the same way, so an operator meets ONE
        // rule rather than three — and the control is that a real needle still parses.
        for flag in ["--class", "--format", "--search"] {
            let Err(e) = parse_of(&["ls", "--venue", "binance", flag, ""]) else {
                panic!("`{flag} \"\"` must be refused by name");
            };
            assert!(e.contains("EMPTY"), "{flag}: {e}");
        }
        assert_eq!(
            parse_of(&["ls", "--venue", "binance", "--search", " perp"])
                .expect("whitespace is a needle, not an accident")
                .search
                .as_deref(),
            Some(" perp")
        );
    }

    /// The format axis, and the refusals that are as much a part of it as the two values — the
    /// same contract `crate::cmd::data`'s `parse_format` states, reached through that function so
    /// there is no second roster here.
    ///
    /// ⚠ **THIS TEST WAS CALLED `…refuses_the_unbuilt_ones_by_name` AND ASSERTED THE WORDS "not
    /// built" FOR `csv`/`parquet`, AND BOTH HALVES ARE NOW FALSE.** `data hist export --out FILE`
    /// writes Parquet (and always did — the old row's own text said so while filing it under
    /// "nothing writes one") and `export --addr --format csv` writes CSV. So `crate::cmd::data`'s
    /// `UNBUILT_FORMATS` is EMPTY and neither value is waiting on a phase: they are refused HERE
    /// because a catalog is printed and these are FILE formats, which is a fact about this verb
    /// rather than about the workspace. A test still demanding the old words would have kept a
    /// message pointing operators at a plan for something they can run today.
    #[test]
    fn the_format_axis_carries_json_and_refuses_the_file_formats_by_name() {
        assert!(!parse_of(&["venues"]).expect("default").json);
        assert!(parse_of(&["venues", "--json"]).expect("--json").json);
        assert!(parse_of(&["venues", "--format", "json"]).expect("--format json").json);
        assert!(!parse_of(&["venues", "--format", "table"]).expect("--format table").json);
        for file_format in ["csv", "parquet"] {
            let err = parse_of(&["venues", "--format", file_format]).expect_err("a FILE format");
            assert!(err.contains(file_format), "{file_format}: {err}");
            assert!(err.contains("FILE format"), "{file_format}: {err}");
            // ...and it names the verb that WRITES one, so an operator who wanted a file has a
            // command line rather than a diagnosis.
            assert!(err.contains("data hist export"), "{file_format}: {err}");
            // THE ANTI-VACUITY CONTROL, and the reason this test was renamed: a shipped format may
            // not be described as waiting on a phase.
            assert!(!err.contains("not built"), "{file_format} SHIPS: {err}");
        }
        // ⚠ **`jsonl` LEFT that loop when `data hist get` shipped, and the split is the point.**
        // It is refused here for a different reason from its two former neighbours — not "a FILE
        // format" but "built, on the verb that emits ROWS to stdout" — so a message conflating the
        // two would send an operator who typed the right format on the wrong verb to `--out`
        // instead of to a pipe. Reached through `crate::cmd::data`'s `parse_format`, so this
        // asserts that function's arm rather than a second roster.
        let err = parse_of(&["venues", "--format", "jsonl"]).expect_err("a catalog is not rows");
        assert!(err.contains("data hist get"), "the refusal names the ROW verb: {err}");
        assert!(!err.contains("not built"), "…and does not call a shipped format unbuilt: {err}");
        assert!(parse_of(&["venues", "--json=1"]).expect_err("boolean").contains("takes no value"));
    }

    /// ⚠ **The two groups spell one rule and there is no shared const to import**, so this is what
    /// holds them equal: `crate::cmd::data`'s `parse` refuses the identical contradiction for the
    /// `hist` group, and a reader who meets both must not learn that one is a different KIND of
    /// no. It fails when either side is reworded, which is the moment to reword the other.
    ///
    /// ⚠ **The comparison is over WORDS, not bytes, and that is a measurement rather than a
    /// loosening.** The sibling's literal carries an eighteen-space run where a `\` line
    /// continuation was meant — measured on this branch — so a byte comparison would demand that
    /// this file reproduce that spacing in order to pass, which is copying a typographic defect
    /// into a second place under the guise of agreement. Splitting on whitespace compares the
    /// sentence both operators actually read, and still reddens on any rewording of either side.
    #[test]
    fn the_two_groups_refuse_the_json_format_contradiction_in_the_same_words() {
        fn words(s: &str) -> Vec<&str> {
            s.split_whitespace().collect()
        }
        let mine = parse_of(&["venues", "--json", "--format", "table"])
            .expect_err("the contradiction is refused");
        let hist = super::super::parse(
            ["hist", "ls", "--json", "--format", "table"].into_iter().map(String::from),
            None,
        )
        .expect_err("the sibling group refuses it too");
        assert_eq!(words(&mine), words(&hist), "one group reworded the shared refusal");
        // Anti-vacuity: `words` on two empty or two generic strings would also compare equal, so
        // the sentence has to be the real one — and it has to be more than a couple of tokens.
        assert!(mine.contains("--json") && mine.contains("--format table"), "{mine}");
        assert!(words(&mine).len() > 8, "a near-empty message would compare equal too: {mine}");
    }

    /// **…and the `jsonl` spelling, which is the one that actually parted.**
    ///
    /// ⚠ `crate::cmd::data`'s `parse` grew a `--json --format jsonl` arm with `get`, and applied
    /// it ABOVE the verb dispatch — so `data hist ls --json --format jsonl` answered with a
    /// sentence about GET's document, ending "Pass one", while `data catalog ls --json --format
    /// jsonl` answered with `ROW_VERB`, because THIS parser reads `--format` eagerly and its
    /// contradiction check never sees a `jsonl` at all. One question, two answers, on the same
    /// plane. The arm is `Sub::Get`'s alone now and this is what holds the two groups equal — the
    /// case the `table` twin above could never have covered, since `table` is valid on both.
    #[test]
    fn the_two_groups_refuse_the_jsonl_format_contradiction_in_the_same_words() {
        fn words(s: &str) -> Vec<&str> {
            s.split_whitespace().collect()
        }
        let mine = parse_of(&["venues", "--json", "--format", "jsonl"])
            .expect_err("a catalog verb emits no rows");
        let hist = super::super::parse(
            ["hist", "ls", "--json", "--format", "jsonl"].into_iter().map(String::from),
            None,
        )
        .expect_err("the sibling group refuses it too");
        assert_eq!(words(&mine), words(&hist), "one group reworded the shared refusal");
        // Anti-vacuity, the same two rungs the twin above uses: the sentence has to be the real
        // one, and it has to name where `jsonl` DOES work rather than merely be long.
        assert!(mine.contains(super::super::ROW_VERB), "{mine}");
        assert!(words(&mine).len() > 8, "a near-empty message would compare equal too: {mine}");
        // THE CONTROL: the verb that SERVES `jsonl` answers differently, so the equality above is
        // about these two groups agreeing rather than about one sentence for every line.
        let get = super::super::parse(
            ["hist", "get", "d:S:1h", "--days", "1", "--json", "--format", "jsonl"]
                .into_iter()
                .map(String::from),
            None,
        )
        .expect_err("a sequence is not one document");
        assert_ne!(words(&get), words(&mine), "`get` has a contradiction of its own: {get}");
    }

    /// The flag, then the configured address, then the default — the ladder
    /// `crate::cmd::data`'s `parse` already climbs, with a BLANK configured rung skipped rather
    /// than honoured.
    #[test]
    fn the_address_ladder_is_cli_then_configured_then_default() {
        assert_eq!(parse_of(&["venues"]).expect("default").addr, DEFAULT_ADDR);
        let configured = parse(&argv(&["venues"]), Some("<host>:9")).expect("configured");
        assert_eq!(configured.addr, "<host>:9");
        let flagged = parse(&argv(&["venues", "--addr", "127.0.0.1:1"]), Some("<host>:9"))
            .expect("the flag wins");
        assert_eq!(flagged.addr, "127.0.0.1:1");
        let blank = parse(&argv(&["venues"]), Some("   ")).expect("a blank rung is skipped");
        assert_eq!(blank.addr, DEFAULT_ADDR);
    }

    /// The search is a case-insensitive substring over every field an operator can SEE in the
    /// table, and the control is what makes the claim mean something: a term matching none of them
    /// keeps nothing.
    #[test]
    fn the_search_reaches_every_visible_field_and_a_miss_keeps_nothing() {
        let r = row("BTCUSDT", AssetClass::CryptoSpot);
        for hit in ["btcusd", "BTC", "usdt", "bitcoin", "BITCOIN"] {
            assert!(keeps(&r, None, Some(hit)), "`{hit}` must match");
        }
        assert!(!keeps(&r, None, Some("ethereum")), "a miss must keep nothing");
        assert!(keeps(&r, None, None), "an absent filter matches everything");
    }

    #[test]
    fn the_class_filter_keeps_only_that_class_and_is_anded_with_the_search() {
        let spot = row("BTCUSDT", AssetClass::CryptoSpot);
        let perp = row("BTCUSDT", AssetClass::CryptoPerp);
        assert!(keeps(&spot, Some(AssetClass::CryptoSpot), None));
        assert!(!keeps(&perp, Some(AssetClass::CryptoSpot), None));
        // ANDed: the right class with the wrong search keeps nothing.
        assert!(!keeps(&spot, Some(AssetClass::CryptoSpot), Some("ethereum")));
        assert!(keeps(&spot, Some(AssetClass::CryptoSpot), Some("btc")));
    }

    /// **The property this whole group's shape exists for.** A venue that listed NOTHING and a
    /// venue that CANNOT be listed must not render alike — in the table or in the document. It is
    /// `vike_datahub_client::catalog`'s decision 5, carried across the last hop.
    #[test]
    fn an_empty_listing_and_an_unlistable_venue_never_render_alike() {
        let empty = CatalogListing {
            venue: "binance".to_string(),
            outcome: CatalogOutcome::Listed {
                instruments: Vec::new(),
                truncated: false,
                cached: false,
            },
        };
        let none = CatalogListing {
            venue: "ig".to_string(),
            outcome: CatalogOutcome::Refused(CatalogRefusal::NoBulkList {
                why: "searched live per query".to_string(),
            }),
        };
        // The DOCUMENT: a count against a null, under two different tokens.
        let empty_doc = outcome_json(&empty.outcome);
        let none_doc = outcome_json(&none.outcome);
        assert_eq!(empty_doc["outcome"], "listed");
        assert_eq!(empty_doc["listed"], 0);
        assert_eq!(none_doc["outcome"], "refused");
        assert_eq!(none_doc["refusal"], "no_bulk_list");
        assert!(none_doc["listed"].is_null(), "a refusal counts nothing: {none_doc}");
        assert_ne!(empty_doc, none_doc);
        // The TABLE: the empty listing renders an empty-note and the venue's own sentence; the
        // refusal never reaches this renderer at all (it is the EMPTY rung), and its sentence is
        // the wire's.
        let lines = ls_lines(&[], 0, false, false, &empty.describe());
        assert!(lines.iter().any(|l| l.contains("no instruments")), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("0 instruments")), "{lines:?}");
        assert!(
            none.describe().contains("publishes no bulk instrument list"),
            "{}",
            none.describe()
        );
        assert!(!none.describe().contains("0 instruments"), "{}", none.describe());
    }

    /// Which VARIANT of `CatalogRefusal` a sample is, as an index into the sample list below.
    ///
    /// ⚠ **This match is the completeness gate, and it is the COMPILER's.** It carries no `_` arm,
    /// so a variant added to `vike_datahub_client::catalog::CatalogRefusal` stops this file
    /// compiling until it is given an index — and `every_refusal_carries_its_own_token_and_none_
    /// of_them_counts_anything` then demands a sample for that index. It is the same property
    /// [`live_lanes`]' destructure buys, bought for an enum rather than a struct.
    fn refusal_slot(refusal: &CatalogRefusal) -> usize {
        // ⚠ IF YOU ARE HERE BECAUSE THE COMPILER DEMANDED AN ARM: give it the next free slot AND
        // bump [`REFUSAL_VARIANTS`] below, then add a sample to the test. Without the bump the
        // completeness check silently stops covering your variant — which is the exact hole this
        // pair was rewritten to close.
        match refusal {
            CatalogRefusal::NoBulkList { why: _ } => 0,
            CatalogRefusal::NeedsCredentials => 1,
            CatalogRefusal::NotServed { supported: _ } => 2,
        }
    }

    /// How many variants [`refusal_slot`] assigns a slot to.
    ///
    /// ⚠ **This exists because the completeness check compared a derivation against its own
    /// input.** It read `slots == (0..refusals.len())`, and BOTH sides came from the same
    /// hand-typed three-element sample array — so a fourth variant given slot 3 and no sample left
    /// `slots == [0, 1, 2]` and `0..3`, equal, green, with two distinct refusals free to render as
    /// one token in every `--json` document. Against a count that the samples cannot move, the same
    /// mistake is `[0, 1, 2] != 0..4` and fails by name.
    ///
    /// It is a hand-maintained number and that is the residual, declared rather than hidden: the
    /// compiler forces the author to the match above, and the note there is what carries them here.
    const REFUSAL_VARIANTS: usize = 3;

    /// Every non-`Listed` outcome gets its OWN token, and none of them counts anything.
    ///
    /// ⚠ **The doc used to claim "an exhaustive check rather than a spot one" over a HAND-TYPED
    /// three-element array, and it was neither.** Nothing forced a new `CatalogRefusal` variant
    /// into that array — the compiler forces an ARM in [`outcome_json`], never a ROW here — so
    /// adding one and giving it `("not_served", Null)` would have shipped two distinct refusals
    /// rendering as one token in every `--json` document, green. It also omitted
    /// `CatalogRefusal::NoBulkList` entirely, checking that variant's token only incidentally in
    /// `an_empty_listing_and_an_unlistable_venue_never_render_alike`.
    ///
    /// What makes it exhaustive now is [`refusal_slot`]: its match has no `_`, and the two
    /// assertions below turn a missing sample into a failure rather than a silent shortfall.
    #[test]
    fn every_refusal_carries_its_own_token_and_none_of_them_counts_anything() {
        let refusals = [
            CatalogRefusal::NoBulkList { why: "searched live per query".to_string() },
            CatalogRefusal::NeedsCredentials,
            CatalogRefusal::NotServed { supported: vec!["binance".to_string(), "okx".to_string()] },
        ];
        // THE COMPLETENESS HALF, against [`REFUSAL_VARIANTS`] and NOT against `refusals.len()`.
        // ⚠ It compared the slots to `0..refusals.len()` — a derivation against its own input, so
        // a fourth variant with no sample here compared `[0,1,2]` to `0..3` and passed.
        let mut slots: Vec<usize> = refusals.iter().map(refusal_slot).collect();
        slots.sort_unstable();
        slots.dedup();
        assert_eq!(
            slots,
            (0..REFUSAL_VARIANTS).collect::<Vec<_>>(),
            "every `CatalogRefusal` variant needs exactly one sample above: {slots:?}"
        );
        assert_eq!(
            refusals.len(),
            REFUSAL_VARIANTS,
            "one sample per variant, no duplicates — {} samples for {REFUSAL_VARIANTS} variants",
            refusals.len()
        );

        let expected_supported = [
            serde_json::Value::Null,
            serde_json::Value::Null,
            serde_json::json!(["binance", "okx"]),
        ];
        // `NotArmed` is not a refusal and carries its own outcome token, so it is checked beside
        // them rather than through the slot machinery.
        let mut tokens: Vec<(String, String)> = vec![{
            let doc = outcome_json(&CatalogOutcome::NotArmed);
            assert_eq!(doc["outcome"], "not_armed");
            assert!(doc["listed"].is_null(), "{doc}");
            (doc["outcome"].to_string(), doc["refusal"].to_string())
        }];
        for (refusal, supported) in refusals.iter().zip(expected_supported) {
            let doc = outcome_json(&CatalogOutcome::Refused(refusal.clone()));
            assert_eq!(doc["outcome"], "refused");
            assert!(doc["listed"].is_null(), "{doc}");
            assert!(doc["truncated"].is_null(), "{doc}");
            assert!(doc["cached"].is_null(), "{doc}");
            assert_eq!(doc["supported"], supported);
            assert!(!doc["refusal"].is_null(), "a refusal must carry a token of its own: {doc}");
            tokens.push((doc["outcome"].to_string(), doc["refusal"].to_string()));
        }
        tokens.sort();
        let before = tokens.len();
        tokens.dedup();
        assert_eq!(before, tokens.len(), "two outcomes render as one token: {tokens:?}");
    }

    /// A truncated listing under a FILTER owes the reader a sentence the wire cannot give — and
    /// the control is the same listing with no filter, where the wire's own sentence is enough.
    #[test]
    fn a_truncated_listing_warns_only_where_the_filter_could_hide_the_tail() {
        assert!(truncation_warning(true, true)[0].contains("TRUNCATED"));
        assert!(truncation_warning(true, false).is_empty(), "the wire already said so");
        assert!(truncation_warning(false, true).is_empty(), "nothing was truncated");
        assert!(truncation_warning(false, false).is_empty());
    }

    /// `refresh` must SAY it re-asked nothing when the server answered from its memo, because its
    /// own name promises the opposite. The control is the fresh arm, which says the venue was
    /// called.
    #[test]
    fn a_cached_refresh_says_it_re_asked_nothing_and_a_fresh_one_says_it_called_the_venue() {
        let cached = refresh_lines(12, true, "`okx`: 12 instruments (from this server's cache).");
        assert!(cached.iter().any(|l| l.contains("NOTHING was re-asked")), "{cached:?}");
        assert!(cached.iter().any(|l| l.contains("TTL")), "{cached:?}");
        let fresh = refresh_lines(12, false, "`okx`: 12 instruments.");
        assert!(fresh.iter().any(|l| l.contains("called the venue")), "{fresh:?}");
        assert!(!fresh.iter().any(|l| l.contains("NOTHING was re-asked")), "{fresh:?}");
    }

    /// The matrix is the canonical roster, every row of it, and nothing typed here. A venue added
    /// to `vike_model::VENUES` appears with no edit to this file — and a row this file invented
    /// would fail the second half.
    #[test]
    fn every_roster_venue_has_a_row_and_no_row_names_a_venue_off_the_roster() {
        let rows = matrix();
        assert_eq!(rows.len(), vike_model::VENUES.len());
        for (row, venue) in rows.iter().zip(vike_model::VENUES) {
            assert_eq!(&row.venue, venue, "the matrix must be the roster, in its order");
        }
        // Anti-vacuity: the roster is not empty, and the matrix is not uniformly blank — at least
        // one venue declares a live lane and at least one declares a backfill kind, so the
        // renderers below are exercised on real values rather than on a table of dashes.
        assert!(!rows.is_empty());
        assert!(rows.iter().any(|r| !r.live.is_empty()), "no venue declares a live lane");
        assert!(rows.iter().any(|r| r.backfill_bars), "no venue declares a bar backfill");
    }

    /// Every lane the model declares is NAMED here. The completeness half is the compiler's — see
    /// [`live_lanes`] — and this pins the half it cannot check: that the names are distinct and
    /// that an all-on row yields all of them.
    #[test]
    fn every_live_lane_the_model_declares_has_a_distinct_name() {
        let all = LiveDataCaps { bars: true, quotes: true, trades: true, book: true, depth: true };
        let mut names = live_lanes(&all);
        assert_eq!(names.len(), 5, "an all-on row must name every lane: {names:?}");
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "two lanes share a name: {names:?}");
        // The control: nothing on names nothing, so the assertion above is not passing because
        // this function returns a constant.
        assert!(live_lanes(&LiveDataCaps::NONE).is_empty());
    }

    fn venue_row(venue: &'static str, live: &[&'static str], bars: bool, ticks: bool) -> VenueRow {
        VenueRow { venue, live: live.to_vec(), backfill_bars: bars, backfill_ticks: ticks }
    }

    /// **§8.1's demand, as a test.** The build's columns and the server's column are three
    /// independent statements, and the rendering must let a reader see WHICH side said no. So a
    /// venue this build declares four live lanes for, against a server that advertises none, still
    /// shows those four lanes AND a `not served` cell — and the skew line names it.
    #[test]
    fn the_build_column_and_the_server_column_are_never_merged_into_one_verdict() {
        let rows = vec![venue_row("binance", &["bars", "trades"], true, false)];
        let view = ServerView::Answered(vec!["inventory".to_string()]);
        let lines = venues_lines(&rows, &view, "127.0.0.1:7878");
        let body = lines.join("\n");
        assert!(body.contains("bars,trades"), "the build's lanes survive: {body}");
        assert!(body.contains("not served"), "the server's answer is its own cell: {body}");
        assert!(body.contains("SKEW"), "the difference is named: {body}");
        assert!(body.contains("that datahub advertises none"), "{body}");
        // ⚠ ...and it names NO ROUTE. The sentence this replaced told the operator that a
        // `data realtime watch` on a skewed venue "is refused by the server, not by this binary".
        // When it was struck, BOTH halves were false because the route did not exist — that group
        // was refused on this binary's own usage rung. `crate::cmd::data::realtime`'s `watch` ships
        // now, so the first half is true; the SECOND half is still false, and this fixture is
        // exactly the case that shows it. `ServerView::Answered(vec!["inventory"])` advertises no
        // market-data plane, so `vike_datahub_client::DatahubClient::md_subscribe` refuses on the
        // capability LOCALLY — "nothing was sent" — and the refusal that sentence attributed to the
        // server never reaches it. Which side said no is the one thing this verb keeps legible.
        assert!(
            !body.contains("data realtime"),
            "the skew line may not attribute a refusal to a side that did not make it: {body}"
        );
        // The control: a server that DOES advertise it renders the same build columns with a
        // different server cell and NO skew line.
        let served = ServerView::Answered(vec![vike_datahub_client::md_venue_feature("binance")]);
        let ok = venues_lines(&rows, &served, "127.0.0.1:7878").join("\n");
        assert!(ok.contains("bars,trades"), "{ok}");
        // ⚠ `contains("served")` would also match `not served`, so the control asserts the
        // NEGATIVE cell is absent — an assertion that cannot pass for the wrong reason.
        assert!(!ok.contains("not served"), "{ok}");
        assert!(ok.contains("served"), "{ok}");
        assert!(!ok.contains("SKEW"), "nothing differs, so nothing is reported: {ok}");
    }

    /// A server NEWER than this binary — it serves a venue this roster does not carry — is the
    /// other direction of the same skew, and it is the one an operator can otherwise diagnose only
    /// by reading two build logs.
    #[test]
    fn a_venue_the_server_serves_and_this_roster_does_not_name_is_reported_as_a_version_skew() {
        let rows = matrix();
        let mut features: Vec<String> =
            rows.iter().map(|r| vike_datahub_client::md_venue_feature(r.venue)).collect();
        features.push(vike_datahub_client::md_venue_feature("nextvenue"));
        let view = ServerView::Answered(features);
        let s = skew(&rows, &view).expect("the server answered");
        assert_eq!(s.served_there_unknown_here, vec!["nextvenue".to_string()]);
        assert!(
            s.declared_here_unserved_there.is_empty(),
            "every roster venue is served in this fixture: {s:?}"
        );
        let body = venues_lines(&rows, &view, "127.0.0.1:7878").join("\n");
        assert!(body.contains("OLDER than that server"), "{body}");
        assert!(body.contains("nextvenue"), "{body}");
    }

    /// **§8.1's other demand.** With no datahub the local columns are the whole answer and the
    /// verb still answers: every roster venue is rendered, the server column says UNASKED rather
    /// than unserved, and the document carries `null` rather than an empty list.
    ///
    /// ⚠ **The fixture is an `io::Error`'s OWN text, and it is built that way because the hand-typed
    /// one drifted.** It read `cannot connect to datahub at 127.0.0.1:1` — [`connect`]'s prefix,
    /// which [`ask_the_server`] deliberately does NOT add (the address is already on that line, and
    /// the prefix printed it twice). So this test showed a reader the doubled shape the fix had
    /// removed, and stayed green because it only looked for `NOT REACHED`. The message now comes
    /// from the same place production's does — an `io::Error`, stringified — and the count
    /// assertion below is what would fail if the prefix ever came back.
    #[test]
    fn an_unreachable_datahub_still_renders_every_local_row_and_says_the_column_is_unasked() {
        let rows = matrix();
        let view =
            ServerView::Unreachable(io::Error::from(io::ErrorKind::ConnectionRefused).to_string());
        let body = venues_lines(&rows, &view, "127.0.0.1:1").join("\n");
        for r in &rows {
            assert!(body.contains(r.venue), "{} is missing: {body}", r.venue);
        }
        assert!(body.contains("NOT REACHED"), "{body}");
        assert_eq!(
            body.matches("127.0.0.1:1").count(),
            1,
            "the address is named ONCE — this view carries the client's own sentence, without the \
             `cannot connect to datahub at {{addr}}` prefix `connect` adds: {body}"
        );
        assert!(body.contains("unasked, which is not the same as unserved"), "{body}");
        assert!(!body.contains("not served"), "nothing may claim the server refused: {body}");
        assert!(skew(&rows, &view).is_none(), "there is no difference to state");
        // ...and `?` is not `false` in the document either.
        assert_eq!(view.serves(FEATURE_BACKFILL), None);
        assert_eq!(ServerView::Answered(Vec::new()).serves(FEATURE_BACKFILL), Some(false));
    }

    /// **A server that ANSWERED and said no is not an absent server**, and this is the case the
    /// two-variant [`ServerView`] could not express.
    ///
    /// ⚠ The defect it pins is measured rather than hypothetical: `crate::cmd::data`'s [`connect`]
    /// folds a denied mac, a keyed server with no keys in the store, and a PROTO_VERSION skew into
    /// the same `CliError::connect` sentence, so `execute_venues` rendered every one of them as
    /// `NOT REACHED` with `"reachable": false` — in the verb whose own module doc says it exists
    /// to surface a version skew. [`ask_the_server`] reads the `io::ErrorKind` instead, and this
    /// holds the three renderings apart.
    #[test]
    fn a_server_that_answered_and_refused_is_not_rendered_as_an_absent_one() {
        let rows = matrix();
        let refused = ServerView::Refused(
            "datahub protocol version mismatch: client speaks 9, server speaks 10".to_string(),
        );
        let body = venues_lines(&rows, &refused, "127.0.0.1:7878").join("\n");
        assert!(body.contains("REACHED, and it REFUSED"), "{body}");
        assert!(
            !body.contains("NOT REACHED"),
            "a server that answered may not be reported as absent: {body}"
        );
        assert!(body.contains("PROTOCOL VERSION SKEW"), "the cause an operator acts on: {body}");
        assert_eq!(
            body.matches("127.0.0.1:7878").count(),
            1,
            "the address is named ONCE here too — same view, same client sentence: {body}"
        );
        // ⚠ …and the `?` column is attributed to THIS side. The sentence this replaced said the
        // server "was reached and never asked", which is false for the commonest served refusal:
        // a keyed datahub met with no keys advertised its venues in the `Welcome` and this binary
        // discarded them. See the arm in `venues_lines`.
        assert!(
            !body.contains("never asked"),
            "this side's own discard may not be reported as the server saying nothing: {body}"
        );
        assert!(
            body.contains("not the server's silence"),
            "…and it says whose choice it is: {body}"
        );
        // The build's own columns are untouched — the whole verb still answers.
        for r in &rows {
            assert!(body.contains(r.venue), "{} is missing: {body}", r.venue);
        }
        // ⚠ [`SERVER_VERBS`] is the ANSWERED arm's alone, and this is the assertion that holds it
        // there: with no advertisement to read, [`ServerView::serves`] is `None` for every one of
        // them and the loop renders `was not asked for` — the same false claim about the far side
        // wearing a spelling the ban above does not match.
        //
        // ⚠ It replaces `!body.contains("not served")`, a string `venues_lines` renders in NO arm
        // (the answered one says `does NOT serve`), so that assertion could not fail for its stated
        // reason — and its message, "nothing was asked, so nothing was refused", was the deleted
        // claim itself, three lines under the assertion that forbids it.
        assert!(
            !body.contains("was not asked for"),
            "a refused server's capability rows may not be rendered, least of all as unasked: \
             {body}"
        );

        // The DOCUMENT: three tokens, because `reachable` was a boolean answering a three-state
        // question — and for this state it answered `false` about a server that was reached.
        assert_eq!(refused.state(), "refused");
        assert_eq!(ServerView::Unreachable("closed".to_string()).state(), "unreachable");
        assert_eq!(ServerView::Answered(Vec::new()).state(), "answered");
        let args = parse_of(&["venues", "--json"]).expect("parses");
        let doc: serde_json::Value =
            serde_json::from_str(&venues_json(&args, &rows, &refused)).expect("one document");
        assert_eq!(doc["server"]["state"], "refused");
        assert!(doc["server"]["features"].is_null(), "a discarded handshake advertised nothing");
        assert!(doc["skew"].is_null(), "there is no difference to state: {doc}");
    }

    /// The `io::ErrorKind` split [`ask_the_server`] turns on, as a table — the kinds the client
    /// produces when the far side SPOKE, against the ones that mean nothing answered.
    ///
    /// It is a unit test of the CLASSIFIER rather than of a connection, deliberately: the three
    /// served refusals need a server that denies a mac, one that speaks another PROTO_VERSION and
    /// one that is not a datahub at all, and `DatahubClient`'s own tests are where those live. What
    /// this file owns is the decision made on the kind they each arrive as.
    #[test]
    fn the_answered_and_refused_kinds_are_the_ones_the_far_side_spoke_on() {
        for kind in [io::ErrorKind::PermissionDenied, io::ErrorKind::InvalidData] {
            assert!(answered_and_refused(kind), "{kind:?} is a served refusal");
        }
        for kind in [
            io::ErrorKind::ConnectionRefused,
            io::ErrorKind::TimedOut,
            io::ErrorKind::WouldBlock,
            io::ErrorKind::NotFound,
            io::ErrorKind::ConnectionReset,
        ] {
            assert!(!answered_and_refused(kind), "{kind:?} is a socket, not an answer");
        }
    }

    /// Both server-wide rows are rendered with their own verdict and their own cost, and neither
    /// is a per-venue column — see [`SERVER_VERBS`].
    #[test]
    fn the_server_wide_capabilities_are_reported_with_what_their_absence_costs() {
        let rows = matrix();
        let view = ServerView::Answered(vec![FEATURE_BACKFILL.to_string()]);
        let body = venues_lines(&rows, &view, "127.0.0.1:7878").join("\n");
        assert!(body.contains(&format!("serves `{FEATURE_BACKFILL}`")), "{body}");
        assert!(body.contains(&format!("does NOT serve `{FEATURE_VENUE_CATALOG}`")), "{body}");
        for (_, why) in SERVER_VERBS {
            assert!(body.contains(*why), "the cost must be stated: {body}");
        }
    }

    /// The documents name the group and the verb from the declarations rather than as literals,
    /// and the `venues` document keeps the two sources APART — which is the shape a consumer
    /// branches on.
    #[test]
    fn the_documents_derive_their_verb_and_keep_the_two_sources_apart() {
        let args = parse_of(&["venues", "--json"]).expect("parses");
        let rows = matrix();
        let doc: serde_json::Value = serde_json::from_str(&venues_json(
            &args,
            &rows,
            &ServerView::Unreachable("closed".to_string()),
        ))
        .expect("one JSON document");
        assert_eq!(doc["group"], "catalog");
        assert_eq!(doc["verb"], Verb::Venues.as_str());
        assert_eq!(doc["build"]["venues"].as_array().expect("rows").len(), rows.len());
        assert_eq!(doc["server"]["state"], "unreachable");
        assert!(doc["server"]["features"].is_null(), "unasked is null, never []: {doc}");
        assert!(doc["skew"].is_null());
        // There is deliberately NO merged per-venue verdict anywhere in the document.
        assert!(doc["venues"].is_null(), "the two sources must not be flattened: {doc}");
    }

    /// The `show` rendering names its SOURCE and says which absence it is looking at — the trap
    /// the module doc opens with, at the one place a reader can act on it.
    #[test]
    fn show_names_the_store_as_its_source_and_says_when_a_grid_is_empty() {
        let target = InstrumentRef { venue: "okx".to_string(), symbol: "BTC-USDT".to_string() };
        let recorded = vike_model::SymbolProperties {
            tick_size: 0.1,
            step_size: 0.001,
            asset_class: Some(AssetClass::CryptoPerp),
            ..Default::default()
        };
        let good = show_lines(&target, &recorded).join("\n");
        assert!(good.contains("kind=properties"), "the source is named: {good}");
        assert!(good.contains(AssetClass::CryptoPerp.sql_word()), "{good}");
        assert!(good.contains("0.1"), "{good}");
        assert!(!good.contains("names no tick size"), "this grid has one: {good}");
        // A DEFAULT grid: a row exists, so something recorded it — and every number is the model's
        // absent-is-zero rather than a fact.
        let empty = show_lines(&target, &vike_model::SymbolProperties::default()).join("\n");
        assert!(empty.contains("unclassified"), "{empty}");
        assert!(empty.contains("names no tick size"), "{empty}");
        assert!(
            !empty.contains("  tick size      0"),
            "a zero must not render as a number: {empty}"
        );
    }

    /// `ls`'s TABLE on real rows — the path
    /// `an_empty_listing_and_an_unlistable_venue_never_render_alike` cannot reach, since that case
    /// renders the EMPTY answer. A width bug or a lost column is invisible without this.
    #[test]
    fn the_listing_table_renders_every_column_and_says_what_a_filter_narrowed() {
        let mut bare = row("ETH-PERP", AssetClass::CryptoPerp);
        bare.base = "ETH".to_string();
        bare.quote = "USD".to_string();
        bare.description = String::new();
        bare.tick = 0.0;
        bare.lot = 0.0;
        let rows = vec![row("BTCUSDT", AssetClass::CryptoSpot), bare];

        let body = ls_lines(&rows, 400, true, false, "`binance`: 400 instruments.").join("\n");
        for header in ["SYMBOL", "CLASS", "BASE", "QUOTE", "TICK", "LOT", "DESCRIPTION"] {
            assert!(body.contains(header), "the {header} column is missing: {body}");
        }
        assert!(body.contains("BTCUSDT") && body.contains("ETH-PERP"), "{body}");
        assert!(body.contains(AssetClass::CryptoPerp.sql_word()), "{body}");
        assert!(body.contains("2 of 400 instruments"), "a filter states both sides: {body}");
        // ⚠ The absent-grid row carries NO DIGIT at all — every one of its cells is a word or a
        // dash. A `0` here would be the model's absent rendered as a fact, which is the whole of
        // [`grid_cell`]'s argument, checked on the real row rather than on the helper.
        let eth = body.lines().find(|l| l.starts_with("ETH-PERP")).expect("the bare row");
        assert!(!eth.contains('0'), "an absent grid must not render as a zero: {eth}");
        // ...and the control: the row that HAS a grid prints it.
        let btc = body.lines().find(|l| l.starts_with("BTCUSDT")).expect("the populated row");
        assert!(btc.contains("0.01") && btc.contains("0.001"), "{btc}");

        // An UNNARROWED listing states one number, not two: "2 of 2" would invent a filter.
        let whole = ls_lines(&rows, 2, false, false, "`binance`: 2 instruments.").join("\n");
        assert!(!whole.contains("2 of 2"), "nothing was narrowed: {whole}");
        assert!(whole.contains("`binance`: 2 instruments."), "{whole}");
    }

    /// The `ls` document carries the RAW grid and echoes what narrowed it — the two things a
    /// machine reader cannot recover from the table.
    ///
    /// ⚠ The listing's own `instruments` is empty here while the rows are passed apart, and that is
    /// exactly the split [`execute_ls`] performs: `vike_catalog::Instrument` cannot be CONSTRUCTED
    /// in this crate (see [`InstrumentRow`]), and by the time this renderer sees a row it is
    /// already flattened. The outcome fields and the row fields therefore come from the two halves
    /// independently, which is what this case checks.
    #[test]
    fn the_listing_document_carries_the_raw_grid_and_echoes_the_filter() {
        let args = parse_of(&["ls", "--venue", "binance", "--class", "CryptoPerp", "--json"])
            .expect("parses");
        let listing = CatalogListing {
            venue: "binance".to_string(),
            outcome: CatalogOutcome::Listed {
                instruments: Vec::new(),
                truncated: true,
                cached: true,
            },
        };
        let mut r = row("ETHUSDT", AssetClass::CryptoPerp);
        r.tick = 0.0;
        let doc: serde_json::Value = serde_json::from_str(&ls_json(&args, &listing, &[r]))
            .expect("the document is one JSON object");
        assert_eq!(doc["verb"], Verb::Ls.as_str());
        assert_eq!(doc["venue"], "binance");
        assert_eq!(doc["shown"], 1);
        assert_eq!(doc["filter"]["class"], AssetClass::CryptoPerp.sql_word());
        assert!(doc["filter"]["search"].is_null(), "an unused filter is null: {doc}");
        assert_eq!(doc["outcome_detail"]["truncated"], true);
        assert_eq!(doc["outcome_detail"]["cached"], true);
        // ⚠ The RAW `0.0`, never the table's dash: a machine reader asked for the grid in order to
        // fold it, and a dash is this side's reading rather than the datum.
        assert_eq!(doc["instruments"][0]["tick_size"], 0.0);
        assert_eq!(doc["instruments"][0]["asset_class"], AssetClass::CryptoPerp.sql_word());
    }

    /// The two documents a NON-listing answer produces. Both must be unmistakable for a listing:
    /// the refusal carries no `instruments` key at all, and an unrecorded instrument carries no
    /// zeroed grid — an absent number and a recorded zero are different facts about the world.
    ///
    /// ⚠ **Each half is now checked against the renderer its own verb REACHES**, which it was not:
    /// both halves ran on `refresh`'s `Args` while [`execute_refresh`] emitted
    /// [`outcome_only_json`] on that path, so the `reasked: null` assertion described a document
    /// this binary never produced. `ls` refuses through [`outcome_only_json`] and `refresh`
    /// through [`refresh_json`], and that is how they are exercised here.
    #[test]
    fn a_refusal_and_an_unrecorded_instrument_carry_no_rows_and_no_zeroes() {
        let ls_args = parse_of(&["ls", "--venue", "ig", "--json"]).expect("parses");
        let refresh_args = parse_of(&["refresh", "--venue", "ig", "--json"]).expect("parses");
        let listing = CatalogListing {
            venue: "ig".to_string(),
            outcome: CatalogOutcome::Refused(CatalogRefusal::NoBulkList {
                why: "searched live per query".to_string(),
            }),
        };
        let refused: serde_json::Value =
            serde_json::from_str(&outcome_only_json(&ls_args, &listing)).expect("a document");
        assert_eq!(refused["verb"], Verb::Ls.as_str());
        assert_eq!(refused["outcome_detail"]["refusal"], "no_bulk_list");
        assert!(refused["instruments"].is_null(), "a refusal lists nothing: {refused}");
        // ...and `refresh`'s own field is PRESENT and null rather than absent or a misleading
        // `false`: nothing was listed, so "did not re-ask" would invite a wrapper to retry
        // forever, while an absent key is the shape `ls` emits and tells this verb's consumer
        // nothing at all.
        let doc: serde_json::Value =
            serde_json::from_str(&refresh_json(&refresh_args, &listing)).expect("a document");
        assert_eq!(doc["verb"], Verb::Refresh.as_str());
        assert!(doc["reasked"].is_null(), "{doc}");
        assert!(
            doc.get("reasked").is_some(),
            "the key must be THERE — an absent one is `ls`'s document, not this verb's: {doc}"
        );
        // The control: a LISTED answer puts a real boolean in it, so the null above is the
        // refusal's own value rather than a field this renderer never fills.
        let listed = CatalogListing {
            venue: "ig".to_string(),
            outcome: CatalogOutcome::Listed {
                instruments: Vec::new(),
                truncated: false,
                cached: true,
            },
        };
        let fresh: serde_json::Value =
            serde_json::from_str(&refresh_json(&refresh_args, &listed)).expect("a document");
        assert_eq!(fresh["reasked"], false, "a cached listing re-asked nothing: {fresh}");

        let show_args = parse_of(&["show", "okx:BTC-USDT", "--json"]).expect("parses");
        let target = InstrumentRef { venue: "okx".to_string(), symbol: "BTC-USDT".to_string() };
        let missing: serde_json::Value =
            serde_json::from_str(&show_missing_json(&show_args, &target)).expect("a document");
        assert_eq!(missing["recorded"], false);
        assert!(missing["tick_size"].is_null(), "no grid field may be zeroed: {missing}");
        assert!(missing["asset_class"].is_null(), "{missing}");
        // The control: a RECORDED default grid does carry the zeroes, because they were recorded.
        let recorded: serde_json::Value = serde_json::from_str(&show_json(
            &show_args,
            &target,
            &vike_model::SymbolProperties::default(),
        ))
        .expect("a document");
        assert_eq!(recorded["recorded"], true);
        assert_eq!(recorded["tick_size"], 0.0);
    }

    /// `-` is the model's absent-is-`0.0`, and a real number is a real number. Paired, because a
    /// renderer that dashed everything would pass the first half alone.
    #[test]
    fn an_absent_grid_number_is_a_dash_and_a_present_one_is_itself() {
        assert_eq!(grid_cell(0.0), "-");
        assert_eq!(grid_cell(-1.0), "-");
        assert_eq!(grid_cell(0.001), "0.001");
        assert_eq!(grid_cell(100.0), "100");
    }

    /// The usage page is the only place this group's verbs and flags are named, so one missing
    /// from it is one an operator cannot discover. The assertion is over the ROW each one owns —
    /// the label column plus its own first line — never over the page.
    ///
    /// ⚠ **This shipped as `the_usage_names_every_verb_and_every_flag_this_parser_accepts` and it
    /// could not fail for either reason it named.** The verb half asserted
    /// `usage().contains(v.as_str())`, and every verb name occurs in the page's PROSE independently
    /// of its own block: measured over the text this replaced, `ls` appeared 9 times, `venues` 5,
    /// `show` 3 and `refresh` 2 (`--venue V   ls/refresh:`, "`ls` lists may have nothing here",
    /// "`data catalog venues` is the roster"), so deleting a verb's whole paragraph left this test
    /// AND `crates/vike-cli/tests/data_cli.rs`'s
    /// `the_catalog_group_answers_and_its_help_names_every_verb` green with the verb
    /// undiscoverable from `--help`. The flag half was a hand-typed array of seven
    /// spellings under a name promising "every flag this parser accepts" — the subtract-only shape
    /// that file's own `help_names_every_subcommand_and_exits_zero` warns about. Its doc's claim
    /// that "both rosters are DERIVED here" was true of the classes and of nothing else.
    ///
    /// What holds it now is structural first and assertive second: [`usage`] RENDERS one block per
    /// [`VERBS`] row through [`Verb::usage_block`]'s exhaustive match and one row per [`FLAGS`]
    /// entry, so a block cannot be deleted without deleting a declaration the compiler wants.
    ///
    /// ⚠ **The residual, declared rather than implied:** a flag arm added to [`parse`] with a
    /// fresh literal and no [`FLAGS`] row is invisible here. A `match` over literals cannot be
    /// enumerated from inside the process and this file has no source reflection, so no assertion
    /// can reach it. What narrows it is that every arm [`parse`] carries today names one of the
    /// consts [`FLAGS`] is built from — a new arm spelled as a bare literal is the only one that
    /// would not — and `every_declared_flag_is_accepted_by_the_parser` holds the other direction.
    #[test]
    fn the_usage_renders_a_block_for_every_verb_and_a_row_for_every_flag_it_declares() {
        let text = usage();
        let verb_head = |v: &Verb| format!("  {:<9}{}", v.as_str(), expand(v.usage_block()[0]));
        for v in VERBS {
            assert!(
                text.contains(&verb_head(v)),
                "`{}` has no block of its own: {text}",
                v.as_str()
            );
        }
        for f in FLAGS {
            let head = format!("  {:<12}{}", f.label(), expand(f.help[0]));
            assert!(text.contains(&head), "`{}` has no row of its own: {text}", f.label());
        }
        for class in AssetClass::ALL {
            assert!(text.contains(class.sql_word()), "the class roster is derived and complete");
        }
        assert!(text.contains(DEFAULT_ADDR), "the default address is stated, not implied");

        // THE KILL PROOF, and it is what separates this spelling from the one it replaced: strip
        // `refresh`'s row out of the rendered page and the check above fails on it — WHILE the
        // word `refresh` still occurs elsewhere in the page, which is precisely why
        // `contains("refresh")` stayed green over the same mutilation.
        let head = verb_head(&Verb::Refresh);
        let mutilated: Vec<&str> = text.lines().filter(|l| !l.contains(&head)).collect();
        let mutilated = mutilated.join("\n");
        assert!(!mutilated.contains(&head), "the mutilation must remove the row: {mutilated}");
        assert!(
            mutilated.contains(Verb::Refresh.as_str()),
            "…and the verb's NAME must survive it, which is the whole measurement: {mutilated}"
        );
    }

    /// Every flag [`FLAGS`] declares is one [`parse`] actually accepts — the other direction of
    /// the page, and the one that catches a row kept after its arm was deleted or renamed.
    #[test]
    fn every_declared_flag_is_accepted_by_the_parser() {
        // A value each flag will take. `ls` is the verb every one of them is legal on.
        fn probe(spelling: &str) -> &'static str {
            match spelling {
                FLAG_VENUE => "okx",
                FLAG_CLASS => AssetClass::ALL[0].sql_word(),
                FLAG_SEARCH => "BTC",
                FLAG_ADDR => "127.0.0.1:1",
                FLAG_FORMAT => "json",
                other => panic!("`{other}` declares a value placeholder and this probe has none"),
            }
        }
        for f in FLAGS {
            for spelling in f.spellings {
                let mut line = vec!["ls", FLAG_VENUE, "binance", *spelling];
                if !f.arg.is_empty() {
                    line.push(probe(spelling));
                }
                // ⚠ `-h`/`--help` is an `Err` BY DESIGN — it is the help REQUEST sentinel — so the
                // assertion is not "this parses" but "this is not an unknown option", which is the
                // one answer a deleted arm produces.
                if let Err(e) = parse_of(&line) {
                    assert!(
                        !e.contains("unknown option"),
                        "`{spelling}` has a usage row and no parser arm: {e}"
                    );
                }
            }
        }
        // The control: a spelling [`FLAGS`] does not declare IS refused that way, so the loop above
        // is not passing because nothing in this parser ever says "unknown option".
        let err = parse_of(&["ls", FLAG_VENUE, "binance", "--limit", "5"])
            .expect_err("an undeclared flag is unknown");
        assert!(err.contains("unknown option"), "{err}");
    }

    /// Every token [`expand`] is asked to substitute is one it knows. A `{…}` surviving into the
    /// rendered page is a block naming a fact nothing supplies — which reads to an operator as a
    /// literal brace where a vocabulary or an address should be.
    #[test]
    fn the_usage_leaves_no_placeholder_unexpanded() {
        let text = usage();
        assert!(!text.contains('{'), "an unexpanded placeholder survived into the page: {text}");
        // The control: the tokens are real and [`expand`] is doing work, so the assertion above is
        // not passing because the page never carried one.
        assert_eq!(expand("({classes})"), format!("({})", class_roster()));
        assert_eq!(expand("default {default_addr}."), format!("default {DEFAULT_ADDR}."));
    }
}
