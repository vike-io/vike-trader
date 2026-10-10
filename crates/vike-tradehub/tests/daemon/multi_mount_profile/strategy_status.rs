//! `StrategyStatus` over the wire: a publisher spawned with N mount rows answers N rows.

use vike_tradehub::publish;
use vike_tradehub_client::NodeKeys;
use vike_tradehub_client::proto::{Request, Response, read_frame, write_frame};
use vike_tradehub_client::wire::{WireMountRow, WireNodeIdentity};

use crate::support::{authed_observe_stream, serve_on_loopback};

// ---------------------------------------------------------------------------------------------
// StrategyStatus: N rows over the wire
// ---------------------------------------------------------------------------------------------

/// The observe key this test's server and client share.
const OBSERVE_KEY: &[u8] = b"multi-mount-observe-key";

/// A `StrategyStatus` answered from a publisher spawned WITH mount rows carries all N rows
/// verbatim — the `mounts: Vec<WireMountRow>` B4 shipped for exactly this — while
/// `effective_params`/identity keep the daemon-level singular shape. (The mount-less `spawn`
/// fallback — one identity-derived row — is pinned by `observe_roundtrip.rs`.)
#[test]
fn strategy_status_returns_one_row_per_mount() {
    vike_log::test_init();
    // A real (single-mount) paper core supplies the snapshot cell; the ROWS under test are the
    // publisher's process-static block, exactly as `main.rs` passes them.
    let cfg = vike_mount::MakerMountConfig::outcome_token(
        "polymarket",
        "MM_STATUS_TOK",
        Some(3_000_000_000),
    );
    let mount = vike_mount::build_paper_maker_core(&cfg);
    let identity = WireNodeIdentity {
        name: "multi-mount-status".into(),
        strategy: "buy_hold+grid".into(),
        params: "joined".into(),
        live: false,
        build: "test-build".into(),
        advertise_addr: String::new(),
    };
    let rows = vec![
        WireMountRow {
            strategy: "buy_hold".into(),
            params: "venue=polymarket symbol=TOK_A interval=1m :: size=3".into(),
            live: false,
            venue: String::new(),
            symbol: String::new(),
            interval: String::new(),
            mount_id: String::new(),
            typed_params: None,
            asset_class: None,
        },
        WireMountRow {
            strategy: "grid".into(),
            params: "venue=binance symbol=BTCUSDT interval=1m :: size=1 rungs=4".into(),
            live: false,
            venue: String::new(),
            symbol: String::new(),
            interval: String::new(),
            mount_id: String::new(),
            typed_params: None,
            asset_class: None,
        },
    ];
    let publisher =
        publish::spawn_with_mounts(mount.handle.snapshot_cell(), Some(identity), rows.clone());
    let keys = NodeKeys::new(OBSERVE_KEY.to_vec(), Vec::new());
    // No SettingsShow source — this test's server answers StrategyStatus only.
    let addr = serve_on_loopback(publisher.clone(), keys, None, None, None);

    let mut stream = authed_observe_stream(addr, OBSERVE_KEY);
    write_frame(&mut stream, &Request::StrategyStatus).expect("status request");
    match read_frame::<_, Response>(&mut stream).expect("status response") {
        Response::StrategyStatus(status) => {
            assert_eq!(status.identity.name, "multi-mount-status");
            // The PROCESS-STATIC halves ride through verbatim, one row per mount in mount order —
            // the property this test was written for, unchanged by the live overlay below.
            assert_eq!(
                status
                    .mounts
                    .iter()
                    .map(|m| (m.strategy.as_str(), m.params.as_str(), m.live))
                    .collect::<Vec<_>>(),
                rows.iter()
                    .map(|m| (m.strategy.as_str(), m.params.as_str(), m.live))
                    .collect::<Vec<_>>(),
                "one row per mount, verbatim, in mount order"
            );
            // ...and the half the LIVE overlay must never break. The core behind this publisher has
            // ONE mount against a static block of TWO, so the second row is one the snapshot cannot
            // match — it is KEPT rather than dropped, because a status that omits a mount reads as
            // "that mount is gone", a claim about a live book. (What the overlay WRITES onto a row
            // it CAN match is the subject of
            // `crates/vike-tradehub/tests/daemon/live_params_overlay.rs`, driven through a real
            // publish.)
            assert_eq!(status.mounts.len(), 2, "the overlay drops no row it cannot match");
            assert_eq!(
                (
                    status.mounts[1].venue.as_str(),
                    status.mounts[1].symbol.as_str(),
                    status.mounts[1].interval.as_str()
                ),
                ("", "", ""),
                "an unmatched row keeps its shape rather than borrowing another mount's key"
            );
            assert!(status.mounts[1].typed_params.is_none(), "...and types nothing");
            assert_eq!(
                status.effective_params, "joined",
                "the daemon-level params line stays the identity's"
            );
        }
        other => panic!("StrategyStatus must answer, got {other:?}"),
    }

    publisher.shutdown();
    mount.handle.shutdown_and_join();
}
