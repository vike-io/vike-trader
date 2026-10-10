//! **Hot-reload classification + the tick-side apply seam** (split-plane REQ-7 v2).
//!
//! The write half (`vike_config::set_setting`, lowered by
//! `crates/vike-tradehub/src/server/settings.rs`'s `apply_set_setting`) landed restart-to-apply for every
//! key. This module is the v2 follow-up: it classifies every `config` / `preferences` / `flags`
//! key into HOT-SAFE (the running daemon can apply it) vs RESTART-REQUIRED, and carries the
//! machinery that performs a hot apply — off the wire thread, off the core fold, on the daemon's
//! existing periodic summary tick.
//!
//! # The classification table ([`CLASSIFICATION`])
//!
//! One row per key, the per-venue-capability-map discipline: a key's row is always NAMED, even
//! when its value equals the conservative default, because the named row proves it was CLASSIFIED
//! rather than forgotten. Exhaustiveness is machine-checked against
//! `vike_config::provenance::setting_keys` (the same authority `config show` renders from), so a
//! NEW settings key reddens this module's tests until it is classified. The idiom is
//! `vike_strategy::registry`'s `LIVE_CAPABLE`: the class that GRANTS power carries the written
//! justification — every [`HotClass::Hot`] row states, at the row, why applying it live is safe.
//!
//! **`policy` is NEVER hot, by construction rather than by rows**: [`classify`] answers
//! [`HotClass::Restart`] for the `policy` section before the table is consulted, so no future row
//! can make a risk ceiling hot-appliable. That is the sealed-policy doctrine —
//! `docs/decisions/0005-settings-split-by-authority.md`: policy deliberately has FEWER apply
//! paths than everything else, and a ceiling a remote peer could hot-lower today is a ceiling the
//! same peer could hot-RAISE tomorrow. A policy edit commits its row (bounded by the loader; the
//! typed confirm that used to sit at the server arm is deleted for every key,
//! `docs/decisions/0086` point 7) and takes effect on the next boot, full stop.
//!
//! # The apply seam, and why the tick
//!
//! A hot write does NOT apply on the server's connection thread. The accepted write enqueues an
//! apply job ([`HotApplyHandle::request_apply`]) and BLOCKS (bounded, [`HOT_APPLY_DEADLINE`])
//! while the daemon's existing periodic summary tick — the same off-fold thread that prints the
//! stdout summary and drives alerting — drains the queue ([`HotApplyTicker::drain`]) and executes
//! the apply ([`LogLevelApplier`]). One thread owns every runtime mutation, so two concurrent
//! `SetSetting`s cannot race an apply, and nothing new ever touches the core fold.
//!
//! The wire contract stays honest by construction: `Response::SettingsWritten.restart_required`
//! is `false` ONLY when the tick confirmed the apply executed within the deadline. A timeout, a
//! failed apply, a daemon without the seam wired (`SettingsShowSource.hot: None`) and a
//! restart-class key all answer `true` — which is always SAFE (the write is on disk and the next
//! boot loads it; the operator restarts and loses nothing). The one residual: an apply that lands
//! AFTER the deadline was already reported as restart-required — conservative, never false the
//! other way.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, SyncSender, channel, sync_channel};
use std::time::Duration;

use vike_config::SettingsSection;

/// How long an accepted hot write waits for the summary tick to confirm the apply. The tick
/// drains on every WAKE of its loop (≤100 ms granularity — `main.rs`'s summary thread sleeps in
/// small steps so `stop` is prompt), so two seconds is an order of magnitude of headroom, while
/// still bounding how long a `SetSetting` peer can be held on a stalled tick.
pub const HOT_APPLY_DEADLINE: Duration = Duration::from_secs(2);

/// One key's verdict: may the RUNNING daemon apply it, or does it wait for the next boot?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotClass {
    /// The daemon applies the new value live, on the summary tick. `reason` is the WRITTEN
    /// argument for why that is safe — required at the row, the `LIVE_CAPABLE` idiom.
    Hot {
        /// Why applying this key live is safe: what performs the apply, and why no thread,
        /// socket, mount or risk decision depends on the boot-time value.
        reason: &'static str,
    },
    /// The write lands on disk; the running daemon keeps its boot-time value. The conservative
    /// default for every key whose boot-time value is BUILT INTO something — a spawned thread, an
    /// opened socket, a mounted venue, a resolved path.
    Restart,
}

/// **The classification table** — one row per `config.` / `preferences.` / `flags.` key.
/// `policy.` keys are deliberately NOT rows: [`classify`] refuses the whole section first (the
/// sealed-policy doctrine, module doc). Exhaustiveness against
/// `vike_config::provenance::setting_keys` is gated by this module's tests.
pub const CLASSIFICATION: &[(SettingsSection, &str, HotClass)] = &[
    // -- config: EVERY key here is resolved at boot into something the process then HOLDS —
    //    a store root the recorder opened, a log directory the appender is writing into, an
    //    address a listener is bound to, an identity a minted id already carries. Re-pointing any
    //    of them mid-flight would change where a held resource claims to live without moving the
    //    resource, so the whole section is restart-required — `datahub_advertise_addr` included:
    //    `serve` Arc<str>s it ONCE for the listener's lifetime and hands the same value into every
    //    Welcome, so a mid-flight edit would be advertised by no connection this process accepts.
    (SettingsSection::Config, "store_root", HotClass::Restart),
    (SettingsSection::Config, "log_dir", HotClass::Restart),
    (SettingsSection::Config, "journal_dir", HotClass::Restart),
    (SettingsSection::Config, "datahub_addr", HotClass::Restart),
    // ⚠ The COMPUTE daemon's address (ruling 7), and one of the TWO config keys THIS DAEMON NEVER
    // READS — `node_addr` below is the other. `Restart` for the same reason every address here is,
    // and with the extra one that pair shares: `Hot` is a claim that THIS PROCESS APPLIES the value
    // live, and there is no applier for a key it does not consume. Its readers are the
    // `vike-backend backtest --addr` process that BINDS the address and the
    // `vike-cli backtest`/`sweep`/`walkforward`/`study` that DIAL it
    // (`crates/vike-config/src/config.rs`'s `Config::backtest_addr`), each taking it at its own
    // next start. It is classified rather than exempted because the table is EXHAUSTIVE over
    // the `config` keys: a key this process ignores would otherwise be reported as HOT to an
    // operator who then expects some other process to have picked it up. The write lands on disk
    // either way; what `Restart` refuses to do is tell the peer an edit took effect in a process
    // that cannot have applied it.
    //
    // ⚠ This row was written TWICE — once by ruling 7's daemon half and once by ruling 16's client
    // half, at different offsets in this array, so git merged both without a conflict. Both said
    // `Restart`, so nothing about the ANSWER changed; what a duplicate breaks is
    // `every_settings_key_is_classified_and_no_row_is_stale`'s closing `rows.len() ==
    // authority.len()`. ⚠ And it would not necessarily have broken it: `setting_keys()` carried a
    // duplicate of the SAME key from the SAME two branches, so both sides of that equality were
    // one too long and the count gate could have read green over two live defects at once. One row
    // per key, and the comment carries both halves' arguments.
    (SettingsSection::Config, "backtest_addr", HotClass::Restart),
    (SettingsSection::Config, "tradehub_addr", HotClass::Restart),
    (SettingsSection::Config, "datahub_advertise_addr", HotClass::Restart),
    // ⚠ The daemon's SELF-REPORT override, and `Restart` for a reason worth writing down: the
    // value is folded ONCE at startup into the process-static `WireNodeIdentity` that every
    // published frame carries, so a mid-flight edit would be reported by no frame this process
    // sends. The discovered half is process-static too — the route lookup answers a question about
    // the box's networking, which does not change without the daemon's own address having moved
    // under a live mount, at which point a restart is the honest response anyway.
    (SettingsSection::Config, "tradehub_advertise_addr", HotClass::Restart),
    // ⚠ **The account-admin barrier DECLARATION** (`docs/decisions/0065`), and `Restart` is the
    // whole of what makes that record's named self-escalation path survivable. The value is read
    // ONCE, in `crate::node`'s `account_admin_source`, before the listener exists — the
    // capability it decides is an `Option` handed to `serve`, and the bind it is CHECKED against
    // is already bound by the time any settings write could land. So a `Hot` row here could not
    // be honoured even if somebody wanted it: there is nothing mid-flight to re-decide.
    //
    // ⚠ And that is the property 0065 §3c leans on. A `SetSetting` frame carries any `config`
    // key with no server-side typed confirm, so a Control peer can in principle WRITE
    // this key; three things stand in the way and this row is the second of them — the daemon
    // cannot write its own settings directory on a shipped unit (`ProtectSystem=strict`), the
    // value is not consulted again until a restart, and arming still needs an ADMIN KEY a settings
    // write cannot mint.
    (SettingsSection::Config, "tradehub_account_admin", HotClass::Restart),
    // ⚠ `node_addr` is the OTHER of the two config keys THIS DAEMON NEVER READS (`backtest_addr`
    // above is the first) — it is a CLIENT's dial address,
    // the default for `vike-cli`'s `--node`, written on a laptop by `vike-cli backend connect` and
    // resolved by that dispatcher's boot. The tempting classification is therefore `Hot`, on the
    // ground that nothing here holds a resource derived from it, and that is exactly the wrong
    // move: `Hot` is a claim that THE DAEMON APPLIES the new value live, and there is no applier
    // for a key it does not consume. A `restart_required: false` on the wire would tell the peer
    // its edit had taken effect in this process, which is not a thing that can be true here.
    // `Restart` is both conservative AND accurate — the write lands on disk, and the reader that
    // cares is a different binary on a different box, which picks it up on its next invocation.
    (SettingsSection::Config, "node_addr", HotClass::Restart),
    // ⚠ `instance_origin` is restart-required TWICE OVER, and the second reason is the one that
    // makes a future "make it hot" request answerable: the tag is folded into the coid SESSION
    // that `vike_core::CoreConfig::instance_origin` stamps once at core assembly, and a running
    // core cannot change it without breaking the very continuity the session exists for — a
    // restart RESUMES the journalled session verbatim, so even a boot does not pick a new tag up
    // until a fresh one. Hot-applying it would put two different origins on one session's ids and
    // make reconcile call this instance's own outstanding orders foreign.
    (SettingsSection::Config, "instance_origin", HotClass::Restart),
    // The decision-0111 P3 rows. `Restart`, each for the section's own reason: the reconcile
    // family is built into the recon driver's `ReconConfig` at mount, the control rate into every
    // surface's token bucket, the pinning into threads already pinned, the snapshot cadence into the
    // journal the core opened — and the two datahub keys are another daemon's, which `Restart`
    // reports honestly (`backtest_addr`'s argument above).
    (SettingsSection::Config, "reconcile_policy", HotClass::Restart),
    (SettingsSection::Config, "reconcile_interval_ms", HotClass::Restart),
    (SettingsSection::Config, "reconcile_audit_ms", HotClass::Restart),
    (SettingsSection::Config, "reconcile_lookback_ms", HotClass::Restart),
    (SettingsSection::Config, "reconcile_startup_delay_ms", HotClass::Restart),
    (SettingsSection::Config, "reconcile_balance_tol_abs", HotClass::Restart),
    (SettingsSection::Config, "reconcile_balance_tol_rel", HotClass::Restart),
    (SettingsSection::Config, "tradehub_control_rate", HotClass::Restart),
    (SettingsSection::Config, "pin_cores", HotClass::Restart),
    (SettingsSection::Config, "datahub_bind_addr", HotClass::Restart),
    (SettingsSection::Config, "datahub_live_resident", HotClass::Restart),
    (SettingsSection::Config, "journal_snapshot_every", HotClass::Restart),
    // -- preferences: the two log LEVELS are the v2 hot set.
    (
        SettingsSection::Preferences,
        "log_level",
        HotClass::Hot {
            reason: "applied by recomposing the CONSOLE `EnvFilter` through \
                     `vike_log::LogReloadHandles::reload_console_level` on the summary tick — \
                     the same `RUST_LOG` > `VIKE_LOG` > preference precedence and the same \
                     credential-target pins a boot composes, so a reload can never produce a \
                     filter a restart would not. A level filters what is EMITTED; no thread, \
                     socket, mount or risk decision holds the boot-time value.",
        },
    ),
    (
        SettingsSection::Preferences,
        "log_file_level",
        HotClass::Hot {
            reason: "applied by recomposing the FILE `EnvFilter` through \
                     `vike_log::LogReloadHandles::reload_file_level` on the summary tick — the \
                     same `VIKE_LOG_FILE_LEVEL` > preference precedence a boot composes. This is \
                     the knob whose restart-to-apply gap has a measured cost (the 341 GB trace \
                     firehose `vike_log::file_level_directive` documents): being able to turn \
                     the file level DOWN on a running daemon is the point of the hot set. When \
                     no file layer was installed at boot the apply reports failure and the write \
                     stays restart-required — honest, since the level cannot land in this \
                     process either way.",
        },
    ),
    // preferences the daemon never reads: hot-applying a key this process does not consume would
    // confirm an apply that changed nothing here (chart_style is the GUI's, sweep_threads the
    // backtester's). Restart-required is the honest wire answer: the value is on disk for the
    // process that DOES read it, at ITS next boot.
    (SettingsSection::Preferences, "chart_style", HotClass::Restart),
    (SettingsSection::Preferences, "sweep_threads", HotClass::Restart),
    // The five APPEARANCE preferences are the desktop GUI's (design system spec §5): read at ITS
    // start and changed live by ITS Settings window. This daemon never reads them, so — like
    // `chart_style` above — `Restart` is the honest wire answer for a key this process does not
    // consume.
    (SettingsSection::Preferences, "theme", HotClass::Restart),
    (SettingsSection::Preferences, "market_colors", HotClass::Restart),
    (SettingsSection::Preferences, "header_gradient", HotClass::Restart),
    (SettingsSection::Preferences, "density", HotClass::Restart),
    (SettingsSection::Preferences, "text_size", HotClass::Restart),
    // `vike-cli`'s advisory preview cap: a CLIENT's preference this daemon never reads.
    (SettingsSection::Preferences, "max_order_qty", HotClass::Restart),
    // -- flags: every flag is a BOOT gate here — it decides what gets BUILT (a recon driver,
    //    a control server, a Telegram channel, a venue mount, a preflight), not how a built thing
    //    behaves per tick. Flipping one live would require constructing or tearing down whole
    //    subsystems off a settings write, which is B5-mount-verb territory, not a hot apply.
    //    Conservative v2: all restart-required.
    (SettingsSection::Flags, "reconcile", HotClass::Restart),
    // ⚠ RESTART-required like every other flag here, and worth one line of why it is not an
    // exception: it refuses a DEFAULT-ON behaviour, so the temptation is to make it hot ("turn the
    // venue reads off without a restart"). It cannot be — the driver, its `vt-core-recon` thread
    // and every venue `ReconClient` are BUILT at mount, so honouring a live write would mean
    // tearing that subsystem down mid-session. An operator who needs the reads to stop now stops
    // the daemon; that is what the kill-switch page says, and it is honest.
    (SettingsSection::Flags, "reconcile_off", HotClass::Restart),
    // ⚠ RESTART-required, and it is NOT this daemon's flag at all — it refuses the venue-catalog
    // lane on a `vike-datahub` (`docs/decisions/0066`). It has a row because this table's gate is
    // over `vike_config::provenance::setting_keys`, i.e. EVERY non-policy key in the tree rather
    // than the ones this binary reads, which is the property that makes the table complete instead
    // of a list somebody maintains. Restart is the right class twice over: the lane is built once,
    // at that other daemon's startup, and nothing here would apply a write to it live.
    (SettingsSection::Flags, "venue_catalog_off", HotClass::Restart),
    (SettingsSection::Flags, "reconcile_generate_missing", HotClass::Restart),
    (SettingsSection::Flags, "reconcile_balance", HotClass::Restart),
    (SettingsSection::Flags, "oco_cancel_sibling_on_dead_exit", HotClass::Restart),
    (SettingsSection::Flags, "tradehub_live", HotClass::Restart),
    (SettingsSection::Flags, "tradehub_control", HotClass::Restart),
    (SettingsSection::Flags, "tradehub_allow_public_bind", HotClass::Restart),
    (SettingsSection::Flags, "telegram_control", HotClass::Restart),
    (SettingsSection::Flags, "cancel_orders_on_shutdown", HotClass::Restart),
    (SettingsSection::Flags, "poly_exec", HotClass::Restart),
    (SettingsSection::Flags, "poly_reconcile", HotClass::Restart),
    (SettingsSection::Flags, "poly_heartbeat", HotClass::Restart),
    (SettingsSection::Flags, "poly_auto_redeem", HotClass::Restart),
    (SettingsSection::Flags, "poly_redeem_halt", HotClass::Restart),
    (SettingsSection::Flags, "pm_resolve", HotClass::Restart),
    (SettingsSection::Flags, "hl_outcome", HotClass::Restart),
    (SettingsSection::Flags, "hyperliquid_hip3", HotClass::Restart),
    (SettingsSection::Flags, "record_properties", HotClass::Restart),
    (SettingsSection::Flags, "record_chains", HotClass::Restart),
    // ⚠ `record_dvol` stood HERE and went with the key (`vike_config::DEAD_FLAG_KEYS`). The row
    // had to go in the same PR: `every_settings_key_is_classified`'s other direction fails a row
    // that names no live key.
    (SettingsSection::Flags, "allow_withdraw_keys", HotClass::Restart),
    (SettingsSection::Flags, "preflight_skip", HotClass::Restart),
    // The data daemon's three (decision 0111, P3) — `venue_catalog_off`'s argument: another
    // daemon's flags, built at ITS start.
    (SettingsSection::Flags, "datahub_live", HotClass::Restart),
    (SettingsSection::Flags, "datahub_chart_seed", HotClass::Restart),
    (SettingsSection::Flags, "datahub_allow_public_bind", HotClass::Restart),
];

/// Classify one accepted write. **The `policy` section answers [`HotClass::Restart`] before the
/// table is consulted** — the sealed-policy doctrine (module doc;
/// `docs/decisions/0005-settings-split-by-authority.md`) made structural, so no future table row
/// can make a ceiling hot. A key the table does not name
/// (unreachable after the loader validated the write, but this function does not assume its
/// caller) is conservatively restart-required.
pub fn classify(section: SettingsSection, key: &str) -> HotClass {
    if section == SettingsSection::Policy {
        return HotClass::Restart;
    }
    // The dotted key's first segment must name the section it was written to — the same rule
    // `WireCommand::SetSetting` states and the write arm enforces. A key that does not carry its
    // section is not a key this table can answer for, so it falls to the conservative default
    // rather than being matched on its tail.
    let Some(field) = key.strip_prefix(section.section()).and_then(|r| r.strip_prefix('.')) else {
        return HotClass::Restart;
    };
    CLASSIFICATION
        .iter()
        .find(|(s, k, _)| *s == section && *k == field)
        .map(|(_, _, class)| *class)
        .unwrap_or(HotClass::Restart)
}

/// The dotted key one [`CLASSIFICATION`] row describes (`"preferences.log_level"`) — composed
/// from the row's own section rather than stored, which is why no row spells one.
///
/// ⚠ That is not stylistic. `crates/vike-config/tests/settings_are_consumed.rs` scans every
/// non-comment `src/` line for a SECTION-QUALIFIED key and fails a `Consumer::Not` row whose key
/// it finds, on the rule that a wired consumer necessarily spells one. A table that NAMED its
/// keys therefore read, to that gate, as the code that consumes 30-odd settings it merely
/// classifies — measured on the CI box, eleven rows at once. Composing the key here keeps the gate's
/// rule intact (this file genuinely consumes only the two log levels, in [`LogLevelApplier`])
/// instead of carving an exemption out of it.
pub fn row_key(section: SettingsSection, field: &str) -> String {
    format!("{}.{field}", section.section())
}

/// One queued hot-apply job: the dotted key, plus the one-shot lane the tick answers on.
struct HotApplyJob {
    key: String,
    done: SyncSender<bool>,
}

/// The SERVER side of the apply seam: held (as `SettingsShowSource.hot`) by the node server,
/// which enqueues an accepted hot write and waits — bounded — for the tick's verdict.
#[derive(Debug, Clone)]
pub struct HotApplyHandle {
    tx: Sender<HotApplyJob>,
}

impl HotApplyHandle {
    /// Enqueue `key` for the tick and wait up to `deadline` for the apply verdict. `true` ONLY
    /// when the tick reported the apply executed successfully; a timeout, an apply failure, or a
    /// ticker that is gone (shutdown) all answer `false` — the caller then reports
    /// restart-required, which is always the safe direction (see the module doc's one residual).
    pub fn request_apply(&self, key: &str, deadline: Duration) -> bool {
        let (done_tx, done_rx) = sync_channel(1);
        if self.tx.send(HotApplyJob { key: key.to_string(), done: done_tx }).is_err() {
            return false;
        }
        // Both error arms mean the same thing and both are `false`: a TIMEOUT (the tick is
        // stalled or not yet running) and a DISCONNECT (the ticker is gone — shutdown) each
        // leave the value un-applied, and an un-applied value is restart-required. Spelled as
        // `unwrap_or_default` because clippy is right that the match added no information the
        // sentence above does not carry.
        done_rx.recv_timeout(deadline).unwrap_or_default()
    }
}

/// The TICK side of the apply seam: moved onto the daemon's summary thread, drained on every wake
/// of its loop.
#[derive(Debug)]
pub struct HotApplyTicker {
    rx: Receiver<HotApplyJob>,
}

impl HotApplyTicker {
    /// Drain every queued job, executing each through `apply` (which answers whether the apply
    /// executed successfully) and reporting the verdict back to the waiting server thread. A
    /// waiter that gave up (deadline) makes the report a no-op; an empty queue makes the whole
    /// call one `try_recv`.
    pub fn drain(&self, mut apply: impl FnMut(&str) -> bool) {
        while let Ok(job) = self.rx.try_recv() {
            let applied = apply(&job.key);
            // Receiver gone = the waiter hit its deadline and left (see above); nobody to tell.
            let _ = job.done.send(applied);
        }
    }
}

/// Build the two ends of the apply seam.
pub fn hot_apply_channel() -> (HotApplyHandle, HotApplyTicker) {
    // control, rare: one job per accepted hot settings write (admin rate), each waited on by
    // `request_apply`; a timed-out job can linger until the next tick, never a stream.
    let (tx, rx) = channel();
    (HotApplyHandle { tx }, HotApplyTicker { rx })
}

/// **The v2 applier** — executes the two hot keys' applies on the tick thread. Holds what a
/// re-apply needs: the reload handles [`vike_log::init_with_reload`] returned, the settings
/// directory the daemon booted from, and the daemon's ONE startup env sweep (a daemon's
/// environment is fixed at spawn, so the boot sweep IS the current one — the
/// `SettingsShowSource` argument).
///
/// An apply RE-LOADS the settings from the store (`vike_config::load_with_source`, the same
/// env>db>default layering the boot ran) rather than trusting the written string, then recomposes
/// the affected filter through the same `vike-log` precedence a boot composes. So the post-apply
/// filter is byte-identical to what a restart would have built — including the case where an env
/// override outranks the freshly written preference: the apply executes, the effective level stays
/// the env's, and a restart would answer the same.
pub struct LogLevelApplier {
    /// The reload handles over the installed subscriber's two filters.
    pub handles: vike_log::LogReloadHandles,
    /// `<project>/settings` as the daemon's boot walk resolved it.
    pub settings_dir: Option<PathBuf>,
    /// The daemon's startup `std::env::vars()` sweep (owned by the binary — the settings-registry
    /// rule: this module reads no environment).
    pub env: HashMap<String, String>,
}

impl LogLevelApplier {
    /// Execute the apply for one hot key. `true` only when it actually executed. An unknown key
    /// answers `false` — this applier owns exactly the two log levels, and claiming success for
    /// anything else would let a future hot row silently "apply" as a no-op.
    pub fn apply(&self, key: &str) -> bool {
        // ⚠ THROUGH THE SETTINGS SOURCE. A store-blind `vike_config::load` reads no rows, so a hot
        // apply would quietly set the level to the compiled-in default instead of to the value the
        // operator just wrote. The exemption `crates/vike-boot/tests/one_owner.rs` grants this call
        // argues that the DIRECTORY is a parameter, which stays true and says nothing about the
        // LAYER SET.
        let read = self.settings_dir.as_deref().map(vike_secrets::read_settings_in);
        let mut refusal = String::new();
        let source = vike_config::StoreLayer::of(read.as_ref(), &mut refusal);
        let Ok(settings) = vike_config::load_with_source(
            self.settings_dir.as_deref(),
            source,
            &self.env,
            &vike_config::CliOverrides::default(),
        ) else {
            // The settings no longer load (a removed `<project>/vike.toml` appeared, or could not
            // be statted, since the validated write) — nothing trustworthy to apply.
            // Restart-required is then also what the next boot will say, loudly.
            return false;
        };
        let get = |name: &str| self.env.get(name).map(String::as_str);
        match key {
            "preferences.log_level" => self
                .handles
                .reload_console_level(
                    get(vike_config::preferences::RUST_LOG_ENV),
                    get(vike_config::preferences::LOG_LEVEL_ENV),
                    &settings.preferences.log_level,
                )
                .is_ok(),
            "preferences.log_file_level" => self
                .handles
                .reload_file_level(
                    get(vike_config::preferences::LOG_FILE_LEVEL_ENV),
                    &settings.preferences.log_file_level,
                )
                .is_ok(),
            _ => false,
        }
    }
}

#[path = "hot_reload_tests.rs"]
#[cfg(test)]
mod hot_reload_tests;
