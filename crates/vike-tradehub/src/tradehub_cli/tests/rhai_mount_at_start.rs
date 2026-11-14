//! **A `.rhai` mount resolves and runs AT THE START** — pinned on the code from before the start stopped waiting
//! for a `.rs` file's build (the owner's ruling of 2026-11-14), and kept after it: the file compiles in this
//! process at resolve, no build is asked for, its params line and its file are what they were, and the core
//! holds the mount from its first publish, with nothing handed to it later.

use std::time::{Duration, Instant};

use crate::config::ProfileDaemon;
use crate::tradehub_cli::profile::resolve_mounts;

#[test]
fn a_rhai_mount_resolves_and_runs_at_the_start_as_before() {
    let files = crate::test_strategy_files::standard();
    let profile = ProfileDaemon::from_toml_str(
        "venue = \"bybit\"\nsymbol = \"BTCUSDT\"\ninterval = \"1m\"\n[strategy]\n\
         file = \"buy_hold.rhai\"\n[strategy.params]\nsize = 1.0\n",
    )
    .expect("the profile loads");
    let mut resolved =
        resolve_mounts(&profile, false, Some(files.path()), false).expect("resolves at the start");
    assert_eq!(resolved.len(), 1);
    let m = resolved.remove(0);
    assert!(m.remote.is_none(), "a .rhai file runs in this process");
    assert_eq!(m.strategy_params, "script=buy_hold.rhai size=1");
    let file = m.spec.strategy_file.clone().expect("its file and sha256 are recorded");
    assert_eq!(file.path, "buy_hold.rhai");
    assert_eq!(
        file.sha256,
        vike_hub_protocol::hash::sha256_hex(crate::test_strategy_files::BUY_HOLD.as_bytes())
    );

    // A sentinel this test owns and never creates: the paper book's HALT stays off.
    let sentinel = std::env::temp_dir()
        .join(format!("vike-tradehub-rhai-at-start-{}", std::process::id()))
        .join("HALT");
    let opts = vike_tradehub_core::StrategyCorePaperConfig {
        halt: vike_tradehub_core::StrategyCorePaperHalt::Pinned(sentinel),
        ..Default::default()
    };
    let core = vike_tradehub_core::build_paper_strategy_core_with(m.strategy, &m.spec, opts);
    let cell = core.handle.snapshot_cell();
    // An idle core publishes on a change: an EMPTY parcel of mounts hands it nothing and makes it publish.
    core.handle
        .event_sender()
        .add_account_mounts(Box::new(Vec::<vike_core::Mount>::new()))
        .expect("the core runs");
    let deadline = Instant::now() + Duration::from_secs(10);
    while cell.load().sequence == 0 {
        assert!(Instant::now() < deadline, "the core never published");
        std::thread::sleep(Duration::from_millis(10));
    }
    let held = cell.load().mounts.iter().any(|v| v.mount_id == "bybit__BTCUSDT__1m");
    core.handle.shutdown_and_join();
    assert!(held, "the core holds the .rhai mount from its first publish");
}
