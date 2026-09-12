//! **Hot-reload classification + the tick-side apply seam** (split-plane REQ-7 v2).
//!
//! The write half (`vike_config::set_setting`, lowered by
//! `crates/vike-tradehub/src/server.rs`'s `apply_set_setting`) landed restart-to-apply for every
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
//! **`policy.toml` is NEVER hot, by construction rather than by rows**: [`classify`] answers
//! [`HotClass::Restart`] for the policy file before the table is consulted, so no future row can
//! make a risk ceiling hot-appliable. That is the sealed-policy doctrine —
//! `docs/decisions/0005-settings-split-by-authority.md`: policy deliberately has FEWER apply
//! paths than everything else, and a ceiling a remote peer could hot-lower today is a ceiling the
//! same peer could hot-RAISE tomorrow. A policy edit lands on disk (typed-confirm enforced at the
//! server arm) and takes effect on the next boot, full stop.
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

use vike_config::SettingsFile;

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
/// `policy.` keys are deliberately NOT rows: [`classify`] refuses the whole file first (the
/// sealed-policy doctrine, module doc). Exhaustiveness against
/// `vike_config::provenance::setting_keys` is gated by this module's tests.
pub const CLASSIFICATION: &[(SettingsFile, &str, HotClass)] = &[
    // -- config.toml: EVERY key here is resolved at boot into something the process then HOLDS —
    //    a store root the recorder opened, a log directory the appender is writing into, an
    //    address a listener is bound to, an identity a minted id already carries. Re-pointing any
    //    of them mid-flight would change where a held resource claims to live without moving the
    //    resource, so the whole file is restart-required — `datahub_advertise_addr` included:
    //    `serve` Arc<str>s it ONCE for the listener's lifetime and hands the same value into every
    //    Welcome, so a mid-flight edit would be advertised by no connection this process accepts.
    (SettingsFile::Config, "store_root", HotClass::Restart),
    (SettingsFile::Config, "log_dir", HotClass::Restart),
    (SettingsFile::Config, "journal_dir", HotClass::Restart),
    (SettingsFile::Config, "state_dir", HotClass::Restart),
    (SettingsFile::Config, "datahub_addr", HotClass::Restart),
    // ⚠ The COMPUTE daemon's address (ruling 7), and one of the TWO config keys THIS DAEMON NEVER
    // READS — `node_addr` below is the other. `Restart` for the same reason every address here is,
    // and with the extra one that pair shares: `Hot` is a claim that THIS PROCESS APPLIES the value
    // live, and there is no applier for a key it does not consume. Its readers are the
    // `vike-backend backtest --addr` process that BINDS the address and the
    // `vike-cli backtest`/`sweep`/`walkforward`/`study` that DIAL it
    // (`crates/vike-config/src/config.rs`'s `Config::backtest_addr`), each taking it at its own
    // next start. It is classified rather than exempted because the table is EXHAUSTIVE over
    // `config.toml`'s keys: a key this process ignores would otherwise be reported as HOT to an
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
    (SettingsFile::Config, "backtest_addr", HotClass::Restart),
    (SettingsFile::Config, "tradehub_addr", HotClass::Restart),
    (SettingsFile::Config, "datahub_advertise_addr", HotClass::Restart),
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
    (SettingsFile::Config, "node_addr", HotClass::Restart),
    // ⚠ `instance_origin` is restart-required TWICE OVER, and the second reason is the one that
    // makes a future "make it hot" request answerable: the tag is folded into the coid SESSION
    // that `vike_core::CoreConfig::instance_origin` stamps once at core assembly, and a running
    // core cannot change it without breaking the very continuity the session exists for — a
    // restart RESUMES the journalled session verbatim, so even a boot does not pick a new tag up
    // until a fresh one. Hot-applying it would put two different origins on one session's ids and
    // make reconcile call this instance's own outstanding orders foreign.
    (SettingsFile::Config, "instance_origin", HotClass::Restart),
    // -- preferences.toml: the two log LEVELS are the v2 hot set.
    (
        SettingsFile::Preferences,
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
        SettingsFile::Preferences,
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
    (SettingsFile::Preferences, "chart_style", HotClass::Restart),
    (SettingsFile::Preferences, "sweep_threads", HotClass::Restart),
    // -- flags.toml: every flag is a BOOT gate here — it decides what gets BUILT (a recon driver,
    //    a control server, a Telegram channel, a venue mount, a preflight), not how a built thing
    //    behaves per tick. Flipping one live would require constructing or tearing down whole
    //    subsystems off a settings write, which is B5-mount-verb territory, not a hot apply.
    //    Conservative v2: all restart-required.
    (SettingsFile::Flags, "reconcile", HotClass::Restart),
    // ⚠ RESTART-required like every other flag here, and worth one line of why it is not an
    // exception: it refuses a DEFAULT-ON behaviour, so the temptation is to make it hot ("turn the
    // venue reads off without a restart"). It cannot be — the driver, its `vt-core-recon` thread
    // and every venue `ReconClient` are BUILT at mount, so honouring a live write would mean
    // tearing that subsystem down mid-session. An operator who needs the reads to stop now stops
    // the daemon; that is what the kill-switch page says, and it is honest.
    (SettingsFile::Flags, "reconcile_off", HotClass::Restart),
    (SettingsFile::Flags, "reconcile_generate_missing", HotClass::Restart),
    (SettingsFile::Flags, "reconcile_balance", HotClass::Restart),
    (SettingsFile::Flags, "oco_cancel_sibling_on_dead_exit", HotClass::Restart),
    (SettingsFile::Flags, "tradehub_live", HotClass::Restart),
    (SettingsFile::Flags, "tradehub_control", HotClass::Restart),
    (SettingsFile::Flags, "tradehub_allow_public_bind", HotClass::Restart),
    (SettingsFile::Flags, "telegram_control", HotClass::Restart),
    (SettingsFile::Flags, "tradehub_record", HotClass::Restart),
    (SettingsFile::Flags, "cancel_orders_on_shutdown", HotClass::Restart),
    (SettingsFile::Flags, "poly_exec", HotClass::Restart),
    (SettingsFile::Flags, "poly_reconcile", HotClass::Restart),
    (SettingsFile::Flags, "poly_presubmit_register", HotClass::Restart),
    (SettingsFile::Flags, "poly_heartbeat", HotClass::Restart),
    (SettingsFile::Flags, "poly_rate_gate", HotClass::Restart),
    (SettingsFile::Flags, "poly_chain_watch", HotClass::Restart),
    (SettingsFile::Flags, "poly_chain_proxy", HotClass::Restart),
    (SettingsFile::Flags, "poly_auto_redeem", HotClass::Restart),
    (SettingsFile::Flags, "poly_redeem_halt", HotClass::Restart),
    (SettingsFile::Flags, "pm_resolve", HotClass::Restart),
    (SettingsFile::Flags, "hl_outcome", HotClass::Restart),
    (SettingsFile::Flags, "bybit_fast_exec", HotClass::Restart),
    (SettingsFile::Flags, "binance_trade_lite_fill", HotClass::Restart),
    (SettingsFile::Flags, "hyperliquid_hip3", HotClass::Restart),
    (SettingsFile::Flags, "record_properties", HotClass::Restart),
    (SettingsFile::Flags, "record_chains", HotClass::Restart),
    (SettingsFile::Flags, "record_dvol", HotClass::Restart),
    (SettingsFile::Flags, "allow_withdraw_keys", HotClass::Restart),
    (SettingsFile::Flags, "preflight_skip", HotClass::Restart),
];

/// Classify one accepted write. **`policy.toml` answers [`HotClass::Restart`] before the table is
/// consulted** — the sealed-policy doctrine (module doc; `docs/decisions/0005-settings-split-by-authority.md`)
/// made structural, so no future table row can make a ceiling hot. A key the table does not name
/// (unreachable after the loader validated the write, but this function does not assume its
/// caller) is conservatively restart-required.
pub fn classify(file: SettingsFile, key: &str) -> HotClass {
    if file == SettingsFile::Policy {
        return HotClass::Restart;
    }
    // The dotted key's first segment must name the file it was written to — the same rule
    // `WireCommand::SetSetting` states and the write arm enforces. A key that does not carry its
    // file's section is not a key this table can answer for, so it falls to the conservative
    // default rather than being matched on its tail.
    let Some(field) = key.strip_prefix(file.section()).and_then(|r| r.strip_prefix('.')) else {
        return HotClass::Restart;
    };
    CLASSIFICATION
        .iter()
        .find(|(f, k, _)| *f == file && *k == field)
        .map(|(_, _, class)| *class)
        .unwrap_or(HotClass::Restart)
}

/// The dotted key one [`CLASSIFICATION`] row describes (`"preferences.log_level"`) — composed
/// from the row's own file rather than stored, which is why no row spells one.
///
/// ⚠ That is not stylistic. `crates/vike-config/tests/settings_are_consumed.rs` scans every
/// non-comment `src/` line for a SECTION-QUALIFIED key and fails a `Consumer::Not` row whose key
/// it finds, on the rule that a wired consumer necessarily spells one. A table that NAMED its
/// keys therefore read, to that gate, as the code that consumes 30-odd settings it merely
/// classifies — measured on the CI box, eleven rows at once. Composing the key here keeps the gate's
/// rule intact (this file genuinely consumes only the two log levels, in [`LogLevelApplier`])
/// instead of carving an exemption out of it.
pub fn row_key(file: SettingsFile, field: &str) -> String {
    format!("{}.{field}", file.section())
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
            let _ = job.done.send(applied);
        }
    }
}

/// Build the two ends of the apply seam.
pub fn hot_apply_channel() -> (HotApplyHandle, HotApplyTicker) {
    let (tx, rx) = channel();
    (HotApplyHandle { tx }, HotApplyTicker { rx })
}

/// **The v2 applier** — executes the two hot keys' applies on the tick thread. Holds what a
/// re-apply needs: the reload handles [`vike_log::init_with_reload`] returned, the settings
/// directory the daemon booted from, and the daemon's ONE startup env sweep (a daemon's
/// environment is fixed at spawn, so the boot sweep IS the current one — the
/// `SettingsShowSource` argument).
///
/// An apply RE-LOADS the settings from disk (`vike_config::load`, the same env>file>default
/// layering the boot ran) rather than trusting the written string, then recomposes the affected
/// filter through the same `vike-log` precedence a boot composes. So the post-apply filter is
/// byte-identical to what a restart would have built — including the case where an env override
/// outranks the freshly written preference: the apply executes, the effective level stays the
/// env's, and a restart would answer the same.
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
        let Ok(settings) = vike_config::load(self.settings_dir.as_deref(), &self.env) else {
            // The directory no longer loads (a file broken by hand since the validated write) —
            // nothing trustworthy to apply. Restart-required is then also what the next boot
            // will say, loudly.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// **Exhaustiveness, both directions**, against the same authority `config show` renders
    /// from: every non-policy key `vike_config::provenance::setting_keys` knows has a named
    /// [`CLASSIFICATION`] row (a NEW settings key reddens this until classified), and every row
    /// names a real key (a DELETED key cannot leave a stale row behind). Policy keys are
    /// deliberately absent from the table — the file-level seal is the next test.
    #[test]
    fn every_settings_key_is_classified_and_no_row_is_stale() {
        let authority: Vec<String> = vike_config::provenance::setting_keys()
            .into_iter()
            .filter(|k| k.file != "policy.toml")
            .map(|k| k.key)
            .collect();
        let rows: Vec<String> =
            CLASSIFICATION.iter().map(|(file, field, _)| row_key(*file, field)).collect();
        for key in &authority {
            assert!(
                rows.contains(key),
                "settings key {key:?} has no CLASSIFICATION row — classify it (HotClass::Restart \
                 is the conservative default; a HotClass::Hot row must carry its written reason)"
            );
        }
        for key in &rows {
            assert!(
                authority.contains(key),
                "CLASSIFICATION row {key:?} names no live settings key — delete the stale row"
            );
        }
        assert_eq!(
            rows.len(),
            authority.len(),
            "one row per key exactly (a duplicate row would shadow nothing but confuse readers)"
        );
    }

    /// Every HOT row carries a real written reason — the `LIVE_CAPABLE` idiom's teeth. Length-
    /// checked the way `vike_config::consumed`'s gate checks `why`: a one-word waved-through
    /// reason is the shape being prevented.
    #[test]
    fn every_hot_row_carries_a_written_reason() {
        for (file, field, class) in CLASSIFICATION {
            if let HotClass::Hot { reason } = class {
                assert!(
                    reason.len() >= 80 && !reason.to_ascii_lowercase().contains("todo"),
                    "hot key {:?} needs a real written reason (what applies it, and why no state \
                     depends on the boot-time value); got {reason:?}",
                    row_key(*file, field)
                );
            }
        }
    }

    /// **`policy.toml` is sealed against the table itself**: [`classify`] answers `Restart` for a
    /// policy key even when a (hypothetical, wrong) table row would say otherwise — the file is
    /// refused before the table is consulted, so the doctrine cannot be undone by one row.
    #[test]
    fn a_policy_key_is_never_hot() {
        // ⚠ The policy keys are DERIVED from the authority, never spelled here — two reasons, and
        // the second is why this test reads the way it does. (1) EVERY policy key is covered, not
        // two hand-picked ones, so a new ceiling is sealed the day it is added. (2) A literal
        // `policy.<field>` in this file is READ AS A CONSUMER by
        // `crates/vike-config/tests/policy_is_consumed.rs`, whose scanner greps the tree for a
        // field name and promotes a `Consumed::No` row when it finds one — a test asserting that
        // a ceiling is NOT applied would have been recorded as the code that applies it (measured
        // on the CI box: `Policy::max_leverage is marked Consumed::No, but hot_reload.rs reads it`).
        let policy_keys: Vec<String> = vike_config::provenance::setting_keys()
            .into_iter()
            .filter(|k| k.file == "policy.toml")
            .map(|k| k.key)
            .collect();
        assert!(!policy_keys.is_empty(), "the authority must know some policy keys");
        for key in &policy_keys {
            assert_eq!(
                classify(SettingsFile::Policy, key),
                HotClass::Restart,
                "{key} must be restart-only: policy is sealed (decision 0005)"
            );
        }
        // ...and no policy key has a table row at all, so the seal is not merely shadowing one.
        assert!(
            CLASSIFICATION.iter().all(|(f, _, _)| *f != SettingsFile::Policy),
            "the sealed-policy doctrine forbids policy rows in CLASSIFICATION"
        );
    }

    /// **The v2 hot set is EXACTLY the two log levels** — a pin, so growing the set is a
    /// deliberate diff on this line plus a reasoned table row, never a drive-by.
    #[test]
    fn the_hot_set_is_exactly_the_two_log_levels() {
        let hot: Vec<String> = CLASSIFICATION
            .iter()
            .filter(|(_, _, c)| matches!(c, HotClass::Hot { .. }))
            .map(|(f, field, _)| row_key(*f, field))
            .collect();
        assert_eq!(hot, ["preferences.log_level", "preferences.log_file_level"]);
    }

    /// The classify dispatch: a table row answers for its own key; an unknown key (or a key from
    /// the wrong file) is conservatively restart-required.
    #[test]
    fn classify_answers_the_table_and_defaults_to_restart() {
        assert!(matches!(
            classify(SettingsFile::Preferences, "preferences.log_level"),
            HotClass::Hot { .. }
        ));
        assert_eq!(classify(SettingsFile::Config, "config.tradehub_addr"), HotClass::Restart);
        assert_eq!(
            classify(SettingsFile::Preferences, "preferences.no_such_key"),
            HotClass::Restart
        );
    }

    /// The seam round-trip: a hot request from one thread is applied by a drain on another, the
    /// waiter sees `true`; a FAILED apply reports `false`; with NOBODY draining, the deadline
    /// answers `false` (the honest restart-required); with the ticker DROPPED (shutdown), the
    /// request answers `false` immediately.
    #[test]
    fn the_apply_seam_reports_executed_failed_timeout_and_gone() {
        // executed
        let (handle, ticker) = hot_apply_channel();
        let waiter = {
            let handle = handle.clone();
            std::thread::spawn(move || {
                handle.request_apply("preferences.log_level", Duration::from_secs(5))
            })
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut applied_keys: Vec<String> = Vec::new();
        while applied_keys.is_empty() && std::time::Instant::now() < deadline {
            ticker.drain(|key| {
                applied_keys.push(key.to_string());
                true
            });
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(waiter.join().expect("waiter thread"), "an executed apply answers true");
        assert_eq!(applied_keys, ["preferences.log_level"], "the tick saw the requested key");

        // failed apply
        let waiter = {
            let handle = handle.clone();
            std::thread::spawn(move || {
                handle.request_apply("preferences.log_level", Duration::from_secs(5))
            })
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut drained = false;
        while !drained && std::time::Instant::now() < deadline {
            ticker.drain(|_| {
                drained = true;
                false
            });
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!waiter.join().expect("waiter thread"), "a failed apply answers false");

        // timeout: nobody drains
        assert!(
            !handle.request_apply("preferences.log_level", Duration::from_millis(50)),
            "an undrained request times out to false (restart-required, honestly)"
        );

        // gone: ticker dropped
        drop(ticker);
        assert!(
            !handle.request_apply("preferences.log_level", Duration::from_millis(50)),
            "a dropped ticker (shutdown) answers false immediately"
        );
    }
}
