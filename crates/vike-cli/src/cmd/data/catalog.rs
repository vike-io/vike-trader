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

use std::process::ExitCode;

use vike_datahub_client::advertised_md_venues;
use vike_model::AssetClass;
use vike_node_proto::auth::NodeKeys;

use crate::cmd::args::exit_for_parse_error;
use crate::exit::CmdResult;

use super::{CLASS_AS_OF_TS, DEFAULT_ADDR, Format, col, connect, empty_note, parse_format};

mod grammar;
mod json;
mod ls;
mod venues;

use self::grammar::{parse, usage};
use self::json::{document_head, merged};
use self::ls::{execute_ls, execute_refresh, execute_show};
use self::venues::execute_venues;

#[cfg(test)]
use self::grammar::{FLAG_ADDR, FLAG_CLASS, FLAG_FORMAT, FLAG_SEARCH, FLAG_VENUE, FLAGS};
#[cfg(test)]
use self::grammar::{class_roster, expand};
#[cfg(test)]
use self::json::{ls_json, outcome_json, outcome_only_json, refresh_json};
#[cfg(test)]
use self::json::{show_missing_json, venues_json};
#[cfg(test)]
use self::ls::{grid_cell, keeps, ls_lines, refresh_lines, show_lines, truncation_warning};
#[cfg(test)]
use self::venues::{SERVER_VERBS, answered_and_refused, live_lanes, matrix, skew, venues_lines};
#[cfg(test)]
use std::io;
#[cfg(test)]
use vike_datahub_client::catalog::validate_catalog_venue;
#[cfg(test)]
use vike_datahub_client::catalog::{CatalogListing, CatalogOutcome, CatalogRefusal};
#[cfg(test)]
use vike_datahub_client::{FEATURE_BACKFILL, FEATURE_VENUE_CATALOG};
#[cfg(test)]
use vike_model::venues::venue_caps::LiveDataCaps;

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
/// ⚠ **It was flattened because `vike_catalog::Instrument` could not be NAMED here, and that
/// stopped being true on 2026-09-30.** This paragraph said `vike-catalog` "is not a dependency" of
/// `crates/vike-cli/Cargo.toml` and that adding it "would pull the whole catalog tree" in. The
/// first half went the day the history-channels table took the edge for its own reason — the
/// manifest carries the argument and its measurement: the crate was already linked through
/// `vike-datahub-client`, so the edge adds no package — and the second half was never a link-time
/// fact. The struct STAYS flat: it converts at the one site where type inference supplies the name
/// and carries plain fields everywhere else, the answer `crate::cmd::data::tape_health`'s module
/// doc records for `vike_data::TsRange`, which really is out of reach. But that is a CHOICE now,
/// and re-shaping this struct onto the wire type is its own change.
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

// ─── `venues`: THE CAPABILITY MATRIX ────────────────────────────────────────────────────────────

/// One venue's row of the matrix, as THIS BUILD declares it.
///
/// ⚠ **Both halves come from `vike_model::venues::venue_caps`, and the backfill half is below a LAYER
/// WALL.** `crates/vike-cli/Cargo.toml` declares no `vike-backfill` edge and must not grow one —
/// that crate drags DataFusion and the whole collector tree into a binary whose identity is being
/// free of both, which CI's `light-consumers` lane exists to catch. The backfill columns are
/// reachable anyway because `vike_model::VenueCaps` carries `backfill_bars` /
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
    /// `vike_datahub_client::md_venue_feature`, and the ONE genuinely per-venue fact a
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

#[path = "catalog_tests.rs"]
#[cfg(test)]
mod catalog_tests;
