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
//! 5. [`ProjectDefault::Discovered`] — **`<project>/market_data/hist`**, the project the working directory
//!    sits in, found by walking up through
//!    [`crate::state_path::project_hist_store_dir_from`]. The default a
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
//!    [`crate::state_path::HIST_SUBDIR`] uses. **A module doc may not cite a specific machine's
//!    on-disk layout**: nothing in this repository can re-derive it, so it rots in total silence and
//!    is then quoted as evidence for a design it never supported. State what the rung guarantees;
//!    leave the box out of it.
//! 7. `repo_default` again, as a last resort, so this function is total and never invents a path from
//!    the CWD.
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
//! workspace's standing rule for this variable ([`crate::state_path::project_settings_dir_from`]
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
//! calls the [`crate::state_path`] walk, which are both pure functions of their arguments; the
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
//! [`crate::state_path::project_hist_store_dir_from`], so the one variable that relocates settings,
//! credentials and state relocates the store's default with them — and, since that variable is what
//! separates rung 3 from rung 5, relocates it from a HEIGHT no inference can reach. The alternative —
//! settings from
//! the override, data from the walk — falsifies this change's own premise: an operator who moved
//! their project would read one project's `secrets.env` while writing another project's tape. The
//! override's PARENT is the project root; a parentless value (`VIKE_SETTINGS_DIR=settings`) is
//! refused rather than resolved against the CWD, and a settings directory that is genuinely not
//! inside its project is what `$VIKE_HIST_STORE` is for.
//!
//! # ⚠ The DISCOVERED project rung follows the WORKING DIRECTORY, and that is inherited, not new
//!
//! The marker that answers rung 5 is the settings walk's marker, so the same installed binary run
//! from two directories can resolve two different stores. (Rung 3 has no such property — a
//! declaration names the project outright, which is most of why it is a separate rung.) Three things
//! make that the right trade rather than a hole:
//!
//! * **It is strictly less than what already ships.** That binary ALREADY resolves `secrets.env`,
//!   `policy.toml` and `settings/state/` by the same walk from the same working directory. Data now
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
//! diagnosis instead (it names `ReadWritePaths=` in the error), and `docs/ops/upgrading.md` states
//! the operator rule: the unit's `ReadWritePaths=` and the store root must name the SAME path.
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
//! that already builds one (`std::env::vars().collect()` — the idiom `vike-app`, `vike-cli`,
//! `vike-mount` and `vike-tradehub` all use) and leaves exactly one file naming the variables.
//!
//! This function does not read the environment either: it is the same pure resolution with a
//! different argument shape. Both are kept, because a caller that already holds three `Option<&str>`
//! — a test, or a binary that read three specific variables — should not have to build a map to ask
//! the question.
//!
//! ⚠ This is the ONLY consumer of the platform trio left in the workspace. Its sibling
//! [`crate::state_path`] resolves program-written STATE, and does so by WALKING up for the project
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
    /// Rung 1 — `--store`, a profile's `store =`, `config.toml`'s `store_root`.
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
/// [`crate::state_path::project_settings_dir_from`] already ignores such a value rather than
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
pub fn resolve_store_root_from(
    explicit: Option<PathBuf>,
    env_hist_store: Option<String>,
    repo_default: &Path,
    cwd: Option<&Path>,
    vars: &HashMap<String, String>,
) -> StoreRoot {
    // ⚠ `_from`, not the bare walk: `VIKE_SETTINGS_DIR` relocates settings, credentials and state,
    // and the store's default has to travel with them or a relocated project reads one project's
    // keys while writing another project's tape.
    let settings_override = vars.get(crate::state_path::SETTINGS_DIR_ENV).map(String::as_str);
    // ⚠ The `cwd` guard is the residual the module doc declares: the WALK genuinely needs somewhere
    // to start, so a binary that cannot read its own working directory reaches neither project rung.
    // Bypassing it for a declaration alone would rest on `project_hist_store_dir_from` ignoring
    // `start` whenever the override is non-blank — a cross-module invariant nothing here can check.
    let project =
        cwd.and_then(|cwd| crate::state_path::project_hist_store_dir_from(settings_override, cwd));
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
/// derived from its `CARGO_MANIFEST_DIR`), so this module needs no notion of the workspace layout.
/// `project_default` is `<project>/market_data/hist` as the caller resolved it, TAGGED with where it came
/// from, so the walk stays in [`crate::state_path`] and this stays a pure function of its arguments.
pub fn resolve_store_root(
    explicit: Option<PathBuf>,
    env_hist_store: Option<String>,
    repo_default: &Path,
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
    let dev_checkout =
        repo_default.is_dir() || repo_default.parent().and_then(Path::parent).is_some_and(is_repo);
    if dev_checkout {
        return StoreRoot { root: repo_default.to_path_buf(), rung: StoreRootRung::DevCheckout };
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
        None => StoreRoot { root: repo_default.to_path_buf(), rung: StoreRootRung::LastResort },
    }
}

/// The file whose PRESENCE at the probed repo root is what makes that directory a CHECKOUT rather
/// than a directory that merely shares its path — the evidence the dev-checkout hinge tests for.
///
/// A cargo workspace cannot have been built without one at its root, so every box that genuinely
/// still has the checkout has this file; a directory conjured by something else does not. That
/// asymmetry is the whole of the probe.
///
/// ⚠ **Deliberately NOT "does it declare `[workspace]`".** [`crate::state_path`]'s walk reads the
/// manifest's CONTENT because it must separate a member crate from its workspace root; this
/// question is coarser — "is anything cargo-shaped here at all" — and the stronger probe would
/// refuse a `repo_default` a caller anchored at a member crate, which the tests in this module do.
///
/// ⚠ **The name is spelled twice in this crate**, here and privately in [`crate::state_path`], and
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A `repo_default` guaranteed NOT to exist on any machine — the INSTALLED shape, where the
    /// compile-time path names a directory on whoever's box did the build. Spelled once, because
    /// every rung below the hinge is only reachable when the hinge does not fire.
    const NO_CHECKOUT: &str = "/definitely/not/a/real/build/machine/path/market_data/hist";

    #[test]
    fn explicit_beats_everything() {
        let got = resolve_store_root(
            Some(PathBuf::from("explicit")),
            Some("env".into()),
            Path::new("repo"),
            ProjectDefault::Discovered(PathBuf::from("project")),
            Some(PathBuf::from("user")),
        );
        assert_eq!(got.root, PathBuf::from("explicit"));
        assert_eq!(got.rung, StoreRootRung::Explicit);
    }

    #[test]
    fn env_beats_the_defaults() {
        let got = resolve_store_root(
            None,
            Some("env".into()),
            Path::new(NO_CHECKOUT),
            ProjectDefault::Discovered(PathBuf::from("project")),
            Some(PathBuf::from("user")),
        );
        assert_eq!(got.root, PathBuf::from("env"));
        assert_eq!(got.rung, StoreRootRung::EnvVar);
    }

    /// An empty/whitespace `VIKE_HIST_STORE` must not win — it would resolve the store to "" and
    /// create a store at the CWD, the exact class of bug this module exists to remove.
    #[test]
    fn a_blank_env_value_is_ignored() {
        let got = resolve_store_root(
            None,
            Some("   ".into()),
            Path::new(NO_CHECKOUT),
            ProjectDefault::Discovered(PathBuf::from("project")),
            Some(PathBuf::from("user")),
        );
        assert_eq!(got.root, PathBuf::from("project"));
    }

    /// THE compatibility hinge: in a dev checkout the answer is unchanged from before this module
    /// existed — even though `market_data/hist` itself does not exist yet.
    ///
    /// It outranks the PROJECT rung too, which is the whole reason the hinge survived the project
    /// rung landing: a developer's populated `<repo>/market_data/hist` must not silently relocate, and a
    /// store that relocates does not merge — the old one just stops being read.
    #[test]
    fn a_dev_checkout_keeps_the_repo_default_even_before_data_hist_exists() {
        // `<this crate>/market_data/hist` — the leaf does NOT exist, but its repo root does, which is
        // exactly the fresh-clone shape.
        let repo_default =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("market_data").join("hist");
        assert!(!repo_default.is_dir(), "precondition: the leaf must not exist");
        let got = resolve_store_root(
            None,
            None,
            &repo_default,
            ProjectDefault::Discovered(PathBuf::from("project")),
            Some(PathBuf::from("user")),
        );
        assert_eq!(got.root, repo_default, "a checkout keeps resolving to <repo>/market_data/hist");
        assert_eq!(got.rung, StoreRootRung::DevCheckout);
    }

    /// **THE CONTAINER CASE, and the reason the hinge probes for a MANIFEST rather than for a
    /// directory.** `<repo>` here exists and is EMPTY — which is not a contrived shape: a runtime
    /// image's `WORKDIR /app` CREATES `/app`, and a builder stage that had the workspace at `/app`
    /// bakes `/app/market_data/hist` into every binary it produces. The two collide with no source tree
    /// anywhere, and a bare `is_dir` probe reads that empty directory as "the checkout that built
    /// this binary is still here".
    ///
    /// The cost of getting this wrong is the one this whole module exists to prevent, wearing its
    /// worst clothes: the hinge outranks the project rung, so the tape lands on the container's
    /// ephemeral layer instead of in the bind-mounted project, and it is gone at the next
    /// `docker run` — a store does not merge, so the operator sees zero rows rather than an error.
    #[test]
    fn an_empty_directory_at_the_repo_root_is_not_a_checkout() {
        let scratch = Scratch::new("empty-repo-root");
        // Exactly what `WORKDIR /app` leaves behind: the directory, and nothing in it.
        let repo = scratch.path().join("app");
        std::fs::create_dir_all(&repo).unwrap();
        let repo_default = repo.join("market_data").join("hist");
        assert!(!repo_default.is_dir(), "precondition: the store leaf must not exist");
        assert!(repo.is_dir(), "precondition: the repo ROOT must exist — that is the whole trap");

        let got = resolve_store_root(
            None,
            None,
            &repo_default,
            ProjectDefault::Discovered(PathBuf::from("project")),
            Some(PathBuf::from("user")),
        );
        assert_eq!(
            (got.root, got.rung),
            (PathBuf::from("project"), StoreRootRung::Project),
            "an empty directory is not a checkout: the PROJECT rung must answer"
        );
    }

    /// …and the other half, which is what stops the test above from being cured by simply deleting
    /// the rung: a repo root carrying a `Cargo.toml` IS a checkout, and still answers before the
    /// project — on a SYNTHETIC tree, so the claim is about the probe rather than about the one
    /// directory this crate happens to be compiled in.
    #[test]
    fn a_repo_root_carrying_a_manifest_is_a_checkout_even_with_no_data_dir() {
        let scratch = Scratch::new("manifest-repo-root");
        let repo = scratch.path().join("checkout");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("Cargo.toml"), "[workspace]\n").unwrap();
        let repo_default = repo.join("market_data").join("hist");
        assert!(
            !repo_default.is_dir(),
            "precondition: the fresh-clone shape — no market_data/hist yet"
        );

        let got = resolve_store_root(
            None,
            None,
            &repo_default,
            ProjectDefault::Discovered(PathBuf::from("project")),
            Some(PathBuf::from("user")),
        );
        assert_eq!(
            (got.root, got.rung),
            (repo_default, StoreRootRung::DevCheckout),
            "a checkout still outranks the project rung"
        );
    }

    /// And when the store already exists, obviously still the repo default.
    #[test]
    fn an_existing_repo_default_wins_over_the_user_dir() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let got = resolve_store_root(
            None,
            None,
            &dir,
            ProjectDefault::Discovered(PathBuf::from("project")),
            Some(PathBuf::from("user")),
        );
        assert_eq!(got.root, dir);
    }

    /// **THE new rung.** The installed case: the compile-time repo path does not exist on this
    /// machine, so the PROJECT's own `market_data/hist` answers — not the per-user directory, which is
    /// a second location outside the one folder the owner asked for.
    #[test]
    fn a_missing_repo_default_falls_through_to_the_project_dir() {
        let got = resolve_store_root(
            None,
            None,
            Path::new(NO_CHECKOUT),
            ProjectDefault::Discovered(PathBuf::from("project")),
            Some(PathBuf::from("user")),
        );
        assert_eq!(got.root, PathBuf::from("project"));
        assert_eq!(got.rung, StoreRootRung::Project);
    }

    /// …and with NO project above the working directory (a bare binary run from `/tmp`), the
    /// per-user directory is still the answer. This is why that rung is kept rather than deleted:
    /// the alternative is inventing a path from the CWD.
    #[test]
    fn without_a_project_it_falls_through_to_the_user_dir() {
        let got = resolve_store_root(
            None,
            None,
            Path::new(NO_CHECKOUT),
            ProjectDefault::None,
            Some(PathBuf::from("user")),
        );
        assert_eq!(got.root, PathBuf::from("user"));
        assert_eq!(got.rung, StoreRootRung::UserDir);
    }

    /// Total function: with neither a project nor a user dir (scrubbed env), it still answers —
    /// and answers with the repo path rather than something CWD-relative.
    #[test]
    fn without_a_project_or_a_user_dir_it_still_answers_with_the_repo_default() {
        let got =
            resolve_store_root(None, None, Path::new(NO_CHECKOUT), ProjectDefault::None, None);
        assert_eq!(got.root, PathBuf::from(NO_CHECKOUT));
        assert_eq!(got.rung, StoreRootRung::LastResort);
    }

    /// **The whole precedence in ONE ordered assertion.** Each rung is knocked out in turn, and the
    /// answer must step down exactly one place. A mutation that reorders two rungs — or drops one —
    /// changes an answer here even when every single-rung test above still passes.
    #[test]
    fn the_rungs_step_down_in_order() {
        let repo = Path::new(NO_CHECKOUT);
        let checkout =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("market_data").join("hist");
        // ⚠ The two project rungs supply DIFFERENT paths, so a step that answered with the wrong
        // provenance is caught on the VALUE and not only on the rung tag.
        let declared = || ProjectDefault::Declared(PathBuf::from("declared"));
        let proj = || ProjectDefault::Discovered(PathBuf::from("project"));
        let user = || Some(PathBuf::from("user"));

        // 1. explicit — every other rung supplied, and still ignored.
        let got = resolve_store_root(
            Some("explicit".into()),
            Some("env".into()),
            &checkout,
            declared(),
            user(),
        );
        assert_eq!((got.root, got.rung), (PathBuf::from("explicit"), StoreRootRung::Explicit));
        // 2. the env var, once nothing was stated on the command line — still above a DECLARED
        //    project, because `$VIKE_HIST_STORE` names the store itself rather than the project.
        let got = resolve_store_root(None, Some("env".into()), &checkout, declared(), user());
        assert_eq!((got.root, got.rung), (PathBuf::from("env"), StoreRootRung::EnvVar));
        // 3. the DECLARED project, once the env var is gone — ABOVE the dev-checkout hinge, which
        //    is supplied here and must lose to it.
        let got = resolve_store_root(None, None, &checkout, declared(), user());
        assert_eq!(
            (got.root, got.rung),
            (PathBuf::from("declared"), StoreRootRung::DeclaredProject)
        );
        // 4. the dev-checkout hinge, once nothing is declared — ABOVE the DISCOVERED project rung.
        let got = resolve_store_root(None, None, &checkout, proj(), user());
        assert_eq!((got.root, got.rung), (checkout, StoreRootRung::DevCheckout));
        // 5. the discovered project, once this box has no checkout.
        let got = resolve_store_root(None, None, repo, proj(), user());
        assert_eq!((got.root, got.rung), (PathBuf::from("project"), StoreRootRung::Project));
        // 6. the per-user dir, once there is no project either.
        let got = resolve_store_root(None, None, repo, ProjectDefault::None, user());
        assert_eq!((got.root, got.rung), (PathBuf::from("user"), StoreRootRung::UserDir));
        // 7. and the repo path as a last resort, so the function is total.
        let got = resolve_store_root(None, None, repo, ProjectDefault::None, None);
        assert_eq!((got.root, got.rung), (PathBuf::from(NO_CHECKOUT), StoreRootRung::LastResort));
    }

    /// **The rung a caller LOGS must be the rung that answered.** The path and the rung are two
    /// fields of one struct, so a copy-paste that returned the right path with a neighbouring
    /// rung would report a store as "explicit" while it came from the project walk — an operator
    /// then trusts a value nobody stated. Every variant is reachable and distinct, and each
    /// [`StoreRootRung::why`] sentence is non-empty, because an empty explanation in a log line is
    /// the same as no log line.
    #[test]
    fn every_rung_is_reachable_and_carries_its_own_explanation() {
        use std::collections::BTreeSet;
        let checkout =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("market_data").join("hist");
        let repo = Path::new(NO_CHECKOUT);
        let none = || ProjectDefault::None;
        let seen: Vec<StoreRootRung> = vec![
            resolve_store_root(Some("x".into()), None, repo, none(), None).rung,
            resolve_store_root(None, Some("y".into()), repo, none(), None).rung,
            // ⚠ `repo`, not `checkout`: the DECLARED rung must report itself with NO checkout in
            // play, so this row proves the variant is reachable rather than proving the ordering
            // (which `the_rungs_step_down_in_order` owns, with the checkout supplied).
            resolve_store_root(None, None, repo, ProjectDefault::Declared("d".into()), None).rung,
            resolve_store_root(None, None, &checkout, none(), None).rung,
            resolve_store_root(None, None, repo, ProjectDefault::Discovered("p".into()), None).rung,
            resolve_store_root(None, None, repo, none(), Some("u".into())).rung,
            resolve_store_root(None, None, repo, none(), None).rung,
        ];
        assert_eq!(
            seen,
            vec![
                StoreRootRung::Explicit,
                StoreRootRung::EnvVar,
                StoreRootRung::DeclaredProject,
                StoreRootRung::DevCheckout,
                StoreRootRung::Project,
                StoreRootRung::UserDir,
                StoreRootRung::LastResort,
            ],
            "each rung must be reachable and report ITSELF"
        );
        let tags: BTreeSet<&str> = seen.iter().map(|r| r.as_str()).collect();
        assert_eq!(tags.len(), seen.len(), "the log tags must be distinct");
        for rung in &seen {
            assert!(!rung.why().trim().is_empty(), "{rung:?} must explain itself");
        }
    }

    /// The `Display` line a binary logs carries BOTH halves: an operator who only sees the path
    /// cannot tell a deliberate `--store` from a walk that quietly moved.
    #[test]
    fn the_display_line_names_the_path_and_the_reason() {
        let got = resolve_store_root(
            None,
            None,
            Path::new(NO_CHECKOUT),
            ProjectDefault::Discovered("/p/market_data/hist".into()),
            None,
        );
        let line = got.to_string();
        assert!(line.contains("/p/market_data/hist"), "the path must be in the line: {line}");
        assert!(line.contains(StoreRootRung::Project.why()), "the reason must be too: {line}");
    }

    // ---- the OPERATIONAL cases: the real walk feeding the real precedence -----------------------

    /// A private scratch directory under the system temp dir — this crate has no `tempfile`
    /// dev-dependency, so the two deployment tests below make (and remove) their own.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let p = std::env::temp_dir().join(format!("vike-store-path-{tag}-{nanos}"));
            std::fs::create_dir_all(&p).expect("scratch dir");
            Self(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// **THE deployment case, end to end.** A project root holds a binary and a `settings/`
    /// directory and no source tree — so the hinge cannot fire — and the tape must land in that
    /// same folder,
    /// not under `$HOME`. The walk and the precedence are exercised together here precisely because
    /// each is separately correct in the shipped tree and the pairing is what was wrong.
    ///
    /// ⚠ The `settings/` directory is what makes this resolve at all: it is the deployment MARKER
    /// (`crates/vike-model/src/state_path.rs`'s `nearest_project_marker`), which is why the install
    /// recipe creates it even when empty.
    #[test]
    fn a_deployment_with_a_settings_marker_stores_inside_the_project() {
        let scratch = Scratch::new("deployment");
        let opt_vike = scratch.path().join("opt").join("vike");
        std::fs::create_dir_all(opt_vike.join("settings")).unwrap();

        let project = crate::state_path::project_hist_store_dir(&opt_vike);
        assert_eq!(
            project.as_deref(),
            Some(opt_vike.join("market_data").join("hist").as_path()),
            "the walk must find the deployment's own data dir"
        );

        let got = resolve_store_root(
            None,
            None,
            Path::new(NO_CHECKOUT),
            // No override — this deployment is DISCOVERED by its `settings/` marker, which is the
            // rung this test has always been about.
            project_default_from(None, project),
            Some(PathBuf::from("/home/u/.local/share/vike-data")),
        );
        assert_eq!(
            got.root,
            opt_vike.join("market_data").join("hist"),
            "a deployment's tape belongs in the deployment's own folder, not under $HOME"
        );
        assert_eq!(got.rung, StoreRootRung::Project);
    }

    /// …and WITHOUT that marker nothing changes from before this rung existed: no project, no
    /// project rung, and the per-user directory still answers. A tree with no marker anywhere is
    /// the one shape where inventing a project path would be a guess.
    #[test]
    fn a_deployment_without_a_marker_falls_through_exactly_as_before() {
        let scratch = Scratch::new("unmarked");
        let bare = scratch.path().join("opt").join("vike");
        std::fs::create_dir_all(&bare).unwrap();

        // ⚠ The system temp dir is an ancestor, and a stray `Cargo.toml` or `settings/` up there
        // would make this test assert nothing. Skip rather than pass vacuously.
        if crate::state_path::project_hist_store_dir(&bare).is_some() {
            eprintln!("skipped: a project marker exists above {}", bare.display());
            return;
        }

        let got = resolve_store_root(
            None,
            None,
            Path::new(NO_CHECKOUT),
            project_default_from(None, crate::state_path::project_hist_store_dir(&bare)),
            Some(PathBuf::from("/home/u/.local/share/vike-data")),
        );
        assert_eq!(got.root, PathBuf::from("/home/u/.local/share/vike-data"));
        assert_eq!(got.rung, StoreRootRung::UserDir);
    }

    // ---- THE WIRING: `resolve_store_root_from`, the one site rungs 4 and 5 are assembled at ----

    /// A shaped environment map, as `std::env::vars().collect()` would produce it. (Declared here
    /// as well as beside the `user_data_dir_from_vars` tests below; this block is above them.)
    fn vars_of(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
    }

    /// **THE mutation gate for B5.** `resolve_store_root`'s rungs 4 and 5 are two adjacent
    /// `Option<PathBuf>`s: transposing them compiles silently and sends gigabytes to the wrong
    /// disk. Every binary now assembles them HERE and nowhere else, so this one assertion covers
    /// all four call sites at once.
    ///
    /// The two answers are made unmistakably different — a real project directory in a scratch
    /// tree, and a per-user directory derived from a fake `$HOME` — so a transposition inside
    /// `resolve_store_root_from` reddens on the value, not on a subtle path suffix. Verified by
    /// mutation: swapping the two arguments in that function makes this test fail with the
    /// `$HOME`-derived path.
    #[test]
    fn the_wiring_puts_the_project_before_the_user_dir() {
        let scratch = Scratch::new("wiring");
        let project = scratch.path().join("proj");
        std::fs::create_dir_all(project.join(crate::state_path::PROJECT_SETTINGS_DIR)).unwrap();
        let fake_home = scratch.path().join("home");
        // ⚠ A stray `Cargo.toml` declaring `[workspace]` above the system temp dir would capture
        // the walk and make the assertion below mean something else. Say so here rather than
        // failing later with a confusing path mismatch.
        assert_eq!(
            crate::state_path::project_hist_store_dir(&project).as_deref(),
            Some(project.join("market_data").join("hist").as_path()),
            "precondition: the walk must find this scratch project"
        );

        let vars = vars_of(&[
            (HOME_VAR, fake_home.to_str().unwrap()),
            (XDG_DATA_HOME_VAR, fake_home.to_str().unwrap()),
            (LOCALAPPDATA_VAR, fake_home.to_str().unwrap()),
        ]);
        let got =
            resolve_store_root_from(None, None, Path::new(NO_CHECKOUT), Some(&project), &vars);

        assert_eq!(
            got.root,
            project.join("market_data").join("hist"),
            "the PROJECT rung must answer before the per-user directory"
        );
        assert_eq!(got.rung, StoreRootRung::Project);
        // The user dir is genuinely available and genuinely different — otherwise the assertion
        // above would pass for a transposed wiring too.
        let user = user_data_dir_from_vars(&vars).expect("the fixture supplies a per-user dir");
        assert_ne!(got.root, user, "the two rungs must be distinguishable in this fixture");
    }

    /// …and with NO project above `cwd`, the SAME call falls to the per-user directory — the other
    /// half of the transposition proof, since a wiring that hard-coded either rung would fail one
    /// of these two.
    #[test]
    fn the_wiring_falls_to_the_user_dir_when_there_is_no_project() {
        let scratch = Scratch::new("wiring-noproj");
        let bare = scratch.path().join("nowhere");
        std::fs::create_dir_all(&bare).unwrap();
        // ⚠ A stray marker above the system temp dir would make this vacuous. Skip, never pass.
        if crate::state_path::project_hist_store_dir(&bare).is_some() {
            eprintln!("skipped: a project marker exists above {}", bare.display());
            return;
        }
        let fake_home = scratch.path().join("home");
        let vars = vars_of(&[
            (HOME_VAR, fake_home.to_str().unwrap()),
            (XDG_DATA_HOME_VAR, fake_home.to_str().unwrap()),
            (LOCALAPPDATA_VAR, fake_home.to_str().unwrap()),
        ]);

        let got = resolve_store_root_from(None, None, Path::new(NO_CHECKOUT), Some(&bare), &vars);
        assert_eq!(got.root, user_data_dir_from_vars(&vars).unwrap());
        assert_eq!(got.rung, StoreRootRung::UserDir);
    }

    /// **B4 through the wiring:** `VIKE_SETTINGS_DIR` relocates the project, so the store's default
    /// moves with it. A binary cannot forget to honour it, because the lookup happens inside
    /// `resolve_store_root_from` rather than at each call site.
    #[test]
    fn the_wiring_honours_the_settings_dir_override() {
        let scratch = Scratch::new("wiring-override");
        // A resolvable project on the walk, so this proves the override BEAT it.
        let walked = scratch.path().join("walked");
        std::fs::create_dir_all(walked.join(crate::state_path::PROJECT_SETTINGS_DIR)).unwrap();
        let relocated = scratch.path().join("relocated");

        let vars = vars_of(&[(
            crate::state_path::SETTINGS_DIR_ENV,
            relocated.join(crate::state_path::PROJECT_SETTINGS_DIR).to_str().unwrap(),
        )]);
        let got = resolve_store_root_from(None, None, Path::new(NO_CHECKOUT), Some(&walked), &vars);

        assert_eq!(
            got.root,
            relocated.join("market_data").join("hist"),
            "the store must follow VIKE_SETTINGS_DIR, not stay on the walk"
        );
        // ⚠ CHANGED, and the change is the whole point: this used to report `Project`. Setting the
        // variable IS the declaration, so the rung an operator reads now says which project rung
        // answered — and `declared-project` is the one that outranks a dev checkout.
        assert_eq!(got.rung, StoreRootRung::DeclaredProject);
        assert_ne!(got.root, walked.join("market_data").join("hist"));
    }

    /// **THE DEFECT.** A project DECLARED by `$VIKE_SETTINGS_DIR` must outrank the dev-checkout
    /// hinge, which is the program's own INFERENCE that the build tree still exists at the path
    /// baked into this binary.
    #[test]
    fn a_declared_project_outranks_a_dev_checkout() {
        let scratch = Scratch::new("declared-vs-checkout");
        let walked = scratch.path().join("walked");
        std::fs::create_dir_all(walked.join(crate::state_path::PROJECT_SETTINGS_DIR)).unwrap();
        let declared = scratch.path().join("declared");
        let checkout =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("market_data").join("hist");

        let declared_vars = vars_of(&[(
            crate::state_path::SETTINGS_DIR_ENV,
            declared.join(crate::state_path::PROJECT_SETTINGS_DIR).to_str().unwrap(),
        )]);

        // ⚠ The precondition is keyed on the fixture, NOT on the thing under test: with nothing
        // declared, this `repo_default` must genuinely be a live checkout. A guard keyed on the
        // resolution itself would SKIP rather than fail under mutation.
        let without = resolve_store_root_from(None, None, &checkout, Some(&walked), &vars_of(&[]));
        assert_eq!(
            (without.root.as_path(), without.rung),
            (checkout.as_path(), StoreRootRung::DevCheckout),
            "precondition: this fixture's repo_default really is a live checkout"
        );

        let got = resolve_store_root_from(None, None, &checkout, Some(&walked), &declared_vars);
        assert_eq!(
            (got.root, got.rung),
            (declared.join("market_data").join("hist"), StoreRootRung::DeclaredProject),
            "a project DECLARED by VIKE_SETTINGS_DIR must outrank the build checkout"
        );
    }

    /// **THE NO-REGRESSION ASSERTION, and it must be exhaustive enough that a careless reorder
    /// cannot pass it.** A developer sets no variable, so their `<repo>/market_data/hist` still wins — over
    /// the DISCOVERED project rung, over the per-user directory, and whether or not the walk found a
    /// project at all.
    ///
    /// The three shapes are asserted together because each alone is passable by a different wrong
    /// ladder: dropping the hinge below the discovered project passes shape 3, and hoisting the
    /// discovered project above the hinge passes shapes 2 and 3.
    #[test]
    fn without_the_variable_a_dev_checkout_still_beats_every_project_rung() {
        let scratch = Scratch::new("no-regression");
        let walked = scratch.path().join("walked");
        std::fs::create_dir_all(walked.join(crate::state_path::PROJECT_SETTINGS_DIR)).unwrap();
        let checkout =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("market_data").join("hist");
        // ⚠ Keyed on the FIXTURE, not on the resolution: the hinge fires on the repo ROOT's
        // manifest, so that manifest existing is what makes every assertion below non-vacuous.
        assert!(
            checkout.parent().and_then(Path::parent).unwrap().join(REPO_MARKER).is_file(),
            "precondition: the repo root must carry a manifest, or the hinge cannot fire at all"
        );
        assert!(
            !checkout.is_dir(),
            "precondition: the fresh-clone shape — no market_data/hist leaf yet"
        );

        // 1. through the WIRING, with a walkable project beside it and a fully populated user dir.
        let fake_home = scratch.path().join("home");
        let vars = vars_of(&[
            (HOME_VAR, fake_home.to_str().unwrap()),
            (XDG_DATA_HOME_VAR, fake_home.to_str().unwrap()),
            (LOCALAPPDATA_VAR, fake_home.to_str().unwrap()),
        ]);
        let got = resolve_store_root_from(None, None, &checkout, Some(&walked), &vars);
        assert_eq!(
            (got.root.as_path(), got.rung),
            (checkout.as_path(), StoreRootRung::DevCheckout),
            "no variable set: the developer's own checkout still answers"
        );

        // 2. through the LADDER, with the project rung explicitly DISCOVERED.
        let got = resolve_store_root(
            None,
            None,
            &checkout,
            ProjectDefault::Discovered(PathBuf::from("project")),
            Some(PathBuf::from("user")),
        );
        assert_eq!(
            (got.root.as_path(), got.rung),
            (checkout.as_path(), StoreRootRung::DevCheckout),
            "a DISCOVERED project must never displace a live checkout"
        );

        // 3. …and with no project found at all, which must not change the answer either.
        let got = resolve_store_root(
            None,
            None,
            &checkout,
            ProjectDefault::None,
            Some(PathBuf::from("user")),
        );
        assert_eq!(
            (got.root.as_path(), got.rung),
            (checkout.as_path(), StoreRootRung::DevCheckout),
            "and the checkout still beats the per-user directory"
        );
    }

    /// **A BLANK variable is not a declaration.** An empty `Environment=VIKE_SETTINGS_DIR=` line in
    /// a unit file configures nothing, so promoting it above the dev-checkout hinge would relocate a
    /// developer's store on the strength of a value nobody set. The workspace rule is
    /// `crates/vike-model/src/state_path.rs`'s `project_settings_dir_from`, which ignores a blank
    /// override rather than honouring it; this asserts the ladder agrees.
    #[test]
    fn a_blank_settings_dir_is_not_a_declaration() {
        let scratch = Scratch::new("blank-declaration");
        let walked = scratch.path().join("walked");
        std::fs::create_dir_all(walked.join(crate::state_path::PROJECT_SETTINGS_DIR)).unwrap();
        let checkout =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("market_data").join("hist");

        for blank in ["", "   ", "\t", "\n"] {
            let vars = vars_of(&[(crate::state_path::SETTINGS_DIR_ENV, blank)]);
            let got = resolve_store_root_from(None, None, &checkout, Some(&walked), &vars);
            assert_eq!(
                (got.root.as_path(), got.rung),
                (checkout.as_path(), StoreRootRung::DevCheckout),
                "a blank VIKE_SETTINGS_DIR ({blank:?}) must behave exactly as UNSET"
            );
        }

        // …and the classifier itself, so the rule has a named home rather than only an effect.
        assert_eq!(
            project_default_from(Some("  "), Some(PathBuf::from("p"))),
            ProjectDefault::Discovered(PathBuf::from("p")),
            "a blank override leaves the walk's answer DISCOVERED"
        );
        assert_eq!(
            project_default_from(Some(" /x/settings "), Some(PathBuf::from("p"))),
            ProjectDefault::Declared(PathBuf::from("p")),
            "…and a value with surrounding whitespace is still a declaration"
        );
        assert_eq!(project_default_from(Some("/x/settings"), None), ProjectDefault::None);
    }

    /// **Rungs 1 and 2 still outrank BOTH project rungs**, proven with a declaration in play and a
    /// dev checkout underneath — the configuration where a mis-ordered insert would be invisible to
    /// every other test here. `$VIKE_HIST_STORE` names the STORE; `$VIKE_SETTINGS_DIR` names the
    /// PROJECT, and naming the store is the more specific statement.
    #[test]
    fn the_stated_rungs_still_outrank_a_declared_project() {
        let scratch = Scratch::new("stated-vs-declared");
        let walked = scratch.path().join("walked");
        std::fs::create_dir_all(walked.join(crate::state_path::PROJECT_SETTINGS_DIR)).unwrap();
        let declared = scratch.path().join("declared");
        let checkout =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("market_data").join("hist");
        let vars = vars_of(&[(
            crate::state_path::SETTINGS_DIR_ENV,
            declared.join(crate::state_path::PROJECT_SETTINGS_DIR).to_str().unwrap(),
        )]);

        // ⚠ The independent precondition: this fixture really does reach the DECLARED rung when
        // nothing is stated above it. Without this the two assertions below pass vacuously for a
        // ladder that had lost the rung entirely.
        let bare = resolve_store_root_from(None, None, &checkout, Some(&walked), &vars);
        assert_eq!(
            (bare.root, bare.rung),
            (declared.join("market_data").join("hist"), StoreRootRung::DeclaredProject),
            "precondition: the declaration is live in this fixture"
        );

        let got = resolve_store_root_from(
            Some("/x/explicit".into()),
            Some("/y/env".into()),
            &checkout,
            Some(&walked),
            &vars,
        );
        assert_eq!((got.root, got.rung), (PathBuf::from("/x/explicit"), StoreRootRung::Explicit));

        let got =
            resolve_store_root_from(None, Some("/y/env".into()), &checkout, Some(&walked), &vars);
        assert_eq!((got.root, got.rung), (PathBuf::from("/y/env"), StoreRootRung::EnvVar));
    }

    /// The wiring changes NO rung above 4: an explicit path and `$VIKE_HIST_STORE` still win, and a
    /// dev checkout still outranks the project — proven through the same entry point the binaries
    /// call, not only through the ladder underneath it.
    #[test]
    fn the_wiring_leaves_the_stated_and_checkout_rungs_untouched() {
        let scratch = Scratch::new("wiring-above");
        let project = scratch.path().join("proj");
        std::fs::create_dir_all(project.join(crate::state_path::PROJECT_SETTINGS_DIR)).unwrap();
        let checkout =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("market_data").join("hist");
        let vars = vars_of(&[]);

        let got = resolve_store_root_from(
            Some("/x/explicit".into()),
            Some("/y/env".into()),
            Path::new(NO_CHECKOUT),
            Some(&project),
            &vars,
        );
        assert_eq!((got.root, got.rung), (PathBuf::from("/x/explicit"), StoreRootRung::Explicit));

        let got = resolve_store_root_from(
            None,
            Some("/y/env".into()),
            Path::new(NO_CHECKOUT),
            Some(&project),
            &vars,
        );
        assert_eq!((got.root, got.rung), (PathBuf::from("/y/env"), StoreRootRung::EnvVar));

        let got = resolve_store_root_from(None, None, &checkout, Some(&project), &vars);
        assert_eq!((got.root, got.rung), (checkout, StoreRootRung::DevCheckout));
    }

    /// A binary that cannot read its own working directory passes `None`, and the resolution still
    /// answers — with the per-user directory, never with something CWD-relative.
    #[test]
    fn the_wiring_survives_an_unknown_working_directory() {
        let vars = vars_of(&[(HOME_VAR, "/home/u"), (XDG_DATA_HOME_VAR, "/xdg")]);
        let got = resolve_store_root_from(None, None, Path::new(NO_CHECKOUT), None, &vars);
        assert_eq!(got.root, user_data_dir_from_vars(&vars).unwrap());
        assert_eq!(got.rung, StoreRootRung::UserDir);
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_user_dir_prefers_xdg_then_home() {
        assert_eq!(
            user_data_dir(Some("/xdg"), Some("/home/u"), None),
            Some(PathBuf::from("/xdg/vike-data"))
        );
        assert_eq!(
            user_data_dir(None, Some("/home/u"), None),
            Some(PathBuf::from("/home/u/.local/share/vike-data"))
        );
        assert_eq!(user_data_dir(None, None, None), None);
        assert_eq!(user_data_dir(Some(""), Some(""), None), None, "empty is absent");
    }

    /// A shaped environment map, as `std::env::vars().collect()` would produce it.
    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
    }

    /// **The behaviour-preservation gate for the map form.** The four callers that used to paste
    /// three `std::env::var` lines now pass a map; this asserts the map form answers EXACTLY what
    /// those three arguments answered, on a unix-shaped AND a windows-shaped environment, on
    /// whichever platform the test runs. Absent, present and blank values all agree by
    /// construction, because the map form only chooses the three arguments — it re-implements none
    /// of the precedence.
    #[test]
    fn the_map_form_answers_exactly_what_the_three_argument_form_answers() {
        let cases: &[&[(&str, &str)]] = &[
            // unix-shaped: XDG set, HOME set, no LOCALAPPDATA
            &[("XDG_DATA_HOME", "/home/u/.local/share"), ("HOME", "/home/u")],
            // unix-shaped, XDG absent — the `~/.local/share` arm
            &[("HOME", "/home/u")],
            // windows-shaped: LOCALAPPDATA + HOME, no XDG
            &[("LOCALAPPDATA", "C:\\Users\\u\\AppData\\Local"), ("HOME", "C:\\Users\\u")],
            // windows-shaped, LOCALAPPDATA absent — the bare-home arm
            &[("HOME", "C:\\Users\\u")],
            // both platforms' variables present at once (MSYS/Git-Bash on Windows)
            &[
                ("XDG_DATA_HOME", "/xdg"),
                ("HOME", "/home/u"),
                ("LOCALAPPDATA", "C:\\Users\\u\\AppData\\Local"),
            ],
            // blank values must behave exactly like absent ones
            &[("XDG_DATA_HOME", ""), ("HOME", "/home/u"), ("LOCALAPPDATA", "")],
            // a scrubbed daemon environment
            &[],
        ];
        for pairs in cases {
            let vars = env(pairs);
            let want = user_data_dir(
                vars.get("XDG_DATA_HOME").map(String::as_str),
                vars.get("HOME").map(String::as_str),
                vars.get("LOCALAPPDATA").map(String::as_str),
            );
            assert_eq!(user_data_dir_from_vars(&vars), want, "diverged for {pairs:?}");
        }
    }

    /// …and the concrete answers are PINNED, not merely self-consistent: a refactor that changed
    /// both forms together would still pass the equivalence test above. These are the paths a live
    /// install resolves to today.
    #[cfg(not(windows))]
    #[test]
    fn the_pinned_unix_answers() {
        assert_eq!(
            user_data_dir_from_vars(&env(&[("XDG_DATA_HOME", "/xdg"), ("HOME", "/home/u")])),
            Some(PathBuf::from("/xdg/vike-data"))
        );
        assert_eq!(
            user_data_dir_from_vars(&env(&[("HOME", "/home/u")])),
            Some(PathBuf::from("/home/u/.local/share/vike-data"))
        );
        assert_eq!(user_data_dir_from_vars(&env(&[])), None);
    }

    #[cfg(windows)]
    #[test]
    fn the_pinned_windows_answers() {
        assert_eq!(
            user_data_dir_from_vars(&env(&[
                ("LOCALAPPDATA", "C:\\Users\\u\\AppData\\Local"),
                ("HOME", "C:\\Users\\u"),
            ])),
            Some(PathBuf::from("C:\\Users\\u\\AppData\\Local").join("vike-data"))
        );
        assert_eq!(
            user_data_dir_from_vars(&env(&[("HOME", "C:\\Users\\u")])),
            Some(PathBuf::from("C:\\Users\\u").join("vike-data"))
        );
        assert_eq!(user_data_dir_from_vars(&env(&[])), None);
    }

    #[cfg(windows)]
    #[test]
    fn windows_user_dir_prefers_localappdata() {
        assert_eq!(
            user_data_dir(None, Some("C:\\Users\\u"), Some("C:\\Users\\u\\AppData\\Local")),
            Some(PathBuf::from("C:\\Users\\u\\AppData\\Local").join("vike-data"))
        );
        assert_eq!(
            user_data_dir(None, Some("C:\\Users\\u"), None),
            Some(PathBuf::from("C:\\Users\\u").join("vike-data"))
        );
        assert_eq!(user_data_dir(None, None, None), None);
    }
}
