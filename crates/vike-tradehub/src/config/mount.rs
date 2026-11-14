//! `ProfileDaemon`'s lowering into what the mount builders are handed: the strategy FILE it resolves
//! (`resolve_mount`, the header's params checks `check_params`, the `.rhai` half `resolve_script`,
//! the `.rs` half through `config/sandboxed.rs`) and the strategy-free mount spec it lowers into
//! (`to_mount_spec`).
//!
//! Split out of `config.rs`'s one `impl ProfileDaemon` by concern. `use super::*` brings in the
//! parent module's imports and items.

use std::path::Path;

use vike_strategy::file::scan::StrategyFileLanguage;
use vike_strategy::file::{StrategyFileHeader, StrategyFileRunsOn};

use super::*;

impl ProfileDaemon {
    /// Resolve this profile's strategy FILE under `strategies` (the project's strategies folder),
    /// **saying WHERE it runs** ([`MountResolvedStrategy`]). `live` is the daemon's live gate
    /// (`flags.tradehub_live`): a file whose header does not say `runs_on = "live"` is refused there.
    ///
    /// In order, each step a refusal in words, all before any core spawns:
    ///
    /// 1. the path rule (`vike_strategy::file::scan::resolve`: relative, no `..`, `.rs` or `.rhai`;
    ///    [`Self::validate`] already refused the rest at load) and the read (a regular file within
    ///    `crate::mount_factory::MAX_STRATEGY_FILE_BYTES`, or what a wire mount read at the door);
    /// 2. the file's header ([`StrategyFileHeader::parse`]) and where it may run;
    /// 3. the `[strategy.params]` table against the header's declarations ([`Self::check_params`]);
    /// 4. a `.rhai` file compiles in this process ([`Self::resolve_script`]); a `.rs` file opens in
    ///    its own sandboxed process ([`sandboxed::open`]), whose own refusal comes back verbatim.
    ///
    /// The audit line: ONE INFO line carries the file and the sha256 of the bytes that were ACTUALLY
    /// read — the answer to "which code traded", which a path alone cannot give (the file can be
    /// edited between restarts). [`MountResolvedStrategy`] carries both, so a test asserts them without a
    /// log subscriber.
    ///
    /// # Errors
    ///
    /// The refusal, naming the file.
    pub fn resolve_mount(
        &self,
        strategies: &Path,
        live: bool,
    ) -> Result<MountResolvedStrategy, String> {
        self.resolve_mount_with(strategies, live, &sandboxed::open)
    }

    /// [`Self::resolve_mount`] for the daemon's START, which never waits for a build (the owner's ruling of
    /// 2026-11-14): a `.rs` file is judged as [`Self::resolve_mount`] judges it (the file, its header, where it
    /// may run, its params, its sha256) and NOT built. Its strategy is a placeholder the core parks, and the
    /// build it waits for comes back beside it ([`SandboxBuild`], keyed under `controller_id`, the id the core
    /// names the mount by), for a thread of its own to run once the node is up. A `.rhai` file resolves
    /// exactly as [`Self::resolve_mount`] resolves it, and `None` comes back beside it.
    ///
    /// # Errors
    ///
    /// As [`Self::resolve_mount`], minus the build's and the child's own refusals, which come later.
    pub fn resolve_mount_awaiting_build(
        &self,
        strategies: &Path,
        live: bool,
        controller_id: Option<&str>,
    ) -> Result<(MountResolvedStrategy, Option<SandboxBuild>), String> {
        let kept = std::cell::RefCell::new(None);
        let mounted = self.resolve_mount_with(strategies, live, &|request: &SandboxOpen<'_>| {
            Ok(sandboxed::awaiting_build(request, controller_id, &kept))
        })?;
        Ok((mounted, kept.into_inner()))
    }

    /// [`Self::resolve_mount`] with the `.rs` arm's opener named: the daemon passes
    /// [`sandboxed::open`], a test of the resolution around it a stand-in.
    ///
    /// # Errors
    ///
    /// As [`Self::resolve_mount`].
    pub fn resolve_mount_with(
        &self,
        strategies: &Path,
        live: bool,
        open_sandboxed: &dyn Fn(&SandboxOpen<'_>) -> Result<SandboxOpened, String>,
    ) -> Result<MountResolvedStrategy, String> {
        // `validate` refused a profile naming no file at load; a hand-built one gets the same words.
        let Some((s, file)) = self.strategy.as_ref().and_then(|s| Some((s, s.file.as_deref()?)))
        else {
            return Err(no_file_refusal());
        };
        let path = vike_strategy::file::scan::resolve(strategies, file)?;
        let source = crate::mount_factory::strategy_source(&path)?;
        let sha256 = vike_hub_protocol::hash::sha256_hex(source.as_bytes());
        let header = StrategyFileHeader::parse(&source)
            .map_err(|e| format!("strategy file {file:?}: {e}"))?;
        runs_here(file, &header, live)?;
        self.check_params(file, &header, &s.params)?;
        let mounted = match vike_strategy::file::scan::language_of(&path) {
            Some(StrategyFileLanguage::Rhai) => {
                let strategy = Self::resolve_script(file, &source, s, self.mount_symbol())?;
                MountResolvedStrategy::Script {
                    strategy,
                    path: file.to_string(),
                    sha256: sha256.clone(),
                    params: script_params_line(file, &s.params),
                }
            }
            // `scan::resolve` admits nothing but the two languages.
            Some(StrategyFileLanguage::Rust) | None => {
                let mount = self.to_mount_spec();
                let maker_round_trip_fee =
                    vike_tradehub_core::maker_round_trip_fee_for(&mount.venue, &mount.symbol);
                let opened = open_sandboxed(&SandboxOpen {
                    strategies,
                    file,
                    path: &path,
                    source: &source,
                    sha256: &sha256,
                    header: &header,
                    params: &s.params,
                    mount: &mount,
                    maker_round_trip_fee,
                })?;
                let params = echo_params_line(file, &opened.echo);
                MountResolvedStrategy::Sandboxed {
                    strategy: opened.strategy,
                    path: file.to_string(),
                    sha256: sha256.clone(),
                    params,
                    remote: opened.remote,
                }
            }
        };
        // The audit trail: WHICH code trades. INFO, once, at resolve — which `main` runs immediately
        // before either mount builder, so this line sits directly above the mount announcement in the
        // log, paper and live alike.
        let runs = match &mounted {
            MountResolvedStrategy::Script { .. } => "in this process",
            MountResolvedStrategy::Sandboxed { .. } => "in a sandboxed process",
        };
        tracing::info!(
            file = %file,
            sha256 = %sha256,
            runs,
            "mounting a strategy file — this hash is the file that runs"
        );
        Ok(mounted)
    }

    /// The `[strategy.params]` half of [`Self::resolve_mount`]: WHAT the file was handed, judged
    /// against what its header declares (`vike_strategy::file`'s checks, whose findings keep the
    /// registry checks' words byte for byte).
    ///
    /// ⚠ **Four refusals over one table, and each one is the case the previous one passes.** A key
    /// the header does not declare; a key whose VALUE its reader cannot take (a wrong type, or outside
    /// the declared range); a key that is read, well-typed, and OVERRIDDEN BY THE MOUNT (a route key
    /// naming another market); and — at the `.rs` file's open, its own `refuse` export — a whole
    /// TABLE that describes no order at all. The first three end in the same place: the knob runs at
    /// something the profile does not state, and the third is the worst, because the keys it covers
    /// name WHICH INSTRUMENT and WHICH VENUE real orders go to. MEASURED on the CI box before it existed: a
    /// `buy_hold` profile with `symbol = "MOUNTED_SYMBOL"` and `[strategy.params] symbol =
    /// "A_COMPLETELY_DIFFERENT_SYMBOL"` loaded, announced the second symbol, and filled on the first.
    ///
    /// The checks exist because a params reader CANNOT FAIL — every one in this workspace ignores a
    /// key it does not recognise, so `qtyy = 0.005` is not an error, it is `qty` at the compiled
    /// default. `deny_unknown_fields` on [`ProfileDaemon`] stops at the `[strategy.params]` boundary,
    /// a free-form table, and this table is the SIZE INPUT TO REAL ORDERS.
    ///
    /// ⚠ Deliberately the SAME strictness on the paper mount as on the live one: a paper rehearsal
    /// exists to predict the live mount.
    ///
    /// ⚠ **An INERT KEY is deliberately NOT a refusal** (the header's `needs` are not judged here):
    /// a key can pass every check and still be read by nobody, because another key in the same table
    /// sent the strategy down a branch that never looks at it (`anchor_price` with `anchor` unset).
    /// It is armed from the SAME table, and one shipped caller supplies half of an inert pair as a
    /// matter of course (`trailing_scalper`'s batch tool), so a refusal would reject that tool's own
    /// documented default.
    fn check_params(
        &self,
        file: &str,
        header: &StrategyFileHeader,
        params: &Params,
    ) -> Result<(), String> {
        let unknown = header.unknown_params(params);
        if !unknown.is_empty() {
            return Err(format!(
                "strategy file {:?} does not read these `[strategy.params]` keys: {}. A params \
                 reader IGNORES what it does not recognise, so each of them would mount at the \
                 strategy's COMPILED DEFAULT — a size knob among them is a live order at a size \
                 nobody typed. It reads: {}.",
                file,
                unknown.join(", "),
                header.readable_keys()
            ));
        }
        // ...and the same hazard one level down: a key it DOES read, carrying a value of a type (or
        // outside a range) it cannot take. Quoting a number (`size = "2"`) is the ordinary way to get
        // there, and it is invisible to `deny_unknown_fields`, to the unknown check, and to the reader.
        let mut mistyped = header.mistyped_params(params);
        mistyped.extend(header.out_of_range_params(params));
        if !mistyped.is_empty() {
            return Err(format!(
                "strategy file {:?} was handed `[strategy.params]` values of the wrong TYPE: {}. A \
                 params reader takes a value only when its TOML type matches, and IGNORES it \
                 otherwise — exactly as it ignores an unknown key — so each of these would mount at \
                 the strategy's COMPILED DEFAULT while the profile says otherwise. It reads: {}.",
                file,
                mistyped.iter().map(|e| e.to_string()).collect::<Vec<_>>().join("; "),
                header.readable_keys()
            ));
        }
        // ...and the third case, which the two above BOTH pass: a key that is read, whose value is
        // well-typed, and which the MOUNT then overrides. See the message for the mechanism.
        let misrouted = header.misrouted_params(params, self.venue(), self.mount_symbol());
        if !misrouted.is_empty() {
            return Err(format!(
                "strategy file {:?} was handed `[strategy.params]` keys naming a market this mount \
                 does not trade: {}. A live mount trades its OWN (venue, symbol) only — \
                 `crates/vike-core/src/runtime/strategy_drive/broker_drain.rs`'s \
                 `resolve_intent_venue` stamps the mount's venue on every order and \
                 `resolve_intent_symbol` REFUSES an order naming another symbol, because this \
                 strategy declares no leg of its own (`vike_model::Strategy::symbols`, the one \
                 source of `vike_tradehub_core::MountSpec`'s `legs`). So each \
                 of these configures NOTHING while the mount line echoes it as real — the \
                 operator reads one instrument while the orders go to another venue or are \
                 refused. Set it to this mount's own \
                 `venue = {:?}` / `symbol = {:?}`, or drop the key.",
                file,
                misrouted.iter().map(|m| m.to_string()).collect::<Vec<_>>().join("; "),
                self.venue(),
                self.mount_symbol(),
            ));
        }
        Ok(())
    }

    /// The `.rhai` arm of [`Self::resolve_mount`]: refuse an override the script never asks for,
    /// then compile it at [`vike_core::LiveBroker`] — the SAME broker every strategy mounts at.
    ///
    /// Two of the four rails `docs/decisions/0024-rhai-strategies-live.md` names live in this
    /// resolution (the audit line, in [`Self::resolve_mount`], and the engine surface here); the other
    /// two need no code in this crate (the mandatory live risk budget is `vike_tradehub_core`'s
    /// pre-connect `require_live_risk_budget`, and the HALT/panic safe-state is `vike-core`'s
    /// per-dispatch `catch_unwind`). [`vike_rhai::RhaiStrategy::compile_with_params`] builds its own
    /// resource-limited engine through `vike-rhai`'s ONE `build_engine`, the construction the
    /// backtest's script arm uses, verbatim; this daemon registers NO host function of its own.
    ///
    /// The unknown-override refusal is keyed on `vike_rhai::discover_params`, the script's own
    /// one-time TOP-LEVEL run, so a header that declares a key the script never asks for is refused
    /// too. The declared residual: a `param()` call made only INSIDE a hook body is invisible to that
    /// run, so its override key is refused with the message below — binding params at top level
    /// (`const SIZE = param("size", 1.0);`) is the documented idiom, and it is what bakes the value in.
    ///
    /// The script trades `symbol`, the mount's, as its own ([`vike_rhai::RhaiStrategy::mounted_on`]).
    fn resolve_script(
        file: &str,
        source: &str,
        s: &ProfileStrategy,
        symbol: &str,
    ) -> Result<Box<dyn vike_model::Strategy<vike_core::LiveBroker> + Send>, String> {
        let declared = vike_rhai::discover_params(source)
            .map_err(|e| format!("rhai script {file} failed to compile: {e}"))?;
        let overrides = script_overrides(&s.params);
        let unknown: Vec<&str> = overrides
            .keys()
            .filter(|k| !declared.iter().any(|(name, _)| name == *k))
            .map(String::as_str)
            .collect();
        if !unknown.is_empty() {
            return Err(format!(
                "rhai script {file} never asks for these `[strategy.params]` keys: {}. An \
                 override applies only where the script itself calls `param(name, default)` at \
                 top level, so each of these would configure NOTHING while the profile states a \
                 number. The script's own knobs: {}. (A `param()` call made only inside a hook \
                 body is invisible to this check — bind it at top level, `const X = \
                 param(\"x\", …);`, which is also what bakes the value in.)",
                unknown.join(", "),
                if declared.is_empty() {
                    "none — it declares no param() call at top level".to_string()
                } else {
                    declared
                        .iter()
                        .map(|(name, default)| format!("{name} (default {default})"))
                        .collect::<Vec<_>>()
                        .join(", ")
                },
            ));
        }
        let strategy = vike_rhai::RhaiStrategy::<vike_core::LiveBroker>::compile_with_params(
            source, overrides,
        )
        .map_err(|e| format!("rhai script {file} failed to compile: {e}"))?
        .mounted_on(symbol);
        Ok(Box::new(strategy))
    }

    /// [`Self::resolve_mount`]'s strategy, boxed for the two mount builders.
    ///
    /// # Errors
    ///
    /// As [`Self::resolve_mount`].
    pub fn resolve_strategy(
        &self,
        strategies: &Path,
        live: bool,
    ) -> Result<Box<dyn vike_model::Strategy<vike_core::LiveBroker> + Send>, String> {
        Ok(self.resolve_mount(strategies, live)?.into_strategy())
    }

    /// The strategy-free mount both generic builders take: venue, symbol, interval, the mount's paper
    /// equity and price grid, and the ACCOUNT. ONE derivation, so every strategy this daemon can mount
    /// agrees about venue/symbol/interval/seed_cash and the paper fee model.
    ///
    /// The unset defaults follow the venue's price domain: `1m` / `60_000` ms; paper equity `1_000`
    /// and a `0.01` grid on the `[0,1]` outcome-token venue (polymarket), `100_000` and a `1.0` grid
    /// on a `$`-scale one (one BTC clip is ~$300+); zero slippage and the venue's registry fee
    /// schedule (no flat fee).
    pub fn to_mount_spec(&self) -> MountPlacement {
        let outcome = self.venue() == "polymarket";
        let mut spec = MountPlacement::new(
            self.venue(),
            self.mount_symbol(),
            self.seed_cash.unwrap_or(if outcome { 1_000.0 } else { 100_000.0 }),
            self.tick_size.unwrap_or(if outcome { 0.01 } else { 1.0 }),
        );
        if let Some(interval) = &self.interval {
            spec.interval = interval.clone();
        }
        if let Some(ms) = self.interval_ms {
            spec.interval_ms = ms;
        }
        // WHICH ACCOUNT a mount trades on is a fact about the mount, not about the strategy.
        spec.account = self.account.clone();
        spec
    }
}

/// A `.rs` file's effective-params line: the rows its `echo` export answered when it opened, `key=value`
/// in the file's order, or the sentence saying it exports none.
pub(crate) fn echo_params_line(file: &str, echo: &[(String, String)]) -> String {
    if echo.is_empty() {
        format!(
            "(unreported — {file:?} exports no echo, so nothing here can state what it resolved)"
        )
    } else {
        echo.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ")
    }
}

/// The refusal of a mount that names no strategy file: there is no default strategy, and the
/// sentence names the folder a file is looked up in.
pub(super) fn no_file_refusal() -> String {
    "this mount names no strategy file: set `[strategy] file = \"<path>\"`, the path of a `.rs` or \
     `.rhai` file relative to the project's `strategies/` folder (for example \
     `file = \"trend/momentum.rs\"`). A mount without a file is not mounted: there is no default \
     strategy"
        .to_string()
}

/// Refuse a file whose header does not let it run on this daemon: `backtest` never (this daemon
/// does not link the simulator), `paper` not while the live gate is on, `live` always runs.
fn runs_here(file: &str, header: &StrategyFileHeader, live: bool) -> Result<(), String> {
    let why = header.strategy.why.as_deref().map(|w| format!(" ({w})")).unwrap_or_default();
    match header.strategy.runs_on {
        StrategyFileRunsOn::Backtest => Err(format!(
            "strategy file {file:?} runs in a backtest only{why}: its header says `runs_on = \
             \"backtest\"`, and this daemon does not link the simulator and could not mount it"
        )),
        StrategyFileRunsOn::Paper if live => Err(format!(
            "strategy file {file:?} cannot mount live: its header says `runs_on = \"paper\"` (a \
             header that says nothing is paper). A file trades on a live daemon only when its header \
             says `runs_on = \"live\"` and `why` it is safe there"
        )),
        StrategyFileRunsOn::Paper | StrategyFileRunsOn::Live => Ok(()),
    }
}

/// A `.rhai` file's effective-params line: its path plus the numeric overrides the compile bakes in —
/// a true claim, because [`ProfileDaemon::resolve_mount`] refuses an override the script's own top
/// level never passes to `param(…)`, so every key printed WAS asked for and DID land. Knobs the
/// profile does not override run at the script's own `param` defaults, in the source whose sha256
/// the resolve's audit line pins.
fn script_params_line(file: &str, params: &Params) -> String {
    let overrides = script_overrides(params);
    if overrides.is_empty() {
        format!("script={file} (no overrides — every param() runs its script default)")
    } else {
        let knobs: Vec<String> = overrides.iter().map(|(k, v)| format!("{k}={v}")).collect();
        format!("script={file} {}", knobs.join(" "))
    }
}
