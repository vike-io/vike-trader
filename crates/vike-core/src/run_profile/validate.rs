//! Loading a `RunProfile` from TOML text, and its semantic validation.

use vike_model::ProfileError;

use super::schema::{Mode, RunProfile, is_frac_unit, is_pos};

impl RunProfile {
    /// Parse a profile from a TOML string, then [`validate`](Self::validate) it.
    ///
    /// The one loader. The text is the rendered body of a `run` settings row
    /// (`vike_secrets::profile_store::render_run_toml`); there is no file loader, because no
    /// binary reads a run-profile file (decision 0111).
    pub fn from_toml_str(s: &str) -> Result<Self, ProfileError> {
        let profile: RunProfile =
            toml::from_str(s).map_err(|e| ProfileError::Parse(e.to_string()))?;
        profile.validate()?;
        Ok(profile)
    }

    /// Reject syntactically-valid but semantically-nonsensical profiles with a clear message.
    pub fn validate(&self) -> Result<(), ProfileError> {
        let bail = |m: String| Err(ProfileError::Validation(m));

        // --- the two DELETED tables, refused BY NAME --------------------------------------------
        //
        // ⚠ FIRST, before anything this profile actually configures. `deny_unknown_fields` would
        // refuse these two on its own the moment the fields went, and the refusal it produces says
        // only "unknown field" — which reads as a typo to the one operator who is not making one.
        // [`RunProfile::event_source`] and [`RunProfile::broker`] carry the argument; this is the
        // sentence they produce.
        for (table, present) in
            [("event_source", self.event_source.is_some()), ("broker", self.broker.is_some())]
        {
            if present {
                return bail(format!(
                    "`[{table}]` is no longer part of a run profile — it was REQUIRED by this \
                     schema, validated on load and documented in the shipped template, and NO \
                     RUNNER read it, in either binary. Keeping it would have handed you positive \
                     confirmation of something false: an `event_source`/`broker` you filled in \
                     truthfully never selected a feed or an execution target. Delete the whole \
                     `[{table}]` table. What actually decides those: the daemon's `[[mounts]]` \
                     rows pick the venue, symbol and account, and the live gate decides whether \
                     the mount is paper or real. This profile's `mode` and `[risk]`/`[guards]`/\
                     `[sinks]` tables are unchanged"
                ));
            }
        }

        // --- risk limits ------------------------------------------------------------------------
        let r = &self.risk;
        for (name, v) in [
            ("tick_size", r.tick_size),
            ("lot_size", r.lot_size),
            ("min_notional", r.min_notional),
            ("max_notional_per_order", r.max_notional_per_order),
            ("max_total_exposure", r.max_total_exposure),
        ] {
            if let Some(v) = v
                && !is_pos(v)
            {
                return bail(format!("`risk.{name}` must be finite and > 0 (got {v})"));
            }
        }
        // `max_leverage` is the ONE operator-facing leverage knob (issue #822) and it is ENFORCED:
        // `ProfileRisk::im_requirement` converts it to the initial-margin fraction the pre-trade
        // buying-power check reads (`im = 1/lev`). So this bound is no longer cosmetic — `0.0`
        // would divide by zero and a negative would mean unbounded buying power.
        if let Some(lev) = r.max_leverage
            && (!lev.is_finite() || lev < 1.0)
        {
            return bail(format!(
                "`risk.max_leverage` must be finite and >= 1.0 (got {lev}) — 1.0 is no \
                     leverage, 10.0 is 10x; it arms the buying-power check at an initial-margin \
                     requirement of 1/max_leverage"
            ));
        }
        if !(0.0..1.0).contains(&r.required_free_bp_pct) {
            return bail(format!(
                "`risk.required_free_bp_pct` must be in [0.0, 1.0) (got {})",
                r.required_free_bp_pct
            ));
        }
        if let Some(n) = r.max_orders_per_window {
            if n == 0 {
                return bail("`risk.max_orders_per_window` must be >= 1 (omit to disable)".into());
            }
            if r.window_ms <= 0 {
                return bail(
                    "`risk.window_ms` must be > 0 when `max_orders_per_window` is set".into(),
                );
            }
        }
        if r.window_ms < 0 {
            return bail(format!("`risk.window_ms` must be >= 0 (got {})", r.window_ms));
        }
        // STRUCTURAL load-time gate, unconditional for `live` mode — no escape hatch (see the
        // module doc's "`GridSource` is derived from `mode`, not a second flag" section): a
        // `mode = "live"` profile that sets ANY venue-owned instrument field is a config error at
        // load, before any venue connection is attempted. `live` always fetches a real grid at
        // mount (`RunProfile::grid_source` -> `GridSource::VenueFetched`), so the profile can never
        // be a sound source for these fields — there is no flag that makes it sound, unlike
        // `backtest`/`paper`, which never fetch a grid and so may freely set them (enforced only at
        // mount by `ProfileRisk::apply_to`'s semantic `GridSource` check, not needed here).
        if self.mode == Mode::Live {
            let offending = r.venue_owned_fields_set();
            if !offending.is_empty() {
                return bail(format!(
                    "`mode = \"live\"` profile's `[risk]` sets venue-owned instrument field(s) [{}] \
                     — a live mount always fetches the real venue grid, so the venue owns \
                     tick_size/lot_size/min_qty/min_notional and a profile may never set them, at \
                     load or at mount. Remove {} from `[risk]` (only `backtest`/`paper` profiles may \
                     supply the instrument grid themselves).",
                    offending.join(", "),
                    if offending.len() == 1 { "it" } else { "them" }
                ));
            }
        }

        // --- guards -----------------------------------------------------------------------------
        let g = &self.guards;
        if let Some(ms) = g.submit_ack_timeout_ms
            && ms == 0
        {
            return bail("`guards.submit_ack_timeout_ms` must be > 0 (omit to disable)".into());
        }
        // ⚠ A second `if` arm stood inside this one and went with `[broker]`, and what it was
        // protecting is written here so the next reader does not conclude the protection was lost.
        // It required a positive `broker.seed_cash` whenever `max_drawdown` was set: the latch
        // measures a FRACTION of `Σ seed_cash + own PnL` (`CoreThread::sweep_drawdown_latch`), so a
        // non-positive seed leaves it with no denominator and it can never arm — a profile that
        // reads as protected and silently is not. That refusal was made against a number NO RUNNER
        // ever carried into `CoreConfig::seed_cash`: the table was read by nothing.
        //
        // The check that matters survives, at the place the seed is actually read —
        // `vike_tradehub::config::MountCfg`'s `seed_cash`, whose own refusal names the same 25%
        // latch and the same denominator. So what is deleted here is a load-time check over an
        // input with no consumer, not the guard.
        if let Some(dd) = g.max_drawdown
            && !is_frac_unit(dd)
        {
            return bail(format!(
                "`guards.max_drawdown` must be in (0.0, 1.0] (omit to disable, got {dd})"
            ));
        }
        if let Some(ms) = g.freshness_ms
            && ms == 0
        {
            return bail("`guards.freshness_ms` must be > 0 (omit to disable)".into());
        }
        if let Some(mc) = &g.margin_call {
            if !is_frac_unit(mc.mm_requirement) {
                return bail(format!(
                    "`guards.margin_call.mm_requirement` must be in (0.0, 1.0] (got {})",
                    mc.mm_requirement
                ));
            }
            if !is_frac_unit(mc.warn_fraction) {
                return bail(format!(
                    "`guards.margin_call.warn_fraction` must be in (0.0, 1.0] (got {})",
                    mc.warn_fraction
                ));
            }
            if !mc.buffer.is_finite() || mc.buffer < 0.0 {
                return bail(format!(
                    "`guards.margin_call.buffer` must be finite and >= 0 (got {})",
                    mc.buffer
                ));
            }
        }

        // --- sinks ------------------------------------------------------------------------------
        // ⚠ A `sinks.recorder` needs-a-live-feed check stood HERE and went with `[event_source]`:
        // it compared one UNWIRED key against another. `sinks.recorder` is itself on this type's
        // own unwired list ([`RunProfile::guards_report`] pushes it), so the operator who sets it
        // is already told by NAME that it arms nothing — which is a stronger and truer answer than
        // a cross-check between two keys neither of which reaches a runner. Re-deriving the rule
        // from `mode` was available and refused: `mode = "paper"` says nothing about whether the
        // feed is live, so a mode-based rule would be a DIFFERENT rule wearing the old one's name.
        if let Some(j) = &self.sinks.journal {
            if j.dir.trim().is_empty() {
                return bail("`sinks.journal.dir` must not be empty".into());
            }
            if j.snapshot_every == 0 {
                // 0 would snapshot+flush on EVERY record — a silent p99 cliff (CoreThread asserts this).
                return bail("`sinks.journal.snapshot_every` must be >= 1".into());
            }
            if j.segment_bytes == 0 {
                return bail("`sinks.journal.segment_bytes` must be > 0".into());
            }
            if j.flush_every == 0 {
                return bail("`sinks.journal.flush_every` must be > 0".into());
            }
        }

        Ok(())
    }
}
