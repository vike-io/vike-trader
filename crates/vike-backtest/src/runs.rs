//! What a run LEAVES BEHIND: one directory under `<project>/user_data/runs/`, holding a manifest
//! every producer writes the same way beside a report only that producer's kind understands.
//!
//! # Why a run has to leave anything behind
//!
//! A backtest that only PRINTS has no history. There is nothing to list, nothing to compare a
//! second run against, and nothing a UI could ever show — the result exists for as long as the
//! terminal scrollback does. `<project>/user_data/runs/` (`vike_model::state_path::RUNS_SUBDIR`)
//! is where that stops being true.
//!
//! # Why the manifest is the load-bearing half
//!
//! **A listing must be able to render a row without a parser per kind.** A backtest run and a
//! research run differ in what their results CONTAIN, not in what a listing needs to say about
//! them — an id, when it ran, what produced it, which config drove it. So those fields sit at the
//! TOP LEVEL of [`RunManifest`], identical for every producer, and everything a particular kind of
//! run wants to record nests BELOW them in [`RunManifest::detail`]. A kind-specific field placed
//! beside them rather than under them is the defect this shape exists to refuse: the moment one
//! kind adds a top-level field of its own, every reader must know which kind it is holding before
//! it can read the first key, and "one common manifest" has become two manifests sharing a
//! filename.
//!
//! # The report is written FIRST and the manifest LAST
//!
//! The report is the irreplaceable half — it is what the run computed — so a disk that fills
//! between the two writes should cost the metadata rather than the result. Ordering them this way
//! also hands a reader a completion marker it never had to be told about: a directory holding a
//! [`MANIFEST_FILE`] is a run that finished writing, so a listing can skip a half-written run
//! without a lock file, a temp name or a rename dance.
//!
//! # The runs directory is a PARAMETER, and so is the CLOCK
//!
//! Nothing here resolves a path. The BINARY calls `vike_model::state_path::user_runs_dir` and hands
//! the answer down, because a library that resolves its own project root reads global state its
//! caller can neither see nor override — the rule `crates/vike-ops/tests/settings_registry.rs`'s
//! `LIBRARY_PIN` ratchets down, and the reason ONE walk decides which project a process is in.
//!
//! Nothing here reads the clock either, for the stronger reason
//! `crates/vike-ops/tests/clock_pin.rs`'s `CLOCK_PIN` states: in the crates that carry
//! `backtest == paper == live`, TIME IS AN INPUT, and this crate contains no ambient clock read at
//! all. So [`create_run_dir`] takes the run's start SECOND and [`utc_rfc3339`] takes any second —
//! which is also what lets a test mint an id against a fixed instant instead of racing one.
//!
//! # Persisting is additive, never a new way to fail
//!
//! Every function here returns its failure as a value. A caller prints its result FIRST and
//! persists afterwards, so a run whose directory cannot be created still hands the operator the
//! numbers it computed, with the failure named on stderr rather than swallowed. Saving a run is
//! worth doing; it is not worth losing a run over.
//!
//! # Where this module lives, and when it moves
//!
//! It sits in `vike-backtest` because that is where the first producer is. The manifest is common
//! by SHAPE today and its code is not yet shared, so the second producer to write a run directory
//! decides one thing: if it may depend on this crate it calls this module, and if the layering
//! refuses that edge (`crates/vike-ops/tests/layer_gate.rs`'s `tier_of`) the module
//! MOVES DOWN, whole, beside `RUNS_SUBDIR` in `vike-model`. A move, never a copy, and never a
//! `pub use` shim behind it: two definitions of a common manifest is the one outcome the shape
//! above cannot survive.
//!
//! ⚠ The second producer has ARRIVED and took the first branch, so the move is not owed:
//! `crates/vike-studio-core/src/study_run.rs`'s `run_study` persists a study run by CALLING
//! [`create_run_dir`] and [`write_run`], and it can because it declares layer 55 against this
//! crate's 50. (This paragraph named the research engine as the likely second producer and
//! predicted the opposite outcome — that crate declared a LOWER layer and so would have forced the
//! module down. It never wrote through this module and has since been deleted outright, so the
//! prediction was retired by the crate's disappearance rather than tested.)

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The manifest's file name inside a run directory — also the completion marker, for the reason
/// this module's doc gives.
pub const MANIFEST_FILE: &str = "manifest.json";

/// The report's file name inside a run directory. Its CONTENT is whatever the producer's own report
/// type serializes to; only the NAME is common.
pub const REPORT_FILE: &str = "report.json";

/// How many ids [`create_run_dir`] tries before giving up. A bound rather than an unbounded loop,
/// because a directory tree that reports `AlreadyExists` without ever yielding a free name would
/// otherwise spin forever; a thousand runs started by ONE process inside ONE second is far past
/// anything a backtest binary can do.
const MINT_ATTEMPTS: u32 = 1_000;

/// Which config produced a run — the identity question every listing asks and no kind can answer
/// for another. Both halves are optional: a producer driven by flags alone has no file to name, and
/// a config file need not carry a label.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunConfig {
    /// The config FILE, spelled as the operator spelled it on the command line — deliberately NOT
    /// canonicalized. A canonical path answers about the box the run happened on; the operator's
    /// own spelling is what they can paste back to reproduce it.
    pub path: Option<String>,
    /// A short human label for that config when it carries one (a backtest profile's `name`).
    pub name: Option<String>,
}

/// The COMMON half of a run directory: what every producer of every kind writes identically, so a
/// listing renders a row without knowing which kind it is holding.
///
/// # `run_id` cannot collide, and the FILESYSTEM is what guarantees it
///
/// The id is `<unix-seconds>-<pid>-<seq>`. Seconds say WHEN, which is what a human sorting a
/// directory wants; the pid says WHICH PROCESS, which is what two runs started in the same second
/// differ by and what a bug report can actually use; `seq` breaks the rest.
///
/// ⚠ **None of those three is the guarantee.** Two processes in different pid namespaces sharing
/// one bind-mounted project can hold the same pid at the same instant, and a discriminator drawn
/// from a clock is a probability rather than a rule. The guarantee is that [`create_run_dir`] mints
/// the id by CREATING the directory: `create_dir` is atomic on every platform this ships to and
/// fails with `AlreadyExists` rather than opening what is already there, so the first creator of a
/// given id owns it and every later one is told to try the next `seq`. The id is not checked and
/// then used — checking and then using is the race. Creating IS the check.
///
/// The research producer minted its `run_id` from a bare unix-seconds clock read
/// (`crates/vike-research/src/bin/research.rs`'s `now_secs`), so two of its runs starting in the
/// same second collided. That rule is deliberately NOT inherited: this namespace is shared, and a
/// producer arriving second must be able to mint into it without asking what the first one did.
/// ⚠ That binary is GONE — it went with the research crate, and the citation is kept as the
/// evidence for the rule rather than as a file to go and read (it is filed in
/// `crates/vike-ops/tests/citation_gate.rs`'s `DEAD_PATH_EXCEPTIONS`). The rule outlived it: the
/// producer that did arrive, `crates/vike-studio-core/src/study_run.rs`'s `run_study`, mints
/// through [`create_run_dir`] and inherits the guarantee instead of a clock.
///
/// # Fields are public and there is no constructor
///
/// A struct literal cannot omit a field. That is the point: when this manifest gains a common
/// field, every producer stops compiling until it decides what to put there — exactly the review a
/// shared shape needs, and exactly what a `new()` with a default would have hidden.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RunManifest {
    /// The directory name this manifest sits in. Duplicated INTO the file on purpose: a listing
    /// that has read the manifest should not have to re-derive identity from the path it read it
    /// through, and a manifest copied out of its directory should still say which run it is.
    pub run_id: String,
    /// What KIND of run this was — `"backtest"` for this producer. The one field a reader may
    /// branch on, and a plain string rather than an enum because the set of kinds grows in crates
    /// that cannot see each other; a reader that does not recognise a kind still renders every
    /// field above and beside it.
    pub kind: String,
    /// The BINARY that produced the run, spelled literally. Not `CARGO_PKG_NAME`, which is the
    /// PACKAGE (`vike-backtest`) rather than the binary (`backtest`) — the same distinction
    /// `backtest --version` makes, for the same reason: a name the operator never invoked is a name
    /// a bug report cannot use.
    pub produced_by: String,
    /// When the run STARTED, RFC-3339 UTC to the second (`2026-08-24T09:15:04Z`) — see
    /// [`utc_rfc3339`], which is the one spelling so two producers cannot disagree about the format
    /// of a common field. A string rather than a number because a manifest is a document a human
    /// opens and every language a listing could be written in parses RFC-3339 in one line. Derived
    /// from the SAME clock read that minted [`Self::run_id`], so the two can never disagree about
    /// when the run began.
    pub started_at: String,
    /// When the run FINISHED, same format. Read after the work and before the result is printed, so
    /// it measures the run rather than the terminal.
    pub finished_at: String,
    /// The commit the producing binary was BUILT from, or `None` when the producer cannot name one.
    ///
    /// ⚠ **Always serialized, `null` included** — never skipped. A listing renders a column off a
    /// key set it can rely on, and "this producer could not name a build" is an answer worth
    /// showing; a missing key would make it indistinguishable from a manifest written before the
    /// field existed. `backtest` writes `None`: naming a commit means `vike-buildinfo`, whose
    /// `build.rs` resolves it at COMPILE time rather than shelling `git rev-parse` at runtime (which
    /// would answer about the working directory instead of about the binary), and `vike-backtest`
    /// does not depend on that crate. A producer whose BINARY can answer fills this in —
    /// `crates/vike-studio-core/src/study_run.rs`'s `StudyRunRequest` takes the sha as a parameter
    /// for exactly that reason.
    pub git_sha: Option<String>,
    /// Which config drove the run. See [`RunConfig`].
    pub config: RunConfig,
    /// Everything KIND-SPECIFIC, nested so it cannot crowd the common fields above. `Null` for a
    /// producer with nothing to add. A reader that does not recognise [`Self::kind`] ignores this
    /// whole subtree and still renders a complete listing row — which is the entire argument for
    /// the nesting.
    #[serde(default)]
    pub detail: serde_json::Value,
}

/// A run directory that now EXISTS, and the id its creation minted.
#[derive(Clone, Debug)]
pub struct RunDir {
    /// The minted id — [`RunManifest::run_id`] carries the rule and why it cannot collide.
    pub run_id: String,
    /// The created directory: `<runs_root>/<run_id>`.
    pub path: PathBuf,
}

/// Why a run could not be persisted. Every variant names the PATH it failed on, because "the run
/// was not saved" without a location is a message an operator cannot act on.
#[derive(Debug)]
pub enum RunPersistError {
    /// A directory could not be created.
    Dir {
        /// The directory that could not be created.
        path: PathBuf,
        /// The operating system's own words.
        why: String,
    },
    /// A file could not be written.
    Write {
        /// The file that could not be written.
        path: PathBuf,
        /// The operating system's own words.
        why: String,
    },
    /// A value could not be turned into JSON.
    Serialize {
        /// Which of the two documents it was — [`MANIFEST_FILE`] or [`REPORT_FILE`].
        file: &'static str,
        /// serde_json's own words.
        why: String,
    },
    /// [`MINT_ATTEMPTS`] ids in a row were already taken — in practice a runs directory that
    /// refuses creation while reporting `AlreadyExists`, not a genuine flood of runs.
    IdExhausted {
        /// The runs directory that yielded no free id.
        root: PathBuf,
        /// How many ids were tried.
        attempts: u32,
    },
}

impl fmt::Display for RunPersistError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dir { path, why } => write!(f, "cannot create {}: {why}", path.display()),
            Self::Write { path, why } => write!(f, "cannot write {}: {why}", path.display()),
            Self::Serialize { file, why } => write!(f, "cannot serialize {file}: {why}"),
            Self::IdExhausted { root, attempts } => {
                write!(f, "no free run id under {} after {attempts} tries", root.display())
            }
        }
    }
}

impl std::error::Error for RunPersistError {}

/// Unix seconds as RFC-3339 UTC to the second — the ONE spelling every manifest timestamp takes, so
/// two producers cannot disagree about the format of a common field.
///
/// A second no calendar date can hold falls back to the raw number rather than panicking: a
/// manifest is metadata about a run that already succeeded, and must never be the thing that kills
/// it.
pub fn utc_rfc3339(unix_secs: i64) -> String {
    chrono::DateTime::from_timestamp(unix_secs, 0)
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_else(|| unix_secs.to_string())
}

/// Create a fresh run directory under `runs_root`, minting an id that cannot collide with another
/// run's — [`RunManifest::run_id`] carries the rule and why creation IS the check.
///
/// `started_at` is the run's own clock read in unix seconds, passed IN rather than read here so the
/// id and [`RunManifest::started_at`] name the same instant.
///
/// `runs_root` and its parents are created when absent: a project that has run nothing has no
/// `user_data/runs/`, which is a fresh install rather than an error.
pub fn create_run_dir(runs_root: &Path, started_at: i64) -> Result<RunDir, RunPersistError> {
    std::fs::create_dir_all(runs_root)
        .map_err(|e| RunPersistError::Dir { path: runs_root.to_path_buf(), why: e.to_string() })?;
    let pid = std::process::id();
    for seq in 0..MINT_ATTEMPTS {
        let run_id = format!("{started_at}-{pid}-{seq}");
        let path = runs_root.join(&run_id);
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(RunDir { run_id, path }),
            // The whole collision rule, in one arm: somebody else owns that id, so try the next.
            // Reached by a second run in the same second of the same process, and by a second
            // PROCESS that shares this one's pid — which is the case a clock-derived discriminator
            // would have called impossible.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(RunPersistError::Dir { path, why: e.to_string() }),
        }
    }
    Err(RunPersistError::IdExhausted { root: runs_root.to_path_buf(), attempts: MINT_ATTEMPTS })
}

/// Write the two documents of a run directory: the report FIRST, the manifest LAST — this module's
/// doc carries the argument for that order.
///
/// `report` is generic rather than a named type because the report is the KIND-SPECIFIC half: this
/// function's contract is the file names and the ordering, never the schema of what a particular
/// producer computed.
pub fn write_run<R>(dir: &Path, manifest: &RunManifest, report: &R) -> Result<(), RunPersistError>
where
    R: Serialize + ?Sized,
{
    write_json(&dir.join(REPORT_FILE), REPORT_FILE, report)?;
    write_json(&dir.join(MANIFEST_FILE), MANIFEST_FILE, manifest)
}

fn write_json<T>(path: &Path, file: &'static str, value: &T) -> Result<(), RunPersistError>
where
    T: Serialize + ?Sized,
{
    let mut json = serde_json::to_string_pretty(value)
        .map_err(|e| RunPersistError::Serialize { file, why: e.to_string() })?;
    json.push('\n');
    std::fs::write(path, json)
        .map_err(|e| RunPersistError::Write { path: path.to_path_buf(), why: e.to_string() })
}

/// Why a run directory's manifest could not be READ back. The counterpart of [`RunPersistError`],
/// and it lives here rather than in whatever crate happens to list runs first: the schema is
/// [`RunManifest`]'s, so the code that turns bytes into one belongs beside the code that turns one
/// into bytes. A reader written up the dependency graph would be a second definition of a common
/// manifest, which this module's doc names as the one outcome that shape cannot survive.
///
/// ⚠ [`RunReadError::Missing`] is deliberately NOT folded into [`RunReadError::Read`], even though
/// both are "no manifest came back". The manifest is written LAST (see this module's doc), so its
/// absence means the run is being written RIGHT NOW or a process died between the two writes —
/// while an unreadable one means a file that exists and cannot be opened. A listing renders those
/// as different rows because they have different fixes: wait, versus go and look at the file.
#[derive(Debug)]
pub enum RunReadError {
    /// The directory holds no [`MANIFEST_FILE`] — an unfinished run, not a broken one.
    Missing {
        /// Where the manifest was looked for.
        path: PathBuf,
    },
    /// The manifest is there and could not be read as text — permissions, or not UTF-8.
    Read {
        /// The file that could not be read.
        path: PathBuf,
        /// The operating system's own words.
        why: String,
    },
    /// The text is not a [`RunManifest`]: not JSON at all, or JSON missing a COMMON field. Both are
    /// one variant because a listing acts on them identically — the document cannot produce a row,
    /// and `why` carries which of the two it was in the parser's own words.
    Parse {
        /// The file that could not be parsed.
        path: PathBuf,
        /// serde_json's own words.
        why: String,
    },
}

impl fmt::Display for RunReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing { path } => write!(
                f,
                "no {MANIFEST_FILE} at {} — the run is still being written, or it stopped between \
                 its report and its manifest",
                path.display()
            ),
            Self::Read { path, why } => write!(f, "cannot read {}: {why}", path.display()),
            Self::Parse { path, why } => write!(f, "cannot parse {}: {why}", path.display()),
        }
    }
}

impl std::error::Error for RunReadError {}

/// Read one run directory's [`MANIFEST_FILE`] — the READING half of this module, and the call any
/// listing of `<project>/user_data/runs/` is written on top of
/// (`crates/vike-studio-core/src/listing.rs`'s `list_runs`).
///
/// Takes the RUN DIRECTORY rather than the manifest path, symmetrically with [`write_run`], so the
/// file name stays this module's business: a caller that spelled `manifest.json` itself would be a
/// second place the name lives.
///
/// Every failure is a value naming the path — nothing here panics, and nothing is silently skipped.
/// A run that vanishes from a listing is worse than one that shows as broken: the first is
/// unanswerable, the second names its own fix.
pub fn read_manifest(dir: &Path) -> Result<RunManifest, RunReadError> {
    let path = dir.join(MANIFEST_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(RunReadError::Missing { path });
        }
        Err(e) => return Err(RunReadError::Read { path, why: e.to_string() }),
    };
    serde_json::from_str(&text).map_err(|e| RunReadError::Parse { path, why: e.to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn a_manifest(run_id: &str) -> RunManifest {
        RunManifest {
            run_id: run_id.to_string(),
            kind: "backtest".to_string(),
            produced_by: "backtest".to_string(),
            started_at: utc_rfc3339(1_756_000_000),
            finished_at: utc_rfc3339(1_756_000_012),
            git_sha: None,
            config: RunConfig {
                path: Some("profiles/sma.toml".to_string()),
                name: Some("sma cross".to_string()),
            },
            detail: json!({ "strategy": "sma_cross" }),
        }
    }

    /// The collision rule, stated as the property that matters: the SAME clock second, minted
    /// twice, must not name one directory. The research producer's bare-seconds id failed exactly
    /// this, which is why the rule is decided here rather than inherited.
    #[test]
    fn two_runs_starting_in_the_same_second_cannot_share_a_run_id() {
        let root = tempfile::tempdir().unwrap();
        let runs = root.path().join("runs");

        let first = create_run_dir(&runs, 1_756_000_000).unwrap();
        let second = create_run_dir(&runs, 1_756_000_000).unwrap();

        assert_ne!(first.run_id, second.run_id, "one clock second must not mint one id twice");
        assert_ne!(first.path, second.path);
        assert!(first.path.is_dir(), "minting a run id CREATES the directory — that is the check");
        assert!(second.path.is_dir());
    }

    /// Creation is the check, so an id whose directory already exists must be refused rather than
    /// reused: handing one out twice lets the second run overwrite the first one's report.
    #[test]
    fn an_id_whose_directory_already_exists_is_never_handed_out() {
        let root = tempfile::tempdir().unwrap();
        let runs = root.path().join("runs");
        std::fs::create_dir_all(&runs).unwrap();
        let pid = std::process::id();
        let taken = format!("1756000000-{pid}-0");
        std::fs::create_dir(runs.join(&taken)).unwrap();
        std::fs::write(runs.join(&taken).join(REPORT_FILE), b"the first run's result").unwrap();

        let minted = create_run_dir(&runs, 1_756_000_000).unwrap();

        assert_ne!(minted.run_id, taken);
        assert!(
            minted.run_id.ends_with("-1"),
            "a taken id must advance to the NEXT seq, not to some unrelated name: {}",
            minted.run_id
        );
        assert_eq!(
            std::fs::read(runs.join(&taken).join(REPORT_FILE)).unwrap(),
            b"the first run's result",
            "the run already holding that id keeps its report"
        );
    }

    /// The id opens with the second the run started, so a plain directory listing sorts by when —
    /// the one ordering a human scanning `user_data/runs/` actually wants.
    #[test]
    fn a_run_id_opens_with_the_second_its_run_started() {
        let root = tempfile::tempdir().unwrap();
        let minted = create_run_dir(&root.path().join("runs"), 1_756_000_000).unwrap();

        assert!(
            minted.run_id.starts_with("1756000000-"),
            "run id {} must open with its start second",
            minted.run_id
        );
    }

    /// The whole reason this manifest is common: a listing renders a row off the TOP level, without
    /// knowing what kind of run produced the file.
    #[test]
    fn every_common_field_sits_at_the_top_level_so_a_listing_needs_no_per_kind_parser() {
        let v = serde_json::to_value(a_manifest("r-1")).unwrap();
        let obj = v.as_object().expect("a manifest is a JSON object");

        for key in
            ["run_id", "kind", "produced_by", "started_at", "finished_at", "git_sha", "config"]
        {
            assert!(obj.contains_key(key), "a listing row needs `{key}` at the top level");
        }
    }

    /// Kind-specific detail nests BELOW the common fields. Beside them is the defect the nesting
    /// exists to refuse — it forces every reader to know the kind before it can read the first key.
    #[test]
    fn kind_specific_detail_nests_below_the_common_fields_rather_than_beside_them() {
        let v = serde_json::to_value(a_manifest("r-1")).unwrap();
        let obj = v.as_object().unwrap();

        assert_eq!(obj["detail"]["strategy"], json!("sma_cross"));
        assert!(
            !obj.contains_key("strategy"),
            "`strategy` is a backtest's business and must not sit beside the common fields"
        );
    }

    /// `git_sha` is written even when the producer cannot name a build. A missing key would be
    /// indistinguishable from a manifest predating the field; `null` says "this producer does not
    /// know", which is a different and reportable answer.
    #[test]
    fn a_producer_that_cannot_name_a_build_still_writes_the_git_sha_key() {
        let v = serde_json::to_value(a_manifest("r-1")).unwrap();

        assert_eq!(v.as_object().unwrap().get("git_sha"), Some(&serde_json::Value::Null));
    }

    /// A run directory holds BOTH documents, and the report is stored verbatim — a report's JSON is
    /// a machine contract, and persisting it must not reshape it.
    #[test]
    fn a_written_run_holds_the_report_verbatim_beside_its_manifest() {
        let root = tempfile::tempdir().unwrap();
        let run = create_run_dir(&root.path().join("runs"), 1_756_000_000).unwrap();
        let report = json!({ "name": "sma cross", "sharpe": 1.25, "trades": 42 });

        write_run(&run.path, &a_manifest(&run.run_id), &report).unwrap();

        let back: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(run.path.join(REPORT_FILE)).unwrap())
                .unwrap();
        assert_eq!(back, report);
        let m: RunManifest =
            serde_json::from_str(&std::fs::read_to_string(run.path.join(MANIFEST_FILE)).unwrap())
                .unwrap();
        assert_eq!(m.run_id, run.run_id);
        assert_eq!(m.kind, "backtest");
    }

    /// Persisting is additive, never a new way to fail: a runs root that cannot be created comes
    /// back as a VALUE naming the path, so the caller can print the result it already computed and
    /// report the failure beside it.
    #[test]
    fn a_runs_root_that_cannot_be_created_is_reported_rather_than_panicking() {
        let root = tempfile::tempdir().unwrap();
        let blocked = root.path().join("blocked");
        std::fs::write(&blocked, b"a file where the runs directory would go").unwrap();

        let err = create_run_dir(&blocked, 1_756_000_000).unwrap_err();

        assert!(
            matches!(err, RunPersistError::Dir { .. }),
            "expected a directory failure, got {err:?}"
        );
        assert!(err.to_string().contains("blocked"), "the message must name the path: {err}");
    }

    /// The round trip that makes a listing possible at all: what [`write_run`] wrote comes back as
    /// the same COMMON fields, through the reader that lives beside the writer rather than through
    /// a second spelling of the schema somewhere up the dependency graph.
    #[test]
    fn a_written_manifest_reads_back_with_every_common_field_intact() {
        let root = tempfile::tempdir().unwrap();
        let run = create_run_dir(&root.path().join("runs"), 1_756_000_000).unwrap();
        let written = a_manifest(&run.run_id);
        write_run(&run.path, &written, &json!({ "sharpe": 1.25 })).unwrap();

        let back = read_manifest(&run.path).unwrap();

        assert_eq!(back.run_id, written.run_id);
        assert_eq!(back.kind, "backtest");
        assert_eq!(back.produced_by, "backtest");
        assert_eq!(back.started_at, written.started_at);
        assert_eq!(back.finished_at, written.finished_at);
        assert_eq!(back.git_sha, None);
        assert_eq!(back.config.path.as_deref(), Some("profiles/sma.toml"));
        assert_eq!(back.detail["strategy"], json!("sma_cross"));
    }

    /// The manifest is written LAST, so its ABSENCE is a distinct answer rather than a corrupt
    /// file: the run is being written right now, or a process died between the two writes. A reader
    /// that collapsed this into "unreadable" would make a listing call a running backtest broken.
    #[test]
    fn a_directory_whose_manifest_is_absent_reads_as_missing_not_as_unreadable() {
        let root = tempfile::tempdir().unwrap();
        let run = create_run_dir(&root.path().join("runs"), 1_756_000_000).unwrap();
        // The half-written shape exactly: the report landed, the manifest has not.
        std::fs::write(run.path.join(REPORT_FILE), b"{}\n").unwrap();

        let err = read_manifest(&run.path).unwrap_err();

        assert!(matches!(err, RunReadError::Missing { .. }), "expected Missing, got {err:?}");
        assert!(
            err.to_string().contains(MANIFEST_FILE),
            "the message must name the file it looked for: {err}"
        );
    }

    /// A corrupt manifest is REPORTED, never treated as absent — a run that vanishes from a listing
    /// is worse than one that shows as broken, and the two have different fixes.
    #[test]
    fn a_manifest_that_is_not_json_is_reported_with_the_parser_error_and_the_path() {
        let root = tempfile::tempdir().unwrap();
        let run = create_run_dir(&root.path().join("runs"), 1_756_000_000).unwrap();
        std::fs::write(run.path.join(MANIFEST_FILE), b"{ not json").unwrap();

        let err = read_manifest(&run.path).unwrap_err();

        match &err {
            RunReadError::Parse { path, why } => {
                assert!(path.ends_with(MANIFEST_FILE), "the path must be the manifest: {path:?}");
                assert!(!why.is_empty(), "serde_json's own words must be carried through");
            }
            other => panic!("expected Parse, got {other:?}"),
        }
    }

    /// A manifest that parses as JSON but is missing a COMMON field is a parse failure too: the
    /// common fields are what a listing renders a row from, so a document without them is not a
    /// manifest, whatever it is.
    #[test]
    fn a_json_document_missing_a_common_field_is_a_parse_failure() {
        let root = tempfile::tempdir().unwrap();
        let run = create_run_dir(&root.path().join("runs"), 1_756_000_000).unwrap();
        std::fs::write(run.path.join(MANIFEST_FILE), br#"{ "run_id": "r-1" }"#).unwrap();

        let err = read_manifest(&run.path).unwrap_err();

        assert!(matches!(err, RunReadError::Parse { .. }), "expected Parse, got {err:?}");
    }

    /// A manifest that exists and cannot be READ is its own answer — a permissions bug must not
    /// wear the "not written yet" reply, for the same reason the credential store refuses to.
    #[test]
    fn a_manifest_that_cannot_be_read_is_distinct_from_one_that_is_absent() {
        let root = tempfile::tempdir().unwrap();
        let run = create_run_dir(&root.path().join("runs"), 1_756_000_000).unwrap();
        // A DIRECTORY where the file belongs: unreadable as text on every platform this ships to,
        // and reached without asking a test to change file permissions.
        std::fs::create_dir(run.path.join(MANIFEST_FILE)).unwrap();

        let err = read_manifest(&run.path).unwrap_err();

        assert!(matches!(err, RunReadError::Read { .. }), "expected Read, got {err:?}");
        assert!(err.to_string().contains(MANIFEST_FILE), "the message must name the path: {err}");
    }
}
