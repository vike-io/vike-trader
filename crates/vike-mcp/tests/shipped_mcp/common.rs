//! The two fixtures the goldens stand on, the same bodies as `crates/vike-cli/tests/common/mod.rs`'s
//! (the two programs are siblings, so neither test tree may name the other).

/// Create a scratch settings directory's database, and write `rows` where the production writer puts
/// them: a platform key in the `hub_key` table, any other name in `credential` as a deployment row.
pub fn seed_store(settings: &std::path::Path, rows: &[(&str, &str)]) {
    vike_secrets::create_store(Some(settings.to_str().expect("a UTF-8 scratch path")))
        .expect("create the scratch settings database");
    let mut node = Vec::new();
    let mut credential = Vec::new();
    for (k, v) in rows {
        let row = (k.to_string(), v.to_string());
        if vike_venues::credential_keys::is_platform_key(k) {
            node.push(row);
        } else {
            credential.push(row);
        }
    }
    if !node.is_empty() {
        vike_secrets::save_credentials_to_store_unjournalled(
            settings,
            vike_secrets::StoreTable::HubKey,
            &node,
            vike_secrets::CredentialUpsertIntent::Add,
        )
        .expect("write the fixture's hub keys");
    }
    if !credential.is_empty() {
        vike_secrets::save_credentials_to_store_unjournalled(
            settings,
            vike_secrets::StoreTable::Credential,
            &credential,
            vike_secrets::CredentialUpsertIntent::Add,
        )
        .expect("write the fixture's credentials");
    }
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
        vike_strategies_check::makers::spread_maker::SpreadMaker::from_params,
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
    );
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
