//! The one settings load, the credential-store cells and the control-limit resolution.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::server;

use super::{PROCESS_ENV, process_env};

/// The deployment's per-order notional CEILING — `policy.max_notional_per_order` — resolved
/// exactly once by [`resolve_settings`] at the top of [`main`].
///
/// A process-wide `OnceLock` rather than a threaded parameter because it is a property of the
/// MACHINE, not of a server or a channel, and both control surfaces must see the identical value.
/// It replaces an `env::var` that was re-read on every call to [`resolve_control_limits`], so this
/// is less global state than before, not more.
static POLICY_MAX_NOTIONAL: std::sync::OnceLock<Option<f64>> = std::sync::OnceLock::new();

/// **`<project>/settings/state` as the ONE boot walk resolved it** — `vike_boot::Booted::state_dir`,
/// stored by [`resolve_settings`] so [`state_dir`] never has to walk for it again.
///
/// It is a `OnceLock` for the same reason [`POLICY_MAX_NOTIONAL`] is: the state root is a property
/// of the PROCESS, and its four consumers ([`log_dir`], [`alerts_path`], [`telegram_ledger_paths`]
/// and — through them — everything they write) sit in four unrelated places, none of which is
/// reachable from `main`'s locals.
///
/// ⚠ **`None` inside the cell is a legitimate answer** (no project above the working directory);
/// an UNSET cell means [`resolve_settings`] has not run, which cannot happen after `main`'s first
/// statement and which [`state_dir`] treats as "no state root" rather than silently walking — a
/// fallback walk is the exact defect this static removes.
pub static SETTINGS_STATE_DIR: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();

/// The credential store, read ONCE per process — **the map AND the verdict about the store it came
/// from**. See [`workspace_credentials`] for why once, and [`workspace_credentials_checked`] for
/// why both.
pub(super) static CREDENTIALS: std::sync::OnceLock<(
    HashMap<String, String>,
    vike_bridge_core::credentials::StoreHealth,
)> = std::sync::OnceLock::new();

/// **The credential store: the settings database (`<project>/settings/secrets.env` only on a box
/// that has not migrated).**
///
/// The one place this daemon asks "what credentials do I have" — the live mount, the node server's
/// auth keys, the alert webhooks, and (under its feature) the Telegram control channel all come
/// through here.
///
/// An unreadable store returns an EMPTY map with a `tracing::error!` — every venue stays paper and
/// the daemon fails loudly at the venue, instead of signing orders with whichever credentials
/// happened to load. `crates/vike-bridge-core/tests/credential_chain_roots.rs` gates it.
///
/// ⚠ **The read is memoized, and that is a correctness property, not a micro-optimization.** Four
/// call sites reach this function and up to four of them run on one start; each used to perform a
/// COMPLETE, independent load — `resolve_project` → `read_to_string` → `parse_dotenv` — of a
/// plaintext file holding live venue API secrets. Two consequences, both observed on a clean
/// install:
///
/// 1. The store's PERMISSION finding is emitted by `try_load_workspace_secrets_at` on every
///    invocation (deliberately: "no caller of either wrapper can forget to surface it"). So a 0644
///    store logged the identical `WARN … readable beyond its owner …` line TWICE per start —
///    measured, on a paper daemon with a node address and the Telegram channel up. A warning that
///    repeats reads as two findings, and the operator goes looking for the second file.
/// 2. Every extra call re-read the secrets off disk and materialised a second copy of every venue
///    key in the process. Reading a credential file once is simply the smaller surface.
///
/// The two static `OnceLock`s answer different questions and neither subsumes the other:
/// [`PROCESS_ENV`] caches the `std::env::vars()` SWEEP (which is what names `VIKE_SETTINGS_DIR`, so
/// it must be the real process env), while this one caches the resulting STORE READ. Only the first
/// existed; that is why the sweep happened once and the file read happened four times.
///
/// The clone is per call and deliberate: `NodeConfig` owns its `vars`, and the Telegram channel
/// takes this function as a `fn() -> HashMap<..>` pointer, so an owned map is the shape every caller
/// already wants. What is saved is the I/O and the log line, not the allocation.
pub(super) fn workspace_credentials() -> HashMap<String, String> {
    workspace_credentials_checked().0.clone()
}

/// [`workspace_credentials`], plus **whether the store it came from could be OPENED** — the one
/// fact the infallible loader deliberately swallows.
///
/// ⚠ **ONE read, two answers**, never two reads: the map and the verdict describe the same open of
/// the same store, and both are memoized together in [`CREDENTIALS`] so every later caller sees the
/// pair that actually happened.
///
/// # Why this daemon needs the verdict at all
///
/// `load_workspace_secrets_from_env` is documented INFALLIBLE, and the empty map it returns for an
/// unreadable store is byte-identical to the map an UNCONFIGURED box produces. Downstream, an empty
/// credential map is not an error — it IS the live gate, so every venue drops to paper and nothing
/// fails. On a daemon that is the whole defect: `Restart=on-failure` never fires, `OnFailure=` never
/// pages, and the ready banner reads `LIVE (venue=none)`, which
/// `deploy/vike-tradehub.service` documents as a legitimate answer ("gate on, nothing
/// armed"). A box that cannot read its keys and a box that has none become the same line of output.
/// The root `CLAUDE.md` forbids exactly that by name: *"those two must never look the same to an
/// operator, because a permissions bug wearing the 'not configured' answer looks exactly like a
/// correct fresh install while every venue drops to paper for a different reason."*
///
/// `vike-desktop`'s own `workspace_credentials_checked` is the precedent, for the same reason in a
/// different surface: a UI folding the empty map into `0 set` states a measured number it never
/// measured. This daemon's count is the BANNER — see [`ready_mode_line`].
///
/// ⚠ The verdict is NOT a refusal, and [`credential_store_health`]'s doc argues why against
/// `docs/decisions/0013-degrade-vs-refuse.md`.
fn workspace_credentials_checked()
-> &'static (HashMap<String, String>, vike_bridge_core::credentials::StoreHealth) {
    CREDENTIALS.get_or_init(|| {
        vike_bridge_core::credentials::load_workspace_secrets_from_env_checked(process_env())
    })
}

/// **Whether the credential store this daemon read could be OPENED at all**, from the ONE read
/// [`workspace_credentials_checked`] memoized.
///
/// # The disposition, argued against `docs/decisions/0013-degrade-vs-refuse.md`
///
/// An unreadable store **does not stop this daemon**, and the record's own four questions are why:
///
/// 1. *Protection or capability?* 0013 names "a set of credentials" a CAPABILITY in as many words,
///    and its table already files this exact case — *credential store unreadable → `error!`, empty
///    map, all paper* — as a CONFORMING degrade.
/// 2. *Did the operator ask for it?* Nothing is set-but-unhonoured. A dead writer's rollback
///    journal is an accident of a crash, not a configuration anybody wrote.
/// 3. *Does the degrade reduce authority, or redirect it?* It reduces it to the floor: with no
///    credentials, `vike_mount::make_engine` builds a paper client for every venue, opens no socket
///    and signs nothing. This is the strongest possible reduction, not a redirection.
/// 4. *Would the failure be visible where the operator already looks?* ⚠ **THIS is the question
///    that was failing, and it is the only thing fixed here.** The operator's stated authority on
///    paper-vs-live is the ready banner (`docs/ops/tradehub-the CI box.md` and
///    `deploy/vike-tradehub.service` both say so), and it said `LIVE (venue=none)`.
///
/// So the defect was never the degrade — it was a FAULT wearing the capability's clothes. The cure
/// is to take the clothes off, not to convert the degrade into a refusal.
///
/// ⚠ **Refusing was considered and is WORSE, and 0013's own *What would reopen this* names the
/// shape:** *"an unattended-daemon deployment where a startup refusal is worse than the
/// misconfiguration it prevents — a node that will not start cannot flatten a position either."*
/// This is that deployment. A refusal here is not a one-off stop but a RESTART LOOP: the daemon
/// opens the store read-only, and a read-only open may not replay a journal, so **nothing inside
/// the unit can replay it** — every restart meets the identical state.
/// `Restart=on-failure` would then cycle the process for as long as the operator is asleep, and
/// each cycle tears down a daemon that may be holding resting orders and positions, re-running the
/// teardown's cancel sweep with NO credentials to cancel them WITH. An all-paper daemon that
/// announces itself as broken keeps its control surface reachable; a crash-looping one answers
/// nothing and can neither report nor flatten.
pub(super) fn credential_store_health() -> &'static vike_bridge_core::credentials::StoreHealth {
    &workspace_credentials_checked().1
}

/// Startup step 0 — before the LOG SUBSCRIBER, the profile, or any mount: refuse a stale
/// environment, then load the whole of `<project>/settings/`.
///
/// 1. **Refuse.** `VIKE_TRADEHUB_MAX_ORDER_NOTIONAL` is no longer read (Phase 5 of the
///    settings-unification design — `docs/superpowers/specs/2026-08-04-settings-unification-design.md`).
///    An operator who set it in a unit file believes a ceiling is armed on this node; a build that
///    quietly ignored it would accept remote orders of ANY size while they believed otherwise.
///    That is strictly worse than either keeping the variable or refusing to boot, so the daemon
///    refuses, and `vike_config::refuse_removed_env` names the file and key that replace it.
/// 2. **Resolve.** Load the settings directory. A missing file is NOT an error — it is the
///    permissive default, byte-identical to a pre-Phase-5 daemon with nothing set. A file that
///    exists and is broken IS an error: a deployment that wrote a ceiling and typo'd the key must
///    not silently run without one.
///
/// The BINARY owns the environment read, per the settings-registry rule; `vike_config` never
/// touches `std::env` itself.
///
/// **RETURNS the whole [`vike_config::Settings`], not just the policy**, and that is the shape the
/// wiring needed: this daemon reads `config.log_dir`, `config.tradehub_addr`,
/// `config.datahub_advertise_addr`,
/// `preferences.log_level`, `preferences.log_file_level` and five `flags.*` fields out of it, on top
/// of the policy the live mount projects onto `vike_mount::MountPolicy`. **Exactly one load DECIDES
/// what this daemon does**, and every consumer above reads that one value.
///
/// ⚠ It is no longer the only `vike_config::load` in the process, and the distinction is the whole
/// point of the sentence. `vike_config::boot_lines` re-loads through `vike_config::provenance`,
/// because a row's ORIGIN cannot be recovered from a merged `Settings` — that read is a DISCLOSURE
/// and its result reaches nothing but the log. What must never appear is a second load whose value
/// is CONSUMED: two answers to "what is the ceiling" is the class of bug the ceilings are
/// file-only to avoid. The disclosure read is safe because it is given this function's own
/// `settings_dir` and this binary's own [`PROCESS_ENV`] sweep, so it can only differ by re-reading
/// the same files microseconds later — and a file edited inside that window is the one case where
/// the log showing the NEWER value is the useful answer.
///
/// **…and the settings DIRECTORY is returned beside the settings**, for that reason: the startup
/// disclosure in [`main`] describes the directory that was actually loaded from rather than walking
/// for one of its own. Two walks that could answer with two different projects is precisely the
/// failure the disclosure exists to surface (the CI box: an unrelated directory, no policy, no
/// credentials, every venue silently paper), so the disclosure must not be able to reproduce it.
///
/// ⚠ **It logs NOTHING, on purpose.** It runs before `vike_log::init` (the log destination and both
/// levels are among the settings it resolves), so a `tracing` call here would go to no subscriber at
/// all. Everything it would have said is emitted by [`main`] the moment the subscriber exists —
/// `settings_warning_lines` for the loader's own resolutions, and `vike_config::boot_lines` for what
/// was resolved and from where.
pub(super) fn resolve_settings(
    env: &HashMap<String, String>,
    cwd: Option<&Path>,
) -> Result<vike_boot::Booted, String> {
    let vars: HashMap<String, String> = env.clone();
    // The SAME sweep serves the credential chain (see [`PROCESS_ENV`] / [`workspace_credentials`]),
    // so the settings directory is resolved once, here, at the root — before anything mounts a
    // venue. It is `set` BEFORE the boot below because [`workspace_credentials`], which the boot
    // calls, reads it.
    let _ = PROCESS_ENV.set(vars.clone());

    // ⚠ THE ORDER BELOW IS `vike-boot`'s, not this file's, and four other composition roots run the
    // same one. It used to be written out here — refuse, arm-check, walk, load — and in four other
    // `main`s besides, which is what made the CI box's failure expensive: the walk happening in five
    // places is five places to fix and five chances for two of them to disagree.
    vike_boot::boot(&vike_boot::BootSpec {
        env: &vars,
        cwd,
        identity: vike_boot::Identity {
            name: env!("CARGO_PKG_NAME"),
            version: env!("CARGO_PKG_VERSION"),
        },
        removed_env: vike_boot::RemovedEnv::Refuse,
        // ⚠ **REFUSE on an unsound SEAL.** This root mounts venues and signs orders, so it is the
        // one disposition that reverses `docs/decisions/0013` for that state — the argument is at
        // `vike_boot::SettingsLoad::LoadAndRefuseUnsoundSeal`, and the short form is that what
        // degrades here is the CEILING rather than a capability: an adopted box with unsound rows
        // resolves `max_notional_per_order` to `None`, which is no size cap at all. It refuses
        // including on a box it believes is paper, because `flags.tradehub_live` and
        // `policy.venues` are themselves in the layer that failed, so *am I live?* is not a
        // question it may answer from that layer's own faults.
        //
        // ⚠ **This comment READ "REFUSE on an unreadable settings store" while the arm was
        // `SettingsLoad::Load`, which refuses nothing** — an unreadable store is a MARK
        // (`Settings::store_refusal`) and this root started anyway. That half is still a
        // degrade-and-announce, deliberately: `vike_secrets::Backend` answers for CREDENTIALS on
        // the same probe, so an unopenable store means an empty credential map, which IS the live
        // gate — the box is all-paper and the ceiling cannot be reached. The SEAL is different
        // because the store opened FINE and said something illegal, so the credentials loaded and
        // the venues armed. Two marks, two dispositions, and the difference is now in the code
        // rather than only in a sentence.
        settings: vike_boot::SettingsLoad::LoadAndRefuseUnsoundSeal,
        // the daemon mounts on these ceilings; its unit grants `settings/db` read-write, so it
        // migrates a pre-0095 store itself and refuses one it cannot.
        ceilings: vike_boot::Ceilings::Interpret { now_ms: vike_model::now_ms() },

        // ...and the credential file may not ARM REAL MONEY. On a box that has not migrated,
        // `secrets.env` is plaintext and parsed last-wins, so appending ONE line to it — with no
        // read access at all — would otherwise be enough to flip this daemon onto a live venue. `vike-boot` runs the refusal at step 0,
        // before the settings load and a long way before every mount, precisely because this
        // process signs real orders. It does NOT change which sources arm a venue (decision 0095:
        // the ceiling alone, for binance/bybit/okx/hyperliquid); it makes an arming credential file
        // STOP the process rather than run it. See `vike_config::arming`.
        //
        // The LOADER is this daemon's own ([`workspace_credentials`], memoized), passed as a
        // function: `vike-boot` owns WHEN the store is opened and this binary owns HOW, which is
        // what keeps that crate free of `vike_bridge_core`'s transport stack.
        credentials: vike_boot::Credentials::LoadWith(&workspace_credentials),
        log_home: vike_boot::LogHome::Elsewhere(
            "this daemon's log home hangs off `$VIKE_STATE_ROOT` when set — see [`state_dir`] and \
             [`log_dir`] — which relocates the whole STATE tree (alerts.json included) \
             independently of where the settings were read from. `<settings>/state/logs` is only \
             its fallback, and it is `Booted::state_dir` (this boot's OWN walk) that supplies it \
             — see [`SETTINGS_STATE_DIR`].",
        ),
        disclosure: vike_boot::Disclosure::Render,
    })
    .inspect(|booted| {
        let _ = POLICY_MAX_NOTIONAL.set(booted.settings.policy.max_notional_per_order);
        // The rung UNDER `$VIKE_STATE_ROOT`, taken from the boot rather than walked for again —
        // see [`state_dir`]. Set here, inside the one function that runs before anything reads it.
        let _ = SETTINGS_STATE_DIR.set(booted.state_dir.clone());
    })
}

/// Every non-fatal resolution the loader made, formatted for the daemon's log — the PURE half of
/// [`resolve_settings`]'s surfacing step.
///
/// It exists as a function rather than an inline `for` loop for one reason: "the binary that loaded
/// the settings LOGS the warnings, never swallows them" is a real property with a real failure mode
/// (a clamped preference silently taking effect while the operator believes their number is in
/// force), and a property worth stating is worth gating. `vike_config` deliberately returns these as
/// DATA — it depends on serde + toml + vike-model and NOT on `tracing`, because a library that
/// writes to stderr on its own initiative cannot be used by a binary whose stdout is a protocol,
/// which this daemon's is. So the obligation to emit them is the binary's, and
/// `a_clamp_warning_is_surfaced_not_swallowed` below is what holds it.
pub(super) fn settings_warning_lines(settings: &vike_config::Settings) -> Vec<String> {
    settings.warnings.iter().map(|w| format!("settings: {w}")).collect()
}

/// The server-edge control limits (PR-12 defense-in-depth), resolved ONCE at startup and fixed for
/// the process lifetime. Deliberate behavior change from the old in-library per-connection re-read:
/// limits no longer hot-reload per accepted connection (never a documented feature); a changed
/// limit lands on daemon restart.
///
/// The two halves come from DIFFERENT authorities, which is the Phase-5 point:
/// - the NOTIONAL ceiling from [`POLICY_MAX_NOTIONAL`] (the policy file — no env layer at all;
///   `VIKE_TRADEHUB_MAX_ORDER_NOTIONAL` was removed because a ceiling a stale unit file can raise
///   is not a ceiling), and
/// - the command RATE still from `VIKE_TRADEHUB_CONTROL_RATE`, read HERE in the binary (audit F13,
///   the settings-registry rule) — a throughput knob, not a risk ceiling: raising it cannot place a
///   larger order.
///
/// The resolution itself stays the pure, unit-tested
/// [`server::control::ControlLimitsConfig::from_policy`].
///
/// ONE config, every control SURFACE: the TCP server ([`crate::node::start_observe_server`]) and —
/// under the `telegram` feature — the Telegram channel (`maybe_start_telegram`) both build their
/// own token bucket from this same resolved value, exactly as each TCP connection does.
pub(crate) fn resolve_control_limits() -> server::control::ControlLimitsConfig {
    server::control::ControlLimitsConfig::from_policy(
        POLICY_MAX_NOTIONAL.get().copied().flatten(),
        process_env().get("VIKE_TRADEHUB_CONTROL_RATE").map(String::as_str),
    )
}
