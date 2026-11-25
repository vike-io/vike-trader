//! The build jail (`vike_sandbox` applies it, `vike_strategy_builder::jail` grants it; decision
//! 0273, step 1): a strategy's source, compiled by the builder, may read no file but the toolchain,
//! the strategy-source tree and the build's own dirs, and may open no socket. Two proofs:
//!
//! - cheap and un-ignored (it compiles NOTHING): a closure run through [`vike_sandbox::run_jailed`]
//!   cannot connect a socket, cannot read a file outside its grants, and CAN read a granted one. This
//!   is the mechanism, proven directly, on every PR — the lane's kernel offers Landlock ABI 4.
//! - lane-only (`#[ignore]`, it runs cargo): a hostile source whose `include_bytes!` reaches a
//!   credential-shaped file OUTSIDE the jail fails to build and leaves no artifact carrying its
//!   bytes; a normal strategy still builds and loads.
//!
//! Linux only: the jail is Linux's, and off Linux every build is refused by name (the
//! `cfg(not(target_os = "linux"))` arm of `vike_sandbox`), which `render.rs`'s `BuildError::Unsandboxed`
//! unit coverage and the daemon's startup refusal already carry.
#![cfg(target_os = "linux")]

use std::io::Write as _;
use std::net::TcpStream;
use std::path::{Path, PathBuf};

use vike_sandbox::Grants;

/// Skip loudly on a box without Landlock ABI 4 rather than fail: the mechanism cannot be proven
/// where it does not exist, and the daemon refuses every build there anyway (its own test). Returns
/// whether to go on.
fn jail_available() -> bool {
    match vike_sandbox::probe() {
        Ok(()) => true,
        Err(why) => {
            eprintln!("SKIP: this box cannot apply the build jail ({why}); nothing to prove here");
            false
        }
    }
}

/// THE mechanism, proven directly and cheaply: inside the jail, a socket connect and a read outside
/// the grants both fail, and a read inside them succeeds. No cargo, so this gates every PR.
#[test]
fn the_jail_denies_a_socket_and_a_file_outside_its_grants() {
    if !jail_available() {
        return;
    }
    // A readable file INSIDE a granted directory, and a secret OUTSIDE every grant.
    let granted = tempfile::tempdir().expect("granted dir");
    let inside = granted.path().join("ok.txt");
    std::fs::write(&inside, b"readable").expect("write the granted file");
    let secret_dir = tempfile::tempdir().expect("secret dir");
    let secret = secret_dir.path().join("vike.db");
    std::fs::write(&secret, b"pretend-credential").expect("write the secret");

    // A listener the jailed closure will TRY (and fail) to reach — bound on THIS thread, before the
    // jail, so a failure to connect is the jail's doing and not a missing server.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind a victim listener");
    let victim = listener.local_addr().expect("victim addr");

    let granted_path = granted.path().to_path_buf();
    let secret_path = secret.clone();
    let inside_path = inside.clone();
    let grants = Grants {
        // Read the system libraries (so the thread can run at all) and the granted dir; write the
        // granted dir. No secret dir, no network.
        read: vec![
            PathBuf::from("/usr"),
            PathBuf::from("/lib"),
            PathBuf::from("/lib64"),
            granted_path.clone(),
        ],
        write: vec![granted_path],
        network: false,
    };
    let report = vike_sandbox::run_jailed(&grants, move || {
        let socket = TcpStream::connect(victim).is_err();
        let secret_denied = std::fs::read(&secret_path).is_err();
        let granted_ok = std::fs::read(&inside_path).is_ok_and(|b| b == b"readable");
        (socket, secret_denied, granted_ok)
    })
    .expect("the jail was applied");
    let (socket_denied, secret_denied, granted_ok) = report;
    assert!(socket_denied, "the jailed closure opened a TCP socket — the network is not closed");
    assert!(
        secret_denied,
        "the jailed closure read a file OUTSIDE its grants — the file jail leaks"
    );
    assert!(granted_ok, "the jailed closure could not read a GRANTED file — the jail is too tight");
}

// --- the lane-only end-to-end halves (they run cargo) -------------------------------------------

/// The checkout root, for `build_strategy_file`'s `workspace_root`. `env!("CARGO_MANIFEST_DIR")` in a
/// `tests/` file is one of the always-correct sites
/// `crates/vike-ops/tests/hygiene/compile_time_path_gate.rs`'s own doc names.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn cargo_bin() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string())
}

/// The service's own allowlist over this test's environment, with `CARGO_HOME` ensured (the jail
/// needs it, as a deployed unit always sets it): the real one, else `$HOME/.cargo` — the WARM home
/// the lane built the workspace with, so the jailed `--offline` build finds its crates.
fn child_env() -> Vec<(String, String)> {
    let mut vars: std::collections::HashMap<String, String> = std::env::vars().collect();
    if !vars.get("CARGO_HOME").is_some_and(|v| !v.trim().is_empty())
        && let Some(home) = vars.get("HOME").filter(|h| !h.trim().is_empty())
    {
        vars.insert("CARGO_HOME".to_string(), format!("{home}/.cargo"));
    }
    vike_strategy_builder::render::child_env(&vars)
}

/// Every regular file under `dir`, recursively — to scan the artifact (and any scratch left) for a
/// leaked secret.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else { continue };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out
}

/// A hostile strategy whose source reads a credential-shaped file at compile time
/// (`include_bytes!`) fails to build — the jail denies the read — and no artifact lands carrying
/// those bytes. This is the exact hole decision 0273 step 1 closes: before the jail, this source
/// would compile and bake every byte of `settings/db/vike.db` into the `.so`.
#[test]
#[ignore = "invokes cargo; lane-only"]
fn a_source_that_reads_a_credential_file_fails_to_build_and_leaks_nothing() {
    if !jail_available() {
        return;
    }
    // A secret with a DISTINCTIVE marker, OUTSIDE every grant (its own tempdir, under no granted
    // tree). A build that read it would put the marker in the artifact.
    let secret_dir = tempfile::tempdir().expect("secret dir");
    let secret = secret_dir.path().join("vike.db");
    let marker = "STOLEN-CREDENTIAL-7f3a-must-never-reach-an-artifact";
    let mut f = std::fs::File::create(&secret).expect("write the secret");
    writeln!(f, "{marker}").expect("write the secret marker");

    let src = format!(
        "use vike_model::{{OrderBroker, Strategy}};\n\
         use vike_marketdata::Bar;\n\
         const _STOLEN: &[u8] = include_bytes!({:?});\n\
         pub struct H;\n\
         impl<B: OrderBroker> Strategy<B> for H {{\n\
         \x20   fn on_bar(&mut self, _b: &mut B, _bar: &Bar) {{ let _ = _STOLEN.len(); }}\n\
         }}\n\
         pub fn build<B: vike_model::OrderBroker + 'static>(\n\
         \x20   _p: &vike_model::params::Params,\n\
         ) -> Box<dyn vike_model::Strategy<B> + Send> {{ Box::new(H) }}\n",
        secret.display()
    );

    let out = tempfile::tempdir().expect("out dir");
    let err = vike_strategy_builder::render::build_strategy_file(
        &src,
        "hostile_reader.rs",
        out.path(),
        &workspace_root(),
        &cargo_bin(),
        &child_env(),
        vike_strategy_builder::render::Profile::Debug,
        &vike_strategy_builder::render::CargoHomePin::disabled(),
    )
    .expect_err("a source reading a file outside the jail must not build");
    // A compile error (rustc could not read the denied file), not a jail-probe refusal — the jail
    // IS applied here, and it is what makes rustc's read fail. The message must name THE READ (rustc
    // reports `couldn't read <path>: Permission denied`) so this cannot pass on an unrelated compile
    // error — e.g. a build that broke at a compiler WRAPPER before ever reaching the `include_bytes!`.
    let vike_strategy_builder::render::BuildError::Compile { diagnostics } = &err else {
        panic!("the build must fail at the denied read, got {err:?}");
    };
    assert!(
        diagnostics.contains("couldn't read") || diagnostics.contains("vike.db"),
        "the build must fail AT the denied `include_bytes!` read, not elsewhere:\n{diagnostics}"
    );
    // Nothing the build touched carries the secret: no artifact in out_dir, and no file of its own
    // build directory, the one scratch directory the jail let it write (with the cargo home). Not
    // the whole scratch root: every other build's directories are there (104 GB on the latency box), and a
    // byte scan of them all ran for many minutes.
    let mut searched = files_under(out.path());
    searched.extend(files_under(
        &vike_strategy_builder::render::work_directory_of(
            out.path(),
            &workspace_root(),
            vike_rust_strategy::sdk::TemplateKind::Strategy,
            vike_strategy_builder::render::Profile::Debug,
        )
        .expect("the checkout is a source tree"),
    ));
    for path in searched {
        let Ok(bytes) = std::fs::read(&path) else { continue };
        assert!(
            !bytes.windows(marker.len()).any(|w| w == marker.as_bytes()),
            "the secret marker reached {}",
            path.display()
        );
    }
}

/// A normal strategy still builds and loads — the control the hostile half needs so a jail that
/// refused EVERYTHING would not pass for closing the hole. The ma_cross fixture is an ordinary user
/// strategy; it is built here under this binary's own profile so the loader accepts it.
#[test]
#[ignore = "invokes cargo; lane-only"]
fn a_normal_strategy_still_builds_and_loads_under_the_jail() {
    if !jail_available() {
        return;
    }
    let src = include_str!("../../vike-rust-strategy/tests/fixtures/equivalence/ma_cross.rs");
    let out = tempfile::tempdir().expect("out dir");
    let so = vike_strategy_builder::render::build_strategy_file(
        src,
        "ma_cross.rs",
        out.path(),
        &workspace_root(),
        &cargo_bin(),
        &child_env(),
        vike_strategy_builder::render::Profile::of_this_binary(),
        &vike_strategy_builder::render::CargoHomePin::disabled(),
    )
    .expect("a normal strategy builds under the jail");
    vike_rust_strategy::loader::load(&so).expect("the built artifact loads");
}

// --- the two build-speed mechanisms (2026-11-19), lane-only: they run cargo ----------------------

/// [`child_env`] with an ABSOLUTE `RUSTC` and no rustup variables, the way the deployed unit runs
/// cargo (its `RUSTC` names the pinned compiler), and `CARGO_HOME` replaced when `cargo_home` is given.
fn deployed_like_env(cargo_home: Option<&Path>) -> Vec<(String, String)> {
    let sysroot = std::process::Command::new("rustc")
        .args(["--print", "sysroot"])
        .output()
        .expect("rustc --print sysroot");
    let rustc = PathBuf::from(String::from_utf8_lossy(&sysroot.stdout).trim()).join("bin/rustc");
    let mut env: Vec<(String, String)> = child_env()
        .into_iter()
        .filter(|(k, _)| !k.starts_with("RUSTUP_") && k != "RUSTC")
        .collect();
    env.push(("RUSTC".to_string(), rustc.display().to_string()));
    if let Some(home) = cargo_home {
        env.retain(|(k, _)| k != "CARGO_HOME");
        env.push(("CARGO_HOME".to_string(), home.display().to_string()));
    }
    env
}

/// Write a crate at `dir`: `manifest` as its `Cargo.toml` and `main` as its `src/main.rs`.
fn write_crate(dir: &Path, manifest: &str, main: &str) {
    std::fs::create_dir_all(dir.join("src")).expect("crate dir");
    std::fs::write(dir.join("Cargo.toml"), manifest).expect("Cargo.toml");
    std::fs::write(dir.join("src/main.rs"), main).expect("main.rs");
}

/// cargo's `rustc` probe cache (`target/.rustc_info.json`) is written by a jailed build and REUSED by
/// the next one where no cargo config may name a wrapper, and the wrapper is still forced off where
/// one does: an empty `RUSTC_WRAPPER` made cargo drop the cache (it stats the empty path), so every
/// build re-ran `rustc -vV` four times and the target probe (`render::cargo_build_jailed`).
#[test]
#[ignore = "invokes cargo; lane-only"]
fn a_jailed_build_keeps_cargos_rustc_probe_cache_unless_a_config_may_name_a_wrapper() {
    if !jail_available() {
        return;
    }
    let manifest = "[package]\nname = \"probe\"\nversion = \"0.0.0\"\nedition = \"2024\"\n";
    let build = |dir: &Path, home: &Path| {
        let out = vike_strategy_builder::render::cargo_build_jailed(
            dir,
            &workspace_root(),
            &cargo_bin(),
            &deployed_like_env(Some(home)),
            vike_strategy_builder::render::Profile::Debug,
        )
        .expect("cargo ran");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    };

    // A cargo home with no config, as deployed: the cache is written, then reused (not rewritten).
    let crate_dir = tempfile::tempdir().expect("crate dir");
    let home = tempfile::tempdir().expect("cargo home");
    let config_above = crate_dir
        .path()
        .ancestors()
        .any(|d| d.join(".cargo/config.toml").exists() || d.join(".cargo/config").exists());
    if config_above {
        eprintln!(
            "SKIP the cache half: a directory above {} holds a cargo config",
            crate_dir.path().display()
        );
    } else {
        write_crate(crate_dir.path(), manifest, "fn main() {}\n");
        build(crate_dir.path(), home.path());
        let cache = crate_dir.path().join("target/.rustc_info.json");
        let first = std::fs::metadata(&cache)
            .and_then(|m| m.modified())
            .expect("cargo wrote no `target/.rustc_info.json`: its rustc probe cache is off again");
        std::fs::write(crate_dir.path().join("src/main.rs"), "fn main() { let _ = 1; }\n")
            .expect("edit main.rs");
        build(crate_dir.path(), home.path());
        let second = std::fs::metadata(&cache).and_then(|m| m.modified()).expect("cache");
        assert_eq!(first, second, "the second build rewrote the probe cache instead of reusing it");
    }

    // A cargo home whose config names a wrapper that does not exist: the build still succeeds, so
    // the spawn forced it off (and cargo, as before, keeps no cache).
    let crate_dir = tempfile::tempdir().expect("crate dir");
    let home = tempfile::tempdir().expect("cargo home");
    std::fs::write(
        home.path().join("config.toml"),
        "[build]\nrustc-wrapper = \"/nonexistent/vike-test-wrapper\"\n",
    )
    .expect("config.toml");
    write_crate(crate_dir.path(), manifest, "fn main() {}\n");
    build(crate_dir.path(), home.path());
    assert!(!crate_dir.path().join("target/.rustc_info.json").exists());
}

/// A NEW SDK's first work directory seeded from the previous SDK's (`render::seed_target`) reuses the
/// THIRD-PARTY units and rebuilds the PATH crate that changed, even when the change carries an
/// mtime older than the older build (what `git archive`'s commit-time mtimes give a deployed
/// tree): a whole copied `target/` would have linked the old code. The binary's output proves it.
#[test]
#[ignore = "invokes cargo; lane-only"]
fn a_seeded_scratch_keeps_third_party_units_and_rebuilds_a_changed_path_crate() {
    if !jail_available() {
        return;
    }
    let tree = tempfile::tempdir().expect("tree");
    let dep = tree.path().join("dep");
    let dep_src = dep.join("src/lib.rs");
    std::fs::create_dir_all(dep.join("src")).expect("dep dir");
    std::fs::write(
        dep.join("Cargo.toml"),
        "[package]\nname = \"dep\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
    )
    .expect("dep manifest");
    std::fs::write(&dep_src, "pub fn v() -> u32 { 1 }\n").expect("dep source");
    let manifest = format!(
        "[package]\nname = \"probe\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[dependencies]\n\
         dep = {{ path = \"{}\" }}\nitoa = \"1\"\n",
        dep.display()
    );
    let main = "fn main() { println!(\"{} {}\", dep::v(), itoa::Buffer::new().format(7)); }\n";
    let scratch_root = tempfile::tempdir().expect("scratch root");
    // Two SDKs' work directories of one kind (`vike_rust_strategy::artifact::work_directory`'s layout).
    let (old, new) = (
        scratch_root.path().join("sdk-1").join("work-0123abcd"),
        scratch_root.path().join("sdk-2").join("work-89abcdef"),
    );
    let env = deployed_like_env(None);
    let build = |dir: &Path| {
        let out = vike_strategy_builder::render::cargo_build_jailed(
            dir,
            tree.path(),
            &cargo_bin(),
            &env,
            vike_strategy_builder::render::Profile::Debug,
        )
        .expect("cargo ran");
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(out.status.success(), "{stderr}");
        stderr
    };
    let run = |dir: &Path| {
        let out = std::process::Command::new(dir.join("target/debug/probe")).output().expect("run");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };

    // The workspace's own lock, as `render::seed_lock` writes it: `itoa` at a version this box has.
    write_crate(&old, &manifest, main);
    std::fs::copy(workspace_root().join("Cargo.lock"), old.join("Cargo.lock")).expect("lock");
    std::fs::write(old.join("build.lock"), b"").expect("build.lock");
    let before_old_build = std::time::SystemTime::now() - std::time::Duration::from_secs(5);
    build(&old);
    assert_eq!(run(&old), "1 7");

    // The change, stamped BEFORE the old build, then the new directory seeded from the old one.
    std::fs::write(&dep_src, "pub fn v() -> u32 { 2 }\n").expect("change dep");
    std::fs::File::options()
        .write(true)
        .open(&dep_src)
        .and_then(|f| f.set_modified(before_old_build))
        .expect("stamp the change");
    write_crate(&new, &manifest, main);
    std::fs::copy(old.join("Cargo.lock"), new.join("Cargo.lock")).expect("lock");
    vike_strategy_builder::render::seed_target(
        &new,
        vike_rust_strategy::sdk::TemplateKind::Strategy,
        vike_strategy_builder::render::Profile::Debug,
    );
    let units = new.join("target/debug/build");
    assert!(units.join("itoa").is_dir(), "the third-party unit was not seeded");
    assert!(!units.join("dep").exists() && !units.join("probe").exists(), "a path unit was seeded");

    let stderr = build(&new);
    assert!(stderr.contains("Compiling dep"), "the changed path crate was not rebuilt:\n{stderr}");
    assert!(
        !stderr.contains("Compiling itoa"),
        "the seeded third-party unit was rebuilt:\n{stderr}"
    );
    assert_eq!(run(&new), "2 7", "the seeded build linked the OLD path crate");
}
