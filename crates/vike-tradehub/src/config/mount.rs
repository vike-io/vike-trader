//! `DaemonProfile`'s lowering into what the mount builders are handed: the strategy it resolves
//! (`mounted_maker`, `maker_name`, `resolve_mount`, `resolve_script`, `resolve_strategy`,
//! `strategy_name`), the one-line echo of what that strategy RESOLVED to (`effective_params`), and
//! the mount config / strategy-free mount spec it lowers into (`to_mount_config`, `to_mount_spec`).
//!
//! Split out of `config.rs`'s one `impl DaemonProfile` by concern (behaviour byte-identical; the
//! methods moved verbatim). `use super::*` brings in the parent module's imports and items, so
//! nothing about resolution changes.

use super::*;

impl DaemonProfile {
    /// The A-S [`SpreadMaker`] this profile mounts, or `None` when it names a different strategy.
    ///
    /// **This is the ONE construction site for the maker, under BOTH spellings** — absent
    /// `[strategy]` and `[strategy] name = "spread_maker"` (or `"gueant_maker"`) reach the identical
    /// `vike_mount::build_maker(cfg)` call over the identical `cfg`, so they cannot produce different
    /// makers. See [`AS_MAKER_NAMES`] for what the registry arm would have produced instead, and why
    /// it was a live hazard rather than a cosmetic difference.
    ///
    /// `gueant_maker` is that same maker with the GLFT closed form selected — the registry's own
    /// definition of the alias (`spread_maker` forced to [`SpreadModel::Gueant`]), applied to the
    /// profile's config rather than to a default one.
    pub fn mounted_maker(&self, cfg: &MakerMountConfig) -> Option<SpreadMaker> {
        let name = self.maker_name()?;
        let maker = vike_mount::build_maker(cfg);
        Some(if name == "gueant_maker" {
            maker.with_spread_model(SpreadModel::Gueant)
        } else {
            maker
        })
    }

    /// WHICH maker name this profile means, or `None` when it names a different strategy. The ONE
    /// routing decision behind [`Self::mounted_maker`], [`Self::resolve_mount`] and
    /// [`Self::effective_params`], so the three cannot disagree about what is mounted.
    fn maker_name(&self) -> Option<&str> {
        match &self.strategy {
            None => Some("spread_maker"),
            Some(s) => match s.name.as_deref() {
                Some(n) if AS_MAKER_NAMES.contains(&n) => Some(n),
                // Any other registry name, or a `rhai = "<path>"` script — not the maker.
                _ => None,
            },
        }
    }

    /// Resolve this profile's strategy, **saying WHICH construction produced it**.
    ///
    /// The variant is the property under test. `[strategy] name = "spread_maker"` reaching
    /// [`MountedStrategy::Registered`] is exactly the defect this shape exists to make impossible:
    /// that arm reads `[strategy.params]` alone, so it would mount `qty = 1`, `tick_size = 0` and
    /// the `[0,1]` wall clamp while the profile said otherwise. Returning an opaque box from both
    /// paths is what let the difference hide — nothing downstream could tell the two apart, and
    /// neither could a test.
    ///
    /// [`Self::validate`] already rejected every name the registry arm can fail on, so an `Err` here
    /// means the two disagreed — worth surfacing rather than unwrapping.
    pub fn resolve_mount(&self, cfg: &MakerMountConfig) -> Result<MountedStrategy, String> {
        if let Some(maker) = self.mounted_maker(cfg) {
            return Ok(MountedStrategy::AsMaker(Box::new(maker)));
        }
        let s =
            self.strategy.as_ref().expect("mounted_maker returns Some for an absent [strategy]");
        if let Some(path) = s.rhai.as_deref() {
            return Self::resolve_script(path, s);
        }
        let name = s
            .name
            .as_deref()
            .expect("validate refused a [strategy] table with neither `name` nor `rhai`");
        match vike_strategy::strategy_by_name::<vike_core::LiveBroker>(name, &s.params) {
            Ok(boxed) => Ok(MountedStrategy::Registered(boxed)),
            // Not a built-in: the USER registry, tried last (same order as `validate_strategy`,
            // which already gated live-capability — an unknown name surviving to here still errors
            // with the built-in wording, per the "the two disagreed" contract above).
            Err(vike_strategy::RegistryError::Unknown(_)) => {
                match vike_user_strategies::user_strategy_by_name::<vike_core::LiveBroker>(
                    name, &s.params,
                ) {
                    Some(boxed) => Ok(MountedStrategy::Registered(boxed)),
                    None => {
                        Err(vike_strategy::RegistryError::Unknown(name.to_string()).to_string())
                    }
                }
            }
            Err(e) => Err(e.to_string()),
        }
    }

    /// The `rhai = "<path>"` arm of [`Self::resolve_mount`]: read the script, hash it, refuse an
    /// override the script never asks for, compile it at [`vike_core::LiveBroker`] — the SAME
    /// broker every registry strategy mounts at — and say so at INFO.
    ///
    /// Two of the four rails `docs/decisions/0024-rhai-strategies-live.md` names live HERE; the
    /// other two need no code in this crate at all (the mandatory live risk budget is
    /// `vike_mount`'s pre-connect `require_live_risk_budget`, strategy-agnostic by construction,
    /// and the HALT/panic safe-state is `vike-core`'s per-dispatch `catch_unwind` — a script
    /// strategy rides both unchanged because it enters the core as an opaque
    /// `Box<dyn Strategy<LiveBroker>>` like every other strategy):
    ///
    /// * **The audit line.** The INFO log below carries the script PATH and the sha256 of the
    ///   source that was ACTUALLY compiled — the journal answer to "which code traded", which a
    ///   path alone cannot give (the file can be edited between restarts). [`script_sha256`] is
    ///   the pure half; [`MountedStrategy::Script`] carries both values so a test asserts them
    ///   without a log subscriber.
    /// * **The engine surface.** [`vike_script::RhaiStrategy::compile_with_params`] builds its own
    ///   resource-limited engine through `vike-script`'s ONE `build_engine` — the same construction
    ///   the backtest's `rhai` arm uses, verbatim. This daemon registers NO host function of its
    ///   own, so what a script may call live is byte-identical to what it may call in a backtest.
    ///
    /// The unknown-override refusal is the daemon's no-silently-ignored-key posture in the script
    /// vocabulary, and it is deliberately keyed on `vike_script::discover_params` — the script's
    /// own one-time TOP-LEVEL run. The declared residual: a `param()` call made only INSIDE a hook
    /// body is invisible to that run, so its override key would be refused here with the message
    /// below. That is the right side of the trade: binding params at top level
    /// (`const SIZE = param("size", 1.0);`) is the documented idiom (`vike-script`'s `param`
    /// registration says so — the one-time run is what BAKES the value in), and refusing loudly
    /// with the fix in the message beats silently accepting a key that may configure nothing.
    fn resolve_script(path: &str, s: &StrategyCfg) -> Result<MountedStrategy, String> {
        let source =
            std::fs::read_to_string(path).map_err(|e| format!("read rhai script {path}: {e}"))?;
        let sha256 = script_sha256(&source);
        let declared = vike_script::discover_params(&source)
            .map_err(|e| format!("rhai script {path} failed to compile: {e}"))?;
        let overrides = script_overrides(&s.params);
        let unknown: Vec<&str> = overrides
            .keys()
            .filter(|k| !declared.iter().any(|(name, _)| name == *k))
            .map(String::as_str)
            .collect();
        if !unknown.is_empty() {
            return Err(format!(
                "rhai script {path} never asks for these `[strategy.params]` keys: {}. An \
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
        let strategy = vike_script::RhaiStrategy::<vike_core::LiveBroker>::compile_with_params(
            &source, overrides,
        )
        .map_err(|e| format!("rhai script {path} failed to compile: {e}"))?;
        // The audit trail: WHICH code trades. INFO, once, at resolve — which `main` runs
        // immediately before either mount builder, so this line sits directly above the mount
        // announcement in the journal, paper and live alike.
        tracing::info!(
            script = %path,
            sha256 = %sha256,
            "mounting a Rhai script strategy — this hash is the source that was compiled"
        );
        Ok(MountedStrategy::Script { strategy: Box::new(strategy), path: path.to_string(), sha256 })
    }

    /// [`Self::resolve_mount`] boxed for the two mount builders — what `main` calls. Boxing is the
    /// only thing this adds; the routing, and the fact that it is OBSERVABLE, live one frame up.
    pub fn resolve_strategy(
        &self,
        cfg: &MakerMountConfig,
    ) -> Result<Box<dyn vike_model::Strategy<vike_core::LiveBroker> + Send>, String> {
        Ok(match self.resolve_mount(cfg)? {
            MountedStrategy::AsMaker(maker) => maker,
            MountedStrategy::Registered(strategy) => strategy,
            MountedStrategy::Script { strategy, .. } => strategy,
        })
    }

    /// The name of the strategy this profile mounts — the registry name, `"rhai"` for a
    /// `rhai = "<path>"` script (the PATH is in the resolve's own audit line, not here), or
    /// `"spread_maker"` for the absent-`[strategy]` default (which mounts exactly that strategy).
    pub fn strategy_name(&self) -> &str {
        match &self.strategy {
            None => "spread_maker",
            // A validated `[strategy]` table carries `name` XOR `rhai`, so `name`-absent IS the
            // script arm; every construction path goes through `validate` (the `mount_symbol`
            // contract).
            Some(s) => s.name.as_deref().unwrap_or("rhai"),
        }
    }

    /// The EFFECTIVE parameters of the mounted strategy, as one log-safe line.
    ///
    /// ⚠ Not decoration. Until this existed the daemon logged `strategy = <name>` and NOTHING about
    /// what it was configured with, so an operator could not tell — then or afterwards, from the
    /// journal — which numbers were actually mounted. That is half of why a silently-dropped params
    /// key was so hard to see: the other half (`DaemonProfile::validate_strategy`) makes it impossible, and
    /// this makes it diagnosable.
    ///
    /// **Both paths report what was RESOLVED, never what was typed.** For the A-S maker that is the
    /// knobs the mount actually built (which the profile may not state — the venue-selected price
    /// domain and variance/horizon modes come from [`Self::to_mount_config`]); for a registry
    /// strategy it is `vike_strategy::resolved_params`, which re-runs the SAME pure `from_params`
    /// the mount used and reports every knob it landed on.
    ///
    /// ⚠ It echoed the RAW `[strategy.params]` table for a whole round, and that was worse than
    /// logging nothing: `size = "2"` printed `size="2"` while the strategy ran `size = 1`, so the
    /// one diagnostic added to make the mount visible affirmatively misreported it. This repo's own
    /// settings-consumption rule is the authority on why — a declared-but-unread key "hands the
    /// operator positive confirmation of something false", and `Policy::max_total_exposure` was
    /// deleted for exactly that. `DaemonProfile::validate_strategy` now refuses that particular
    /// input, but a type check only ever covers the inputs it rejects: a reader may still CLAMP
    /// (`read_rungs` floors `rungs = -5` at `0`), fall back on an unrecognised string
    /// (`side = "shrot"` mounts LONG), or supply a default the profile never mentions (a
    /// controller harness's
    /// `venue = "sim"`). Echoing the resolution is what makes ALL of them visible, including the
    /// ones nobody has thought of yet.
    ///
    /// ⚠ **Read this line for what it is: it says what each knob RESOLVED TO, and it does not claim
    /// any knob is in force.** Whether a resolved value is CONSUMED can depend on the other knobs
    /// and on the market — `anchor_price` on a grid whose `anchor` is unset resolves to exactly the
    /// number the operator typed and is then read by nobody
    /// (`crates/vike-strategy/src/strategies/grid_dca.rs`'s `anchor_at` looks at it only in the
    /// `AnchorMode::Fixed` arm). ⚠ The most extreme case of that shape no longer reaches this line:
    /// a `grid`/`dca_accumulate` whose ladder rests nothing read none of its own knobs at all, and
    /// `vike_strategy::unarmable_params` refuses that table at load rather than letting it mount and
    /// echo a configuration nothing runs. Two rounds annotated the subset of that a predicate over the params
    /// table can reach, and `vike_strategy::PARAM_GATES`' own doc records why each round's
    /// annotation turned out to be a fresh false claim and why the whole marking was deleted rather
    /// than repaired again. Said once, here, is the whole of it.
    ///
    /// ## ⚠ The maker arm reports the knobs that DECIDE THE POSTED WIDTH, which it once did not
    ///
    /// It formatted nine fields and not one of the four that set the quoted half-spread —
    /// `min_half_spread_ticks`, `max_half_spread_ticks`, `kappa_default`, `tau_hold_ms` — so an
    /// operator reading the startup line could see `gamma` and the price domain while the two numbers
    /// that actually bound `δ` were invisible. It also printed `half_spread`, which is DEAD on this
    /// daemon: A-S always prices here, so that field is only the fixed-spread seed
    /// `SpreadMaker::new` takes and nothing consumes it (`MakerMountConfig::crypto`'s own comment says
    /// "Unused while A-S prices"). Logging a dead knob beside the live ones is the declared-but-unread
    /// failure this method's doc opens with, so it is GONE rather than annotated.
    ///
    /// `round_trip_fee_rate` is here because it is the one knob whose `None` an operator must be able
    /// to see: `None` means NO break-even floor is armed — "nobody could name this venue's maker fee
    /// as a fraction of price" — and it is NOT the same statement as `Some(0.0)`, a measured zero-fee
    /// venue. Read it beside `max_half_spread_ticks`: when `½ · rate · mid` exceeds
    /// `max_half_spread_ticks · tick_size` the maker posts NOTHING, by design
    /// (`vike_mm::avellaneda::bounded_half_spread`), and these are the numbers that say so.
    pub fn effective_params(&self, cfg: &MakerMountConfig) -> String {
        if self.mounted_maker(cfg).is_some() {
            let p = &cfg.as_params;
            return format!(
                "qty={} tick_size={} min_half_spread_ticks={} max_half_spread_ticks={} \
                 round_trip_fee_rate={:?} kappa_default={} tau_hold_ms={} price_domain={:?} \
                 variance_mode={:?} horizon_mode={:?} spread_model={:?} gamma={} \
                 resolution_ts={:?}",
                cfg.qty,
                cfg.tick_size,
                p.min_half_spread_ticks,
                p.max_half_spread_ticks,
                p.round_trip_fee_rate,
                p.kappa_default,
                p.tau_hold_ms,
                p.price_domain,
                p.variance_mode,
                p.horizon_mode,
                if self.strategy_name() == "gueant_maker" {
                    SpreadModel::Gueant
                } else {
                    p.spread_model
                },
                p.gamma,
                p.resolution_ts,
            );
        }
        let Some(s) = self.strategy.as_ref() else {
            // Unreachable: an absent `[strategy]` IS the maker, handled above.
            return "(none)".to_string();
        };
        // A Rhai script: report the path plus the numeric OVERRIDES the compile bakes in — a true
        // claim, because `resolve_script` refuses any override key the script's own top level
        // never passes to `param(…)`, so every key printed here WAS asked for and DID land. Knobs
        // the profile does not override run at the script's own `param` defaults, which live in
        // the source the path names — and the resolve's sha256 audit line pins WHICH source.
        if let Some(path) = s.rhai.as_deref() {
            let overrides = script_overrides(&s.params);
            return if overrides.is_empty() {
                format!("script={path} (no overrides — every param() runs its script default)")
            } else {
                let knobs: Vec<String> =
                    overrides.iter().map(|(k, v)| format!("{k}={v}")).collect();
                format!("script={path} {}", knobs.join(" "))
            };
        }
        let name = s.name.as_deref().unwrap_or_default();
        match vike_strategy::resolved_params(name, &s.params) {
            Some(rows) => {
                rows.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ")
            }
            // A USER strategy (compiled from user_data): its reader is the operator's own code,
            // not instrumented by `resolved_params` — say so, and do NOT echo the raw table (the
            // misreport this method exists to have stopped applies with extra force to a reader
            // nobody in-tree has audited).
            None if vike_user_strategies::USER_STRATEGIES.contains(&name) => format!(
                "(user strategy {name:?} — its own reader is not instrumented, so nothing here \
                 can state what it resolved)"
            ),
            // A name that declines to enumerate its knobs. On this daemon that is only the two
            // maker aliases, which took the branch above — so say SO rather than falling back to
            // echoing the raw table, which is the misreport this method exists to have stopped.
            None => format!(
                "(unreported — {name:?} enumerates no knobs, so nothing here can state what it \
                 resolved)"
            ),
        }
    }

    /// The strategy-free mount projection both generic builders take — this profile's
    /// [`MakerMountConfig`] lowering, minus the A-S knobs. One derivation, so a `[strategy]` mount
    /// and the default A-S mount can never disagree about venue/symbol/interval/seed_cash or the
    /// paper fee model.
    pub fn to_mount_spec(&self) -> MountSpec {
        let mut spec = self.to_mount_config().mount_spec();
        // …plus the ACCOUNT, which `MakerMountConfig` has no field for and should not grow one:
        // that type is the A-S maker's own knobs, and WHICH ACCOUNT a mount trades on is a fact
        // about the mount rather than about the maker (`vike_mount::MountSpec::account` is where it
        // belongs, and every other strategy this daemon can mount reaches it through the same
        // spec). Set here rather than threaded through `to_mount_config` so a maker built for a
        // paper rehearsal is byte-identical.
        spec.account = self.account.clone();
        spec
    }

    /// Lower into the [`vike_mount::MakerMountConfig`] the paper mount is built from: start from the
    /// recommended defaults for this venue's PRICE DOMAIN, then override ONLY the fields this profile
    /// set. Feature-free — no A-S internals are exposed here (operator A-S tuning is a follow-up);
    /// the recommended `AsParams` (with `resolution_ts`) applies.
    ///
    /// The domain choice is `[0,1]` for polymarket and `$`-scale for every other venue — see the
    /// comment on the branch, which is the one thing in this function that decides whether the
    /// mounted maker quotes at all.
    pub fn to_mount_config(&self) -> MakerMountConfig {
        // ⚠ POLYMARKET IS THE EXCEPTION, NOT THE RULE — and the test in that direction is what this
        // condition asserts. `::outcome_token` tunes Avellaneda–Stoikov for `[0,1]` OUTCOME-TOKEN prices
        // (a Bernoulli variance cap and a `[tick, 1−tick]` wall clamp); `::crypto` tunes it for an
        // UNBOUNDED `$`-priced asset (Unbounded price domain + RawLocal variance + ConstantTau horizon
        // + a min-half-spread floor). Those are the only two price domains this daemon mounts, and a
        // `$`-scale asset priced through the `[0,1]` one does not quote AT ALL — every quote is wall-
        // clamped away from a $65k mid, so the maker mounts cleanly, logs a healthy feed, and posts
        // ZERO orders forever.
        //
        // ⚠ It USED to read `if self.venue == "hyperliquid"`, with polymarket as the catch-all. That
        // was correct only while hyperliquid was the one live-wired `$`-scale venue: binance, bybit
        // and okx are all `$`-scale, all now live-wired, and every one of them would have fallen into
        // the `[0,1]` arm and silently never quoted. Keying on the ONE venue that genuinely is a
        // `[0,1]` market makes the next `$`-scale venue correct by default instead of silently mute —
        // the failure mode this repo calls a silent do-nothing, and the direction the wrong default
        // must point.
        let mut cfg = if self.venue() == "polymarket" {
            MakerMountConfig::outcome_token(
                self.venue(),
                self.mount_symbol(),
                self.resolution_ts_ms,
            )
        } else {
            MakerMountConfig::crypto(
                self.venue(),
                self.mount_symbol(),
                self.tick_size.unwrap_or(1.0),
                self.qty.unwrap_or(0.005),
            )
        };
        if let Some(interval) = &self.interval {
            cfg.interval = interval.clone();
        }
        if let Some(ms) = self.interval_ms {
            cfg.interval_ms = ms;
        }
        if let Some(qty) = self.qty {
            cfg.qty = qty;
        }
        if let Some(half_spread) = self.half_spread {
            cfg.half_spread = half_spread;
        }
        if let Some(tick_size) = self.tick_size {
            cfg.tick_size = tick_size;
        }
        if let Some(seed_cash) = self.seed_cash {
            cfg.seed_cash = seed_cash;
        }
        cfg
    }
}
