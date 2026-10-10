//! The params lane by mount id: an `UpdateParams` retunes ONE mount, addressed by the id the
//! snapshot publishes (`MountView::mount_id`), or retunes none and says why. The first-match twin of
//! bug B/D: with two mounts on one series the unaddressed update used to retune the FIRST.
//!
//! A child of `multi_mount.rs` on purpose, like `attribution.rs`: it reuses that file's private
//! `Listener` (which logs `on_params_updated` as `"params"`), `Heard`, `listener_mount` and
//! `listener_core`, and the parent is at the crate's 1,000-line file cap.

use super::*;

/// The params lane (an `UpdateParams` retunes ONE mount, or says why it retuned none): a
/// `SpreadMaker` bag. The listeners never read it, so only its shape matters.
fn retune_params() -> StrategyParams {
    serde_json::from_value(serde_json::json!({ "SpreadMaker": {
        "qty": 1.0, "half_spread": 0.5, "target_inventory": 0.0, "max_inventory": 1.0,
        "skew": 0.25, "fill_window_ms": 1000, "net_fill_threshold": 2.5,
        "suppress_cooldown_ms": 5000, "style": "Join", "depth_levels": 2, "tick_size": 0.5,
        "filter_own": true, "avellaneda_stoikov": null, "refresh_tolerance": null,
        "ladder": null, "reward": null, "toxicity": null
    } }))
    .unwrap()
}

/// One `UpdateParams` for `(sim, BTCUSDT, <interval>)` through the real command arm.
fn retune(core: &mut CoreThread<RecordingClient>, mount_id: Option<&str>, interval: &str) {
    core.dispatch(Ingest::Command(Command::UpdateParams(Box::new(ParamsUpdate {
        venue: "sim".into(),
        symbol: "BTCUSDT".into(),
        interval: interval.into(),
        mount_id: mount_id.map(str::to_string),
        params: retune_params(),
    }))));
}

/// The labels of the mounts whose `on_params_updated` ran, in order.
fn retuned(heard: &Heard) -> Vec<&'static str> {
    let got = heard.lock().unwrap();
    got.iter().filter(|(_, hook)| *hook == "params").map(|(label, _)| *label).collect()
}

fn recent_has(core: &CoreThread<RecordingClient>, needle: &str) -> bool {
    core.recent.iter().any(|l| l.contains(needle))
}

/// **The first-match trap.** Two mounts share one series and the update names no mount: before the
/// fix the FIRST one (A) was retuned and B was never heard of. Now neither is, and the ring names
/// the mounts that share the series so the operator knows to address one.
#[test]
fn an_unaddressed_params_update_on_a_shared_series_retunes_neither_and_says_so() {
    let heard: Heard = Arc::new(TestMutex::new(Vec::new()));
    let mut core = listener_core(vec![
        listener_mount("A", "1m", "maker-a", false, &heard),
        listener_mount("B", "1m", "maker-b", false, &heard),
    ]);

    retune(&mut core, None, "1m");

    assert_eq!(retuned(&heard), Vec::<&str>::new(), "an ambiguous update reaches NO mount");
    assert!(
        recent_has(&core, "2 mounts share sim/BTCUSDT @ 1m (`maker_a`, `maker_b`)")
            && recent_has(&core, "address one by mount id"),
        "the refusal names the sharing ids: {:?}",
        core.recent
    );
}

/// An addressed update reaches exactly its mount, and the id is matched the way ids are STORED
/// (sanitized): `maker-b` finds `maker_b`.
#[test]
fn a_params_update_addressed_by_mount_id_retunes_only_that_mount() {
    let heard: Heard = Arc::new(TestMutex::new(Vec::new()));
    let mut core = listener_core(vec![
        listener_mount("A", "1m", "maker-a", false, &heard),
        listener_mount("B", "1m", "maker-b", false, &heard),
    ]);

    retune(&mut core, Some("maker-b"), "1m");
    assert_eq!(retuned(&heard), vec!["B"], "only B, found through the sanitized id");
    retune(&mut core, Some("maker_a"), "1m");
    assert_eq!(retuned(&heard), vec!["B", "A"], "then only A, by its stored spelling");
    assert!(core.recent.is_empty(), "a delivered update says nothing: {:?}", core.recent);
}

/// Every addressed update that cannot be delivered changes nothing and leaves a note saying which
/// rule it broke: an empty id, an id no mount has, an id whose mount was unmounted, and an id that
/// is live but mounted on a different series than the sender believes.
#[test]
fn a_params_update_that_names_no_live_mount_or_the_wrong_series_retunes_nothing() {
    let heard: Heard = Arc::new(TestMutex::new(Vec::new()));
    let mut core = listener_core(vec![
        listener_mount("A", "1m", "maker-a", false, &heard),
        listener_mount("B", "1m", "maker-b", false, &heard),
        // C: live ON the 5m series the mismatched id is sent to; a fall-back-to-series bug retunes it
        listener_mount("C", "5m", "maker-c", false, &heard),
    ]);
    core.unmount_strategy_runtime("maker-b");

    for (id, interval, note) in [
        ("  ", "1m", "PARAMS REFUSED: empty mount id"),
        ("nope", "1m", "no live strategy mount with id `nope`"),
        ("maker-b", "1m", "no live strategy mount with id `maker_b`"),
        ("maker-a", "5m", "mount `maker_a` runs sim/BTCUSDT @ 1m, not sim/BTCUSDT @ 5m"),
    ] {
        retune(&mut core, Some(id), interval);
        assert!(recent_has(&core, note), "`{id}` @ {interval} leaves `{note}`: {:?}", core.recent);
    }
    assert_eq!(retuned(&heard), Vec::<&str>::new(), "none of the four reached A (1m) or C (5m)");
}

/// The unaddressed shape keeps working where it is unambiguous (one mount on the series), says so
/// where no mount is there, and an id the snapshot publishes — derived or explicit — is one the
/// update accepts.
#[test]
fn an_unaddressed_params_update_still_reaches_the_sole_mount_and_ids_are_published() {
    let heard: Heard = Arc::new(TestMutex::new(Vec::new()));
    let mut core = listener_core(vec![
        listener_mount("A", "1m", "maker-a", false, &heard),
        StrategyMount { controller_id: None, ..listener_mount("B", "5m", "unused", false, &heard) },
    ]);

    retune(&mut core, None, "1m");
    assert_eq!(retuned(&heard), vec!["A"], "one mount on the series: delivered as it always was");
    retune(&mut core, None, "15m");
    assert_eq!(retuned(&heard), vec!["A"], "nothing on 15m: nothing retuned");
    assert!(recent_has(&core, "no live strategy mount on sim/BTCUSDT @ 15m"), "{:?}", core.recent);

    let ids: Vec<String> = core.mount_views().into_iter().map(|v| v.mount_id).collect();
    assert_eq!(ids, ["maker_a", "sim__BTCUSDT__5m", ""], "explicit, derived, and the residual row");
    retune(&mut core, Some(&ids[1]), "5m");
    assert_eq!(retuned(&heard), vec!["A", "B"], "a published id addresses its mount");
}
