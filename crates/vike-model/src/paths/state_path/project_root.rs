//! The project-root walk: the settings resolver, its two markers and the manifest chain.

use std::path::{Path, PathBuf};

#[cfg(doc)]
use super::SETTINGS_DIR_ENV;
use super::{CARGO_MANIFEST, PROJECT_SETTINGS_DIR, PROJECT_USER_DATA_DIR};

/// **THE settings directory: `<project>/settings`.** The one place every setting, credential and
/// state file lives. Found by walking UP from `start` for the project root.
///
/// This is the ONLY resolver — there is no second one wrapping it and no fallback anywhere else. A
/// location the app silently used instead would be exactly the scattering this replaced.
///
/// ```text
/// <project>/settings/db/vike.db                  settings rows and credentials
/// <project>/settings/state/*.json                program-written (pace.json, runtime state)
/// ```
///
/// The four settings TOMLs this diagram used to list are gone (`docs/decisions/0086`): their keys
/// are rows in `db/vike.db` now. `state/` is a sub-directory because those files are written by
/// the PROGRAM, and a program rewriting a file a human is editing loses that human's work.
///
/// **Resolved at RUNTIME, deliberately.** The credential path used to be
/// `concat!(env!("CARGO_MANIFEST_DIR"), …)`, baked in at COMPILE time, so a binary built in one
/// checkout read that checkout's file even when run from another — and every additional checkout
/// needed its own copy. Walking up at runtime means one settings directory per project, whichever
/// checkout the binary came from.
///
/// # A project is not always a source checkout — the TWO markers, and which one wins
///
/// The first version of this walk knew exactly one marker, `Cargo.toml`, and therefore knew exactly
/// one shape of project: a source checkout. **A DEPLOYMENT is the other shape, and it broke.** All
/// three shipped units (`deploy/vike-{tradehub,datahub,recorder}.service`) run
/// `WorkingDirectory=<project>`, where the install recipe puts a BINARY and a profile and nothing
/// else — there is no `Cargo.toml` at that root or above it, so the walk returned `None`, and a
/// production daemon loaded no `policy.toml`, no `config.toml` and no credentials: every venue
/// silently on paper, with no error anywhere, because "no settings" and "settings that say nothing"
/// are indistinguishable downstream.
///
/// So there are two markers — but only ONE of them is ever strong evidence, and the rule is a
/// precedence over the STRENGTH of the evidence, not over the kind of marker:
///
/// | evidence | means | strength | matched at |
/// |---|---|---|---|
/// | a `Cargo.toml` declaring `[workspace]` | a source checkout's ROOT | **decisive** | the OUTERMOST such |
/// | a `Cargo.toml` that could not be READ | unknown — must not be guessed at | **decisive** | the OUTERMOST manifest |
/// | a `settings/` DIRECTORY | a project, self-describing | weak | the NEAREST |
/// | a `Cargo.toml` declaring no workspace | *some* crate — maybe not this one | weak | the NEAREST |
///
/// 1. **A declared `[workspace]` root decides ALONE.** No `settings/` at any depth can move it.
///    This is what keeps #1089 fixed: `cargo test -p vike-aster` runs with the CWD set to the crate
///    directory, that crate directory holds a `settings/` the moment a buggy build writes one there,
///    and the answer must still be the workspace root
///    (`the_walk_reaches_the_workspace_root_not_the_nearest_crate`,
///    `a_settings_dir_inside_a_checkout_never_beats_the_workspace_root`).
/// 2. **An UNREADABLE manifest also decides alone**, on the OUTERMOST manifest — byte-identical to
///    the rule that shipped. An unreadable file is evidence of nothing, and letting a `settings/`
///    answer instead would resolve `crates/bridges/aster/settings` for any checkout whose root
///    manifest happens to be unreadable: #1089 again, and the failure that goes silently to paper.
/// 3. **Otherwise the NEAREST marker of EITHER kind answers.** Every manifest on the chain was read
///    and none claims to be a workspace root, so no manifest is strong evidence of anything; a
///    `settings/` directory is at least evidence about ITSELF. A level holding both yields the same
///    path either way, so the tie needs no rule — see `nearest_project_marker`, where that is
///    structural rather than argued.
/// 4. `None` when neither marker exists anywhere. The caller reports that with the path it wanted;
///    this never invents a second location.
///
/// ⚠ **(3) is the fix for the DEPLOYMENT hijack, and it is narrow on purpose.** A deployment is a
/// `settings/` beside a binary with no `Cargo.toml` at that level; one unrelated `[package]`
/// manifest anywhere above it used to take it, because the manifest arm fell back to the NEAREST
/// manifest — still above the deployment — and the `settings/` arm was never reached at all.
/// Measured on the CI box with real binaries: `no store found — every venue stays paper`, and
/// `max_notional_per_order` back to its unset default, from a deployment whose own `settings/` held
/// both files. With a `settings/` beside the stray, the STRANGER'S ceiling and credentials won
/// instead. The guards are `a_stray_manifest_above_a_deployment_cannot_capture_it` and
/// `a_deployment_under_a_stray_manifest_takes_the_nearest_settings_dir`.
///
/// ⚠ **"A `settings/` beats a workspace-less manifest" is NOT the rule, and the difference is a
/// credential leak.** It must be the NEAREST marker of either kind, because the mirror-image tree
/// exists: a stray `settings/` ABOVE a plain-package project, which under a settings-first rule
/// would capture it — #1101's bug, wearing the other marker
/// (`a_stray_settings_dir_above_a_plain_package_project_cannot_capture_it`).
///
/// ⚠ **The other tempting wrong move: "prefer the nearest ancestor holding BOTH markers".** It
/// reads like the obvious cure for a stray manifest above the project, and it reopens #1089 exactly
/// — the aster crate directory holds both the moment a `settings/` appears under it, which is
/// precisely what the buggy build created there. Those two trees (a member crate under its
/// workspace root; a project under a stranger's manifest) are INDISTINGUISHABLE by marker presence
/// alone, so no marker-counting rule can separate them. Only the manifest's CONTENT can, which is
/// what [`workspace_root`] reads — and (1) above is where that content is spent.
///
/// **The one accepted residual**, pinned by
/// `a_settings_dir_inside_a_checkout_never_beats_the_workspace_root`: a deployment installed INSIDE
/// a checkout (a `settings/` strictly below a declared `[workspace]` root) resolves to the
/// CHECKOUT's settings, because that tree is byte-identical to #1089's. [`SETTINGS_DIR_ENV`] names
/// the directory outright for anyone who genuinely wants that layout.
///
/// A deployment that has not created `<project>/settings/` yet still resolves to `None` — the probe
/// is [`Path::is_dir`], so *creating the directory* is the whole fix, and [`SETTINGS_DIR_ENV`] names
/// it outright for anyone who wants no probe at all.
pub fn project_settings_dir(start: &Path) -> Option<PathBuf> {
    match ManifestChain::walk(start).decisive_root() {
        Some(root) => Some(root.join(PROJECT_SETTINGS_DIR)),
        None => nearest_project_marker(start),
    }
}

/// The NEAREST project marker at or above `start`, of EITHER kind — the answer whenever the
/// manifest chain is not decisive (see [`project_settings_dir`]'s rule 3).
///
/// Both markers resolve to the SAME expression, `<level>/settings`: a `settings/` directory is
/// itself the answer, and a `Cargo.toml` names the directory the answer sits in. So a level holding
/// both needs no tie-break — it cannot produce two answers. That is why this is one interleaved
/// walk and not two passes with a precedence bolted on top.
///
/// The walk can only ever NARROW relative to the manifest chain's own nearest answer: a `settings/`
/// can win only by being nearer than every manifest. It can never escape upward past one, which is
/// what keeps #1101's hijack fixed while curing the deployment shape.
fn nearest_project_marker(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        let settings = d.join(PROJECT_SETTINGS_DIR);
        if settings.is_dir() || d.join(CARGO_MANIFEST).is_file() {
            return Some(settings);
        }
        dir = d.parent();
    }
    None
}

/// [`project_settings_dir`] with [`SETTINGS_DIR_ENV`]'s value, which WINS over the whole walk.
///
/// The override arrives as a parameter and the CALLER reads it, per the module doc's purity
/// section: a binary that already swept `std::env::vars()` passes `vars.get(SETTINGS_DIR_ENV)`, and
/// this module still touches no environment. A blank or whitespace-only value is ignored rather than
/// honoured — an empty `Environment=VIKE_SETTINGS_DIR=` line would otherwise resolve settings to
/// `""` and read them out of the working directory, the same class of bug
/// [`crate::paths::store_path`]'s blank-value guard exists for.
pub fn project_settings_dir_from(override_dir: Option<&str>, start: &Path) -> Option<PathBuf> {
    match override_dir.map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) => Some(PathBuf::from(p)),
        None => project_settings_dir(start),
    }
}

/// **THE user-content directory: `<project>/user_data`** — see [`PROJECT_USER_DATA_DIR`] for what
/// lives under it and why it is a sibling of `settings/` rather than a child.
///
/// Resolved by the SAME walk as [`project_settings_dir`], deliberately: "which project am I in" must
/// have one answer, or a user could edit a strategy in one project while the app reads another's.
/// Reusing the walk rather than restating it is what keeps that true as the walk's rules change —
/// and they have changed three times, each time to fix a mis-resolution (#1089, #1101, and the
/// deployment shape).
///
/// The probe differs in one way that matters: [`project_settings_dir`] can answer with a `settings/`
/// directory it FOUND, since that directory is itself the marker. `user_data/` is not a marker — a
/// tree with no `user_data/` is the ordinary state of a fresh install, not evidence of a
/// mis-resolution. So this returns the path where it BELONGS, whether or not it exists yet, and the
/// caller decides between "create it" (`vike-cli init`) and "no user content" (a daemon, which has
/// none and needs none).
pub fn project_user_data_dir(start: &Path) -> Option<PathBuf> {
    Some(project_root(start)?.join(PROJECT_USER_DATA_DIR))
}

/// The directory the SIBLING folders hang off — the project root [`project_settings_dir`] resolved,
/// recovered by stripping its trailing `settings` component.
///
/// Going through the settings resolver rather than re-walking is the whole point: one walk, one
/// answer, for `settings/`, `user_data/` and `market_data/` alike. The strip is safe because every path
/// that function returns ends in [`PROJECT_SETTINGS_DIR`] — both its branches join it — and a
/// `parent()` that somehow failed leaves us with `None`, which the caller already handles.
pub(super) fn project_root(start: &Path) -> Option<PathBuf> {
    project_settings_dir(start)?.parent().map(Path::to_path_buf)
}

/// [`project_root`] under [`SETTINGS_DIR_ENV`]'s value — the project root an operator who set that
/// variable MEANT, recovered as the override's parent.
///
/// The variable names `<project>/settings`, so its parent is `<project>` and every sibling folder
/// hangs off it. Without this, `VIKE_SETTINGS_DIR` would relocate settings, credentials and state
/// while leaving `market_data/` behind on the walk — one project's configuration paired with another
/// project's tape, which is the exact disagreement the shared walk exists to prevent, and which the
/// three shipped units would have hit on the first box whose working directory was not the project.
///
/// ⚠ **A parent that comes back EMPTY is refused** (`None`, so the caller falls through), and the
/// case is real rather than theoretical: `VIKE_SETTINGS_DIR=settings` — a bare relative name, easy
/// to write in a unit file — has `""` for a parent, and joining `market_data/hist` onto that would resolve
/// the store against the WORKING DIRECTORY. That is precisely the CWD-relative default
/// [`crate::paths::store_path`] exists to eliminate, so it must not sneak back in through the override.
///
/// ⚠ **The override is a PROJECT root claim, not merely a settings location.** An operator who
/// points it at a directory that is NOT `<project>/settings` — say `/etc/vike` — gets `/etc/market_data` for
/// the sibling, which is almost certainly not what they wanted. That is stated rather than
/// second-guessed: the alternative is a heuristic on the last path component, and a heuristic here
/// would silently disagree with the very variable it claims to honour. The `config.store_root` row
/// names the store outright and outranks this whole rung for exactly that layout.
pub(super) fn project_root_from(override_dir: Option<&str>, start: &Path) -> Option<PathBuf> {
    project_settings_dir_from(override_dir, start)?
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
}

/// The WORKSPACE root above `start`: the **OUTERMOST directory whose `Cargo.toml` declares a
/// `[workspace]` table**, else — when no manifest on the chain declares one — the NEAREST directory
/// holding a `Cargo.toml`.
///
/// # Why the content, and not just the file
///
/// ⚠ **Taking the FIRST manifest is a bug, and it shipped.** Every crate has its own `Cargo.toml`,
/// and `cargo test -p vike-aster` runs with the CWD set to the CRATE directory — so a first-match
/// walk resolved `crates/bridges/aster/settings/secrets.env` and the credentials silently vanished
/// (#1089). ⚠ **Taking the OUTERMOST manifest is also a bug, and it shipped too.** One unrelated
/// `Cargo.toml` above the project — a `cargo new` at the wrong level, a parent monorepo, a vendored
/// crate directory — captured it: with no `settings/` beside the stray the project's own populated
/// store vanished, and with one the project silently read the STRANGER'S credentials.
///
/// Both bugs are the same mistake: `Cargo.toml` was treated as the marker when the QUESTION is "is
/// this the workspace root?", and only a `[workspace]` table answers that. It is cargo's own
/// definition, not a heuristic of ours:
///
/// * a MEMBER crate's manifest never carries one, so a member can never be picked — #1089 is fixed
///   by construction rather than by "climb further";
/// * an unrelated `[package]` manifest is not a workspace root, so it cannot capture a project that
///   has one — the hijack, fixed without climbing further at all.
///
/// The result is always the same directory as the previous rule's, or a DESCENDANT of it: the
/// candidates are a subset of the manifest directories, so this walk can only ever narrow, never
/// escape further.
///
/// # The three fallbacks, in order
///
/// 1. Some manifest declares `[workspace]` ⇒ the **OUTERMOST** such directory. Outermost, not
///    nearest, because a genuinely nested workspace (`crates/bridges/ctrader/protogen` carries its
///    own `[workspace]` so the drift-gate codegen stays out of the build) must not become "the
///    project" for a tool run inside it — that is the #1089 failure shape again, one level up.
/// 2. Every manifest was read and NONE declares one ⇒ the **NEAREST** manifest. A plain package
///    cannot own another plain package, so the nearest is the project and the outermost is just the
///    nearest stranger. ⚠ This arm cannot reopen #1089: that bug needs `cargo test -p <crate>`,
///    which needs a workspace, which needs a `[workspace]` table — a chain that has one never
///    reaches here.
/// 3. A manifest could NOT be read ⇒ the **OUTERMOST** manifest, byte-identical to the rule that
///    shipped. An unreadable file is evidence of nothing, and guessing "not a workspace" would mean
///    an unreadable ROOT manifest resolves to the nearest crate — #1089, the worse of the two
///    failures, since it fails silently onto paper.
///
/// ⚠ **This answers "what does the manifest chain point at", which is NOT always "where are this
/// project's settings".** Fallback (2) is a GUESS — the nearest manifest may be a stranger's, and
/// on a deployment it always is — so [`project_settings_dir`] does not consult this function; it
/// takes `ManifestChain::decisive_root` and lets a nearer `settings/` answer otherwise. This
/// function keeps the full three-fallback shape because callers ask it a different question:
/// `crates/vike-tradehub/tests/daemon/policy_ceiling_e2e.rs`'s
/// `a_ceiling_in_a_deployment_refuses_the_same_order_a_checkout_refuses` asserts through it that a
/// deployment has no manifest root at all.
pub fn workspace_root(start: &Path) -> Option<PathBuf> {
    ManifestChain::walk(start).best_effort_root().map(Path::to_path_buf)
}

/// What the chain of `Cargo.toml`s at and above a start directory says, gathered in ONE walk and
/// read by two questions with deliberately different appetites for a guess
/// (`ManifestChain::decisive_root` vs `ManifestChain::best_effort_root`).
struct ManifestChain {
    /// The nearest directory holding a manifest — a GUESS at the project, and the fallback that
    /// hijacks a deployment when the manifest is a stranger's.
    nearest: Option<PathBuf>,
    /// The outermost directory holding a manifest — used only when one could not be read.
    outermost: Option<PathBuf>,
    /// The outermost directory whose manifest declares a `[workspace]` table: cargo's OWN
    /// definition of a workspace root, and the only strong evidence on the chain.
    outermost_workspace: Option<PathBuf>,
    /// Some manifest could not be READ. Deliberately distinct from "declares no workspace".
    unreadable: bool,
}

impl ManifestChain {
    fn walk(start: &Path) -> Self {
        let mut chain =
            Self { nearest: None, outermost: None, outermost_workspace: None, unreadable: false };

        let mut dir = Some(start);
        while let Some(d) = dir {
            let manifest = d.join(CARGO_MANIFEST);
            if manifest.is_file() {
                if chain.nearest.is_none() {
                    chain.nearest = Some(d.to_path_buf());
                }
                chain.outermost = Some(d.to_path_buf());
                match declares_a_workspace(&manifest) {
                    Some(true) => chain.outermost_workspace = Some(d.to_path_buf()),
                    Some(false) => {}
                    None => chain.unreadable = true,
                }
            }
            dir = d.parent();
        }
        chain
    }

    /// The root when the manifest evidence DECIDES ALONE — a declared `[workspace]` table, or a
    /// manifest we could not read and must not second-guess. `None` means the chain is weak
    /// evidence (only plain packages, or no manifest at all) and a nearer `settings/` may answer.
    fn decisive_root(&self) -> Option<&Path> {
        match (&self.outermost_workspace, self.unreadable) {
            (Some(root), _) => Some(root.as_path()),
            (None, true) => self.outermost.as_deref(),
            (None, false) => None,
        }
    }

    /// [`Self::decisive_root`], else the NEAREST manifest — the chain's best guess, including when
    /// that guess is only "some crate lives here".
    fn best_effort_root(&self) -> Option<&Path> {
        self.decisive_root().or(self.nearest.as_deref())
    }
}

/// Does this `Cargo.toml` declare a `[workspace]` table? `None` when the file could not be READ —
/// which is deliberately distinct from `Some(false)`, because [`workspace_root`] resolves the two
/// differently.
///
/// Line-buffered and short-circuiting: the common case reads one block and stops at the header.
fn declares_a_workspace(manifest: &Path) -> Option<bool> {
    use std::io::BufRead;

    let file = std::fs::File::open(manifest).ok()?;
    let mut reader = std::io::BufReader::new(file);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return Some(false),
            // Invalid UTF-8 and I/O errors both land here: we do not know, and must not pretend.
            Err(_) => return None,
            Ok(_) if opens_the_workspace_table(&line) => return Some(true),
            Ok(_) => {}
        }
    }
}

/// Is this line a `[workspace]` / `[workspace.…]` TABLE HEADER?
///
/// Header only, on purpose. A member crate carries `workspace = "../.."` INSIDE its `[package]`
/// table — matching a bare `workspace =` assignment would read that member as a workspace root and
/// hand #1089 straight back. `# [workspace]` is prose, not a table, and does not match either.
///
/// Declared limits, both vanishingly rare in a real manifest and neither able to produce a WIDER
/// answer than the rule that shipped: a quoted header (`["workspace"]`) is not recognised, and
/// `workspace = { members = [...] }` written inline at the top level is not either — both simply
/// fall through to the fallbacks above.
fn opens_the_workspace_table(line: &str) -> bool {
    let Some(rest) = line.trim_start().strip_prefix('[') else { return false };
    let Some(rest) = rest.trim_start().strip_prefix("workspace") else { return false };
    let rest = rest.trim_start();
    rest.starts_with(']') || rest.starts_with('.')
}

/// The NEAREST existing `settings/` directory at or above `start` — the DEPLOYMENT marker, ALONE.
///
/// A deployed box is a binary, a profile and a `settings/` directory beside them; it has no
/// `Cargo.toml` anywhere, which is precisely why [`workspace_root`] cannot see it. Unlike that walk
/// this one takes the NEAREST match and returns the directory ITSELF (not its parent): a `settings/`
/// directory is self-describing, so the closest one is the most specific answer, and taking the
/// closest bounds the damage a stray `settings/` higher up can do.
///
/// ⚠ **This is the marker in isolation, not the resolver.** [`project_settings_dir`] does NOT call
/// it: a `settings/` may only answer when it is nearer than every manifest, so the two markers are
/// probed in ONE interleaved walk (`nearest_project_marker`) rather than in two passes. Reaching
/// for this function instead re-creates the bug the interleaving fixed — a stray `settings/` above a
/// plain-package project capturing it.
pub fn deployed_settings_dir(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        let candidate = d.join(PROJECT_SETTINGS_DIR);
        if candidate.is_dir() {
            return Some(candidate);
        }
        dir = d.parent();
    }
    None
}
