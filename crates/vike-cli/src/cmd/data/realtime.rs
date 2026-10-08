//! `vike-cli data realtime` — WHEN = NOW, the surface design's §7 second group.
//!
//! ⚠ **A GROUP owns its own grammar**, which is why this is a module rather than more `Sub` arms —
//! `crate::cmd::data`'s [`super::Args`] answers for the `hist` group's verbs and every flag on it is
//! refused by name on the verbs it does not belong to, a discipline that holds because those verbs
//! share a vocabulary (a series, a window, a store). This group shares none of it: its noun is a
//! LIVE KEY — `(venue, symbol, lane)` — which lands in no store, has no window and no gap.
//!
//! # The two verbs, and the SUB-GROUP beside them
//!
//! | verb | wire | answers |
//! |---|---|---|
//! | `watch SPEC --lane L` | `MdSubscribe` + the `Md` push stream | what is happening on one key, now |
//! | `status` | the handshake THIS CONNECTION already performed | which venues this datahub ADVERTISES a live feed for |
//!
//! §7's tree also names `record` — what the BOX persists — and since 2026-09-22 it is BUILT and is
//! a GROUP rather than a verb: [`super::record`] owns `ls | add | rm` and its own `Args`/`parse`/`usage`,
//! routed above [`parse`] exactly as `crate::cmd::data` routes this group. It is the first
//! four-token path in this binary.
//!
//! ⚠ **This section said `record` was deliberately ABSENT, and before that that it was BLOCKED on
//! an unruled question. Both are history now** and the sentences are struck here rather than
//! rewritten in place, because a reader who deferred work against either deserves to see which
//! part broke:
//!
//! * *"blocked on a scope classification (`docs/decisions/0052` and `0058` … point in different
//!   directions)"* — **RULED 2026-09-21.**
//!   `docs/decisions/0081-a-recorded-subscription-is-a-write-verb.md`: `record add`/`rm` is a
//!   **Write** verb needing node keys, the classification `DeleteSeries` carries. (⚠ `VerbScope`
//!   is `Handshake | Read | Write` today; 0052/0058 say Observe/Control.)
//! * *"the subscription set is a file on the server's own box, read once at mount"* — **FALSE**
//!   since the recorder-profile branch landed: it is settings-DB ROWS, and
//!   `vike_secrets::profile_store::render_recorder_toml` renders them back into the document the
//!   mount already read, which is why nothing above the store had to change.
//! * *"No restart — the recorder's tick loop already drives the venue feed to the resolved set
//!   every tick"* — **WRONG, and it was the load-bearing half.** The owner ruled on 2026-09-22
//!   that a row is live at the daemon's NEXT RESTART: the tick re-resolves a subscription's
//!   SYMBOLS and never re-reads the PROFILE, `vike_recorder::runtime`'s `RecorderRuntime` has
//!   `add_feed` and no removal, and `VenueFeed` cannot say which subscription built it. The
//!   asymmetry that sentence was describing — `add` live, `rm` at restart — is REFUSED rather than
//!   merely unbuilt, because an operator would see `record ls` come back empty while the tape kept
//!   growing. [`super::record`]'s own doc carries the ruling.
//!
//! What still holds is the REMOTE half: `vike_datahub_client::proto::Request` has no arm that reads
//! that set, adds to it or removes from it, so `--addr` from another machine is still real work and
//! the Write scope is what governs it — which is why [`super::record`] accepts `--addr` and refuses it by
//! name.
//!
//! # ⚠ `status` IS AN ADVERTISEMENT, NOT A LIVENESS PROBE
//!
//! §11's phase table says `status` needs no server work, and that is true only because there is no
//! feed-health verb to call: `MdSubscribe`/`MdUpdate` and the three `Md*` responses are the ENTIRE
//! realtime surface. What a handshake DOES carry is one `md_venue=<slug>` entry per venue the
//! server's build links a market-data client for
//! ([`vike_datahub_client::md_venue_feature`]), and that is what this verb reports —
//! read through [`vike_datahub_client::advertised_md_venues`], the READ half of that pair.
//!
//! So an entry means *this server would accept a subscription for that venue*. It does **not** mean
//! a venue socket is open, that a frame has arrived, or that the feed is healthy. §11 calls the verb
//! "feed health" and a reader will expect up/down, so [`NOT_A_PROBE`] says the opposite in as many
//! words and rides EVERY answer, table and document alike. Printing a green column that nothing
//! measured is positive confirmation of something false — the defect class this repository deleted
//! `Policy::max_total_exposure` for.
//!
//! A real probe — subscribe briefly to each advertised venue and time the first frame — is a
//! defensible design and is deliberately NOT built here: it would make a read-only status verb open
//! venue sockets on the server's behalf, which is a DECISION about what this box does rather than an
//! implementation detail.
//!
//! # ⚠ It does not reuse `catalog`'s three-state server view, and the asymmetry is the reason
//!
//! `crate::cmd::data::catalog`'s `venues` tells an UNREACHABLE datahub from one that answered and
//! then refused, because its local capability columns are the bulk of that answer and a missing
//! server must not empty them. `status` has no local half at all — every fact in it comes from the
//! handshake — so an unreachable datahub leaves nothing to render and is simply
//! [`crate::exit::Exit::Connect`], through the same [`super::connect`] the `hist` read verbs use. One
//! verb needs the distinction and one has nothing to distinguish; sharing the machinery would put a
//! three-armed enum in front of a verb with one arm.
//!
//! # `watch`: the rules §8.3 states, and where each one lands in the code
//!
//! * **Every stream is BOUNDED by default.** [`Bound`] — `--for`, `--events`, or both, and
//!   `--unbounded` has to be asked for. *"That is the difference between a verb that goes in a
//!   pipeline and a verb that goes in a screenshot."*
//! * **Three lanes and no quotes lane.** [`parse_lane`] refuses `quotes` BY NAME rather than mapping
//!   it onto `depth`: a superseded depth frame is not a loss and a dropped print is, and this wire
//!   discloses the difference as [`MdFrame::TapeGap`].
//! * **A CLAMP IS AN ACCEPTANCE.** [`MdSpec::depth_levels`]'s own doc says so — a client that asked
//!   for 200 and got 50 learns it from `MdSubscribed.accepted`, so [`depth_note`] renders the number
//!   the SERVER served rather than the one the operator typed.
//! * **`--format`**: `jsonl` for a non-terminal destination, `table` for a terminal
//!   ([`default_render`]).
//! * **A FAULT IS NEVER EXIT 0**, and `--unbounded` does not change that. [`End::is_fault`] splits
//!   the ways a stream can END (a goodbye, a hang-up, a reader that left) from the ways it can
//!   BREAK (a dead link, a transport error, a protocol desync); the bound decides only whether an
//!   ENDING was the one asked for. A wrapper piping `--out FILE` reads one channel and it has to
//!   carry that difference — see [`End::was_asked_for`] for what it cost when it did not.
//!
//! ⚠ **This group parses `--format` itself rather than calling `super::parse_format`**, and that is
//! the narrow half of "jsonl ships now": that function is the CATALOG verbs' parser and refuses
//! `jsonl` by name, which is the right answer one group over and the wrong one here. [`Render`]
//! admits `jsonl` for `watch` and for nothing else, so `data hist ls --format jsonl` is refused
//! exactly as it was.
//!
//! ⚠ **The REASON that sibling gives changed underneath this paragraph, and the shape did not.**
//! It used to refuse `jsonl` as designed-and-unbuilt, *"waiting on a verb there that emits rows"*;
//! `data hist get` shipped, so it now refuses it as built-ELSEWHERE and names that verb. Nothing
//! here moved — three parsers still answer for three groups — but a reader who took the old
//! sentence for the current one would think `jsonl` is still unreachable from the `hist` group,
//! which it is not.
//!
//! # ⚠ ON `watch`, STDOUT CARRIES FRAMES AND NOTHING ELSE
//!
//! The subscription notes, the clamp disclosure and the closing summary all go to STDERR (or, under
//! `--out FILE`, stdout keeps nothing at all and the file keeps only frames). A `| jq` reads a clean
//! stream, a `--out` file is the tape and not a transcript of this binary's opinions, and the one
//! thing an operator must not have to do is strip our prose out of their data.

mod grammar;
mod render;
mod status;
mod watch;

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use vike_datahub_client::market::MdLane;
use vike_node_proto::auth::NodeKeys;

use super::{DEFAULT_ADDR, col, connect};
use super::{FILE_VERB, ROW_VERB};
use crate::cmd::args::exit_for_parse_error;

use self::grammar::parse;
use self::render::usage;
use self::watch::execute;

/// What [`exit_for_parse_error`] and every failure line name this command. The GROUP is part of it,
/// for `crate::cmd::data::catalog`'s reason: `vike-cli data: …` on a line typed under
/// `data realtime` would send a reader to the `hist` group's usage, which is the page that does not
/// contain the flag they got wrong.
const COMMAND: &str = "data realtime";

/// Which verb ran.
///
/// Adding one is FOUR edits and the compiler asks for three: an arm here, an arm in
/// [`Verb::as_str`], an arm in [`execute`]. The fourth — a row in [`VERBS`] — is the one nothing
/// forces, and it is the one that costs the most: the missing-verb and unknown-verb refusals are
/// both RENDERED from that roster, so a verb absent from it is one an operator can neither discover
/// nor be told about while it still parses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verb {
    /// `watch SPEC --lane L` — one key's live frames, bounded.
    Watch,
    /// `status` — what this datahub advertises. See the module doc for what that word may not be
    /// read as.
    Status,
}

impl Verb {
    /// The name the operator typed, which is also what every refusal names it by.
    fn as_str(self) -> &'static str {
        match self {
            Verb::Watch => "watch",
            Verb::Status => "status",
        }
    }
}

/// HOW a verb renders its answer.
///
/// A type of this group's own rather than `crate::cmd::data`'s [`super::Format`] — see the module
/// doc. The two verbs take DIFFERENT halves of it and [`parse`] refuses the wrong half per verb,
/// because the split is a fact about the shape of each answer rather than a preference: a stream is
/// a sequence and cannot be one document, and `status` is one question about one server and is not a
/// sequence of anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Render {
    /// Aligned columns and the disclosures, for a person at a terminal.
    Table,
    /// ONE JSON object per FRAME, newline-separated — `watch`'s machine form.
    Jsonl,
    /// ONE JSON document — `status`'s machine form, and what `--json` has always meant.
    Json,
}

/// The `VENUE:SYMBOL` positional, parsed. A struct rather than two `Option<String>`s on [`Args`],
/// for the reason `crate::cmd::data`'s `RmArgs` gives for its own: an `Args` that can hold half a
/// key is an `Args` some future arm will read one from.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Key {
    venue: String,
    symbol: String,
}

/// How a stream ENDS — §8.3's *"every stream is bounded by default"*, as a type.
///
/// ⚠ The default is the refusal: a `watch` naming none of the three is a USAGE error, not an
/// unbounded stream. A verb that never returns cannot go in a pipeline, a CI step or a `$(…)`, and
/// the one thing worse than having to type a bound is discovering you needed one from a terminal
/// that has stopped responding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bound {
    /// At least one of the two is `Some`, and whichever is reached FIRST ends the stream. Both are
    /// admitted together deliberately: *"500 frames or 30 seconds, whichever lands first"* is ONE
    /// bound in a pipeline, and refusing the pair would make the caller implement it with a
    /// `timeout(1)` wrapper that turns a clean end into a signal.
    First { events: Option<usize>, duration: Option<Duration> },
    /// `--unbounded`: runs until the far side ends it or the reader goes away.
    ///
    /// ⚠ It removes the BOUND, never the fault classification: a dead link, a transport error and
    /// a protocol desync still exit non-zero here — [`End::was_asked_for`] carries what folding
    /// those in with a clean stop cost.
    Unbounded,
}

/// The parsed `data realtime …` line. PURE — the whole grammar is unit-tested below.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Args {
    verb: Verb,
    /// `watch`'s positional. Always `Some` on [`Verb::Watch`] and always `None` otherwise — [`parse`]
    /// refuses both other shapes rather than defaulting either way.
    key: Option<Key>,
    /// `--lane`. REQUIRED on `watch`, refused on `status`.
    lane: Option<MdLane>,
    /// `--depth N`, RAW. Carried to the wire unchanged so the server's clamp is the only one.
    depth: Option<u16>,
    /// How the stream ends. `Bound::Unbounded` only when it was asked for.
    bound: Bound,
    /// `--out FILE` — `watch` only.
    out: Option<String>,
    /// `None` = follow the destination; see [`default_render`]. Resolved at RUN time rather than
    /// here, because "is stdout a terminal" is I/O and this function is pure.
    render: Option<Render>,
    addr: String,
}

// ─── running ─────────────────────────────────────────────────────────────────────────────────────

/// Run a `data realtime …` line. `argv` is everything AFTER the group word.
///
/// ⚠ **THE SUB-GROUP LAYER SPLITS HERE, above [`parse`]**, which is `crate::cmd::data::run`'s own
/// rule applied one rung down and for the same reason: [`Args`] is one struct answering for this
/// group's two STREAM verbs, and every flag on it is refused by name on the verb it does not
/// belong to — a discipline that holds because those two share a vocabulary (a live key, a lane, a
/// bound). `record`'s noun is a stored SELECTION and shares none of it, so folding it in would make
/// one type answer for two command languages and every refusal in it ambiguous.
///
/// ⚠ `settings_dir` is a PARAMETER, and `super::record`'s own doc carries why `project_root` could
/// not have substituted for it.
pub(super) fn run(
    argv: &[String],
    settings_dir: Option<&Path>,
    keys: Option<&NodeKeys>,
    configured_addr: Option<&str>,
) -> ExitCode {
    if argv.first().is_some_and(|w| w == "record") {
        return super::record::run(&argv[1..], settings_dir, keys, configured_addr);
    }
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

#[cfg(test)]
use std::io::Write;
#[cfg(test)]
use std::net::TcpStream;

#[cfg(test)]
use vike_datahub_client::market::{MD_DEPTH_LEVELS_CEILING, MD_DEPTH_LEVELS_DEFAULT};
#[cfg(test)]
use vike_datahub_client::market::{MdFrame, MdRefusal, MdSpec, WireStreamStatus};
#[cfg(test)]
use vike_datahub_client::{BookSnapshot, FEATURE_MARKET_DATA, MdBye, proto::Response};

#[cfg(test)]
use self::grammar::{LANES, SUBGROUPS, VERBS, default_render, lane_roster};
#[cfg(test)]
use self::grammar::{parse_depth, parse_for, parse_key, parse_lane, parse_render};
#[cfg(test)]
use self::render::{aggressor, bye_sentence, bye_token, jsonl_row, status_sentence, table_line};
#[cfg(test)]
use self::status::{ServerFeeds, status_json, status_lines, status_notes};
#[cfg(test)]
use self::watch::{End, Sink, Tally, count_frame, depth_note};
#[cfg(test)]
use self::watch::{refusal_sentence, stream_frames, summary_lines};

#[path = "realtime_tests.rs"]
#[cfg(test)]
mod realtime_tests;
