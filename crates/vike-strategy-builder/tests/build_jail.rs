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
    let vike_strategy_builder::render::BuildError::Compile(diagnostics) = &err else {
        panic!("the build must fail at the denied read, got {err:?}");
    };
    assert!(
        diagnostics.contains("couldn't read") || diagnostics.contains("vike.db"),
        "the build must fail AT the denied `include_bytes!` read, not elsewhere:\n{diagnostics}"
    );
    // Nothing the build touched carries the secret: no artifact in out_dir, and no scratch file
    // under the daemon's scratch root holds the marker.
    let mut searched = files_under(out.path());
    searched.extend(files_under(&vike_strategy_builder::render::scratch_root()));
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
