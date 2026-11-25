//! The LIVE smoke for the path the operator's GUI takes: Studio's OWN Build and Run calls, dialling
//! the addresses Studio dials BY DEFAULT, against a real deployment.
//!
//! `crates/vike-strategy-builder/tests/builder_live_smoke.rs` proves the builder service end to end,
//! and it does so through the SERVICE's client at an address it resolves from the environment. That
//! is the right shape for proving the service and the wrong one for proving the GUI's route: the
//! Studio reaches the builder THROUGH THE BACKTESTHUB (decision 0288's amendment of 2026-11-16: no
//! person holds the builder's key), with `vike_studio::spawn_backtesthub_build`, and runs with
//! `vike_studio::spawn_backtesthub_run`, both at `vike_studio::StudioRunBackend::source_default`'s address. This file
//! calls exactly those, at exactly that address, and takes NO address from the environment — a knob
//! would let it pass against an address the GUI never dials.
//!
//! # Where it runs
//!
//! Wherever those defaults reach the deployment: on the server box itself (they ARE its loopback
//! daemons), or on any other machine through `just studio-tunnel`, which forwards each one to the
//! SAME port — the operator's own PC being the case that matters. `#[ignore]`d and self-skipping,
//! the same double gate every live smoke in this tree uses. It takes the one key the desktop takes,
//! from where the desktop reads it: THIS machine's device key (decision 0288), the `device_key` row of
//! its settings database, which the server's access list must hold at the role `control`:
//!
//! ```text
//! just studio-tunnel                      # on any machine that is not the server box
//! cargo test -p vike-studio --test studio_live_path_smoke -- --ignored --nocapture --test-threads=1
//! ```
//!
//! Nothing is exported for it: `vike-cli backend connect` makes this machine's device key and
//! registers it on the server (the shared backtest key it replaced is read by nothing since decision
//! 0288's cutover). Nothing here prints a byte of it.
//!
//! A debug build is enough, unlike the builder's smoke: nothing here `dlopen`s the artifact, so no
//! toolchain fingerprint has to match this process.
//!
//! # ⚠ It sends the builder smoke's EXACT strategy, on purpose
//!
//! Same bytes (`crates/vike-strategy-builder/tests/fixtures/live_smoke_strategy.rs.in`) under the
//! same NAME, so both smokes content-address to ONE artifact and whichever runs second on a
//! deployment is a cache hit — no compile on the production box and no new file in its strategy file
//! directory. This file's Build is therefore a witness of the ROUTE (the address, the tunnel, the
//! key, the handshake, the content address) and deliberately not of the compiler; the builder's own
//! smoke owns that claim.
//!
//! # What a green run proves
//!
//! 1. [`studio_builds_through_the_address_it_dials_by_default`] — Studio's own Build call reaches
//!    the backtesthub at its compiled-in default address, authenticates with the key the launcher
//!    hands the GUI, and gets back (from the builder the hub forwards to) the sha256 of the source it
//!    sent.
//! 2. [`studio_runs_the_strategy_file_on_the_backtesthub_with_its_device_key`] — the listener at
//!    Studio's compiled-in compute default IS the backtesthub with a Studio runner table (its
//!    pre-auth `Welcome` advertises the backtest plane's sentinel and none of the data plane's), and
//!    Studio's own Run call there, signed with this machine's device key, RETURNS A RESULT for the strategy file
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
//! to REQUIRE a result once question 1 is answered". It is: with a key present (this machine's device
//! key since decision 0288), anything but `Ok` is red, and "REQUIRES authentication" is no longer an acceptable answer. Without the key
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
//! the client's guard read the same sentinels — `vike_hub_protocol::plane::BACKTEST_PLANE_SENTINEL`,
//! `DATA_PLANE_SENTINEL`, `STUDIO_RUNNER_SENTINEL`, the constants both servers push — and a renamed
//! one fails here loudly rather than silently.
//!
//! Nothing here renders a pixel, and nothing here starts the GUI: the launcher, the GUI's panes and
//! the Build and Run buttons are proven only by the operator running them.

use std::collections::HashMap;
use std::net::TcpStream;
use std::time::{Duration, Instant};

use vike_backtesthub_client::proto::STUDIO_RUNNER_SENTINEL;
use vike_data::TimestampRange;
use vike_datahub_client::PROTO_VERSION;
use vike_hub_protocol::frame::{read_frame, write_frame};
use vike_hub_protocol::handshake::{HandshakeFrame, HandshakeReply};
use vike_hub_protocol::hash::sha256_hex;
use vike_hub_protocol::plane::{BACKTEST_PLANE_SENTINEL, DATA_PLANE_SENTINEL};
use vike_studio::StudioRunBackend;
use vike_studio_core::{DataSlice, StrategySpec};

/// The feature list the server at `address` advertises BEFORE authentication: `Hello` -> `Welcome`,
/// then hang up. Both daemons answer it, keyed or not, which is what makes it the one question
/// that can tell them apart on a box where every other verb needs a key.
fn pre_auth_features(address: &str) -> Result<Vec<String>, String> {
    let mut stream = TcpStream::connect(address).map_err(|e| format!("connect {address}: {e}"))?;
    let deadline = Some(Duration::from_secs(10));
    stream.set_read_timeout(deadline).map_err(|e| e.to_string())?;
    stream.set_write_timeout(deadline).map_err(|e| e.to_string())?;
    write_frame(&mut stream, &HandshakeFrame::Hello { proto_version: PROTO_VERSION })
        .map_err(|e| format!("Hello to {address}: {e}"))?;
    match read_frame::<_, HandshakeReply>(&mut stream)
        .map_err(|e| format!("Welcome from {address}: {e}"))?
    {
        HandshakeReply::Welcome { features, .. } => Ok(features),
        other => Err(format!("{address} answered Hello with {other:?}, not a Welcome")),
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

/// This machine's device key as Studio's dials carry it, or `None` to self-skip — the `device_key` row
/// of THIS machine's settings database (where `$VIKE_SETTINGS_DIR` points, else the project walk), the
/// row the desktop reads. No row is the skip; a store that exists and will not answer is a failure
/// (the operator asked for a run).
fn device_keys() -> Option<vike_hub_protocol::auth::HubKeys> {
    let env = env();
    let dir = vike_secrets::workspace_settings_dir_from(
        env.get(vike_secrets::SETTINGS_DIR_ENV).map(String::as_str),
    );
    let row = vike_secrets::access::device_key_load(&dir)
        .unwrap_or_else(|e| panic!("this machine's device key could not be read: {e}"))?;
    let device = vike_hub_protocol::access::DeviceKey::from_pkcs8(&row.private_key)
        .unwrap_or_else(|e| panic!("this machine's device key {} cannot be used: {e}", row.key_id));
    Some(vike_hub_protocol::auth::HubKeys::of_device(device))
}

/// Studio's own Build call, at Studio's own default (backtesthub) address, waited on.
fn build_through_studio(key: vike_hub_protocol::auth::HubKeys) -> Result<String, String> {
    vike_studio::spawn_backtesthub_build(
        StudioRunBackend::source_default().address().to_string(),
        Some(key),
        // The builder builds a FILE: the path the strategy is built as.
        format!("{STRATEGY_NAME}.rs"),
        strategy_source().to_string(),
        None,
    )
    .recv()
    .expect("the build worker thread answered")
}

#[test]
#[ignore = "needs the backtesthub (and its builder) at its default address and this machine's device key — see the module doc"]
fn studio_builds_through_the_address_it_dials_by_default() {
    let Some(key) = device_keys() else {
        eprintln!("SKIP: this machine's settings database holds no device key to build with.");
        return;
    };
    let address = StudioRunBackend::source_default().address().to_string();
    let expected = sha256_hex(strategy_source().as_bytes());
    eprintln!(
        "studio_live_path_smoke: Build -> {address} (Studio's default), expecting sha {expected}"
    );

    let started = Instant::now();
    let sha = match build_through_studio(key) {
        Ok(sha) => sha,
        Err(e) => panic!(
            "Studio's Build did not return a sha from {address}. This is what the Strategy file pane would \
             show, verbatim:\n\n{e}\n"
        ),
    };
    eprintln!("studio_live_path_smoke: BuildOk {sha} in {:.2?}", started.elapsed());
    assert_eq!(
        sha, expected,
        "the Build answered with a content address that is not the sha256 of the source Studio \
         sent — whatever it built, it was not this"
    );
}

#[test]
#[ignore = "needs the backtesthub (and its builder) reachable at its default address — see the module doc"]
fn studio_runs_the_strategy_file_on_the_backtesthub_with_its_device_key() {
    let backend = StudioRunBackend::source_default();
    let address = backend.address().to_string();

    // WHICH daemon is at Studio's compute address — asked before anything is signed, and needing no
    // key, so it runs even when the Run below is skipped.
    let features = pre_auth_features(&address).unwrap_or_else(|e| {
        panic!("nothing answered a Hello at Studio's compute address {address}: {e}")
    });
    let has = |f: &str| features.iter().any(|x| x == f);
    assert!(
        !has(DATA_PLANE_SENTINEL),
        "the server at Studio's compute address {address} is the DATAHUB (it advertises \
         `{DATA_PLANE_SENTINEL}`): {features:?}. Studio's Run would be refused by plane — and with \
         a device key in hand, by the client before it signs. See `crates/vike-desktop/src/main.rs`'s \
         wrong-plane note."
    );
    assert!(
        has(BACKTEST_PLANE_SENTINEL),
        "the server at Studio's compute address {address} does not advertise \
         `{BACKTEST_PLANE_SENTINEL}`, so it is not the backtesthub: {features:?}"
    );
    assert!(
        has(STUDIO_RUNNER_SENTINEL),
        "the backtesthub at {address} advertises no `{STUDIO_RUNNER_SENTINEL}`: it was started \
         without a Studio runner table, and Studio's Run can never succeed there whatever the \
         credential: {features:?}"
    );
    eprintln!("studio_live_path_smoke: {address} is the backtesthub; its Welcome: {features:?}");

    // This machine's device key, read where the desktop reads it — or a SKIP. ⚠ Not an accepted
    // refusal: without the key there is nothing this test can claim about the Run, and a refusal
    // passing here is exactly what this test stopped allowing.
    let Some(keys) = device_keys() else {
        eprintln!(
            "SKIP (the Run): this machine's settings database holds no device key. The daemon above              was identified; the Run itself needs this machine's device key on the server's list at              the role `control` — `vike-cli backend connect <host> --server-project <dir>` makes and              registers it — and re-run."
        );
        return;
    };
    // A sha to run. The same bytes as the Build test, so on a deployment that has built them once
    // this is a cache hit rather than a compile.
    let sha = build_through_studio(keys.clone())
        .unwrap_or_else(|e| panic!("the Build that supplies Run's sha failed:\n\n{e}\n"));

    let spec =
        StrategySpec::strategy_file(STRATEGY_NAME, sha.clone(), vike_studio_core::empty_params());
    let range = TimestampRange { start: Some(START_MS), end: Some(END_MS) };
    let slice = DataSlice::bars(VENUE, SYMBOL, INTERVAL, range);
    eprintln!("studio_live_path_smoke: Run {STRATEGY_NAME}-{sha} -> {address} (Studio's default)");

    let started = Instant::now();
    let engine = vike_studio::StudioRunEngine::default();
    let outcome =
        vike_studio::spawn_backtesthub_run(address.clone(), Some(keys), spec, slice, engine, false)
            .recv()
            .expect("the run worker thread answered");
    match outcome {
        Ok(result) => eprintln!(
            "studio_live_path_smoke: RUN OK against {address} in {:.2?}: {} trades, final equity {}",
            started.elapsed(),
            result.summary.n_trades,
            result.summary.final_equity
        ),
        Err(e) => panic!(
            "Studio's Run, signed with this machine's device key, did not return a result from {address}. With \
             the key present nothing but a result is a pass — a refused key, a missing artifact and \
             a failed run are all red. Studio would show, verbatim:\n\n{e}\n"
        ),
    }
}
