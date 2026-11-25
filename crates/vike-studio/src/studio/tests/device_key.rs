//! **Studio signs every remote backtesthub dial with this PC's DEVICE key** (decision 0288; the shared
//! backtest key it signed with until the cutover is verified by no hub). Over a real KEYED backtesthub
//! whose access list holds this device at `control`, Studio's own Run reaches a result; with the
//! device off the list, or listed at `observe` only, the same Run is refused in words that say how to
//! get on the list.

use super::super::*;
use super::support::{seeded_store, spawn_backtesthub_server_keyed, state_new};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use vike_hub_protocol::access::{AccessEntry, AccessList, DeviceKey};
use vike_hub_protocol::auth::HubKeys;

/// Press Run and poll until it answers.
fn run_to_the_end(st: &mut StudioState) -> crate::backend::backtesthub::StudioRunViewOutcome {
    st.start_run();
    assert!(st.running, "the Run was not dispatched");
    for _ in 0..500 {
        st.poll();
        if !st.running {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(!st.running, "the Run never answered");
    st.last.take().expect("an outcome")
}

#[test]
fn a_listed_device_runs_and_an_unlisted_one_is_told_how_to_register() {
    let (device, _) = DeviceKey::generate().expect("a device key");
    // The server's list: this device, at a role the test moves; `listed` false is no row at all.
    let listed = Arc::new(AtomicBool::new(true));
    let role = Arc::new(Mutex::new("control".to_string()));
    let list = {
        let (key_id, public_key) = (device.key_id(), device.public_key().to_vec());
        let (listed, role) = (Arc::clone(&listed), Arc::clone(&role));
        AccessList::new(move |asked| {
            (asked == key_id && listed.load(Ordering::SeqCst)).then(|| AccessEntry {
                person: "owner".into(),
                device: "pc".into(),
                role: role.lock().unwrap().clone(),
                public_key: public_key.clone(),
                revoked: false,
            })
        })
    };
    let (dir, store) = seeded_store();
    let server = HubKeys::new(Vec::new(), Vec::new()).with_access(list);
    let address = spawn_backtesthub_server_keyed(Arc::clone(&store), Some(server));

    let mut st = state_new(&dir, store);
    st.backend = crate::backend::backtesthub::StudioRunBackend::Source { address };
    st.hub_keys = Some(HubKeys::of_device(device));
    st.picker.select(0);
    st.strategy_source = StudioStrategySource::File;
    st.picked_file = "trend/buy_hold.rs".to_string();
    st.file_params = vec![("symbol".into(), "BTCUSDT".into()), ("size".into(), "2".into())];

    let result = run_to_the_end(&mut st).expect("a device listed at `control` runs");
    assert!(!result.summary.curve.is_empty());

    for (what, on_the_list, at) in
        [("off the list", false, "control"), ("at observe", true, "observe")]
    {
        listed.store(on_the_list, Ordering::SeqCst);
        *role.lock().unwrap() = at.to_string();
        let refused = run_to_the_end(&mut st).expect_err(what).to_string();
        for needle in
            ["refused this PC's device key", "Data Manager → Backend → Server", "Set role"]
        {
            assert!(refused.contains(needle), "{what}: missing {needle:?}: {refused}");
        }
    }
}

/// **A Studio opened before this PC held its device key signs with it once the key moves, with no
/// restart.** The shell's per-frame step (`crates/vike-desktop/src/studio_wiring.rs`'s `open_studio`):
/// seed `hub_keys` when `hub_keys_due` says so. Before the Connect the seed finds no key and the Run is
/// refused for holding none; a frame at the same epoch seeds nothing again; the Connect moves the
/// epoch, and the next frame's seed is what the same Studio's next Run signs with.
#[test]
fn a_studio_opened_before_its_device_key_existed_signs_with_it_after_the_connect() {
    let (device, _) = DeviceKey::generate().expect("a device key");
    let list = {
        let (key_id, public_key) = (device.key_id(), device.public_key().to_vec());
        AccessList::new(move |asked| {
            (asked == key_id).then(|| AccessEntry {
                person: "owner".into(),
                device: "pc".into(),
                role: "control".into(),
                public_key: public_key.clone(),
                revoked: false,
            })
        })
    };
    let (dir, store) = seeded_store();
    let server = HubKeys::new(Vec::new(), Vec::new()).with_access(list);
    let address = spawn_backtesthub_server_keyed(Arc::clone(&store), Some(server));

    let mut st = state_new(&dir, store);
    st.backend = crate::backend::backtesthub::StudioRunBackend::Source { address };
    st.picker.select(0);
    st.strategy_source = StudioStrategySource::File;
    st.picked_file = "trend/buy_hold.rs".to_string();
    st.file_params = vec![("symbol".into(), "BTCUSDT".into()), ("size".into(), "2".into())];
    // The desktop's frame: seed from the key this PC holds when the epoch moved.
    let frame = |st: &mut StudioState, epoch: u64, held: Option<&DeviceKey>| {
        if st.hub_keys_due(epoch) {
            st.hub_keys = held.map(|d| HubKeys::new(Vec::new(), Vec::new()).with_device(d.clone()));
        }
    };

    frame(&mut st, 0, None);
    let refused = run_to_the_end(&mut st).expect_err("no key yet").to_string();
    assert!(refused.contains("holds no device key"), "{refused}");

    frame(&mut st, 0, Some(&device));
    assert!(st.hub_keys.is_none(), "a frame at an unmoved epoch seeded again");
    frame(&mut st, 1, Some(&device));
    let result = run_to_the_end(&mut st).expect("the key the Connect made runs, with no restart");
    assert!(!result.summary.curve.is_empty());
}
