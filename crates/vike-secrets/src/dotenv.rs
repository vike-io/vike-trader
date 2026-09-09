//! The `KEY=value` store reader, and where the store lives.
//!
//! It sits in this crate rather than in `vike-bridge-core` for one dependency reason: `vike-cli
//! secrets` reads the store, and `vike-cli` is DataFusion-free, transport-free and on the FAST CI
//! lane — linking `vike-bridge-core` for a 12-line `KEY=VALUE` parser would drag `ureq`,
//! `tungstenite` and `rustls` into it. The `vike-alerting` split made exactly this argument.
//!
//! `vike_bridge_core::credentials` re-exports these functions under their historical paths, so
//! every one of the ~179 existing call sites
//! (`vike_bridge_core::credentials::load_workspace_dotenv()`) resolves unchanged.
//!
//! ⚠ **Read-only, always.** No code path in this workspace deletes, moves or rewrites the store:
//! it is the user's only copy of live venue credentials.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Minimal `KEY=VALUE` parser ('#' comments; optional surrounding quotes) — enough to read the
/// store without a dotenv dependency. Returns the map instead of mutating the process env
/// (`set_var` is unsafe under threads).
///
/// ⚠ Its expressive limits are real: the format cannot represent a value containing a newline, and
/// it strips surrounding quotes unconditionally.
pub fn parse_dotenv(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim().trim_matches('"').trim_matches('\'');
        out.insert(k.trim().to_string(), v.to_string());
    }
    out
}

/// The settings directory NAME and the credential file inside it: `<project>/settings/secrets.env`.
///
/// Duplicated from `vike_model::state_path` on purpose — this crate has **zero `vike-*`
/// dependencies** by policy, so that `vike-bridge-core` (which owns the ureq/tungstenite/rustls
/// stack) and `vike-cli` (deliberately DataFusion- and transport-free) can both link it without
/// dragging each other in. Ten duplicated lines are the cheaper side of that trade; a test pins the
/// two spellings equal.
pub const SETTINGS_DIR: &str = "settings";
/// The credential file inside [`SETTINGS_DIR`].
pub const SECRETS_FILE: &str = "secrets.env";
/// The NODE-key file inside [`SETTINGS_DIR`], beside [`SECRETS_FILE`].
///
/// ⚠ **Two files, and this is NOT a precedence chain.** The rule "one store, no chain, no second
/// location" is about one KEY having one HOME — no ladder, no "it might be here or there" — and it
/// is intact: which file a name lives in is decided STATICALLY by
/// `vike_model::credential_keys::is_platform_key`, and no name is ever looked for in both. "Where is
/// my Binance key" and "where is my node key" each still answer in one line.
///
/// WHY THEY ARE SEPARATE, measured 2026-09-08: [`SECRETS_FILE`] holds **168** venue key names
/// (`VENUES × 4 tiers × 3 suffixes`, the product `vike_model::credential_keys`'s
/// `the_credential_grid_is_the_roster_times_the_tiers_times_the_suffixes` pins) plus the bespoke FX
/// shapes, and leaking one of those means somebody signs orders with real money. This file holds
/// **4** — two services × two scopes — and leaking one means somebody reaches a data service. They
/// also grow differently: a new venue adds twelve names there and NONE here.
///
/// ⚠ What this buys is BLAST RADIUS, not access control, and the difference matters. Every
/// subcommand runs as one user with one filesystem view, so anything that can read one file can read
/// the other. What it prevents is a `backtest` process — which needs a node key and no venue key —
/// holding 168 venue secrets in memory where a core dump, a panic payload or a future logging bug
/// could reach them.
pub const NODE_FILE: &str = "node.env";
/// The sub-directory of [`SETTINGS_DIR`] holding PROGRAM-written files: `<project>/settings/state`.
///
/// Separate from the TOMLs beside it because a program rewriting a file a human is editing loses
/// that human's work. Duplicated from `vike_model::state_path::STATE_SUBDIR` for the same
/// zero-dependency reason as [`SETTINGS_DIR`]; the same test pins the two equal.
pub const STATE_DIR: &str = "state";

/// The variable that names the settings directory OUTRIGHT, skipping the walk: `VIKE_SETTINGS_DIR`.
///
/// The twin of `vike_model::state_path::SETTINGS_DIR_ENV`, duplicated for the same
/// zero-`vike-*`-dependency reason as [`SETTINGS_DIR`] above —
/// `crates/vike-bridge-core/tests/settings_dir_spellings.rs` is the test that pins the two equal
/// (it is the only crate in the tree depending on BOTH, which is why the pin lives there and not in
/// either of them).
///
/// Read by [`project_settings_dir_from`]'s CALLER: this module performs no environment read of its
/// own, so nothing here joins the settings registry's `Layer::Library` work-list.
pub const SETTINGS_DIR_ENV: &str = "VIKE_SETTINGS_DIR";

/// The project's credential store: `<project>/settings/secrets.env`, resolved at RUNTIME by walking
/// UP from `start` for the workspace root.
///
/// **Resolved at RUNTIME, deliberately.** A compile-time path would bake in whichever checkout the
/// binary was built in, so a binary built in one worktree would read that worktree's credentials
/// when run from another, and every extra checkout would need its own copy of the live signing
/// keys. Walking up at runtime means one store per project, whichever build produced the binary.
///
/// `None` when neither project marker is above `start` (see [`project_settings_dir`]). The caller
/// reports that with the path it wanted; this never resolves a second location.
pub fn project_secrets_path(start: &Path) -> Option<PathBuf> {
    Some(project_settings_dir(start)?.join(SECRETS_FILE))
}

/// [`project_secrets_path`] under [`SETTINGS_DIR_ENV`]'s value, which wins over the walk.
pub fn project_secrets_path_from(override_dir: Option<&str>, start: &Path) -> Option<PathBuf> {
    Some(project_settings_dir_from(override_dir, start)?.join(SECRETS_FILE))
}

/// The project's settings DIRECTORY — `<project>/settings` — by walking UP from `start`. Every
/// setting, credential and state file lives under it.
///
/// Exported from this crate as well as `vike_model::state_path` so that `vike-cli` — which links
/// neither `vike-model` nor `vike-bridge-core`, deliberately — can resolve it without taking a
/// dependency purely to learn a directory name. The two copies are pinned equal by
/// `crates/vike-bridge-core/tests/settings_dir_spellings.rs`.
///
/// # Two markers, because a project is not always a source checkout
///
/// Only ONE of them is ever strong evidence, so the rule is a precedence over the STRENGTH of the
/// evidence, not over the kind of marker:
///
/// | evidence | means | strength | matched at |
/// |---|---|---|---|
/// | a `Cargo.toml` declaring `[workspace]` | a source checkout's ROOT | **decisive** | the OUTERMOST such |
/// | a `Cargo.toml` that could not be READ | unknown — must not be guessed at | **decisive** | the OUTERMOST manifest |
/// | a `settings/` DIRECTORY | a project, self-describing | weak | the NEAREST |
/// | a `Cargo.toml` declaring no workspace | *some* crate — maybe not this one | weak | the NEAREST |
///
/// 1. **A declared `[workspace]` root decides ALONE** — no `settings/` at any depth moves it, which
///    is what keeps the nearest-crate bug fixed: `cargo test -p vike-aster` runs with the CWD set
///    to the crate directory, and that directory holds a `settings/` the moment a buggy build
///    writes one there (`the_walk_reaches_the_workspace_root_not_the_nearest_crate`,
///    `a_source_checkout_ignores_settings_dirs_above_and_below_its_root`).
/// 2. **An UNREADABLE manifest also decides alone**, on the OUTERMOST manifest — an unreadable file
///    is evidence of nothing, and letting a `settings/` answer instead would resolve
///    `crates/bridges/aster/settings/secrets.env` for any checkout whose root manifest happens to
///    be unreadable: #1089 again, the failure that goes silently to paper.
/// 3. **Otherwise the NEAREST marker of EITHER kind answers.** Every manifest was read and none
///    claims to be a workspace root, so no manifest is strong evidence of anything; a `settings/`
///    is at least evidence about ITSELF. A level holding both yields the same path either way — see
///    `nearest_project_marker`, where that is structural rather than argued.
/// 4. `None` when neither marker exists anywhere.
///
/// ⚠ The manifest arm asks about the manifest's CONTENT, not about which manifest is nearest or
/// outermost — both of those rules shipped and both were bugs. Every crate has a manifest, so the
/// nearest is a false positive (#1089); one unrelated manifest above the project is enough to
/// capture it, so the outermost is a false positive too
/// (`an_unrelated_outer_manifest_cannot_capture_the_project`).
/// `crates/vike-model/src/state_path.rs`'s `workspace_root` carries that rule at length.
///
/// ⚠ **(3) is the fix for the DEPLOYMENT hijack, and it is narrow on purpose.** A deployment is a
/// `settings/` beside a binary with no `Cargo.toml` at that level; one unrelated `[package]`
/// manifest anywhere above it used to take it, because the manifest arm fell back to the NEAREST
/// manifest — still above the deployment — and the `settings/` arm was never reached at all.
/// Measured on the CI box with real binaries: `no store found — every venue stays paper` from a
/// deployment whose own `settings/secrets.env` was sitting right there, and with a `settings/`
/// beside the stray, the STRANGER'S credentials instead.
///
/// ⚠ **"A `settings/` beats a workspace-less manifest" is NOT the rule, and the difference is a
/// credential leak.** It must be the NEAREST marker of either kind, because the mirror-image tree
/// exists: a stray `settings/` ABOVE a plain-package project, which a settings-first rule would let
/// capture it (`a_stray_settings_dir_above_a_plain_package_project_cannot_capture_it`).
///
/// ⚠ **The other tempting wrong move: "prefer the nearest ancestor holding BOTH markers".** It
/// reopens the nearest-crate bug exactly — the aster crate directory holds both the moment a
/// `settings/` appears under it. A member crate under its workspace root and a project under a
/// stranger's manifest are INDISTINGUISHABLE by marker presence alone; only the manifest's CONTENT
/// separates them, and (1) is where that content is spent.
///
/// **The one accepted residual**: a deployment installed INSIDE a checkout (a `settings/` strictly
/// below a declared `[workspace]` root) resolves to the CHECKOUT's settings, because that tree is
/// byte-identical to #1089's. [`SETTINGS_DIR_ENV`] names the directory outright for that layout.
pub fn project_settings_dir(start: &Path) -> Option<PathBuf> {
    match ManifestChain::walk(start).decisive_root() {
        Some(root) => Some(root.join(SETTINGS_DIR)),
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
/// can win only by being nearer than every manifest, never by escaping upward past one.
fn nearest_project_marker(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        let settings = d.join(SETTINGS_DIR);
        if settings.is_dir() || d.join(CARGO_MANIFEST).is_file() {
            return Some(settings);
        }
        dir = d.parent();
    }
    None
}

/// [`project_settings_dir`] with [`SETTINGS_DIR_ENV`]'s value, which WINS over the whole walk.
///
/// The override arrives as a PARAMETER — this crate reads no environment. A blank or
/// whitespace-only value falls through to the walk rather than resolving settings to `""` and
/// reading credentials out of the working directory.
pub fn project_settings_dir_from(override_dir: Option<&str>, start: &Path) -> Option<PathBuf> {
    match nonblank(override_dir) {
        Some(p) => Some(PathBuf::from(p)),
        None => project_settings_dir(start),
    }
}

/// [`project_settings_dir_from`] for a caller that may have **no working directory at all** — the
/// start arrives as an `Option`, because the WALK is the half that needs somewhere to start and
/// [`SETTINGS_DIR_ENV`] is not: it NAMES the directory.
///
/// # `cwd: None` is a reachable state, not a curiosity
///
/// `std::env::current_dir()` fails whenever the directory a process was started in has been removed,
/// unmounted or made unsearchable — an ordinary event for a long-lived deployment and for a
/// verification lane whose tree is replaced under it. All three shipped `deploy/*.service` units set
/// `$VIKE_SETTINGS_DIR`, so the pairing *no working directory, override in hand* is exactly the one
/// a deployment reaches.
///
/// # Why the law lives HERE and not at the caller
///
/// It is the same law `dotenv_path_for` already applies one level down for the credential FILE, and
/// it is spelled ONCE — here — because the settings DIRECTORY and the store inside it disagreeing
/// about which project this process belongs to is the whole failure. `vike_boot::boot` wrote its own
/// copy of it as `spec.cwd.and_then(..)` with no override arm at all, and the two halves of a daemon
/// then read different projects: the credentials came out of the NAMED directory
/// (`crate::resolve_project` -> [`workspace_dotenv_path_from`] -> `dotenv_path_for`, the override
/// carried the whole way with no walk), while the policy CEILINGS, the state root, the log home and
/// the startup banner all fell back to the no-project answers. `nonblank`'s own doc carries what
/// this file already paid for writing one law out twice.
///
/// # The override is NOT probed, deliberately
///
/// Exactly as [`project_settings_dir_from`] does not probe it: a named directory that is not on disk
/// resolves to that path, and the layers above report it. That is what lets `vike-cli config check`
/// FAIL a set-but-unhonoured `$VIKE_SETTINGS_DIR` by name while merely warning about a walked
/// directory that is not there — a distinction an `is_dir` probe here would erase by handing the
/// walk's answer back for a directory that is only not mounted YET.
pub fn project_settings_dir_for(override_dir: Option<&str>, cwd: Option<&Path>) -> Option<PathBuf> {
    cwd.and_then(|cwd| project_settings_dir_from(override_dir, cwd))
        .or_else(|| nonblank(override_dir).map(PathBuf::from))
}

/// "A blank override is not an override" — ONE spelling of it, because this law decides which file
/// credentials come out of and it was written out twice, in two functions, in this file. Both
/// copies agreed; a sweep deleted the `!` from the second and every test stayed green, which is the
/// standing evidence that agreement was never checked. The two callers are
/// [`project_settings_dir_from`] and [`workspace_dotenv_path_from`].
fn nonblank(v: Option<&str>) -> Option<&str> {
    v.map(str::trim).filter(|s| !s.is_empty())
}

/// The manifest FILE name, spelled once — both `ManifestChain::walk` and
/// `nearest_project_marker` probe for it.
const CARGO_MANIFEST: &str = "Cargo.toml";

/// What the chain of `Cargo.toml`s at and above a start directory says, gathered in ONE walk and
/// read by `ManifestChain::decisive_root` — the only question this crate asks of it: does the
/// manifest evidence DECIDE on its own?
///
/// It once carried a second, GUESSING reader (`best_effort_root`: `decisive_root`, else the nearest
/// manifest) behind a public `workspace_root`, plus the `nearest` field that reader needed. Nothing
/// in the workspace ever called THIS crate's copy — the live one is
/// `crates/vike-model/src/state_path.rs`'s `workspace_root`, which keeps the full three-fallback
/// shape and has a caller — so the guess, its field and its walk bookkeeping went together.
struct ManifestChain {
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
        let mut chain = Self { outermost: None, outermost_workspace: None, unreadable: false };

        let mut dir = Some(start);
        while let Some(d) = dir {
            let manifest = d.join(CARGO_MANIFEST);
            if manifest.is_file() {
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
}

/// Does this `Cargo.toml` declare a `[workspace]` table? `None` when the file could not be READ —
/// which is deliberately distinct from `Some(false)`, because `ManifestChain::decisive_root`
/// resolves the two differently.
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
/// hand the nearest-crate bug straight back. `# [workspace]` is prose, not a table, and does not
/// match either.
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

/// The credential store for the CURRENT working directory. See [`project_secrets_path`].
///
/// ⚠ **`$VIKE_SETTINGS_DIR`-BLIND**, which is the whole difference between this and
/// [`workspace_dotenv_path_from`], and it is NOT merely "the override is absent here": with no
/// readable working directory the two answer differently even though this one passes `None` to
/// the same resolver — see `dotenv_path_for` below, where that arm lives and is tested. A caller
/// holding an override must spell the `_from` twin.
pub fn workspace_dotenv_path() -> PathBuf {
    workspace_dotenv_path_from(None)
}

/// [`workspace_dotenv_path`] under [`SETTINGS_DIR_ENV`]'s value, which wins over the walk.
///
/// The override is a PARAMETER, deliberately: making `workspace_dotenv_path` read it directly
/// would be a library reading process env its caller cannot see — a new `Layer::Library` row on a
/// work-list `crates/vike-ops/tests/settings_registry.rs` pins as may-only-shrink. The composition
/// roots pass it down instead (`vike_bridge_core::credentials::load_workspace_secrets_from_env`
/// pulls it out of the one `std::env::vars()` sweep they already own).
pub fn workspace_dotenv_path_from(override_dir: Option<&str>) -> PathBuf {
    dotenv_path_for(override_dir, std::env::current_dir().ok().as_deref())
}

/// The PURE half of [`workspace_dotenv_path_from`]: the working directory arrives as a PARAMETER,
/// so every arm below — including the one that has no working directory at all — is reachable from
/// a test without `std::env::set_current_dir`, which is process-global and would race every other
/// test in this binary. The same shape `vike_boot::BootSpec`'s `cwd` field already uses.
///
/// ⚠ **`cwd: None` is where the two public spellings STOP agreeing, and it is a reachable state,
/// not a curiosity.** `std::env::current_dir()` fails when the directory a process was started in
/// has been removed, unmounted, or become unsearchable — an ordinary event for a long-lived
/// deployment and for a verification lane whose tree is replaced under it. In that state:
///
/// * `dotenv_path_for(Some("/srv/x/settings"), None)` -> `/srv/x/settings/secrets.env`, because a
///   named directory needs no walk to reach it, and
/// * `dotenv_path_for(None, None)` -> the relative last resort `settings/secrets.env`.
///
/// So `workspace_dotenv_path()` and `workspace_dotenv_path_from(Some(dir))` DISAGREE there, while
/// with a readable working directory the override short-circuits the walk and they agree by
/// construction. `with_no_working_directory_the_override_still_answers` pins both halves — the
/// disagreement is what makes an override-blind call from a root that HAS an override a defect
/// rather than a spelling preference.
///
/// ⚠ **The no-CWD arm is [`project_settings_dir_for`]'s, not a second copy of it.** This function
/// used to spell the "an override needs no walk" law inline, which left the settings DIRECTORY and
/// the store INSIDE it free to answer differently in exactly this state — and they did, through
/// `vike_boot::boot`. Adding the DIRECTORY resolver and writing this one in terms of it is what
/// makes them one law; the last resort below is the only thing left that is this function's own.
fn dotenv_path_for(override_dir: Option<&str>, cwd: Option<&Path>) -> PathBuf {
    settings_file_path_for(override_dir, cwd, SECRETS_FILE)
}

/// The path of one file inside the resolved settings directory.
///
/// ⚠ Extracted from [`dotenv_path_for`] when [`NODE_FILE`] joined [`SECRETS_FILE`], so the two
/// stores cannot resolve their directory differently. That is not tidiness: the settings walk has
/// been the source of three separate defects (#1089, #1101, and the deployment that read a
/// stranger's `settings/`), and a second copy of it would be a fourth place for the same bug to
/// live. One walk, one last-resort, two file names.
fn settings_file_path_for(override_dir: Option<&str>, cwd: Option<&Path>, file: &str) -> PathBuf {
    project_settings_dir_for(override_dir, cwd)
        .map(|dir| dir.join(file))
        // Neither a walk nor a name: the RELATIVE last resort, resolved against a working directory
        // this process does not have. It is a path, not an answer, and it is all that is left.
        .unwrap_or_else(|| PathBuf::from(SETTINGS_DIR).join(file))
}

/// The project's NODE-key store — `<project>/settings/node.env`, resolved exactly as
/// [`workspace_dotenv_path_from`] resolves the credential store.
///
/// The override is a PARAMETER for the same reason it is there: this crate reads no environment.
pub fn workspace_node_path_from(override_dir: Option<&str>) -> PathBuf {
    node_path_for(override_dir, std::env::current_dir().ok().as_deref())
}

/// The PURE half of [`workspace_node_path_from`] — the working directory arrives as a parameter, so
/// every arm is reachable from a test without `set_current_dir`.
pub fn node_path_for(override_dir: Option<&str>, cwd: Option<&Path>) -> PathBuf {
    settings_file_path_for(override_dir, cwd, NODE_FILE)
}

/// Load and parse the project's credential store.
///
/// Returns an empty map when the file is absent — callers then hit the live gate (no creds → stay
/// paper).
pub fn load_workspace_dotenv() -> HashMap<String, String> {
    load_workspace_dotenv_from(None)
}

/// [`load_workspace_dotenv`] under [`SETTINGS_DIR_ENV`]'s value, which wins over the walk — the
/// LOADER half of [`workspace_dotenv_path_from`], which shipped without one.
///
/// **The override is a PARAMETER, and that is the whole design.** Making [`load_workspace_dotenv`]
/// read the variable for itself would be a library reading process env its caller can neither see
/// nor substitute — a new `Layer::Library` row on the work-list
/// `crates/vike-ops/tests/settings_registry.rs`'s `LIBRARY_PIN` pins as may-only-shrink, so
/// `library_rows_do_not_grow` would refuse it. This function reads no environment. A caller that
/// has one hands the value down: a BINARY through
/// `vike_bridge_core::credentials::load_workspace_secrets_from_env`, out of the single
/// `std::env::vars()` sweep it already owns; a TEST binary — itself a `main`, so the read scores
/// `Layer::TestOnly` rather than `Layer::Library` — at its own call site.
///
/// ⚠ **Its absence was a live defect, not an asymmetry.** `settings/` is gitignored, so a git
/// worktree or a the CI box verification lane checks out `settings/*.toml` and never `secrets.env`;
/// every `#[ignore]`d venue smoke called the override-blind twin, resolved the empty `settings/`
/// beside it, and self-SKIPPED in silence — "no creds → stay paper" is a legitimate state, so
/// nothing was logged and nothing went red. Measured 2026-08-19 in a lane whose `VIKE_SETTINGS_DIR`
/// named a store holding the credentials: `alpaca_reconcile_smoke` reported no creds and passed.
///
/// A blank or whitespace-only value falls through to the walk — the law
/// [`workspace_dotenv_path_from`] inherits from `nonblank`. An absent store still yields an empty
/// map, exactly as the no-override twin does.
pub fn load_workspace_dotenv_from(override_dir: Option<&str>) -> HashMap<String, String> {
    match std::fs::read_to_string(workspace_dotenv_path_from(override_dir)) {
        Ok(text) => parse_dotenv(&text),
        Err(_) => HashMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pairs_comments_and_quotes() {
        let m = parse_dotenv(
            "# a comment\n\nA=1\n B = two \nC=\"quoted\"\nD='single'\nnot a pair\nE=has=equals\n",
        );
        assert_eq!(m.get("A").map(String::as_str), Some("1"));
        assert_eq!(m.get("B").map(String::as_str), Some("two"));
        assert_eq!(m.get("C").map(String::as_str), Some("quoted"));
        assert_eq!(m.get("D").map(String::as_str), Some("single"));
        assert_eq!(m.get("E").map(String::as_str), Some("has=equals"));
        assert!(!m.contains_key("not a pair"));
    }

    /// The blank-override law, tested once now that it is spelled once. A blank `VIKE_SETTINGS_DIR`
    /// must fall through to the walk rather than resolving the settings directory to `""` and
    /// reading credentials out of whatever the working directory happens to be.
    ///
    /// Deliberately a pure-function test: the sibling path helpers consult the real CWD, and
    /// `std::env::set_current_dir` is PROCESS-GLOBAL — using it here would race every other test in
    /// this binary.
    #[test]
    fn a_blank_override_is_not_an_override() {
        assert_eq!(nonblank(Some("/srv/vike-<unit>")), Some("/srv/vike-<unit>"));
        assert_eq!(nonblank(Some("  /srv/vike-<unit>  ")), Some("/srv/vike-<unit>"));
        assert_eq!(nonblank(Some("")), None, "empty is not a directory");
        assert_eq!(nonblank(Some("   \t ")), None, "and neither is whitespace");
        assert_eq!(nonblank(None), None);
    }

    /// A COMMENTED-OUT credential is the case the `||` in the skip guard exists for, and nothing
    /// tested it: the fixture above pairs its `#` comment with a line that contains no `=`, so a
    /// sweep could turn `is_empty() || starts_with('#')` into `&&` and the comment arm went dead
    /// with every assertion still passing.
    ///
    /// Under `&&` a rotated-out key comes back as an entry named `"# BINANCE_LIVE_API_KEY"`. No
    /// venue loader matches that, so nothing would trade on it — but `vike-cli secrets list` prints
    /// key NAMES, and its count is what an operator reads to confirm a rotation actually happened.
    /// A retired credential silently rejoining that list is the failure worth a test.
    #[test]
    fn a_commented_out_credential_stays_out_of_the_map() {
        let m = parse_dotenv("LIVE=in-force\n# RETIRED=old-key-material\n#ALSO_RETIRED=x\n");
        assert_eq!(m.get("LIVE").map(String::as_str), Some("in-force"));
        assert!(!m.contains_key("# RETIRED"), "a comment is not a key: {:?}", m.keys());
        assert!(!m.contains_key("RETIRED"), "and it is certainly not the key it names");
        assert!(!m.contains_key("#ALSO_RETIRED"), "with or without the space after the #");
        assert_eq!(m.len(), 1, "the count `secrets list` prints must be 1: {:?}", m.keys());
    }

    #[test]
    fn loads_without_panic_and_returns_map() {
        // The store may or may not exist in a given checkout; either way the helper must return a
        // map (empty when absent) and never panic.
        let vars = load_workspace_dotenv();
        let _n: usize = vars.len();
    }

    #[test]
    fn the_store_is_secrets_env_inside_the_projects_settings_dir() {
        let path = workspace_dotenv_path();
        assert_eq!(path.file_name().and_then(|n| n.to_str()), Some(SECRETS_FILE));
        assert_eq!(
            path.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()),
            Some(SETTINGS_DIR),
            "the credential store lives in <project>/settings/, not the project root"
        );
    }

    /// ⚠ The walk must reach the WORKSPACE root, not the first `Cargo.toml`. Every crate has one,
    /// and `cargo test -p <crate>` sets the CWD to the crate directory — so a first-match walk
    /// resolved `crates/<c>/settings/secrets.env` and the credentials silently vanished. That
    /// shipped, and this is the regression guard.
    #[test]
    fn the_walk_reaches_the_workspace_root_not_the_nearest_crate() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let crate_dir = root.join("crates").join("bridges").join("aster");
        std::fs::create_dir_all(crate_dir.join("src")).unwrap();
        std::fs::write(
            root.join("Cargo.toml"),
            "[workspace]
",
        )
        .unwrap();
        std::fs::write(
            crate_dir.join("Cargo.toml"),
            "[package]
name=\"a\"
",
        )
        .unwrap();
        let want = root.join(SETTINGS_DIR).join(SECRETS_FILE);
        // From the crate dir — where `cargo test -p aster` runs — and from its src/.
        assert_eq!(project_secrets_path(&crate_dir).as_deref(), Some(want.as_path()));
        assert_eq!(project_secrets_path(&crate_dir.join("src")).as_deref(), Some(want.as_path()));
        assert_eq!(project_secrets_path(root).as_deref(), Some(want.as_path()));
    }

    /// ⚠ **THE HIJACK.** One unrelated `Cargo.toml` a single level above a project used to capture
    /// it, because the walk kept the OUTERMOST manifest unconditionally — a `cargo new` at the
    /// wrong level, a parent monorepo or a vendored crate directory was enough. With no outer
    /// `settings/` the project's own populated store simply vanished; with one, the project
    /// silently read the OTHER tree's credentials.
    ///
    /// Reproduced end-to-end on the CI box before the fix, in exactly this shape: `vike-cli secrets
    /// list` printed `no store found — every venue stays paper` without the outer `settings/`, and
    /// listed the OUTER key and never the inner one with it.
    #[test]
    fn an_unrelated_outer_manifest_cannot_capture_the_project() {
        let tmp = tempfile::tempdir().unwrap();
        let outer = tmp.path();
        // The stray: a plain PACKAGE manifest, plus a settings/ to be stolen from.
        std::fs::write(outer.join("Cargo.toml"), "[package]\nname=\"unrelated\"\n").unwrap();
        std::fs::create_dir_all(outer.join(SETTINGS_DIR)).unwrap();
        // The project: a real cargo WORKSPACE with its own settings/.
        let proj = outer.join("proj");
        std::fs::create_dir_all(proj.join("src")).unwrap();
        std::fs::write(proj.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        std::fs::create_dir_all(proj.join(SETTINGS_DIR)).unwrap();

        let want = proj.join(SETTINGS_DIR).join(SECRETS_FILE);
        for from in [&proj, &proj.join("src")] {
            assert_eq!(
                project_secrets_path(from).as_deref(),
                Some(want.as_path()),
                "a stranger's manifest above the project must not capture its credentials"
            );
        }
    }

    /// The same escape with **no `[workspace]` table anywhere** — a single-crate project under a
    /// stray manifest. Nothing on the chain claims to be a workspace root, so the NEAREST manifest
    /// is the project and the outermost is just the nearest stranger.
    ///
    /// ⚠ This arm cannot reopen #1089: that bug needs `cargo test -p <crate>`, which needs a
    /// workspace, which needs a `[workspace]` table — and a chain that has one never reaches here.
    #[test]
    fn a_plain_package_project_under_a_stray_manifest_keeps_its_own_store() {
        let tmp = tempfile::tempdir().unwrap();
        let outer = tmp.path();
        std::fs::write(outer.join("Cargo.toml"), "[package]\nname=\"unrelated\"\n").unwrap();
        let proj = outer.join("proj");
        std::fs::create_dir_all(proj.join("src")).unwrap();
        std::fs::write(proj.join("Cargo.toml"), "[package]\nname=\"proj\"\n").unwrap();

        let want = proj.join(SETTINGS_DIR).join(SECRETS_FILE);
        assert_eq!(project_secrets_path(&proj.join("src")).as_deref(), Some(want.as_path()));
    }

    /// A COMMENTED-OUT `[workspace]` is not a workspace table — the marker is a table HEADER, and
    /// `# [workspace]` is prose. Without this the stray above would capture the project again.
    #[test]
    fn a_commented_out_workspace_table_is_not_a_workspace_root() {
        let tmp = tempfile::tempdir().unwrap();
        let outer = tmp.path();
        std::fs::write(outer.join("Cargo.toml"), "# [workspace]\n[package]\nname=\"x\"\n").unwrap();
        let proj = outer.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("Cargo.toml"), "[workspace]\n").unwrap();

        let want = proj.join(SETTINGS_DIR);
        assert_eq!(project_settings_dir(&proj).as_deref(), Some(want.as_path()));
    }

    /// A NESTED workspace resolves to the OUTERMOST one, not the nearest — this repo HAS one
    /// (`crates/bridges/ctrader/protogen` carries its own `[workspace]` table so the drift-gate
    /// codegen stays out of the build), and a tool run from inside it must still find the project's
    /// settings. This is the guard on choosing "outermost `[workspace]`" over "nearest".
    #[test]
    fn a_nested_workspace_still_resolves_to_the_outermost_one() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        let inner = root.join("crates").join("ctrader").join("protogen");
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(inner.join("Cargo.toml"), "[package]\nname=\"p\"\n[workspace]\n").unwrap();

        assert_eq!(
            project_settings_dir(&inner).as_deref(),
            Some(root.join(SETTINGS_DIR).as_path()),
            "a nested workspace must not become the project"
        );
    }

    /// An UNREADABLE manifest is evidence of NOTHING, so the walk degrades to the rule that
    /// shipped — the outermost manifest — rather than to a new answer. Guessing the other way
    /// would mean an unreadable ROOT manifest resolves to the nearest crate, which is #1089
    /// (credentials silently vanish, every venue on paper) — the worse of the two failures.
    ///
    /// Invalid UTF-8 is the portable stand-in for "cannot be read"; a mode-000 file is not.
    #[test]
    fn an_unreadable_manifest_degrades_to_the_shipped_outermost_rule() {
        let tmp = tempfile::tempdir().unwrap();
        let outer = tmp.path();
        std::fs::write(outer.join("Cargo.toml"), [0xff_u8, 0xfe, 0x00, 0x80]).unwrap();
        let proj = outer.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("Cargo.toml"), "[package]\nname=\"proj\"\n").unwrap();

        assert_eq!(
            project_settings_dir(&proj).as_deref(),
            Some(outer.join(SETTINGS_DIR).as_path()),
        );
    }

    /// ⚠ Assumes the system temp directory's own ancestry holds NEITHER marker — no `Cargo.toml`
    /// and no `settings/`. A failure here means a stray was created above `TMPDIR`, not that the
    /// walk regressed.
    #[test]
    fn the_walk_finds_the_project_root_and_refuses_when_there_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let deep = root.join("crates").join("vike-secrets").join("src");
        std::fs::create_dir_all(&deep).unwrap();
        // Neither marker anywhere above: no guess.
        assert_eq!(project_secrets_path(&deep), None);
        // Now the project exists — every depth resolves to the SAME file.
        std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        let want = root.join(SETTINGS_DIR).join(SECRETS_FILE);
        assert_eq!(project_secrets_path(&deep).as_deref(), Some(want.as_path()));
        assert_eq!(project_secrets_path(root).as_deref(), Some(want.as_path()));
    }

    /// **The DEPLOYMENT shape.** A project root holds a binary, a profile and `settings/`, and no
    /// `Cargo.toml` anywhere above it — the layout all three shipped systemd units install and run
    /// from (`WorkingDirectory=<project>`). Before the second marker this resolved to `None`, so a
    /// production daemon loaded NO credentials and every venue silently stayed on paper.
    #[test]
    fn a_deployment_without_a_cargo_toml_resolves_through_its_settings_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let opt_vike = tmp.path().join("opt").join("vike");
        std::fs::create_dir_all(opt_vike.join("bin")).unwrap();
        std::fs::create_dir_all(opt_vike.join(SETTINGS_DIR)).unwrap();

        let want = opt_vike.join(SETTINGS_DIR).join(SECRETS_FILE);
        // From the unit's WorkingDirectory, and from anywhere below it.
        assert_eq!(project_secrets_path(&opt_vike).as_deref(), Some(want.as_path()));
        assert_eq!(project_secrets_path(&opt_vike.join("bin")).as_deref(), Some(want.as_path()));
        // The marker is a DIRECTORY probe, so nothing needs to exist inside it yet.
        assert!(!want.exists());
    }

    /// ⚠ **THE DEPLOYMENT HIJACK — the half #1101 left behind.** A deployment is a `settings/`
    /// directory beside a binary with **no `Cargo.toml` at that level**, and ONE unrelated
    /// `[package]` manifest anywhere above it used to take it: with no `[workspace]` table on the
    /// chain the manifest arm falls back to the NEAREST manifest, which is still above the
    /// deployment, and the `settings/` arm was never reached at all because a manifest existed.
    ///
    /// Reproduced end-to-end on the CI box with a real `vike-cli` before the fix: from a deployment
    /// holding `settings/secrets.env`, `secrets list` printed `no store found — every venue stays
    /// paper` — the live gate, silently, because one stranger's manifest sat above the install
    /// directory.
    #[test]
    fn a_stray_manifest_above_a_deployment_cannot_capture_its_store() {
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path();
        std::fs::write(parent.join("Cargo.toml"), "[package]\nname=\"unrelated\"\n").unwrap();
        let opt_vike = parent.join("opt-vike");
        std::fs::create_dir_all(opt_vike.join("bin")).unwrap();
        std::fs::create_dir_all(opt_vike.join(SETTINGS_DIR)).unwrap();

        let want = opt_vike.join(SETTINGS_DIR).join(SECRETS_FILE);
        for from in [&opt_vike, &opt_vike.join("bin")] {
            assert_eq!(
                project_secrets_path(from).as_deref(),
                Some(want.as_path()),
                "a stranger's manifest above a deployment must not capture its credentials"
            );
        }
    }

    /// …and when the stranger has a `settings/` of its own, the deployment must still take ITS OWN
    /// — the NEAREST one. Measured on the CI box before the fix: `secrets list` printed the STRANGER'S
    /// key and never the deployment's.
    #[test]
    fn a_deployment_under_a_stray_manifest_takes_the_nearest_settings_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path();
        std::fs::write(parent.join("Cargo.toml"), "[package]\nname=\"unrelated\"\n").unwrap();
        std::fs::create_dir_all(parent.join(SETTINGS_DIR)).unwrap();
        let opt_vike = parent.join("opt-vike");
        std::fs::create_dir_all(opt_vike.join(SETTINGS_DIR)).unwrap();

        assert_eq!(
            project_settings_dir(&opt_vike).as_deref(),
            Some(opt_vike.join(SETTINGS_DIR).as_path()),
            "the deployment's own settings/ is nearer than the stranger's — it must win"
        );
    }

    /// **The cure must not overreach into #1101's bug.** A stray `settings/` ABOVE a plain-package
    /// project must not capture it: the project's own manifest is NEARER, and it is the nearest
    /// marker OF EITHER KIND that answers — not "any `settings/` outranks any workspace-less
    /// manifest", which would hand back exactly the credential theft #1101 fixed.
    #[test]
    fn a_stray_settings_dir_above_a_plain_package_project_cannot_capture_it() {
        let tmp = tempfile::tempdir().unwrap();
        let outer = tmp.path();
        std::fs::write(outer.join("Cargo.toml"), "[package]\nname=\"unrelated\"\n").unwrap();
        std::fs::create_dir_all(outer.join(SETTINGS_DIR)).unwrap();
        let proj = outer.join("proj");
        std::fs::create_dir_all(proj.join("src")).unwrap();
        std::fs::write(proj.join("Cargo.toml"), "[package]\nname=\"proj\"\n").unwrap();
        // …and the project has NOT created its settings/ yet: the stray's is the only one that
        // EXISTS, which is precisely when a settings-first rule would reach for it.

        let want = proj.join(SETTINGS_DIR).join(SECRETS_FILE);
        for from in [&proj, &proj.join("src")] {
            assert_eq!(
                project_secrets_path(from).as_deref(),
                Some(want.as_path()),
                "the project's own manifest is nearer than the stray settings/ — it must win"
            );
        }
    }

    /// **An UNREADABLE manifest still decides ALONE**, so a crate-level `settings/` cannot answer
    /// for a checkout whose root manifest could not be read. This is the guard on NOT applying the
    /// nearest-marker rule to every case: the aster crate directory holds BOTH markers, so a walk
    /// that let `settings/` compete here would resolve `crates/bridges/aster/settings` — #1089
    /// exactly, credentials silently gone and every venue on paper.
    #[test]
    fn an_unreadable_root_manifest_still_outranks_a_crate_level_settings_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("Cargo.toml"), [0xff_u8, 0xfe, 0x00, 0x80]).unwrap();
        let crate_dir = root.join("crates").join("bridges").join("aster");
        std::fs::create_dir_all(crate_dir.join("src")).unwrap();
        std::fs::write(crate_dir.join("Cargo.toml"), "[package]\nname=\"a\"\n").unwrap();
        std::fs::create_dir_all(crate_dir.join(SETTINGS_DIR)).unwrap();

        assert_eq!(
            project_settings_dir(&crate_dir.join("src")).as_deref(),
            Some(root.join(SETTINGS_DIR).as_path()),
            "an unreadable manifest is evidence of nothing — it must not let a crate-level \
             settings/ answer, which is #1089"
        );
    }

    /// The deployment marker is the NEAREST `settings/`, so a stray one higher up cannot capture a
    /// deployment that has its own.
    #[test]
    fn the_deployment_marker_is_the_nearest_settings_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let stray = tmp.path().join("srv");
        let opt_vike = stray.join("opt").join("vike");
        std::fs::create_dir_all(opt_vike.join(SETTINGS_DIR)).unwrap();
        std::fs::create_dir_all(stray.join(SETTINGS_DIR)).unwrap();

        assert_eq!(
            project_settings_dir(&opt_vike).as_deref(),
            Some(opt_vike.join(SETTINGS_DIR).as_path()),
        );
    }

    /// **A checkout is unaffected by either kind of `settings/` stray** — the "dev behaviour does
    /// not change" half of the precedence rule, in the two configurations that could break it: a
    /// `settings/` at a CRATE level (nearer than the workspace root) and one ABOVE the checkout.
    #[test]
    fn a_source_checkout_ignores_settings_dirs_above_and_below_its_root() {
        let tmp = tempfile::tempdir().unwrap();
        let outer = tmp.path();
        std::fs::create_dir_all(outer.join(SETTINGS_DIR)).unwrap(); // a stray ABOVE the checkout
        let root = outer.join("checkout");
        let crate_dir = root.join("crates").join("bridges").join("aster");
        std::fs::create_dir_all(crate_dir.join("src")).unwrap();
        std::fs::create_dir_all(crate_dir.join(SETTINGS_DIR)).unwrap(); // …and one BELOW the root
        std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        std::fs::write(crate_dir.join("Cargo.toml"), "[package]\nname=\"a\"\n").unwrap();

        let want = root.join(SETTINGS_DIR).join(SECRETS_FILE);
        for from in [&root, &crate_dir, &crate_dir.join("src")] {
            assert_eq!(
                project_secrets_path(from).as_deref(),
                Some(want.as_path()),
                "the workspace root must win over every settings/ stray"
            );
        }

        // …including a `settings/` at a level with NO manifest of its own — deployment-SHAPED, but
        // inside a checkout. **The one accepted residual, pinned rather than left to drift**: it is
        // byte-identical to the #1089 tree, so no marker rule can serve both and a declared
        // workspace root must keep winning. `VIKE_SETTINGS_DIR` is the way to deploy inside one.
        let inside = root.join("deploy").join("vike");
        std::fs::create_dir_all(inside.join(SETTINGS_DIR)).unwrap();
        assert_eq!(
            project_secrets_path(&inside).as_deref(),
            Some(want.as_path()),
            "a declared [workspace] root decides alone — relaxing that is #1089"
        );
    }

    /// A `settings` FILE is not the marker — the probe is `is_dir`, not `exists`.
    #[test]
    fn a_settings_file_is_not_the_deployment_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("deploy");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(SETTINGS_DIR), "not a directory").unwrap();
        assert_eq!(project_settings_dir(&dir), None);
    }

    /// **The override wins over both markers**; blank falls through to the walk.
    #[test]
    fn the_settings_dir_override_beats_the_walk() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        let elsewhere = root.join("elsewhere");

        assert_eq!(project_settings_dir_from(elsewhere.to_str(), root), Some(elsewhere.clone()));
        assert_eq!(
            project_secrets_path_from(elsewhere.to_str(), root),
            Some(elsewhere.join(SECRETS_FILE)),
        );

        let want = root.join(SETTINGS_DIR);
        for blank in [None, Some(""), Some("  \t ")] {
            assert_eq!(project_settings_dir_from(blank, root).as_deref(), Some(want.as_path()));
        }
    }

    /// The override reaches the CWD-based entry point too — the one `resolve_project` is built on
    /// — without this module ever reading the environment.
    #[test]
    fn the_override_reaches_the_cwd_entry_point() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("explicit");
        assert_eq!(
            workspace_dotenv_path_from(dir.to_str()),
            dir.join(SECRETS_FILE),
            "an explicit settings dir must not be second-guessed by the CWD walk"
        );
        // …and no override is byte-identical to the historical entry point.
        assert_eq!(workspace_dotenv_path_from(None), workspace_dotenv_path());
    }

    /// **With no working directory the two public spellings DISAGREE — that is the whole reason a
    /// caller holding an override may not reach for the blind one.**
    ///
    /// It is tempting to argue that [`workspace_dotenv_path`] "is just `workspace_dotenv_path_from`
    /// under `None`, so it cannot answer differently". It is the same function under a DIFFERENT
    /// argument, and the argument decides: with a readable CWD the override short-circuits the walk
    /// and the answers coincide, but `std::env::current_dir()` FAILS when the directory a process
    /// started in was removed, unmounted or made unsearchable — and there the walk contributes
    /// nothing, so the override is the only thing left that can name a directory. `None` then falls
    /// all the way through to the RELATIVE last resort, which is a different file, resolved against
    /// a working directory this process does not have.
    ///
    /// The CWD is a parameter here rather than something this test sets, because
    /// `std::env::set_current_dir` is process-global and would race every other test in this binary
    /// — the reason [`dotenv_path_for`] exists at all.
    ///
    /// ⚠ The `assert_ne!` is the load-bearing line: an implementation that dropped the override on
    /// the no-CWD arm would still satisfy both `assert_eq!`s above it if the last resort happened to
    /// match, and this is what refuses that.
    #[test]
    fn with_no_working_directory_the_override_still_answers() {
        let named = "/srv/vike-<unit>/settings";
        let last_resort = PathBuf::from(SETTINGS_DIR).join(SECRETS_FILE);

        assert_eq!(
            dotenv_path_for(Some(named), None),
            Path::new(named).join(SECRETS_FILE),
            "a NAMED settings directory needs no walk to reach it"
        );
        assert_eq!(
            dotenv_path_for(None, None),
            last_resort,
            "with neither a walk nor a name, the relative last resort is all that is left"
        );
        assert_ne!(
            dotenv_path_for(Some(named), None),
            dotenv_path_for(None, None),
            "the override-blind spelling cannot produce the named store — so a caller that HAS an \
             override and calls `workspace_dotenv_path()` reports a file nothing reads"
        );

        // A blank override is still not an override, on this arm as on every other.
        for blank in [Some(""), Some("  \t ")] {
            assert_eq!(dotenv_path_for(blank, None), last_resort);
        }
    }

    /// **The DIRECTORY resolver applies the same law as the FILE one, on the same arm** — the half
    /// that did not exist, and whose absence let `vike_boot::boot` answer `None` for the settings
    /// directory on a box whose credential store it was simultaneously opening by name.
    ///
    /// Four inputs, because the law has four cases and only one of them was ever in doubt: a walk
    /// with a start, a walk WITHOUT one, a name with a start, and a NAME WITHOUT ONE. The fourth is
    /// where the fix lives, and it is the assertion that goes red without it.
    ///
    /// The loop at the end is the other load-bearing half: the two resolvers must not merely both be
    /// correct, they must be the SAME law — a `secrets.env` resolved into a directory the settings
    /// loader believes does not exist is the split this pairing exists to prevent, and it is exactly
    /// what a second inline copy of the law produced.
    #[test]
    fn the_directory_resolver_honours_an_override_with_no_working_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        let named = root.join("elsewhere");

        assert_eq!(
            project_settings_dir_for(None, Some(root)).as_deref(),
            Some(root.join(SETTINGS_DIR).as_path()),
            "with a start and no name, the walk answers exactly as it always did"
        );
        assert_eq!(
            project_settings_dir_for(None, None),
            None,
            "with neither a start nor a name there is nothing to answer with"
        );
        assert_eq!(
            project_settings_dir_for(named.to_str(), Some(root)).as_deref(),
            Some(named.as_path()),
            "a name beats the walk, as it does in `project_settings_dir_from`"
        );
        assert_eq!(
            project_settings_dir_for(named.to_str(), None).as_deref(),
            Some(named.as_path()),
            "…and a name needs NO walk, so losing the working directory cannot drop it"
        );

        // A blank override is not an override here either — it must not resolve the settings
        // directory to `""`, which is the working directory this arm does not have.
        for blank in [Some(""), Some("  \t ")] {
            assert_eq!(project_settings_dir_for(blank, None), None, "{blank:?}");
        }

        // …and the FILE resolver is this one plus a join, on every arm above. One law, two names.
        for (o, cwd) in
            [(None, Some(root)), (None, None), (named.to_str(), Some(root)), (named.to_str(), None)]
        {
            if let Some(dir) = project_settings_dir_for(o, cwd) {
                assert_eq!(
                    dotenv_path_for(o, cwd),
                    dir.join(SECRETS_FILE),
                    "the store must sit inside the directory the settings loader is handed"
                );
            }
        }
    }

    /// **The LOADER honours the override, which is the defect this pair was added for.** A path
    /// helper that resolves the right file is worth nothing to a caller that then loads through an
    /// override-blind reader — which is what every venue smoke did, silently skipping in a worktree
    /// whose gitignored `settings/` holds no `secrets.env`.
    ///
    /// Plants a store in a directory the CWD walk could never reach, so a regression cannot pass by
    /// accident of this checkout having credentials of its own.
    #[test]
    fn the_loader_reads_the_named_settings_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let settings = tmp.path().join("named-settings");
        std::fs::create_dir_all(&settings).unwrap();
        std::fs::write(settings.join(SECRETS_FILE), "BINANCE_DEMO_API_KEY=from-the-override\n")
            .unwrap();

        let vars = load_workspace_dotenv_from(settings.to_str());
        assert_eq!(
            vars.get("BINANCE_DEMO_API_KEY").map(String::as_str),
            Some("from-the-override"),
            "a named settings directory must supply the credentials"
        );

        // No override is byte-identical to the historical entry point — the walk still answers.
        assert_eq!(load_workspace_dotenv_from(None), load_workspace_dotenv());
        // …and so is a blank one: it configured nothing, so it must not resolve settings to `""`.
        for blank in [Some(""), Some("  \t ")] {
            assert_eq!(load_workspace_dotenv_from(blank), load_workspace_dotenv());
        }
    }

    /// A named directory with no store in it is the ordinary unconfigured state, not an error —
    /// the live gate (no creds → stay paper), and the reason this loader is infallible.
    #[test]
    fn a_named_settings_directory_with_no_store_is_an_empty_map() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(load_workspace_dotenv_from(tmp.path().to_str()).is_empty());
    }
}
