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
        Some(vike_boot::halt::halt_path_from_env()),
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
/// drive `live_runtime_with` under no subscriber.
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

/// The SIBLING half of the same warning: `link_deadman_grace_ms = 0`'s way back is `config unset`,
/// which removes the row so the grace is the compiled-in default again — not a hand edit of a line
/// and not a write of today's default number into a new row.
#[test]
fn the_link_grace_way_back_is_a_config_unset() {
    let both_off = vike_config::Policy {
        link_deadman_grace_ms: Some(vike_config::LINK_DEADMAN_DISABLED_MS),
        ..vike_config::Policy::default()
    };
    let msg = deadman_absent_warning(&both_off).unwrap();
    assert!(msg.contains("Run `vike-cli config unset policy.link_deadman_grace_ms`"), "{msg}");
    assert!(!msg.contains("config set policy.link_deadman_grace_ms"), "{msg}");
    assert!(msg.contains("AND NEITHER IS THE OTHER ONE"), "{msg}");
    assert!(msg.contains("NO automatic stop of any kind"), "{msg}");
}

// --- THE LINK DEAD-MAN's fold (M13) --------------------------------------------------------
// The FOUR inputs are the policy grace, `vike_venues::link_deadman_default`, the venues this
// mount has and the LANES it subscribed for each; each test below moves ONE of them.

/// A mounted venue whose lanes DO carry a disconnect, for the fold tests — the disclosure half
/// is a separate question, tested against real [`VenuePlan`]s by
/// [`the_cex_arm_subscribes_no_lane_that_could_report_a_dead_link`] below.
fn seen(venue: &str) -> (String, AdapterLinkDisclosure) {
    (venue.to_string(), AdapterLinkDisclosure::Discloses { lane: "a test lane" })
}

/// The same venue mounted over a plan whose only voice is the datahub link (no venue-side lane).
fn unseen(venue: &str) -> (String, AdapterLinkDisclosure) {
    (
        venue.to_string(),
        AdapterLinkDisclosure::StratLinkOnly { lane: "a test lane: the datahub link only" },
    )
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
        Some(vike_boot::halt::halt_path_from_env()),
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
fn an_fx_only_start_builds_no_link_deadman_at_all() {
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

/// **The PRICE-ONLY venues** (`wire_price_only_feeds`: the control door's price handle for a venue no mount
/// names) are the admitting venues that no mount names, and nothing else: on a box that mounts bybit alone
/// they are binance, okx, aster, hyperliquid and deribit; mounting one removes it; a venue the door does not
/// admit for is never one.
#[test]
fn the_price_only_venues_are_the_admitting_venues_no_mount_names() {
    let admitting = crate::server::symbol_admission::admitting_venues();
    let only = |mounted: &[&str]| {
        let mounted: Vec<String> = mounted.iter().map(|m| (*m).to_string()).collect();
        let mut v = crate::feeds::price_only_venues(&admitting, &mounted);
        v.sort();
        v
    };
    assert_eq!(only(&["bybit"]), ["aster", "binance", "deribit", "hyperliquid", "okx"]);
    assert_eq!(only(&["bybit", "okx", "hyperliquid", "deribit"]), ["aster", "binance"]);
    assert!(only(&["aster", "binance", "bybit", "deribit", "hyperliquid", "okx"]).is_empty());
    assert!(crate::feeds::price_only_venues(&[], &[]).is_empty(), "no admission, no venue");
    assert!(price_only_config(&[]).is_none(), "nothing price-only, nothing armed on the core");
}

/// **A price-only venue IS in the link dead-man's set, beside the mounted ones** — through the lane
/// reading `price_only_link_disclosure` gives it, so the same table decides: the CEX venues (the
/// pump) and hyperliquid and deribit (the hub links) arm, and nothing that is not price-only is added. The
/// core is told which of them it may leave unwatched while they hold nothing (`price_only_config`).
#[test]
fn a_price_only_venue_is_in_the_link_deadman_set() {
    let mounted = [seen("bybit")];
    let price_only: Vec<String> = ["aster", "binance", "deribit", "hyperliquid", "okx"]
        .into_iter()
        .map(str::to_string)
        .collect();
    let venues: Vec<_> = mounted
        .iter()
        .cloned()
        .chain(price_only.iter().map(|v| (v.clone(), price_only_link_disclosure(v))))
        .collect();
    let cfg =
        link_deadman_config_from_policy(&vike_config::Policy::default(), &venues).expect("armed");
    for venue in ["bybit", "binance", "okx", "aster", "hyperliquid", "deribit"] {
        assert!(cfg.venues.contains(venue), "{venue} is heard by the link dead-man");
    }
    assert_matches!(
        price_only_link_disclosure("hyperliquid"),
        AdapterLinkDisclosure::StratLinkOnly { .. }
    );
    assert_matches!(
        price_only_link_disclosure("deribit"),
        AdapterLinkDisclosure::StratLinkOnly { .. }
    );
    assert_matches!(price_only_link_disclosure("okx"), AdapterLinkDisclosure::Discloses { .. });
    let po = price_only_config(&price_only).expect("five price-only venues");
    let expected: std::collections::BTreeSet<String> =
        cfg.venues.iter().filter(|v| *v != "bybit").cloned().collect();
    assert_eq!(po.venues, expected);
    assert_eq!(po.sweep_interval, Duration::from_secs(30));
    assert!(
        !po.venues.contains("bybit"),
        "a mounted venue is never gated on holdings: its windows keep every rule they had"
    );
}

/// **The wiring in `live_runtime_with`, pinned on the whitespace-free source** (the runtime needs a datahub
/// and a core): the dead-man's venue list is the mounts' plans PLUS the price-only set built from the same
/// `admitting` and `mounted_venues` the feeds are wired from, and the same set reaches the core.
#[test]
fn the_price_only_set_reaches_the_link_deadman_and_the_core() {
    let src: String = include_str!("../live_runtime.rs").split_whitespace().collect();
    for needle in [
        "venue_plans.iter().map(|(v,plan)|(v.clone(),adapter_link_disclosure(plan))).collect();",
        "crate::feeds::price_only_venues(&admitting,&mounted_venues);",
        "link_venues.extend(price_only_set.iter().map(|v|(v.clone(),price_only_link_disclosure(v))));",
        "link_deadman:link_deadman_config_from_policy(policy,&link_venues),",
        "price_only:price_only_config(&price_only_set),",
    ] {
        assert!(src.contains(needle), "live_runtime_with lost `{needle}`");
    }
}

/// **The price-only wiring's joins in `live_runtime_with`, pinned on the whitespace-free source** (the repo's
/// wiring-pin idiom; the runtime itself needs a datahub and a core). Each fragment is a way the wiring went
/// quietly wrong: the Hyperliquid network hard-coded instead of asked of the rows (`hl_price_network`), the
/// price-only feeds asked about a different venue list than the mounts' (`mounted_venues`), the watchers not
/// registered with the door (`watchers.extend`), the feeds not handed to the teardown (`feeds.extend`), and the
/// wired network not reaching the account follower (`hl_feed_network`, two joins).
#[test]
fn the_price_only_wiring_joins_in_live_runtime_are_pinned() {
    let src: String = include_str!("../live_runtime.rs").split_whitespace().collect();
    for needle in [
        "hl_unmounted.then(||crate::feeds::hl_price_network(&plan_table,&adapter_policy))",
        "crate::feeds::wire_price_only_feeds(&admitting,&mounted_venues,hl_price_network,&handle,&wrap,",
        "watchers.extend(price_only.watchers);",
        "hl_feed_network=price_only.hl_network;",
        "feeds.extend(price_only.feeds);",
        "hl_feed_network=Some(*network);",
        "hl_feed_network,recon_legs:recon_driver",
    ] {
        assert!(src.contains(needle), "live_runtime_with lost `{needle}`");
    }
    // …and the watchers it registered are what the door is bound to.
    assert!(
        src.contains("facts.bind_prices(watchers);"),
        "the door is bound to the registered watchers"
    );
}

/// **The CEX arm's two lanes, gated on the CODE.** The kline lane subscribes BARS and nothing else; the
/// tick pump is `subscribe_ticks`; and neither calls `subscribe_depth`, the silent-no-op trap (it emits
/// `LiveDataSink::l2_snapshot`, which every sink this daemon owns drops: a depth-wired maker connects,
/// reports healthy and posts zero orders forever).
///
/// This replaced the gate on the bybit `recon_feed_statuses` row's single-writer condition
/// (`the_cex_arm_subscribes_only_the_bar_verb_on_the_status_bearing_handle`): that row keyed on the
/// bridge's `Feeds::status` string, which this process no longer holds, so the condition it guarded is
/// gone. The surviving half of it — "the bar lane is bars only" — is the same scan, aimed at the same
/// anchors.
#[test]
fn the_cex_arm_subscribes_bars_then_the_tick_pump_and_never_depth() {
    const FEEDS: &str = include_str!("../../feeds/cex.rs");
    // Anchored on the kline lane's function (`wire_venue_feeds`' CEX arm calls it, then calls the
    // tick-pump function, which sits directly below it in the file).
    let arm_start = FEEDS.find("fn wire_cex_kline_lane(").expect(
        "the CEX kline lane's function head has moved — this scan is anchored on it and would \
             otherwise check nothing",
    );
    // Bound the first scan at LANE 2's banner, where the tick pump takes over.
    let lane2_banner = FEEDS[arm_start..]
        .find("── LANE 2:")
        .expect("the CEX arm's lane-2 banner has moved — re-anchor this scan");
    let lane1 = &FEEDS[arm_start..arm_start + lane2_banner];
    assert!(
        lane1.contains("subscribe_bars"),
        "the CEX arm must still subscribe bars in its kline lane, or this gate is checking nothing"
    );
    for forbidden in ["subscribe_trades", "subscribe_depth", "subscribe_book", "subscribe_ticks"] {
        assert!(
            !lane1.contains(forbidden),
            "the CEX kline lane now calls `{forbidden}`: bars are this lane's one verb (the pump is \
             lane 2's, and depth is the silent-no-op trap)"
        );
    }
    // Lane 2 runs from its banner to the arming announcement.
    let lane2_start = arm_start + lane2_banner;
    let lane2_end = FEEDS[lane2_start..]
        .find("/// The CEX arming disclosure")
        .expect("the arming announcement's doc has moved — re-anchor this scan");
    let lane2 = &FEEDS[lane2_start..lane2_start + lane2_end];
    assert!(
        lane2.contains("subscribe_ticks("),
        "the CEX tick pump is `subscribe_ticks` — losing it silently un-arms the link dead-man on four venues"
    );
    // The comment above names the trap by its verb; only a CALL is forbidden.
    assert!(
        !lane2.contains(".subscribe_depth(") && !lane2.contains(".subscribe_bars("),
        "the tick pump lane subscribes the pump, not depth and not bars"
    );
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
/// on `StratLinkOnly` now, because losing the pump's disclosure would silently un-arm four venues on
/// every live daemon while `vike_venues::link_deadman_default` still called them Armed —
/// exactly the false-promise state this whole fold exists to prevent, wearing the other face.
#[test]
fn the_cex_arm_subscribes_the_tick_pump_that_reports_a_dead_link() {
    for venue in [CexVenue::Binance, CexVenue::Bybit, CexVenue::Okx, CexVenue::Aster] {
        let plan = VenuePlan::Cex { venue, mainnet: true };
        match adapter_link_disclosure(&plan) {
            AdapterLinkDisclosure::Discloses { lane } => {
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
            AdapterLinkDisclosure::StratLinkOnly { lane: why } => panic!(
                "the CEX arm claims to disclose NOTHING ({why:?}) — if the tick pump's \
                     transport disclosure was removed or a subscription changed, then \
                     vike_venues::link_deadman_default's Armed rows for binance/aster/bybit/okx are \
                     a promise this daemon cannot keep: fix the pump, or move those rows, and say \
                     so here"
            ),
        }
    }
    // …and the venue whose disclosure rides a SINK rather than the tick lane is unaffected.
    assert_matches!(
        adapter_link_disclosure(&VenuePlan::Polymarket),
        AdapterLinkDisclosure::Discloses { .. }
    );
    assert_matches!(
        adapter_link_disclosure(&VenuePlan::Deribit),
        AdapterLinkDisclosure::StratLinkOnly { .. },
        "deribit's bridge calls stream_status nowhere: only the datahub link can speak for it"
    );
}

/// ⚠ **The end-to-end arming claim for the newly-covered venues, over the REAL plans and the
/// REAL table** — the join every other test here takes one leg of. A default `Policy`, a
/// CEX mount, and the four venues the table arms all end up in the config's venue set AND read
/// ARMED in the operator's startup lines. This is the test that would have been red on the day
/// the feature shipped.
#[test]
fn a_default_cex_start_arms_the_link_deadman_and_says_so() {
    let policy = vike_config::Policy::default();
    let mounted: Vec<(String, AdapterLinkDisclosure)> =
        [CexVenue::Binance, CexVenue::Bybit, CexVenue::Okx, CexVenue::Aster]
            .into_iter()
            .map(|v| {
                let plan = VenuePlan::Cex { venue: v, mainnet: true };
                (v.slug().to_string(), adapter_link_disclosure(&plan))
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

    // ⚠ …and a venue whose own bridge discloses nothing (deribit's row is `Inert`) is armed for the
    // DATAHUB LINK only, and says so — asserted over the real `VenuePlan` rather than the constructed
    // `seen()`/`unseen()` the fold tests use, since the whole point of that arm is that it describes a
    // real mount. It used to read "off ... stream_status nowhere", which is false for the strategy lane.
    let with_deribit =
        [mounted[0].clone(), ("deribit".to_string(), adapter_link_disclosure(&VenuePlan::Deribit))];
    let lines = link_deadman_arming_report(&policy, &with_deribit);
    assert!(lines[1].contains("ARMED for deribit at 120000 ms"), "{}", lines[1]);
    assert!(lines[1].contains("datahub"), "…over the datahub link: {}", lines[1]);
    assert!(
        lines[1].contains("The venue's own socket can never trip it"),
        "…and that the venue's own socket cannot: {}",
        lines[1]
    );
    // ⚠ The switch arms per VENUE, so the account's TRADE LINK to the gatewayhub (`gateway-trade`) trips it as
    // surely as the datahub link does: a gatewayhub restart over the grace halts trading on deribit. The line
    // said "only the link to the datahub can" until W12c, which was false (review 17, finding 2).
    assert!(
        lines[1].contains("datahub OR gatewayhub outage")
            && lines[1].contains("the account's trade link to the gatewayhub")
            && lines[1].contains("a gatewayhub restart longer than that halts trading on deribit"),
        "…and that the gatewayhub's trade link trips it too: {}",
        lines[1]
    );
    assert!(
        !lines[1].contains("only the link to the datahub can"),
        "the retired claim (only the datahub link can trip it) is false and gone: {}",
        lines[1]
    );
    assert!(
        !lines[1].contains("no automatic stop"),
        "the retired false claim (no lane this daemon could subscribe would disclose a dead link) \
         is gone: {}",
        lines[1]
    );
}

/// **The datahub link is watched on every venue whose own socket cannot open a window** (hyperliquid,
/// deribit, alpaca, ctrader: `Inert` rows), over the REAL plans and the REAL table, with the CEX venues and
/// polymarket exactly as before, and oanda and ig still OFF with a line that says the datahub link goes
/// unwatched with them (the switch arms per venue, and a `SessionBounded` venue's own socket would be armed
/// with it).
#[test]
fn the_datahub_link_arms_the_venues_that_used_to_be_unwatched() {
    let policy = vike_config::Policy::default();
    let plans: [(&str, VenuePlan); 9] = [
        ("binance", VenuePlan::Cex { venue: CexVenue::Binance, mainnet: true }),
        ("polymarket", VenuePlan::Polymarket),
        ("hyperliquid", VenuePlan::Hyperliquid(crate::HlNetwork::Mainnet)),
        ("deribit", VenuePlan::Deribit),
        ("alpaca", VenuePlan::Alpaca),
        ("ctrader", VenuePlan::Ctrader),
        ("oanda", VenuePlan::Oanda),
        ("ig", VenuePlan::Ig),
        ("aster", VenuePlan::Cex { venue: CexVenue::Aster, mainnet: true }),
    ];
    let mounted: Vec<(String, AdapterLinkDisclosure)> =
        plans.iter().map(|(v, p)| ((*v).to_string(), adapter_link_disclosure(p))).collect();
    let cfg = link_deadman_config_from_policy(&policy, &mounted).expect("armed");
    let armed: Vec<&str> = cfg.venues.iter().map(String::as_str).collect();
    assert_eq!(
        armed,
        ["alpaca", "aster", "binance", "ctrader", "deribit", "hyperliquid", "polymarket"],
        "oanda and ig stay out; everything else is watched"
    );
    let lines = link_deadman_arming_report(&policy, &mounted);
    for (i, (venue, _)) in plans.iter().enumerate() {
        let line = &lines[i];
        if matches!(*venue, "oanda" | "ig") {
            assert!(line.contains(&format!("off for {venue}")), "{line}");
            assert!(line.contains("strategy-lane link to the datahub"), "{line}");
        } else {
            assert!(line.contains(&format!("ARMED for {venue}")), "{line}");
        }
    }
    // The CEX and polymarket lines are the ones they always were (their lane text is unchanged).
    assert!(lines[0].contains("tick pump") && !lines[0].contains("datahub outage"), "{}", lines[0]);
    assert!(lines[1].contains("subscribe_book on every seated token"), "{}", lines[1]);
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
    // through the one store there is, and the way back is `config unset` (it removes the row, so
    // the key is the default again) rather than "delete the line" or a write of today's default
    // (same class as the silence dead-man's own remedy; `crates/vike-config/src/remedy.rs`'s
    // `unset_clause` carries the argument).
    let off = vike_config::Policy {
        link_deadman_grace_ms: Some(vike_config::LINK_DEADMAN_DISABLED_MS),
        ..vike_config::Policy::default()
    };
    for line in link_deadman_arming_report(&off, &venues) {
        assert!(line.contains("`link_deadman_grace_ms = 0` in the settings database"), "{line}");
        assert!(
            line.contains("Run `vike-cli config unset policy.link_deadman_grace_ms`"),
            "{line}"
        );
        assert!(!line.contains("config set policy.link_deadman_grace_ms"), "{line}");
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
    let profile =
        vike_config::run_profile::samples::live().expect("the shipped live sample parses");
    let mut cfg = vike_core::CoreConfig {
        submit_ack_timeout: Some(Duration::from_secs(7)),
        link_deadman: link_deadman_config_from_policy(
            &vike_config::Policy::default(),
            &[seen("binance")],
        ),
        ..vike_core::CoreConfig::default()
    };
    let _ = vike_tradehub_core::apply_guards_and_sinks(&profile, &mut cfg);
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
/// after the `CoreConfig` literal in `live_runtime_with` and overwrites the guards it names; it
/// must have no way to reach `deadman`, or a `[guards]` table — a file with none of
/// the `policy` section's protections — could disarm the switch. Driven through the shipped LIVE
/// sample profile, which DOES name `submit_ack_timeout_ms`, and the assertion on that field is
/// what proves the profile was actually applied rather than ignored: a test where nothing
/// changed would be green for the wrong reason.
#[test]
fn the_run_profile_cannot_touch_the_deadman() {
    let profile =
        vike_config::run_profile::samples::live().expect("the shipped live sample parses");
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
    let _ = vike_tradehub_core::apply_guards_and_sinks(&profile, &mut cfg);
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
fn a_configured_band_reaches_the_live_starts_projection() {
    let policy =
        vike_config::Policy { market_slippage: Some(0.002), ..vike_config::Policy::default() };
    assert_eq!(vike_tradehub_core::AdapterPolicy::from(&policy).market_slippage, Some(0.002));
}

/// **The FACT behind deribit's `StratLinkOnly` classification, read off the bridge rather than asserted in
/// prose** (restored: the classification test used to say `stream_status nowhere` for the market feed and W12b
/// reduced it to a word). Deribit's `market_feed` calls `LiveDataSink::stream_status` from exactly ONE place, the
/// DEPTH lane (`depth_main`, labelled `StreamLabel::Depth`), which this daemon does not subscribe; its bars,
/// quotes, trades and book lanes (what the deribit arm subscribes) disclose nothing, so only the two hub links
/// can speak for the venue. A bridge that grew a disclosure on a subscribed lane makes this red: the venue's
/// `Inert` row and this classification must then be revisited together. The file is read at run time and the
/// test says so and passes when the tree is not there (a mirror without `crates/bridges`).
#[test]
fn deribits_subscribed_lanes_disclose_no_status_only_its_unsubscribed_depth_lane_does() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("bridges")
        .join("deribit")
        .join("src")
        .join("market_feed.rs");
    let Ok(src) = std::fs::read_to_string(&path) else {
        eprintln!("skipped: {} is not in this tree", path.display());
        return;
    };
    let calls: Vec<&str> = src.lines().filter(|l| l.contains("stream_status(")).collect();
    assert_eq!(
        calls.len(),
        1,
        "deribit's market feed calls stream_status from one place (the depth lane): {calls:?}"
    );
    assert!(
        calls[0].contains("StreamLabel::Depth"),
        "…and that place is the DEPTH lane, which the tradehub does not subscribe: {}",
        calls[0]
    );
    assert_matches!(
        adapter_link_disclosure(&VenuePlan::Deribit),
        AdapterLinkDisclosure::StratLinkOnly { .. }
    );
}
