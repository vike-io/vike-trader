//! The versioned strategy SDK's vocabulary (decision 0296,
//! `docs/decisions/0296-a-rust-strategy-file-builds-against-a-numbered-frozen-sdk-it-may-pin.md`): which SDK
//! a build compiles against ([`Sdk`]), the template kinds a snapshot is hashed per ([`TemplateKind`]), the
//! snapshot's MARKER `sdk-N/SDK` ([`SdkMarker`]: its reader, writer and verifier), its rename table
//! `sdk-N/RENAMES` ([`rename_table`]: the format, its writer and its reader), the host's
//! [`BOUNDARY_HASH`], the ADAPTERS that let an older SDK cut with another boundary build against this
//! host's ([`SdkAdapter`], [`built_in_adapters`], [`host_tree`]: step 6), and the ONE rule saying whether an SDK is
//! usable on this host and how it builds ([`usable`], [`SdkUse`]): an SDK whose lent rows are laid out
//! otherwise is retired, never adapted ([`SdkUnusable::RowsMoved`]).
//!
//! **The layout.** SDK N's source is a frozen snapshot unpacked at `<project>/`[`SDK_SOURCE_DIR`]`/sdk-N/`,
//! its marker the TOML file [`SDK_MARKER_FILE`] at that folder's root. A git checkout (the `workspace_root`
//! of a test or a lane) is "SDK dev" ([`Sdk::Dev`], [`is_dev_checkout`]): its hash is computed live, its
//! artifacts land under `sdk-dev/`, and a file's pin is ignored there. Where a build lands is
//! `crate::artifact`'s.
//!
//! **Why here.** This crate owns the walk both hashes are folded by (`build/source_hash.rs`, compiled in
//! below by `#[path]` and by `build.rs`), the toolchain fingerprint and the boundary itself; the builder
//! (`vike-strategy-builder`) and the sandboxed child (`vike-strategy-sandbox`) both sit above it. The marker
//! is a strategy file's concern only: a `.rhai` file runs on the host's own code (decision 0296, verdict 2).

use std::fmt;
use std::fs;
use std::io::{ErrorKind, Write as _};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The hash of the strategy BOUNDARY this binary was built with: `src/abi.rs`, `src/guest.rs`,
/// `src/host.rs`, `src/study.rs` (a study file's byte boundary), `src/guest/`, `src/host/`, `template/` and
/// the text goldens under `tests/boundary/`, FNV-1a 64,
/// sixteen lowercase hex digits (`build/source_hash.rs`'s `boundary_hash`, baked by `build.rs` beside the
/// fingerprint). An SDK cut with another one builds here only through this host's adapter from it ([`usable`]).
pub const BOUNDARY_HASH: &str = env!("VIKE_STRATEGY_BOUNDARY_HASH");

/// `vendor/vike-strategy-source`, under the project root: the parent of every installed SDK snapshot,
/// one `sdk-N/` folder each ([`Sdk::directory_name`]).
pub const SDK_SOURCE_DIR: &str = "vendor/vike-strategy-source";

/// The marker's file name at a snapshot's root: `sdk-N/SDK` ([`SdkMarker`]).
pub const SDK_MARKER_FILE: &str = "SDK";

/// Where the `vike-rust-strategy` crate sits in a source tree (a snapshot or a checkout): its templates
/// start the source walk, and its boundary files are what [`boundary_hash`] reads.
pub const RUST_STRATEGY_CRATE_DIR: &str = "crates/vike-rust-strategy";

/// How many SDKs BEFORE the newest still build on a host (decision 0296, verdict 5): when SDK 9 ships,
/// 7 and 8 still build and 6 retires.
pub const SDK_SUPPORT_WINDOW: u32 = 2;

/// The start of the strategy template's one opt-in line, the ONNX runtime's dependency
/// (`crates/vike-strategy-builder/src/render.rs`'s `render_cargo_toml` keeps it only for a file naming that
/// crate): [`TemplateKind::Strategy`]'s walk starts from the template without it,
/// [`TemplateKind::StrategyOnnx`]'s with it.
pub const ONNX_DEPENDENCY_LINE: &str = "vike-ml-onnx-prediction =";

#[path = "../build/source_hash.rs"]
// `allow`, not `expect`: the walk's other readers (build.rs, xtask, `tests/source_hash.rs`) use the items
// this module leaves unread.
#[allow(dead_code, clippy::allow_attributes)]
mod source_hash;

use source_hash::HashForm;

pub mod rename_table;

/// The SDK a build compiles against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Sdk {
    /// A numbered, frozen snapshot under [`SDK_SOURCE_DIR`].
    Number(u32),
    /// A git checkout's live tree ([`is_dev_checkout`]): its hashes are computed at each build, and a
    /// file's pin is ignored.
    Dev,
}

impl Sdk {
    /// The folder this SDK's source and its builds are kept in: `sdk-N`, or `sdk-dev`.
    pub fn directory_name(self) -> String {
        match self {
            Sdk::Number(number) => format!("sdk-{number}"),
            Sdk::Dev => "sdk-dev".to_string(),
        }
    }

    /// The SDK a folder named by [`Sdk::directory_name`] holds; `None` for any other name (`sdk-03` too,
    /// so one SDK has one folder).
    pub fn of_directory_name(name: &str) -> Option<Sdk> {
        let rest = name.strip_prefix("sdk-")?;
        if rest == "dev" {
            return Some(Sdk::Dev);
        }
        let number: u32 = rest.parse().ok()?;
        (number.to_string() == rest).then_some(Sdk::Number(number))
    }
}

impl fmt::Display for Sdk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Sdk::Number(number) => write!(f, "SDK {number}"),
            Sdk::Dev => f.write_str("SDK dev"),
        }
    }
}

/// The template an artifact is built from; a snapshot records one source hash per kind
/// ([`SdkSourceHashes`]), so a study-only crate never moves a strategy's build folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TemplateKind {
    /// A strategy file that names no ONNX runtime: the strategy template without [`ONNX_DEPENDENCY_LINE`].
    Strategy,
    /// A strategy file that names the ONNX runtime's crate: the strategy template whole.
    StrategyOnnx,
    /// A research study file: the study template.
    Study,
}

impl TemplateKind {
    /// Every kind, in the marker's order.
    pub const ALL: [TemplateKind; 3] =
        [TemplateKind::Strategy, TemplateKind::StrategyOnnx, TemplateKind::Study];

    /// The kind's key in the marker's `[source_hash]` table.
    pub fn name(self) -> &'static str {
        match self {
            TemplateKind::Strategy => "strategy",
            TemplateKind::StrategyOnnx => "onnx",
            TemplateKind::Study => "study",
        }
    }

    /// Appended to the kind's work directory's name (`crate::artifact::work_directory`): each kind's
    /// closure differs, so each keeps its own `target/`.
    pub fn work_suffix(self) -> &'static str {
        match self {
            TemplateKind::Strategy => "",
            TemplateKind::StrategyOnnx => "-onnx",
            TemplateKind::Study => "-study",
        }
    }

    /// The template manifest(s) the kind's source walk starts from, given the tree's two template
    /// manifests' text.
    fn manifests(self, strategy: &str, study: &str) -> String {
        match self {
            TemplateKind::Strategy => strategy
                .split_inclusive('\n')
                .filter(|line| !line.starts_with(ONNX_DEPENDENCY_LINE))
                .collect(),
            TemplateKind::StrategyOnnx => strategy.to_string(),
            TemplateKind::Study => study.to_string(),
        }
    }
}

/// The source hash of the tree at `root` (a snapshot or a checkout) for `kind`: the walk the build folder
/// has always been named by (`build/source_hash.rs`'s `source_hash`, FNV-1a 64, the same under every
/// toolchain), started from that tree's OWN templates, each source read as its tokens (a comment moves it
/// not). Sixteen lowercase hex digits. Reads and lexes about 13 MB of text.
pub fn source_hash(root: &Path, kind: TemplateKind) -> String {
    source_hash_in(root, kind, HashForm::Tokens)
}

/// [`source_hash`], each file read in `form`.
fn source_hash_in(root: &Path, kind: TemplateKind, form: HashForm) -> String {
    let template = |relative: &str| {
        fs::read_to_string(root.join(RUST_STRATEGY_CRATE_DIR).join(relative)).unwrap_or_default()
    };
    let [strategy, study] = source_hash::TEMPLATE_MANIFESTS.map(template);
    source_hash::source_hash(root, &[kind.manifests(&strategy, &study).as_str()], form).hash
}

/// The crates a `kind` build of the tree at `root` compiles, root-relative (`crates/vike-model`): its
/// template's `path` dependencies and theirs, transitively, read off the manifests alone (the walk
/// [`source_hash`] hashes). What a snapshot of that tree holds beside the root manifest and lock.
pub fn closure(root: &Path, kind: TemplateKind) -> Vec<String> {
    let template = |relative: &str| {
        fs::read_to_string(root.join(RUST_STRATEGY_CRATE_DIR).join(relative)).unwrap_or_default()
    };
    let [strategy, study] = source_hash::TEMPLATE_MANIFESTS.map(template);
    source_hash::closure(root, &[kind.manifests(&strategy, &study).as_str()])
}

/// The boundary hash of the tree at `root`: of its `vike-rust-strategy` crate, as [`BOUNDARY_HASH`] is of
/// this binary's.
pub fn boundary_hash(root: &Path) -> String {
    boundary_hash_in(root, HashForm::Tokens)
}

/// [`boundary_hash`], each file read in `form`.
fn boundary_hash_in(root: &Path, form: HashForm) -> String {
    source_hash::boundary_hash(&root.join(RUST_STRATEGY_CRATE_DIR), form).hash
}

/// A Rust source `text` as the hashes read it: its tokens, every comment and the layout dropped
/// (`build/tokens.rs`); the text itself when it does not lex. What a build folder's RECIPE hashes of the
/// code the builder writes around a file, so a comment there moves no folder either.
pub fn rust_tokens(text: &str) -> String {
    source_hash::tokens::rust_tokens(text).unwrap_or_else(|| text.to_string())
}

/// A TOML manifest `text` as the hashes read it: its comments and layout dropped (`build/tokens.rs`).
pub fn toml_tokens(text: &str) -> String {
    source_hash::tokens::toml_tokens(text)
}

/// Whether `root` is a git checkout, "SDK dev" ([`Sdk::Dev`]): a dev box's, a test's or a lane's tree,
/// never an installed snapshot (a snapshot is unpacked from an archive and carries no `.git`).
pub fn is_dev_checkout(root: &Path) -> bool {
    root.join(".git").exists()
}

/// The snapshot folder of SDK `number` under `sdk_root` (`<project>/`[`SDK_SOURCE_DIR`]).
pub fn snapshot_dir(sdk_root: &Path, number: u32) -> PathBuf {
    sdk_root.join(Sdk::Number(number).directory_name())
}

/// The numbers of the SDKs installed under `sdk_root`, ascending: every `sdk-N` folder holding a marker
/// file. A missing `sdk_root` is none; one that cannot be read is an `Err` in words.
pub fn installed(sdk_root: &Path) -> Result<Vec<u32>, String> {
    let entries = match fs::read_dir(sdk_root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("cannot list the SDKs under {}: {e}", sdk_root.display())),
    };
    let mut numbers: Vec<u32> = entries
        .flatten()
        .filter(|entry| entry.path().join(SDK_MARKER_FILE).is_file())
        .filter_map(|entry| match Sdk::of_directory_name(entry.file_name().to_str()?)? {
            Sdk::Number(number) => Some(number),
            Sdk::Dev => None,
        })
        .collect();
    numbers.sort_unstable();
    Ok(numbers)
}

/// The per-kind source hashes a marker records, the `[source_hash]` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SdkSourceHashes {
    /// [`TemplateKind::Strategy`]'s.
    pub strategy: String,
    /// [`TemplateKind::StrategyOnnx`]'s.
    pub onnx: String,
    /// [`TemplateKind::Study`]'s.
    pub study: String,
}

impl SdkSourceHashes {
    /// Every kind's [`source_hash`] of the tree at `root`.
    pub fn of_tree(root: &Path) -> SdkSourceHashes {
        let [strategy, onnx, study] = TemplateKind::ALL.map(|kind| source_hash(root, kind));
        SdkSourceHashes { strategy, onnx, study }
    }

    /// The hash recorded for `kind`.
    pub fn of(&self, kind: TemplateKind) -> &str {
        match kind {
            TemplateKind::Strategy => &self.strategy,
            TemplateKind::StrategyOnnx => &self.onnx,
            TemplateKind::Study => &self.study,
        }
    }
}

/// A snapshot's MARKER, the TOML file `sdk-N/`[`SDK_MARKER_FILE`] (it replaces the `SOURCE_GIT_SHA`
/// stamp): which SDK the snapshot is, the commit it was cut at, its per-kind source hashes, its boundary
/// hash and its interface version. Written once when the snapshot is installed ([`SdkMarker::write`]),
/// checked against the snapshot's own bytes at the builder's start ([`SdkMarker::verify`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SdkMarker {
    /// N.
    pub number: u32,
    /// The commit tagged `sdk-N`.
    pub commit: String,
    /// The snapshot's `ABI_VERSION`.
    pub abi_version: u32,
    /// The snapshot's [`boundary_hash`].
    pub boundary_hash: String,
    /// The snapshot's [`source_hash`] per kind.
    pub source_hash: SdkSourceHashes,
}

impl SdkMarker {
    /// The marker of the snapshot at `root`, SDK `number` cut at `commit` with interface version
    /// `abi_version` (the cutting binary's `crate::abi::ABI_VERSION`, built from that commit): its
    /// hashes are computed over the snapshot's own bytes, so [`SdkMarker::verify`] of the same tree holds.
    pub fn of_snapshot(root: &Path, number: u32, commit: &str, abi_version: u32) -> SdkMarker {
        SdkMarker {
            number,
            commit: commit.to_string(),
            abi_version,
            boundary_hash: boundary_hash(root),
            source_hash: SdkSourceHashes::of_tree(root),
        }
    }

    /// The marker at `snapshot`'s root, parsed; `Err` in words for an absent, unreadable or malformed one.
    pub fn read(snapshot: &Path) -> Result<SdkMarker, String> {
        let path = snapshot.join(SDK_MARKER_FILE);
        let text = fs::read_to_string(&path).map_err(|e| {
            format!(
                "{} is not an installed SDK: its marker {} cannot be read ({e})",
                snapshot.display(),
                path.display()
            )
        })?;
        toml::from_str(&text)
            .map_err(|e| format!("the SDK marker {} does not parse: {e}", path.display()))
    }

    /// The marker's text, as [`SdkMarker::write`] writes it.
    pub fn to_toml(&self) -> String {
        let body = toml::to_string(self).expect("the SDK marker is plain strings and integers");
        format!(
            "# The versioned strategy SDK's marker (decision 0296). Written once; never edit it.\n{body}"
        )
    }

    /// Write the marker at `snapshot`'s root, once. An existing marker with the same content is left
    /// untouched (no mtime moves); one that differs is REFUSED, both named: an installed SDK never
    /// changes (decision 0296, verdict 1).
    pub fn write(&self, snapshot: &Path) -> Result<(), String> {
        let path = snapshot.join(SDK_MARKER_FILE);
        match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => file
                .write_all(self.to_toml().as_bytes())
                .map_err(|e| format!("writing the SDK marker {}: {e}", path.display())),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                let installed = SdkMarker::read(snapshot)?;
                if installed == *self {
                    Ok(())
                } else {
                    Err(format!(
                        "{} already holds {} with another marker ({installed:?}), and an installed SDK never \
                         changes: refusing to write {self:?}. Cut a new SDK number instead.",
                        snapshot.display(),
                        Sdk::Number(installed.number),
                    ))
                }
            }
            Err(e) => Err(format!("creating the SDK marker {}: {e}", path.display())),
        }
    }

    /// Check the snapshot at `snapshot` against this marker: recompute every kind's [`source_hash`] and
    /// the [`boundary_hash`] over its files and refuse, in words, a snapshot whose sources moved since the
    /// marker was written (a hand edit, a partial unpack, a copy from another SDK). A marker written since
    /// 2026-11-20 records the hashes of the tokens; one written before (SDK 1's) of the bytes, and is
    /// checked in that form: an SDK's marker never changes.
    pub fn verify(&self, snapshot: &Path) -> Result<(), String> {
        let sdk = Sdk::Number(self.number);
        let moved = self.moved(snapshot, HashForm::Tokens);
        if moved.is_empty() || self.moved(snapshot, HashForm::Bytes).is_empty() {
            return Ok(());
        }
        Err(format!(
            "{sdk} at {} is not the snapshot its marker describes ({}): an installed SDK never changes, \
             so its files were edited or only partly unpacked. Install it again, which REPLACES it: \
             `scripts/fetch_release_tools.sh strategy-source --sdk {n}` packs it from its tag, or \
             `bin/vikectl add-sdk <archive>` takes the vike-strategy-source.tar.gz of a release whose \
             newest SDK is {n}; a deploy of such a release does it too.",
            snapshot.display(),
            moved.join("; "),
            n = self.number
        ))
    }

    /// What of the snapshot at `snapshot`, read in `form`, differs from this marker, in words.
    fn moved(&self, snapshot: &Path, form: HashForm) -> Vec<String> {
        let mut moved = Vec::new();
        for kind in TemplateKind::ALL {
            let now = source_hash_in(snapshot, kind, form);
            if now != self.source_hash.of(kind) {
                moved.push(format!(
                    "the {} source hash is {now}, its marker says {}",
                    kind.name(),
                    self.source_hash.of(kind)
                ));
            }
        }
        let now = boundary_hash_in(snapshot, form);
        if now != self.boundary_hash {
            moved.push(format!(
                "the boundary hash is {now}, its marker says {}",
                self.boundary_hash
            ));
        }
        moved
    }
}

/// The folder of the `vike-rust-strategy` crate that holds the ADAPTERS (decision 0296's step 6): one
/// `sdk-N/` each, holding its marker [`ADAPTER_FILE`] and its face [`ADAPTER_FACE_FILE`]. A vike takes its
/// own, compiled in ([`built_in_adapters`]), never an installed SDK's.
pub const ADAPTERS_DIR: &str = "adapters";

/// An adapter's marker at its folder's root: `adapters/sdk-N/ADAPTER` ([`SdkAdapter`]).
pub const ADAPTER_FILE: &str = "ADAPTER";

/// An adapter's face: `adapters/sdk-N/face.rs`, what stands for `src/guest/face.rs` when this boundary's
/// guest glue is compiled against SDK N's crates (`crate::guest`'s `face`).
pub const ADAPTER_FACE_FILE: &str = "face.rs";

/// **An adapter** (decision 0296, verdict 7 and step 6): what lets a file pinned to SDK `number`, cut with
/// another boundary than this host's, BUILD against this host's boundary. Its marker, the TOML file
/// `adapters/sdk-N/ADAPTER`, names the SDK and the boundary it adapts FROM; its face beside it is this
/// boundary's guest glue as that SDK's API sees it. The PR that moves the boundary adds one for every SDK
/// inside the window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SdkAdapter {
    /// The SDK it adapts.
    pub number: u32,
    /// That SDK's boundary hash as THIS vike reads it ([`boundary_hash`] of its tree at its tag): [`usable`]
    /// compares it with the installed snapshot's, recomputed, so a marker written by an older walk (SDK 1's,
    /// of its bytes) still finds its adapter.
    pub boundary_hash: String,
    /// The face's text, [`ADAPTER_FACE_FILE`] (not in the marker).
    #[serde(skip)]
    pub face: String,
}

impl SdkAdapter {
    /// The folder of SDK `number`'s adapter in the tree at `root`.
    pub fn dir(root: &Path, number: u32) -> PathBuf {
        root.join(RUST_STRATEGY_CRATE_DIR)
            .join(ADAPTERS_DIR)
            .join(Sdk::Number(number).directory_name())
    }
}

/// What a tree's adapters folder holds ([`adapters`]): the adapters that read, ascending, and why each other
/// one did not. One broken adapter refuses only the SDK it was for: the others stay usable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdapterSet {
    /// Every adapter whose marker reads, names its folder's SDK and has its face.
    pub adapters: Vec<SdkAdapter>,
    /// Why each other `adapters/sdk-N/` holding a marker is no adapter (or why the folder cannot be listed).
    pub broken: Vec<String>,
}

/// The adapters the tree at `root` holds: every `adapters/sdk-N/` with a marker. None for a tree without
/// the folder; a marker that does not read, one naming another SDK than its folder, or an adapter without
/// its face is said in [`AdapterSet::broken`], never skipped in silence, and costs only that SDK. A vike
/// serves its own ([`built_in_adapters`]); this reads a checkout's (CI's check of each against its tag).
pub fn adapters(root: &Path) -> AdapterSet {
    let mut set = AdapterSet::default();
    let dir = root.join(RUST_STRATEGY_CRATE_DIR).join(ADAPTERS_DIR);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => return set,
        Err(e) => {
            set.broken.push(format!("cannot list the SDK adapters under {}: {e}", dir.display()));
            return set;
        }
    };
    for entry in entries.flatten() {
        let Some(Sdk::Number(number)) = entry.file_name().to_str().and_then(Sdk::of_directory_name)
        else {
            continue;
        };
        let path = entry.path().join(ADAPTER_FILE);
        if !path.is_file() {
            continue;
        }
        let marker = fs::read_to_string(&path)
            .map_err(|e| format!("the SDK adapter marker {} cannot be read: {e}", path.display()));
        let face = fs::read_to_string(entry.path().join(ADAPTER_FACE_FILE)).ok();
        match marker
            .and_then(|text| adapter_of(number, &text, face, &entry.path().display().to_string()))
        {
            Ok(adapter) => set.adapters.push(adapter),
            Err(why) => set.broken.push(why),
        }
    }
    set.adapters.sort_by_key(|adapter| adapter.number);
    set.broken.sort();
    set
}

/// The adapter of SDK `number` whose marker reads `marker` and whose face reads `face` (`None`: none), at
/// `place` (for the words); why it is none otherwise.
fn adapter_of(
    number: u32,
    marker: &str,
    face: Option<String>,
    place: &str,
) -> Result<SdkAdapter, String> {
    let adapter: SdkAdapter = toml::from_str(marker)
        .map_err(|e| format!("the SDK adapter marker at {place} does not parse: {e}"))?;
    if adapter.number != number {
        return Err(format!(
            "the SDK adapter marker at {place} names SDK {}, and its folder SDK {number}",
            adapter.number
        ));
    }
    let face = face.ok_or_else(|| {
        format!("the adapter of SDK {number} at {place} has no {ADAPTER_FACE_FILE}")
    })?;
    Ok(SdkAdapter { face, ..adapter })
}

/// The adapters this binary was built with, each `adapters/sdk-N/`'s name, marker and face (`None`:
/// none), compiled in by `build.rs`'s `write_adapters`.
const COMPILED_IN: &[(&str, &str, Option<&str>)] =
    include!(concat!(env!("OUT_DIR"), "/adapters.rs"));

#[cfg(feature = "test-support")]
thread_local! {
    /// What [`with_adapters`] puts in [`built_in_adapters`]' place on this thread.
    static STAND_IN: std::cell::RefCell<Option<AdapterSet>> = const { std::cell::RefCell::new(None) };
}

/// **This vike's adapters**: the ones its crate held when it was built, compiled in (`build.rs`), never an
/// installed SDK's (decision 0296's amendment of 2026-11-20). An adapter's fix reaches a box with any deploy,
/// takes no SDK number, and rebuilds only the files pinned to the SDK it adapts (it is in their folder's
/// recipe alone). A broken one is said in [`AdapterSet::broken`] and costs only its SDK.
pub fn built_in_adapters() -> AdapterSet {
    #[cfg(feature = "test-support")]
    {
        if let Some(set) = STAND_IN.with_borrow(Clone::clone) {
            return set;
        }
    }
    let mut set = AdapterSet::default();
    for &(folder, marker, face) in COMPILED_IN {
        let Some(Sdk::Number(number)) = Sdk::of_directory_name(folder) else { continue };
        let place = format!("{RUST_STRATEGY_CRATE_DIR}/{ADAPTERS_DIR}/{folder} (built in)");
        match adapter_of(number, marker, face.map(str::to_string), &place) {
            Ok(adapter) => set.adapters.push(adapter),
            Err(why) => set.broken.push(why),
        }
    }
    set.adapters.sort_by_key(|adapter| adapter.number);
    set
}

/// Run `f` with `set` standing for this binary's adapters ([`built_in_adapters`]) on this thread: how a test
/// serves an SDK it made up through an adapter it made up. Never a vike's.
#[cfg(feature = "test-support")]
pub fn with_adapters<T>(set: AdapterSet, f: impl FnOnce() -> T) -> T {
    let before = STAND_IN.replace(Some(set));
    let out = f();
    STAND_IN.set(before);
    out
}

/// **The tree that holds THIS host's boundary** among the SDKs installed under `sdk_root`: the newest whose
/// marker records [`BOUNDARY_HASH`]. A deploy that moves the boundary cuts an SDK first
/// (`docs/ops/sdk-release.md`), so on a deployed box it is the newest. Its boundary's sources are what an
/// older SDK's build compiles as its glue, with this vike's adapter's face ([`built_in_adapters`]). Only a tree whose
/// bytes are the ones its marker describes ([`SdkMarker::verify`], a few milliseconds): a half-unpacked one
/// is passed over. `None` when no installed SDK holds it (then no older boundary is adapted).
pub fn host_tree(sdk_root: &Path) -> Option<PathBuf> {
    installed(sdk_root).ok()?.into_iter().rev().map(|number| snapshot_dir(sdk_root, number)).find(
        |dir| {
            SdkMarker::read(dir).is_ok_and(|marker| {
                marker.boundary_hash == BOUNDARY_HASH && marker.verify(dir).is_ok()
            })
        },
    )
}

/// The adapters this host can build through: its own ([`built_in_adapters`]) where an installed SDK holds
/// its boundary ([`host_tree`], the glue's source), none where none does.
pub fn host_adapters(sdk_root: &Path) -> AdapterSet {
    host_tree(sdk_root).map(|_| built_in_adapters()).unwrap_or_default()
}

/// Where a source tree's `vike-rust-strategy` keeps the layout of every row the boundary lends in place:
/// each type's size and alignment, then every field's type and offset, in declaration order
/// (`tests/boundary.rs` holds it to the compiled types, so CI keeps it true at every cut): an SDK's layout
/// of its lent rows, read without compiling it. A golden cut before 2026-11-20 (SDK 1's) holds the field
/// types alone.
pub const LENT_FIELDS_FILE: &str = "tests/boundary/lent_fields.txt";

/// THIS boundary's [`LENT_FIELDS_FILE`]: the layout of the rows this host lends.
pub const LENT_FIELDS: &str = include_str!("../tests/boundary/lent_fields.txt");

/// The lines of a [`LENT_FIELDS_FILE`] that describe an SDK's own types (`Bar: 120 bytes, align 8`,
/// `Bar.timestamp: i64 @ 0`), not the boundary's (`OrderListLegAbi…`, whose layout the glue always takes
/// from the host).
fn sdk_lent_fields(golden: &str) -> Vec<&str> {
    golden
        .lines()
        .filter(|line| line.split(['.', ':']).next().is_some_and(|owner| !owner.ends_with("Abi")))
        .collect()
}

/// Whether the SDK lent-field golden `golden` lays its rows out as this host's [`LENT_FIELDS`] does. A
/// golden of sizes and offsets compares whole; one of the field types alone (cut before the offsets were
/// recorded) compares on the types and their order, and the glue's compile-time assertions hold the sizes
/// and offsets (`crate::abi`'s `lent_in_place!`: such a build fails in words, never misreads a row).
fn same_lent_rows(golden: &str) -> bool {
    let (sdk, host) = (sdk_lent_fields(golden), sdk_lent_fields(LENT_FIELDS));
    if sdk.iter().any(|line| line.contains(" @ ")) {
        return sdk == host;
    }
    let types = host.iter().filter_map(|line| line.rsplit_once(" @ ").map(|(field, _)| field));
    sdk.into_iter().eq(types)
}

/// How an SDK [`usable`] on this host builds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SdkUse {
    /// It was cut with this host's boundary: built as it is.
    SameBoundary,
    /// It was cut with another boundary: built against this host's through this adapter.
    Adapted(SdkAdapter),
}

/// Why an SDK cannot be built against on this host ([`usable`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SdkUnusable {
    /// The SDK was cut with another strategy boundary than this host's, and this host holds no adapter
    /// from it.
    BoundaryMoved {
        /// The SDK's number.
        number: u32,
        /// The newest SDK on the host, the pin the fix moves to.
        newest: u32,
        /// The boundary hash its marker records.
        sdk: String,
        /// This host's [`BOUNDARY_HASH`].
        host: String,
    },
    /// The SDK is older than the support window ([`SDK_SUPPORT_WINDOW`]).
    Retired {
        /// The SDK's number.
        number: u32,
        /// The newest SDK on the host.
        newest: u32,
    },
    /// The SDK lays out a row this host lends in place otherwise than this host does (its
    /// [`LENT_FIELDS_FILE`] differs, or cannot be read): retired, whatever adapter exists (decision 0296,
    /// verdict 7; the owner's "Б" of 2026-11-20).
    RowsMoved {
        /// The SDK's number.
        number: u32,
        /// The newest SDK on the host.
        newest: u32,
    },
}

impl fmt::Display for SdkUnusable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SdkUnusable::BoundaryMoved { number, newest, sdk, host } => write!(
                f,
                "SDK {number} was cut with another strategy boundary ({sdk}) than this vike's ({host}), and \
                 this vike holds no adapter from it, so it cannot build against this boundary: move the \
                 file to SDK {newest} (`vike-cli strategy migrate <FILE> --to {newest}`)"
            ),
            SdkUnusable::Retired { number, newest } => write!(
                f,
                "SDK {number} is retired on this vike, which builds SDK {} to {newest} (the newest and the \
                 {SDK_SUPPORT_WINDOW} before it): move the file to SDK {newest} \
                 (`vike-cli strategy migrate <FILE> --to {newest}`)",
                newest.saturating_sub(SDK_SUPPORT_WINDOW)
            ),
            SdkUnusable::RowsMoved { number, newest } => write!(
                f,
                "SDK {number} is retired on this vike: it lays out the bars, closed rows or ticks this vike \
                 lends in place otherwise than this vike does (its `{RUST_STRATEGY_CRATE_DIR}/{LENT_FIELDS_FILE}` \
                 differs from this vike's), and such a change retires the older SDKs (decision 0296, verdict \
                 7): move the file to SDK {newest} (`vike-cli strategy migrate <FILE> --to {newest}`)"
            ),
        }
    }
}

impl std::error::Error for SdkUnusable {}

/// **THE rule for whether an SDK is usable on this host**, and the only place it is written: the SDK
/// whose marker is `marker` and whose snapshot is `snapshot`, on a host whose newest SDK is `newest` and
/// whose adapters are `adapters` ([`host_adapters`]), builds when
/// 1. it is `newest` or one of the [`SDK_SUPPORT_WINDOW`] before it (decision 0296, verdict 5), and
/// 2. its `boundary_hash` equals this host's [`BOUNDARY_HASH`] ([`SdkUse::SameBoundary`]), or
/// 3. this host holds an adapter from it, one naming its number and its boundary (the snapshot's
///    [`boundary_hash`], recomputed: the marker's may be an older walk's), and it lays out every row
///    this host lends in place as this host does (its [`LENT_FIELDS_FILE`]'s lines of the SDK's own types
///    agree with this host's [`LENT_FIELDS`]', `same_lent_rows`):[`SdkUse::Adapted`], verdict 7 and step 6, the file builds
///    against this host's boundary through that adapter, compiled into it. With no adapter it is
///    [`SdkUnusable::BoundaryMoved`]; with one and its rows moved, [`SdkUnusable::RowsMoved`].
///
/// A number above `newest` passes the window (the caller named the wrong newest; the boundary still
/// decides). Outside the window an SDK is retired whatever adapter exists, and so is one whose lent rows
/// moved ([`SdkUnusable::RowsMoved`]: verdict 7, the owner's "Б" of 2026-11-20; MEASURED, a converted row
/// cost about 220 ns a bar). The golden's types are `std::any::type_name`'s words, so a toolchain that
/// spells one otherwise retires the older SDKs too: too much refused, never a row misread.
pub fn usable(
    marker: &SdkMarker,
    snapshot: &Path,
    newest: u32,
    adapters: &[SdkAdapter],
) -> Result<SdkUse, SdkUnusable> {
    let number = marker.number;
    if number.saturating_add(SDK_SUPPORT_WINDOW) < newest {
        return Err(SdkUnusable::Retired { number, newest });
    }
    if marker.boundary_hash == BOUNDARY_HASH {
        return Ok(SdkUse::SameBoundary);
    }
    let boundary = boundary_hash(snapshot);
    let adapter = adapters
        .iter()
        .find(|adapter| adapter.number == number && adapter.boundary_hash == boundary)
        .ok_or_else(|| SdkUnusable::BoundaryMoved {
            number,
            newest,
            sdk: marker.boundary_hash.clone(),
            host: BOUNDARY_HASH.to_string(),
        })?;
    // An adapter is held: it carries a call's difference, never a lent row's (verdict 7).
    let golden =
        fs::read_to_string(snapshot.join(RUST_STRATEGY_CRATE_DIR).join(LENT_FIELDS_FILE)).ok();
    if !golden.as_deref().is_some_and(same_lent_rows) {
        return Err(SdkUnusable::RowsMoved { number, newest });
    }
    Ok(SdkUse::Adapted(adapter.clone()))
}
