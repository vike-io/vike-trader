//! A live or demo daemon runs only a FROZEN SDK (decision 0296): a build against the builder's
//! development tree is refused there in words with the fix (`crate::config::sandboxed::frozen_for_live`),
//! and a paper daemon or a numbered SDK runs.

use crate::config::OpenRefusal;
use crate::config::sandboxed::frozen_for_live;

#[test]
fn a_live_daemon_refuses_a_development_trees_build_and_names_the_fix() {
    let Err(OpenRefusal::Refused(why)) = frozen_for_live("trend/momentum.rs", true, None) else {
        panic!("a development tree's build on a live daemon is refused for good");
    };
    assert_eq!(
        vike_utility::errors::Declared::declared_id(&why),
        Some("tradehub.profile.strategy.development_sdk_on_live"),
        "the mount row's id"
    );
    let why = why.to_string();
    assert!(why.starts_with("strategy file trend/momentum.rs: NOT RUNNING:"), "{why}");
    assert!(why.contains("not a numbered SDK") && why.contains("--sdk-root"), "{why}");
    assert!(why.contains("paper daemon"), "the other way out is named: {why}");
}

/// **A `.rs` mount's refusal shows ONE id, the one it carries**: the sandboxed child's declared refusal and the
/// builder client's show through the profile's carriers (`MountProfileRefusal::SandboxNotRunning`, `BuilderUnanswered`),
/// their words bare inside, and free words (a model run that cannot be read) show the carrier's own id.
#[test]
fn a_sandboxed_mounts_refusal_shows_the_id_it_carries() {
    use crate::refusals::MountProfileRefusal::{BuilderUnanswered, SandboxNotRunning};
    use vike_utility::errors::CarriedRefusal;
    let file = || "trend/momentum.rs".to_string();
    let child = CarriedRefusal::new(
        Some("strategy.sandbox.not_created".into()),
        "refused: `trend/momentum.rs` did not build from its params and mount facts".into(),
    );
    assert_eq!(
        SandboxNotRunning { file: file(), why: child }.carried().rendered(),
        "error [strategy.sandbox.not_created]: strategy file trend/momentum.rs: NOT RUNNING: refused: \
         `trend/momentum.rs` did not build from its params and mount facts"
    );
    assert_eq!(
        SandboxNotRunning { file: file(), why: "model run `r1` cannot be read".into() }.rendered(),
        "error [tradehub.profile.strategy.not_running]: strategy file trend/momentum.rs: NOT RUNNING: model run \
         `r1` cannot be read"
    );
    let unanswered = BuilderUnanswered {
        file: file(),
        why: CarriedRefusal::new(
            Some("strategy.builder.unreachable".into()),
            "connect to the strategy builder failed: connection refused".into(),
        ),
    };
    assert_eq!(unanswered.shown_id(), "strategy.builder.unreachable");
    assert_eq!(
        unanswered.id().word(),
        "tradehub.profile.strategy.builder_unanswered",
        "its type stays its own"
    );
}

/// The SDK a `.rs` file's build answered (`BuildOk`'s `sdk`, through `SandboxOpened::sdk`) is what the journal
/// records for its mount (`JournalStrategyFile::sdk`, decision 0296's verdict 3); a `.rhai` file records none.
#[test]
fn a_built_files_sdk_is_what_its_journal_record_names() {
    use crate::config::{ProfileDaemon, SandboxOpen, SandboxOpened};
    use vike_strategy_lab::makers::spread_maker::SpreadMaker;
    let dir = super::strategies();
    let built_on_four = |_: &SandboxOpen<'_>| -> Result<SandboxOpened, String> {
        Ok(SandboxOpened {
            strategy: Box::new(SpreadMaker::new(1.0, 0.5)),
            echo: Vec::new(),
            remote: None,
            sdk: Some(4),
        })
    };
    let profile = |file: &str| {
        ProfileDaemon::from_toml_str(&format!(
            "venue = \"hyperliquid\"\nsymbol = \"BTC\"\n[strategy]\nfile = \"{file}\"\n"
        ))
        .expect("a profile")
    };
    let rust = profile("grid.rs").resolve_mount_with(dir.path(), false, &built_on_four);
    assert_eq!(rust.expect("the .rs file mounts").strategy_file().sdk, Some(4));
    let rhai = profile("buy_hold.rhai").resolve_mount_with(dir.path(), false, &built_on_four);
    assert_eq!(rhai.expect("the .rhai file mounts").strategy_file().sdk, None);
}

#[test]
fn a_numbered_sdk_runs_live_and_a_paper_daemon_runs_either() {
    assert_eq!(frozen_for_live("trend/momentum.rs", true, Some(3)), Ok(()));
    assert_eq!(frozen_for_live("trend/momentum.rs", false, None), Ok(()));
    assert_eq!(frozen_for_live("trend/momentum.rs", false, Some(3)), Ok(()));
}
