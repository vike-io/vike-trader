//! The two dead-man switches the LIVE mount arms — pure folds from `policy` onto the core's config.

use std::time::Duration;

#[cfg(doc)]
use super::warn_deadman_absent;
use crate::VenuePlan;

// -------------------------------------------------------------------------------------------
// The dead-man switches the LIVE mount arms — pure folds from `policy` onto the core's config
// -------------------------------------------------------------------------------------------

/// The file spelling of the dead-man action → the core's own type.
///
/// An extension trait rather than a `From` impl because BOTH types are foreign here (the orphan
/// rule), and here rather than in `vike-config` because that crate sits below `vike-core` and must
/// not depend on it — this binary is the crate that depends on both, so the mapping is its job
/// (`vike_config::DeadManActionSetting`'s doc says the same from the other side). The `match` is
/// exhaustive on the file side, so a variant added there is a compile error here rather than a
/// spelling that silently maps to the other action.
pub(crate) trait DeadManActionToCore {
    fn to_core(self) -> vike_core::DeadManAction;
}

impl DeadManActionToCore for vike_config::DeadManActionSetting {
    fn to_core(self) -> vike_core::DeadManAction {
        match self {
            vike_config::DeadManActionSetting::CancelAllAndHalt => {
                vike_core::DeadManAction::CancelAllAndHalt
            }
            vike_config::DeadManActionSetting::CancelAll => vike_core::DeadManAction::CancelAll,
        }
    }
}

/// The dead-man switch the LIVE mount arms, folded from `policy.deadman_timeout_ms` and
/// `policy.deadman_action` — the whole of
/// M4's wiring surface, as one pure function so the fold can be pinned without racing a timer.
///
/// `None` when the key is ABSENT (`Policy::deadman_timeout_ms` is `None`) and when the operator
/// wrote `deadman_timeout_ms = 0` (`Some(vike_config::DEADMAN_DISABLED_MS)`) — the two are OFF
/// alike, and only the WARNING beside this call ([`deadman_absent_warning`]) tells them apart.
/// `vike_core::CoreConfig::deadman` being `None` is what makes the core build no `DeadMan`, arm no
/// sweep timer and touch nothing on the fold — the switch's "off" is the ABSENCE of the config,
/// not a zero inside it (the core would clamp a zero timeout to 1 ms and trip on the first quiet
/// millisecond). Every other value arms it at exactly that many milliseconds; the file edge
/// (`vike_config::Policy::apply`) has already refused the sub-second and multi-day values, so
/// nothing is clamped here either. ⚠ For one morning `None` was unreachable from this function:
/// the key was a `u64` defaulting to 60 s, and this doc said "`None` exactly when the operator
/// wrote 0". The key's own doc (`vike_config::Policy::deadman_timeout_ms`) records why that was
/// reversed the same day — the switch observes SILENCE, not the connection, so an armed default
/// halted every session-bounded venue at every close.
///
/// `halt_file` is `vike_bridge_core::halt::halt_path_from_env()` — resolved by THIS binary, not by
/// `vike-core` (env reads stay in binaries, and `vike-core` deliberately carries no
/// `vike-bridge-core` edge). It is the SAME sentinel a manual `touch HALT` writes and every venue's
/// `ExecActor` submit boundary checks, so the automatic trip and the operator's hand reach one
/// file; the resolver is memoized, so the path the switch will write is the path the mount's own
/// halt report already printed.
///
/// ⚠ Called from `live_mount_with` ONLY. The daemon's `paper_mount` arm does not read these keys,
/// by ruling — see `vike_config::Policy::deadman_timeout_ms` for why a rehearsal that halts itself
/// over a quiet minute is not wanted. ⚠ That is the live GATE, not exec actually arming: the call
/// site runs before `build_node` decides per venue, so a live-gate run whose every exec is still
/// paper (no `[venues]` table, every venue capped `paper`, `data_only`) IS armed and a trip there
/// writes the process's real HALT sentinel. The key's doc states that shape and flags the
/// alternative (keying on `vike_mount::armed_live_venues`) as an owner decision not taken here.
/// `vike-app`'s live mount did not construct it either, nor did `crates/vike-run/src/bin/ibkr_mount.rs`
/// (deleted 2026-09-28); `docs/ops/kill-switches.md` carries those residuals.
pub(crate) fn deadman_config_from_policy(
    policy: &vike_config::Policy,
) -> Option<vike_core::DeadManConfig> {
    let timeout_ms = match policy.deadman_timeout_ms {
        None | Some(vike_config::DEADMAN_DISABLED_MS) => return None,
        Some(ms) => ms,
    };
    Some(vike_core::DeadManConfig {
        timeout: Duration::from_millis(timeout_ms),
        action: policy.deadman_action.to_core(),
        halt_file: Some(vike_bridge_core::halt::halt_path_from_env()),
    })
}

/// **Does THIS daemon subscribe a lane that can disclose a dead link for the venue?** — the third
/// fact the link dead-man's arming decision needs, and the one neither the `policy` rows nor
/// `vike_model::link_deadman_default` can answer.
///
/// ⚠ **Why it exists at all: the per-venue table answers a question about the ADAPTER, and this
/// daemon does not subscribe every lane an adapter has.** `link_deadman_default` says a venue's
/// bridge discloses a disconnect and cites the emitters; on binance/aster/bybit/okx there are two,
/// both on a BOOK socket, and this daemon subscribes exactly one of them — the HFT quote/trade/book
/// tick pump, never `subscribe_depth` (whose `l2_snapshot` verb is a default no-op in every sink
/// this daemon owns — [`crate::feeds`]'s `VenuePlan::Cex` arm says so in its own comment). So this
/// fold is what keeps the report honest in BOTH directions: without it the mount once logged
/// `LINK DEAD-MAN ARMED for binance` while nothing could reach the latch — a false claim about an
/// automatic stop, the exact class `docs/ops/kill-switches.md` opens by warning about — and a
/// venue whose only emitter this daemon stopped subscribing would silently go back to that state.
///
/// One arm per [`VenuePlan`] variant, each read from the feed wiring it describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MountLinkDisclosure {
    /// This mount subscribes a lane that hands `StreamStatus` to a sink the core reads, so a link
    /// death on it reaches `vike_core`'s latch. `lane` names the subscription for the startup line.
    Discloses { lane: &'static str },
    /// It does not — so however the table classifies the venue, the switch cannot fire for it in
    /// THIS daemon. `why` is told to the operator verbatim.
    Silent { why: &'static str },
}

/// The per-plan reading behind [`MountLinkDisclosure`]. Pure and total over [`VenuePlan`], so a new
/// variant has to answer the question before it compiles.
pub(crate) fn mount_link_disclosure(plan: &VenuePlan) -> MountLinkDisclosure {
    match plan {
        // ⚠ The CEX arm subscribes TWO lanes and exactly ONE of them discloses. The kline `Feeds`
        // lane does not and never has: it rides `vike_bridge_core::market_pump`, which carries no
        // link disclosure at all (`crates/bridges/binance/src/family/trades.rs`'s module doc states
        // it for the trade lane in the same words). The quote/trade/book PUMP does: it owns no
        // `LiveDataSink` — it takes a bare `vike_exec::TickSender` — and pushes a
        // `vike_model::FeedStatus` straight onto it, which is the same `Ingest::StreamStatus` arm a
        // sink-side disclosure reaches. That is the lane this arm rests on, and it is the venue's
        // BOOK socket rather than a side channel: the same `@depth`-class stream `depth_main`
        // reads, through the other seam.
        //
        // ⚠ **`subscribe_depth` is still NOT the answer here and must not be "added for
        // completeness".** It would open a SECOND socket to the SAME stream and maintain a SECOND
        // copy of the same book, to obtain a signal the pump already has — and deliver it into
        // `l2_snapshot`, a default no-op in every sink this daemon owns. See the pump's own
        // `disclose_link` for the argument.
        VenuePlan::Cex { .. } => MountLinkDisclosure::Discloses {
            lane: "the quote/trade/book tick pump (crates/bridges/binance/src/family/depth.rs's \
                   md_main and its bybit/okx twins), which discloses its transport state straight \
                   onto the core tick lane — one Disconnected per outage, one Live on the first \
                   frame back. NOT the kline lane, which discloses nothing, and NOT the DOM depth \
                   feed, which this daemon does not subscribe",
        },
        // The one arm that DOES: `subscribe_book` on every seated token, onto a `vike_mount::
        // MakerSink` whose `stream_status` forwards to its inner `vike_core::CoreLaneSink`.
        #[cfg(feature = "polymarket")]
        VenuePlan::Polymarket => MountLinkDisclosure::Discloses {
            lane: "subscribe_book on every seated token, onto a MakerSink that forwards \
                   stream_status to its inner CoreLaneSink",
        },
        // Both of these reach a `CoreLaneSink` directly and their feeds DO emit GapStart — so the
        // mount is not what stops them; `link_deadman_default` is (both are SessionBounded).
        VenuePlan::Oanda(_) => MountLinkDisclosure::Discloses {
            lane: "the pricing-stream + candles-poll lanes onto a CoreLaneSink",
        },
        VenuePlan::Ig(_) => MountLinkDisclosure::Discloses {
            lane: "the Lightstreamer quote/candle subscriptions onto a CoreLaneSink",
        },
        // The rest are `LinkDeadMan::Inert` in the table for the same underlying reason — their
        // bridges call `LiveDataSink::stream_status` nowhere — so the two facts agree and the
        // table's row is the one an operator is shown.
        VenuePlan::Hyperliquid(_)
        | VenuePlan::Alpaca(_)
        | VenuePlan::Ctrader(_)
        | VenuePlan::Deribit => MountLinkDisclosure::Silent {
            why: "this venue's bridge calls LiveDataSink::stream_status nowhere, so no lane this \
                  daemon could subscribe would disclose a dead link",
        },
    }
}

/// **The LINK dead-man the live mount arms** (M13) — the fold from `policy.link_deadman_grace_ms`, the
/// per-venue table, the venues this daemon actually mounts AND the lanes it subscribed for each,
/// as one pure function so the whole decision can be pinned without racing a timer.
///
/// `None` — the switch NOT constructed — in exactly two cases, and they are different facts:
/// the operator wrote `link_deadman_grace_ms = 0` (`vike_config::Policy::
/// link_deadman_grace_ms_effective` resolves that to `None`, and an ABSENT key to the armed
/// default), or NOT ONE mounted venue both is armed by `vike_model::link_deadman_default` AND has
/// a disclosing lane in this mount ([`mount_link_disclosure`]). The second is the ordinary state
/// of an FX-only daemon; it was ALSO the state of a CEX-only one until the tick pumps grew a
/// disclosure (2026-09-06), which is the reach gap `docs/decisions/0038-the-dead-man-observes-the-
/// connection-not-silence.md` recorded and its "what would reopen this" clause named.
/// [`link_deadman_arming_report`] is what tells the operator which of the three happened, per
/// venue.
///
/// ⚠ **An empty venue set is never constructed as `Some`.** A `LinkDeadManConfig` whose `venues`
/// is empty would arm a timer, contribute a waker cadence and forfeit journal replay to watch
/// nothing — the "a mechanism exists" claim `docs/ops/kill-switches.md` opens by warning about,
/// wearing a config struct.
///
/// `action` is `deadman_action`, SHARED with the silence switch: what a trip DOES is the same
/// question for both, and a second key would be a second answer to it. `halt_file` is this
/// binary's own resolved sentinel, exactly as [`deadman_config_from_policy`] resolves it, so the
/// two switches and an operator's hand all reach ONE file.
///
/// ⚠ Called from `live_mount_with` ONLY — the daemon's `paper_mount` arm constructs neither
/// switch, by the same ruling and for the same reason (`vike_config::Policy::deadman_timeout_ms`
/// argues it once). ⚠ And that is the live GATE, not exec actually arming: a live-gate run whose
/// every exec is still paper carries this switch too, and a trip there writes the process's real
/// HALT sentinel — the shape the sibling's doc states and this one inherits unchanged.
pub(crate) fn link_deadman_config_from_policy(
    policy: &vike_config::Policy,
    mounted_venues: &[(String, MountLinkDisclosure)],
) -> Option<vike_core::LinkDeadManConfig> {
    let grace_ms = policy.link_deadman_grace_ms_effective()?;
    let venues: std::collections::BTreeSet<String> = mounted_venues
        .iter()
        .filter(|(v, disclosure)| {
            // BOTH halves, and the second is not a formality: an armed venue whose lanes disclose
            // nothing here would put a name in this set that the latch can never hear about, which
            // is the "a mechanism exists" claim wearing a config struct.
            vike_model::link_deadman_default(v).is_on()
                && matches!(disclosure, MountLinkDisclosure::Discloses { .. })
        })
        .map(|(v, _)| v.clone())
        .collect();
    if venues.is_empty() {
        return None;
    }
    Some(vike_core::LinkDeadManConfig {
        grace: Duration::from_millis(grace_ms),
        action: policy.deadman_action.to_core(),
        venues,
        halt_file: Some(vike_bridge_core::halt::halt_path_from_env()),
    })
}

/// **One line per mounted venue**, saying whether the link dead-man armed there and why — returned
/// rather than logged so the DECISION is testable with no subscriber (the
/// [`deadman_absent_warning`] / `venue_arming_migration_message` idiom).
///
/// Why per venue and not one summary line: the FOUR ways a venue ends up unarmed are not the same
/// fact and an operator must be able to tell them apart. A venue can be off because its market has
/// SESSIONS (`vike_model::LinkDeadMan::SessionBounded` — the conservative default, flippable by an
/// observation), because its bridge discloses no disconnect at all
/// (`vike_model::LinkDeadMan::Inert` — a declared residual nothing can fix from the `policy` rows),
/// because THIS MOUNT subscribes no lane that would carry one ([`MountLinkDisclosure::Silent`] —
/// a fact about the daemon rather than the venue, and the only one of the four a code change here
/// could fix), or because the operator wrote `link_deadman_grace_ms = 0`. A summary saying "3 of 5
/// venues armed" hides all four behind a count.
///
/// ⚠ **The mount-lane case is checked BEFORE the armed line is printed, and that ordering is the
/// whole fix.** The first version of this function printed `LINK DEAD-MAN ARMED for binance …
/// cancels this venue's resting orders` on a daemon where nothing could ever reach the latch for
/// binance, because it consulted the venue table alone. A false promise about an automatic stop is
/// worse than no line at all.
pub(crate) fn link_deadman_arming_report(
    policy: &vike_config::Policy,
    mounted_venues: &[(String, MountLinkDisclosure)],
) -> Vec<String> {
    let grace = policy.link_deadman_grace_ms_effective();
    // ⚠ The SAME class as [`deadman_absent_warning`] below, found by the sweep that fixed it: the
    // `(None, _, _)` arm told an operator to DELETE A LINE from a file nothing reads any more.
    // `vike_config::remedy` renders the write instruction; `vike_config::DEFAULT_LINK_DEADMAN_GRACE_MS`
    // is the value it has to name outright, because writing the default back is how one "deletes" a
    // row (there is no `config unset`).
    let grace_remedy = vike_config::SettingsSection::Policy.write_remedy("link_deadman_grace_ms");
    mounted_venues
        .iter()
        .map(|(venue, disclosure)| {
            let row = vike_model::link_deadman_default(venue);
            match (grace, row.off_reason(), disclosure) {
                (None, _, _) => format!(
                    "LINK DEAD-MAN off for {venue}: `link_deadman_grace_ms = 0` in {} turns it \
                     off for EVERY venue. {} to restore the default grace",
                    grace_remedy.holder(),
                    grace_remedy
                        .unset_clause(&vike_config::DEFAULT_LINK_DEADMAN_GRACE_MS.to_string())
                ),
                (Some(_), Some(why), _) => format!(
                    "LINK DEAD-MAN off for {venue}: {why}. This venue has NO automatic stop for a \
                     dead link — `vike_model::link_deadman_default` is the row, and it says what \
                     would change it"
                ),
                (Some(_), None, MountLinkDisclosure::Silent { why }) => format!(
                    "LINK DEAD-MAN off for {venue}: the venue's bridge DOES report a dead link, \
                     but THIS DAEMON subscribes no lane that carries it — {why}. So there is no \
                     automatic stop for a dead {venue} link here, whatever the venue table says"
                ),
                (Some(ms), None, MountLinkDisclosure::Discloses { lane }) => format!(
                    "LINK DEAD-MAN ARMED for {venue} at {ms} ms over {lane}: a feed-reported \
                     disconnect lasting longer than that cancels this venue's resting orders \
                     (action: {})",
                    policy.deadman_action.as_str()
                ),
            }
        })
        .collect()
}

/// **The absent-key warning** — the message the live mount emits ONCE when there is no
/// `policy.deadman_timeout_ms` row, so that a switch which is off by omission is at least off in
/// the log. `None` for `Some(0)` (the operator decided, and wrote it) and for `Some(n)` (armed).
///
/// # Why a WARNING and not a default, and why not silence either
///
/// The switch this key arms observes SILENCE across the whole core's ingest, not the connection
/// (`vike_config::Policy::deadman_timeout_ms` carries the derivation and the morning's reversed
/// ruling), so a compiled-in default halts every session-bounded venue at every close — which is
/// why there is none. But a key nobody has heard of is not a decision: a 24/7 crypto mount that
/// WOULD want the switch gets the same silence as an FX mount that would be harmed by it, and the
/// daemon cannot tell them apart. So the mount says, once, that the switch is off, what it would
/// do, and the one line that arms it — and `deadman_timeout_ms = 0` is the spelling that says
/// "I read this and decided", which is the ONLY thing that silences it. The shape is
/// `crates/vike-mount/src/paper_fallback.rs`'s `venue_arming_migration_message`: name the file,
/// name the key, name what was refused (here: nothing is armed), and end with a paste-ready block
/// the operator CHOOSES rather than copies — the recommendation is
/// `vike_config::RECOMMENDED_DEADMAN_TIMEOUT_MS` and it is wrong for a venue that closes, so the
/// text says so beside the number.
///
/// Returns the message rather than logging it, so the decision is testable with no subscriber
/// (the `venue_arming_migration_message` idiom); [`warn_deadman_absent`] is the `Once` latch over
/// it.
///
/// ⚠ **The headline read `DEAD-MAN SWITCH IS OFF` and the body read "this live mount has NO
/// automatic stop".** Both became FALSE the day the CONNECTION-state dead-man shipped ARMED BY
/// DEFAULT (M13 — [`link_deadman_config_from_policy`] above): an operator reading the old text
/// would conclude they were unprotected, and would then either write a key that halts their FX
/// mount every Friday or trust a warning that no longer described their daemon. The message names
/// WHICH switch is off, says the other one is on, and points at the per-venue LINK DEAD-MAN lines
/// emitted beside it. `crates/vike-tradehub/tests/deadman_absent_warning.rs` asserts the retired
/// sentence cannot come back.
///
/// ⚠ **"The other one is on" is BRANCHED on, not asserted.** This function holds the very
/// [`vike_config::Policy`] that may carry `link_deadman_grace_ms = 0`, and the first rewrite
/// stated the sibling was armed unconditionally — telling the one operator who had turned BOTH
/// switches off that they had a protection they had explicitly removed. On that box the message
/// says so instead, and names the line that did it.
pub fn deadman_absent_warning(policy: &vike_config::Policy) -> Option<String> {
    if policy.deadman_timeout_ms.is_some() {
        return None;
    }
    // ⚠ **THE REMEDY NAMES THE ONE STORE THERE IS** — rendered by `vike_config::remedy`, never
    // spelled here, so it cannot send an operator to write somewhere nothing reads
    // (`docs/decisions/0086`: one store, one remedy, on every box).
    let remedy = vike_config::SettingsSection::Policy.write_remedy("deadman_timeout_ms");
    // ⚠ BRANCH on the OTHER switch's effective state rather than asserting it. This function holds
    // the very `Policy` that may say `link_deadman_grace_ms = 0`, and on such a box the
    // unconditional sentence told the operator a switch was armed while every per-venue line below
    // said it was off for that exact reason.
    let sibling: String = match policy.link_deadman_grace_ms_effective() {
        Some(_) => "\
         ⚠ Do not read that as \"no automatic stop\": the CONNECTION-state dead-man is armed by \
         default and is a DIFFERENT key. It cancels a venue's resting orders when that venue's \
         own feed reports the link DISCONNECTED for longer than `link_deadman_grace_ms`, and it \
         stays quiet through a market that merely closed — which is why it may have an armed \
         default and this key may not.\n\n\
         ⚠ Armed by default is not armed EVERYWHERE, and this message will not guess for you: \
         where it arms is a per-venue question (every FX and equities venue is off because its \
         market has sessions, several venues disclose no disconnect at all, and a venue whose \
         only emitter is a lane this daemon does not subscribe is off too). The per-venue LINK \
         DEAD-MAN lines beside this message are the answer for THIS mount — read those, not this \
         paragraph. This key is the optional EXTRA for a 24/7 mount that also wants a silence \
         detector."
            .to_string(),
        None => {
            // ⚠ The SIBLING's way back is the same class as this message's own remedy, rendered
            // through the same `vike_config::remedy` so the two halves of one warning can never
            // disagree about how to write it.
            let grace = vike_config::SettingsSection::Policy.write_remedy("link_deadman_grace_ms");
            format!(
                "\
         ⚠ AND NEITHER IS THE OTHER ONE: you wrote `link_deadman_grace_ms = 0`, which turns the \
         CONNECTION-state dead-man off for EVERY venue. So this mount has NO automatic stop of \
         any kind — not for a dead socket and not for a quiet feed. {} to restore the \
         connection-state switch at its default grace; this key stays a separate decision.",
                grace.unset_clause(&vike_config::DEFAULT_LINK_DEADMAN_GRACE_MS.to_string())
            )
        }
    };
    Some(format!(
        "THE SILENCE DEAD-MAN IS OFF: {}, so this live mount has no automatic stop for \"the feed \
         went QUIET and orders are still resting\". It is off by OMISSION, not by decision — \
         nothing arms it unless you write the key, and this line is the only place that says \
         so.\n\n\
         {sibling}\n\n\
         What THIS key would add: after that many milliseconds with no venue event, tick, bar, \
         quote, trade or book update on ANY venue or symbol this daemon follows, cancel every \
         resting order and (with the default `deadman_action`) engage HALT — `Halted` on every \
         engine plus the HALT sentinel file, which an operator must delete before the next \
         submit.\n\n\
         ⚠ It observes SILENCE, not the connection. On a venue whose market CLOSES (FX over the \
         weekend, equities every evening) or on a thin instrument in a quiet minute it trips with \
         no outage at all. Arm it on a 24/7 mount only.\n\n\
         To arm it for a venue that never closes, {} \
         (milliseconds; {} is the recommendation for a 24/7 venue, raise it for a thin one):\n\n\
         {}\n\n\
         To record that you decided AGAINST it, and silence this message:\n\n\
         {}\n",
        remedy.absent_clause(),
        remedy.write_clause(),
        vike_config::RECOMMENDED_DEADMAN_TIMEOUT_MS,
        remedy.write_line(&vike_config::RECOMMENDED_DEADMAN_TIMEOUT_MS.to_string()),
        remedy.write_line("0"),
    ))
}
