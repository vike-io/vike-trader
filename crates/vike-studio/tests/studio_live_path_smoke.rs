//! The LIVE smoke for the path the operator's GUI takes: Studio's OWN Build and Run calls, dialling
//! the addresses Studio dials BY DEFAULT, against a real deployment.
//!
//! `crates/vike-strategy-builder/tests/builder_live_smoke.rs` proves the builder service end to end,
//! and it does so through the SERVICE's client at an address it resolves from the environment. That
//! is the right shape for proving the service and the wrong one for proving the GUI's route: the
//! Studio reaches the builder through `vike_studio::spawn_build` at
//! `vike_strategy_builder::client::default_addr`, and reaches the compute daemon through
//! `vike_studio::spawn_run_remote` at `vike_studio::Backend::remote_default`'s address. This file
//! calls exactly those, at exactly those addresses, and takes NO address from the environment —
//! a knob would let it pass against an address the GUI never dials.
//!
//! # Where it runs
//!
//! Wherever those defaults reach the deployment: on the server box itself (they ARE its loopback
//! daemons), or on any other machine through `just studio-tunnel`, which forwards each one to the
//! SAME port — the operator's own PC being the case that matters. `#[ignore]`d and self-skipping,
//! the same double gate every live smoke in this tree uses. It takes the two keys the desktop takes,
//! from the variables the desktop reads them from, through the SAME readers:
//!
//! ```text
//! just studio-tunnel                      # on any machine that is not the server box
//! VIKE_STRATEGY_BUILDER_KEY=… VIKE_STUDIO_COMPUTE_KEY=… cargo test -p vike-studio --test studio_live_path_smoke -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Set both without typing either: `scripts/studio.ps1` shows the two reads it performs over ssh
//! (the builder's key file, and `vike-cli datahub control-key` on the box), and the ops page's
//! Studio section spells the by-hand version. Never echo either value.
//!
//! A debug build is enough, unlike the builder's smoke: nothing here `dlopen`s the artifact, so no
//! toolchain fingerprint has to match this process.
//!
//! # ⚠ It sends the builder smoke's EXACT strategy, on purpose
//!
//! Same bytes (`crates/vike-strategy-builder/tests/fixtures/live_smoke_strategy.rs.in`) under the
//! same NAME, so both smokes content-address to ONE artifact and whichever runs second on a
//! deployment is a cache hit — no compile on the production box and no new file in its plugin
//! directory. This file's Build is therefore a witness of the ROUTE (the address, the tunnel, the
//! key, the handshake, the content address) and deliberately not of the compiler; the builder's own
//! smoke owns that claim.
//!
//! # What a green run proves
//!
//! 1. [`studio_builds_through_the_address_it_dials_by_default`] — Studio's own Build call reaches
//!    the builder at its compiled-in default address, authenticates with the key the launcher hands
//!    the GUI, and gets back the sha256 of the source it sent.
//! 2. [`studio_runs_the_plugin_on_the_compute_daemon_with_its_compute_key`] — the listener at
//!    Studio's compiled-in compute default IS the compute daemon with a Studio runner table (its
//!    pre-auth `Welcome` advertises the compute plane's sentinel and none of the data plane's), and
//!    Studio's own Run call there, signed with Studio's compute key, RETURNS A RESULT for the plugin
//!    the Build produced. Any failure — no listener, the wrong daemon, a refused key, a missing
//!    artifact, a failed run — fails the test.
//!
//! # ⚠ It was a REFUSAL test until the owner answered 0083's question 1, and why it is not now
//!
//! Until 2026-09-26 the desktop held no datahub Control key at all, so (2) could only pin what the
//! route WAS — Studio's Run reaches the compute daemon, and a missing credential is the one thing
//! left standing — and it accepted the refusal. The owner then ruled
//! (`docs/decisions/0083-the-runtime-plugin-join-lands.md`, question 1, option (a)) that the desktop
//! resolves that key for Studio's compute dial, and that record said this test "should be tightened
//! to REQUIRE a result once question 1 is answered". It is: with the compute key present, anything
//! but `Ok` is red, and "REQUIRES authentication" is no longer an acceptable answer. Without the key
//! it SKIPS — after identifying the daemon, which needs no key — rather than accepting a refusal: a
//! refusal can no longer be read as the route working.
//!
//! # ⚠ Why (2) still identifies the daemon before it Runs
//!
//! Its first version accepted a refusal on the string `"REQUIRES authentication"` alone — and
//! `vike_datahub_client::DatahubClient::connect` returns that string for ANY server advertising
//! `auth`, the keyed DATAHUB included. So the regression `crates/vike-desktop/src/main.rs` records
//! (Studio's compute address overwritten with the datahub's) would have passed it. The `Welcome` is
//! the one thing both daemons send before auth, and its feature list says which plane answered.
//! With a key in hand the question is sharper still: the client now REFUSES to sign toward a
//! data-plane `Welcome` (`DatahubClient::connect_authed_on`), so this test's own identification and
//! the client's guard read the same sentinels — `vike_datahub_client::COMPUTE_PLANE_SENTINEL`,
//! `DATA_PLANE_SENTINEL`, `STUDIO_RUNNER_SENTINEL`, the constants both servers push — and a renamed
//! one fails here loudly rather than silently.
//!
//! Nothing here renders a pixel, and nothing here starts the GUI: the launcher, the GUI's panes and
//! the Build and Run buttons are proven only by the operator running them.

use std::collections::HashMap;
use std::net::TcpStream;
use std::time::{Duration, Instant};

use vike_data::TsRange;
use vike_datahub_client::{
    COMPUTE_PLANE_SENTINEL, DATA_PLANE_SENTINEL, PROTO_VERSION, STUDIO_RUNNER_SENTINEL,
    proto::{Request, Response, write_frame},
    read_frame,
};
use vike_strategy_builder::render::sha256_hex;
use vike_strategy_builder::{builder, client};
use vike_studio::{Backend, DataSlice, StrategySpec};

/// The feature list the server at `addr` advertises BEFORE authentication: `Hello` -> `Welcome`,
/// then hang up. Both daemons answer it, keyed or not, which is what makes it the one question
/// that can tell them apart on a box where every other verb needs a key.
fn pre_auth_features(addr: &str) -> Result<Vec<String>, String> {
    let mut stream = TcpStream::connect(addr).map_err(|e| format!("connect {addr}: {e}"))?;
    let deadline = Some(Duration::from_secs(10));
    stream.set_read_timeout(deadline).map_err(|e| e.to_string())?;
    stream.set_write_timeout(deadline).map_err(|e| e.to_string())?;
    write_frame(&mut stream, &Request::Hello { proto_version: PROTO_VERSION })
        .map_err(|e| format!("Hello to {addr}: {e}"))?;
    match read_frame::<_, Response>(&mut stream).map_err(|e| format!("Welcome from {addr}: {e}"))? {
        Response::Welcome { features, .. } => Ok(features),
        other => Err(format!("{addr} answered Hello with {other:?}, not a Welcome")),
    }
}

/// The builder smoke's own name — see the module doc for why the two share it.
const STRATEGY_NAME: &str = "builder_live_smoke";

/// The builder smoke's own bytes — see the module doc for why the two share them.
fn strategy_source() -> &'static str {
    include_str!("../../vike-strategy-builder/tests/fixtures/live_smoke_strategy.rs.in")
}

/// A window the production store holds (a week of hourly Hyperliquid BTC bars from the first day
/// that series was recorded).
const VENUE: &str = "hyperliquid";
const SYMBOL: &str = "BTC";
const INTERVAL: &str = "1h";
const START_MS: i64 = 1_775_001_600_000; // 2026-04-01T00:00:00Z
const END_MS: i64 = 1_775_606_400_000; // 2026-04-08T00:00:00Z

/// The process environment, swept ONCE per test — the shape the desktop's own `PROCESS_ENV` has.
fn env() -> HashMap<String, String> {
    std::env::vars().collect()
}

/// The builder's key, or `None` to self-skip — read through the SERVICE's own reader, so this test
/// cannot disagree with the daemon (or with the desktop, which calls the same function) about how
/// a key becomes a `NodeKeys`.
fn builder_keys() -> Option<vike_node_proto::auth::NodeKeys> {
    let keys = builder::keys_from_vars(&env());
    if keys.is_none() {
        eprintln!(
            "SKIP: {} is not set, so there is no key to build with. `just studio` reads it into the \
             GUI's environment; export it the same way (never echo the value) and re-run.",
            builder::BUILDER_KEY_ENV
        );
    }
    keys
}

/// Studio's own Build call, at Studio's own default address, waited on.
fn build_through_studio(keys: vike_node_proto::auth::NodeKeys) -> Result<String, String> {
    vike_studio::spawn_build(
        client::default_addr(),
        Some(keys),
        STRATEGY_NAME.to_string(),
        strategy_source().to_string(),
    )
    .recv()
    .expect("the build worker thread answered")
}

#[test]
#[ignore = "needs the strategy builder reachable at its default address and its key — see the module doc"]
fn studio_builds_through_the_address_it_dials_by_default() {
    let Some(keys) = builder_keys() else { return };
    let addr = client::default_addr();
    let expected = sha256_hex(strategy_source().as_bytes());
    eprintln!(
        "studio_live_path_smoke: Build -> {addr} (Studio's default), expecting sha {expected}"
    );

    let started = Instant::now();
    let sha = match build_through_studio(keys) {
        Ok(sha) => sha,
        Err(e) => panic!(
            "Studio's Build did not return a sha from {addr}. This is what the Plugin pane would \
             show, verbatim:\n\n{e}\n"
        ),
    };
    eprintln!("studio_live_path_smoke: BuildOk {sha} in {:.2?}", started.elapsed());
    assert_eq!(
        sha, expected,
        "the builder answered with a content address that is not the sha256 of the source Studio \
         sent — whatever it built, it was not this"
    );
}

#[test]
#[ignore = "needs the builder AND the compute daemon reachable at their default addresses, and both keys — see the module doc"]
fn studio_runs_the_plugin_on_the_compute_daemon_with_its_compute_key() {
    let Some(keys) = builder_keys() else { return };
    // A sha to run. The same bytes as the Build test, so on a deployment that has built them once
    // this is a cache hit rather than a compile.
    let sha = build_through_studio(keys)
        .unwrap_or_else(|e| panic!("the Build that supplies Run's sha failed:\n\n{e}\n"));

    let backend = Backend::remote_default();
    let addr = backend.addr().to_string();

    // WHICH daemon is at Studio's compute address — asked before anything is signed, and needing no
    // key, so it runs even when the Run below is skipped.
    let features = pre_auth_features(&addr).unwrap_or_else(|e| {
        panic!("nothing answered a Hello at Studio's compute address {addr}: {e}")
    });
    let has = |f: &str| features.iter().any(|x| x == f);
    assert!(
        !has(DATA_PLANE_SENTINEL),
        "the server at Studio's compute address {addr} is the DATAHUB (it advertises \
         `{DATA_PLANE_SENTINEL}`): {features:?}. Studio's Run would be refused by plane — and with \
         a compute key in hand, by the client before it signs. See `crates/vike-desktop/src/main.rs`'s \
         wrong-plane note."
    );
    assert!(
        has(COMPUTE_PLANE_SENTINEL),
        "the server at Studio's compute address {addr} does not advertise \
         `{COMPUTE_PLANE_SENTINEL}`, so it is not the compute daemon: {features:?}"
    );
    assert!(
        has(STUDIO_RUNNER_SENTINEL),
        "the compute daemon at {addr} advertises no `{STUDIO_RUNNER_SENTINEL}`: it was started \
         without a Studio runner table, and Studio's Run can never succeed there whatever the \
         credential: {features:?}"
    );
    eprintln!("studio_live_path_smoke: {addr} is the compute daemon; its Welcome: {features:?}");

    // Studio's compute key, through the ONE reader the desktop uses — or a SKIP. ⚠ Not an accepted
    // refusal: without the key there is nothing this test can claim about the Run, and a refusal
    // passing here is exactly what this test stopped allowing.
    let Some(compute_key) = vike_studio::compute_key_from_vars(&env()) else {
        eprintln!(
            "SKIP (the Run): {} is not set, so Studio holds no compute key. The daemon above was \
             identified; the Run itself needs the key `just studio` hands the GUI — export it the \
             same way (never echo the value) and re-run.",
            vike_studio::COMPUTE_KEY_ENV
        );
        return;
    };

    let spec =
        StrategySpec::plugin(STRATEGY_NAME, sha.clone(), toml::Value::Table(Default::default()));
    let range = TsRange { start: Some(START_MS), end: Some(END_MS) };
    let slice = DataSlice::bars(VENUE, SYMBOL, INTERVAL, range);
    eprintln!("studio_live_path_smoke: Run {STRATEGY_NAME}-{sha} -> {addr} (Studio's default)");

    let started = Instant::now();
    let outcome = vike_studio::spawn_run_remote(addr.clone(), Some(compute_key), spec, slice)
        .recv()
        .expect("the run worker thread answered");
    match outcome {
        Ok(result) => eprintln!(
            "studio_live_path_smoke: RUN OK against {addr} in {:.2?}: {} trades, final equity {}",
            started.elapsed(),
            result.n_trades,
            result.final_equity
        ),
        Err(e) => panic!(
            "Studio's Run, signed with its compute key, did not return a result from {addr}. With \
             the key present nothing but a result is a pass — a refused key, a missing artifact and \
             a failed run are all red. Studio would show, verbatim:\n\n{e}\n"
        ),
    }
}
