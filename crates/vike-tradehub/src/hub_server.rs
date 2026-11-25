//! The node server's STARTUP — everything [`crate::tradehub_cli::run`] decides about whether this
//! daemon opens an authenticated TCP surface at all, and which capabilities that surface carries:
//! the bind classification, the hub-key store read, the control gate that withholds the command sink,
//! the account-admin barrier, and the publisher plus accept thread that serve the result.
//!
//! ⚠ **This module is new with the `run` phase split** (code-layout phase 2, Task 12b): these four
//! functions were private items of `crate::tradehub_cli` and moved here WITHOUT their bodies
//! changing — the only differences are `pub(crate)` on the three that [`crate::tradehub_cli`] or its
//! tests call, and the imports this file now spells for itself. The call stays at ONE site in
//! `run`, with every input that decides whether a remote order-write surface opens spelled as an
//! argument there, which is the reason `start_observe_server` takes them as parameters and reads
//! no settings of its own.
//!
//! It reads two things back from `crate::tradehub_cli`, which owns them: the credential store handle
//! the boot opened (`credential_store`) and the resolved control limits (`resolve_control_limits`).

use std::net::{TcpListener, ToSocketAddrs};

use vike_core::CoreHandle;
use vike_hub_protocol::access::{AccessEntry, AccessList};
use vike_hub_protocol::auth::HubKeys;

use crate::publish::{self, PublisherHandle};
use crate::server;
use crate::tradehub_cli::settings::{credential_store, resolve_control_limits};

/// **The node server's DOOR**: everything [`start_observe_server`] needs that does not need a running
/// core — the bound listener, the verified keys, the account capability — decided once, BEFORE the
/// engines start, so the port listens (answering `starting`, see [`ObserveDoor::answer_starting`])
/// while a slow venue is still being started. `None` where the old function returned `None` for
/// the same reasons (no address, a refused bind, no key, an unreadable store).
pub(crate) struct ObserveDoor {
    addr: String,
    listener: TcpListener,
    keys: HubKeys,
    limits: server::control::ControlLimitsConfig,
    accounts: Option<server::accounts::AccountAdminSource>,
    control_enabled: bool,
    datahub_advertise: Option<String>,
    observer_stream: publish::ObserverStream,
}

impl ObserveDoor {
    /// Start answering every authenticated peer with `starting` and the node's progress, on a clone
    /// of this door's listener. `None` (logged) when the thread could not be started: the node then
    /// has no door until the server, as before.
    pub(crate) fn answer_starting(
        &self,
        progress: server::starting::StartProgress,
    ) -> Option<server::starting::StartingDoor> {
        match server::starting::StartingDoor::open(
            &self.listener,
            self.keys.clone(),
            self.datahub_advertise.clone(),
            self.control_enabled,
            self.accounts.is_some(),
            progress,
        ) {
            Ok(door) => {
                tracing::info!(
                    addr = %self.addr,
                    "node server listening while the engines start: every verb answers `starting` \
                     until the server takes over"
                );
                Some(door)
            }
            Err(e) => {
                tracing::warn!(addr = %self.addr, error = %e, "the starting door could not open; the port stays closed until the engines are up");
                None
            }
        }
    }

    /// Stop answering `starting` (when the door was open) and return the door ready for
    /// [`start_observe_server`]; `None` (logged) when the listener cannot be made blocking again, in
    /// which case no server is started and the daemon keeps trading headless.
    pub(crate) fn stop_answering_starting(
        self,
        starting: Option<server::starting::StartingDoor>,
    ) -> Option<Self> {
        // With no door the listener must still be blocking for the server: a door whose open failed
        // after the mode switch is repaired in `open`, and this is the backstop for any path that is not.
        let given_back = match starting {
            Some(starting) => starting.hand_over(&self.listener),
            None => self.listener.set_nonblocking(false),
        };
        if let Err(e) = given_back {
            tracing::error!(addr = %self.addr, error = %e, "the starting door could not hand its listener to the server — observe server NOT started; the daemon keeps trading headless");
            return None;
        }
        Some(self)
    }
}

/// Open the node server's door (see [`ObserveDoor`]) IFF an ADDRESS was configured, and run every
/// decision that does not need the core: the bind classification, the access list, the control
/// gate, the account-admin barrier, the bind itself.
///
/// Both gates arrive as PARAMETERS, resolved once by `crate::tradehub_cli`'s `resolve_settings` at
/// the top of [`crate::tradehub_cli::run`]:
/// `addr` is the `config.tradehub_addr` row and `control_enabled` the `flags.tradehub_control` row
/// (decision 0111: no variable reaches either). Taking them as arguments puts the decision to open
/// a remote order-write surface at ONE call site, where a diff can see it.
///
/// - `addr = None` (or blank) ⇒ `None`: the daemon is byte-identical to the pure-stdio PR-9 daemon.
/// - Addr set but no settings directory resolved ⇒ logged + `None`: there is no access list to admit a
///   device by (decision 0288 item 8: no shared key is read), and the daemon keeps trading headless. The
///   list reads the store HERE, in the daemon binary (`access_list`); the light `vike-tradehub-client`
///   crate must not.
/// - A bind failure is likewise logged and non-fatal (the daemon keeps trading).
/// - A NON-LOOPBACK address ⇒ logged + `None` unless `allow_public_bind` — see below.
///
/// ## ⚠ The non-loopback refusal (`allow_public_bind` = `flags.tradehub_allow_public_bind`)
///
/// `crate::server`'s handshake is PLAINTEXT and authenticates the CONNECTION, not each
/// frame — so on a reachable network it hands out an offline cracking target for the hub key and,
/// after `AuthOk`, an on-path attacker can inject a `Command`. The design answer is a loopback
/// listener plus an SSH tunnel (`ssh -L 7879:localhost:7879 the CI box`), the same way `vike-datahub` is
/// reached; `vike_config::config::DEFAULT_TRADEHUB_ADDRESS` is loopback for that reason. Nothing ENFORCED it: `check_address`
/// only requires a `:`, so `tradehub_addr = "0.0.0.0:7879"` — which is simply what one types for a
/// server — published an order-write surface with no warning.
///
/// So a non-loopback bind now needs a SECOND, differently-named opt-in
/// (the `flags.tradehub_allow_public_bind` row, or the `--allow-public-bind` argument a bridge-network
/// container's OPERATOR passes after the image name; the image passes none, decision 0280). It is a flag
/// rather than a refusal without an escape hatch because reaching a node from a trusted LAN or
/// from inside a WireGuard/VPN interface is legitimate, and a guard people cannot turn off is a
/// guard they route around. It is a SEPARATE flag rather than an inference from the address
/// because the mistake this catches is *typing an address*, and no address can be its own consent.
/// ON, the bind proceeds behind a `warn!` naming what is now exposed — never silently.
///
/// The publisher reads ONLY the arc-swap snapshot cell (`CoreHandle::snapshot_cell`), never the core
/// fold, and the accept loop runs on a detached thread. ORDER CONTROL is double-gated: `Scope::Write`
/// is offered only when `control_enabled` — off, no device's role reaches it (`server::handshake`'s
/// `scope_admission` asks for the command sink) AND no `CommandSink` is passed to `serve`, so a
/// `Control` auth / `Command` is refused two ways.
pub(crate) fn open_observe_door(
    addr: Option<&str>,
    control_enabled: bool,
    allow_public_bind: bool,
    // The ACCOUNT-ADMIN declaration (`config.tradehub_account_admin`), caller-owned like every
    // other settings fact here. This function DECIDES from it — see `account_admin_source` — and
    // the decision is deliberately taken beside the bind decision rather than in `main`, because
    // the two ask the same `resolved` addresses and a second resolution could answer differently.
    account_admin: Option<&str>,
    // The BOOT's settings directory, so the account store and the daemon's own credential read
    // resolve from ONE walk.
    settings_dir: Option<&std::path::Path>,
    // The REQ-2 advertisement (`config.datahub_advertise_addr`), likewise caller-owned: this
    // function stamps it into `Welcome` and decides nothing about it.
    datahub_advertise: Option<&str>,
    // The observer stream's two rows (`config.observer_frames_per_second`, `config.observer_frame_compression`), likewise
    // caller-owned: handed on to the publisher the server starts.
    observer_stream: publish::ObserverStream,
) -> Option<ObserveDoor> {
    // An absent, blank or whitespace-only address means "no publisher" — the `?` returns None for
    // the whole function, as the explicit `None => return None` arm did before 1.97's
    // `question_mark` lint asked for it.
    let addr = addr.map(str::trim).filter(|a| !a.is_empty())?.to_string();
    // Resolve ONCE, exactly as `TcpListener::bind` will, and classify before anything else is built
    // — no key read, no publisher spawned, no socket opened on a refusal. An address that resolves
    // to nothing is left to `bind` to reject with its own message (below), as it always was.
    let resolved: Vec<std::net::SocketAddr> =
        addr.to_socket_addrs().map(|it| it.collect()).unwrap_or_default();
    match server::bind_decision(&resolved, allow_public_bind) {
        server::BindDecision::Proceed => {}
        server::BindDecision::ProceedExposed(exposed) => {
            tracing::warn!(
                %addr, %exposed, control = control_enabled,
                "node server binding a NON-LOOPBACK address (a `flags.tradehub_allow_public_bind` \
                 row, or --allow-public-bind) — the node \
                 handshake is PLAINTEXT and authenticates the connection, not each frame, so \
                 anyone who can reach this address can collect the nonce+mac for offline cracking \
                 and, once a session is up, inject frames. Put a tunnel or a VPN in front of it"
            );
        }
        // ⚠ The cure is for both shapes: a container's image consents for nobody (decision 0280), so
        // this line is what a `docker run` operator reads, and a host install or a `vikectl` server
        // (which take no argument) consents with the row.
        server::BindDecision::Refuse(exposed) => {
            tracing::error!(
                %addr, %exposed,
                "node-server address is NOT loopback — observe server NOT started. This surface's \
                 handshake is plaintext and (with control on) places REAL orders, so keep \
                 `config.tradehub_addr` on 127.0.0.1. To open trading to the network on purpose, \
                 start the container with `--allow-public-bind` and firewall the port to your own \
                 IP. Otherwise connect with `vike-cli backend connect HOST --server-project DIR` \
                 over ssh. (A host install or a `vikectl` server consents with `vike-cli config set \
                 flags.tradehub_allow_public_bind true` instead.) The daemon keeps trading headless"
            );
            return None;
        }
    }
    // ⚠ **No shared key is read here any more** (decision 0288 item 8, the cutover): not the tradehub's
    // observe/control pair, not its admin key. A person reaches this server by a device key the ACCESS LIST
    // admits, over the boot's own settings directory, and nothing else; a hub with no list serves no person.
    // The two CLIENT keys this daemon holds to reach the gatewayhub and the datahub (0164's daemon keys) are
    // read elsewhere (`resolve_daemon_hub_key_store`) and verify nothing here.
    let Some(dir) = settings_dir else {
        tracing::error!(
            %addr,
            "a node-server address is configured but this daemon resolved NO settings directory at \
             boot (no project above its working directory), so there is no access list to admit a \
             device by — observe server NOT started. Set $VIKE_SETTINGS_DIR, or run the daemon from \
             its project root; the daemon keeps trading headless"
        );
        return None;
    };
    // ⚠ **THE ACCOUNT-ADMIN CAPABILITY, decided HERE and nowhere else.** `None` — every box that
    // has not declared the barrier — means the server holds no account writer at all, advertises
    // no `account-verbs` capability, and is byte-identical to a binary without the verb. The three
    // questions it asks, and why the process cannot ask the confidentiality one for itself, are on
    // `account_admin_source`. Who may then USE it is a device's admin ROLE (decision 0288, Q3 = A).
    //
    // ⚠ It reads the SAME `resolved` addresses `bind_decision` classified above, rather than
    // resolving the address a second time: two resolutions of one hostname can answer differently
    // (a DNS change between them), and the one thing this capability may not do is arm against a
    // bind that is not the one that happened.
    let accounts =
        account_admin_source(account_admin, &resolved, settings_dir, credential_store().scope());
    // The served keys: no shared key at all, the access list alone. With control OFF no device reaches
    // `Write` either (`server::handshake`'s `scope_admission` asks the flag), and no `CommandSink` is
    // passed to `serve`, so a `Command` is refused two ways.
    let keys = HubKeys::new(Vec::new(), Vec::new()).with_access(access_list(dir.to_path_buf()));
    if control_enabled {
        tracing::warn!(
            "order-control channel ENABLED (the `flags.tradehub_control` row) — a \
             Control-authenticated peer may place/cancel REAL orders"
        );
    }
    let limits = resolve_control_limits();
    let listener = match TcpListener::bind(&addr) {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(%addr, error = %e, "observe server bind failed; daemon keeps trading headless");
            return None;
        }
    };
    // The REQ-2 datahub advertisement (the `config.datahub_advertise_addr` row): set ⇒ every Welcome carries `datahub=<addr>` and a client
    // with no explicit `datahub_addr` of its own dials the datahub there. Blank-trimmed like
    // `addr` above; unset advertises nothing (the pre-REQ-2 Welcome). ⚠ The REQ-7 settings source
    // is NOT built here — the caller owns it and hands it to [`start_observe_server`], which is
    // why only the advertisement is normalized at this line.
    let datahub_advertise =
        datahub_advertise.map(str::trim).filter(|a| !a.is_empty()).map(str::to_string);
    Some(ObserveDoor {
        addr,
        listener,
        keys,
        limits,
        accounts,
        control_enabled,
        datahub_advertise,
        observer_stream,
    })
}

/// Start the authenticated node server (observe always; order-control gated) + snapshot publisher on
/// the [`ObserveDoor`] [`open_observe_door`] opened, now that the core is up. Returns the
/// [`PublisherHandle`] so the bounded teardown can stop it; `None` (logged) when the door's
/// listener cannot be used, and the daemon keeps trading headless.
///
/// The door must not be answering `starting` any more ([`ObserveDoor::stop_answering_starting`]):
/// this function serves on its listener in blocking mode.
///
/// The command sink is WITHHELD unless control is enabled — the second, independent gate, so even
/// a device whose role is control can never reach the core without the master flag.
pub(crate) fn start_observe_server(
    door: ObserveDoor,
    handle: &CoreHandle,
    identity: vike_tradehub_client::wire::HubIdentityWire,
    mounts: Vec<vike_tradehub_client::wire::MountViewWire>,
    // The accounts this node does NOT run, each with why (decision 0156): process-static publisher
    // data like `mounts`, stamped into every frame.
    not_running: Vec<vike_execution::ArmingNotRunningView>,
    // The `SettingsShow`/`SetSetting` source (REQ-7): the boot-resolved settings directory, this
    // binary's ONE startup env sweep and the hot-apply seam, built by the CALLER — it owns all
    // three facts (the settings-registry rule), and this function only hands the handle on.
    settings_source: server::settings::SettingsShowSource,
    // Where a new symbol's facts are looked up (decision 0173): the live mount's open trade leases,
    // or none for a mount with none, one per trait. Handed on to the publisher the server reads them from.
    symbol_sources: crate::server::symbol_admission::SymbolSources,
    // Where the core records its runtime mounts (`CoreConfig::state_dir`, `None` with no project), so a
    // `StrategyStatus` row can name a runtime mount's account.
    mount_topology_dir: Option<std::path::PathBuf>,
    // The late-retry memory the boot restore built (decision 0270, D7), which every control surface refuses ids from.
    boot_order_client_ids: std::sync::Arc<std::collections::HashSet<String>>,
    // What each `.rs` mount waiting for its build says now (owner 2026-11-14), for its `StrategyStatus` row.
    mount_builds: crate::config::MountBuilds,
) -> Option<PublisherHandle> {
    let ObserveDoor {
        addr,
        listener,
        keys,
        limits,
        accounts,
        control_enabled,
        datahub_advertise,
        observer_stream,
    } = door;
    let commands = control_enabled.then(|| handle.command_sink());
    // `spawn_with_mounts` (split-plane I10): the per-mount rows travel as publisher process-static
    // data, exactly like the identity block.
    //
    // ⚠ A SINGLE-MOUNT daemon now passes ONE ROW rather than none. It used to pass none and let the
    // server's `StrategyStatus` arm derive the row from the identity block, and that derivation is
    // where `MountViewWire::live` picked up `HubIdentityWire::live` — the process-wide
    // `flags.tradehub_live` gate — as its answer to a per-VENUE question. The row `main` now builds
    // carries the identity's own `strategy` and `params` strings, so the wire answer is
    // byte-identical apart from `live`, which is taken from the mount's arming record instead. The
    // server's fallback arm STAYS: it still serves an identity-only publisher (`publish::spawn`).
    //
    // The HALT sentinel's path rides the publisher too, so every frame says whether the file exists
    // (`CoreSnapshotWire::halt_sentinel`, which `vike-cli trade health` judges). It is the process's ONE
    // resolved path, memoized (`vike_boot::halt::halt_path_from_env`) and declared by this binary's
    // root before the first mount; the publisher resolves nothing itself.
    let publisher =
        publish::spawn_with_mounts(handle.snapshot_cell(), Some(identity), mounts, not_running)
            .with_symbol_sources(symbol_sources)
            .with_halt_sentinel_path(vike_boot::halt::halt_path_from_env())
            .with_mount_topology_dir(mount_topology_dir)
            .with_boot_order_client_ids(boot_order_client_ids)
            .with_mount_builds(mount_builds)
            .with_observer_stream(observer_stream);
    if observer_stream != publish::ObserverStream::default() {
        tracing::info!(
            frames_per_second = observer_stream.frames_per_second,
            compression = observer_stream.compression,
            "observer stream shaped by the settings rows (config.observer_frames_per_second,              config.observer_frame_compression)"
        );
    }
    let server_publisher = publisher.clone();
    let advertise_log = datahub_advertise.clone();
    // The row's address as the kernel bound it: a row naming port 0 learns its port only here (a
    // test's daemon does, and reads it off the line below).
    let bound =
        listener.local_addr().map_or_else(|e| format!("unreadable: {e}"), |a| a.to_string());
    std::thread::Builder::new()
        .name("th-node-accept".into())
        .spawn(move || {
            if let Err(e) = server::serve(
                listener,
                server_publisher,
                keys,
                commands,
                limits,
                Some(settings_source),
                accounts,
                datahub_advertise,
            ) {
                tracing::error!(error = %e, "node server accept loop exited");
            }
        })
        .expect("spawn node accept thread");
    tracing::info!(
        %addr,
        %bound,
        control = control_enabled,
        datahub_advertise = advertise_log.as_deref().unwrap_or("(none)"),
        "tradehub server listening (authenticated; observe always, control gated by a \
         `flags.tradehub_control` row)"
    );
    Some(publisher)
}

/// **The ONE read of the hub-key store this daemon makes, and the scope it makes it with**: the two CLIENT
/// keys it holds to reach the other daemons, 0164's daemon class (`is_tradehub_gatewayhub_trade_client_key`:
/// the gatewayhub's `Scope::Trade` key, and `is_tradehub_datahub_trade_client_key`: the datahub's strategy-lane
/// key). Both are named BY NAME, in this one function, so the hub-key store gate sees the whole scope in one
/// place; neither is verified by this daemon.
///
/// ⚠ **No person key is in this scope any more** (decision 0288 item 8): not the tradehub's observe/control
/// pair, not its admin key. The node server admits devices by the access list alone (`open_observe_door`).
///
/// A function of its own, and not an inline call, so a test can drive the production scope end to
/// end (`tradehub_cli/tests/admin_barrier.rs`): a test that spelled the predicate itself would pass
/// whatever this daemon passed. `crates/vike-ops/tests/settings_secrets/hub_key_store_gate/probe_rule.rs`
/// pins this file as the only one that may name the two client predicates' reads.
pub(crate) fn resolve_daemon_hub_key_store(
    store: &vike_secrets::CredentialStore,
) -> Result<vike_secrets::CredentialRead, vike_secrets::StoreReadError> {
    store.hub_keys(|key| {
        vike_venues::credential_keys::is_tradehub_gatewayhub_trade_client_key(key)
            || vike_venues::credential_keys::is_tradehub_datahub_trade_client_key(key)
    })
}

/// **The link this daemon, and its one-shot commands, dial the gatewayhub from** (decision 0164,
/// wave 2): the gatewayhub's `Scope::Trade` key — `VIKE_GATEWAYHUB_TRADE_KEY`, minted on the
/// gatewayhub's box by `vike-cli gatewayhub setup` — read out of the SAME hub-key resolution this
/// file already makes ([`resolve_daemon_hub_key_store`], which names the client predicate), and
/// nothing else: no observe key, no control key, no venue credential. Reading it here keeps the one
/// fenced home (`crates/vike-ops/tests/settings_secrets/hub_key_store_gate/probe_rule.rs`).
///
/// It dials nothing. A store that EXISTS and cannot be read is an ERROR (logged naming the
/// consequence), never "no key"; an absent key is a link that cannot sign, which the backend turns
/// into `gatewayhub unavailable` naming the key, once, instead of dialling unsigned. The key's
/// PRESENCE is logged, never a value.
pub(crate) fn gateway_trade_link(
    addr: String,
    store: &vike_secrets::CredentialStore,
) -> vike_gatewayhub_client::client::GatewayLink {
    let name = vike_venues::credential_keys::gatewayhub_trade_key();
    let key: Vec<u8> = match resolve_daemon_hub_key_store(store) {
        Ok(resolved) => {
            resolved.report();
            resolved
                .secrets
                .into_map()
                .get(name)
                .map(|v| v.trim())
                .filter(|v| !v.is_empty())
                .map(|v| v.as_bytes().to_vec())
                .unwrap_or_default()
        }
        Err(e) => {
            tracing::error!(
                error = %e,
                "the hub-key store is PRESENT but UNREADABLE — the gatewayhub trade key was NOT \
                 loaded. This is NOT the same as the key being absent: every call to the \
                 gatewayhub is about to be refused locally for want of a key. Fix the store's \
                 permissions and restart"
            );
            Vec::new()
        }
    };
    let link = vike_gatewayhub_client::client::GatewayLink::new(
        addr,
        HubKeys::new(Vec::new(), Vec::new()).with_trade(key),
    );
    tracing::info!("vike-tradehub: gatewayhub link: {link:?}");
    link
}

/// Can this PROCESS create a file in `dir`? The question `ls -ld` cannot answer.
///
/// Mode bits and ownership are the wrong instrument here: under `ProtectSystem=strict` the
/// directory's metadata is unchanged and the read-only-ness lives in this process's own mount
/// namespace, so the only honest probe is to try. Used by [`account_admin_source`] to refuse at
/// ARMING time rather than at the operator's first credential write.
///
/// ⚠ **It can never touch the credential store.** `create_new(true)` refuses to open an existing
/// path, so this cannot truncate or clobber anything — and the name is a fixed sentinel that no
/// store file uses. It is removed immediately; a leftover means the removal itself failed, which is
/// reported through the same `Err` rather than swallowed, because a directory that accepts a
/// creation and refuses a removal is not a directory this surface should call writable.
fn probe_writable(dir: &std::path::Path) -> std::io::Result<()> {
    let probe = dir.join(".vike-write-probe");
    std::fs::OpenOptions::new().write(true).create_new(true).open(&probe)?;
    std::fs::remove_file(&probe)
}

/// **The ONE site that decides whether this daemon can write credentials from the wire** —
/// `docs/decisions/0065-accounts-are-managed-and-the-barrier-is-declared.md`'s questions, asked
/// in order, returning `Some` only when each answers yes.
///
/// It is a free function taking every input as a PARAMETER for [`start_observe_server`]'s reason
/// verbatim: *"so this call site is the one place that decides whether a remote order-write surface
/// opens, and so that decision is visible in a diff."* One rung sharper here — what this one opens
/// is a KEY-MATERIAL surface.
///
/// # The three questions
///
/// 1. **Has the operator DECLARED a barrier?** `config.tradehub_account_admin`, three-valued
///    (unset/`off` ⇒ `None` and nothing else happens; `loopback`; `contained`). ⚠ An UNRECOGNISED
///    value is OFF and is LOGGED — a typo'd `loopbak` must never arm a credential surface, and an
///    operator who typed one must be told rather than left believing the barrier is up.
/// 2. **If `loopback`, does the BIND agree?** [`vike_hub_protocol::bind::bind_exposure`] over the
///    already-resolved addresses — the same classification [`server::bind_decision`] uses one step earlier. A
///    non-loopback bind under this value REFUSES the capability at `error`, naming the key and the
///    address. ⚠ **That refusal is UNWAIVABLE by `tradehub_allow_public_bind`**, which is the shape
///    copied from `vike_datahub_client::bind`'s `BindDecision::RefuseUnauthenticated`: the
///    public-bind flag is consent to publish an ORDER surface, and it is not consent to publish a
///    key-material one. An operator who genuinely wants the account verbs on a wide bind says
///    `contained`, which is an assertion about a barrier OUTSIDE the process rather than permission
///    for there to be none.
/// 3. ~~Is there an ADMIN KEY?~~ No longer asked (decision 0288, Q3 = A, 2026-11-16): who may use the armed
///    surface is the admin ROLE of a device on the access list, decided per connection and re-read before every
///    account verb, so no key gates the ARMING. What a settings write still cannot mint is an admin row.
///
/// # Why `contained` states rather than second-guesses
///
/// `docs/decisions/0026-containerisation-additive-backend-image.md` ruled that whether a container's
/// port is reachable is decided OUTSIDE the container, at `docker run -p`, and is invisible to the
/// process — so this daemon may not infer containment, and may not second-guess a declaration of it
/// either. What it can do, and does, is say out loud exactly what has been asserted, so the
/// assertion is auditable in a way an inferred one would not be.
pub(crate) fn account_admin_source(
    declaration: Option<&str>,
    resolved: &[std::net::SocketAddr],
    settings_dir: Option<&std::path::Path>,
    scope: vike_secrets::CredentialScope,
) -> Option<server::accounts::AccountAdminSource> {
    use server::accounts::AccountBarrier;
    use vike_hub_protocol::bind::{BindExposure, bind_exposure};

    let raw = declaration.map(str::trim).filter(|d| !d.is_empty() && *d != "off")?;
    let Some(barrier) = AccountBarrier::parse(Some(raw)) else {
        tracing::error!(
            value = raw,
            "config.tradehub_account_admin is not a value this daemon recognises, so \
             account administration is NOT armed. It must be \"loopback\" (the listener is on \
             loopback and reached through a tunnel — CHECKED against the bind) or \"contained\" \
             (the barrier is outside this process, e.g. a container port published to 127.0.0.1 — \
             asserted, never verified). An unrecognised value is treated as OFF rather than \
             refusing to start: a typo must not arm a credential surface, and a daemon that will \
             not start is worse than one trading with this one capability down"
        );
        return None;
    };

    // ⚠ THE ONE ASSERTION THIS PROCESS CAN MAKE, and it is why the key has three values rather than
    // two. `contained` reaches no branch here at all: there is nothing about it to check.
    if barrier == AccountBarrier::Loopback
        && let BindExposure::Public(exposed) = bind_exposure(resolved)
    {
        tracing::error!(
            %exposed,
            "config.tradehub_account_admin = \"loopback\" DECLARES that this node server is \
             reachable only through a tunnel, and its bind is NOT loopback — account \
             administration is NOT armed. This wire is PLAINTEXT and authenticates the connection \
             rather than each frame, so a credential value on it is readable by anyone on the \
             path. ⚠ `tradehub_allow_public_bind` does NOT waive this: that flag is consent to \
             publish an ORDER surface, not a key-material one. Either put the listener back on \
             127.0.0.1 and reach it with `ssh -L`, or — if the barrier is genuinely outside this \
             process (a container port published to 127.0.0.1, a private interface) — declare that \
             instead with `tradehub_account_admin = \"contained\"`, which this daemon cannot \
             verify and will say so on every start"
        );
        return None;
    }

    // ⚠ NO KEY IS REQUIRED TO ARM since decision 0288 (Q3 = A, the owner's 2026-11-16): the AUTHORIZATION,
    // 0065's barrier 2, is the admin ROLE of a device on this box's access list, admitted at connect
    // (`vike_hub_protocol::access::verify_person` for `Scope::Account`) and re-read before every account verb.
    // A Control peer that wrote this declaration into a config row still cannot authenticate for the
    // scope: it holds no admin row. No shared admin key is read at all (decision 0288 item 8). Barriers 1 (this
    // declaration) and 3 (the bind, above) stay.

    // The SETTINGS DIRECTORY, from the BOOT's own answer — never a second walk. A daemon whose boot
    // resolved no project has no store to administer, and resolving one here would be the
    // `_from`-less resolver's failure wearing a new surface.
    let Some(settings_dir) = settings_dir else {
        tracing::error!(
            "config.tradehub_account_admin declares a barrier, but this daemon resolved NO \
             settings directory at boot (no project above its working directory) — there is no \
             store to administer, so account administration is NOT armed. Set $VIKE_SETTINGS_DIR, \
             or run the daemon from its project root"
        );
        return None;
    };

    // ⚠ **THE SANDBOX — the wall that made every write verb on this surface fail at the FIRST
    // write with a bare SQLite string, on the shipped unit, measured 2026-09-17.**
    //
    // `deploy/vike-tradehub.service` runs `ProtectSystem=strict` with ONE grant,
    // `ReadWritePaths=<project>/settings/state`. The settings database is
    // `<project>/settings/db/vike.db` (`vike_secrets::db_path_in`) — OUTSIDE it. So inside
    // the daemon's own mount namespace the credential store is READ-ONLY, while `ls -ld` from a
    // shell shows the directory writable: the fact is only visible from inside. Reads are
    // unaffected, which is why arming looked fine and only a write discovered it.
    //
    // 0065 §3c itself leans on that read-only-ness as a SAFETY argument for the settings surface.
    // It is the same fact, and for THIS surface it is a wall rather than a guarantee — so it is
    // probed here, at arming time, in front of the operator who just declared the barrier, instead
    // of surfacing as an unactionable error on their first `SetCredential`.
    //
    // ⚠ **The shipped unit now GRANTS it** — `deploy/vike-tradehub.service` carries a second
    // `ReadWritePaths=` naming `<settings>/db`, on the owner's ruling of 2026-09-18 that the daemon
    // has to be able to write its own database. So on a CURRENT install this probe passes and
    // nothing below fires. It stays because the probe is cheap, because an install predating that
    // ruling still has one grant, and because the failure it catches is otherwise invisible: the
    // read-only-ness lives in this process's mount namespace, so `ls -ld` from a shell shows a
    // writable directory and every message that sends an operator there sends them nowhere.
    //
    // The grant is the sub-directory and not `settings/`: granting the root would hand this daemon
    // write access to the whole settings directory rather than the one database it has to write.
    // The unit's own block argues the rest — including that the database this grant names holds
    // the `policy` ceiling rows too, which a filesystem grant cannot guard — and names the guard
    // that is owed. Read it there; it is not restated here.
    let db = vike_secrets::db_path_in(settings_dir);
    if let Some(db_dir) = db.parent()
        && db_dir.is_dir()
        && let Err(e) = probe_writable(db_dir)
    {
        tracing::error!(
            error = %e,
            dir = %db_dir.display(),
            "config.tradehub_account_admin declares a barrier, but \
             this daemon CANNOT WRITE the settings database's directory — account administration \
             is NOT armed, because every verb on that surface would fail at its first write. On \
             the shipped unit this is the sandbox, not the filesystem: ProtectSystem=strict grants \
             `settings/state` alone, so `settings/db` is read-only INSIDE this process even though \
             it looks writable from a shell. The cure is the drop-in shipped beside the unit — \
             `deploy/vike-tradehub-account-admin.conf`, which grants `settings/db` and nothing \
             else — installed with `systemctl edit vike-tradehub` and a restart. Do NOT widen the \
             grant to `settings/`: that hands this daemon write access to the settings database \
             that caps it"
        );
        return None;
    }

    match barrier {
        AccountBarrier::Loopback => tracing::warn!(
            "ACCOUNT ADMINISTRATION ARMED (config.tradehub_account_admin = \"loopback\") — \
             an ADMIN-authenticated peer may add, rename, deactivate and REMOVE accounts on this \
             box, and may WRITE CREDENTIAL VALUES into its store. The bind was checked and is \
             loopback; this wire is plaintext, so its confidentiality is the tunnel's"
        ),
        // ⚠ Says exactly what has been ASSERTED rather than what has been checked, because nothing
        // here was checked. 0026's refusal to infer containment is what makes this the honest line
        // and a `/.dockerenv` probe the dishonest one.
        AccountBarrier::Contained => tracing::warn!(
            "ACCOUNT ADMINISTRATION ARMED (config.tradehub_account_admin = \"contained\") — \
             an ADMIN-authenticated peer may add, rename, deactivate and REMOVE accounts on this \
             box, and may WRITE CREDENTIAL VALUES into its store. ⚠ THE BARRIER IS ASSERTED, NOT \
             VERIFIED: this daemon has NOT checked its bind, and cannot see what is in front of \
             it. You have declared that something outside this process makes this listener \
             unreachable. If that is not true, every credential written over this wire crosses it \
             in PLAINTEXT"
        ),
    }
    Some(server::accounts::AccountAdminSource {
        settings_dir: settings_dir.to_path_buf(),
        scope,
        barrier,
    })
}

/// **The access list this hub admits devices by** (decision 0288): one `access_key` row read by key id, afresh at
/// every call (`vike_secrets::access::find_access_key`), so a revoke or a role change needs no restart. A store that
/// exists and cannot be read admits nobody and says so at `error` ([`AccessList::from_store`]'s policy).
pub(crate) fn access_list(settings_dir: std::path::PathBuf) -> AccessList {
    let held_dir = settings_dir.clone();
    AccessList::from_store(
        move |key_id| {
            vike_secrets::access::find_access_key(&settings_dir, key_id).map(|row| {
                row.map(|r| AccessEntry {
                    person: r.person,
                    device: r.device,
                    role: r.role,
                    public_key: r.public_key,
                    revoked: r.revoked_ms.is_some(),
                })
            })
        },
        move || vike_secrets::access::access_list_held(&held_dir),
        |e| tracing::error!(error = %e, "vike-tradehub: the access list is UNREADABLE — it admits nobody"),
    )
}

#[path = "hub_server_tests.rs"]
#[cfg(test)]
mod hub_server_tests;
