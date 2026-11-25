//! The BUILD JAIL's GRANTS: what the strategy builder and every build it starts may read, write and
//! reach (`docs/decisions/0273-a-strategy-is-source-the-user-owns-vike-strategyhub-builds-it-and-runs-each-mount-in-its-own-keyless-worker.md`,
//! step 1). The mechanism that applies them — Landlock and seccomp, the probe, `run_jailed` — is
//! `crates/vike-sandbox/src/lib.rs`'s; this file says only what a cargo build needs.
//!
//! A strategy is shared source now, so its author is no longer the key holder, and `cargo` runs
//! build scripts and `rustc` reads any file a source names (`include_bytes!`). So a build sees ONLY
//! [`Grants`]: the toolchain, the strategy-source tree, a short list of system files and the cargo
//! home to read, its own scratch and the cargo home to write, and no network. The settings database,
//! credential files, the home directory and the rest of the project are outside it.
//!
//! ⚠ **No strategy's code runs in the builder, and none runs where it could write the builds directory.**
//! The builder's own serving thread is jailed with WRITE on the builds directory (it publishes artifacts),
//! so it never loads one: its cache check reads an artifact's compatibility record from the file
//! (`vike_rust_strategy::loader::check_identity`, `crate::render`'s `cached`) instead of `dlopen`ing it,
//! which would run the file's initializers (`.init_array`) here, where they could rename or link another
//! strategy's published artifact. A build runs `cargo` over the text in a child whose jail writes only its
//! work directory and the cargo home ([`grants`]): rustc compiles the text and runs none of it, and no
//! build script is the user's (the template is fixed). The strategy process does not rely on this
//! alone: it opens each artifact beneath the builds directory with no symbolic link followed and grants
//! the inode it opened (`crates/vike-sandbox/src/lib.rs`'s module doc, "The artifact grant").
//!
//! ⚠ **Where the jail cannot be applied, the builder REFUSES; it never runs unjailed.**
//! `vike_sandbox::probe` says why, and `render::build_strategy_file` turns that into
//! `BuildError::Unsandboxed`. There is no override (decision 0273). ⚠ A systemd unit whose
//! `SystemCallFilter=` lacks `@sandbox` KILLS the process at the first Landlock call (systemd 255's
//! `@system-service` does not include it). Every unit that runs the builder names `@sandbox`, AND the
//! daemon never makes its first Landlock call itself: `vike_sandbox::probe_in_child` asks a child, so
//! under a unit written before `@sandbox` only that child dies, and the daemon keeps serving and
//! refuses every build by name.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use vike_sandbox::Grants;

/// What every build reads from the system: the C toolchain and libraries, the loader's cache, the
/// user and group names, and `/proc` (Landlock keeps other processes' sensitive entries out of
/// reach). NOT `/etc` as a whole, `/home`, `/opt`, `/srv`, `/mnt`, `/var` or `/run`.
const SYSTEM_READ: &[&str] = &[
    "/usr",
    "/bin",
    "/sbin",
    "/lib",
    "/lib32",
    "/lib64",
    "/libx32",
    "/proc",
    "/sys",
    "/etc/ld.so.cache",
    "/etc/ld.so.conf",
    "/etc/ld.so.conf.d",
    "/etc/passwd",
    "/etc/group",
    "/etc/nsswitch.conf",
    "/etc/localtime",
    "/dev/null",
    "/dev/zero",
    "/dev/full",
    "/dev/random",
    "/dev/urandom",
];

/// What every build writes on the system: nothing but the null device.
const SYSTEM_WRITE: &[&str] = &["/dev/null"];

/// Extra reads the ONE networked step needs — name resolution and the CA bundle — and a jailed
/// BUILD deliberately does not get (it has no socket to use them with, and keeping them off a build
/// is the tighter jail). World-readable system config either way; a missing path is skipped.
const NETWORK_READ: &[&str] = &[
    "/etc/resolv.conf",
    "/etc/hosts",
    "/etc/gai.conf",
    "/etc/host.conf",
    "/etc/ssl",
    "/etc/pki",
    "/etc/ca-certificates",
    "/etc/ca-certificates.conf",
    "/usr/share/ca-certificates",
    "/run/systemd/resolve",
];

/// The grants for a build, or for the builder itself: the system files, `workspace_root`, the
/// toolchain `cargo_bin` and the child environment's `RUSTC`/`RUSTUP_HOME` live in, to READ; the
/// child environment's `CARGO_HOME` and `writable`, to WRITE.
///
/// ⚠ **`CARGO_HOME` must be in `child_env`.** The deployed unit always sets it (`Environment=`),
/// and without it cargo falls back to a home directory the jail does not name and the build would
/// fail to lock — so an absent one is refused here rather than guessed at. A cargo home holding a
/// registry token (`credentials.toml`) is NOT refused: the jail gives the build no network to send
/// one out with, and an artifact baking one is the run-time exfil the backtesthub still owns
/// (decision 0273, step 1's open item).
///
/// # Errors
/// `CARGO_HOME` is not in `child_env`.
pub fn grants(
    workspace_root: &Path,
    writable: &[&Path],
    cargo_bin: &str,
    child_env: &[(String, String)],
) -> Result<Grants, String> {
    let env: HashMap<&str, &str> =
        child_env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let cargo_home =
        env.get("CARGO_HOME").filter(|v| !v.trim().is_empty()).map(PathBuf::from).ok_or(
            "CARGO_HOME is not set, so a build's cargo cache could not be named in the jail",
        )?;
    let mut read: Vec<PathBuf> = SYSTEM_READ.iter().map(PathBuf::from).collect();
    read.push(workspace_root.to_path_buf());
    // When `workspace_root` is a git checkout (a hand-maintained one, or a lane's worktree), cargo
    // reads the repo's exclude file while scanning each path dependency's build script — and a
    // worktree's git dir lives OUTSIDE the checkout, so it must be granted too or the build dies on
    // "Could not read repository exclude". A deployed vendored tree is NOT a git checkout, so this
    // adds nothing there; the git dir carries source history, never a credential.
    read.extend(git_dirs(workspace_root));
    let path = env.get("PATH").copied();
    read.extend(executable(cargo_bin, path).map(|p| toolchain_root(&p)));
    read.extend(env.get("RUSTC").and_then(|r| executable(r, path)).map(|p| toolchain_root(&p)));
    read.extend(env.get("RUSTUP_HOME").map(PathBuf::from));
    let mut write: Vec<PathBuf> = SYSTEM_WRITE.iter().map(PathBuf::from).collect();
    write.push(cargo_home);
    write.extend(writable.iter().map(|p| p.to_path_buf()));
    Ok(Grants { read, write, network: false })
}

/// [`grants`] plus the network and the few extra reads it needs — the grant for the builder's ONE
/// networked step, `render::prefetch` (`cargo fetch` over the fixed template). A BUILD never gets
/// this.
///
/// # Errors
/// [`grants`]'s, unchanged.
pub fn network_grants(
    workspace_root: &Path,
    writable: &[&Path],
    cargo_bin: &str,
    child_env: &[(String, String)],
) -> Result<Grants, String> {
    let mut g = grants(workspace_root, writable, cargo_bin, child_env)?;
    g.read.extend(NETWORK_READ.iter().map(PathBuf::from));
    g.network = true;
    Ok(g)
}

/// The git director(ies) cargo reads when `workspace_root` is a checkout: the `.git` directory when
/// it is one, or — when `.git` is a FILE (a worktree) — the worktree's own git dir and the shared
/// common dir it names (where `info/exclude` lives). Empty for a non-git tree (the deployed case).
fn git_dirs(workspace_root: &Path) -> Vec<PathBuf> {
    let dot_git = workspace_root.join(".git");
    match std::fs::symlink_metadata(&dot_git) {
        Ok(m) if m.is_dir() => vec![dot_git],
        Ok(_) => {
            // A worktree: `.git` is a file `gitdir: <path>`.
            let Some(gitdir) = std::fs::read_to_string(&dot_git).ok().and_then(|c| {
                c.lines().find_map(|l| l.strip_prefix("gitdir:").map(|p| PathBuf::from(p.trim())))
            }) else {
                return Vec::new();
            };
            let mut out = Vec::new();
            // `commondir` points at the shared `.git` (usually `../..`), relative to `gitdir`.
            if let Ok(common) = std::fs::read_to_string(gitdir.join("commondir")) {
                let c = gitdir.join(common.trim());
                out.push(c.canonicalize().unwrap_or(c));
            }
            out.push(gitdir);
            out
        }
        Err(_) => Vec::new(),
    }
}

/// The first grant that would show `secret` (a grant equal to it or above it), if any — the
/// builder's startup check that its own jail hides the settings directory and the home directory.
pub fn grant_covering<'a>(grants: &'a Grants, secret: &Path) -> Option<&'a Path> {
    grants.read.iter().chain(&grants.write).map(PathBuf::as_path).find(|g| secret.starts_with(g))
}

/// `name` as a real file: a path as given, a bare name looked up on `path`; symlinks resolved, so
/// the grant covers the file the kernel will actually open.
fn executable(name: &str, path: Option<&str>) -> Option<PathBuf> {
    let found = if name.contains('/') {
        Some(PathBuf::from(name))
    } else {
        path?.split(':').map(|dir| Path::new(dir).join(name)).find(|p| p.is_file())
    }?;
    found.canonicalize().ok()
}

/// The toolchain a binary belongs to: the directory above its `bin/` (so `lib/` and the sysroot
/// come with it), or the file alone when it sits elsewhere or directly in `/bin`.
fn toolchain_root(bin: &Path) -> PathBuf {
    match bin.parent() {
        Some(dir) if dir.file_name().is_some_and(|n| n == "bin") => match dir.parent() {
            Some(root) if root != Path::new("/") => root.to_path_buf(),
            _ => bin.to_path_buf(),
        },
        _ => bin.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_toolchain_binary_grants_its_toolchain_and_a_stray_one_only_itself() {
        let root = toolchain_root(Path::new("/srv/x/bin/rust-toolchain/bin/rustc"));
        assert_eq!(root, Path::new("/srv/x/bin/rust-toolchain"));
        assert_eq!(toolchain_root(Path::new("/bin/cargo")), Path::new("/bin/cargo"));
        assert_eq!(toolchain_root(Path::new("/tmp/w/fake-cargo")), Path::new("/tmp/w/fake-cargo"));
    }

    #[test]
    fn an_absent_cargo_home_draws_no_jail_and_a_present_one_grants_scratch() {
        let none = grants(Path::new("/w"), &[], "/nowhere/cargo", &[]);
        assert!(none.is_err_and(|e| e.contains("CARGO_HOME")));
        let env = [("CARGO_HOME".to_string(), "/srv/p/cargo-home".to_string())];
        let scratch = Path::new("/tmp/build/x");
        let g = grants(Path::new("/srv/p/src"), &[scratch], "/nowhere/cargo", &env)
            .expect("CARGO_HOME present");
        assert!(g.write.iter().any(|p| p == Path::new("/srv/p/cargo-home")));
        assert!(g.write.iter().any(|p| p == scratch));
        assert!(g.read.iter().any(|p| p == Path::new("/srv/p/src")));
        assert!(!g.network);
    }

    #[test]
    fn a_grant_above_a_secret_is_found_and_a_sibling_is_not() {
        let g = Grants {
            read: vec!["/srv/p/vendor/src".into(), "/usr".into()],
            write: vec!["/srv/p".into()],
            network: false,
        };
        assert_eq!(grant_covering(&g, Path::new("/srv/p/settings")), Some(Path::new("/srv/p")));
        let only_vendor = Grants { write: Vec::new(), ..g };
        assert_eq!(grant_covering(&only_vendor, Path::new("/srv/p/settings")), None);
    }
}
