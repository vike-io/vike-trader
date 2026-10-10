//! The node server's STARTUP — everything [`crate::tradehub_cli::run`] decides about whether this
//! daemon opens an authenticated TCP surface at all, and which capabilities that surface carries:
//! the bind classification, the node-key store read, the control gate that zeroes the control key,
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
//! It reads two things back from `crate::tradehub_cli`, which owns them: the process-wide
//! environment sweep (`process_env`) and the resolved control limits (`resolve_control_limits`).

use std::collections::HashMap;
use std::net::{TcpListener, ToSocketAddrs};

use vike_core::CoreHandle;
use vike_tradehub_client::{NodeKeys, proto::Scope};

use crate::publish::{self, PublisherHandle};
use crate::server;
use crate::tradehub_cli::process_env;
use crate::tradehub_cli::settings::resolve_control_limits;

/// Start the authenticated node server (observe always; order-control gated) + snapshot publisher IFF
/// an ADDRESS was configured. Returns the [`PublisherHandle`] so the bounded teardown can stop it.
///
/// Both gates arrive as PARAMETERS, resolved once by `crate::tradehub_cli`'s `resolve_settings` at
/// the top of [`crate::tradehub_cli::run`]:
/// `addr` is `config.tradehub_addr` (still overridden by `VIKE_TRADEHUB_ADDR`) and
/// `control_enabled` is `flags.tradehub_control` (still overridden by
/// `VIKE_TRADEHUB_CONTROL`). They used to be `std::env::var` reads RIGHT HERE, which is why the file
/// layer did nothing at all: a validated `tradehub_addr = "127.0.0.1:7979"` in `config.toml` left
/// nothing listening and printed no line saying so, while `vike-cli config show` reported the file as
/// its origin. Taking them as arguments also puts the decision to open a remote order-write surface
/// at ONE call site, where a diff can see it.
///
/// - `addr = None` (or blank) ⇒ `None`: the daemon is byte-identical to the pure-stdio PR-9 daemon.
/// - Addr set but no `VIKE_TRADEHUB_OBSERVE_KEY` in the credential store ⇒ logged + `None` (the
///   absent-credential-is-the-gate convention: no key, no server), and the daemon keeps trading
///   headless. The keys are read HERE — the daemon binary owns that I/O (the credential store
///   through `crate::tradehub_cli`'s `workspace_credentials`, the node-key store through
///   `vike_secrets::resolve_node_keys`); the light `vike-tradehub-client` crate must not.
/// - A bind failure is likewise logged and non-fatal (the daemon keeps trading).
/// - A NON-LOOPBACK address ⇒ logged + `None` unless `allow_public_bind` — see below.
///
/// ## ⚠ The non-loopback refusal (`allow_public_bind` = `flags.tradehub_allow_public_bind`)
///
/// `crate::server`'s handshake is PLAINTEXT and authenticates the CONNECTION, not each
/// frame — so on a reachable network it hands out an offline cracking target for the node key and,
/// after `AuthOk`, an on-path attacker can inject a `Command`. The design answer is a loopback
/// listener plus an SSH tunnel (`ssh -L 7879:localhost:7879 the CI box`), the same way `vike-datahub` is
/// reached; `server::DEFAULT_ADDR` is loopback for that reason. Nothing ENFORCED it: `check_addr`
/// only requires a `:`, so `tradehub_addr = "0.0.0.0:7879"` — which is simply what one types for a
/// server — published an order-write surface with no warning.
///
/// So a non-loopback bind now needs a SECOND, differently-named opt-in
/// (`flags.tradehub_allow_public_bind`, or `VIKE_TRADEHUB_ALLOW_PUBLIC_BIND=1`). It is a flag
/// rather than a refusal without an escape hatch because reaching a node from a trusted LAN or
/// from inside a WireGuard/VPN interface is legitimate, and a guard people cannot turn off is a
/// guard they route around. It is a SEPARATE flag rather than an inference from the address
/// because the mistake this catches is *typing an address*, and no address can be its own consent.
/// ON, the bind proceeds behind a `warn!` naming what is now exposed — never silently.
///
/// The publisher reads ONLY the arc-swap snapshot cell (`CoreHandle::snapshot_cell`), never the core
/// fold, and the accept loop runs on a detached thread. ORDER CONTROL is double-gated: `Scope::Write`
/// is offered only when `control_enabled` — off, the control key is zeroed AND no `CommandSink` is
/// passed to `serve`, so a `Control` auth / `Command` is refused two ways.
pub(crate) fn start_observe_server(
    handle: &CoreHandle,
    identity: vike_tradehub_client::wire::WireNodeIdentity,
    mounts: Vec<vike_tradehub_client::wire::WireMountRow>,
    addr: Option<&str>,
    control_enabled: bool,
    allow_public_bind: bool,
    // The `SettingsShow`/`SetSetting` source (REQ-7): the boot-resolved settings directory, this
    // binary's ONE startup env sweep and the hot-apply seam, built by the CALLER — it owns all
    // three facts (the settings-registry rule), and this function only hands the handle on.
    settings_source: server::settings::SettingsShowSource,
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
) -> Option<PublisherHandle> {
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
                 row, or VIKE_TRADEHUB_ALLOW_PUBLIC_BIND=1) — the node \
                 handshake is PLAINTEXT and authenticates the connection, not each frame, so \
                 anyone who can reach this address can collect the nonce+mac for offline cracking \
                 and, once a session is up, inject frames. Put a tunnel or a VPN in front of it"
            );
        }
        server::BindDecision::Refuse(exposed) => {
            tracing::error!(
                %addr, %exposed,
                "node-server address is NOT loopback — observe server NOT started. This surface's \
                 handshake is plaintext and (with control on) places REAL orders, so it is meant \
                 to be reached over an SSH tunnel: keep `tradehub_addr` on 127.0.0.1 and run \
                 `ssh -L 7879:localhost:7879 <host>`. If this host genuinely must listen on a \
                 trusted network, `vike-cli config set flags.tradehub_allow_public_bind true` (or \
                 VIKE_TRADEHUB_ALLOW_PUBLIC_BIND=1). The daemon keeps trading headless"
            );
            return None;
        }
    }
    // `auth::from_vars`, not `NodeKeys::from_vars`: the TYPE moved down to
    // `vike_node_proto::auth` when 0025 gave the datahub the same handshake, so the
    // tradehub-NAMED constructor is a free function in this service's binding module. Same two key
    // names, same trimming, same credential-is-the-gate `None`.
    // ⚠ THE NODE STORE, not `workspace_credentials()`. This daemon legitimately reads BOTH tables —
    // it trades, so it needs the venue grid, and it serves, so it needs a node pair — but they are
    // different namespaces for different reasons and it must not find one in the other: the
    // `node_key` table of the settings database, and nothing else.
    // The override comes out of the ONE sweep this binary owns, exactly as `workspace_credentials`
    // takes it — never a second `std::env::var`, which would be a `Layer::Library` row on a
    // may-only-shrink work-list and could answer differently from the boot.
    // ⚠ The scope is THIS DAEMON'S OWN, not all five platform names: `resolve_node_keys` hands back
    // the names the predicate admits and no others, so this daemon never holds the datahub's pair
    // (see `resolve_daemon_node_store` for the scope, and why it includes the admin key).
    let settings_override = process_env().get(vike_secrets::SETTINGS_DIR_ENV).map(String::as_str);
    let node_store = match resolve_daemon_node_store(settings_override) {
        Ok(resolved) => resolved,
        Err(e) => {
            tracing::error!(
                error = %e,
                %addr,
                "the node-key store is PRESENT but UNREADABLE — observe server NOT started. This is \
                 NOT the absent-credential gate: an unreadable store and a missing one must never \
                 look the same to an operator. Fix its permissions and restart; the daemon keeps \
                 trading headless"
            );
            return None;
        }
    };
    // ⚠ The node store as a MAP, kept rather than consumed: the admin-key probe below reads the
    // same map, and a second `into_map` would be a second read of the same store with no guarantee
    // the two saw one state.
    let node_vars = node_store.secrets.into_map();
    let keys = match vike_tradehub_client::auth::from_vars(&node_vars) {
        Some(k) if k.has(Scope::Read) => k,
        _ => {
            tracing::error!(
                %addr,
                "a node-server address is configured but there is no VIKE_TRADEHUB_OBSERVE_KEY in \
                 the node-key store — observe server NOT started (absent credential is the gate); \
                 daemon keeps trading headless"
            );
            return None;
        }
    };
    // Belt-and-suspenders: when control is OFF, ZERO the control key so it is never even loaded into
    // the server — a `Control` auth then cannot verify regardless of what the `.env` held.
    let keys = if control_enabled {
        keys
    } else {
        NodeKeys::new(keys.key_for(Scope::Read).to_vec(), Vec::new())
    };
    // The command sink is WITHHELD unless control is enabled — the second, independent gate, so even
    // a mis-set control key can never reach the core without the master flag.
    let commands = control_enabled.then(|| handle.command_sink());
    // ⚠ **THE ACCOUNT-ADMIN CAPABILITY, decided HERE and nowhere else.** `None` — every box that
    // has not declared the barrier — means the server holds no account writer at all, advertises
    // no `account-verbs` capability, and is byte-identical to a binary without the verb. The three
    // questions it asks, and why the process cannot ask the confidentiality one for itself, are on
    // `account_admin_source`.
    //
    // ⚠ It reads the SAME `resolved` addresses `bind_decision` classified above, rather than
    // resolving the address a second time: two resolutions of one hostname can answer differently
    // (a DNS change between them), and the one thing this capability may not do is arm against a
    // bind that is not the one that happened.
    let accounts = account_admin_source(account_admin, &resolved, &node_vars, settings_dir);
    // ⚠ **THE ADMIN KEY IS ATTACHED ONLY WHEN THE CAPABILITY IS ARMED**, which is the key-ZEROING
    // gate above wearing the other polarity — and it is the stronger half of the same idea. The
    // control gate ZEROES a key it loaded; this one never loads one at all unless
    // `account_admin_source` said yes, so an `Admin` auth against an undeclared box cannot verify
    // REGARDLESS of what the node-key store holds. `from_vars` two lines up reads two names and
    // leaves `Scope::Account` empty, which is what makes that the default rather than a check.
    //
    // ⚠ **EXTEND the value the control gate already produced — never RE-READ the map.** This line
    // spelled `from_vars_with_admin(&node_vars)`, which is `from_vars` + `with_admin`, and
    // `from_vars` re-reads BOTH `OBSERVE_KEY_ENV` and `CONTROL_KEY_ENV`. That DISCARDED the zeroing
    // above rather than building on it, so arming the account capability on a control-DISABLED box
    // handed the process back the very control key that gate had emptied — and `run_handshake`'s
    // `scope == Scope::Write && !keys.has(Scope::Write)` refusal then admitted a Control peer to
    // `Request::Preview`, whose arm checks the scope and has NO sink gate. Measured 2026-09-17 by a
    // probe replaying both steps verbatim. `the_ordinary_node_key_read_leaves_the_admin_scope_absent`
    // cannot see it: it asserts only that `Scope::Account` is absent, never that Control stays absent.
    let keys = keys_for_account_capability(keys, accounts.is_some(), &node_vars);
    if control_enabled {
        tracing::warn!(
            "order-control channel ENABLED (a `flags.tradehub_control` row, or \
             VIKE_TRADEHUB_CONTROL=1) — a Control-authenticated peer may place/cancel REAL orders"
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
    // `spawn_with_mounts` (split-plane I10): the per-mount rows travel as publisher process-static
    // data, exactly like the identity block.
    //
    // ⚠ A SINGLE-MOUNT daemon now passes ONE ROW rather than none. It used to pass none and let the
    // server's `StrategyStatus` arm derive the row from the identity block, and that derivation is
    // where `WireMountRow::live` picked up `WireNodeIdentity::live` — the process-wide
    // `flags.tradehub_live` gate — as its answer to a per-VENUE question. The row `main` now builds
    // carries the identity's own `strategy` and `params` strings, so the wire answer is
    // byte-identical apart from `live`, which is taken from the mount's arming record instead. The
    // server's fallback arm STAYS: it still serves an identity-only publisher (`publish::spawn`).
    let publisher = publish::spawn_with_mounts(handle.snapshot_cell(), Some(identity), mounts);
    let server_publisher = publisher.clone();
    // The REQ-2 datahub advertisement (`config.datahub_advertise_addr`, or
    // VIKE_DATAHUB_ADVERTISE_ADDR): set ⇒ every Welcome carries `datahub=<addr>` and a client
    // with no explicit `datahub_addr` of its own dials the datahub there. Blank-trimmed like
    // `addr` above; unset advertises nothing (the pre-REQ-2 Welcome). ⚠ The REQ-7 settings source
    // is NOT built here any more — the caller owns it (see this function's parameter), which is
    // why only the advertisement is normalized at this line.
    let datahub_advertise =
        datahub_advertise.map(str::trim).filter(|a| !a.is_empty()).map(str::to_string);
    let advertise_log = datahub_advertise.clone();
    std::thread::Builder::new()
        .name("vt-tradehub-node-accept".into())
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
        control = control_enabled,
        datahub_advertise = advertise_log.as_deref().unwrap_or("(none)"),
        "tradehub node server listening (authenticated; observe always, control gated by a \
         `flags.tradehub_control` row / VIKE_TRADEHUB_CONTROL)"
    );
    Some(publisher)
}

/// **The ONE read of the node-key store this daemon makes, and the scope it makes it with** —
/// `vike_model::credential_keys::is_tradehub_daemon_key`: the observe/control pair AND the admin
/// key, never the datahub's pair.
///
/// ⚠ **The admin key has to be in this map.** [`account_admin_source`] and
/// [`keys_for_account_capability`] read `VIKE_TRADEHUB_ADMIN_KEY` out of the very map this resolves,
/// and `vike_secrets::resolve_node_keys` returns only the names its predicate admits. Handed the
/// pair-only `is_tradehub_node_key` (the CLI's and the desktop's scope, which must not carry the
/// key that writes key material), this daemon never saw the admin key and
/// `config.tradehub_account_admin` could never arm — silently, as "no key in the store".
///
/// A function of its own, and not an inline call, so a test can drive the production scope end to
/// end (`tradehub_cli/tests/admin_barrier.rs`): a test that spelled the predicate itself would pass
/// whatever this daemon passed. `crates/vike-ops/tests/settings_secrets/node_key_store_gate/probe_rule.rs`
/// pins this file as the only one that may name the daemon predicate.
pub(crate) fn resolve_daemon_node_store(
    settings_override: Option<&str>,
) -> Result<vike_secrets::Resolved, vike_secrets::SecretsError> {
    vike_secrets::resolve_node_keys(
        settings_override,
        vike_model::credential_keys::is_tradehub_daemon_key,
    )
}

/// **The ONE site that decides whether this daemon can write credentials from the wire** —
/// `docs/decisions/0065-accounts-are-managed-and-the-barrier-is-declared.md`'s three parts, asked
/// in order, returning `Some` only when all three answer yes.
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
/// 2. **If `loopback`, does the BIND agree?** [`server::bind_exposure`] over the already-resolved
///    addresses — the same classification [`server::bind_decision`] uses one step earlier. A
///    non-loopback bind under this value REFUSES the capability at `error`, naming the key and the
///    address. ⚠ **That refusal is UNWAIVABLE by `tradehub_allow_public_bind`**, which is the shape
///    copied from `vike_datahub_client::bind`'s `BindDecision::RefuseUnauthenticated`: the
///    public-bind flag is consent to publish an ORDER surface, and it is not consent to publish a
///    key-material one. An operator who genuinely wants the account verbs on a wide bind says
///    `contained`, which is an assertion about a barrier OUTSIDE the process rather than permission
///    for there to be none.
/// 3. **Is there an ADMIN KEY?** `VIKE_TRADEHUB_ADMIN_KEY` in the node-key store — the
///    credential-is-the-gate idiom, and the one thing a settings write cannot mint. A declaration
///    with no key arms nothing and says so.
///
/// # Why `contained` states rather than second-guesses
///
/// `docs/decisions/0026-containerisation-additive-backend-image.md` ruled that whether a container's
/// port is reachable is decided OUTSIDE the container, at `docker run -p`, and is invisible to the
/// process — so this daemon may not infer containment, and may not second-guess a declaration of it
/// either. What it can do, and does, is say out loud exactly what has been asserted, so the
/// assertion is auditable in a way an inferred one would not be.
/// **Attach the admin key when — and only when — the account capability is armed, EXTENDING the
/// keys the control gate already decided rather than re-reading the store.**
///
/// A free function so the composition can be DRIVEN by a test. `start_observe_server` binds a
/// socket, so the two steps this joins (the control gate's zeroing, then this attachment) had no
/// reachable seam between them, and a test that merely replayed them would pass no matter what the
/// daemon did — an assertion that cannot fail for its stated reason.
///
/// ⚠ **The bug this shape exists to prevent.** This was `from_vars_with_admin(&node_vars)`, which
/// is `from_vars` + `with_admin`, and `from_vars` re-reads BOTH `OBSERVE_KEY_ENV` and
/// `CONTROL_KEY_ENV` out of the same map. On a box with `flags.tradehub_control` OFF the caller has
/// already replaced `keys` with a control-EMPTY pair; re-reading DISCARDED that and handed the
/// process back the live control key, so `run_handshake`'s
/// `scope == Scope::Write && !keys.has(Scope::Write)` refusal stopped refusing and a Control
/// peer reached `Request::Preview` — whose arm checks the scope and has no sink gate behind it.
/// Measured 2026-09-17.
///
/// `keys` is therefore consumed and returned: the only way to obtain the result is to hand over the
/// value the gate produced, so a future edit cannot quietly source a fresh one.
pub(crate) fn keys_for_account_capability(
    keys: vike_tradehub_client::NodeKeys,
    capability_armed: bool,
    node_vars: &HashMap<String, String>,
) -> vike_tradehub_client::NodeKeys {
    if !capability_armed {
        return keys;
    }
    keys.with_admin(
        node_vars
            .get(vike_tradehub_client::auth::ADMIN_KEY_ENV)
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
            .map(|v| v.as_bytes().to_vec())
            .unwrap_or_default(),
    )
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

pub(crate) fn account_admin_source(
    declaration: Option<&str>,
    resolved: &[std::net::SocketAddr],
    node_store: &HashMap<String, String>,
    settings_dir: Option<&std::path::Path>,
) -> Option<server::accounts::AccountAdminSource> {
    use server::BindExposure;
    use server::accounts::AccountBarrier;

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
        && let BindExposure::Public(exposed) = server::bind_exposure(resolved)
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

    // The KEY. Credential-is-the-gate, and the ONE thing a settings write cannot mint: a Control
    // peer that wrote this declaration into a config row still could not authenticate for the scope.
    //
    // ⚠ **The key is minted by `vike-cli backend admin-key`**, which writes
    // `PLATFORM_KEYS[4]` (`vike_model::credential_keys`) into the node-key store on its own: it
    // touches no other name, so the observe/control pair `vike-cli backend setup` mints is never
    // rotated to get it. `vike-cli secrets set` refuses the name and routes the operator there.
    // ⚠ **`node_store` must hold it, which is `resolve_daemon_node_store`'s scope** — an admin key
    // present in the database is invisible here if the resolution's predicate does not admit it.
    let keys = vike_tradehub_client::auth::from_vars_with_admin(node_store);
    if !keys.as_ref().is_some_and(|k| k.has(Scope::Account)) {
        tracing::error!(
            barrier = barrier.as_str(),
            "config.tradehub_account_admin declares a barrier, but there is no {} in this \
             box's node-key store — account administration is NOT armed, and an absent credential \
             is the gate. Mint it with `vike-cli backend admin-key` (it writes this one name and \
             rotates nothing else; `vike-cli secrets set` refuses it) and restart. Until it is \
             there, this daemon stays exactly as it is — the capability absent, which is the \
             safe direction",
            vike_tradehub_client::auth::ADMIN_KEY_ENV
        );
        return None;
    }

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
            "config.tradehub_account_admin declares a barrier and the key is present, but \
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
    Some(server::accounts::AccountAdminSource { settings_dir: settings_dir.to_path_buf(), barrier })
}
