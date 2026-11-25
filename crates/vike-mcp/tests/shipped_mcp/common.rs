//! The two fixtures the goldens stand on, the same bodies as `crates/vike-cli/tests/common/mod.rs`'s
//! (the two programs are siblings, so neither test tree may name the other).

/// Create a scratch settings directory's database, EMPTY, the way a fresh box creates it.
pub fn seed_store(settings: &std::path::Path) {
    vike_secrets::create_store(Some(settings.to_str().expect("a UTF-8 scratch path")))
        .expect("create the scratch settings database");
}

/// [`seed_store`], then this box's DEVICE key (decision 0288 item 8: what the program signs with) for the fixture
/// VALUE `value` ([`device_for`]), so a hub keyed with the same value ([`fixture_access`]) lists it.
pub fn seed_device(settings: &std::path::Path, value: &str) {
    seed_store(settings);
    let (device, pkcs8) = device_for(value);
    vike_secrets::access::device_key_insert(settings, &device.key_id(), "owner", "mcp", &pkcs8)
        .expect("write the fixture's device key");
}

/// **The fixtures' device keys**: ONE Ed25519 pair per key VALUE a fixture spells, made once per test process, so a
/// store seeded with a value ([`seed_device`]) and a node keyed with the same value ([`fixture_access`]) agree.
pub fn device_for(value: &str) -> (vike_hub_protocol::access::DeviceKey, Vec<u8>) {
    /// One fixture value's pair: the key and its PKCS#8 document.
    type Made = (vike_hub_protocol::access::DeviceKey, Vec<u8>);
    static MADE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, Made>>> =
        std::sync::OnceLock::new();
    let made = MADE.get_or_init(Default::default);
    let mut made = made.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    made.entry(value.to_string())
        .or_insert_with(|| vike_hub_protocol::access::DeviceKey::generate().expect("a device key"))
        .clone()
}

/// The access list of a fixture hub keyed with `observe` / `control` VALUES: the device of `observe` at the role
/// `observe` and the device of `control` at the role `control` (an empty value lists nothing).
pub fn fixture_access(observe: &str, control: &str) -> vike_hub_protocol::access::AccessList {
    let mut rows = std::collections::HashMap::new();
    for (value, role) in [(observe, "observe"), (control, "control")] {
        if value.is_empty() {
            continue;
        }
        let (device, _) = device_for(value);
        rows.insert(
            device.key_id(),
            vike_hub_protocol::access::AccessEntry {
                person: "owner".into(),
                device: "mcp".into(),
                role: role.into(),
                public_key: device.public_key().to_vec(),
                revoked: false,
            },
        );
    }
    vike_hub_protocol::access::AccessList::new(move |key_id| rows.get(key_id).cloned())
}

/// Far-future resolution so the A-S horizon is positive.
const PAPER_NODE_RESOLUTION_TIMESTAMP: i64 = 3_000_000_000;

/// A PAPER `vike-tradehub` node on `venue`/`token`, served on an ephemeral loopback port with CONTROL
/// enabled. Hold the returned mount for the case's lifetime.
pub fn spawn_paper_node(
    venue: &str,
    token: &str,
    observe_key: &str,
    control_key: &str,
) -> (vike_tradehub_core::StrategyCorePaper, std::net::SocketAddr) {
    use vike_tradehub::{publish, server};

    let mount = vike_tradehub_core::test_support::paper_maker::paper_maker_core(
        vike_strategy_lab::makers::spread_maker::SpreadMaker::from_params,
        venue,
        token,
        Some(PAPER_NODE_RESOLUTION_TIMESTAMP),
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let publisher = publish::spawn(mount.handle.snapshot_cell(), None);
    let commands = Some(mount.handle.command_sink());
    let keys = vike_hub_protocol::auth::HubKeys::new(
        observe_key.as_bytes().to_vec(),
        control_key.as_bytes().to_vec(),
    )
    .with_access(fixture_access(observe_key, control_key));
    std::thread::spawn(move || {
        let _ = server::serve(
            listener,
            publisher,
            keys,
            commands,
            server::control::ControlLimitsConfig::default(),
            None,
            None,
            None,
        );
    });
    (mount, addr)
}
