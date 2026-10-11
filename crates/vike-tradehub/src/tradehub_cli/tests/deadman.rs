//! The dead-man policy fold (M4) and the LINK dead-man (M13).

use super::*;
use std::assert_matches;

// ---------------------------------------------------------------------------------------------
// The dead-man switch (M4) — the policy key REACHES `CoreConfig`. The trip behaviour itself is
// `crates/vike-core/src/runtime/tests/deadman.rs`'s job (`trips_after_timeout_and_cancels_all_
// open_orders`, `cancel_all_and_halt_engages_the_sentinel`); these pin only the fold from the
// file's two keys to the config the live mount hands the core, which is the half that did not
// exist before. Asserting the config is far more robust than racing a timer, and it is the
// whole of what this binary adds. The file→`Policy` half is `vike-config`'s own; `Policy`'s
// fields are public, so the variants are spelled as struct updates rather than round-tripped
// through a temp file that would prove the loader a second time.
// ---------------------------------------------------------------------------------------------

/// **The re-ruling, pinned at the seam:** a live mount that says NOTHING arms NO dead-man.
/// This test was `the_default_policy_arms_the_deadman_at_sixty_seconds_halting_on_the_
/// process_sentinel` for one morning and asserted the opposite; `vike_config::Policy::
/// deadman_timeout_ms` records why the ruling reversed (the switch observes silence, so an
/// armed default halted every session-bounded venue at every close). The absent key and the
/// explicit zero reach the core IDENTICALLY — `None`, no `DeadMan`, no timer — and are told
/// apart only by the warning, which the sibling test below pins.
#[test]
fn a_policy_that_says_nothing_arms_no_deadman() {
    assert!(
        deadman_config_from_policy(&vike_config::Policy::default()).is_none(),
        "an ABSENT key is OFF — nothing arms the silence-detector unless an operator writes it"
    );
}

/// The recommended line, WRITTEN, arms the switch at sixty seconds, halting, writing the
/// process's ONE HALT sentinel. Each of the four is asserted separately, because each is a
/// distinct way to be wrong — `None` (not armed), a different number (armed at the wrong
/// silence), `CancelAll` (trips and lets the strategy re-quote into a market it has not seen),
/// and a `None` halt file (engages the in-process gate but leaves the venue adapters'
/// cross-process check un-tripped).
#[test]
fn the_recommended_line_arms_the_deadman_at_sixty_seconds_halting_on_the_process_sentinel() {
    let policy = vike_config::Policy {
        deadman_timeout_ms: Some(vike_config::RECOMMENDED_DEADMAN_TIMEOUT_MS),
        ..vike_config::Policy::default()
    };
    let cfg = deadman_config_from_policy(&policy).expect("a written key ARMS the dead-man");
    assert_eq!(cfg.timeout, Duration::from_secs(60));
    assert_eq!(cfg.action, vike_core::DeadManAction::CancelAllAndHalt);
    assert!(cfg.action.engages_halt(), "the default action must engage HALT, not only cancel");
    // The resolver is memoized process-wide, so this is the path every venue's submit boundary
    // in this process checks and the one `touch HALT` in the runbook writes.
    assert_eq!(
        cfg.halt_file,
        Some(vike_bridge_core::halt::halt_path_from_env()),
        "the automatic trip and the manual kill switch must write ONE file"
    );
}

/// `deadman_timeout_ms = 0` is the EXPLICIT-off spelling, and it disables by ABSENCE: the core
/// gets `None`, builds no `DeadMan` and arms no timer — not a config with a zero inside it,
/// which the core would clamp to 1 ms and trip on the first quiet millisecond.
#[test]
fn deadman_timeout_zero_disarms_the_switch_by_absence() {
    let policy = vike_config::Policy {
        deadman_timeout_ms: Some(vike_config::DEADMAN_DISABLED_MS),
        ..vike_config::Policy::default()
    };
    assert!(deadman_config_from_policy(&policy).is_none(), "0 means OFF, and OFF means None");
}

/// The absent-key warning's DECISION, with no subscriber: a message for `None` only — not for
/// the explicit zero (the operator's recorded decision) and not for an armed value — and the
/// message names the file, the key, what the switch would do, the recommendation as a
/// paste-ready line, the zero that silences it, and the successor. The EMISSION (once, through
/// `tracing`) is `crates/vike-tradehub/tests/deadman_absent_warning.rs`'s job, in its own
/// binary, because a global-subscriber capture cannot share a test binary with tests that
/// drive `live_mount_with` under no subscriber.
///
/// ⚠ **This used to be a PAIR — one test per `Authority` arm — because the remedy used to
/// render differently depending on whether a box's settings files still answered.**
/// `docs/decisions/0086` deletes the files arm outright: there is one store now, so there is one
/// rendering, and this test carries what both halves used to prove.
#[test]
fn the_absent_key_warning_fires_for_none_alone_and_names_what_an_operator_needs() {
    let absent = vike_config::Policy::default();
    let msg = deadman_absent_warning(&absent).expect("an absent key WARNS whichever store answers");
    assert!(msg.contains("`policy.deadman_timeout_ms`"), "names the key: {msg}");
    assert!(msg.contains("cancel every resting"), "says what it would do: {msg}");
    assert!(msg.contains("engage HALT"), "…and that the default action halts: {msg}");
    assert!(msg.contains("observes SILENCE, not the connection"), "states the cost: {msg}");
    // The remedy, both halves, in the vocabulary of the store that answered.
    assert!(
        msg.contains("vike-cli config set policy.deadman_timeout_ms 60000"),
        "the arming line must be a command this box can run: {msg}"
    );
    assert!(
        msg.contains("vike-cli config set policy.deadman_timeout_ms 0"),
        "…and so must the one that records a decision AGAINST it: {msg}"
    );
    assert!(
        msg.contains("the settings database does not carry `policy.deadman_timeout_ms`"),
        "the headline names the store: {msg}"
    );
    // ⚠ The sentence this message must NOT contain any more. It said "this live mount has NO
    // automatic stop", which became FALSE the day the connection-state switch shipped armed by
    // default (M13) — an operator reading it would disable a real protection or write a key
    // they do not need. The successor is now named as SHIPPED, not as coming.
    assert!(
        !msg.contains("NO automatic stop"),
        "the link dead-man IS an automatic stop and is on by default: {msg}"
    );
    assert!(
        msg.contains("CONNECTION-state dead-man is armed by default"),
        "names what IS armed: {msg}"
    );
    assert!(msg.contains("link_deadman_grace_ms"), "names the key that governs it: {msg}");

    let decided = vike_config::Policy {
        deadman_timeout_ms: Some(vike_config::DEADMAN_DISABLED_MS),
        ..vike_config::Policy::default()
    };
    assert_eq!(deadman_absent_warning(&decided), None, "an explicit 0 is a decision");

    let armed =
        vike_config::Policy { deadman_timeout_ms: Some(5_000), ..vike_config::Policy::default() };
    assert_eq!(deadman_absent_warning(&armed), None, "an armed switch has nothing to warn");
}

/// The SIBLING half of the same warning: `link_deadman_grace_ms = 0`'s way back is a
/// `config set` of the default, because there is no `config unset` and therefore no line to
/// delete.
#[test]
fn the_link_grace_way_back_is_a_config_set_of_the_default() {
    let both_off = vike_config::Policy {
        link_deadman_grace_ms: Some(vike_config::LINK_DEADMAN_DISABLED_MS),
        ..vike_config::Policy::default()
    };
    let msg = deadman_absent_warning(&both_off).unwrap();
    assert!(msg.contains("Run `vike-cli config set policy.link_deadman_grace_ms 120000`"), "{msg}");
    assert!(msg.contains("AND NEITHER IS THE OTHER ONE"), "{msg}");
    assert!(msg.contains("NO automatic stop of any kind"), "{msg}");
}

// --- THE LINK DEAD-MAN's fold (M13) --------------------------------------------------------
// The FOUR inputs are the policy grace, `vike_model::link_deadman_default`, the venues this
// mount has and the LANES it subscribed for each; each test below moves ONE of them.

/// A mounted venue whose lanes DO carry a disconnect, for the fold tests — the disclosure half
/// is a separate question, tested against real [`VenuePlan`]s by
/// [`the_cex_arm_subscribes_no_lane_that_could_report_a_dead_link`] below.
fn seen(venue: &str) -> (String, MountLinkDisclosure) {
    (venue.to_string(), MountLinkDisclosure::Discloses { lane: "a test lane" })
}

/// The same venue mounted over lanes that carry nothing.
fn unseen(venue: &str) -> (String, MountLinkDisclosure) {
    (venue.to_string(), MountLinkDisclosure::Silent { why: "a test lane that discloses none" })
}

/// **The default, and the whole point of M13:** a policy that says NOTHING arms the link
/// switch on the venues the table says default ON — the exact opposite of the silence switch's
/// absent-key behaviour two tests up, and deliberately so.
#[test]
fn a_policy_that_says_nothing_arms_the_link_deadman_on_the_armed_venues() {
    let policy = vike_config::Policy::default();
    let venues = [seen("binance"), seen("oanda")];
    let cfg = link_deadman_config_from_policy(&policy, &venues)
        .expect("an ABSENT key ARMS the link dead-man — that is the M13 default");
    assert_eq!(cfg.grace, Duration::from_millis(vike_config::DEFAULT_LINK_DEADMAN_GRACE_MS));
    assert_eq!(cfg.action, vike_core::DeadManAction::CancelAllAndHalt, "shares deadman_action");
    assert_eq!(
        cfg.venues,
        ["binance".to_string()].into_iter().collect::<std::collections::BTreeSet<_>>(),
        "the FX venue is NOT armed — a weekend close must not be able to trip this switch"
    );
    assert_eq!(
        cfg.halt_file,
        Some(vike_bridge_core::halt::halt_path_from_env()),
        "both switches and the operator's hand must reach ONE sentinel"
    );
}

/// `link_deadman_grace_ms = 0` disarms it by ABSENCE, the sibling's idiom: the core gets
/// `None`, builds no latch and arms no timer.
#[test]
fn a_zero_link_grace_disarms_the_switch_by_absence() {
    let policy = vike_config::Policy {
        link_deadman_grace_ms: Some(vike_config::LINK_DEADMAN_DISABLED_MS),
        ..vike_config::Policy::default()
    };
    assert!(
        link_deadman_config_from_policy(&policy, &[seen("binance")]).is_none(),
        "0 means OFF, and OFF means None"
    );
}

/// ⚠ **A mount with no ARMED venue builds nothing.** An FX-only daemon must not carry a config
/// whose venue set is empty: that would arm a timer, contribute a waker cadence and forfeit
/// journal replay in order to watch nothing, which is the "a mechanism exists" claim
/// `docs/ops/kill-switches.md` opens by warning about.
#[test]
fn an_fx_only_mount_builds_no_link_deadman_at_all() {
    let policy = vike_config::Policy::default();
    let venues = [seen("oanda"), seen("ig"), seen("ibkr")];
    assert!(
        link_deadman_config_from_policy(&policy, &venues).is_none(),
        "no mounted venue defaults ON ⇒ no config, not an empty one"
    );
}

/// ⚠ **A venue the TABLE arms but this MOUNT cannot hear about is not armed either** — the
/// second half of the fold, and the one a venue-table-only version got wrong: it put binance
/// in the config's venue set (and printed an ARMED line for it) on a daemon that subscribes no
/// lane carrying a binance disconnect.
#[test]
fn an_armed_venue_whose_lanes_disclose_nothing_here_is_not_armed() {
    let policy = vike_config::Policy::default();
    assert!(
        link_deadman_config_from_policy(&policy, &[unseen("binance"), unseen("bybit")]).is_none(),
        "no venue can reach the latch on this mount ⇒ no config at all"
    );
    let cfg = link_deadman_config_from_policy(
        &policy,
        &[unseen("binance"), seen("polymarket"), unseen("okx")],
    )
    .expect("the one venue with a disclosing lane still arms");
    assert_eq!(
        cfg.venues,
        ["polymarket".to_string()].into_iter().collect::<std::collections::BTreeSet<_>>(),
        "only the venue this mount can actually hear a disconnect from"
    );
}

#[test]
fn the_cex_arm_subscribes_only_the_bar_verb_on_the_status_bearing_handle() {
    // **The CODE half of the bybit `recon_feed_statuses` condition.**
    //
    // That row keys on `CexBars::status()` — ONE `Arc<Mutex<String>>` shared by every lane the
    // bridge spawns on that handle. `LiveFeeds::recon_feed_statuses`' doc keeps the row on the
    // measured fact that this daemon drives exactly ONE lane on it, which is why the CI box's latch
    // was total and permanent rather than thrashing between lanes. That fact was a READING of
    // today's code; this makes it a gate.
    //
    // ⚠ It gates the CODE only. The PROFILE half — one interval, a spot symbol — stays an
    // operator fact: a second interval in the mount spawns a second `feed_main` through the
    // `intervals` loop below, and a `.P` symbol spawns `mark_main`. That is why condition (1)
    // of keeping the row (every lane, `mark_main` included, having a `SessionStatus::Live` arm)
    // is not optional, and why the doc says "the daemon runs one lane on this handle, and that
    // is gated" rather than "bybit's string is unambiguous".
    const FEEDS: &str = include_str!("../../feeds/cex.rs");
    // Anchored on the kline lane's function (`wire_venue_feeds`' CEX arm calls it, then calls the
    // tick-pump function, which sits directly below it in the file).
    let arm_start = FEEDS.find("fn wire_cex_kline_lane(").expect(
        "the CEX kline lane's function head has moved — this scan is anchored on it and would \
             otherwise check nothing",
    );
    // Bound the scan at LANE 2's banner, where the tick pump (a DIFFERENT object, which never
    // touches `market_feed::Feeds::status`) takes over.
    let arm_end = FEEDS[arm_start..]
        .find("── LANE 2:")
        .expect("the CEX arm's lane-2 banner has moved — re-anchor this scan");
    let lane1 = &FEEDS[arm_start..arm_start + arm_end];
    assert!(
        lane1.contains("subscribe_bars"),
        "the CEX arm must still subscribe bars on the status-bearing handle, or this gate is \
             checking nothing"
    );
    for forbidden in ["subscribe_trades", "subscribe_depth", "subscribe_book"] {
        assert!(
            !lane1.contains(forbidden),
            "the CEX arm now calls `{forbidden}` on `CexBars` — a SECOND lane writing the one \
                 `Arc<Mutex<String>>` bybit's `recon_feed_statuses` row keys on. That row rests on \
                 this being a single-writer handle; either give every new lane a \
                 `SessionStatus::Live` arm and re-argue the row in \
                 `LiveFeeds::recon_feed_statuses`' doc, or withdraw the row."
        );
    }
}

/// ⚠ **The fact behind that filter, read off the REAL plans rather than asserted in prose.**
/// The CEX arm subscribes the kline lane and the tick pump; the kline lane discloses nothing,
/// the TICK PUMP discloses its transport state onto the core tick lane
/// (`crates/bridges/binance/src/family/depth.rs`'s `disclose_link` and its bybit/okx twins),
/// and the DOM depth lane — the venue's OTHER emitter — is deliberately not subscribed here.
/// Polymarket's `subscribe_book` reaches the core through a sink instead; deribit's bridge
/// emits nothing at all.
///
/// ⚠ **The panic arm is the point of the test and it has now flipped direction.** It used to
/// fire on `Discloses` (a depth subscription added without revisiting the decision); it fires
/// on `Silent` now, because losing the pump's disclosure would silently un-arm four venues on
/// every live daemon while `vike_model::link_deadman_default` still called them Armed —
/// exactly the false-promise state this whole fold exists to prevent, wearing the other face.
#[test]
fn the_cex_arm_subscribes_the_tick_pump_that_reports_a_dead_link() {
    for venue in [CexVenue::Binance, CexVenue::Bybit, CexVenue::Okx, CexVenue::Aster] {
        let plan = VenuePlan::Cex { venue, mainnet: true };
        match mount_link_disclosure(&plan) {
            MountLinkDisclosure::Discloses { lane } => {
                assert!(
                    lane.contains("tick pump"),
                    "the CEX arm's disclosure must name the lane it rests on: {lane}"
                );
                assert!(
                    lane.contains("NOT the kline lane"),
                    "…and must say which subscribed lane does NOT carry it, since that is the \
                         one a reader assumes: {lane}"
                );
            }
            MountLinkDisclosure::Silent { why } => panic!(
                "the CEX arm claims to disclose NOTHING ({why:?}) — if the tick pump's \
                     transport disclosure was removed or a subscription changed, then \
                     vike_model::link_deadman_default's Armed rows for binance/aster/bybit/okx are \
                     a promise this daemon cannot keep: fix the pump, or move those rows, and say \
                     so here"
            ),
        }
    }
    // …and the venue whose disclosure rides a SINK rather than the tick lane is unaffected.
    #[cfg(feature = "polymarket")]
    assert_matches!(
        mount_link_disclosure(&VenuePlan::Polymarket),
        MountLinkDisclosure::Discloses { .. }
    );
    assert_matches!(
        mount_link_disclosure(&VenuePlan::Deribit),
        MountLinkDisclosure::Silent { .. },
        "deribit's bridge calls stream_status nowhere"
    );
}

/// ⚠ **The end-to-end arming claim for the newly-covered venues, over the REAL plans and the
/// REAL table** — the join every other test here takes one leg of. A default `Policy`, a
/// CEX mount, and the four venues the table arms all end up in the config's venue set AND read
/// ARMED in the operator's startup lines. This is the test that would have been red on the day
/// the feature shipped.
#[test]
fn a_default_cex_mount_arms_the_link_deadman_and_says_so() {
    let policy = vike_config::Policy::default();
    let mounted: Vec<(String, MountLinkDisclosure)> =
        [CexVenue::Binance, CexVenue::Bybit, CexVenue::Okx, CexVenue::Aster]
            .into_iter()
            .map(|v| {
                let plan = VenuePlan::Cex { venue: v, mainnet: true };
                (v.slug().to_string(), mount_link_disclosure(&plan))
            })
            .collect();

    let cfg = link_deadman_config_from_policy(&policy, &mounted)
        .expect("a default CEX mount now builds the switch — that is the whole change");
    assert_eq!(
        cfg.venues,
        ["aster", "binance", "bybit", "okx"]
            .into_iter()
            .map(str::to_string)
            .collect::<std::collections::BTreeSet<_>>(),
        "every CEX venue the table arms is in the switch's venue set"
    );

    let lines = link_deadman_arming_report(&policy, &mounted);
    assert_eq!(lines.len(), 4, "one line per mounted venue");
    for line in &lines {
        assert!(line.contains("ARMED"), "an armed venue must READ armed: {line}");
        assert!(line.contains("120000 ms"), "…at the default grace: {line}");
        assert!(line.contains("tick pump"), "…naming the lane it rests on: {line}");
    }

    // ⚠ …and the venue-table half still refuses independently: an FX venue mounted through a
    // DISCLOSING lane stays off, because its market has sessions. Without this the test above
    // would pass on a fold that had quietly become "whatever the mount can hear".
    let with_fx = [mounted[0].clone(), seen("oanda")];
    let cfg = link_deadman_config_from_policy(&policy, &with_fx).expect("binance still arms");
    assert!(
        !cfg.venues.contains("oanda"),
        "a session-bounded venue must not arm however well this daemon hears it"
    );

    // ⚠ …and a venue whose REAL plan discloses nothing still gets its own off-reason printed
    // rather than being dropped from the report — asserted over the real `VenuePlan` rather
    // than the constructed `unseen()` the report test uses, since the whole point of that arm
    // is that it describes a real mount.
    let with_deribit =
        [mounted[0].clone(), ("deribit".to_string(), mount_link_disclosure(&VenuePlan::Deribit))];
    let lines = link_deadman_arming_report(&policy, &with_deribit);
    assert!(lines[1].contains("off for deribit"), "{}", lines[1]);
    assert!(!lines[1].contains("ARMED"), "a silent venue may not read as armed: {}", lines[1]);
    assert!(
        lines[1].contains("stream_status nowhere"),
        "…and must carry the RESIDUAL reason from its own table row, not the mount's: {}",
        lines[1]
    );
}

/// A written grace and the lighter action both arrive verbatim — the file edge has already
/// refused everything outside the bounds, so nothing is clamped here.
#[test]
fn a_written_link_grace_and_the_cancel_all_action_reach_the_core_config() {
    let policy = vike_config::Policy {
        link_deadman_grace_ms: Some(45_000),
        deadman_action: vike_config::DeadManActionSetting::CancelAll,
        ..vike_config::Policy::default()
    };
    let cfg =
        link_deadman_config_from_policy(&policy, &[seen("polymarket")]).expect("45 s arms it");
    assert_eq!(cfg.grace, Duration::from_secs(45));
    assert_eq!(cfg.action, vike_core::DeadManAction::CancelAll);
    assert!(!cfg.action.engages_halt());
}

/// The per-venue REPORT distinguishes the FOUR ways a venue can be off, by NAME — the reason
/// it is one line per venue rather than a count. Each arm is asserted on the fact an operator
/// would act on differently.
#[test]
fn the_arming_report_tells_the_four_off_reasons_apart() {
    let venues = [seen("polymarket"), seen("oanda"), seen("hyperliquid"), unseen("binance")];
    let lines = link_deadman_arming_report(&vike_config::Policy::default(), &venues);
    assert_eq!(lines.len(), 4, "one line per mounted venue");
    assert!(lines[0].contains("ARMED for polymarket at 120000 ms"), "{}", lines[0]);
    assert!(lines[1].contains("off for oanda"), "{}", lines[1]);
    assert!(lines[1].contains("weekend close"), "the SESSION reason, verbatim: {}", lines[1]);
    assert!(lines[2].contains("off for hyperliquid"), "{}", lines[2]);
    assert!(
        lines[2].contains("stream_status nowhere"),
        "the RESIDUAL reason — nothing in the policy rows can fix this one: {}",
        lines[2]
    );
    // ⚠ The MOUNT reason: the venue table arms binance, and this daemon still cannot hear it.
    // The line must not say ARMED — that was the false promise this case was added for.
    assert!(lines[3].contains("off for binance"), "{}", lines[3]);
    assert!(
        !lines[3].contains("ARMED"),
        "an unreachable venue may not read as armed: {}",
        lines[3]
    );
    assert!(
        lines[3].contains("THIS DAEMON subscribes no lane"),
        "says whose fault it is — the mount's, not the venue's: {}",
        lines[3]
    );

    // …and the operator's own off-switch is reported as ITS own reason, on every venue —
    // through the one store there is, and there is no `config unset` so the way back is a
    // `config set` of the default rather than "delete the line" (same class as the silence
    // dead-man's own remedy; `crates/vike-config/src/remedy.rs` carries the measurement).
    let off = vike_config::Policy {
        link_deadman_grace_ms: Some(vike_config::LINK_DEADMAN_DISABLED_MS),
        ..vike_config::Policy::default()
    };
    for line in link_deadman_arming_report(&off, &venues) {
        assert!(line.contains("`link_deadman_grace_ms = 0` in the settings database"), "{line}");
        assert!(
            line.contains("Run `vike-cli config set policy.link_deadman_grace_ms 120000`"),
            "{line}"
        );
        assert!(!line.contains("Delete the line"), "{line}");
        assert!(!line.contains("settings/policy.toml"), "{line}");
    }
}

/// **A run profile may not lower this ceiling either** — the twin of
/// `the_run_profile_cannot_touch_the_deadman`, and worth its own test because this switch is
/// ON by default: a `[guards]` table that could reach it would be able to DISARM a protection
/// the operator never had to ask for.
#[test]
fn the_run_profile_cannot_touch_the_link_deadman() {
    let profile = vike_core::RunProfile::from_toml_str(vike_core::run_profile::samples::LIVE_TOML)
        .expect("the shipped live sample parses");
    let mut cfg = vike_core::CoreConfig {
        submit_ack_timeout: Some(Duration::from_secs(7)),
        link_deadman: link_deadman_config_from_policy(
            &vike_config::Policy::default(),
            &[seen("binance")],
        ),
        ..vike_core::CoreConfig::default()
    };
    let _ = profile.apply_guards_and_sinks(&mut cfg);
    assert_eq!(
        cfg.submit_ack_timeout,
        Some(Duration::from_secs(30)),
        "the profile's [guards] must have been APPLIED for this test to prove anything"
    );
    let ldm = cfg.link_deadman.expect("the profile must not have disarmed the link dead-man");
    assert_eq!(
        ldm.grace,
        Duration::from_millis(vike_config::DEFAULT_LINK_DEADMAN_GRACE_MS),
        "…nor moved its grace"
    );
}

/// A configured timeout and the lighter action both arrive: the file's `"cancel_all"` becomes
/// the core's `CancelAll` (the mapping this crate owns because it is the one that sees both
/// types), and the milliseconds are carried verbatim — the file edge already refused what
/// would need clamping.
#[test]
fn a_configured_timeout_and_the_cancel_all_action_reach_the_core_config() {
    let policy = vike_config::Policy {
        deadman_timeout_ms: Some(5_000),
        deadman_action: vike_config::DeadManActionSetting::CancelAll,
        ..vike_config::Policy::default()
    };
    let cfg = deadman_config_from_policy(&policy).expect("5 s arms it");
    assert_eq!(cfg.timeout, Duration::from_secs(5));
    assert_eq!(cfg.action, vike_core::DeadManAction::CancelAll);
    assert!(!cfg.action.engages_halt(), "cancel_all pulls the book and leaves the state alone");
    // …and the mapping is total in the other direction too.
    assert_eq!(
        vike_config::DeadManActionSetting::CancelAllAndHalt.to_core(),
        vike_core::DeadManAction::CancelAllAndHalt
    );
}

/// **A run profile may not lower a policy ceiling.** `apply_guards_and_sinks` runs immediately
/// after the `CoreConfig` literal in `live_mount_with` and overwrites the guards it names; it
/// must have no way to reach `deadman`, or a `[guards]` table — a file with none of
/// the `policy` section's protections — could disarm the switch. Driven through the shipped LIVE
/// sample profile, which DOES name `submit_ack_timeout_ms`, and the assertion on that field is
/// what proves the profile was actually applied rather than ignored: a test where nothing
/// changed would be green for the wrong reason.
#[test]
fn the_run_profile_cannot_touch_the_deadman() {
    let profile = vike_core::RunProfile::from_toml_str(vike_core::run_profile::samples::LIVE_TOML)
        .expect("the shipped live sample parses");
    // A WRITTEN key: the default policy arms nothing now, and a test that started from `None`
    // could not tell "the profile disarmed it" from "it was never armed".
    let armed = vike_config::Policy {
        deadman_timeout_ms: Some(vike_config::RECOMMENDED_DEADMAN_TIMEOUT_MS),
        ..vike_config::Policy::default()
    };
    let mut cfg = vike_core::CoreConfig {
        // Deliberately NOT the sample's 30 s, so the overwrite below is observable.
        submit_ack_timeout: Some(Duration::from_secs(7)),
        deadman: deadman_config_from_policy(&armed),
        ..vike_core::CoreConfig::default()
    };
    let _ = profile.apply_guards_and_sinks(&mut cfg);
    assert_eq!(
        cfg.submit_ack_timeout,
        Some(Duration::from_secs(30)),
        "the profile's [guards] must have been APPLIED for this test to prove anything"
    );
    let dm = cfg.deadman.expect("the profile must not have disarmed the dead-man");
    assert_eq!(dm.timeout, Duration::from_secs(60), "…nor moved its timeout");
    assert_eq!(dm.action, vike_core::DeadManAction::CancelAllAndHalt, "…nor its action");
}

/// …and a policy that DOES name the band reaches the projection the live mount threads into
/// `make_engine`. Pure: the file→`Policy` half is `vike-config`'s own `tests/load.rs`; this pins
/// the daemon's hand-off, which is the half that did not exist before Phase 6c.
#[test]
fn a_configured_band_reaches_the_live_mounts_projection() {
    let policy =
        vike_config::Policy { market_slippage: Some(0.002), ..vike_config::Policy::default() };
    assert_eq!(vike_mount::MountPolicy::from(&policy).market_slippage, Some(0.002));
}
