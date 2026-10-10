//! Where a hist store LIVES when nobody said — the one precedence every binary resolves through.
//!
//! Ported from nothing: net-new Rust surface, born from a the CI box cleanup (2026-08-02) that found six
//! `DataFusionHist` stores scattered over three directories on two disks, created ad hoc by separate
//! sessions and referenced by nothing. The tools had no opinion about where data goes, so every run
//! put it somewhere new.
//!
//! # Why the old defaults were wrong for anyone but this repo
//!
//! Each consumer had its own final fallback, and two of them cannot work for an INSTALLED binary:
//!
//! - `vike-backtest` / `vike-app` fell back to `CARGO_MANIFEST_DIR/../../market_data/hist` — a path baked in
//!   at COMPILE time, i.e. a directory on the machine that built the binary. On anyone else's box it
//!   names something that does not exist.
//! - `vike-datahub` fell back to the literal `"market_data/hist"`, resolved against the CURRENT WORKING
//!   DIRECTORY — so it silently creates an empty store wherever the user happened to stand, and the
//!   run reports zero data instead of failing.
//!
//! # The precedence
//!
//! 1. `explicit` — `--store`, or a profile's `store =`. Always wins: it was typed for THIS run, and
//!    nothing inferred may outrank something stated.
//! 2. `$VIKE_HIST_STORE` — the operator's standing answer for this box, stated once instead of on
//!    every command line. Above every default for the same reason: stated, not inferred.
//! 3. [`ProjectDefault::Declared`] — **`<project>/market_data/hist` for a project named OUTRIGHT by
//!    `$VIKE_SETTINGS_DIR`.** Same path expression as rung 5, and above the dev-checkout hinge for
//!    the reason rungs 1 and 2 are above it: an operator who sets that variable has STATED which
//!    project this is, while the hinge below only ever INFERS one. See "the two project rungs".
//! 4. `repo_default` **if the CHECKOUT that built this binary still EXISTS** — the dev-checkout
//!    hinge. It outranks the DISCOVERED project rung below deliberately: a developer's existing
//!    `<repo>/market_data/hist` must not silently relocate the day a nearer rung is added, because a store
//!    that moves does not merge — the old one simply stops being read and the run reports zero rows
//!    instead of failing. See the comment on the probe for why it tests the repo ROOT, not the leaf,
//!    and [`REPO_MARKER`] for why the root must carry a MANIFEST rather than merely exist.
//!    ⚠ **`None` in a build that has no checkout to name, and the rung is then simply ABSENT.**
//!    The argument is a compile-time path (`env!("CARGO_MANIFEST_DIR")` at the caller), and a
//!    compile-time path is a string literal in the binary: the `--release` builds `release.yml`
//!    ships carried the runner's checkout path — `/home/<runner account>/…` — in every asset that
//!    called this, which is the runner account's name on a PUBLIC download and, on the deploy box,
//!    a hinge that FIRES for the runner's own checkout (the box that builds is the box that
//!    deploys, so `is_repo` was true there for a tree that was never the operator's). So the
//!    shipped call sites pass the rung only under `cfg(debug_assertions)`; a release build passes
//!    `None` and resolves from rung 5 down, which for anyone standing in the checkout is the same
//!    directory by a different rung name. `scripts/refuse_box_paths.sh` is what refuses the
//!    literal if a site ever forgets.
//! 5. [`ProjectDefault::Discovered`] — **`<project>/market_data/hist`**, the project the working directory
//!    sits in, found by walking up through
//!    [`crate::paths::state_path::project_hist_store_dir_from`]. The default a
//!    fresh install gets, and the owner's decision it implements: all data in ONE folder INSIDE the
//!    project — one folder to move to another disk, back up or delete, with no second location to
//!    remember (the shape Freqtrade's `user_data/data/`, OctoBot and Hummingbot all ship). It is
//!    found by the SAME walk that finds `settings/`, so settings, credentials and data can never
//!    disagree about which project this is. [`ProjectDefault::None`] when no project sits above the
//!    working directory — a bare binary run from `/tmp` — and only then:
//! 6. `user_default` — the per-user data dir ([`user_data_dir`]), `…/vike-data`. Kept rather than
//!    deleted because a binary with NO project above it must still answer with a writable, stable,
//!    per-user location rather than inventing one from the CWD. What it GUARANTEES is exactly that
//!    and no more: a path that does not depend on where the process was launched from, on a
//!    filesystem the user can write. It guarantees nothing about persistence — see the container
//!    section below, where that is the whole problem.
//!    ⚠ This bullet used to corroborate itself with a live box ("the layout the CI box already uses —
//!    `tape/` live, `archive/` shared datasets — reached there through rung 1 or 2"). Measured
//!    2026-08-23: that directory does not exist, that machine's store is a `<project>/market_data/hist`
//!    named outright by the recorder's profile, and the `tape` leaf is not even the name
//!    [`crate::paths::state_path::HIST_SUBDIR`] uses. **A module doc may not cite a specific machine's
//!    on-disk layout**: nothing in this repository can re-derive it, so it rots in total silence and
//!    is then quoted as evidence for a design it never supported. State what the rung guarantees;
//!    leave the box out of it.
//! 7. `repo_default` again, as a last resort, so this function is total and never invents a path from
//!    the CWD. ⚠ A build with NO checkout rung (`repo_default = None`) has nothing compile-time to
//!    fall back on, so its last resort is `market_data/hist` under the WORKING DIRECTORY — the one
//!    rung of this ladder that is CWD-relative, reached only with no stated store, no project
//!    above the working directory, no per-user directory and no checkout, and logged as
//!    [`StoreRootRung::LastResort`] so it is never mistaken for an answer. The alternative was a
//!    path on the machine that built the binary, which is not a better guess — it is a worse one
//!    that also names the box.
//!
//! ⚠ **The project rung is the only place the answer has ever CHANGED, and only for a box with no
//! checkout on it.** Before it existed, a DEPLOYED binary fell through to the per-user directory —
//! so an operator who had installed everything in one folder found the tape under `$HOME` instead: a
//! second location, outside the folder they were told is theirs, on whatever filesystem `$HOME`
//! happens to be. The dev-checkout hinge keeps every dev checkout byte-identical and rungs 1–2 keep
//! every explicit answer untouched, so nothing that already names a store moves.
//!
//! # THE TWO PROJECT RUNGS — a DECLARATION is not a GUESS, and they sit on opposite sides of a hinge
//!
//! Rungs 3 and 5 join the same path expression and differ ONLY in PROVENANCE — [`ProjectDefault`]
//! carries that difference, because an `Option<PathBuf>` cannot: it says WHERE the store would go
//! and nothing about who decided.
//!
//! The ordering defect that forced the split: the dev-checkout hinge is the program's own INFERENCE
//! that the source tree it was compiled in still happens to exist at the path baked into this
//! binary. An operator setting `$VIKE_SETTINGS_DIR` is STATING which project this is. With one
//! undifferentiated project rung below the hinge, **the guess outranked the declaration** — and it
//! made `docs/decisions/0026-containerisation-additive-backend-image.md` false as written, since
//! that record tells a container operator to "name `VIKE_SETTINGS_DIR` outright" as THE hatch that
//! beats the walk. It beat the walk for settings, credentials and state; for the STORE it lost to a
//! runtime image that still carried a source tree at the builder's path, and the tape went to the
//! ephemeral layer while every other project path went to the mount.
//!
//! What each side buys, and why neither may move:
//!
//! * **Declared, ABOVE the hinge.** Nothing else in the ladder can express "this project, whatever
//!   this box happens to have on it". The variable already relocates settings, credentials and
//!   state; the store now travels with them under exactly the same statement.
//! * **Discovered, BELOW the hinge.** A developer sets no variable, so their `<repo>/market_data/hist`
//!   still wins and nothing relocates — the whole reason the hinge was kept when the project rung
//!   landed. A walk is evidence about the working directory, which is inherited rather than chosen,
//!   and it must not silently move a populated store.
//!
//! ⚠ **A blank or whitespace-only value is NOT a declaration** and falls through to the walk — the
//! workspace's standing rule for this variable ([`crate::paths::state_path::project_settings_dir_from`]
//! ignores a blank override rather than honouring it), applied here so the two cannot disagree. An
//! empty `Environment=VIKE_SETTINGS_DIR=` line in a unit file is the shape that matters, and
//! promoting one above the dev-checkout hinge would relocate a developer's store on the strength of
//! a variable that configures nothing. [`project_default_from`] is the one site that decides.
//!
//! ⚠ **One residual, declared rather than fixed**: [`resolve_store_root_from`] computes the project
//! rung under `cwd.and_then(…)`, so a binary that cannot read its own working directory reaches
//! NEITHER project rung — and a declaration set on such a process is therefore still lost to the
//! hinge. The walk genuinely needs a starting directory; the declaration does not, and bypassing the
//! guard for it alone would rest on `project_hist_store_dir_from` ignoring its `start` argument
//! whenever the override is non-blank — a cross-module invariant nothing here can check. Left as it
//! was, because `std::env::current_dir()` fails only on a deleted or unreadable CWD; `$VIKE_HIST_STORE`
//! pins the store outright for anyone who hits it.
//!
//! Every function here is PURE — the working directory and the environment both arrive as
//! parameters and neither is READ here, per the rule the settings registry enforces (libraries take
//! configuration as parameters; only binaries read the process environment). [`resolve_store_root_from`]
//! does look two things up IN the map it is handed (`VIKE_SETTINGS_DIR` and the platform trio) and
//! calls the [`crate::paths::state_path`] walk, which are both pure functions of their arguments; the
//! callers still do the `env::var` sweep and the `current_dir()`.
//!
//! # The resolution is OBSERVABLE, and that is not decoration
//!
//! Every answer comes back as a [`StoreRoot`] — the path AND the [`StoreRootRung`] that chose it —
//! and the binaries log both at startup. A store does not merge: if the answer moves, the old store
//! is simply no longer read and the run reports **zero rows**, which is indistinguishable from "the
//! backfill found nothing". Rung 3 exists precisely because that failure is silent, and it protects
//! DEVELOPERS; an operator, who has no checkout and therefore no rung 3, got nothing. One `info!`
//! naming the root and the rung is what turns "why is my tape empty" from an investigation into a
//! line in the log. [`StoreRootRung::why`] is the operator-facing sentence.
//!
//! ⚠ This module logs NOTHING itself — `vike-model` carries no logging dependency and is not
//! getting one for this. The rung travels as DATA and the CALLER logs it, the same shape as
//! `vike_secrets`' permission finding and `vike-cli`'s `settings_warning_lines`.
//!
//! # `$VIKE_SETTINGS_DIR` moves the DATA root too
//!
//! [`resolve_store_root_from`] resolves both project rungs through
//! [`crate::paths::state_path::project_hist_store_dir_from`], so the one variable that relocates settings,
//! credentials and state relocates the store's default with them — and, since that variable is what
//! separates rung 3 from rung 5, relocates it from a HEIGHT no inference can reach. The alternative —
//! settings from
//! the override, data from the walk — falsifies this change's own premise: an operator who moved
//! their project would read one project's credential store while writing another project's
//! tape. The override's PARENT is the project root; a parentless value
//! (`VIKE_SETTINGS_DIR=settings`) is refused rather than resolved against the CWD, and a settings
//! directory that is genuinely not inside its project is what `$VIKE_HIST_STORE` is for.
//!
//! # ⚠ The DISCOVERED project rung follows the WORKING DIRECTORY, and that is inherited, not new
//!
//! The marker that answers rung 5 is the settings walk's marker, so the same installed binary run
//! from two directories can resolve two different stores. (Rung 3 has no such property — a
//! declaration names the project outright, which is most of why it is a separate rung.) Three things
//! make that the right trade rather than a hole:
//!
//! * **It is strictly less than what already ships.** That binary ALREADY resolves the credential
//!   store, the settings database and `settings/state/` by the same walk from the same working
//!   directory. Data now
//!   shares that property; it does not introduce it. Refusing to inherit it is what would be new —
//!   and it would mean settings resolving to one project while data resolved to another, which is
//!   the disagreement the shared walk was built to remove.
//! * **A daemon does not depend on it.** All three shipped units set both `WorkingDirectory=` and
//!   `VIKE_SETTINGS_DIR=`, and the second now pins the store's project as well, so a systemd run's
//!   answer is stated rather than inferred. `$VIKE_HIST_STORE` pins the store outright for anyone
//!   who wants no project involved at all.
//! * **A move is now visible.** The rung is logged (above), so a store that relocated says so at
//!   startup instead of surfacing as an empty result set later.
//!
//! Narrowing rung 5's marker — "an installed binary needs a `settings/`, a `Cargo.toml` is not
//! enough" — was considered and rejected: it forks the walk, so the two questions ("where are my
//! credentials", "where is my data") could once again answer differently, and it buys nothing a
//! stated `VIKE_SETTINGS_DIR` does not already buy.
//!
//! # ⚠ A CONTAINER is a project whose `<project>` is a BIND MOUNT, and both hinges bite there
//!
//! An image plus a host folder mounted into it is the deployment shape this precedence was not
//! written for, and it fails the same way `<project>` deployments failed before the project rung
//! existed: the
//! store lands somewhere real, writable and DOOMED — the container's ephemeral layer — and the next
//! `docker run` reports zero rows rather than an error, because a store does not merge.
//!
//! Two rungs reach for that layer, and only one of them is a code question:
//!
//! * **Rung 4 no longer fires on a bare directory.** A runtime stage's `WORKDIR /app` CREATES
//!   `/app`, and a builder stage that had the workspace at `/app` baked `/app/market_data/hist` into every
//!   binary it produced — so the two collide with no source tree anywhere on the box, and the old
//!   `is_dir` probe read that empty directory as "the checkout that built this binary is still
//!   here". The probe is [`REPO_MARKER`] now, so an empty collision falls through to the project
//!   (`an_empty_directory_at_the_repo_root_is_not_a_checkout`). ⚠ **The residual this bullet used
//!   to declare — a runtime image that genuinely KEEPS the source tree at the path it was built at,
//!   whose tape then goes to the ephemeral layer "for a reason the log line states accurately" — is
//!   CLOSED for any image that names `$VIKE_SETTINGS_DIR`**, which is the one
//!   `docs/decisions/0026-containerisation-additive-backend-image.md` instructs every image to set.
//!   The declaration is rung 3 and the checkout is rung 4, so the mount wins
//!   (`a_declared_project_outranks_a_dev_checkout`). It survives only for an image that keeps its
//!   source tree AND states nothing — still worth building in a stage you discard.
//! * **Rung 6 is reached exactly when the bind mount carries no project MARKER**, and in a
//!   container `HOME=/root`, so it answers `/root/.local/share/vike-data` — the ephemeral layer
//!   again. That is not a defect in this ladder: the rung is doing the one thing it exists for
//!   (never invent a path from the CWD) and [`StoreRootRung::why`] already names both fixes. The
//!   cure is the same one every shipped systemd unit already applies — `<project>/settings/` must
//!   EXIST inside the mount, or the image must state `$VIKE_SETTINGS_DIR` / `$VIKE_HIST_STORE`.
//!
//! # ⚠ The resolved root must be WRITABLE, and a sandbox is where it is not
//!
//! `DataFusionHist::open` CREATES the root it is given. Under `ProtectSystem=strict` everything is
//! read-only except what `ReadWritePaths=` names, so a unit whose store root is not listed there
//! fails at open with `EROFS`. Resolving a root is not the same as being allowed to write it, and
//! this module deliberately does not probe — a probe would be a TOCTOU guess and would make the
//! pure precedence depend on the filesystem's permissions. `vike-data`'s open path carries the
//! diagnosis instead (it names `ReadWritePaths=` in the error), and
//! `docs/ops/recorder-deploy.md` states the operator rule: the unit's `ReadWritePaths=` and the
//! store root must name the SAME path.
//!
//! # ONE naming site for the platform trio
//!
//! [`user_data_dir_from_vars`] is the MAP-taking twin of [`user_data_dir`], and it is the one place
//! in the workspace that SPELLS `XDG_DATA_HOME` / `HOME` / `LOCALAPPDATA` for the store root. Before
//! it, four callers (`vike-app`, `vike-datahub`, `vike-backtest`'s `binutil`, `vike-backfill`'s
//! `cli`) each pasted the same three `std::env::var` lines to build [`user_data_dir`]'s arguments —
//! identical logic, four chances to diverge, and in the two bin-glue crates those reads sat in a
//! LIBRARY file (four `Layer::Library` rows apiece on the settings registry's STEP-2 work-list).
//! Taking the already-collected environment MAP instead moves the read up to the composition root
//! that already builds one (`std::env::vars().collect()` — the idiom `vike-desktop`, `vike-cli`,
//! `vike-mount` and `vike-tradehub` all use) and leaves exactly one file naming the variables.
//!
//! This function does not read the environment either: it is the same pure resolution with a
//! different argument shape. Both are kept, because a caller that already holds three `Option<&str>`
//! — a test, or a binary that read three specific variables — should not have to build a map to ask
//! the question.
//!
//! ⚠ This is the ONLY consumer of the platform trio left in the workspace. Its sibling
//! [`crate::paths::state_path`] resolves program-written STATE, and does so by WALKING up for the project
//! root rather than by reading a platform variable — the two answer different questions and share
//! no resolution. See that module's doc.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The directory name used under whichever per-user data root the platform provides.
pub const USER_STORE_DIR: &str = "vike-data";

/// The XDG base-directory data root (unix arm of [`user_data_dir`]).
pub const XDG_DATA_HOME_VAR: &str = "XDG_DATA_HOME";

/// The user's home directory — the last-resort root on BOTH platform arms.
pub const HOME_VAR: &str = "HOME";

/// The Windows per-user data root, the platform twin of [`XDG_DATA_HOME_VAR`].
pub const LOCALAPPDATA_VAR: &str = "LOCALAPPDATA";

/// [`user_data_dir`] over an already-collected environment map — the shape a binary's
/// `std::env::vars().collect()` produces. See the module doc for why this exists.
///
/// Byte-identical to `user_data_dir(vars.get("XDG_DATA_HOME"), vars.get("HOME"),
/// vars.get("LOCALAPPDATA"))`: the same three names, in the same order, with the same
/// empty-is-absent handling (which lives in `user_data_dir`, not here).
pub fn user_data_dir_from_vars(vars: &HashMap<String, String>) -> Option<PathBuf> {
    user_data_dir(
        vars.get(XDG_DATA_HOME_VAR).map(String::as_str),
        vars.get(HOME_VAR).map(String::as_str),
        vars.get(LOCALAPPDATA_VAR).map(String::as_str),
    )
}

/// WHICH rung of [`resolve_store_root`] answered — returned as DATA so the CALLER can log it
/// (`vike-model` carries no logging dependency; see the module doc).
///
/// The variants are in precedence order, and that order is asserted by `the_rungs_step_down_in_order`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum StoreRootRung {
    /// Rung 1 — `--store`, a profile's `store =`, the `config.store_root` row.
    Explicit,
    /// Rung 2 — `$VIKE_HIST_STORE` (or a bin's own store variable).
    EnvVar,
    /// Rung 3 — `<project>/market_data/hist` for a project **DECLARED** by `$VIKE_SETTINGS_DIR`.
    ///
    /// Above [`Self::DevCheckout`] because a declaration is not a guess — see the module doc's
    /// "the two project rungs".
    DeclaredProject,
    /// Rung 4 — the source checkout that built this binary still exists on this box.
    DevCheckout,
    /// Rung 5 — `<project>/market_data/hist`, the project the working directory sits in, **DISCOVERED** by
    /// the walk.
    Project,
    /// Rung 6 — the per-user data directory: no project above the working directory.
    UserDir,
    /// Rung 7 — no project and no per-user directory either (a scrubbed environment).
    LastResort,
}

impl StoreRootRung {
    /// A stable machine-readable tag for a structured log field (`rung = "project"`).
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::EnvVar => "env",
            // ⚠ NOT `project-declared`. The tags are grepped, and a `project`-prefixed spelling
            // would make `rung=project` match BOTH project rungs — the one distinction this tag
            // exists to draw.
            Self::DeclaredProject => "declared-project",
            Self::DevCheckout => "dev-checkout",
            Self::Project => "project",
            Self::UserDir => "user-dir",
            Self::LastResort => "last-resort",
        }
    }

    /// The operator-facing sentence: WHY this rung answered, and — for the two rungs that surprise
    /// people — what to state instead. This is the half of the log line that turns "my tape is
    /// empty" into a one-line diagnosis.
    pub const fn why(self) -> &'static str {
        match self {
            Self::Explicit => "stated for this run (--store / store = / store_root)",
            Self::EnvVar => "stated for this box ($VIKE_HIST_STORE)",
            Self::DeclaredProject => {
                "the data folder of the project VIKE_SETTINGS_DIR names outright — a DECLARED \
                 project outranks the build checkout below, which is only ever inferred from a path \
                 baked in at compile time"
            }
            Self::DevCheckout => {
                "the source checkout that built this binary — <repo>/market_data/hist, which outranks the \
                 DISCOVERED project default so a developer's existing store cannot silently \
                 relocate (a project DECLARED by VIKE_SETTINGS_DIR outranks it in turn)"
            }
            Self::Project => {
                "this project's own data folder, DISCOVERED by the same walk as <project>/settings \
                 — set VIKE_SETTINGS_DIR to pin the project (which then outranks a dev checkout \
                 too), or VIKE_HIST_STORE to pin the store outright"
            }
            Self::UserDir => {
                "the per-user data directory: NO project was found above the working directory \
                 (create <project>/settings/, or set VIKE_SETTINGS_DIR / VIKE_HIST_STORE)"
            }
            Self::LastResort => {
                "last resort: no project, no per-user data directory, and a scrubbed environment — \
                 set VIKE_HIST_STORE"
            }
        }
    }
}

/// `<project>/market_data/hist` together with **WHERE THAT PROJECT CAME FROM** — the provenance that
/// decides which side of the dev-checkout hinge the project rung sits on.
///
/// # Why this is not an `Option<PathBuf>`
///
/// It was one, and the distinction was therefore not representable at all: the ladder received a
/// path and could not tell a project an operator had DECLARED from one the walk had GUESSED, so
/// both had to sit at one height — below the hinge, where the guess is safe. That put a
/// declaration below an inference, which is backwards, and made
/// `docs/decisions/0026-containerisation-additive-backend-image.md` false for the store (see the
/// module doc's "the two project rungs").
///
/// A `bool` beside the `Option<PathBuf>` would have carried the same information and is refused for
/// the reason [`resolve_store_root_from`] exists: the ladder's parameters are deliberately all of
/// DIFFERENT types, so no two can be transposed without a type error. A second `Option<PathBuf>`
/// would have been worse still — that is precisely the adjacent-and-swappable shape the wiring
/// function was built to remove.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectDefault {
    /// **DECLARED** — a non-blank `$VIKE_SETTINGS_DIR` named the project outright, so this path is
    /// a STATEMENT and outranks [`StoreRootRung::DevCheckout`].
    Declared(PathBuf),
    /// **DISCOVERED** — the walk up from the working directory found a project marker. Evidence
    /// about where the process was launched, not about what anybody wanted, so it stays BELOW
    /// [`StoreRootRung::DevCheckout`] and a developer's existing store cannot silently relocate.
    Discovered(PathBuf),
    /// No project at all: nothing declared, and no marker above the working directory. A bare
    /// binary run from `/tmp`.
    None,
}

/// Classify a resolved `<project>/market_data/hist` by whether the operator DECLARED the project — the ONE
/// site that decides, so the two rungs cannot drift apart.
///
/// ⚠ **A blank or whitespace-only override is not a declaration.** The rule is not invented here:
/// [`crate::paths::state_path::project_settings_dir_from`] already ignores such a value rather than
/// honouring it, so `project` will have come back from the WALK in that case, and calling it
/// declared would promote a walk's answer above the dev-checkout hinge on the strength of an empty
/// `Environment=VIKE_SETTINGS_DIR=` line. Same `trim`-then-non-empty test as that function, spelled
/// here so the classification and the resolution agree by construction.
pub fn project_default_from(
    settings_dir_override: Option<&str>,
    project: Option<PathBuf>,
) -> ProjectDefault {
    let Some(project) = project else { return ProjectDefault::None };
    match settings_dir_override.map(str::trim).filter(|s| !s.is_empty()) {
        Some(_) => ProjectDefault::Declared(project),
        None => ProjectDefault::Discovered(project),
    }
}

/// A resolved hist-store root together with the [`StoreRootRung`] that chose it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreRoot {
    /// The directory a store will be opened (and CREATED) at.
    pub root: PathBuf,
    /// Which rung answered — the field that makes a silent relocation visible.
    pub rung: StoreRootRung,
}

impl StoreRoot {
    /// The path alone, for the many call sites that only need somewhere to open a store.
    pub fn into_path(self) -> PathBuf {
        self.root
    }
}

impl std::fmt::Display for StoreRoot {
    /// ONE operator-facing line: the path, then why it is that path.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.root.display(), self.rung.why())
    }
}

/// **The form a BINARY should call** — [`resolve_store_root`] with the project rungs and the
/// per-user rung computed here rather than at the call site.
///
/// # Why this exists, and why the defaults are not parameters
///
/// `project_default` and `user_default` were both `Option<PathBuf>`, adjacent, and mean opposite
/// things — so transposing them at any of the four call sites COMPILED SILENTLY and changed where
/// gigabytes land, with the failure surfacing much later as an empty result set. Folding both into
/// this one function removes the hazard by construction rather than by vigilance: no two parameters
/// here share a type (`Option<PathBuf>`, `Option<String>`, `&Path`, `Option<&Path>`, `&HashMap`),
/// so no pair of them can be swapped without a type error, and the one remaining wiring site is
/// this function — pinned by `the_wiring_puts_the_project_before_the_user_dir`.
///
/// ⚠ **That is now belt AND braces below this function too**: `project_default` became
/// [`ProjectDefault`] when the project rung split by provenance, so even the bare ladder's two
/// defaults are no longer the same type. This function is still the form to call — it is what stops
/// a call site classifying the provenance itself, which is the half a transposition-proof signature
/// cannot cover.
///
/// `cwd` is the caller's working directory (`std::env::current_dir().ok()`); `vars` is the process
/// environment as the caller collected it. Everything this reads out of `vars` — `VIKE_SETTINGS_DIR`
/// and the platform trio — is a MAP lookup, so the function stays pure and the settings registry's
/// rule (libraries take configuration as parameters) holds.
///
/// `repo_default` is `None` in a build that has no checkout to name — see [`resolve_store_root`]
/// and the module doc's rung 4 for what a shipped binary passes, and why.
pub fn resolve_store_root_from(
    explicit: Option<PathBuf>,
    env_hist_store: Option<String>,
    repo_default: Option<&Path>,
    cwd: Option<&Path>,
    vars: &HashMap<String, String>,
) -> StoreRoot {
    // ⚠ `_from`, not the bare walk: `VIKE_SETTINGS_DIR` relocates settings, credentials and state,
    // and the store's default has to travel with them or a relocated project reads one project's
    // keys while writing another project's tape.
    let settings_override =
        vars.get(crate::paths::state_path::SETTINGS_DIR_ENV).map(String::as_str);
    // ⚠ The `cwd` guard is the residual the module doc declares: the WALK genuinely needs somewhere
    // to start, so a binary that cannot read its own working directory reaches neither project rung.
    // Bypassing it for a declaration alone would rest on `project_hist_store_dir_from` ignoring
    // `start` whenever the override is non-blank — a cross-module invariant nothing here can check.
    let project = cwd.and_then(|cwd| {
        crate::paths::state_path::project_hist_store_dir_from(settings_override, cwd)
    });
    resolve_store_root(
        explicit,
        env_hist_store,
        repo_default,
        // The SAME variable both resolves the path and classifies it, so the rung an operator is
        // told about is the rung their declaration actually bought.
        project_default_from(settings_override, project),
        user_data_dir_from_vars(vars),
    )
}

/// Resolve a hist-store root. See the module doc for the full precedence and the WHY of each rung.
///
/// ⚠ **Prefer [`resolve_store_root_from`].** This is the pure LADDER, and calling it by hand means
/// classifying the project's PROVENANCE by hand — deciding, at a binary, whether
/// `$VIKE_SETTINGS_DIR` was a declaration. Get that wrong in the permissive direction and a walk's
/// guess is promoted above the dev-checkout hinge, which is the defect this rung split exists to
/// fix, wearing its mirror image. Keeping it public is what lets the precedence be tested rung by
/// rung with no filesystem and no environment at all.
///
/// `repo_default` is the CALLER's own compile-time `<repo>/market_data/hist` (each crate passes its own,
/// derived from its `CARGO_MANIFEST_DIR`), so this module needs no notion of the workspace layout —
/// **or `None`, when the build has no checkout to name.** A compile-time path is a string literal
/// in the binary, so a build that SHIPS (`release.yml`'s `--release` lanes, whose assets the public
/// mirror re-publishes) must not carry one: it names the build box on a public download, and on
/// the box that both builds and deploys it makes the hinge fire for the RUNNER's checkout. The
/// shipped call sites therefore pass the rung under `cfg(debug_assertions)` only; `None` skips the
/// hinge outright and makes the last resort CWD-relative (rung 7 of the module doc says why that
/// is the lesser evil there). `project_default` is `<project>/market_data/hist` as the caller
/// resolved it, TAGGED with where it came from, so the walk stays in [`crate::paths::state_path`] and this
/// stays a pure function of its arguments.
pub fn resolve_store_root(
    explicit: Option<PathBuf>,
    env_hist_store: Option<String>,
    repo_default: Option<&Path>,
    project_default: ProjectDefault,
    user_default: Option<PathBuf>,
) -> StoreRoot {
    if let Some(p) = explicit {
        return StoreRoot { root: p, rung: StoreRootRung::Explicit };
    }
    if let Some(p) = env_hist_store.filter(|s| !s.trim().is_empty()) {
        return StoreRoot { root: PathBuf::from(p), rung: StoreRootRung::EnvVar };
    }
    // THE DECLARED PROJECT, and it sits ABOVE the hinge below for the same reason the two rungs
    // above it do: it was STATED. The hinge is an inference — "the tree I was compiled in still
    // exists at the path baked into me" — and an inference may not outrank a declaration. Borrowed
    // rather than moved so the DISCOVERED half is still available further down; the clone is one
    // `PathBuf` once per process.
    if let ProjectDefault::Declared(p) = &project_default {
        return StoreRoot { root: p.clone(), rung: StoreRootRung::DeclaredProject };
    }
    // THE COMPATIBILITY HINGE. Not `repo_default.is_dir()`: on a fresh checkout `market_data/hist` does not
    // exist YET (it is gitignored and created on first write), and testing the leaf would silently
    // relocate every new clone's data to the user dir. What actually distinguishes "running from the
    // checkout that built me" from "installed elsewhere" is whether that CHECKOUT is still there, so
    // test the repo ROOT — `<repo>/market_data/hist` → up two → `<repo>` — for [`REPO_MARKER`].
    // A build with no checkout rung (`None`) has nothing to probe and skips straight past.
    if let Some(repo_default) = repo_default {
        let dev_checkout = repo_default.is_dir()
            || repo_default.parent().and_then(Path::parent).is_some_and(is_repo);
        if dev_checkout {
            return StoreRoot {
                root: repo_default.to_path_buf(),
                rung: StoreRootRung::DevCheckout,
            };
        }
    }
    // THE DISCOVERED PROJECT RUNG. No existence probe, on purpose — and the asymmetry with the hinge
    // above is the point. The hinge probes because `repo_default` is a COMPILE-TIME path that may
    // name a directory on somebody else's machine, so its existence is the only evidence it is real.
    // This path was resolved at RUNTIME by the same walk that found `settings/`: the project is
    // already proven to exist, and `market_data/` merely has not been written to yet. Probing it would send
    // every fresh install to the per-user directory on its first run and to the project on its
    // second.
    if let ProjectDefault::Discovered(p) = project_default {
        return StoreRoot { root: p, rung: StoreRootRung::Project };
    }
    match user_default {
        Some(p) => StoreRoot { root: p, rung: StoreRootRung::UserDir },
        None => StoreRoot {
            root: repo_default.map_or_else(no_checkout_last_resort, Path::to_path_buf),
            rung: StoreRootRung::LastResort,
        },
    }
}

/// Rung 7 for a build that passed NO checkout: `market_data/hist` relative to the working
/// directory. The only CWD-relative answer this ladder ever gives, and it is reached only when
/// every other rung declined (the module doc's rung 7 carries the argument). Spelled through the
/// same two constants the project rung joins, so the leaf a shipped binary invents in this corner
/// is at least the leaf every other rung would have used.
fn no_checkout_last_resort() -> PathBuf {
    PathBuf::from(crate::paths::state_path::PROJECT_DATA_DIR)
        .join(crate::paths::state_path::HIST_SUBDIR)
}

/// The file whose PRESENCE at the probed repo root is what makes that directory a CHECKOUT rather
/// than a directory that merely shares its path — the evidence the dev-checkout hinge tests for.
///
/// A cargo workspace cannot have been built without one at its root, so every box that genuinely
/// still has the checkout has this file; a directory conjured by something else does not. That
/// asymmetry is the whole of the probe.
///
/// ⚠ **Deliberately NOT "does it declare `[workspace]`".** [`crate::paths::state_path`]'s walk reads the
/// manifest's CONTENT because it must separate a member crate from its workspace root; this
/// question is coarser — "is anything cargo-shaped here at all" — and the stronger probe would
/// refuse a `repo_default` a caller anchored at a member crate, which the tests in this module do.
///
/// ⚠ **The name is spelled twice in this crate**, here and privately in [`crate::paths::state_path`], and
/// that is the accepted cost of keeping the two probes independent: they ask different questions of
/// the same file name (see above), and sharing one constant would imply they share a rule.
const REPO_MARKER: &str = "Cargo.toml";

/// Does this directory look like the source CHECKOUT that produced a `repo_default`?
///
/// Split out of the hinge so the probe has a name and a doc rather than being a clause: the
/// question it answers is the one the rung's [`StoreRootRung::why`] sentence promises an operator,
/// and it was previously answered by [`Path::is_dir`], which promises much less.
fn is_repo(repo: &Path) -> bool {
    repo.join(REPO_MARKER).is_file()
}

/// The per-user data directory for stores, or `None` when the platform's variables are all absent
/// (a daemon with a scrubbed environment) — in which case [`resolve_store_root`] falls back rather
/// than guessing.
///
/// Deliberately computed from the environment VALUES passed in rather than by adding a
/// platform-dirs dependency: this workspace links one order-signing binary and every new crate joins
/// the `cargo deny` audit surface, so a directory string is not worth an edge.
#[cfg(windows)]
pub fn user_data_dir(
    _xdg_data_home: Option<&str>,
    home: Option<&str>,
    localappdata: Option<&str>,
) -> Option<PathBuf> {
    localappdata
        .filter(|s| !s.is_empty())
        .map(|p| Path::new(p).join(USER_STORE_DIR))
        .or_else(|| home.filter(|s| !s.is_empty()).map(|p| Path::new(p).join(USER_STORE_DIR)))
}

/// Unix twin of the above: XDG first, then `~/.local/share`, per the XDG base-directory spec.
#[cfg(not(windows))]
pub fn user_data_dir(
    xdg_data_home: Option<&str>,
    home: Option<&str>,
    localappdata: Option<&str>,
) -> Option<PathBuf> {
    let _ = localappdata;
    xdg_data_home.filter(|s| !s.is_empty()).map(|p| Path::new(p).join(USER_STORE_DIR)).or_else(
        || {
            home.filter(|s| !s.is_empty())
                .map(|p| Path::new(p).join(".local").join("share").join(USER_STORE_DIR))
        },
    )
}

/// The characters a STORE SYMBOL may not contain, because the symbol is interpolated into a
/// DIRECTORY NAME and these are not characters there.
///
/// `vike-data`'s `DataFusionHist::series_dir` builds a series' leaf as
/// `kind=…/venue=…/symbol=…[/interval=…]`, one `PathBuf::join` per level. The symbol is the third
/// component's VALUE, so whatever it contains, the filesystem reads as part of the path.
///
/// This is the POSIX reserved pair (`/`, NUL — NUL cannot reach here from a `&str`) plus the Win32
/// set, because a store written on one box is read on the other: the CI box is Linux and the dev box is
/// Windows, and `just windows-check` compiles this workspace natively there.
pub const PATH_HOSTILE_IN_A_SYMBOL: &[char] = &['/', '\\', ':', '<', '>', '"', '|', '?', '*'];

/// Refuse a symbol that cannot survive being a directory name, naming the character and why.
///
/// # Why this is a REFUSAL rather than a sanitiser
///
/// Rewriting the symbol here would put a SECOND spelling of an instrument into the store, reachable
/// by nobody who looks it up by name. The canonical spelling is `vike-catalog`'s job
/// (`crates/vike-catalog/src/symbol.rs`, the core⇄exchange conversion that already turns a perp into
/// `BTCUSDT.P`); this function's job is to make sure nothing reaches the store without having been
/// through it.
///
/// # The two failure modes, MEASURED 2026-09-19 rather than reasoned about
///
/// They are NOT the same shape, and the quieter one is the dangerous one:
///
/// * **`/` fails SILENTLY, on BOTH platforms.** `PathBuf::join("symbol=HYPE/USDC")` is a path with
///   an extra level in it, so `create_dir_all` SUCCEEDS and the rows land under `symbol=HYPE`
///   containing `USDC`. Measured on the CI box: `mkdir -p 'symbol=HYPE/USDC/interval=1h'` produced three
///   directories where two were meant. The partition column then reads `HYPE`, the `USDC` level is
///   not a `key=value` pair and means nothing to DataFusion, and a reader looking the series up by
///   its real name finds nothing. Nothing errors. And it is not recoverable by re-fetching: the
///   commit key is spent, so a corrective run answers `Ok(0)`.
/// * **`:` fails LOUDLY, and only on Windows.** Linux accepts it — the same the CI box `mkdir` built
///   `symbol=xyz:TSLA` correctly — while Win32 refuses outright (`The directory name is invalid`,
///   measured natively; ⚠ MSYS/Git-Bash CREATES it, so a POSIX-shell probe on a Windows box answers
///   the wrong question). So a store collected on the CI box cannot be copied to or opened on the dev
///   box, and the failure surfaces for whoever reads it rather than whoever wrote it.
///
/// # What is actually blocked today, and it is not hypothetical
///
/// Hyperliquid, from its own live `/info` metadata: **328 spot pairs** (our unified spelling is
/// `BASE/QUOTE`, e.g. `HYPE/USDC`) and **289 builder-deployed perps** across eleven DEXes, whose
/// venue names carry a colon (`xyz:TSLA`, `para:…`). Its 234 CORE perps are bare coins (`BTC`,
/// `HYPE`, `kPEPE`) and are unaffected. Alpaca's crypto pairs are slash-delimited on the WIRE
/// (`BTC/USD`), where the slash is load-bearing — `crates/bridges/alpaca/src/data.rs`'s `class_of`
/// reads it to tell crypto from equity — so that one cannot be dropped, only converted.
///
/// # Errors
///
/// The message names the offending character, says which platform it breaks and how, and points at
/// the crate that owns canonical spellings — because the fix is never "escape it here".
pub fn refuse_a_path_hostile_symbol(symbol: &str) -> Result<(), String> {
    let Some(bad) = symbol.chars().find(|c| PATH_HOSTILE_IN_A_SYMBOL.contains(c)) else {
        return Ok(());
    };
    let how = match bad {
        '/' | '\\' => {
            "it is a path SEPARATOR, so the series would silently gain a directory level and land \
             under a partition nothing spells — on every platform, with no error"
        }
        ':' => {
            "Windows refuses a directory name containing it outright, so a store written on Linux \
             cannot be opened or copied on a Windows box"
        }
        _ => "Windows refuses a directory name containing it",
    };
    Err(format!(
        "symbol {symbol:?} cannot be a store key: {bad:?} may not appear in one, because {how}. A \
         symbol becomes the directory name `symbol=<symbol>`. Give the instrument a canonical \
         spelling in `crates/vike-catalog/src/symbol.rs` — the core⇄exchange conversion that \
         already renders a perp as `BTCUSDT.P` — and store THAT; do not escape or rewrite it here, \
         which would put a second spelling of one instrument into the store."
    ))
}

#[cfg(test)]
mod tests;
