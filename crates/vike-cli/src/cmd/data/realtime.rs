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

use std::fs::File;
use std::io::{self, BufWriter, IsTerminal, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use vike_datahub_client::market::{
    MD_DEPTH_LEVELS_CEILING, MD_DEPTH_LEVELS_DEFAULT, MD_READ_TIMEOUT, MdFrame, MdLane, MdRefusal,
    MdSpec, WireStreamStatus, validate_md_symbol,
};
use vike_datahub_client::{
    BookSnapshot, FEATURE_MARKET_DATA, MdBye, advertised_md_venues, proto::Response, read_frame,
};
use vike_model::time::epoch_ms_to_utc_timestamp;
use vike_node_proto::auth::{NodeKeys, Scope};

use super::{DEFAULT_ADDR, col, connect};
use crate::cmd::args::{Flags, exit_for_parse_error, help_requested, no_value};
use crate::exit::{CliError, CmdResult};

/// What [`exit_for_parse_error`] and every failure line name this command. The GROUP is part of it,
/// for `crate::cmd::data::catalog`'s reason: `vike-cli data: …` on a line typed under
/// `data realtime` would send a reader to the `hist` group's usage, which is the page that does not
/// contain the flag they got wrong.
const COMMAND: &str = "data realtime";

// ─── the vocabulary ──────────────────────────────────────────────────────────────────────────────

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

/// Every verb, in the order [`usage`] lists them — and the roster the refusals RENDER rather than
/// restate. `crate::cmd::data`'s `SUBCOMMANDS` exists for the same reason and carries the incident:
/// the one message whose whole job is to name a roster named it short.
const VERBS: &[Verb] = &[Verb::Watch, Verb::Status];

/// The SUB-GROUPS of `data realtime` — routed above [`parse`] by [`run`], exactly as
/// `crate::cmd::data` routes this group, so no sub-group word reaches that parser at all.
///
/// ⚠ They are NOT [`Verb`] variants and they still have to be in the ROSTER, which is the whole
/// reason this const exists rather than the routing being left implicit: the missing-verb and
/// unknown-verb refusals are both RENDERED from [`verb_roster`], so a sub-group absent from it is
/// one an operator can neither discover nor be told about while it parses perfectly well. That is
/// the incident [`VERBS`]' own doc carries, wearing a different hat.
const SUBGROUPS: &[&str] = &["record"];

impl Verb {
    /// The name the operator typed, which is also what every refusal names it by.
    fn as_str(self) -> &'static str {
        match self {
            Verb::Watch => "watch",
            Verb::Status => "status",
        }
    }
}

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
const LANES: &[MdLane] = &[MdLane::Depth, MdLane::Book, MdLane::Trades];

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
fn lane_roster() -> String {
    LANES.iter().map(|l| l.feed_stream_label()).collect::<Vec<_>>().join(" | ")
}

/// `--lane`'s value, resolved against the wire's own lane labels.
fn parse_lane(value: &str) -> Result<MdLane, String> {
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
fn parse_render(value: &str) -> Result<Render, String> {
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
fn default_render(verb: Verb, to_a_file: bool, stdout_is_a_terminal: bool) -> Render {
    match verb {
        Verb::Watch if to_a_file || !stdout_is_a_terminal => Render::Jsonl,
        Verb::Watch | Verb::Status => Render::Table,
    }
}

// ─── the spec ────────────────────────────────────────────────────────────────────────────────────

/// The `VENUE:SYMBOL` positional, parsed. A struct rather than two `Option<String>`s on [`Args`],
/// for the reason `crate::cmd::data`'s `RmArgs` gives for its own: an `Args` that can hold half a
/// key is an `Args` some future arm will read one from.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Key {
    venue: String,
    symbol: String,
}

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
fn parse_key(spec: &str) -> Result<Key, String> {
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
fn parse_for(raw: &str) -> Result<Duration, String> {
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
fn parse_depth(raw: &str) -> Result<u16, String> {
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

/// Parse this group's argv tail (everything after the group word). PURE — no I/O, no socket.
///
/// ⚠ Every flag is accepted by the ONE loop below and refused PER VERB afterwards, rather than being
/// routed by a per-verb match. That ordering is `crate::cmd::data`'s and it buys the same thing: an
/// inapplicable flag is named in a message that says which verb it DOES belong to, where an
/// unknown-option error would tell an operator the flag does not exist — which is false, and sends
/// them looking in the wrong place.
fn parse(argv: &[String], configured_addr: Option<&str>) -> Result<Args, String> {
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

/// A [`Render`] as the operator spells it — used by the one refusal that has to name the value it
/// was handed back.
fn render_word(render: Render) -> &'static str {
    match render {
        Render::Table => "table",
        Render::Jsonl => "jsonl",
        Render::Json => "json",
    }
}

// ─── the usage page ──────────────────────────────────────────────────────────────────────────────

/// This group's usage page.
///
/// A FUNCTION rather than a `const` because two of the numbers on it are the WIRE's
/// ([`MD_DEPTH_LEVELS_DEFAULT`], [`MD_DEPTH_LEVELS_CEILING`]) and one is the plane's
/// ([`DEFAULT_ADDR`]); a `&'static str` cannot `format!`, and a copy typed here is exactly the shape
/// this repository has watched rot. `the_usage_leaves_no_placeholder_unexpanded` reddens on a token
/// nothing substitutes.
fn usage() -> String {
    const PAGE: &str = "\
usage: vike-cli data realtime <verb> [options]

WHEN = NOW. `data hist` answers about a past window; this group answers about the live
wire — what a datahub is streaming right now, and which venues it says it can stream.

  watch SPEC --lane L [--depth N] (--for D | --events N | --unbounded)
               follow ONE key's live frames. SPEC is VENUE:SYMBOL — the venue's OWN
               spelling, passed to the wire verbatim (binance:BTCUSDT, okx:BTC-USDT-SWAP,
               polymarket:<token id>). ⚠ EVERY STREAM IS BOUNDED: name --for or --events,
               or both. --unbounded exists and has to be asked for
  status       WHICH VENUES this datahub ADVERTISES a live market-data feed for, read off
               the handshake this connection already performed. ⚠ An ADVERTISEMENT, never
               a liveness probe: nothing here opens a venue socket or observes a frame,
               and every answer says so in its own output
  record ...   WHAT THIS BOX PERSISTS — a SUB-GROUP (ls | add | rm) with a page of its own:
               `data realtime record --help`. It edits the recording daemon's subscription
               ROWS in this project's settings database, where the two verbs above touch no
               store at all. ⚠ A row is live at the daemon's NEXT RESTART

options:
  --lane L     watch: which lane — {lanes}. REQUIRED, because the lane is the LOSS
               CONTRACT rather than a detail: `depth` CONFLATES (latest-wins; a superseded
               frame is not a loss) and `book` is LOSSLESS, so they never share a name.
               There is no `quotes` lane on this wire and asking for one is refused by name
  --depth N    watch: levels per side on `depth`/`book` (the wire's default is
               {default_depth}, its ceiling {ceiling}). ⚠ A request above the ceiling is
               CLAMPED AND ACCEPTED, never refused — so the number the SERVER served is
               reported before the first frame. The ONE value this side refuses is one
               that will not FIT the wire's field: levels ride as a u16, so {max_depth} is
               the largest number that can be sent at all. Refused on `trades`, which has
               no levels
  --for D      watch: stop after this much wall-clock time — 30s | 5m | 2h. Seconds are
               admitted here and nowhere else in this workspace's duration grammar,
               because a live stream is the one thing measured in them
  --events N   watch: stop after N DATA frames. A heartbeat, a stream-status disclosure
               and a tape-gap marker are PRINTED and NOT counted — they are what the wire
               says ABOUT the stream rather than the stream
  --unbounded  watch: no bound at all. Runs until the server says goodbye, the reader goes
               away, or you stop it — each of which is an exit 0. ⚠ A FAULT IS NOT: a dead
               link, a transport error and a protocol desync exit NON-ZERO under every
               bound, this one included, so a wrapper piping --out FILE can tell a
               finished capture from a broken one
  --out FILE   watch: write the frames to FILE instead of stdout, flushed line by line so
               a stream you interrupt keeps everything it had already seen
  --addr H:P   the datahub to ask (default {default_addr}). It binds localhost, so reach a
               remote one over `ssh -L 7878:localhost:7878`
  --format F   HOW the answer is rendered, and the two verbs take different halves of it.
               watch: `jsonl` (one JSON object per frame) or `table`, and its default
               follows the DESTINATION — a terminal gets `table`, a pipe or a --out FILE
               gets `jsonl`. status: `json` or `table`, defaulting to `table` on either
               side of a pipe, like every other verb on this plane
  --json       status: shorthand for --format json. Refused on `watch`, by name: a stream
               is a SEQUENCE of frames and `--format jsonl` is its machine form
  -h, --help   this message

⚠ On `watch`, STDOUT CARRIES FRAMES AND NOTHING ELSE. The subscription notes, the clamp
  disclosure and the closing summary all go to stderr, so `| jq` reads a clean stream and
  a --out file holds the tape rather than a transcript of this binary's opinions.";
    PAGE.replace("{lanes}", &lane_roster())
        .replace("{default_depth}", &MD_DEPTH_LEVELS_DEFAULT.to_string())
        .replace("{ceiling}", &MD_DEPTH_LEVELS_CEILING.to_string())
        // The one bound [`parse_depth`] applies, rendered from the WIRE's field width rather than
        // typed — the same rule as the two numbers above it, for the same reason.
        .replace("{max_depth}", &u16::MAX.to_string())
        .replace("{default_addr}", DEFAULT_ADDR)
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

/// Route the parsed line. The two arms share the dial and nothing else.
fn execute(args: &Args, keys: Option<&NodeKeys>) -> CmdResult<()> {
    match args.verb {
        Verb::Watch => execute_watch(args, keys),
        Verb::Status => execute_status(args, keys),
    }
}

// ─── `watch` ─────────────────────────────────────────────────────────────────────────────────────

/// `data realtime watch` — subscribe, stream until the bound, report what happened.
fn execute_watch(args: &Args, keys: Option<&NodeKeys>) -> CmdResult<()> {
    let key = args.key.as_ref().expect("parse guarantees a key on `watch`");
    let lane = args.lane.expect("parse guarantees a lane on `watch`");
    let asked = MdSpec {
        venue: key.venue.clone(),
        symbol: key.symbol.clone(),
        lane,
        depth_levels: args.depth,
    };

    let client = connect(&args.addr, keys, Scope::Read)?;
    // ⚠ The client is CONSUMED on success and handed back INTACT on failure — leg (3) of
    // `FEATURE_MARKET_DATA`'s contract, which is why the error arm carries it. This verb has nothing
    // further to ask a server that said no, so it drops it; the message is the server's own (or the
    // client's local capability refusal, which names the key an operator has to set there).
    let (info, stream) = client.md_subscribe(vec![asked.clone()]).map_err(|(_client, msg)| {
        CliError::failed(format!("the subscription was not opened: {msg}"))
    })?;

    // ⚠ Matched on `MdSpec::key`, which EXCLUDES `depth_levels` by construction — its own doc says
    // so, and it exists precisely so no consumer re-derives the tuple and forgets the exclusion.
    // Comparing whole specs here would fail to find our own subscription the moment a depth was
    // clamped, which is the one case this code is about.
    if let Some((_, why)) = info.refused.iter().find(|(spec, _)| spec.key() == asked.key()) {
        return Err(CliError::failed(refusal_sentence(&asked, why)));
    }
    let Some(accepted) = info.accepted.iter().find(|spec| spec.key() == asked.key()) else {
        return Err(CliError::failed(format!(
            "the server neither accepted nor refused {}:{} on the {} lane — it answered about \
             neither, which is a protocol desync rather than a `no`. Nothing was streamed",
            asked.venue,
            asked.symbol,
            lane.feed_stream_label()
        )));
    };

    // ⚠ EVERY line from here to the summary goes to STDERR: stdout is the stream. See the module
    // doc — a `| jq` must read frames and a `--out` file must hold nothing else.
    for note in subscribe_notes(&asked, accepted, info.heartbeat_ms) {
        eprintln!("{note}");
    }

    let render = args.render.unwrap_or_else(|| {
        default_render(args.verb, args.out.is_some(), io::stdout().is_terminal())
    });
    let mut sink = Sink::open(args.out.as_deref())?;
    let (end, tally) = stream_frames(stream, args.bound, render, &mut sink);
    // ⚠ The summary is emitted BEFORE the close, so a failing flush reports itself INSTEAD of the
    // rung and never instead of the counts: how much arrived is the one thing an operator cannot
    // re-derive from a truncated file.
    for line in summary_lines(&asked, &end, &tally) {
        eprintln!("{line}");
    }
    sink.finish()?;
    if end.was_asked_for(args.bound) {
        return Ok(());
    }
    // A wrapper must be able to tell a finished capture from a broken one, and the exit code is the
    // only channel a pipeline reads — so the two ways of not getting what was asked for are NAMED
    // apart here rather than sharing one sentence. A FAULT is the one that also reaches this line
    // under `--unbounded`, where there is no bound to have missed.
    Err(CliError::failed(if end.is_fault() {
        format!("the stream FAILED: {}", end.sentence())
    } else {
        format!("the stream ended before the bound was reached: {}", end.sentence())
    }))
}

/// What the server said about the subscription, as lines an operator reads BEFORE the first frame.
///
/// ⚠ The depth line is the one that matters and [`MdSpec::depth_levels`]' own doc is why: *"anything
/// above `MD_DEPTH_LEVELS_CEILING` is CLAMPED, and the clamped number comes back in
/// `MdSubscribed.accepted` — a clamp is an acceptance with a smaller number, never a refusal, so a
/// client that asked for 200 LEARNS it got 50."* Rendering the number the operator TYPED is exactly
/// the failure that sentence exists to prevent.
fn subscribe_notes(asked: &MdSpec, accepted: &MdSpec, heartbeat_ms: u64) -> Vec<String> {
    let mut notes = vec![format!(
        "watching {} {} on the {} lane",
        accepted.venue,
        accepted.symbol,
        accepted.lane.feed_stream_label()
    )];
    if let Some(note) = depth_note(asked, accepted) {
        notes.push(note);
    }
    notes.push(format!(
        "the server heartbeats every {heartbeat_ms}ms when it has nothing to say, so silence longer \
         than that is a fault rather than a quiet market"
    ));
    notes
}

/// The depth disclosure, or `None` on a lane that has no levels.
///
/// The comparison is between the RAW request and the SERVER's resolved answer, deliberately: a
/// client-side `resolved_depth()` of the request would already have clamped 5000 to 200, so a note
/// built from it would tell the operator they asked for a number they did not type.
fn depth_note(asked: &MdSpec, accepted: &MdSpec) -> Option<String> {
    if accepted.lane == MdLane::Trades {
        return None;
    }
    let served = accepted.resolved_depth();
    Some(match asked.depth_levels {
        Some(n) if n != served => format!(
            "⚠ the depth was CLAMPED: you asked for {n} levels a side and this server serves \
             {served}. That is an ACCEPTANCE with a smaller number, not a refusal — every frame \
             below carries {served}"
        ),
        Some(n) => format!("depth: {n} levels a side, as asked"),
        None => format!(
            "depth: {served} levels a side (the wire's default — `--depth N` asks for more, up to \
             {MD_DEPTH_LEVELS_CEILING})"
        ),
    })
}

/// One refused spec, as a sentence.
///
/// ⚠ The `String` each variant carries is the FAR SIDE's own text
/// (`vike_data::require_live_verb`'s words for a lane, [`validate_md_symbol`]'s for a symbol) and is
/// forwarded verbatim rather than re-worded, for the reason those variants state: one refusal, one
/// wording, wherever it is reached from.
///
/// The match has no `_` arm, so a new [`MdRefusal`] cannot inherit a sentence written for a
/// different one.
fn refusal_sentence(asked: &MdSpec, why: &MdRefusal) -> String {
    let what =
        format!("{}:{} on the {} lane", asked.venue, asked.symbol, asked.lane.feed_stream_label());
    let detail = match why {
        MdRefusal::UnknownVenue => format!(
            "`{}` is not a venue this workspace knows at all. `data catalog venues` is the roster",
            asked.venue
        ),
        MdRefusal::VenueNotServed(served) => format!(
            "this server's build links no market-data client for `{}`. It serves: {served}. \
             `data realtime status` is the same list, asked before you type",
            asked.venue
        ),
        MdRefusal::LaneUnsupported(msg) => msg.clone(),
        MdRefusal::SymbolRejected(msg) => msg.clone(),
        MdRefusal::KeyCapTotal { held, cap } => format!(
            "the server's process-wide key budget is full ({held} of {cap} held). Nothing about \
             this spec is wrong"
        ),
        MdRefusal::KeyCapVenue { venue, held, cap } => format!(
            "the server's key budget for `{venue}` is full ({held} of {cap} held) — that cap is \
             what protects the order-signing daemon's share of this box's venue budget"
        ),
        MdRefusal::SpecCapSession { held, cap } => {
            format!("this session already holds its maximum of {held}/{cap} specs")
        }
    };
    // ⚠ `is_permanent` is the WIRE's own classification, not a reading of the text — it is what a
    // retrying client uses to decide whether to keep a spec in its desired set, and an operator
    // deserves the same answer rather than having to infer it.
    let again = if why.is_permanent() {
        "Retrying cannot change this answer on this server."
    } else {
        "This is a CAP and can free up — the same line may work later."
    };
    format!("{what} was REFUSED: {detail}. {again}")
}

// ─── the stream ──────────────────────────────────────────────────────────────────────────────────

/// Why the stream stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
enum End {
    /// `--events N` was reached.
    Events(usize),
    /// `--for D` elapsed.
    Elapsed,
    /// The server sent [`MdFrame::Bye`] and closed.
    Bye(MdBye),
    /// The read deadline the SERVER's own heartbeat period armed expired with no frame. The link is
    /// dead — `vike_app_core::data::md_session`'s reader draws the same conclusion from the same signal.
    Silent,
    /// A clean end-of-stream: the socket closed without a `Bye`.
    Closed,
    /// The socket carried something that is not an `Md` frame, which after `MdSubscribed` is a
    /// protocol desync by the wire's own §0 invariant.
    Desync(String),
    /// The transport failed.
    Fault(String),
    /// The READER went away — a `| head -3` that got what it wanted, or a closed pager. Normal, and
    /// deliberately not a failure under any bound.
    ReaderGone,
}

impl End {
    /// Did something BREAK, as opposed to the stream simply ending?
    ///
    /// ⚠ **This is the seam the exit code rests on, and it is separate from [`End::was_asked_for`]
    /// because the two questions have different scopes**: an early ENDING is a failure only against
    /// a bound that was missed, while a FAULT is a failure under every bound there is. A dead link
    /// ([`End::Silent`] — silence past the deadline the server's OWN heartbeat period armed), a
    /// transport error and a protocol desync are all things that went wrong with the machinery
    /// rather than answers about the market.
    ///
    /// [`End::Closed`] is deliberately NOT one of them: a socket that reaches EOF without a `Bye`
    /// is the far side hanging up, which is rude rather than broken, and a stream that delivered
    /// everything it was going to deliver has not failed.
    fn is_fault(&self) -> bool {
        match self {
            End::Silent | End::Desync(_) | End::Fault(_) => true,
            End::Events(_) | End::Elapsed | End::ReaderGone | End::Bye(_) | End::Closed => false,
        }
    }

    /// Did the stream end the way the operator asked it to?
    ///
    /// Under a bound, only that bound counts: an early `Bye` or a hung-up socket means they did not
    /// get the 30 seconds or the 500 frames they named. Under `--unbounded` there is no bound to
    /// miss, so every ENDING is the end — and a closed reader is always a success, because
    /// `| head -3` is a legitimate way to use a stream.
    ///
    /// ⚠ **A FAULT is never asked for, `--unbounded` included, and this returned `true` for one
    /// until now.** `Bound::Unbounded` folded all five non-bound ends together, so a transport
    /// failure and a protocol desync exited 0 — rendered, in the one channel a pipeline reads,
    /// identically to a clean stop. A wrapper running
    /// `vike-cli data realtime watch … --unbounded --out tape.jsonl` under `set -e` saw a success
    /// and took a half-written tape for a complete one. [`End::is_fault`] is the split, and it is
    /// consulted FIRST: what the bound decides is only whether a clean ENDING was the one asked
    /// for.
    fn was_asked_for(&self, bound: Bound) -> bool {
        if self.is_fault() {
            return false;
        }
        match self {
            End::Events(_) | End::Elapsed | End::ReaderGone => true,
            End::Bye(_) | End::Closed => bound == Bound::Unbounded,
            // Unreachable — `is_fault` answered above — and spelled out rather than left to a `_`
            // so a new variant has to classify itself in BOTH functions.
            End::Silent | End::Desync(_) | End::Fault(_) => false,
        }
    }

    /// The reason, in an operator's words.
    fn sentence(&self) -> String {
        match self {
            End::Events(n) => format!("the --events bound was reached ({n} data frames)"),
            End::Elapsed => "the --for bound elapsed".to_string(),
            End::Bye(why) => format!("the server ended the stream: {}", bye_sentence(*why)),
            End::Silent => "the link went silent past the server's own heartbeat deadline — that \
                            is a dead link, not a quiet market"
                .to_string(),
            End::Closed => "the server closed the socket without saying why".to_string(),
            End::Desync(what) => format!("protocol desync: {what}"),
            End::Fault(e) => format!("the transport failed: {e}"),
            End::ReaderGone => "the reader closed the pipe".to_string(),
        }
    }
}

/// What the stream carried, counted by CLASS — because "142 frames" answers nothing an operator
/// asked. A quiet key that heartbeated 4 times and a busy one that delivered 138 books are the same
/// number under one counter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Tally {
    /// DATA frames — depth, book, trades. The unit `--events` counts.
    events: usize,
    /// Stream-status disclosures.
    statuses: usize,
    /// Tape-gap markers.
    gaps: usize,
    /// Heartbeats.
    heartbeats: usize,
    /// Prints the server told us were LOST, summed across every marker.
    dropped: u64,
}

/// Read frames until the bound, a fault or a goodbye, rendering each one.
///
/// ⚠ **The read budget is `min(what is left of --for, the deadline the socket already carries)`, and
/// the second half is READ BACK OFF THE SOCKET rather than recomputed.**
/// `DatahubClient::md_subscribe` arms `max(MD_READ_TIMEOUT, 3 × heartbeat_ms)` — the whole reason the
/// server SENDS its heartbeat period — and a second copy of that expression here could disagree with
/// the deadline actually armed. Without the `--for` half a `--for 5s` would sit in a 45-second read
/// and return forty seconds late; without the deadline half a `--for 2h` would never notice a dead
/// link. Which of the two fired is then the difference between [`End::Elapsed`] and [`End::Silent`],
/// and it is answered by the clock rather than by the error.
fn stream_frames(
    mut stream: TcpStream,
    bound: Bound,
    render: Render,
    sink: &mut Sink,
) -> (End, Tally) {
    let started = Instant::now();
    let mut tally = Tally::default();
    // A socket with no deadline armed is not a shape `md_subscribe` produces; falling back to the
    // wire's own floor keeps this total rather than asserting about a value we did not set.
    let deadline = stream.read_timeout().ok().flatten().unwrap_or(MD_READ_TIMEOUT);
    let (max_events, limit) = match bound {
        Bound::First { events, duration } => (events, duration),
        Bound::Unbounded => (None, None),
    };

    let end = loop {
        if matches!(max_events, Some(n) if tally.events >= n) {
            break End::Events(tally.events);
        }
        let left = limit.map(|d| d.saturating_sub(started.elapsed()));
        if left == Some(Duration::ZERO) {
            break End::Elapsed;
        }
        // ⚠ Never zero: a zero read timeout is an `InvalidInput` error on both Unix and Windows
        // rather than a non-blocking read, so the floor is a millisecond and the bound is re-checked
        // at the top of the loop.
        let budget = left.map_or(deadline, |l| l.min(deadline).max(Duration::from_millis(1)));
        if let Err(e) = stream.set_read_timeout(Some(budget)) {
            break End::Fault(e.to_string());
        }
        match read_frame::<_, Response>(&mut stream) {
            Ok(Response::Md(frame)) => {
                let frame = *frame;
                count_frame(&mut tally, &frame);
                let text = match render {
                    Render::Table => table_line(&frame),
                    // `status`'s document form cannot reach this verb — `parse` refuses it — and the
                    // arm is spelled rather than left to a catch-all so a future third rendering
                    // has to answer for itself here.
                    Render::Jsonl | Render::Json => jsonl_row(&frame),
                };
                if let Err(e) = sink.line(&text) {
                    break match e.kind() {
                        io::ErrorKind::BrokenPipe => End::ReaderGone,
                        _ => End::Fault(e.to_string()),
                    };
                }
                if let MdFrame::Bye(why) = frame {
                    break End::Bye(why);
                }
            }
            Ok(other) => break End::Desync(format!("expected an Md frame, got {other:?}")),
            // ⚠ A read TIMEOUT arrives as `WouldBlock` on Unix and `TimedOut` on Windows — one
            // deadline, two errnos — and this is the one place in this verb that has to know it.
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {
                // Which deadline fired is a question about the CLOCK, not about the error: if the
                // `--for` bound has run out, this is the bound; otherwise the link went silent past
                // the period the server itself declared.
                let spent = limit.is_some_and(|d| started.elapsed() >= d);
                break if spent { End::Elapsed } else { End::Silent };
            }
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break End::Closed,
            Err(e) => break End::Fault(e.to_string()),
        }
    };
    (end, tally)
}

/// Count one frame by CLASS. Only the three DATA variants are events — see [`Tally`] and the
/// `--events` row of [`usage`]: a heartbeat is what the wire says ABOUT the stream, and counting it
/// would let a dead-quiet key satisfy `--events 500` by saying nothing 500 times.
fn count_frame(tally: &mut Tally, frame: &MdFrame) {
    match frame {
        MdFrame::Depth(_) | MdFrame::Book(_) | MdFrame::Trades { .. } => tally.events += 1,
        MdFrame::Status { .. } => tally.statuses += 1,
        MdFrame::TapeGap { dropped, .. } => {
            tally.gaps += 1;
            tally.dropped += dropped;
        }
        MdFrame::Heartbeat => tally.heartbeats += 1,
        MdFrame::Bye(_) => {}
    }
}

/// The closing report — stderr, always, under both renderings.
fn summary_lines(asked: &MdSpec, end: &End, tally: &Tally) -> Vec<String> {
    let mut lines = vec![
        format!(
            "stream ended — {}:{} {}: {}",
            asked.venue,
            asked.symbol,
            asked.lane.feed_stream_label(),
            end.sentence()
        ),
        format!(
            "frames: {} data, {} status, {} tape-gap, {} heartbeat",
            tally.events, tally.statuses, tally.gaps, tally.heartbeats
        ),
    ];
    if tally.dropped > 0 {
        lines.push(format!(
            "⚠ {} PRINTS WERE LOST inside this stream. Anything folded from it — CVD, delta, \
             footprint volume — is wrong by that much and cannot be repaired from what arrived",
            tally.dropped
        ));
    }
    if tally.events == 0 {
        lines.push(
            "no DATA frame arrived. On a quiet key that is the market and not a fault — the \
             heartbeat count above is what says the link was alive"
                .to_string(),
        );
    }
    lines
}

// ─── rendering one frame ─────────────────────────────────────────────────────────────────────────

/// One frame as a JSON object — `watch`'s machine form, one per line.
///
/// Every row carries `type`, including the heartbeat, so a consumer filters on a field rather than
/// on a shape. ⚠ The heartbeat IS rendered: a stream that emitted only data would be one a consumer
/// cannot tell from a stalled one, which is the exact thing the heartbeat exists to answer.
fn jsonl_row(frame: &MdFrame) -> String {
    let value = match frame {
        MdFrame::Depth(snap) => book_json("depth", snap),
        MdFrame::Book(snap) => book_json("book", snap),
        MdFrame::Trades { venue, symbol, ticks, seq } => {
            // ⚠ **The ticks are re-stamped from the ENVELOPE and never serialized raw.**
            // `MdFrame::Trades`' own doc: every tick in the batch carries an EMPTY `symbol`, and
            // this variant's `symbol` field is authoritative for all of them. A row built by
            // serializing `TradeTick` would publish `"symbol": ""` on every print — the mis-key that
            // doc warns about, wearing a JSON field an operator would group by.
            let prints: Vec<serde_json::Value> = ticks
                .iter()
                .map(|t| {
                    serde_json::json!({
                        "ts": t.ts,
                        "local_ts": t.local_ts,
                        "price": t.price,
                        "size": t.size,
                        // The model's OWN field name, carried raw rather than as a `side` word: a
                        // consumer re-derives the aggressor (see `aggressor`) instead of trusting
                        // this side's reading of a flag.
                        "is_buyer_maker": t.is_buyer_maker,
                    })
                })
                .collect();
            serde_json::json!({
                "type": "trades",
                "venue": venue,
                "symbol": symbol,
                "seq": seq,
                "count": prints.len(),
                "prints": prints,
            })
        }
        MdFrame::Status { venue, symbol, lane, status } => {
            let mut row = serde_json::json!({
                "type": "status",
                "venue": venue,
                "symbol": symbol,
                "lane": lane.feed_stream_label(),
            });
            // ⚠ FLATTENED rather than serialized through `WireStreamStatus`' own serde, and
            // deliberately: that enum rides the wire externally tagged (`{"Live":{…}}`), which is a
            // MIRROR type's private spelling and is hostile to `jq`. The state word is a stable
            // machine token — `crate::cmd::data::catalog`'s `ServerView::state` sets the precedent —
            // and every number the verdict rests on rides beside it.
            let (state, at, newest, now) = match status {
                WireStreamStatus::GapStart { at_ts_ms } => {
                    ("gap_start", Some(*at_ts_ms), None, None)
                }
                WireStreamStatus::Live { gap_started_ts_ms } => {
                    ("live", *gap_started_ts_ms, None, None)
                }
                WireStreamStatus::Stale { newest_data_ts_ms, now_ms } => {
                    ("stale", None, Some(*newest_data_ts_ms), Some(*now_ms))
                }
            };
            row["state"] = serde_json::json!(state);
            row["episode_ts"] = serde_json::json!(at);
            row["newest_data_ts"] = serde_json::json!(newest);
            row["judged_at_ts"] = serde_json::json!(now);
            row
        }
        MdFrame::TapeGap { venue, symbol, dropped, from_seq, to_seq } => serde_json::json!({
            "type": "tape_gap",
            "venue": venue,
            "symbol": symbol,
            // ⚠ AUTHORITATIVE for how much was lost; the range says FROM WHERE. Several holes merge
            // into one marker, so `dropped` can exceed what the range appears to cover —
            // `MdFrame::TapeGap`'s own doc carries the rule and this field order follows it.
            "dropped": dropped,
            "from_seq": from_seq,
            "to_seq": to_seq,
        }),
        MdFrame::Heartbeat => serde_json::json!({ "type": "heartbeat" }),
        MdFrame::Bye(why) => {
            let mut row = serde_json::json!({ "type": "bye", "reason": bye_token(*why) });
            if let MdBye::TooSlow { lapses } = why {
                row["lapses"] = serde_json::json!(lapses);
            }
            row
        }
    };
    serde_json::to_string(&value)
        .expect("a tree of strings, numbers and bools; serialization is total")
}

/// A book/depth frame as a row.
///
/// The levels ride as the MODEL's own shape — `vike_model::BookLevel` serializes as a two-element
/// `[price, qty]` array through its own `#[serde(into)]`, which is what the journal has always
/// written — rather than as a shape invented here.
///
/// ⚠ **Best-first is a CONTRACT, not an accident**: `BookSnapshot`'s doc states bids DESCEND and asks
/// ASCEND, and nothing here re-sorts. A renderer that "tidied" the order would paint every DOM
/// ladder upside down, which no round-trip test would catch.
fn book_json(kind: &str, snap: &BookSnapshot) -> serde_json::Value {
    serde_json::json!({
        "type": kind,
        "venue": snap.venue,
        "symbol": snap.symbol,
        "tick_size": snap.tick_size,
        "bids": snap.bids,
        "asks": snap.asks,
        // The WIRE sequence — strictly +1 per frame for this key, so a jump means this connection
        // did not receive frames the server produced.
        "seq": snap.seq,
        // ⚠ DIAGNOSTIC ONLY, both of them: the publisher conflates on a cadence, so consecutive
        // frames legitimately skip venue sequence numbers, and the stamp is a different clock on
        // each lane (the venue's on `depth`, the hub's receipt on `book`).
        "venue_seq": snap.venue_seq,
        "venue_ts": snap.venue_ts,
    })
}

/// One frame as a line for a person.
///
/// ⚠ A book frame is SUMMARISED here (top of book plus the level counts) while [`jsonl_row`] carries
/// every level. That is the split the two renderings are for: 200 levels a side is the DATA and it
/// is unreadable as a terminal line, so the machine form keeps it and the human form keeps what a
/// person watching a ladder actually reads.
fn table_line(frame: &MdFrame) -> String {
    match frame {
        MdFrame::Depth(snap) => book_line("depth", snap),
        MdFrame::Book(snap) => book_line("book", snap),
        MdFrame::Trades { venue, symbol, ticks, seq } => {
            let prints: Vec<String> = ticks
                .iter()
                // The tick's own `symbol` is EMPTY on this wire — see `jsonl_row` — so nothing here
                // reads it; the envelope above is the authority.
                .map(|t| {
                    format!(
                        "{} {} x {} @{}",
                        aggressor(t.is_buyer_maker),
                        t.price,
                        t.size,
                        epoch_ms_to_utc_timestamp(t.ts)
                    )
                })
                .collect();
            format!(
                "trades {venue} {symbol} seq={seq} ({} print(s)) {}",
                prints.len(),
                prints.join(" | ")
            )
        }
        MdFrame::Status { venue, symbol, lane, status } => format!(
            "status {venue} {symbol} {}: {}",
            lane.feed_stream_label(),
            status_sentence(status)
        ),
        MdFrame::TapeGap { venue, symbol, dropped, from_seq, to_seq } => format!(
            "⚠ TAPE GAP {venue} {symbol}: {dropped} print(s) LOST between seq {from_seq} and \
             {to_seq}"
        ),
        MdFrame::Heartbeat => "heartbeat — the link is alive and the key is quiet".to_string(),
        MdFrame::Bye(why) => format!("bye — {}", bye_sentence(*why)),
    }
}

/// A book/depth frame as a line: the top of book, then how deep the frame actually was.
fn book_line(kind: &str, snap: &BookSnapshot) -> String {
    let side = |levels: &[vike_model::BookLevel]| match levels.first() {
        Some(l) => format!("{} x {}", l.price, l.qty),
        None => "-".to_string(),
    };
    format!(
        "{kind} {} {} seq={} bid {} | ask {} ({}x{} levels)",
        snap.venue,
        snap.symbol,
        snap.seq,
        side(&snap.bids),
        side(&snap.asks),
        snap.bids.len(),
        snap.asks.len()
    )
}

/// Which side CROSSED the spread, derived from the model's own flag.
///
/// `TradeTick::is_buyer_maker` says the BUYER was resting, so the taker was the SELLER. The word is
/// derived at ONE site rather than at each render, because inverting it is a silent error: a
/// footprint reading `buy` for every sell prints a chart that is precisely wrong and never empty.
/// [`jsonl_row`] deliberately carries the raw flag instead, so a machine reader re-derives this
/// rather than trusting it.
fn aggressor(is_buyer_maker: bool) -> &'static str {
    if is_buyer_maker { "sell" } else { "buy" }
}

/// A stream-status disclosure in an operator's words. Exhaustive, no `_` arm: a new
/// [`WireStreamStatus`] variant must be given a sentence rather than inheriting one.
fn status_sentence(status: &WireStreamStatus) -> String {
    match status {
        WireStreamStatus::GapStart { at_ts_ms } => format!(
            "the feed can no longer be trusted from {} — a transport fault, a server close or an \
             idle watchdog",
            epoch_ms_to_utc_timestamp(*at_ts_ms)
        ),
        WireStreamStatus::Live { gap_started_ts_ms: Some(from) } => format!(
            "live again, closing the gap that opened at {}",
            epoch_ms_to_utc_timestamp(*from)
        ),
        WireStreamStatus::Live { gap_started_ts_ms: None } => "live".to_string(),
        WireStreamStatus::Stale { newest_data_ts_ms, now_ms } => format!(
            "STALE — the transport is alive and the newest data is from {}, judged at {}. This is \
             the silently-failed re-subscribe a socket watchdog cannot see",
            epoch_ms_to_utc_timestamp(*newest_data_ts_ms),
            epoch_ms_to_utc_timestamp(*now_ms)
        ),
    }
}

/// A goodbye as a stable machine token. Exhaustive, no `_` arm.
fn bye_token(why: MdBye) -> &'static str {
    match why {
        MdBye::TooSlow { .. } => "too_slow",
        MdBye::ServerStopping => "server_stopping",
        MdBye::SessionIdle => "session_idle",
        MdBye::ControlLaneOverflow => "control_lane_overflow",
    }
}

/// A goodbye in an operator's words. ⚠ `too_slow` and `session_idle` are OPPOSITE diagnoses — asking
/// for more than you could read, versus asking for nothing — and [`MdBye`]'s own doc records what
/// sending the wrong one cost, so each keeps its own sentence.
fn bye_sentence(why: MdBye) -> String {
    match why {
        MdBye::TooSlow { lapses } => format!(
            "this reader could not keep up ({lapses} lapses). The server stops serving a client it \
             would otherwise be lying to more slowly — read faster, narrow the lane, or take \
             `--format jsonl` into a file rather than a terminal"
        ),
        MdBye::ServerStopping => "the datahub is shutting down".to_string(),
        MdBye::SessionIdle => {
            "the session held no subscriptions for long enough that the socket bought nothing"
                .to_string()
        }
        MdBye::ControlLaneOverflow => {
            "the reserved CONTROL lane overflowed — this reader could not absorb even the status \
             frames its own keys produced, which is a slow consumer rather than an idle one"
                .to_string()
        }
    }
}

// ─── where the frames go ─────────────────────────────────────────────────────────────────────────

/// Where a rendered frame is written.
///
/// ⚠ **The file is flushed line by line**, and that is a property of a STREAM rather than a
/// preference: a watch is a thing an operator interrupts, and a buffered tail lost on ^C is data the
/// wire will never send again. The cost is one `write` syscall per frame, against a lane whose
/// publish cadence is measured in tens per second.
enum Sink {
    /// stdout, LOCKED ONCE for the stream's whole life.
    ///
    /// ⚠ Not `println!`: that PANICS on a broken pipe, and `| head -3` closes one as a matter of
    /// routine — see [`End::ReaderGone`].
    ///
    /// ⚠ **The doc said "locked once" while [`Sink::line`] took the lock per frame**, which is the
    /// cheaper half of the two claims and the one that was false. Holding it is safe HERE and would
    /// not be everywhere: nothing else in this verb writes stdout at all — every note, disclosure
    /// and summary goes to stderr, by the rule in this module's own doc — so there is no second
    /// writer to deadlock against.
    Stdout(io::StdoutLock<'static>),
    File {
        path: String,
        out: BufWriter<File>,
    },
}

impl Sink {
    /// Open the destination — AFTER the subscription was accepted, deliberately.
    ///
    /// ⚠ `File::create` TRUNCATES, so opening it earlier would destroy a previous capture on a run
    /// that then failed to connect or was refused a spec. The cost of this order is that an
    /// unwritable path is discovered one round trip in rather than at the door, and that is the
    /// cheaper of the two mistakes: a refused subscription costs a connection, and a truncated
    /// capture costs a tape the wire will never send again.
    fn open(path: Option<&str>) -> CmdResult<Self> {
        match path {
            None => Ok(Sink::Stdout(io::stdout().lock())),
            Some(p) => {
                let file = File::create(p).map_err(|e| {
                    CliError::failed(format!("--out {p:?} could not be opened for writing: {e}"))
                })?;
                Ok(Sink::File { path: p.to_string(), out: BufWriter::new(file) })
            }
        }
    }

    /// Write one rendered frame. The `io::Error` is returned rather than classified, because only
    /// the caller knows that a broken pipe is a normal end here.
    fn line(&mut self, text: &str) -> io::Result<()> {
        match self {
            Sink::Stdout(out) => {
                writeln!(out, "{text}")?;
                out.flush()
            }
            Sink::File { out, .. } => {
                writeln!(out, "{text}")?;
                out.flush()
            }
        }
    }

    /// Close the destination. A failure HERE is worth a rung of its own: the frames were rendered
    /// and the file may be short, which a caller reading only the exit code would otherwise take for
    /// a complete capture.
    fn finish(&mut self) -> CmdResult<()> {
        match self {
            Sink::Stdout(_) => Ok(()),
            Sink::File { path, out } => out
                .flush()
                .map_err(|e| CliError::failed(format!("--out {path:?} could not be flushed: {e}"))),
        }
    }
}

// ─── `status` ────────────────────────────────────────────────────────────────────────────────────

/// The sentence EVERY `status` answer ends on, and the reason this verb is honest.
///
/// ⚠ It is unconditional rather than reserved for the empty case, which is the stronger claim: a
/// reader who met it only when the list was short would reasonably read its absence as "these ones
/// were checked". None of them were.
const NOT_A_PROBE: &str = "⚠ this is what the server ADVERTISES, not a liveness check. An entry \
                           means this datahub's build links a market-data client for that venue and \
                           would accept a subscription for it — nothing here opened a venue socket, \
                           timed a frame, or observed one. The verb that observes a frame is \
                           `data realtime watch`.";

/// `data realtime status` — the handshake, read back.
fn execute_status(args: &Args, keys: Option<&NodeKeys>) -> CmdResult<()> {
    // ⚠ Through the SHARED dial, unlike `catalog venues` — see the module doc: this verb has no
    // local half, so an unreachable datahub leaves nothing to render and is the connect rung.
    let client = connect(&args.addr, keys, Scope::Read)?;
    let features: Vec<String> = client.features().to_vec();
    let view = ServerFeeds::of(&features);
    let render =
        args.render.unwrap_or_else(|| default_render(args.verb, false, io::stdout().is_terminal()));
    let text = match render {
        // `watch`'s stream form cannot reach this verb — `parse` refuses it — and the arm is spelled
        // rather than left to a catch-all so a third rendering has to answer for itself here.
        Render::Json | Render::Jsonl => status_json(&args.addr, &view),
        Render::Table => status_lines(&args.addr, &view).join("\n"),
    };
    println!("{text}");
    Ok(())
}

/// What one handshake said about the market-data plane.
///
/// Two INDEPENDENT facts, kept apart because their absences mean different things: a server with no
/// plane mounted advertises neither the capability nor a venue, while a server with the plane and an
/// empty venue set advertises the first and not the second — and the second is a build that links no
/// venue feed, which is a different thing to fix.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ServerFeeds {
    /// Did the `Welcome` carry [`FEATURE_MARKET_DATA`]?
    plane: bool,
    /// The venues it advertised, in ADVERTISEMENT ORDER — [`advertised_md_venues`]' own contract,
    /// preserved rather than sorted: the order is the server's statement about itself.
    venues: Vec<String>,
}

impl ServerFeeds {
    /// Read one handshake. PURE over the feature list, so every rendering below is unit-tested
    /// against planted advertisements rather than against a server.
    fn of(features: &[String]) -> Self {
        ServerFeeds {
            plane: features.iter().any(|f| f == FEATURE_MARKET_DATA),
            venues: advertised_md_venues(features),
        }
    }
}

/// The notes every answer carries, in the order both renderings emit them.
fn status_notes(view: &ServerFeeds) -> Vec<String> {
    let mut notes = Vec::new();
    if !view.plane {
        notes.push(format!(
            "this server advertises no `{FEATURE_MARKET_DATA}` capability at all, so \
             `data realtime watch` is refused before anything is sent — with the server's own \
             sentence, which names what has to be set THERE. Whether the plane is mounted is a \
             runtime fact of that process, not of this binary"
        ));
    } else if view.venues.is_empty() {
        notes.push(
            "the market-data plane is mounted and advertises NO venue: the server would accept the \
             verb and refuse every spec, because its build links no venue feed. That is a build and \
             configuration question on the server, not a spelling one here"
                .to_string(),
        );
    } else {
        notes.push(
            "a venue that is NOT listed is refused per spec (`VenueNotServed`) before any venue is \
             called, so `data realtime watch` on one costs a round trip and nothing else"
                .to_string(),
        );
    }
    notes.push(NOT_A_PROBE.to_string());
    notes
}

/// The table.
fn status_lines(addr: &str, view: &ServerFeeds) -> Vec<String> {
    let mut lines = vec![
        format!("datahub:     {addr}"),
        format!("market data: {}", if view.plane { "advertised" } else { "NOT advertised" }),
        String::new(),
    ];
    if view.venues.is_empty() {
        lines.push("no venue advertises a live feed on this datahub".to_string());
    } else {
        let venue_w = col("VENUE", view.venues.iter().map(String::len));
        lines.push(format!("{:<venue_w$}  LIVE FEED", "VENUE"));
        for v in &view.venues {
            lines.push(format!("{v:<venue_w$}  advertised"));
        }
    }
    for note in status_notes(view) {
        lines.push(String::new());
        lines.push(note);
    }
    lines
}

/// The document.
///
/// ⚠ `liveness_probed` is a hard `false` rather than an omission, the shape
/// `crate::cmd::data::source`'s `verified_against_the_vendor` already uses: a consumer folding this
/// has no prose to read, so the ONE thing it must not be able to assume is that anything was
/// measured.
fn status_json(addr: &str, view: &ServerFeeds) -> String {
    let doc = serde_json::json!({
        "addr": addr,
        "market_data_advertised": view.plane,
        "venues": view.venues,
        "count": view.venues.len(),
        "liveness_probed": false,
        "notes": status_notes(view),
    });
    serde_json::to_string_pretty(&doc)
        .expect("a tree of strings, numbers and bools; serialization is total")
}

#[path = "realtime_tests.rs"]
#[cfg(test)]
mod realtime_tests;
