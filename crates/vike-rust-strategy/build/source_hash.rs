//! The SOURCE HASH a build folder is named by (`crates/vike-rust-strategy/src/artifact.rs`'s
//! `build_folder`): the text of everything the cdylib templates compile against, so a deploy that
//! changed none of it keeps every built artifact, and a change to any of it starts a new folder.
//! `build.rs` runs it (this file is its `#[path]` module) and bakes the answer into
//! `env!("VIKE_STRATEGY_SOURCE_HASH")`; `tests/source_hash.rs` runs it over trees of its own. It reads
//! no environment variable and no clock: `build.rs` hands it every input (the settings gate scores a
//! build script by its `/build.rs` suffix alone, so a read here would count as a library's).
//!
//! **What is hashed.**
//! - The crates: every dependency a template manifest names with a `path` placeholder
//!   (`crates/<name>`, the builder's rule: `crates/vike-strategy-builder/src/render.rs`'s
//!   `render_cargo_toml`), then every crate a `path` dependency of theirs reaches, transitively,
//!   through `[dependencies]` and `[build-dependencies]` (plain, per target or one table per
//!   dependency), never `[dev-dependencies]`: the closure `scripts/package_strategy_source.sh` ships.
//! - Of each crate, what a dependent's compile reads: its `Cargo.toml`, its `build.rs` and every file
//!   under its `src/` and `build/`; never its `tests/`, `benches/`, `examples/` or its prose.
//! - The builder's renderer ([`RENDERER`]), whole.
//! - The root manifest's `[workspace.dependencies]` table, where a `workspace = true` dependency
//!   resolves; the rest of that file (the members, the version, the root's own profiles) reaches no
//!   rendered crate, which is its own workspace.
//! - Every file a hashed `.rs` file names in an `include_str!`, `include_bytes!` or `include!` string
//!   literal ([`include_targets`]), wherever it lives: its bytes are compiled in like a source's.
//! - The root `Cargo.lock`'s entries of the THIRD-PARTY packages the closure compiles
//!   ([`lock_entries`]): a resolved version is linked like a source (a newer `libm` can change a
//!   strategy file's numbers), so a lock change that reaches the closure moves the folder.
//!   The builder seeds every scratch crate (and its prefetch) with the tree's own lock
//!   (`crates/vike-strategy-builder/src/render.rs`'s `seed_lock`; the strategy-source asset ships it), so
//!   the scratch build resolves to the versions this hash keys the folder by. Not `--locked`: cargo must
//!   add the scratch crate's own entry.
//!
//! Each file as its root-relative path (`/` separators) and what [`HashForm::Tokens`] reads of it, in path
//! order, an absent file as absent, spelled out byte by byte ([`source_hash`]'s encoding) into FNV-1a 64
//! ([`fnv1a64_fold`]):
//! the same tree gives the same hash under every toolchain (2026-11-19, the versioned strategy SDK's
//! step 0, `docs/decisions/0296-a-rust-strategy-file-builds-against-a-numbered-frozen-sdk-it-may-pin.md`).
//! Never `std::hash`: `DefaultHasher` and the bytes a `Hash` impl feeds it are both the compiler's to
//! change, and a snapshot's hash is compared across toolchains.
//!
//! **Comments and layout do not count** (the owner's ruling of 2026-11-20, "I don't want to recompile all
//! because of a comment"; decision 0296's amendment of that day): a Rust source is read as its tokens and
//! a manifest without its comments ([`tokens`], which says what is kept and the accepted residual), every
//! other file, and every file a hashed source reads as TEXT (`include_str!`, `include_bytes!`: its
//! comments are data), as its bytes. Until then every file counted byte for byte ([`HashForm::Bytes`]),
//! which is still how an SDK cut before it (SDK 1) is checked against its marker. The 2026-11-17
//! measurement that kept the bytes (`book/build-cache` `f84ff0c984`: 8 of 102 commits that changed a
//! hashed file changed comments or layout alone) is what the ruling weighed.
//!
//! ⚠ **Too much is safe, too little is not.** A hashed byte that reaches no artifact costs one rebuild
//! of every file; a missed one serves an artifact built against other sources. So a manifest is read
//! line by line, and every `path = "…"` in a dependency table counts, an optional or feature-gated
//! one too. A `path` that climbs out of the root names nothing this hash can read and is skipped.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

#[path = "tokens.rs"]
pub(crate) mod tokens;

/// How [`source_hash`] and [`boundary_hash`] read a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HashForm {
    /// A Rust source as its tokens and a manifest (`Cargo.toml`, a `.toml.in` template, the
    /// `[workspace.dependencies]` table) without its comments and layout ([`tokens`]); every other file, and
    /// every file a hashed source reads as text, as its bytes. How every hash is made since 2026-11-20.
    Tokens,
    /// Every file as its bytes: how an SDK cut before 2026-11-20 (SDK 1) was hashed, read only to check such
    /// an SDK's marker against its snapshot (`crates/vike-rust-strategy/src/sdk.rs`'s `SdkMarker`).
    Bytes,
}

/// The entry [`source_hash`] holds the root manifest's `[workspace.dependencies]` table under.
const WORKSPACE_DEPENDENCIES_ENTRY: &str = "Cargo.toml [workspace.dependencies]";

/// What `form` reads of the file `file` (root- or crate-relative) whose bytes are `bytes`: its tokens
/// ([`HashForm::Tokens`]) or its bytes. `read_as_text`: a hashed source names it in `include_str!` or
/// `include_bytes!`, so its comments are data and its bytes count, unless it is a template (`*.in`), whose
/// text the builder renders into a source rustc or cargo reads. A file that is no UTF-8, or a Rust source
/// that does not lex ([`tokens::rust_tokens`]), counts by its bytes too: the safe direction.
fn read_in(form: HashForm, file: &str, bytes: Vec<u8>, read_as_text: bool) -> Vec<u8> {
    if form == HashForm::Bytes || (read_as_text && !file.ends_with(".in")) {
        return bytes;
    }
    let Ok(text) = std::str::from_utf8(&bytes) else { return bytes };
    let read = if file.ends_with(".rs") || file.ends_with(".rs.in") {
        tokens::rust_tokens(text)
    } else if file.ends_with("Cargo.toml")
        || file.ends_with(".toml.in")
        || file == WORKSPACE_DEPENDENCIES_ENTRY
    {
        Some(tokens::toml_tokens(text))
    } else {
        None
    };
    read.map_or(bytes, String::into_bytes)
}

/// What [`source_hash`] answers.
pub(crate) struct SourceHash {
    /// Sixteen lowercase hex digits.
    pub(crate) hash: String,
    /// Every existing path whose change must run the hash again: each crate's manifest, build script
    /// and `src/`/`build/` folders, the renderer and the root manifest. Only paths that EXIST: cargo
    /// re-runs a build script on every build while a watched path is missing (a strategy file's own
    /// build reads a source tree that ships no renderer).
    pub(crate) watched: Vec<PathBuf>,
}

/// The two template manifests, relative to this crate's folder (`src/lib.rs`'s `TEMPLATE_CARGO_TOML` and
/// `TEMPLATE_STUDY_CARGO_TOML`): their `path` dependencies are where the hash's crates start. `build.rs`
/// reads them.
pub(crate) const TEMPLATE_MANIFESTS: [&str; 2] =
    [STRATEGY_TEMPLATE_MANIFEST, "template/study_Cargo.toml.in"];

/// The STRATEGY file's template manifest, the first of [`TEMPLATE_MANIFESTS`]: what a strategy file
/// compiles against. xtask compiles this file in by `#[path]` and walks from it alone for the SDK
/// report's crates (`xtask/src/sdk/report.rs`'s `sdk_crates`): a study is no strategy.
pub(crate) const STRATEGY_TEMPLATE_MANIFEST: &str = "template/Cargo.toml.in";

/// The builder's renderer, root-relative: it writes each crate around the templates and runs cargo on
/// it, so a change to it can change an artifact. Hashed whole, beside the crates.
pub(crate) const RENDERER: &str = "crates/vike-strategy-builder/src/render.rs";

/// The source hash of the tree at `root` for the template manifests `templates` (their text), each file read
/// in `form`.
pub(crate) fn source_hash(root: &Path, templates: &[&str], form: HashForm) -> SourceHash {
    let mut hashed: BTreeMap<String, Option<Vec<u8>>> = BTreeMap::new();
    let mut watched = Vec::new();
    // A crate's folders are watched whole (cargo walks a watched folder), its files one by one.
    let mut whole_files = vec![RENDERER.to_string()];
    let crates = closure(root, templates);
    for krate in &crates {
        whole_files.extend(["Cargo.toml", "build.rs"].map(|name| format!("{krate}/{name}")));
        for folder in ["src", "build"] {
            let relative = format!("{krate}/{folder}");
            if root.join(&relative).is_dir() {
                watched.push(root.join(&relative));
            }
            let mut found = Vec::new();
            files_under(&root.join(&relative), &relative, &mut found);
            for file in found {
                let bytes = fs::read(root.join(&file)).ok();
                hashed.insert(file, bytes);
            }
        }
    }
    let sources: Vec<(&String, &str)> = hashed
        .iter()
        .filter(|(file, _)| file.ends_with(".rs"))
        .filter_map(|(file, bytes)| Some((file, std::str::from_utf8(bytes.as_deref()?).ok()?)))
        .collect();
    let read_as_text: BTreeSet<String> = sources
        .iter()
        .flat_map(|(file, text)| named_by(file, text, &["include_str!", "include_bytes!"]))
        .collect();
    let included: BTreeSet<String> = sources
        .iter()
        .flat_map(|(file, text)| include_targets(file, text))
        .filter(|target| !hashed.contains_key(target))
        .collect();
    for file in whole_files.into_iter().chain(included) {
        let bytes = fs::read(root.join(&file)).ok();
        if bytes.is_some() {
            watched.push(root.join(&file));
        }
        hashed.insert(file, bytes);
    }
    let root_manifest = root.join("Cargo.toml");
    let table = fs::read_to_string(&root_manifest).ok().map(|text| workspace_dependencies(&text));
    if table.is_some() {
        watched.push(root_manifest);
    }
    hashed.insert(WORKSPACE_DEPENDENCIES_ENTRY.to_string(), table.map(String::into_bytes));
    let lock_path = root.join("Cargo.lock");
    let lock = fs::read_to_string(&lock_path).ok().map(|lock| {
        let manifests: Vec<String> = crates
            .iter()
            .map(|krate| {
                fs::read_to_string(root.join(krate).join("Cargo.toml")).unwrap_or_default()
            })
            .collect();
        let named: Vec<&str> =
            templates.iter().copied().chain(manifests.iter().map(String::as_str)).collect();
        lock_entries(&lock, &named)
    });
    if lock.is_some() {
        watched.push(lock_path);
    }
    hashed.insert("Cargo.lock [the closure's packages]".to_string(), lock.map(String::into_bytes));
    let read: BTreeMap<String, Option<Vec<u8>>> = hashed
        .into_iter()
        .map(|(file, bytes)| {
            let text = read_as_text.contains(&file);
            let bytes = bytes.map(|bytes| read_in(form, &file, bytes, text));
            (file, bytes)
        })
        .collect();
    SourceHash { hash: format!("{:016x}", fold_entries(&read)), watched }
}

/// The files of the strategy BOUNDARY, relative to the `vike-rust-strategy` crate's folder: the C-ABI
/// glue a host and a strategy file must agree on byte for byte (decision 0296, verdict 7), the research
/// study file's byte boundary among it (`src/study.rs`: `BytesAbi`, `SinkAbi`, `StudyHostAbi` and the two
/// study exports), plus the folders [`BOUNDARY_FOLDERS`] names.
pub(crate) const BOUNDARY_FILES: [&str; 4] =
    ["src/abi.rs", "src/guest.rs", "src/host.rs", "src/study.rs"];

/// The boundary's folders, every file under each: the guest side's face (`src/guest/face.rs`, decision
/// 0296's step 6) and the host side's children, the cdylib templates, and
/// the committed goldens of every type that crosses as text (`tests/boundary.rs` holds each type's
/// serialized shape to its golden, so a serde-shape change moves this hash with its golden), and of the
/// TYPE of every field read in place (`lent_fields.txt`: the host's bars, closed rows and ticks, defined
/// outside this crate, and the order list's rows), which `abi::layout`'s sizes and offsets cannot see.
pub(crate) const BOUNDARY_FOLDERS: [&str; 4] =
    ["src/guest", "src/host", "template", "tests/boundary"];

/// The BOUNDARY hash of the `vike-rust-strategy` crate at `krate` (its folder): [`BOUNDARY_FILES`] and
/// every file under [`BOUNDARY_FOLDERS`], each as its crate-relative path and what `form` reads of it, in
/// [`source_hash`]'s encoding: a comment edit in `abi.rs` moves it no more than the source hash
/// ([`HashForm::Tokens`]), a golden's edit does. An SDK builds as it is on a host only when the hash its
/// marker records equals the host's (`crates/vike-rust-strategy/src/sdk.rs`'s `usable`).
pub(crate) fn boundary_hash(krate: &Path, form: HashForm) -> SourceHash {
    let mut hashed: BTreeMap<String, Option<Vec<u8>>> = BTreeMap::new();
    let mut watched = Vec::new();
    for file in BOUNDARY_FILES {
        let bytes = fs::read(krate.join(file)).ok();
        if bytes.is_some() {
            watched.push(krate.join(file));
        }
        hashed.insert(file.to_string(), bytes);
    }
    for folder in BOUNDARY_FOLDERS {
        if krate.join(folder).is_dir() {
            watched.push(krate.join(folder));
        }
        let mut found = Vec::new();
        files_under(&krate.join(folder), folder, &mut found);
        for file in found {
            let bytes = fs::read(krate.join(&file)).ok();
            hashed.insert(file, bytes);
        }
    }
    for (file, bytes) in &mut hashed {
        *bytes = bytes.take().map(|bytes| read_in(form, file, bytes, false));
    }
    SourceHash { hash: format!("{:016x}", fold_entries(&hashed)), watched }
}

/// `hashed` folded into FNV-1a 64: each entry as its name, a NUL, then `0` for absent or `1`, the length
/// (eight bytes, little-endian) and the bytes, so no two trees encode alike.
fn fold_entries(hashed: &BTreeMap<String, Option<Vec<u8>>>) -> u64 {
    let mut digest = FNV_OFFSET;
    for (file, bytes) in hashed {
        digest = fnv1a64_fold(digest, file.as_bytes());
        digest = match bytes {
            None => fnv1a64_fold(digest, &[0, 0]),
            Some(bytes) => {
                digest = fnv1a64_fold(digest, &[0, 1]);
                digest = fnv1a64_fold(digest, &(bytes.len() as u64).to_le_bytes());
                fnv1a64_fold(digest, bytes)
            }
        };
    }
    digest
}

/// FNV-1a 64's offset basis: where [`fnv1a64_fold`] starts.
pub(crate) const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

/// `state` with `bytes` folded in by FNV-1a 64, the workspace's frozen identity hash (`vike_utility::fnv`:
/// offset basis [`FNV_OFFSET`], prime `0x100_0000_01b3`, xor then multiply per byte, wrapping). Spelled
/// here because this module is std-only (xtask compiles it in by `#[path]` and names no `vike-*` crate);
/// `tests/source_hash.rs` holds it equal to `vike_utility::fnv::fnv1a64`.
pub(crate) fn fnv1a64_fold(state: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(state, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3))
}

/// The crate directories (root-relative) the templates' dependencies reach: the seeds
/// ([`template_seeds`]) and, transitively, every [`path_dependencies`] entry of each, sorted.
pub(crate) fn closure(root: &Path, templates: &[&str]) -> Vec<String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut queue: Vec<String> = templates.iter().flat_map(|t| template_seeds(t)).collect();
    while let Some(krate) = queue.pop() {
        if !seen.insert(krate.clone()) {
            continue;
        }
        let manifest = fs::read_to_string(root.join(&krate).join("Cargo.toml")).unwrap_or_default();
        queue.extend(
            path_dependencies(&manifest).iter().filter_map(|dep| join_lexically(&krate, dep)),
        );
    }
    seen.into_iter().collect()
}

/// The crates a template manifest names with a `path` placeholder, as `crates/<name>`.
fn template_seeds(template: &str) -> Vec<String> {
    dependency_lines(template)
        .filter(|line| line.contains("path = \"{{"))
        .filter_map(|line| line.split_once('=').map(|(name, _)| format!("crates/{}", name.trim())))
        .collect()
}

/// Every `path = "…"` value in a manifest's dependency tables (see [`is_dependency_table`]).
fn path_dependencies(manifest: &str) -> Vec<String> {
    dependency_lines(manifest).filter_map(path_value).collect()
}

/// The lines of `manifest` inside a dependency table, comments and blank lines dropped.
fn dependency_lines(manifest: &str) -> impl Iterator<Item = &str> {
    let mut inside = false;
    manifest.lines().map(str::trim).filter(move |line| {
        if let Some(header) = line.strip_prefix('[') {
            inside = !header.starts_with('[')
                && is_dependency_table(header.split(']').next().unwrap_or_default().trim());
            return false;
        }
        inside && !line.is_empty() && !line.starts_with('#')
    })
}

/// Whether a table header (the text between the brackets) is one whose dependencies a dependent's
/// build compiles: `dependencies` and `build-dependencies`, plain, under a `target.…` table or as a
/// table of one dependency. A `dev-dependencies` header never is.
fn is_dependency_table(header: &str) -> bool {
    ["dependencies", "build-dependencies"].iter().any(|kind| {
        header == *kind
            || header.starts_with(&format!("{kind}."))
            || header.ends_with(&format!(".{kind}"))
            || header.contains(&format!(".{kind}."))
    })
}

/// The quoted value of a `path` key on `line` (`path = "../x"`, spaces optional), if one is there.
fn path_value(line: &str) -> Option<String> {
    let mut rest = line;
    while let Some(at) = rest.find("path") {
        // A whole key: `vike-paths = …` holds the word without being one.
        let whole = rest[..at].chars().last().is_none_or(|c| !c.is_alphanumeric() && c != '-');
        rest = &rest[at + "path".len()..];
        let value = rest.trim_start().strip_prefix('=').map(str::trim_start);
        if let Some(quoted) = value.and_then(|v| v.strip_prefix('"')).filter(|_| whole) {
            return quoted.split_once('"').map(|(path, _)| path.to_string());
        }
    }
    None
}

/// `base` (root-relative, `/`-separated) joined with the relative path `relative`, `.` dropped and
/// `..` taken back by name; `None` for an absolute path or one that climbs out of the root.
fn join_lexically(base: &str, relative: &str) -> Option<String> {
    if relative.starts_with(['/', '\\']) || relative.contains(':') {
        return None;
    }
    let mut parts: Vec<&str> = base.split('/').filter(|p| !p.is_empty()).collect();
    for part in relative.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            name => parts.push(name),
        }
    }
    Some(parts.join("/"))
}

/// Every file under `dir`, recursively, as `relative/…` (`/` separators), into `out`.
fn files_under(dir: &Path, relative: &str, out: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = format!("{relative}/{}", entry.file_name().to_string_lossy());
        // A link is not followed (a linked folder could loop): it is hashed as the file it reads as.
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            files_under(&entry.path(), &name, out);
        } else {
            out.push(name);
        }
    }
}

/// The files `text` (the Rust file at root-relative `file`) names with `include_str!`, `include_bytes!`
/// or `include!` and a string literal, root-relative. Read off the raw text, so a mention in a comment
/// counts too (an extra file hashed, or an absent one hashed as absent, is the safe direction); a
/// computed path (`concat!(env!("OUT_DIR"), …)`) names nothing here.
pub(crate) fn include_targets(file: &str, text: &str) -> Vec<String> {
    named_by(file, text, &["include_str!", "include_bytes!", "include!"])
}

/// The files `text` (the Rust file at root-relative `file`) names in a string literal after one of `macros`,
/// root-relative ([`include_targets`]' reading).
fn named_by(file: &str, text: &str, macros: &[&str]) -> Vec<String> {
    let folder = file.rsplit_once('/').map_or("", |(folder, _)| folder);
    let mut out = Vec::new();
    for &macro_name in macros {
        for (at, _) in text.match_indices(macro_name) {
            let rest = text[at + macro_name.len()..].trim_start();
            let Some(rest) = rest.strip_prefix('(').map(str::trim_start) else { continue };
            let Some((target, _)) = rest.strip_prefix('"').and_then(|r| r.split_once('"')) else {
                continue;
            };
            out.extend(join_lexically(folder, target));
        }
    }
    out
}

/// The `Cargo.lock` entries (`lock`, the root's) of every REGISTRY or git package the closure compiles,
/// as written, in the lock's order: each package a dependency table of `manifests` (the templates and the
/// closure's crates) names, by name, and every package such an entry lists, transitively. A version
/// resolution of a third-party crate can change an artifact's numbers as a source edit can (a newer
/// `libm`), so it moves the folder too. A PATH package's own entry is never taken nor followed: its
/// source is hashed above, and its entry lists its dev-dependencies, which reach no artifact. Too much is
/// still taken, safely: every version of a named package, and a registry dev-dependency a closure crate
/// names in a dependency table of another kind is not told apart.
pub(crate) fn lock_entries(lock: &str, manifests: &[&str]) -> String {
    let packages: Vec<LockPackage<'_>> =
        lock.split("[[package]]").skip(1).map(lock_package).collect();
    let mut queue: Vec<(String, Option<String>)> =
        manifests.iter().flat_map(|m| dependency_names(m)).map(|name| (name, None)).collect();
    let mut reached: BTreeSet<usize> = BTreeSet::new();
    while let Some((name, version)) = queue.pop() {
        for (at, package) in packages.iter().enumerate() {
            let wanted = package.registry
                && package.name == name
                && version.as_deref().is_none_or(|v| v == package.version);
            if wanted && reached.insert(at) {
                queue.extend(package.dependencies.iter().map(|d| lock_reference(d)));
            }
        }
    }
    reached.into_iter().map(|at| format!("{}\n", packages[at].text)).collect()
}

/// One `[[package]]` entry of a lock: its name, version, whether it comes from a registry or git (a path
/// package has no `source`), the entries it lists, and its text.
struct LockPackage<'a> {
    name: &'a str,
    version: &'a str,
    registry: bool,
    dependencies: Vec<&'a str>,
    text: &'a str,
}

/// The entry `block` (the text after one `[[package]]` header).
fn lock_package(block: &str) -> LockPackage<'_> {
    let value = |key: &str| {
        block.lines().find_map(|line| {
            let rest = line.trim().strip_prefix(key)?.trim_start().strip_prefix('=')?;
            rest.trim().strip_prefix('"')?.strip_suffix('"')
        })
    };
    let dependencies = block
        .split_once("dependencies = [")
        .and_then(|(_, rest)| rest.split_once(']'))
        .map(|(list, _)| {
            list.split(',').map(|item| item.trim().trim_matches('"')).filter(|d| !d.is_empty())
        })
        .into_iter()
        .flatten()
        .collect();
    LockPackage {
        name: value("name").unwrap_or_default(),
        version: value("version").unwrap_or_default(),
        registry: value("source").is_some(),
        dependencies,
        text: block.trim(),
    }
}

/// A lock's dependency reference, `name`, `name version` or `name version (source)`.
fn lock_reference(reference: &str) -> (String, Option<String>) {
    let mut words = reference.split_whitespace();
    let name = words.next().unwrap_or_default().to_string();
    (name, words.next().map(str::to_string))
}

/// The package names `manifest`'s dependency tables name: each key (`serde`, `serde.workspace`'s `serde`)
/// and a renamed one's `package = "…"`.
fn dependency_names(manifest: &str) -> Vec<String> {
    dependency_lines(manifest)
        .filter_map(|line| line.split_once('='))
        .flat_map(|(key, rest)| {
            let key = key.trim().split('.').next().unwrap_or_default().trim_matches('"');
            let renamed = rest
                .split_once("package")
                .and_then(|(_, r)| r.trim_start().strip_prefix('='))
                .and_then(|r| r.trim_start().strip_prefix('"'))
                .and_then(|r| r.split_once('"'))
                .map(|(name, _)| name.to_string());
            std::iter::once(key.to_string()).chain(renamed)
        })
        .filter(|name| !name.is_empty())
        .collect()
}

/// The root manifest's `[workspace.dependencies]` table, its header to the next table, as written.
fn workspace_dependencies(manifest: &str) -> String {
    let mut inside = false;
    let mut table = String::new();
    for line in manifest.lines() {
        if line.trim_start().starts_with('[') {
            inside = line.trim() == "[workspace.dependencies]";
        }
        if inside {
            table.push_str(line);
            table.push('\n');
        }
    }
    table
}
