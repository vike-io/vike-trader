//! The ready banner: its paper-vs-live mode string and sentinels, and the node identity it carries.

use super::mount_rows::{WireMountSeed, wire_mount_rows};
use super::settings::credential_store_health;

/// What the ready banner's `venue=` renders when the LIVE gate is on and the mount armed NOTHING —
/// an empty credential store, or a store holding no key for any wired venue.
///
/// It is a SENTINEL in a field whose other values are venue ids, so it must not be one: `main`'s
/// `banner_sentinel_is_not_a_venue_id` pins it against `vike_model::VENUES` (the roster is derived
/// from the `crates/bridges/*` tree, so a future bridge named `none` reddens that test rather than
/// silently making this line ambiguous).
///
/// ⚠ It is not `PAPER`. The gate being ON is an operator-visible fact independent of what armed:
/// the venue FEEDS are live, the `[[mounts]]` strategies are quoting against real prices, the B11
/// live-account locks are held, and one credential appearing in the store arms real exec on the
/// next start with no config change. Collapsing that to `PAPER` would hide the arm; rendering an
/// empty `LIVE (venue=)` would read as a truncated line rather than as a statement.
pub(super) const NO_VENUE_ARMED: &str = "none";

/// What the ready banner leads with when the credential store EXISTS and could not be OPENED.
///
/// ⚠ **It is a PREFIX, never a replacement, and both halves are load-bearing.** The suffix keeps
/// the existing `PAPER` / `LIVE (venue=…)` grammar verbatim, so every operator grep, every runbook
/// and `crates/vike-tradehub/tests/sigterm_stop.rs`'s exact-string check keep working — the banner
/// still answers paper-vs-live, which is its job. The prefix is what the banner did NOT say: an
/// empty credential map reaches the mount as *no credentials*, the live gate drops every venue to
/// paper, nothing fails, and the line an operator greps came back `LIVE (venue=none)` — which
/// `deploy/vike-tradehub.service`'s own header documents as a legitimate answer ("gate on,
/// nothing armed"). This is the one fact that tells those two boxes apart at the place the operator
/// is already looking.
///
/// UPPERCASE and leading because it is competing with a line that reads as normal. The reason and
/// the repair are NOT in here — a banner is one line and this one is already the widest field in
/// it; they are in the `tracing::error!` [`credential_store_health`] drives, and, for the commonest
/// cause, in `vike_secrets::DbErrorKind::ReadOnlyRollback`'s own message.
pub(super) const STORE_UNREADABLE_BANNER: &str = "CREDENTIAL STORE UNREADABLE";

/// The ready banner's `"mode"` — the string `docs/ops/tradehub-the CI box.md` and
/// `deploy/vike-tradehub.service` both name as the ONE authority on paper-vs-live, and which
/// operators grep for (`grep '"kind":"ready"'`).
///
/// ⚠ **It is a function of the ARMING RECORD, and the profile's mount set is not a parameter.**
/// That signature is the fix. The string used to be built from the daemon's `mount_venues` — the
/// DISTINCT venues the profile mounts a strategy on — which answers a different question and is
/// neither a subset nor a superset of what armed: `vike_mount::build_node` calls
/// `vike_mount::make_engine_with_legs` straight-line for every `WIRED_MARKETS` row and arms a real
/// exec client wherever the credential store answers, so the armed set is decided by the STORE,
/// not by the profile. MEASURED on the CI box, one startup of the shipped daemon, two lines apart:
///
/// ```text
/// live_venues={"hyperliquid","deribit","okx","bybit","alpaca","aster","binance","ig","oanda"}
/// {"kind":"ready","mode":"LIVE (venue=bybit)"}
/// ```
///
/// Nine live authenticated exec sessions; the banner named the one venue the profile mounted.
///
/// `venues` is `vike_mount::build_node`'s own `live_venues` — a venue is in it exactly when
/// `make_engine_with_legs` constructed a REAL `ExecutionClient` for it, so the set excludes a venue
/// with no credentials, a `data_only = true` venue whose keys [`vike_mount::startup::withhold_venue_credentials`] took
/// away, a ctrader/ibkr venue whose synchronous connect failed and demoted it, and polymarket's
/// recon-only fallback. Sorted before rendering: a `HashSet` iterates in an order that changes
/// between runs of the same binary, and an operator diffing two startups must not read a
/// reordering as a change.
///
/// ⚠ **`health` is the THIRD state, and it is not paper-vs-live.** An unreadable credential store
/// produces the same EMPTY map an unconfigured box produces, so the mount arms nothing and this
/// function would otherwise render `LIVE (venue=none)` — the exact string a correctly-unarmed box
/// prints. [`STORE_UNREADABLE_BANNER`] carries that argument; the rendering is a PREFIX so the
/// paper-vs-live half stays byte-identical in both arms.
pub(super) fn ready_mode_line(
    live: bool,
    venues: &std::collections::HashSet<String>,
    health: &vike_bridge_core::credentials::StoreHealth,
) -> String {
    let mode = if live {
        let mut armed: Vec<&str> = venues.iter().map(String::as_str).collect();
        armed.sort_unstable();
        let named = if armed.is_empty() { NO_VENUE_ARMED.to_string() } else { armed.join("+") };
        format!("LIVE (venue={named})")
    } else {
        // ⚠ UNCHANGED, and deliberately not routed through the `venue=` rendering above. A PAPER
        // daemon builds no live client by any path, so there is no set to name and nothing an
        // operator's existing `grep PAPER` should have to learn.
        "PAPER".to_string()
    };
    if health.is_readable() {
        // The ABSENT store is this arm — `StoreHealth::Readable` covers `Source::None`, because an
        // empty map from a box with no store is a real measurement of a real answer. So an
        // unconfigured daemon's banner is byte-for-byte what it was before this parameter existed.
        return mode;
    }
    // ...and the PAPER arm is prefixed too, deliberately. The gate being off does not make an
    // unreadable store harmless: the same store supplies the alert webhook targets and (under its
    // feature) the Telegram control channel, so a paper daemon with this fault is silently running
    // without the surfaces that would have told anyone about it.
    format!("{STORE_UNREADABLE_BANNER} — {mode}")
}

/// **Startup phase — the ready banner's `mode` and the completed wire rows**, both rendered from
/// the mount's ARMING RECORD (`live_venues`) so the two reports cannot disagree about one startup.
pub(super) fn ready_banner(
    live: bool,
    live_venues: &std::collections::HashSet<String>,
    wire_mount_seeds: Vec<WireMountSeed>,
) -> (String, Vec<vike_tradehub_client::wire::WireMountRow>) {
    // ⚠ THE PAPER-VS-LIVE BANNER, and it names the set that is ACTUALLY ARMED — never the set the
    // profile mounts. See the `mount_venues` comment far above for the measured the CI box startup this
    // replaces: nine armed venues under a banner reading `LIVE (venue=bybit)`, because the string
    // was built from the profile. `live_venues` is `build_node`'s own record, so the banner can no
    // longer disagree with the mount that produced it.
    //
    // SORTED, because a `HashSet` iterates in an order that changes between runs of the same
    // binary — an operator diffing two startups must not see a reordering and read it as a change.
    //
    // The PAPER arm is untouched: the string is still exactly `"PAPER"`.
    //
    // The rendering itself is [`ready_mode_line`] — a pure function of exactly these two values, so
    // that what the banner says can be TESTED rather than read off this call site, and so that the
    // profile's mount set is not merely unused here but structurally out of reach of the renderer.
    // ⚠ ...and the THIRD state the banner has to be able to say: the credential store EXISTS and
    // would not OPEN. That produces the same EMPTY map an unconfigured box produces, so every
    // venue drops to paper, the mount's arming record is empty, NOTHING FAILS — `Restart=on-failure`
    // never fires and `OnFailure=vike-notify@` never pages — and this line used to print
    // `LIVE (venue=none)`, the exact string a correctly-unarmed box prints. See
    // [`credential_store_health`] for why this daemon announces the fault rather than refusing to
    // start, argued against `docs/decisions/0013-degrade-vs-refuse.md`.
    //
    // The read itself is the one `vike_boot::boot` already performed through
    // [`workspace_credentials`] at startup; this is its memoized verdict, not a second open.
    let store_health = credential_store_health();
    if let vike_bridge_core::credentials::StoreHealth::Unreadable(why) = store_health {
        // `SecretsError`'s own Display — a path and a reason, never file contents (its doc says
        // so), and for a killed writer's rollback journal it is
        // `vike_secrets::DbErrorKind::ReadOnlyRollback`'s message, which names the repair itself.
        tracing::error!(
            reason = %why,
            "THE CREDENTIAL STORE IS PRESENT AND UNREADABLE — this daemon is running with NO \
             credentials, so EVERY VENUE IS ON PAPER and no order can reach a venue. This is NOT \
             the absent-credential gate: an unreadable store and an unconfigured box must never \
             look the same to an operator. ⚠ THE DAEMON CANNOT REPAIR THIS ITSELF — it opens the \
             store read-only, and a read-only open may not replay a journal, so a restart \
             meets the identical state. Repair it from an OPERATOR SHELL, where the \
             directory is writable: run any `vike-cli secrets` command (`vike-cli secrets list` is \
             enough — opening the store read-write is what replays a rollback journal a killed \
             writer left behind), fix what the reason above names, then restart this daemon. The \
             ready banner below leads with the same finding"
        );
    }

    let mode = ready_mode_line(live, live_venues, store_health);

    // ...and the SAME arming record decides each wire row's `live` — ONE value feeding both
    // reports, so the banner and the `StrategyStatus` verb cannot disagree about one startup.
    let wire_mounts = wire_mount_rows(wire_mount_seeds, live_venues);
    (mode, wire_mounts)
}

/// **Startup phase — WHO this daemon is**, stamped into every published frame (split-plane B3):
/// the identity block, with this box's address resolved and announced.
pub(super) fn node_identity(
    settings: &vike_config::Settings,
    active_daemon: &Option<String>,
    strategy_name: &str,
    strategy_params: &str,
    live: bool,
) -> vike_tradehub_client::wire::WireNodeIdentity {
    // WHICH BOX this daemon is running on, resolved ONCE here and stamped into the identity block
    // below. ⚠ This is the ONE identity fact the client cannot derive: both listeners bind
    // loopback, so a thin client reaches them through an SSH tunnel and its own socket address is
    // always the tunnel mouth — every client on every box reads `127.0.0.1:7879` whichever daemon
    // it is attached to. `crate::self_address` argues the whole discovery: a configured
    // `tradehub_advertise_addr` wins, else the source address the kernel's own routing table names
    // for an off-box destination (a route LOOKUP — no packet leaves this machine and nothing is
    // contacted), else an empty string, which a client renders as "the daemon said nothing" and
    // never as an address.
    //
    // It is resolved into a local rather than inlined below because it is the one part of that
    // block that is not already-resolved — the literal keeps the wiring-only rule.
    let advertise_addr = crate::self_address::advertise_addr(
        settings.config.tradehub_advertise_addr.as_deref(),
        settings.config.tradehub_addr.as_deref(),
    );
    // WHO this daemon is, stamped into every published frame (split-plane B3) so an observer
    // holding several backends can label them and tell paper from live. A struct literal over
    // already-resolved locals — no new reads, no logic (the wiring-only rule).
    let identity = vike_tradehub_client::wire::WireNodeIdentity {
        // ⚠ 0086: the identity name is the ACTIVE DAEMON ROW's name — the same name a `bootstrap-
        // daemon`/`config activate daemon` call gave it — never a `--config` file's stem, which this
        // binary no longer reads at all. `"tradehub"` is the one compiled-in fallback, used only
        // when nothing named a row, which the earlier refusal already makes unreachable in practice
        // (there is no `profile` to have reached this line without an active row) — kept as the
        // honest answer for that impossible case rather than an `unwrap`.
        name: active_daemon.clone().unwrap_or_else(|| "tradehub".to_string()),
        strategy: strategy_name.to_string(),
        params: strategy_params.to_string(),
        live,
        build: vike_buildinfo::summary(),
        advertise_addr,
    };
    // Say out loud which box this daemon decided it is, and whether the operator told it or the
    // kernel did. An EMPTY answer is the one an operator has to be able to act on — it is what a
    // client renders as "nothing was said" — so it is logged as a WARNING naming the key that
    // fixes it, rather than passing in silence like a successful lookup.
    if identity.advertise_addr.is_empty() {
        tracing::warn!(
            "this daemon could not determine its own address (no route to name a source address \
             from, e.g. a loopback-only container) — an observing client will be told nothing \
             about which box this is. `vike-cli config set config.tradehub_advertise_addr <addr>` \
             (or VIKE_TRADEHUB_ADVERTISE_ADDR) to name it."
        );
    } else {
        tracing::info!(
            advertise_addr = %identity.advertise_addr,
            source = if settings.config.tradehub_advertise_addr.is_some() {
                "config.tradehub_advertise_addr"
            } else {
                "discovered from this box's routing table"
            },
            "reporting this box's address to observing clients"
        );
    }
    identity
}
