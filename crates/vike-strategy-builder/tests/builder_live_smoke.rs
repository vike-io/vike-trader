//! The LIVE end-to-end smoke for the builder service: a real user strategy, compiled by a RUNNING
//! `vike-strategy-builder` over its real authenticated socket, and the artifact that comes back
//! proven to be a loadable plugin rather than merely a sha.
//!
//! ⚠ **This is the one test in this tree that exercises the WHOLE path.** Every rung of it was
//! already proven in isolation and the join never was: `tests/builder_auth.rs` drives the
//! handshake against a listener it starts itself, `tests/build_errors.rs` calls
//! `crates/vike-strategy-builder/src/render.rs`'s `build_plugin` IN-PROCESS (never over a socket,
//! never against a deployed binary), and `crates/vike-strategy-plugin/tests/equivalence.rs` builds
//! and loads an artifact its own process produced. Nothing anywhere connected a client to a
//! DEPLOYED daemon, and a green sha from a deployed daemon is not the same claim as a green sha
//! from an in-process call — the deployed shape adds a socket, a systemd sandbox, a relocated
//! `CARGO_HOME`, a named toolchain, and a `workspace_root` that is a release ASSET rather than the
//! checkout the test was compiled from. Each of those five has its own way of failing, and the
//! last one refuses BY DESIGN when it drifts (`render.rs`'s `verify_source_version`).
//!
//! # Running it
//!
//! `#[ignore]`d and SELF-SKIPPING, the same double gate every `crates/bridges/<venue>/tests/*_smoke.rs`
//! uses — see `crates/bridges/okx/tests/okx_reconcile_smoke.rs`'s `load_demo_creds` for the idiom
//! this borrows. The credential here is the builder's own key rather than a venue's, and it is read
//! through `crates/vike-strategy-builder/src/builder.rs`'s `resolve_keys` — the SERVICE's own
//! resolver, both forms included — so the test cannot disagree with the daemon about how a key
//! becomes a `NodeKeys`. Point it at the key FILE rather than exporting the key: the path is not a
//! secret, so nothing sensitive enters this shell's environment, its history or cargo's.
//!
//! ```text
//! VIKE_STRATEGY_BUILDER_KEY_FILE=<project>/settings/strategy-builder.key \
//! VIKE_STRATEGY_BUILDER_OUT_DIR=<project>/user_data/plugins \
//!   cargo test --release -p vike-strategy-builder --test builder_live_smoke -- --ignored --nocapture
//! ```
//!
//! (`VIKE_STRATEGY_BUILDER_KEY=<the key>` still works in its place; setting BOTH is refused, as the
//! service refuses it.)
//!
//! `VIKE_STRATEGY_BUILDER_PORT` is honoured too, with the service's own default, so this reaches a
//! deployment that moved the port as well as the ordinary one — see `service_addr`.
//!
//! ⚠ **`--release` is REQUIRED, and it is a correctness flag rather than a speed one.** The
//! service compiles every artifact under `render::Profile::Release` (`builder.rs`'s
//! `handle_connection` names it outright, because the host that will `dlopen` the result is a
//! release backtest server), and `crates/vike-strategy-plugin/src/fingerprint.rs`'s `FINGERPRINT`
//! folds Cargo's own `PROFILE`/`OPT_LEVEL` in. A DEBUG test binary therefore carries a different
//! fingerprint from anything this service can produce, and step 4 below would fail with a
//! `FingerprintMismatch` that says nothing about the service. The test prints its own host
//! fingerprint before it asks for a build, so a mismatch is diagnosable from the output rather
//! than from this comment.
//!
//! ⚠ **`VIKE_STRATEGY_BUILDER_OUT_DIR` is REQUIRED TOO, and its absence is a FAILURE rather than a
//! skip.** The out directory belongs to the service and this test has no way to discover it — the
//! daemon reads that variable (`builder::run`) and answers with a sha, never a path. Skipping step
//! 4 quietly would leave this file's name claiming more than the run proved, which is the exact
//! shape of overclaim this smoke exists to close. So the key alone decides whether the test RUNS;
//! once it runs, an out directory it cannot find is an error naming the variable.
//!
//! # What a green run PROVES
//!
//! 1. A client reaches a LISTENING builder at the address `service_addr` resolves — the service's
//!    own `VIKE_STRATEGY_BUILDER_PORT`/`builder::DEFAULT_PORT`/`builder::bind_addr` chain, not a
//!    second spelling of it — and its `Hello`/`Welcome`/`Auth`/`AuthOk` handshake completes
//!    against that binary's protocol version and domain separator. ⚠ This rung is proven by
//!    ANY answer that is not `Connect`/`Protocol`/`AuthDenied`: a `BuildErr` means the HMAC was
//!    accepted, so a refusal arriving from the compile stage is itself the handshake's witness.
//! 2. The deployed daemon accepted a real user strategy (the entry contract
//!    `crates/vike-model/src/strategy/mod.rs`'s `Strategy` and the template's
//!    `build<B: HftBroker + 'static>` instantiation demand), ran `cargo` inside its own sandbox,
//!    and rustc compiled it.
//! 3. The returned sha is the sha256 of the source THIS test sent — checked against
//!    `render::sha256_hex` rather than merely asserted to be 64 hex characters, so a service that
//!    answered with any other content address fails here.
//! 4. An artifact really exists at `<out_dir>/<name>-<sha>.so`, `loader::load` accepts it (every
//!    handshake export AND all sixteen dispatch symbols resolve), and the USER'S OWN CODE runs
//!    across the C-ABI boundary: `create` mints a handle from a TOML params document and `warmup`
//!    answers the distinctive number that document asked for. That last step is what separates
//!    "an artifact loaded" from "the artifact contains the strategy I sent" — a plugin whose
//!    `create` returned null still LOADS, and would then trade nothing (the incident
//!    `template/lib.rs.in`'s `vike_plugin_create` comment records, where `.parse()` in place of
//!    `toml::from_str` made every handle null while the artifact built, loaded and handshook
//!    cleanly).
//! 5. The service's CACHE path is real: a second, byte-identical request returns the same sha
//!    without rebuilding, proven by the artifact's mtime being unchanged rather than by a timing
//!    threshold. That rung matters more than it looks — a cache hit is `dlopen`ed and
//!    handshake-checked inside the DAEMON (`build_plugin`'s `verify_handshake` call) before it is
//!    served, so this is the only place that check runs against a genuinely current artifact
//!    produced by the deployed binary itself.
//!
//! # What a green run does NOT prove
//!
//! * **Nothing about the backtest server.** The hand-off between the two daemons is a shared
//!   directory, not a socket (`deploy/vike-strategy-builder.service`'s out-dir block states the
//!   contract from this side). This test loads the artifact in ITS OWN process; that the compute
//!   daemon can also read it depends on that unit resolving the same leaf and on `UMask=0077`
//!   ownership lining up, neither of which is observable from here.
//! * **Nothing about the strategy's NUMBERS.** `create`/`warmup`/`destroy` need no broker; the
//!   dispatch hooks are never fired, so this says nothing about what the compiled strategy would
//!   trade. That claim belongs to `crates/vike-strategy-plugin/tests/equivalence.rs`, which
//!   compares two mechanisms' trade lists and is the only test that can make it.
//! * **Nothing about a COLD `CARGO_HOME` after the first green run.** The very first build on a
//!   deployment is the one that decides whether the daemon works at all (the unit's own
//!   `SystemCallFilter=fchown` block argues why, and measured that a warm cargo home hides the
//!   defect it fixes). This test cannot re-create that state — it would have to delete a directory
//!   inside a live project folder — so it reports its wall-clock and leaves the interpretation to
//!   whoever reads it: minutes means cold, seconds means somebody already paid for it.
//! * **Nothing about a FINGERPRINT the deployed toolchain and this test binary do not share.** The
//!   test is compiled from the branch under test; the artifact is compiled against the release
//!   asset the daemon points at. Step 4 compares them and names both values on a mismatch, which
//!   is a finding about the DEPLOYMENT rather than about this test — but it is a finding this test
//!   can only report, never repair.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Instant, SystemTime};

use vike_strategy_builder::builder;
use vike_strategy_builder::client;
use vike_strategy_builder::render::sha256_hex;
use vike_strategy_plugin::{fingerprint, loader};

/// The strategy NAME the artifact is filed under — `<name>-<sha>.so`, parsed back by
/// `builder::artifact_strategy_name`. Deliberately self-describing: this writes a real file into a
/// real deployment's retention group, and an operator finding it later should be able to tell what
/// put it there without asking.
const STRATEGY_NAME: &str = "builder_live_smoke";

/// The params document the plugin's `create` export is handed, and the number `warmup` must answer.
///
/// Non-zero and not a round default on purpose: `Strategy::warmup`'s own default is `0`, and the
/// template's `vike_plugin_warmup` returns `usize::MAX` for a caught panic or a null handle — so
/// `0` would be indistinguishable from "the user's override never ran" and `usize::MAX` from "it
/// panicked". A value that is neither can only have come from the source this test sent.
const WARMUP_BARS: usize = 7;

/// A trivial but REAL user strategy — the entry-file contract, nothing more.
///
/// It obeys exactly what the cdylib template demands of a user file and knows nothing about FFI:
/// a `Strategy` impl plus
/// `pub fn build<B: HftBroker + 'static>(&toml::Value) -> Box<dyn Strategy<B> + Send>`. The same
/// shape `crates/vike-strategy-builder/tests/build_errors.rs`'s `fixture_source` uses for the
/// in-process pipeline, with one addition that earns its place here: a `warmup` OVERRIDE reading
/// its value out of `params`, which is the only channel this test has for observing that the
/// user's own compiled code — and the params that reached it — really are inside the artifact.
///
/// It overrides no hook in `vike_strategy_plugin::host::UNWIRED_HOOKS`, so the pre-cargo
/// `BuildError::UnwiredHook` refusal does not fire and the request genuinely reaches a compiler.
///
/// ⚠ **The TEXT lives in `tests/fixtures/live_smoke_strategy.rs.in`, and the file is shared.**
/// `crates/vike-studio/tests/studio_live_path_smoke.rs` sends the SAME bytes under the same
/// [`STRATEGY_NAME`], so the two smokes content-address to ONE artifact: whichever runs second on a
/// deployment is a cache hit, compiling nothing and writing nothing into the live box's plugin
/// directory. A private copy here would make every edit to one smoke a cold build on production the
/// next time the other ran. The bytes are the content address, so `.gitattributes` pins the
/// fixture's line endings — a CRLF checkout would be a different sha with no visible difference.
fn strategy_source() -> &'static str {
    include_str!("fixtures/live_smoke_strategy.rs.in")
}

#[test]
#[ignore = "needs a RUNNING vike-strategy-builder and its key — run manually (see module doc)"]
fn a_live_builder_compiles_a_strategy_and_the_artifact_loads() {
    // The composition-root shape, not `env::var`: one `std::env::vars()` sweep handed to the
    // service's OWN readers, so nothing here restates a variable name the daemon already owns.
    let vars: HashMap<String, String> = std::env::vars().collect();

    let keys = match builder::resolve_keys(&vars, &|path| std::fs::read(path)) {
        Ok(keys) => keys,
        Err(builder::KeyRefusal::Neither) => {
            // The self-skip. `eprintln!` rather than `tracing::warn!` — unlike the venue smokes this
            // borrows its shape from, this crate deliberately carries no `tracing` dependency at
            // all (`builder.rs`'s module header argues why), and stderr is what its binary uses.
            eprintln!(
                "SKIP: neither {} nor {} is set, so there is no key to authenticate with. Point \
                 the file form at the deployment's key file (never echo the value) and re-run.",
                builder::BUILDER_KEY_FILE_ENV,
                builder::BUILDER_KEY_ENV
            );
            return;
        }
        // A key that WAS configured and cannot be used is a failure, not a skip: the operator
        // asked for a run, and a skip would read as "nothing to test".
        Err(refusal) => panic!("the key could not be resolved: {refusal}"),
    };

    let out_dir: PathBuf = match vars.get("VIKE_STRATEGY_BUILDER_OUT_DIR") {
        Some(v) if !v.trim().is_empty() => PathBuf::from(v.trim()),
        _ => panic!(
            "VIKE_STRATEGY_BUILDER_OUT_DIR is not set. This test cannot discover the service's \
             own out directory — the daemon answers a Build with a sha, never a path — and \
             skipping the artifact half would let this test's NAME claim more than the run \
             proved. Set it to the same value the running unit does."
        ),
    };

    let addr = service_addr(&vars);
    let src = strategy_source();
    let expected_sha = sha256_hex(src.as_bytes());

    eprintln!("builder_live_smoke: addr {addr}");
    // The LENGTH, never the value — the same rule every credential-touching line in this tree
    // follows. Read through `key_for(required_scope())` rather than a field, so this line also
    // witnesses that the key landed in the ONE slot `resolve_keys` fills (`builder.rs`'s
    // Decision 2: there is no observe-scope key for this protocol at all).
    eprintln!(
        "builder_live_smoke: key length {} bytes (value never printed)",
        keys.key_for(builder::required_scope()).len()
    );
    eprintln!("builder_live_smoke: host fingerprint {}", fingerprint::FINGERPRINT);
    eprintln!("builder_live_smoke: host ABI version {}", vike_strategy_plugin::abi::ABI_VERSION);
    eprintln!("builder_live_smoke: expected sha {expected_sha}");

    // ---- steps 1-3: the socket, the compile, the content address ------------------------------
    //
    // `toolchain_fp` is `""` deliberately, and `client.rs`'s own header argues it: the field rides
    // the wire and is checked against nothing, and the value that will eventually be correct is
    // the fingerprint of the host that will `dlopen` the artifact. Sending this process's own
    // would be a confident wrong answer the day that check lands.
    let started = Instant::now();
    let sha = match client::build_remote(&addr, &keys, STRATEGY_NAME, src, "") {
        Ok(sha) => sha,
        Err(e) => panic!(
            "the live builder did not return a sha. This is the WHOLE finding when it happens — \
             the message below is the service's own words, verbatim:\n\n{e}\n"
        ),
    };
    let first_build = started.elapsed();
    eprintln!("builder_live_smoke: BuildOk sha {sha} in {first_build:.2?}");

    assert_eq!(
        sha, expected_sha,
        "the service answered with a content address that is not the sha256 of the source it was \
         sent — so whatever it built, it was not this"
    );

    // ---- step 4: the artifact is REAL ---------------------------------------------------------
    let artifact = out_dir.join(format!("{STRATEGY_NAME}-{sha}.so"));
    assert!(
        artifact.is_file(),
        "the service returned sha {sha} but no artifact exists at {}. A sha with no loadable \
         artifact behind it is exactly the failure that reaches a user as \"it built and Run \
         refuses it\". Check that this is the same out directory the running unit uses.",
        artifact.display()
    );
    let size = std::fs::metadata(&artifact).expect("stat the artifact").len();
    let built_at = artifact_mtime(&artifact);
    eprintln!("builder_live_smoke: artifact {} ({size} bytes)", artifact.display());
    assert!(size > 0, "the artifact at {} is empty", artifact.display());

    let plugin = match loader::load(&artifact) {
        Ok(p) => p,
        Err(e) => panic!(
            "the artifact exists but this host refuses it: {e}\n\nhost fingerprint: {}\nhost ABI: \
             {}\n\n⚠ A FingerprintMismatch here is a finding about the DEPLOYMENT, not about this \
             test: the service compiles under Profile::Release with the toolchain its unit names, \
             and this binary must have been built with `--release` under the same rustc/target to \
             compare equal. See this file's module doc.",
            fingerprint::FINGERPRINT,
            vike_strategy_plugin::abi::ABI_VERSION
        ),
    };
    eprintln!("builder_live_smoke: loader::load accepted {}", plugin.path.display());

    // ...and the user's OWN code runs across the boundary. `create` parses the params document and
    // calls the `build` in `strategy_source`; `warmup` then reads back the value that document
    // asked for. A null handle (the `.parse()` incident) or a panicking `warmup` (`usize::MAX`)
    // both fail here, and neither would have been visible from `load` alone.
    let params = format!("qty = 2.5\nwarmup_bars = {WARMUP_BARS}\n");
    let handle = (plugin.vtable.create)(params.as_ptr(), params.len());
    assert!(
        !handle.is_null(),
        "vike_plugin_create returned null for params {params:?} — the compiled strategy's own \
         `build` either failed to parse the params document or panicked, and a plugin in that \
         state loads cleanly and then trades nothing"
    );
    let warmup = (plugin.vtable.warmup)(handle);
    (plugin.vtable.destroy)(handle);
    assert_ne!(
        warmup,
        usize::MAX,
        "vike_plugin_warmup answered its panic sentinel — the user's `warmup` override unwound \
         across the C-ABI boundary"
    );
    assert_eq!(
        warmup, WARMUP_BARS,
        "vike_plugin_warmup answered {warmup}, not the {WARMUP_BARS} the params document asked \
         for — the artifact is loadable but it is not carrying the strategy this test sent, or the \
         params never reached it"
    );
    eprintln!("builder_live_smoke: create -> warmup answered {warmup} (the params' own value)");

    // ---- step 5: the second, identical request is served from the cache -----------------------
    let started = Instant::now();
    let again = client::build_remote(&addr, &keys, STRATEGY_NAME, src, "")
        .expect("the second, byte-identical request must also succeed");
    let second_build = started.elapsed();
    assert_eq!(again, sha, "the same source must content-address to the same sha");
    assert_eq!(
        artifact_mtime(&artifact),
        built_at,
        "the artifact's mtime moved, so the service REBUILT rather than serving its cache. That \
         is not wrong in itself — a cache hit is handshake-checked before it is served and a \
         refusal correctly triggers a rebuild — but it means the daemon rejected the artifact it \
         had just produced, which is a finding worth chasing."
    );
    eprintln!(
        "builder_live_smoke: cache hit returned the same sha in {second_build:.2?} \
         (first build {first_build:.2?}), artifact mtime unchanged"
    );
}

/// Where the service is, resolved the way the SERVICE resolves its own bind: the same
/// `VIKE_STRATEGY_BUILDER_PORT` out of the same sweep, the same `builder::DEFAULT_PORT` fallback,
/// through the same `builder::bind_addr` — so the test and the daemon cannot disagree about where
/// this protocol lives, and a deployment that moved the port off the default is still reachable.
///
/// ⚠ **`client::default_addr()` was here and is not enough**, which is worth stating because it
/// looks like the more principled call: it is the client's own default and it derives the whole
/// address from `bind_addr(DEFAULT_PORT)` rather than restating a literal — but the PORT is a real
/// deployment knob (`builder::run` reads it, and a box running more than one of these daemons has
/// to use it), and a test pinned to the default answers `Connect` on such a box, which reads like
/// a dead service rather than a test looking in the wrong place. With the variable unset this
/// function returns exactly `client::default_addr()`, so nothing about the ordinary case changed.
fn service_addr(vars: &HashMap<String, String>) -> String {
    let port: u16 = vars
        .get("VIKE_STRATEGY_BUILDER_PORT")
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(builder::DEFAULT_PORT);
    builder::bind_addr(port).to_string()
}

/// The artifact's modification time — the witness for "was this rebuilt", the same evidence
/// `tests/build_errors.rs`'s `a_cache_hit_with_a_current_artifact_never_invokes_cargo` uses, and
/// preferred over a timing threshold because a threshold is a flake waiting for a loaded box.
fn artifact_mtime(path: &std::path::Path) -> SystemTime {
    std::fs::metadata(path).expect("stat the artifact").modified().expect("mtime")
}
