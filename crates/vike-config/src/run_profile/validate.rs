//! Loading a `RunProfile` from TOML text, and its semantic validation.

use vike_model::ProfileError;

use super::schema::{RunProfile, is_frac_unit, is_pos};

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
                     the mount is paper or real. This profile's `[risk]`/`[guards]`/`[sinks]` \
                     tables are unchanged"
                ));
            }
        }

        // --- the RETIRED `sinks.journal.dir`, refused BY NAME -----------------------------------
        //
        // The same reason and the same place as the two tables above, one key down
        // (`ProfileJournal::dir`'s tombstone): `deny_unknown_fields` alone would say "unknown
        // field `dir`", which reads as a typo to the operator whose profile journalled yesterday.
        if self.sinks.journal.as_ref().is_some_and(|j| j.dir.is_some()) {
            return bail(
                "`sinks.journal.dir` is no longer part of a run profile: the command journal's \
                 directory is the `config.journal_dir` settings row ALONE (decision 0111, \
                 amended 2026-10-11), and the journal is on exactly when that row is set — so \
                 the trade daemon that writes the journal and the data daemon that follows it \
                 read one directory. Set it with `vike-cli config set config.journal_dir \
                 <dir>`, then re-run `vike-cli config bootstrap-run <name> …` WITHOUT \
                 `--sinks.journal.dir`. `[sinks.journal]` keeps only the tuning keys \
                 (`segment_bytes`, `flush_every`, `snapshot_every`)"
                    .into(),
            );
        }

        // --- the four RETIRED keys no mount ever applied, refused BY NAME -----------------------
        //
        // The same reason and the same place as `sinks.journal.dir` above (`Sinks::gui`,
        // `Sinks::recorder`, `Sinks::raw_capture_dir` and `Guards::freshness_ms` carry the
        // tombstones): `deny_unknown_fields` alone would say "unknown field `gui`", which reads as
        // a typo to the operator whose stored body carried it for months and "worked". Presence
        // refuses, whatever the value (`false` included): a stored row is the thing being
        // retired, and a body that still holds one is rewritten without it.
        for (key, present, what) in [
            (
                "guards.freshness_ms",
                self.guards.freshness_ms.is_some(),
                "it named a data-freshness window that belongs to each live subscription, which no \
                 mount ever handed it",
            ),
            (
                "sinks.gui",
                self.sinks.gui.is_some(),
                "it asked for a GUI snapshot to be published, and no mount ever read it",
            ),
            (
                "sinks.recorder",
                self.sinks.recorder.is_some(),
                "it asked for the live feed to be recorded, and no mount ever read it \
                 (recording is a recorder profile, `vike-cli config bootstrap-recorder`, run \
                 inside the datahub — the store's one writer)",
            ),
            (
                "sinks.raw_capture_dir",
                self.sinks.raw_capture_dir.is_some(),
                "it named a raw-frame capture directory, and no mount ever opened a tap on it",
            ),
        ] {
            if present {
                return bail(format!(
                    "`{key}` is no longer part of a run profile: it was accepted, validated and \
                     stored, and then applied by NOTHING — {what} — so the row only read as if \
                     it did something. Re-run `vike-cli config bootstrap-run <name> …` WITHOUT \
                     `--{key}` (that verb REPLACES the body stored under a name, so a key left \
                     off is gone), then restart the daemon. Nothing else in the profile changes"
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
        // ⚠ A load-time refusal of the venue-owned instrument fields (`tick_size`/`lot_size`/
        // `min_qty`/`min_notional`) for a `mode = "live"` profile stood HERE and is DELETED with
        // `mode` (decision 0156 item 7). A profile states no tier, so at load nothing says whether
        // the grid will be the venue's; the refusal is made where that IS known — at the mount of
        // each `demo`/`live` account, by `ProfileRisk::apply_to` under the venue-fetched grid
        // `vike_model::GridSource::for_tier` gives such an account. A `paper` account fetches no
        // grid and takes the profile's.

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
        if let Some(mc) = &g.margin_call {
            if !is_frac_unit(mc.mm_requirement) {
                return bail(format!(
                    "`guards.margin_call.mm_requirement` must be in (0.0, 1.0] (got {})",
                    mc.mm_requirement
                ));
            }
            // An absent key took `MarginCallConfig::default`'s value at parse, which is in range,
            // so judging it is the same as skipping it.
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
        // it compared one key no mount applied against another. `sinks.recorder` is itself retired
        // now (the tombstone check above refuses it by name), which is a stronger and truer answer
        // than a cross-check between two keys neither of which reached a runner. Re-deriving the
        // rule from the since-deleted `mode` was available and refused: `mode = "paper"` said
        // nothing about whether the feed is live, so a mode-based rule would have been a DIFFERENT
        // rule wearing the old one's name.
        if let Some(j) = &self.sinks.journal {
            if j.snapshot_every == Some(0) {
                // 0 would snapshot+flush on EVERY record — a silent p99 cliff (CoreThread asserts this).
                return bail("`sinks.journal.snapshot_every` must be >= 1".into());
            }
            if j.segment_bytes == Some(0) {
                return bail("`sinks.journal.segment_bytes` must be > 0".into());
            }
            if j.flush_every == Some(0) {
                return bail("`sinks.journal.flush_every` must be > 0".into());
            }
        }

        Ok(())
    }
}
