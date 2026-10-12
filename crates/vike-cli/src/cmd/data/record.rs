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
//! The shipped binary records only what `crates/vike-datahub/src/recording.rs`'s `supported` lists,
//! while the LIVE market-data plane serves six venues — so `record add okx:…` would write a legal
//! row that takes the data daemon down at its next restart under `Restart=on-failure`. The datahub
//! advertises a recordable venue as [`REC_VENUE_PREFIX`]`<slug>` in its handshake features, the twin
//! of the `md_venue=` entries `vike_datahub_client::md_venue_feature` builds.
//! [`probe_venue`] answers one of three ways, and only ONE of them refuses:
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
//! `vike_datahub_client::advertised_md_venues` applies to its own values.
//!
//! ⚠ **[`REC_VENUE_PREFIX`] is spelled HERE and must not stay that way.** The advertisement's
//! WRITE half lands in `crates/vike-datahub/src/recorder.rs` and its constant belongs in
//! `crates/vike-datahub-client/src/proto.rs` beside `FEATURE_MD_VENUE_PREFIX`; when that constant
//! exists this one is DELETED and imported, because a rule this workspace states in as many words
//! is that a symbol has one name. It is spelled here rather than blocked on that work so the
//! client half can ship and be tested; the cost is one spelling that must be collapsed, and the
//! third row of the table above is what keeps the two from disagreeing dangerously meanwhile.

mod grammar;
mod plan;
mod render;

use std::path::Path;
use std::process::ExitCode;

use vike_node_proto::auth::NodeKeys;
use vike_secrets::profile_store::{OperatorWrite, read_profiles, store_profile};

use super::{DEFAULT_ADDR, col, connect};
use crate::cmd::args::exit_for_parse_error;
use crate::exit::{CliError, CmdResult};

use self::grammar::parse;
use self::plan::{
    VenueVerdict, now_utc, plan_write, probe_venue, resolve_target, unrecordable_refusal,
};
use self::render::{render_profiles, render_subscriptions, usage};

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

#[cfg(test)]
use vike_secrets::profile_store::{ProfileKind, Profiles, RecorderBody, StoredProfile};
#[cfg(test)]
use vike_secrets::profile_store::{SubscriptionRow, render_recorder_toml, toml_string_array};

#[cfg(test)]
use self::grammar::{VERBS, parse_spec};
#[cfg(test)]
use self::plan::{advertised_rec_venues, parse_symbols, remove_one};

#[path = "record_tests.rs"]
#[cfg(test)]
mod record_tests;
