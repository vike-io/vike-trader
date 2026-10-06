//! HARD LOWER BOUND (a) on the confirm grace, and `apply_guards_and_sinks`, which reports a breach.

use std::time::Duration;

use super::schema::{ProfileTradingState, RunProfile};

/// The adapter's per-re-query REST timeout, in seconds
/// (`crates/vike-bridge-core/src/http.rs`'s `blocking_agent_with_timeout`). Restated here rather
/// than imported because `vike-core` sits BELOW `vike-bridge-core` and cannot name that constant in
/// code; the citation is the link, and
/// `the_warning_predicate_agrees_with_the_bound_the_shipped_pairing_is_gated_on` holds this pair
/// equal to the arithmetic the pin test spells for itself.
const CONFIRM_REQUERY_SECS: u64 = 5;

/// Sequential re-queries the WORST-CASE confirm makes: `N = 2` for Bybit (realtime → history),
/// `N = 1` for OKX. The worst case is the right one here, deliberately: this warning is computed
/// once at startup, before any venue is mounted, and cannot know which venues an operator will arm.
const CONFIRM_REQUERY_HOPS: u64 = 2;

/// The right-hand side of HARD LOWER BOUND (a): `submit_ack_timeout/2 + N·requery`. The first term
/// is the watchdog's tick jitter (the timer fires every `submit_ack_timeout/2`, so the active
/// confirm can be issued up to one tick late), the second the confirm's own worst-case round trip.
pub(crate) fn confirm_budget(timeout: Duration) -> Duration {
    timeout / 2 + Duration::from_secs(CONFIRM_REQUERY_SECS * CONFIRM_REQUERY_HOPS)
}

/// HARD LOWER BOUND (a) on [`crate::CoreConfig::submit_ack_confirm_grace`] —
/// `2·grace > submit_ack_timeout/2 + N·requery` — evaluated on ONE pair. `true` = safe.
///
/// The bound is stated in prose on that field and argued again at the live root's
/// `submit_ack_timeout` literal (`crates/vike-tradehub/src/tradehub_cli.rs`); this is the one place
/// it is COMPUTED, so a run profile can no longer invalidate that argument in silence.
///
/// ⚠ This said "both live roots" and named the GUI shell as the second. There is ONE live root now:
/// the desktop cut took the local trading core out of `crates/vike-desktop/src/main.rs` (then
/// spelled `vike-app`), which builds no `CoreConfig` and arms no watchdog. The bound is unchanged —
/// it was never a property of how many roots stated it.
pub(crate) fn confirm_grace_clears_bound(timeout: Duration, grace: Duration) -> bool {
    grace * 2 > confirm_budget(timeout)
}

/// A confirm-grace pairing that BREAKS HARD LOWER BOUND (a), reported by
/// [`RunProfile::apply_guards_and_sinks`] so its caller can say so out loud.
///
/// ⚠ **The residual this closes, and why it could only be closed here.** #1569 stopped
/// `apply_guards_and_sinks` from rewriting the grace to a stale 5 s whenever `submit_ack_timeout_ms`
/// was named, and declared what it deliberately left: *a profile that RAISES
/// `submit_ack_timeout_ms` without raising the grace still breaks bound (a), silently. Nothing
/// validates the pair.* [`RunProfile::validate`] cannot: it sees only the profile, so it could check
/// nothing but the case where BOTH keys are named — the case least in need of help, since an
/// operator who wrote both numbers was at least looking at both. `apply_guards_and_sinks` takes the
/// caller's [`crate::CoreConfig`], which is where the OTHER half of every pairing lives (both live
/// roots arm `submit_ack_timeout: Some(30s)` in their own literals and leave the grace at
/// `CoreConfig::default`'s), so it is the first and only place both numbers are in one hand.
///
/// ⚠ **It WARNS; it does not refuse.** `docs/decisions/0013-degrade-vs-refuse.md` decides this, and
/// its four questions are worth answering explicitly rather than by analogy:
///
/// 1. *Protection or capability?* Neither, exactly — nothing here is unarmed. The ladder IS armed;
///    it is TUNED to a value that narrows its safety margin. A refusal is the answer to a protection
///    an operator believes is on and is not, and that is not this.
/// 2. *Did the operator ask for it?* They asked for a timeout and got the timeout — there is no
///    set-but-unhonoured key, and so no false belief for a refusal to correct.
/// 3. *Does it redirect authority?* No. The failure mode is a LOCAL synthesized `OrderRejected`
///    against an order the venue may in fact hold; the watchdog calls no venue, so the damage is
///    local-state divergence that reconcile repairs, not a venue write.
/// 4. *Is it visible where the operator already looks?* Yes — the same startup line, from the same
///    call, as the still-unwired disclosure both roots already print.
///
/// And the decisive one, which is not on that list: `N` is the WORST-CASE venue's hop count, so the
/// bound is deliberately conservative and a mount that will only ever touch OKX (`N = 1`) can clear
/// the real bound while failing this one. A refusal computed from a conservative heuristic would
/// refuse correct configurations — and would refuse them at the startup of a daemon that was running
/// yesterday, which `docs/decisions/0013-degrade-vs-refuse.md`'s "What would reopen this" names as
/// the case where refusing is worse than the misconfiguration it prevents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfirmGraceHazard {
    /// Stage 1 as the mount ENDED UP with it — the binary's literal, or the profile's
    /// `guards.submit_ack_timeout_ms` where it named one.
    pub submit_ack_timeout: Duration,
    /// Stage 2, likewise: `CoreConfig::default`'s, or the profile's
    /// `guards.submit_ack_confirm_grace_ms` where it named one.
    pub confirm_grace: Duration,
    /// `submit_ack_timeout/2 + N·requery` — the quantity `2·grace` had to exceed and did not.
    pub confirm_budget: Duration,
}

impl ConfirmGraceHazard {
    /// The smallest whole-millisecond `guards.submit_ack_confirm_grace_ms` that CLEARS the bound at
    /// this timeout — the paste-ready number the warning hands the operator.
    ///
    /// `⌊budget/2⌋` in milliseconds, plus one: `Duration::as_millis` truncates, so `m + 1` ms is
    /// strictly greater than `budget/2` whatever sub-millisecond remainder it dropped, and
    /// `2·grace > budget ⟺ grace > ⌊budget/2⌋` over the integer nanoseconds a `Duration` is.
    /// `the_advised_minimum_grace_actually_clears_the_bound` drives that claim through the real
    /// predicate rather than restating it.
    pub fn min_grace_ms(&self) -> u128 {
        (self.confirm_budget / 2).as_millis() + 1
    }
}

impl std::fmt::Display for ConfirmGraceHazard {
    /// The operator-facing sentence, owned HERE so the two live roots cannot drift about the
    /// arithmetic the way the prose copies of this bound already have once.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "run profile: the stuck-order watchdog's confirm-grace pairing breaks HARD LOWER \
             BOUND (a) — submit_ack_timeout={:?} leaves submit_ack_confirm_grace={:?}, but \
             2·grace ({:?}) must EXCEED submit_ack_timeout/2 + N·requery ({:?}). The last-resort \
             synthesized OrderRejected can fire while the adapter's own re-query is still in \
             flight: a PHANTOM REJECT of an order the venue may actually hold. Set \
             guards.submit_ack_confirm_grace_ms = {} or higher (or lower \
             guards.submit_ack_timeout_ms). Starting anyway — this is a tuning warning, not a \
             refusal; see `vike_core::ConfirmGraceHazard` for why it is not one.",
            self.submit_ack_timeout,
            self.confirm_grace,
            self.confirm_grace * 2,
            self.confirm_budget,
            self.min_grace_ms(),
        )
    }
}

/// What [`RunProfile::apply_guards_and_sinks`] found, for a caller to disclose.
///
/// It RETURNS rather than logs, which is the shape that function already had and the shape this
/// crate owes its callers: `vike-core` is a library, the two live roots word their own disclosure
/// differently on purpose (`in this mount` / `in this daemon`), and a returned value is what lets a
/// test assert the ABSENCE of a warning — which is half of what this report is gated on, and which a
/// `tracing` assertion cannot do reliably (the subscriber's `Interest` cache is process-global).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GuardsReport {
    /// `[guards]`/`[sinks]` keys that are SET and still reach no [`crate::CoreConfig`] field, named
    /// per key. Empty on an ordinary start.
    pub unwired: Vec<&'static str>,
    /// The confirm-grace pairing, when the FINAL pair breaks HARD LOWER BOUND (a). `None` — the
    /// ordinary case, including every shipped default — means the pairing is safe or the watchdog is
    /// disarmed entirely.
    pub confirm_grace: Option<ConfirmGraceHazard>,
}

impl RunProfile {
    /// Apply every `[guards]` / `[sinks]` key that has a [`crate::CoreConfig`] counterpart, and
    /// return the keys that were SET and still reach nothing — so a caller can say so out loud
    /// instead of the operator finding out by watching a guard not fire.
    ///
    /// ⚠ **This is the wiring that was missing for the profile's whole life.** `[risk]` was wired
    /// in #816; `[guards]` and the rest of `[sinks]` were left behind and were, from that day,
    /// inert in every binary. What both composition roots did with `[guards]` instead was
    /// `if p.guards != Guards::default() { warn!("… set but NOT consumed …") }` — a settings key
    /// that nothing reads, announced as such, which is precisely the state this workspace deleted
    /// `Policy::max_total_exposure` for. Five of the seven guards map 1:1 onto `CoreConfig` fields
    /// that already existed and whose converters already returned the right types, so the wiring
    /// was field assignment; it was simply never done.
    ///
    /// ⚠ The caller passes a `CoreConfig` it has ALREADY built, and this only overwrites the fields
    /// the profile actually names — the `Option` guards are skipped when `None`, so a profile that
    /// declares no guard cannot silently reset a knob the binary set for its own reasons. The two
    /// non-`Option` fields (`conditionals_on_ticks`, and `equity_sample` when the profile sets it)
    /// are applied unconditionally, which is what "the profile is the auditable authority" means
    /// for a boolean that has no "unset".
    ///
    /// ⚠ **CONFIRM-GRACE: EVERY key is gated on ITS OWN presence, `submit_ack_confirm_grace_ms`
    /// included.** That reads like a restatement of the paragraph above and is written out because
    /// this one field did NOT obey it, for the whole life of this function, on a live order path.
    /// It was gated on `submit_ack_timeout_ms.is_some()` instead — "the grace is only meaningful
    /// alongside stage 1" — and both halves of that were wrong:
    ///
    /// - A profile arming `submit_ack_timeout_ms` and saying NOTHING about the grace still WROTE a
    ///   grace: `Guards`' own `DEFAULT_CONFIRM_GRACE_MS`, 5 s, over `CoreConfig::default`'s 15 s.
    ///   Silence was being answered with a number, and the number had rotted (the confirm-race
    ///   hardening bumped `CoreConfig`'s to 15 s and left the copy here at 5 s). The result broke
    ///   HARD LOWER BOUND (a) on [`crate::CoreConfig::submit_ack_confirm_grace`] against the 30 s
    ///   timeout the live root arms — `2·5s = 10s`, needed `> 30/2 + 2·5 = 25s` for a two-hop
    ///   (Bybit) confirm — i.e. the last-resort synthesized `OrderRejected` could fire while the
    ///   adapter's own re-query was still in flight: a PHANTOM REJECT of a live order, from a
    ///   profile that never mentioned the grace. `crates/vike-tradehub/src/tradehub_cli.rs`'s
    ///   `submit_ack_timeout` literal argues that pairing at its call site; a profile could
    ///   silently invalidate the argument.
    /// - Conversely, a profile naming ONLY `submit_ack_confirm_grace_ms` was silently DROPPED —
    ///   the same declared-but-inert defect this whole function exists to end. "Only meaningful
    ///   alongside stage 1" confuses the PROFILE's stage 1 with the CORE's: `vike-tradehub` sets
    ///   `submit_ack_timeout: Some(30s)` in its own `CoreConfig` literal, so the watchdog is armed
    ///   whether or not the profile mentions it.
    ///
    /// ⚠ Both bullets said "BOTH live roots" and named the GUI shell as the second. That was true
    /// when they were written and is not now: the desktop cut took the local trading core out of
    /// `crates/vike-desktop/src/main.rs` (then spelled `vike-app`), so `vike-tradehub` is the only
    /// root that builds a `CoreConfig` or arms this watchdog. Neither the defect nor the arithmetic
    /// moves — one root stating the pairing is still one place a profile can invalidate it.
    ///
    /// So: named ⇒ applied, unnamed ⇒ untouched, in both directions and independently per key.
    /// `the_confirm_grace_is_written_only_when_the_operator_names_it` pins all four combinations
    /// and `the_shipped_confirm_grace_pairing_satisfies_the_lower_bound` pins the arithmetic.
    ///
    /// ⚠ **AND THE PAIR IS NOW CHECKED, which is the residual that fix declared.** Gating each key
    /// on its own presence fixes the direction where an unnamed grace was overwritten; it does
    /// nothing about the other one — a profile that RAISES `submit_ack_timeout_ms` and says nothing
    /// about the grace raises the bound the untouched grace must clear, and can walk it under bound
    /// (a) without naming the key. [`RunProfile::validate`] cannot see that, because the other half
    /// of the pair is the CALLER's `CoreConfig`. This function holds both numbers, so it evaluates
    /// the bound on the FINAL pair — after every assignment above — and reports a breach in
    /// [`GuardsReport::confirm_grace`]. It WARNS rather than refusing, and
    /// [`ConfirmGraceHazard`] carries that argument against
    /// `docs/decisions/0013-degrade-vs-refuse.md`'s four questions rather than asserting it.
    ///
    /// Evaluating the FINAL pair rather than the profile's DELTA is deliberate: the hazard is a
    /// property of the two numbers a mount runs with, not of which of them an operator typed, and a
    /// check keyed on "did the profile raise the timeout" would miss a profile that LOWERS the grace
    /// under a timeout it never mentioned. The declared residual of that choice is the mirror one: a
    /// binary whose OWN literals broke the bound would only be told when a profile is present at
    /// all, since nothing calls this function without one —
    /// `the_shipped_confirm_grace_pairing_satisfies_the_lower_bound` is what covers the shipped
    /// literals, and it covers them at merge time rather than at startup, which is better.
    ///
    /// The returned report's `unwired` is the STILL-UNWIRED set, named per key. Two guards remain,
    /// and neither is an oversight left unstated:
    ///
    /// - `guards.initial_trading_state` — `CoreConfig` carries no trading-state field at all; the
    ///   state is set on the ENGINE after assembly, so wiring it is new surface rather than an
    ///   assignment. It is a real operator gap (`docs/ops/kill-switches.md` §6: "You cannot start
    ///   the headless daemon halted from its profile").
    /// - `guards.freshness_ms` — the threshold belongs to a per-subscription `FreshnessTracker` in
    ///   the live feed loops, which no `CoreConfig` reaches.
    ///
    /// `every_guard_and_sink_field_is_wired_or_declared` is the gate that keeps this list honest:
    /// it destructures both structs exhaustively, so a NEW field cannot be added without being
    /// classified here.
    pub fn apply_guards_and_sinks(&self, cfg: &mut crate::CoreConfig) -> GuardsReport {
        if let Some(t) = self.guards.submit_ack_timeout() {
            cfg.submit_ack_timeout = Some(t);
        }
        // ⚠ GATED ON THE GRACE'S OWN KEY, never on stage 1's — see the ⚠ CONFIRM-GRACE paragraph
        // in this function's doc for the live-order hazard the old gate carried.
        if let Some(g) = self.guards.submit_ack_confirm_grace() {
            cfg.submit_ack_confirm_grace = g;
        }
        if let Some(dd) = self.guards.max_drawdown {
            cfg.max_drawdown = Some(dd);
        }
        cfg.conditionals_on_ticks = self.guards.conditionals_on_ticks;
        if let Some(mc) = self.guards.margin_call_config() {
            cfg.margin_call = Some(mc);
        }
        if let Some(every) = self.sinks.equity_sample() {
            cfg.equity_sample = Some(every);
        }

        let mut unwired: Vec<&'static str> = Vec::new();
        if self.guards.initial_trading_state != ProfileTradingState::default() {
            unwired.push("guards.initial_trading_state");
        }
        if self.guards.freshness_ms.is_some() {
            unwired.push("guards.freshness_ms");
        }
        if self.sinks.gui {
            unwired.push("sinks.gui");
        }
        if self.sinks.recorder {
            unwired.push("sinks.recorder");
        }
        if self.sinks.raw_capture_dir.is_some() {
            unwired.push("sinks.raw_capture_dir");
        }

        // ⚠ THE PAIR CHECK — on `cfg`, AFTER every assignment above, so what is judged is the pair
        // the mount will actually run with rather than the half this profile happened to name. A
        // `None` timeout is not a breach: stage 1 is disarmed, no watchdog thread is spawned, and
        // the grace is inert (`CoreConfig::submit_ack_confirm_grace`: "Ignored when
        // `submit_ack_timeout` is `None`") — warning there would be the noise this file's other
        // tests exist to prevent.
        let grace = cfg.submit_ack_confirm_grace;
        let confirm_grace = match cfg.submit_ack_timeout {
            Some(timeout) if !confirm_grace_clears_bound(timeout, grace) => {
                Some(ConfirmGraceHazard {
                    submit_ack_timeout: timeout,
                    confirm_grace: grace,
                    confirm_budget: confirm_budget(timeout),
                })
            }
            _ => None,
        };

        GuardsReport { unwired, confirm_grace }
    }
}
