//! The LIVE smoke for the path the operator's GUI takes: Studio's OWN Build and Run calls, dialling
//! the addresses Studio dials BY DEFAULT, against a real deployment.
//!
//! `crates/vike-strategy-builder/tests/builder_live_smoke.rs` proves the builder service end to end,
//! and it does so through the SERVICE's client at an address it resolves from the environment. That
//! is the right shape for proving the service and the wrong one for proving the GUI's route: the
//! Studio reaches the builder through `vike_studio::spawn_build` at
//! `vike_strategy_builder::client::default_addr`, and reaches the backtesthub through
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
//! from where the desktop reads them, through the SAME readers: BOTH are rows of THIS machine's
//! settings database (the owner's ruling of 2026-10-31, "all in the SQLite, not files") — the
//! builder's through `vike_strategy_builder::builder::resolve_keys`, Studio's backtest key through
//! a scoped read of its one row and `vike_studio::backtest_key_from_credentials`:
//!
//! ```text
//! just studio-tunnel                      # on any machine that is not the server box
//! cargo test -p vike-studio --test studio_live_path_smoke -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Nothing is exported for it: `just studio` (or the ops page's by-hand step) files the backtest key
//! into this machine's store — `scripts/studio.ps1` shows the read it performs over ssh
//! (`vike-cli datahub control-key` on the box, piped into `vike-cli secrets set`) — and the ops
//! page's Studio section spells the one-time step that files the builder's. Never echo either value.
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
//! 2. [`studio_runs_the_plugin_on_the_backtesthub_with_its_backtest_key`] — the listener at
//!    Studio's compiled-in compute default IS the backtesthub with a Studio runner table (its
//!    pre-auth `Welcome` advertises the backtest plane's sentinel and none of the data plane's), and
//!    Studio's own Run call there, signed with Studio's backtest key, RETURNS A RESULT for the plugin
//!    the Build produced. Any failure — no listener, the wrong daemon, a refused key, a missing
//!    artifact, a failed run — fails the test.
//!
//! # ⚠ It was a REFUSAL test until the owner answered 0083's question 1, and why it is not now
//!
//! Until 2026-09-26 the desktop held no datahub Control key at all, so (2) could only pin what the
//! route WAS — Studio's Run reaches the backtesthub, and a missing credential is the one thing
//! left standing — and it accepted the refusal. The owner then ruled
//! (`docs/decisions/0083-the-runtime-plugin-join-lands.md`, question 1, option (a)) that the desktop
//! resolves that key for Studio's compute dial, and that record said this test "should be tightened
//! to REQUIRE a result once question 1 is answered". It is: with the backtest key present, anything
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
//! the client's guard read the same sentinels — `vike_datahub_client::BACKTEST_PLANE_SENTINEL`,
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
    BACKTEST_PLANE_SENTINEL, DATA_PLANE_SENTINEL, PROTO_VERSION, STUDIO_RUNNER_SENTINEL,
    proto::{Request, Response, read_frame, write_frame},
};
use vike_hub_proto::hash::sha256_hex;
use vike_strategy_builder::{builder, client};
use vike_studio::Backend;
use vike_studio_core::{DataSlice, StrategySpec};

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

/// The builder's key, or `None` to self-skip — read through the SERVICE's own reader over THIS
/// machine's settings database, so this test cannot disagree with the daemon about how a key
/// becomes a `HubKeys`, nor with the desktop about where it is. An ABSENT row is the skip; a store
/// that exists and will not open is a failure (the operator asked for a run).
fn builder_keys() -> Option<vike_hub_proto::auth::HubKeys> {
    match builder::resolve_keys(&env()) {
        Ok(keys) => Some(keys),
        Err(refusal @ builder::KeyRefusal::Missing { .. }) => {
            eprintln!("SKIP: there is no key to build with. {refusal}");
            None
        }
        Err(refusal) => panic!("the builder's key could not be resolved: {refusal}"),
    }
}

/// Studio's backtest key, or `None` to self-skip — a scoped read of its ONE row in THIS machine's
/// settings database (so this process holds no other credential), resolved by the SAME function the
/// desktop calls over the credential map it loads. An ABSENT row (or no store) is the skip; a store
/// that exists and will not open is a failure (the operator asked for a run).
fn backtest_key() -> Option<vike_studio::BacktestKey> {
    let name = vike_studio::BACKTEST_KEY_NAME;
    let store = vike_secrets::open_credential_store(&env())
        .unwrap_or_else(|e| panic!("this machine's credential store could not be opened: {e}"));
    let read = store
        .read(&vike_secrets::KeyScope::of([name]))
        .unwrap_or_else(|e| panic!("the `{name}` row could not be read: {e}"));
    let held = read
        .get(name)
        .declared()
        .unwrap_or_else(|e| panic!("the `{name}` row could not be read: {e}"));
    let map: HashMap<String, String> =
        held.map(|value| (name.to_string(), value.to_string())).into_iter().collect();
    vike_studio::backtest_key_from_credentials(&map)
}

/// Studio's own Build call, at Studio's own default address, waited on.
fn build_through_studio(keys: vike_hub_proto::auth::HubKeys) -> Result<String, String> {
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
#[ignore = "needs the builder AND the backtesthub reachable at their default addresses, and both keys — see the module doc"]
fn studio_runs_the_plugin_on_the_backtesthub_with_its_backtest_key() {
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
         a backtest key in hand, by the client before it signs. See `crates/vike-desktop/src/main.rs`'s \
         wrong-plane note."
    );
    assert!(
        has(BACKTEST_PLANE_SENTINEL),
        "the server at Studio's compute address {addr} does not advertise \
         `{BACKTEST_PLANE_SENTINEL}`, so it is not the backtesthub: {features:?}"
    );
    assert!(
        has(STUDIO_RUNNER_SENTINEL),
        "the backtesthub at {addr} advertises no `{STUDIO_RUNNER_SENTINEL}`: it was started \
         without a Studio runner table, and Studio's Run can never succeed there whatever the \
         credential: {features:?}"
    );
    eprintln!("studio_live_path_smoke: {addr} is the backtesthub; its Welcome: {features:?}");

    // Studio's backtest key, through the ONE reader the desktop uses — or a SKIP. ⚠ Not an accepted
    // refusal: without the key there is nothing this test can claim about the Run, and a refusal
    // passing here is exactly what this test stopped allowing.
    let Some(backtest_key) = backtest_key() else {
        eprintln!(
            "SKIP (the Run): this machine's settings database holds no {} row, so Studio holds no \
             backtest key. The daemon above was identified; the Run itself needs the key `just \
             studio` files there — run it once (or `vike-cli secrets set {}`, the value on stdin, \
             never echoed) and re-run.",
            vike_studio::BACKTEST_KEY_NAME,
            vike_studio::BACKTEST_KEY_NAME
        );
        return;
    };

    let spec = StrategySpec::plugin(STRATEGY_NAME, sha.clone(), vike_studio_core::empty_params());
    let range = TsRange { start: Some(START_MS), end: Some(END_MS) };
    let slice = DataSlice::bars(VENUE, SYMBOL, INTERVAL, range);
    eprintln!("studio_live_path_smoke: Run {STRATEGY_NAME}-{sha} -> {addr} (Studio's default)");

    let started = Instant::now();
    let outcome = vike_studio::spawn_run_remote(addr.clone(), Some(backtest_key), spec, slice)
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
            "Studio's Run, signed with its backtest key, did not return a result from {addr}. With \
             the key present nothing but a result is a pass — a refused key, a missing artifact and \
             a failed run are all red. Studio would show, verbatim:\n\n{e}\n"
        ),
    }
}
