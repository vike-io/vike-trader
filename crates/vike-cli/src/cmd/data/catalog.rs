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
    DatahubClient, FEATURE_BACKFILL, FEATURE_VENUE_CATALOG, advertised_md_venues,
};
use vike_model::AssetClass;
use vike_model::venue_caps::{LiveDataCaps, caps_for};
use vike_node_proto::auth::{NodeKeys, Scope};

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

#[path = "catalog_tests.rs"]
#[cfg(test)]
mod catalog_tests;
