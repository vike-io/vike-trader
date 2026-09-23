//! `vike-cli data realtime record` — **what this BOX persists, written as rows.**
//!
//! `watch` streams to THIS terminal and persists nothing. This group edits the SUBSCRIPTION SET a
//! recording datahub mounts: one row per `venue` + (`family` | `symbols`) + `backfill`, stored in
//! `<project>/settings/db/vike.db` and rendered back into the TOML document the mount already knew
//! how to read (`vike_secrets::profile_store::render_recorder_toml`).
//!
//! # The rulings this group is shaped by
//!
//! `docs/superpowers/specs/2026-09-22-data-realtime-record-design.md` carries the measurement
//! behind each; the ones that show up as CODE here:
//!
//! 1. **There is no `--lane`.** The noun is a SUBSCRIPTION, not a stream: `vike-datahub`'s recorder
//!    builds its runtime with a literal `Stream::ALL.to_vec()`, so the stream set is chosen once
//!    for the whole PROCESS and there is no per-stream selection anywhere in this tree to expose.
//!    ⚠ The reserved word is [`RESERVED_GRAIN_FLAG`] — `--lane` is NOT available, because it
//!    already means something else one module up: `crate::cmd::data::realtime`'s `LANES` serves
//!    `depth | book | trades` and refuses `quotes` BY NAME, while the recorder WRITES
//!    `vike_recorder::session::Stream::Quotes`. One word, one group, opposite answers.
//! 2. **A row is live at the daemon's NEXT RESTART**, never within a tick — [`RESTART_NOTE`], said
//!    on every write. `add` and `rm` behave IDENTICALLY here: the asymmetry the code offers for
//!    free (`vike_recorder::runtime`'s `RecorderRuntime` has `add_feed` and no removal) is
//!    REFUSED, because an operator would otherwise see `record ls` come back empty while the tape
//!    kept growing.
//! 3. **`--addr` is accepted and REFUSED by name** — [`remote_refusal`]. The remote half needs wire
//!    verbs `vike_datahub_client::proto::Request` has no arm of, and
//!    `docs/decisions/0081-a-recorded-subscription-is-a-write-verb.md` classifies them Write.
//! 4. **The DEFAULT profile is the `active` row of kind Recorder** — [`resolve_target`], through
//!    `vike_secrets::profile_store::Profiles::resolve_active`, which is the ONE resolution the
//!    daemon's own read shares. Zero recorder profiles is an ERROR naming
//!    `vike-cli config mirror --recorder`, never a silent create.
//! 5. **`rm` takes a SPEC and refuses an ambiguous match by printing the candidates with their
//!    `ord`** — [`remove_one`]. The schema's unique index is PARTIAL
//!    (`subscription_one_family_per_venue … WHERE family IS NOT NULL`), so two symbols-based rows
//!    on one venue are legal and a SPEC is not an identity.
//! 6. **On a box whose daemon reads a FILE, `record add` writes the row and WARNS** —
//!    [`FILE_DAEMON_NOTE`]. Nothing in this CLI reads the daemon's unit, so the warning is
//!    unconditional prose rather than a detected fact, and it is said on `rm` too: the sentence is
//!    symmetric even though the ruling names `add`.
//! 7. **An unrecordable venue is refused BY NAME when the box can say so** — [`probe_venue`]. See
//!    below; it is `docs/decisions/0013-degrade-vs-refuse.md` rather than a new policy.
//!
//! # ⚠ THE WRITE PATH GOES THROUGH ROWS, NEVER THROUGH THE RENDERED TOML
//!
//! `vike_secrets::profile_store::render_recorder_toml` **never emits the per-subscription `note`
//! column** — `RecorderRow`'s own doc calls that "the largest unpriced loss in the move" — so a
//! read-modify-write that went out through the rendered document would silently drop every note on
//! the profile, including the ones `--note` writes. So [`plan_write`] mutates the `StoredProfile`
//! this module READ and hands that same value back to `store_profile`, which writes `note` as a
//! column. The rendering is used as a PARSE CHECK and for nothing else ([`parse_check`]).
//!
//! The whole `StoredProfile` is carried rather than a rebuilt one, for the same reason: its
//! `mounts`, `params`, `settings` and profile-level `note` are re-inserted verbatim, and
//! `store_profile` preserves the `active` bit itself.
//!
//! ⚠ **`store_profile` is a whole-body `DELETE FROM profile` + re-INSERT in an IMMEDIATE
//! transaction, against a read this module took on a SEPARATE connection.** So `add`/`rm` are
//! read-all → mutate → write-all, LAST-WRITER-WINS against a concurrent
//! `vike-cli config mirror --recorder`. That is inherited rather than introduced, and it is why
//! these verbs report the whole resulting subscription list rather than only the row they touched.
//!
//! # ⚠ The venue check is BEST-EFFORT and degrades, and here is exactly when
//!
//! The shipped binary records only what `vike_recorder::venues::supported` lists, while the LIVE
//! market-data plane serves six venues — so `record add okx:…` would write a legal row that takes
//! the data daemon down at its next restart under `Restart=on-failure`. The datahub advertises a
//! recordable venue as [`REC_VENUE_PREFIX`]`<slug>` in its handshake features, the twin of the
//! `md_venue=` entries `vike_datahub_client::proto::md_venue_feature` builds. [`probe_venue`]
//! answers one of three ways, and only ONE of them refuses:
//!
//! | what the probe saw | verdict |
//! |---|---|
//! | the datahub answered and advertises this venue | write |
//! | the datahub answered, advertises SOME venues, and not this one | **REFUSE by name** |
//! | unreachable, or it advertises NO recordable venue at all | WARN and write |
//!
//! The third row is what makes this forward-compatible with a datahub that predates the
//! advertisement: an empty advertisement advertises nothing, so it is read as "this server cannot
//! say" rather than as "this server records nothing" — the same absence-is-the-answer rule
//! `vike_datahub_client::proto::advertised_md_venues` applies to its own values.
//!
//! ⚠ **[`REC_VENUE_PREFIX`] is spelled HERE and must not stay that way.** The advertisement's
//! WRITE half lands in `crates/vike-datahub/src/recorder.rs` and its constant belongs in
//! `crates/vike-datahub-client/src/proto.rs` beside `FEATURE_MD_VENUE_PREFIX`; when that constant
//! exists this one is DELETED and imported, because a rule this workspace states in as many words
//! is that a symbol has one name. It is spelled here rather than blocked on that work so the
//! client half can ship and be tested; the cost is one spelling that must be collapsed, and the
//! third row of the table above is what keeps the two from disagreeing dangerously meanwhile.

use std::path::Path;
use std::process::ExitCode;

use vike_datahub_client::{NodeKeys, Scope};
use vike_secrets::profile_store::{
    ActiveProfile, OperatorWrite, ProfileKind, Profiles, RecorderBody, StoredProfile,
    SubscriptionRow, read_profiles, render_recorder_toml, store_profile, toml_string_array,
};

use super::{DEFAULT_ADDR, col, connect};
use crate::cmd::args::{Flags, exit_for_parse_error, help_requested, no_value};
use crate::exit::{CliError, CmdResult};

/// What [`exit_for_parse_error`] and every failure line name this command. The whole four-token
/// path, for `crate::cmd::data::realtime`'s reason: a shorter name sends a reader to a usage page
/// that does not contain the flag they got wrong.
const COMMAND: &str = "data realtime record";

/// The prefix a datahub advertises one RECORDABLE venue with — see this module's doc for why it is
/// spelled here and what deletes it.
const REC_VENUE_PREFIX: &str = "rec_venue=";

/// The flag RESERVED if per-stream grain is ever wanted. A constant because a refusal has to spell
/// it, and because `--lane` — the word an operator will reach for — is the one that is NOT
/// available here.
const RESERVED_GRAIN_FLAG: &str = "--stream";

/// **The disclosure every write carries.** A row is a fact about the next mount, not about this
/// minute, and an operator watching `ls` show a row while the tape shows nothing is the failure
/// this sentence exists to prevent.
const RESTART_NOTE: &str = "⚠ this takes effect at the recording daemon's NEXT RESTART, not now: \
                            the recorder re-resolves a subscription's SYMBOLS every tick and does \
                            not re-read the PROFILE.";

/// **The warning ruling 6 requires, said unconditionally because nothing here can detect it.**
const FILE_DAEMON_NOTE: &str = "⚠ if this box's datahub unit names `--record <path>` rather than \
                                the profile flag, it reads that TOML FILE and will not read this \
                                row at all. `vike-cli config recorder` prints what is stored; the \
                                unit's own ExecStart is what decides which of the two is read.";

// ─── the vocabulary ──────────────────────────────────────────────────────────────────────────────

/// Which verb ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verb {
    /// `ls` — the stored subscriptions, or (with `--profiles`) the profiles to choose among.
    Ls,
    /// `add SPEC` — one more subscription row.
    Add,
    /// `rm SPEC` — one fewer.
    Rm,
}

/// Every verb, in the order [`usage`] lists them — and the roster the refusals RENDER rather than
/// restate, `crate::cmd::data::realtime`'s `VERBS` rule one rung down.
const VERBS: &[Verb] = &[Verb::Ls, Verb::Add, Verb::Rm];

impl Verb {
    /// The name the operator typed, which is also what every refusal names it by.
    fn as_str(self) -> &'static str {
        match self {
            Verb::Ls => "ls",
            Verb::Add => "add",
            Verb::Rm => "rm",
        }
    }

    /// Does this verb WRITE? Two separate refusals key on it, so it is answered once rather than by
    /// two matches that can drift.
    fn writes(self) -> bool {
        match self {
            Verb::Ls => false,
            Verb::Add | Verb::Rm => true,
        }
    }
}

/// The roster as a refusal renders it — one spelling, used by both the missing-verb and the
/// unknown-verb messages.
fn verb_roster() -> String {
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

/// What a subscription names: a whole market FAMILY recorded as one grouped series, or ONE symbol.
///
/// ⚠ The two are the `family` and `symbols` columns and the schema keeps them apart: only `family`
/// carries the partial unique index, which is exactly why [`match_rows`] cannot treat a SPEC as an
/// identity on the symbol side.
#[derive(Debug, Clone, PartialEq, Eq)]
enum What {
    /// `VENUE:@FAMILY` — `subscription.family`.
    Family(String),
    /// `VENUE:SYMBOL` — one element of `subscription.symbols`.
    Symbol(String),
}

/// The `VENUE:SYMBOL` / `VENUE:@FAMILY` positional, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Spec {
    venue: String,
    what: What,
}

impl Spec {
    /// The spec as an operator typed it — what every refusal echoes.
    fn render(&self) -> String {
        match &self.what {
            What::Family(f) => format!("{}:@{f}", self.venue),
            What::Symbol(s) => format!("{}:{s}", self.venue),
        }
    }
}

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
fn parse_spec(raw: &str) -> Result<Spec, String> {
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
/// handed to `vike_recorder::venues::build_feed` verbatim, so anything that could be a real
/// spelling is admitted. What is refused is the two shapes that are always a typo here: a token
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

/// HOW `ls` renders its answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Render {
    /// Aligned columns and the disclosures, for a person at a terminal.
    Table,
    /// ONE JSON document — the machine form, and what `--json` has always meant.
    Json,
}

/// The parsed `data realtime record …` line. PURE — the whole grammar is unit-tested below.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Args {
    verb: Verb,
    /// The positional. Always `Some` on `add`/`rm`, always `None` on `ls`.
    spec: Option<Spec>,
    /// `--profile NAME`. `None` means the ACTIVE recorder profile — ruling 4.
    profile: Option<String>,
    /// `--profiles`: list the profiles rather than one profile's rows. `ls` only.
    profiles: bool,
    /// `--backfill`. `add` only; `None` files no key.
    backfill: Option<String>,
    /// `--note TEXT`. `add` only.
    note: Option<String>,
    /// `--ord N`, the `rm` disambiguator — ruling 5.
    ord: Option<i64>,
    /// `--dry-run`: print the plan and write nothing.
    dry_run: bool,
    /// `None` = `table` on `ls`; the write verbs render no document at all.
    render: Option<Render>,
    /// The datahub the venue check dials. Resolved from the configured rung or the default — NEVER
    /// from `--addr`, which this group refuses (ruling 3).
    addr: String,
}

/// Parse this group's argv tail (everything after `record`). PURE — no I/O, no socket, no store.
///
/// ⚠ Every flag is accepted by the ONE loop below and refused PER VERB afterwards, which is the
/// discipline `crate::cmd::data` and `crate::cmd::data::realtime` both follow: an inapplicable flag
/// is named in a message saying which verb it DOES belong to, where an unknown-option error would
/// tell an operator the flag does not exist — which is false.
fn parse(argv: &[String], configured_addr: Option<&str>) -> Result<Args, String> {
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

// ─── the usage page ──────────────────────────────────────────────────────────────────────────────

/// This group's usage page.
///
/// A FUNCTION rather than a `const` because the verb roster and the reserved word are DECLARATIONS
/// above, and a copy typed here is exactly the shape this repository has watched rot.
fn usage() -> String {
    const PAGE: &str = "\
usage: vike-cli data realtime record <verb> [options]

WHAT THIS BOX PERSISTS. `data realtime watch` streams to THIS terminal and keeps nothing;
these verbs edit the SUBSCRIPTION ROWS a recording datahub mounts, in this project's
settings database. A subscription is a VENUE plus a FAMILY or a SYMBOL plus a BACKFILL.

  ls           the stored subscriptions — ord, venue, what, backfill, note — leading with
               a `source:` line naming the store that answered. --profiles lists the
               recorder profiles instead, which is how you see what --profile may name
  add SPEC     write one subscription row. SPEC is VENUE:SYMBOL or VENUE:@FAMILY, where
               `@` marks a whole market FAMILY recorded as ONE grouped series
  rm SPEC      remove one. A SPEC is not an identity — two symbols-based rows on one venue
               are legal — so an ambiguous match is REFUSED with every candidate's `ord`
               printed, and --ord N is how you pick

options:
  --profile N  which recorder profile to read or edit. DEFAULT: the profile marked
               `active`, which is the same row the daemon resolves, so the two cannot
               disagree. Zero recorder profiles is an ERROR naming the verb that creates
               one — this verb never creates a profile
  --profiles   ls: list the recorder profiles (name, active, store, how many rows) rather
               than one profile's subscriptions
  --backfill B add: `venue` | `archive` | `off`. OMITTING it files NO key, which is not the
               same as `off` — the row stores the difference, and the daemon applies its
               own default to an absent one
  --note TEXT  add: the operator's own note for this subscription. It is a COLUMN, so it
               survives; the TOML rendering does not carry it and never did
  --ord N      rm: pick one of several candidates by its `ord`, as printed by `ls`
  --dry-run    add/rm: print the plan and write NOTHING
  --format F   ls: `table` or `json`
  --json       ls: shorthand for --format json
  -h, --help   this message

⚠ A ROW IS LIVE AT THE DAEMON'S NEXT RESTART, not now — and `add` and `rm` are identical
  in this. The recorder re-resolves a subscription's SYMBOLS every tick and does not
  re-read the PROFILE.
⚠ There is no --lane and no --stream: the recorder picks its stream set ONCE for the whole
  process, so there is no per-stream grain here to select. {reserved} is the reserved word
  if there ever is one.
⚠ --addr is accepted and REFUSED by name: editing another box's rows needs wire verbs that
  do not exist yet. Run this on the box whose daemon records.

verbs: {verbs}";
    PAGE.replace("{verbs}", &verb_roster()).replace("{reserved}", RESERVED_GRAIN_FLAG)
}

// ─── running ─────────────────────────────────────────────────────────────────────────────────────

/// Run a `data realtime record …` line. `argv` is everything AFTER the `record` word.
///
/// ⚠ `settings_dir` is a PARAMETER and had to become one: `crate::cmd::data::run` received
/// `project_root` and no settings notion, and `crate::run` derives that root as
/// `booted.settings_dir.parent()` — so under a `VIKE_SETTINGS_DIR` override not ending in
/// `settings`, a `project_root.join("settings")` here would name a DIFFERENT directory than the one
/// the boot loaded credentials and settings from. A `src/cmd/` file may not resolve it for itself.
pub(super) fn run(
    argv: &[String],
    settings_dir: Option<&Path>,
    keys: Option<&NodeKeys>,
    configured_addr: Option<&str>,
) -> ExitCode {
    let args = match parse(argv, configured_addr) {
        Ok(a) => a,
        Err(msg) => return exit_for_parse_error(COMMAND, &usage(), &msg),
    };
    match execute(&args, settings_dir, keys) {
        Ok(report) => {
            print!("{report}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("vike-cli {COMMAND}: {}", e.msg);
            e.exit.into()
        }
    }
}

/// Route the parsed line. Every arm produces its whole answer as TEXT and the caller prints it —
/// there is no stream here and no rows, so there is nothing to keep stdout clean FOR, and splitting
/// the warnings onto stderr would hide them from the `| tee` transcript an operator keeps of a
/// write.
fn execute(args: &Args, settings_dir: Option<&Path>, keys: Option<&NodeKeys>) -> CmdResult<String> {
    let dir = settings_dir.ok_or_else(|| {
        CliError::failed(
            "no settings directory resolved, so there is no store to read. Set VIKE_SETTINGS_DIR, \
             or run from a project that has one — `vike-cli secrets path` prints where this box \
             looks",
        )
    })?;
    let db = vike_secrets::db_path_in(dir);
    let profiles = read_profiles(&db).map_err(|e| CliError::failed(e.to_string()))?;
    let source = db.display().to_string();
    if args.verb == Verb::Ls {
        if args.profiles {
            return Ok(render_profiles(&source, &profiles, args.render.unwrap_or(Render::Table)));
        }
        // ⚠ `resolve_target` is called for its REFUSALS as much as for its answer: a read that
        // printed an empty list where the write verbs refuse would be the one place in this group
        // where "no recorder profile" looked like "no subscriptions".
        let target = resolve_target(&db, &profiles, args.profile.as_deref())?;
        return Ok(render_subscriptions(&source, target, args.render.unwrap_or(Render::Table)));
    }

    let spec = args.spec.as_ref().expect("parse guarantees a spec on `add`/`rm`");
    let target = resolve_target(&db, &profiles, args.profile.as_deref())?;
    let plan = plan_write(args, spec, target)?;
    // ⚠ The venue check runs ONCE, HERE — before anything is written and after the edit has been
    // shown to make sense. Running it inside the write arm would dial a datahub for an edit the
    // store was going to refuse anyway; running it twice would open two connections to answer one
    // question.
    let mut warning = None;
    if args.verb == Verb::Add {
        match probe_venue(&args.addr, keys, &spec.venue) {
            VenueVerdict::Recordable => {}
            VenueVerdict::NotRecordable(advertised) => {
                return Err(CliError::failed(unrecordable_refusal(&spec.venue, &advertised)));
            }
            VenueVerdict::CannotSay(why) => warning = Some(format!("⚠ {why}.\n")),
        }
    }
    let mut out = plan.report;
    if args.dry_run {
        out.push_str(&format!("\n--dry-run: NOTHING was written.\n{RESTART_NOTE}\n"));
        if let Some(w) = &warning {
            out.push_str(w);
        }
        return Ok(out);
    }
    let write = OperatorWrite::claim("vike-cli data realtime record");
    store_profile(&db, &plan.stored, &write, now_utc(), vike_model::AssetClass::SQL_WORDS)
        .map_err(|e| CliError::failed(e.to_string()))?;
    out.push_str(&format!("\nwritten to {source}\n{RESTART_NOTE}\n{FILE_DAEMON_NOTE}\n"));
    if let Some(w) = &warning {
        out.push_str(w);
    }
    Ok(out)
}

/// Seconds since the Unix epoch, for the row's `updated_utc` stamp. A wall clock is the right one —
/// the column answers "when did an operator last write this", not an interval. `vike-cli` is not
/// one of `crates/vike-ops/tests/clock_pin.rs`'s determinism-critical crates and already makes
/// several such reads, so this adds no row there.
fn now_utc() -> i64 {
    vike_model::clock::now_ms() / 1_000
}

// ─── resolving the profile — ruling 4 ────────────────────────────────────────────────────────────

/// **Which recorder profile this line acts on**, and, when there is none, WHICH no it is.
///
/// ⚠ The three noes name three different next commands, which is why
/// `vike_secrets::profile_store::Profiles::resolve_active` returns a reason rather than an
/// `Option` — and why this function does not collapse them. It splits the first of them FURTHER,
/// because `read_profiles` cannot: "no database" and "a database written before the profile tables
/// existed" both arrive as `NoProfileStore`, and only a filesystem probe can tell them apart. They
/// need different commands, so the probe is worth its line.
fn resolve_target<'a>(
    db: &Path,
    profiles: &'a Profiles,
    named: Option<&str>,
) -> CmdResult<&'a StoredProfile> {
    if let Some(name) = named {
        let Some(found) = profiles.by_name(name) else {
            return Err(CliError::failed(format!(
                "no profile named `{name}` in {}. {}",
                db.display(),
                stored_recorder_names(profiles)
            )));
        };
        if found.row.kind != ProfileKind::Recorder {
            return Err(CliError::failed(format!(
                "profile `{name}` is a `{}` profile rather than a recorder one, so it has no \
                 subscription rows to edit. {}",
                found.row.kind.sql_word(),
                stored_recorder_names(profiles)
            )));
        }
        return Ok(found);
    }
    match profiles.resolve_active(ProfileKind::Recorder) {
        ActiveProfile::Row(p) => Ok(p),
        ActiveProfile::NoProfileStore => Err(CliError::failed(no_store_refusal(db))),
        ActiveProfile::NoneStored => Err(CliError::failed(format!(
            "{} holds no recorder profile at all, so there is no subscription set to edit. \
             `vike-cli config mirror --recorder <file>` is what creates one from the TOML profile \
             this box already records with — this verb never creates a profile, because inventing \
             one would be inventing which venue feeds OPEN",
            db.display()
        ))),
        ActiveProfile::NoneActive { stored } => Err(CliError::failed(format!(
            "{stored} recorder profile(s) are stored and NONE is marked active, so there is no \
             default to edit. Name one with `--profile NAME` — `{COMMAND} ls --profiles` lists \
             them. {}",
            stored_recorder_names(profiles)
        ))),
    }
}

/// The `NoProfileStore` refusal, split by a filesystem probe into the two states `read_profiles`
/// collapses — they name different next commands.
fn no_store_refusal(db: &Path) -> String {
    if vike_secrets::database_present(db) {
        format!(
            "{} exists but holds no profile tables, so nothing has been mirrored into it yet. \
             `vike-cli config mirror --recorder <file>` is what puts a recorder profile there; \
             until then the recorder profile this box uses is still the FILE its unit's --record \
             names",
            db.display()
        )
    } else {
        format!(
            "there is no settings database at {} — this box has not been migrated, so there is \
             nowhere to store a subscription row. `vike-cli secrets migrate` creates the store and \
             `vike-cli config mirror --recorder <file>` puts a recorder profile in it. This verb \
             creates neither",
            db.display()
        )
    }
}

/// The recorder profiles a refusal names, so an operator is never told "no" without being told what
/// the alternatives are.
fn stored_recorder_names(profiles: &Profiles) -> String {
    let names: Vec<&str> = profiles
        .all()
        .iter()
        .filter(|p| p.row.kind == ProfileKind::Recorder)
        .map(|p| p.row.name.as_str())
        .collect();
    if names.is_empty() {
        return "This store holds no recorder profile at all.".to_string();
    }
    format!("Recorder profiles in this store: {}.", names.join(", "))
}

// ─── reading — `ls` ──────────────────────────────────────────────────────────────────────────────

/// The `--profiles` listing.
fn render_profiles(source: &str, profiles: &Profiles, render: Render) -> String {
    let rows: Vec<&StoredProfile> =
        profiles.all().iter().filter(|p| p.row.kind == ProfileKind::Recorder).collect();
    if render == Render::Json {
        let docs: Vec<serde_json::Value> = rows
            .iter()
            .map(|p| {
                serde_json::json!({
                    "name": p.row.name,
                    "active": p.row.active,
                    "store": p.recorder.as_ref().map(|b| b.row.store.clone()),
                    "subscriptions": p.recorder.as_ref().map_or(0, |b| b.subscriptions.len()),
                })
            })
            .collect();
        return format!(
            "{}\n",
            serde_json::json!({
                "source": source,
                "listing": "profiles",
                "profiles": docs,
                "note": RESTART_NOTE,
            })
        );
    }
    let mut out = format!("source: {source}\n");
    if rows.is_empty() {
        out.push_str(&format!("  (no recorder profiles). {}\n", stored_recorder_names(profiles)));
        return out;
    }
    let w_name = col("profile", rows.iter().map(|p| p.row.name.chars().count()));
    out.push_str(&format!("  {:<w_name$}  active  rows  store\n", "profile"));
    for p in rows {
        out.push_str(&format!(
            "  {:<w_name$}  {:<6}  {:<4}  {}\n",
            p.row.name,
            if p.row.active { "yes" } else { "-" },
            p.recorder.as_ref().map_or(0, |b| b.subscriptions.len()),
            p.recorder.as_ref().map_or("(no recorder body)", |b| b.row.store.as_str()),
        ));
    }
    out
}

/// One profile's subscription rows.
fn render_subscriptions(source: &str, target: &StoredProfile, render: Render) -> String {
    let body = target.recorder.as_ref();
    let subs: &[SubscriptionRow] = body.map_or(&[], |b| b.subscriptions.as_slice());
    if render == Render::Json {
        let docs: Vec<serde_json::Value> = subs.iter().map(subscription_json).collect();
        return format!(
            "{}\n",
            serde_json::json!({
                "source": source,
                "listing": "subscriptions",
                "profile": target.row.name,
                "active": target.row.active,
                "store": body.map(|b| b.row.store.clone()),
                "subscriptions": docs,
                "note": RESTART_NOTE,
            })
        );
    }
    let mut out = format!("source: {source}\n");
    out.push_str(&format!(
        "profile: {}{}\n",
        target.row.name,
        if target.row.active { " (active)" } else { " (NOT the active profile)" }
    ));
    let Some(body) = body else {
        out.push_str(
            "  ⚠ no recorder body — this profile row carries none, so a profile flag naming it \
             would be refused at startup\n",
        );
        return out;
    };
    out.push_str(&format!("  store: {}\n", body.row.store));
    if subs.is_empty() {
        out.push_str(&format!(
            "  (no subscriptions — this profile records NOTHING). `{COMMAND} add VENUE:SYMBOL` \
             writes one.\n"
        ));
        return out;
    }
    let cells: Vec<[String; 5]> = subs
        .iter()
        .map(|s| {
            [
                s.ord.to_string(),
                s.venue.clone(),
                what_of(s),
                s.backfill.clone().unwrap_or_else(|| "-".to_string()),
                s.note.clone().unwrap_or_else(|| "-".to_string()),
            ]
        })
        .collect();
    let w_ord = col("ord", cells.iter().map(|c| c[0].chars().count()));
    let w_venue = col("venue", cells.iter().map(|c| c[1].chars().count()));
    let w_what = col("subscription", cells.iter().map(|c| c[2].chars().count()));
    let w_back = col("backfill", cells.iter().map(|c| c[3].chars().count()));
    out.push_str(&format!(
        "  {:<w_ord$}  {:<w_venue$}  {:<w_what$}  {:<w_back$}  note\n",
        "ord", "venue", "subscription", "backfill"
    ));
    for c in &cells {
        out.push_str(&format!(
            "  {:<w_ord$}  {:<w_venue$}  {:<w_what$}  {:<w_back$}  {}\n",
            c[0], c[1], c[2], c[3], c[4]
        ));
    }
    out.push_str(&format!("{RESTART_NOTE}\n"));
    out
}

/// One subscription row as a JSON object.
///
/// ⚠ **`symbols` is an ARRAY or null and never a string.** A hand-edited row whose column does not
/// parse changes the KEY rather than the TYPE — `symbols_unparsed` carries the raw text — so a
/// consumer's shape cannot be poisoned by a row nobody wrote through this verb.
fn subscription_json(s: &SubscriptionRow) -> serde_json::Value {
    let mut row = serde_json::json!({
        "ord": s.ord,
        "venue": s.venue,
        "family": s.family,
        "symbols": serde_json::Value::Null,
        "backfill": s.backfill,
        "note": s.note,
    });
    match s.symbols.as_deref().map(parse_symbols) {
        None => {}
        Some(Ok(list)) => row["symbols"] = serde_json::json!(list),
        Some(Err(_)) => row["symbols_unparsed"] = serde_json::json!(s.symbols),
    }
    row
}

/// One row's subscription, as `ls` and every refusal spell it.
fn what_of(s: &SubscriptionRow) -> String {
    match (&s.family, &s.symbols) {
        (Some(f), _) => format!("family {f}"),
        (None, Some(syms)) => format!("symbols {syms}"),
        (None, None) => "(nothing)".to_string(),
    }
}

// ─── the symbols column ──────────────────────────────────────────────────────────────────────────

/// Parse the `symbols` column, which is stored as the TOML ARRAY RENDERING (`["BTCUSDT.P"]`) rather
/// than as a list — see `vike_secrets::profile_store::SubscriptionRow`, whose writer produces it
/// with `toml_string_array` so the renderer can emit the column verbatim.
///
/// # Errors
///
/// A `String` when the column is not a TOML array of strings — which a hand-edited row can be, and
/// which this module reports rather than silently treating as empty.
fn parse_symbols(rendered: &str) -> Result<Vec<String>, String> {
    let doc: toml::Value = toml::from_str(&format!("v = {rendered}"))
        .map_err(|e| format!("the stored `symbols` column is not a TOML array: {e}"))?;
    let array = doc
        .get("v")
        .and_then(toml::Value::as_array)
        .ok_or_else(|| "the stored `symbols` column is not an ARRAY".to_string())?;
    let mut out = Vec::with_capacity(array.len());
    for item in array {
        out.push(
            item.as_str()
                .ok_or_else(|| "the stored `symbols` column holds a non-STRING".to_string())?
                .to_string(),
        );
    }
    Ok(out)
}

// ─── writing — `add` and `rm` ────────────────────────────────────────────────────────────────────

/// A write, planned but not performed: the whole profile as it WOULD be stored, plus the report.
#[derive(Debug)]
struct Plan {
    stored: StoredProfile,
    report: String,
}

/// Build the mutated profile and the report, refusing every way the edit does not make sense.
///
/// ⚠ It clones the WHOLE `StoredProfile` rather than rebuilding one — see this module's doc. The
/// mounts, params, settings and profile-level note ride through untouched, and so does every
/// subscription `note` the rendered TOML cannot carry.
fn plan_write(args: &Args, spec: &Spec, target: &StoredProfile) -> CmdResult<Plan> {
    let mut stored = target.clone();
    let Some(body) = stored.recorder.as_mut() else {
        return Err(CliError::failed(format!(
            "profile `{}` carries no recorder body, so it has no subscription set to edit. \
             `vike-cli config mirror --recorder <file>` is what creates one",
            target.row.name
        )));
    };
    let mut report = format!("profile: {}\n", target.row.name);
    match args.verb {
        Verb::Add => {
            if let Some(clash) = duplicate_of(&body.subscriptions, spec)? {
                return Err(CliError::failed(format!(
                    "`{}` is already a subscription of profile `{}`, at ord {} ({}). NOTHING was \
                     written. `{COMMAND} ls` prints the set; `{COMMAND} rm {}` removes it",
                    spec.render(),
                    target.row.name,
                    clash.ord,
                    what_of(&clash),
                    spec.render()
                )));
            }
            let ord = body.subscriptions.iter().map(|s| s.ord).max().map_or(0, |m| m + 1);
            let row = SubscriptionRow {
                ord,
                venue: spec.venue.clone(),
                family: match &spec.what {
                    What::Family(f) => Some(f.clone()),
                    What::Symbol(_) => None,
                },
                symbols: match &spec.what {
                    What::Family(_) => None,
                    What::Symbol(s) => Some(toml_string_array(std::slice::from_ref(s))),
                },
                backfill: args.backfill.clone(),
                note: args.note.clone(),
            };
            report.push_str(&format!("+ ord {ord}  {}  {}\n", row.venue, what_of(&row)));
            body.subscriptions.push(row);
        }
        Verb::Rm => {
            let removed = remove_one(&mut body.subscriptions, spec, args.ord, &target.row.name)?;
            report.push_str(&format!(
                "- ord {}  {}  {}\n",
                removed.ord,
                removed.venue,
                what_of(&removed)
            ));
        }
        Verb::Ls => unreachable!("`ls` never plans a write"),
    }
    // ⚠ THE PARSE CHECK, and it is a CHECK rather than the write: the rendered document is thrown
    // away. It exists because the mount's own read is `render_recorder_toml` →
    // `RecorderProfile::from_toml`, so a row this verb wrote that does not survive that round trip
    // is a daemon that refuses to start — and the place to find that out is here, before anything
    // is stored.
    parse_check(body)?;
    report.push_str(&format!("\nresulting subscriptions ({}):\n", body.subscriptions.len()));
    for s in &body.subscriptions {
        report.push_str(&format!("  ord {}  {}  {}\n", s.ord, s.venue, what_of(s)));
    }
    Ok(Plan { stored, report })
}

/// Render the mutated body and require the result to PARSE. Nothing is stored from it.
fn parse_check(body: &RecorderBody) -> CmdResult<()> {
    let rendered = render_recorder_toml(body);
    toml::from_str::<toml::Value>(&rendered).map_err(|e| {
        CliError::failed(format!(
            "the edited rows render a document that does not parse ({e}), and that document is \
             what the recording daemon reads at mount — so storing them would be storing a profile \
             that refuses to start. NOTHING was written.\n--- what would have been stored \
             ---\n{rendered}"
        ))
    })?;
    Ok(())
}

/// Is this SPEC already stored? `Ok(None)` when it is not.
///
/// # Errors
///
/// A [`CliError`] when a second FAMILY row on one venue was asked for — the store's partial unique
/// index refuses that, and refusing here names the existing row instead of handing back a SQLite
/// message — or when an existing row's `symbols` column cannot be read. A comparison that cannot be
/// made is reported rather than answered "no", because answering "no" writes a duplicate.
fn duplicate_of(rows: &[SubscriptionRow], spec: &Spec) -> CmdResult<Option<SubscriptionRow>> {
    for row in rows {
        if row.venue != spec.venue {
            continue;
        }
        match (&spec.what, &row.family) {
            (What::Family(f), Some(existing)) if f == existing => return Ok(Some(row.clone())),
            // ⚠ `subscription_one_family_per_venue` is UNIQUE over `(profile, venue, family)` WHERE
            // family IS NOT NULL, so a SECOND family row on this venue would be refused by SQLite
            // at INSERT — after `store_profile` had already deleted the whole body inside its
            // transaction. Refusing here names the row that is in the way instead.
            (What::Family(_), Some(existing)) => {
                return Err(CliError::failed(format!(
                    "venue `{}` already has a FAMILY subscription in this profile (`{existing}`, \
                     ord {}), and the store admits only one per venue \
                     (`subscription_one_family_per_venue`). NOTHING was written — remove that one \
                     first, or subscribe to symbols instead",
                    spec.venue, row.ord
                )));
            }
            (What::Symbol(s), None) => {
                let Some(rendered) = row.symbols.as_deref() else { continue };
                let list = parse_symbols(rendered).map_err(|e| {
                    CliError::failed(format!(
                        "profile row ord {} cannot be compared: {e}. NOTHING was written",
                        row.ord
                    ))
                })?;
                if list.iter().any(|x| x == s) {
                    return Ok(Some(row.clone()));
                }
            }
            _ => {}
        }
    }
    Ok(None)
}

/// Remove the ONE row a SPEC names — ruling 5.
///
/// ⚠ **A SPEC IS NOT AN IDENTITY and this function never guesses.** Two symbols-based rows on one
/// venue are legal (the unique index is partial), so a match of more than one is REFUSED with every
/// candidate's `ord` printed, and `--ord N` is how the operator picks. A silent wrong removal
/// leaves a perfectly healthy-looking reconcile.
///
/// ⚠ A symbol that is one of SEVERAL on a row is a NEAR MISS rather than a match, and is reported
/// as one: this verb removes a whole subscription ROW, and removing a four-symbol row because one
/// of its symbols was named would be a silent over-removal. Editing a multi-symbol row's list is
/// not built, and the message says so rather than leaving the operator to infer it.
fn remove_one(
    rows: &mut Vec<SubscriptionRow>,
    spec: &Spec,
    ord: Option<i64>,
    profile: &str,
) -> CmdResult<SubscriptionRow> {
    let (exact, near) = match_rows(rows, spec)?;
    let candidates: Vec<SubscriptionRow> = match ord {
        Some(n) => exact.iter().filter(|r| r.ord == n).cloned().collect(),
        None => exact.clone(),
    };
    if candidates.is_empty() {
        return Err(CliError::failed(no_match_refusal(spec, ord, profile, &exact, &near)));
    }
    if candidates.len() > 1 {
        let mut msg = format!(
            "`{}` matches {} subscriptions of profile `{profile}`, and a SPEC is not an identity \
             here — the store's unique index covers FAMILY rows only, so two symbols-based rows on \
             one venue are legal. NOTHING was removed. Pick one with --ord N:",
            spec.render(),
            candidates.len()
        );
        for r in &candidates {
            msg.push_str(&format!("\n    ord {}  {}  {}", r.ord, r.venue, what_of(r)));
        }
        return Err(CliError::failed(msg));
    }
    let chosen = candidates[0].ord;
    let at = rows.iter().position(|r| r.ord == chosen).expect("the candidate came from these rows");
    Ok(rows.remove(at))
}

/// The refusal when nothing matched — it names the NEAR misses, because the commonest way to get
/// here is naming one symbol of a row that lists several.
fn no_match_refusal(
    spec: &Spec,
    ord: Option<i64>,
    profile: &str,
    exact: &[SubscriptionRow],
    near: &[SubscriptionRow],
) -> String {
    let mut msg = format!(
        "no subscription of profile `{profile}` matches `{}`{}. NOTHING was removed",
        spec.render(),
        ord.map_or_else(String::new, |n| format!(" at ord {n}"))
    );
    if !near.is_empty() {
        msg.push_str(
            ". ⚠ It IS listed on a row that names other symbols too, and this verb removes a whole \
             subscription ROW rather than one symbol of one — editing a row's symbol list is not \
             built:",
        );
        for r in near {
            msg.push_str(&format!("\n    ord {}  {}  {}", r.ord, r.venue, what_of(r)));
        }
    } else if !exact.is_empty() {
        msg.push_str(". The rows this spec DOES match:");
        for r in exact {
            msg.push_str(&format!("\n    ord {}  {}  {}", r.ord, r.venue, what_of(r)));
        }
    }
    msg
}

/// Which rows a SPEC matches EXACTLY, and which it merely appears on.
///
/// A family spec matches a row whose `family` equals it. A symbol spec matches a row whose
/// `symbols` list is EXACTLY that one symbol; a row that lists it among others is a NEAR MISS.
///
/// # Errors
///
/// A [`CliError`] when a row's `symbols` column cannot be parsed — reported rather than skipped,
/// because a row silently treated as non-matching is a row `rm` cannot reach.
fn match_rows(
    rows: &[SubscriptionRow],
    spec: &Spec,
) -> CmdResult<(Vec<SubscriptionRow>, Vec<SubscriptionRow>)> {
    let mut exact = Vec::new();
    let mut near = Vec::new();
    for row in rows {
        if row.venue != spec.venue {
            continue;
        }
        match (&spec.what, &row.family) {
            (What::Family(f), Some(existing)) if f == existing => exact.push(row.clone()),
            (What::Symbol(s), None) => {
                let Some(rendered) = row.symbols.as_deref() else { continue };
                let list = parse_symbols(rendered).map_err(|e| {
                    CliError::failed(format!(
                        "profile row ord {} cannot be matched: {e}. NOTHING was removed — \
                         `vike-cli config recorder` prints the stored column",
                        row.ord
                    ))
                })?;
                if list.len() == 1 && list[0] == *s {
                    exact.push(row.clone());
                } else if list.iter().any(|x| x == s) {
                    near.push(row.clone());
                }
            }
            _ => {}
        }
    }
    Ok((exact, near))
}

// ─── the venue check — ruling 7 ──────────────────────────────────────────────────────────────────

/// What the handshake said about a venue's recordability.
#[derive(Debug, Clone, PartialEq, Eq)]
enum VenueVerdict {
    /// The datahub advertises this venue as recordable.
    Recordable,
    /// It advertises some venues and NOT this one — the one verdict that refuses.
    NotRecordable(Vec<String>),
    /// It could not be asked, or it advertises none at all. WARN and write.
    CannotSay(String),
}

/// Every venue slug a server advertised as RECORDABLE — the read half of the [`REC_VENUE_PREFIX`]
/// pair, written the way `vike_datahub_client::proto::advertised_md_venues` reads its own: each
/// value trimmed, an EMPTY value dropped, because an empty advertisement advertises nothing.
fn advertised_rec_venues(features: &[String]) -> Vec<String> {
    features
        .iter()
        .filter_map(|f| f.strip_prefix(REC_VENUE_PREFIX))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .collect()
}

/// Ask the configured datahub whether it can record this venue. BEST EFFORT — this module's doc
/// carries the three-row table it implements.
fn probe_venue(addr: &str, keys: Option<&NodeKeys>, venue: &str) -> VenueVerdict {
    let client = match connect(addr, keys, Scope::Read) {
        Ok(c) => c,
        Err(e) => {
            return VenueVerdict::CannotSay(format!(
                "the datahub at {addr} could not be asked which venues it can record ({}), so this \
                 row was written UNCHECKED",
                e.msg
            ));
        }
    };
    let advertised = advertised_rec_venues(client.features());
    if advertised.is_empty() {
        return VenueVerdict::CannotSay(format!(
            "the datahub at {addr} advertises no recordable venue at all — it predates the \
             `{REC_VENUE_PREFIX}` advertisement, or it was built with no recorder venue feature. \
             This row was written UNCHECKED"
        ));
    }
    if advertised.iter().any(|v| v == venue) {
        return VenueVerdict::Recordable;
    }
    VenueVerdict::NotRecordable(advertised)
}

/// The refusal a `NotRecordable` verdict produces — named so the message and the verdict cannot
/// drift, and so it states the CONSEQUENCE rather than a preference.
fn unrecordable_refusal(venue: &str, advertised: &[String]) -> String {
    format!(
        "the datahub this box records with does NOT record `{venue}` — it advertises [{}]. NOTHING \
         was written. A row naming a venue the daemon's build has no feed for is legal in the store \
         and takes the daemon down at its NEXT RESTART (`vike_recorder::venues::build_feed` errors \
         by name, and the unit restarts on failure), so this is refused rather than warned about. \
         Rebuild the datahub with that venue's recorder feature, or record it from a box that has \
         one",
        advertised.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use vike_secrets::profile_store::{ProfileRow, RecorderRow};

    use super::*;

    fn parse_of(argv: &[&str]) -> Result<Args, String> {
        let owned: Vec<String> = argv.iter().map(|s| (*s).to_string()).collect();
        parse(&owned, None)
    }

    fn sub(
        ord: i64,
        venue: &str,
        family: Option<&str>,
        symbols: Option<&[&str]>,
    ) -> SubscriptionRow {
        SubscriptionRow {
            ord,
            venue: venue.to_string(),
            family: family.map(str::to_string),
            symbols: symbols.map(|s| {
                toml_string_array(&s.iter().map(|x| (*x).to_string()).collect::<Vec<_>>())
            }),
            backfill: None,
            note: None,
        }
    }

    fn profile(name: &str, active: bool, subs: Vec<SubscriptionRow>) -> StoredProfile {
        StoredProfile {
            row: ProfileRow {
                name: name.to_string(),
                kind: ProfileKind::Recorder,
                active,
                note: None,
            },
            mounts: Vec::new(),
            params: BTreeMap::new(),
            settings: BTreeMap::new(),
            recorder: Some(RecorderBody {
                row: RecorderRow {
                    store: "market_data/hist".to_string(),
                    interval_secs: None,
                    min_parts: None,
                    target_mb: None,
                    max_merge_rows: None,
                    retention_days: None,
                    alert_webhooks: None,
                    alert_repeat_secs: None,
                    alert_series_prefix: None,
                    note: None,
                },
                subscriptions: subs,
            }),
        }
    }

    fn add_args(spec: &str) -> Args {
        let mut a = parse_of(&["add", spec]).expect("a well-formed add line");
        a.addr = "127.0.0.1:1".to_string();
        a
    }

    /// **`@` MEANS A FAMILY HERE and means nothing one module up.** The two parsers answer the same
    /// token differently ON PURPOSE — see [`parse_spec`]'s doc — so this pins BOTH readings rather
    /// than only the one this file implements.
    #[test]
    fn the_at_marker_is_a_family_here_and_a_symbol_one_module_up() {
        assert_eq!(
            parse_spec("polymarket:@btc-updown-5m"),
            Ok(Spec {
                venue: "polymarket".to_string(),
                what: What::Family("btc-updown-5m".to_string()),
            })
        );
        assert_eq!(
            parse_spec("binance:BTCUSDT.P"),
            Ok(Spec { venue: "binance".to_string(), what: What::Symbol("BTCUSDT.P".to_string()) })
        );
        // The hyperliquid spelling the sibling parser exists to protect: `@107` is a real
        // instrument THERE and is a FAMILY here, which is the disagreement worth pinning.
        assert_eq!(
            parse_spec("hyperliquid:@107"),
            Ok(Spec { venue: "hyperliquid".to_string(), what: What::Family("107".to_string()) })
        );
        let empty = parse_spec("hyperliquid:@").expect_err("a bare marker names nothing");
        assert!(empty.contains("EMPTY family"), "{empty}");
    }

    /// A three-part spec is a SERIES and is refused with where an interval belongs; the two typo
    /// shapes a symbol may never have are refused by name.
    #[test]
    fn the_spec_grammar_refuses_a_series_and_the_two_typo_shapes() {
        let series = parse_spec("binance:BTCUSDT:1h").expect_err("a series is not a subscription");
        assert!(series.contains("INTERVAL"), "{series}");
        let comma = parse_spec("binance:BTCUSDT,ETHUSDT").expect_err("a comma is a typo here");
        assert!(comma.contains("comma"), "{comma}");
        assert!(comma.contains("two `add` runs"), "…and says what to do instead: {comma}");
        let space = parse_spec("binance:BTC USDT").expect_err("whitespace is a quoting mistake");
        assert!(space.contains("shell quoting"), "{space}");
        let one = parse_spec("binance").expect_err("one part is not a spec");
        assert!(one.contains("VENUE:@FAMILY"), "{one}");
    }

    /// **`--lane` is REFUSED BY NAME, and the reserved word is named in the refusal.** Ruling 1 is
    /// the one an operator is most likely to trip over, because the word means something else on
    /// the sibling verb in this same group.
    #[test]
    fn the_grain_flags_are_refused_by_name_and_the_reserved_word_is_stated() {
        for flag in ["--lane", "--stream"] {
            let why = parse_of(&["add", "binance:BTCUSDT", flag, "trades"])
                .expect_err("this grain does not exist");
            assert!(why.contains(RESERVED_GRAIN_FLAG), "it must name the reserved word: {why}");
            assert!(why.contains("Stream::ALL"), "…and the measurement behind it: {why}");
            assert!(!why.contains("unknown option"), "a real word is not an unknown one: {why}");
        }
    }

    /// `--addr` is ACCEPTED and refused by name — ruling 3 — and its value is consumed, so the flag
    /// cannot swallow the next token and report a different error.
    #[test]
    fn addr_is_accepted_and_refused_by_name_on_every_verb() {
        for argv in [
            vec!["ls", "--addr", "1.2.3.4:9"],
            vec!["add", "binance:BTCUSDT", "--addr", "1.2.3.4:9"],
            vec!["rm", "binance:BTCUSDT", "--addr=1.2.3.4:9"],
        ] {
            let why = parse_of(&argv).expect_err("the remote half is not built");
            assert!(why.contains("designed and not built"), "{argv:?}: {why}");
            assert!(why.contains("0081"), "…and points at the ruling: {why}");
        }
        // A dangling `--addr` is still the ordinary dangling-flag error rather than this one.
        let dangling = parse_of(&["ls", "--addr"]).expect_err("no value");
        assert!(dangling.contains("requires a value"), "{dangling}");
    }

    /// Each verb's own flags are refused on the others BY NAME, saying where they belong.
    #[test]
    fn a_flag_on_the_wrong_verb_names_the_verb_it_belongs_to() {
        for (argv, flag, belongs) in [
            (vec!["add", "b:S", "--profiles"], "--profiles", "ls"),
            (vec!["ls", "--backfill", "off"], "--backfill", "add"),
            (vec!["ls", "--note", "x"], "--note", "add"),
            (vec!["add", "b:S", "--ord", "1"], "--ord", "rm"),
        ] {
            let why = parse_of(&argv).expect_err("an inapplicable flag");
            assert!(why.contains(flag), "{argv:?}: {why}");
            assert!(why.contains(belongs), "{argv:?}: it must name where it belongs: {why}");
        }
        let dry = parse_of(&["ls", "--dry-run"]).expect_err("ls writes nothing");
        assert!(dry.contains("writes nothing"), "{dry}");
        let fmt = parse_of(&["add", "b:S", "--json"]).expect_err("a write renders no document");
        assert!(fmt.contains("ls --format json"), "…and names the machine form: {fmt}");
    }

    /// The verb roster is RENDERED by both the missing-verb and unknown-verb refusals, and the
    /// deleted `status` verb is named rather than left to the unknown arm.
    #[test]
    fn the_roster_is_rendered_and_the_deleted_verb_is_named() {
        let none = parse(&[], None).expect_err("a verb is required");
        for v in VERBS {
            assert!(none.contains(v.as_str()), "the roster must name {}: {none}", v.as_str());
        }
        let unknown = parse_of(&["frobnicate"]).expect_err("not a verb");
        assert!(unknown.contains("unknown"), "{unknown}");
        let status = parse_of(&["status"]).expect_err("there is no status verb");
        assert!(status.contains("columns are on"), "{status}");
        assert!(!status.contains("unknown"), "a deleted verb is not an unknown one: {status}");
    }

    /// The usage page leaves no placeholder unexpanded and documents every verb.
    #[test]
    fn the_usage_page_expands_and_names_every_verb() {
        let page = usage();
        assert!(!page.contains('{'), "an unsubstituted placeholder survived: {page}");
        for v in VERBS {
            assert!(
                page.lines().any(|l| l.starts_with(&format!("  {}", v.as_str()))),
                "`{}` has no block of its own: {page}",
                v.as_str()
            );
        }
        assert!(page.contains("NEXT RESTART"), "ruling 2 is the thing to read here: {page}");
    }

    /// **Ruling 4's three noes are three DIFFERENT messages**, because they name three different
    /// next commands — and the first splits again on whether the database file is even there.
    #[test]
    fn each_no_names_its_own_next_command() {
        let db = std::path::Path::new("/nope/settings/db/vike.db");
        let none = resolve_target(db, &Profiles::none(), None).expect_err("no store");
        assert!(none.msg.contains("secrets migrate"), "{}", none.msg);

        let empty = Profiles::from_rows(Vec::new());
        let stored = resolve_target(db, &empty, None).expect_err("no recorder profile");
        assert!(stored.msg.contains("config mirror --recorder"), "{}", stored.msg);
        assert!(!stored.msg.contains("secrets migrate"), "a DIFFERENT no: {}", stored.msg);

        let inactive = Profiles::from_rows(vec![profile("a", false, Vec::new())]);
        let none_active = resolve_target(db, &inactive, None).expect_err("none selected");
        assert!(none_active.msg.contains("--profile NAME"), "{}", none_active.msg);
        assert!(
            none_active.msg.contains("Recorder profiles in this store: a."),
            "{}",
            none_active.msg
        );

        // …and the active row is what a bare line resolves to.
        let active = Profiles::from_rows(vec![
            profile("a", false, Vec::new()),
            profile("b", true, Vec::new()),
        ]);
        let got = resolve_target(db, &active, None).expect("the active row answers");
        assert_eq!(got.row.name, "b");
        // A NAMED profile overrides it.
        let named = resolve_target(db, &active, Some("a")).expect("a named profile answers");
        assert_eq!(named.row.name, "a");
        let missing = resolve_target(db, &active, Some("zzz")).expect_err("no such profile");
        assert!(missing.msg.contains("Recorder profiles in this store: a, b."), "{}", missing.msg);
    }

    /// **THE NOTE SURVIVES.** `render_recorder_toml` never emits the `note` column, so a
    /// read-modify-write that went out through the rendered document would drop every note on the
    /// profile. This pins that the planned body carries them, which is the property the whole
    /// row-based write path exists for.
    #[test]
    fn a_planned_write_carries_every_note_the_toml_rendering_would_drop() {
        let mut existing = sub(0, "polymarket", Some("btc-updown-5m"), None);
        existing.note = Some("the family the CI box has recorded since June".to_string());
        let target = profile("default", true, vec![existing.clone()]);
        let mut args = add_args("binance:BTCUSDT.P");
        args.note = Some("added by hand".to_string());
        let plan = plan_write(&args, args.spec.as_ref().unwrap(), &target).expect("a legal add");
        let body = plan.stored.recorder.as_ref().expect("the body rides through");
        assert_eq!(body.subscriptions[0].note.as_deref(), existing.note.as_deref());
        assert_eq!(body.subscriptions[1].note.as_deref(), Some("added by hand"));
        // ...and the rendering genuinely does NOT carry it, which is why the write may not go
        // through it. This half is what makes the assertion above a fact rather than a habit.
        assert!(
            !render_recorder_toml(body).contains("added by hand"),
            "if the renderer ever learns `note`, this test's REASON has changed"
        );
    }

    /// A duplicate `add` is refused by name, and a SECOND family row on one venue is refused with
    /// the index that would otherwise refuse it inside the write transaction.
    #[test]
    fn add_refuses_a_duplicate_and_a_second_family_on_one_venue() {
        let target = profile(
            "default",
            true,
            vec![
                sub(0, "polymarket", Some("btc-updown-5m"), None),
                sub(1, "binance", None, Some(&["BTCUSDT.P"])),
            ],
        );
        let args = add_args("binance:BTCUSDT.P");
        let why = plan_write(&args, args.spec.as_ref().unwrap(), &target).expect_err("a duplicate");
        assert!(why.msg.contains("already a subscription"), "{}", why.msg);
        assert!(why.msg.contains("ord 1"), "…naming which row: {}", why.msg);

        let args = add_args("polymarket:@eth-updown-5m");
        let why =
            plan_write(&args, args.spec.as_ref().unwrap(), &target).expect_err("two families");
        assert!(why.msg.contains("subscription_one_family_per_venue"), "{}", why.msg);
        assert!(why.msg.contains("NOTHING was written"), "{}", why.msg);
    }

    /// **Ruling 5: an ambiguous `rm` REFUSES and prints every candidate's `ord`**, and `--ord N`
    /// resolves it. Two symbols-based rows on one venue are legal, which is the whole reason.
    #[test]
    fn rm_refuses_an_ambiguous_match_and_ord_resolves_it() {
        let mut rows = vec![
            sub(0, "binance", None, Some(&["BTCUSDT.P"])),
            sub(1, "binance", None, Some(&["BTCUSDT.P"])),
        ];
        let spec = parse_spec("binance:BTCUSDT.P").expect("a spec");
        let why = remove_one(&mut rows, &spec, None, "default").expect_err("two candidates");
        assert!(why.msg.contains("matches 2 subscriptions"), "{}", why.msg);
        assert!(why.msg.contains("--ord N"), "…and how to pick: {}", why.msg);
        assert!(why.msg.contains("ord 0") && why.msg.contains("ord 1"), "both: {}", why.msg);
        assert_eq!(rows.len(), 2, "NOTHING was removed");

        let removed = remove_one(&mut rows, &spec, Some(1), "default").expect("--ord picks");
        assert_eq!(removed.ord, 1);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].ord, 0, "the OTHER row survives");
    }

    /// A symbol that is one of SEVERAL on a row is a NEAR MISS, never a match — removing the whole
    /// row would be a silent over-removal, and the refusal says which row it saw.
    #[test]
    fn a_symbol_inside_a_multi_symbol_row_is_reported_not_removed() {
        let mut rows = vec![sub(0, "binance", None, Some(&["BTCUSDT.P", "ETHUSDT.P"]))];
        let spec = parse_spec("binance:ETHUSDT.P").expect("a spec");
        let why = remove_one(&mut rows, &spec, None, "default").expect_err("a near miss");
        assert!(why.msg.contains("names other symbols too"), "{}", why.msg);
        assert!(why.msg.contains("ord 0"), "…naming the row: {}", why.msg);
        assert_eq!(rows.len(), 1, "NOTHING was removed");
    }

    /// A family `rm` removes the family row and leaves the symbols rows alone.
    #[test]
    fn rm_removes_the_row_a_family_spec_names() {
        let mut rows = vec![
            sub(0, "polymarket", Some("btc-updown-5m"), None),
            sub(1, "polymarket", None, Some(&["0xdead"])),
        ];
        let spec = parse_spec("polymarket:@btc-updown-5m").expect("a spec");
        let removed = remove_one(&mut rows, &spec, None, "default").expect("one match");
        assert_eq!(removed.ord, 0);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].ord, 1);

        let spec = parse_spec("polymarket:@nope").expect("a spec");
        let why = remove_one(&mut rows, &spec, None, "default").expect_err("no match");
        assert!(why.msg.contains("no subscription"), "{}", why.msg);
    }

    /// **The three-row degrade table of the venue check**, as the pure half of it: the reader.
    /// An EMPTY advertisement is "cannot say" and not "records nothing", which is what makes this
    /// client forward-compatible with a datahub that predates the advertisement.
    #[test]
    fn an_empty_advertisement_says_nothing_rather_than_saying_no() {
        assert!(advertised_rec_venues(&[]).is_empty());
        assert!(
            advertised_rec_venues(&["market_data".to_string(), "md_venue=binance".to_string()])
                .is_empty(),
            "an md_venue entry describes the LIVE plane and must not be read as a recordable one"
        );
        assert_eq!(
            advertised_rec_venues(&[
                "rec_venue=binance".to_string(),
                "rec_venue= polymarket ".to_string(),
                "rec_venue=".to_string(),
            ]),
            vec!["binance".to_string(), "polymarket".to_string()],
            "trimmed, and an EMPTY value dropped"
        );
        let refusal = unrecordable_refusal("okx", &["binance".to_string()]);
        assert!(refusal.contains("`okx`"), "{refusal}");
        assert!(refusal.contains("NEXT RESTART"), "it must state the consequence: {refusal}");
    }

    /// The `symbols` column is TOML TEXT, and a column that does not parse is REPORTED rather than
    /// read as empty — an unparseable row is a row `rm` could otherwise never reach.
    #[test]
    fn the_symbols_column_is_toml_text_and_a_broken_one_is_reported() {
        assert_eq!(parse_symbols("[\"A\", \"B\"]"), Ok(vec!["A".to_string(), "B".to_string()]));
        assert_eq!(parse_symbols("[]"), Ok(Vec::new()));
        assert!(parse_symbols("[\"A\"").is_err(), "an unbalanced array is an error");
        assert!(parse_symbols("[1]").is_err(), "a non-string element is an error");
        let mut rows = vec![sub(0, "binance", None, None)];
        rows[0].symbols = Some("[\"A\"".to_string());
        let spec = parse_spec("binance:A").expect("a spec");
        let why = remove_one(&mut rows, &spec, None, "default").expect_err("an unreadable row");
        assert!(why.msg.contains("cannot be matched"), "{}", why.msg);
    }

    /// The `ls` renderings: the table leads with `source:` and states ruling 2, and the JSON form
    /// carries `symbols` as an ARRAY rather than the stored text.
    #[test]
    fn ls_renders_the_source_the_rows_and_the_restart_disclosure() {
        let target = profile(
            "default",
            true,
            vec![
                sub(0, "polymarket", Some("btc-updown-5m"), None),
                sub(1, "binance", None, Some(&["BTCUSDT.P"])),
            ],
        );
        let table = render_subscriptions("/s/db/vike.db", &target, Render::Table);
        assert!(table.starts_with("source: /s/db/vike.db"), "{table}");
        assert!(table.contains("profile: default (active)"), "{table}");
        assert!(table.contains("store: market_data/hist"), "{table}");
        assert!(table.contains("family btc-updown-5m"), "{table}");
        assert!(table.contains("NEXT RESTART"), "{table}");

        let doc: serde_json::Value =
            serde_json::from_str(&render_subscriptions("/s/db/vike.db", &target, Render::Json))
                .expect("the json form is one document");
        assert_eq!(doc["profile"], "default");
        assert_eq!(doc["subscriptions"][1]["symbols"][0], "BTCUSDT.P");
        assert!(doc["subscriptions"][0]["symbols"].is_null(), "a family row has no symbols");

        let listing =
            render_profiles("/s/db/vike.db", &Profiles::from_rows(vec![target]), Render::Table);
        assert!(listing.contains("default"), "{listing}");
        assert!(listing.contains("yes"), "the active column: {listing}");
    }
}
