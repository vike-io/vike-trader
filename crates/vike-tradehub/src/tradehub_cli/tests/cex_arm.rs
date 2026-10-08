//! The CEX live arm, its health map and the mount announcement (`cex_arming`).

use super::*;

// ---------------------------------------------------------------------------------------------
// The CEX (binance/bybit/okx) live arm.
// ---------------------------------------------------------------------------------------------

/// A `MakerMountConfig` for `venue` on the symbol `build_node` actually mounts it on — the same
/// lowering `DaemonProfile::to_mount_config` produces for a CEX profile.
fn cex_cfg(venue: CexVenue) -> MakerMountConfig {
    wired_1m_cfg(venue.slug(), 0.5, 0.001)
}

/// **The slug table, pinned against an authority that is NOT `slug()` itself.**
///
/// ⚠ This is the missing gate, and its absence was MEASURED: on the CI box, rewriting
/// `CexVenue::Binance => "binance"` to `"bybit"` left the ENTIRE vike-tradehub suite GREEN.
/// Every other slug assertion here — `every_cex_venue_slug_names_a_wired_market`,
/// `cex_plan_accepts_the_wired_symbol`, the recon feed-status key — reads its expectation FROM
/// `slug()` and compares it against a set that contains all three strings, so no PERMUTATION of
/// them can fail: the declaration-pinning failure mode this repo has been bitten by three times.
///
/// The authority is each bridge crate's OWN public venue constant — the string that crate
/// stamps on the ticks its pump emits and that its exec path signs under. That makes this a
/// genuine cross-check rather than a hand copy of the match arms: `CexVenue::Binance` MUST be
/// the venue `vike_binance` is, because `CexVenue::Binance`'s feed arm calls
/// `vike_binance::market_data::spawn_binance_market_data` and nothing else.
///
/// The literal half is asserted too, because the bridge constant is itself mutable — the two
/// authorities would have to be changed together and in agreement to slip a wrong slug through.
/// The `match` is EXHAUSTIVE, so a fourth CEX venue is a compile error here rather than a row
/// somebody forgets.
#[test]
fn cex_venue_slugs_are_pinned_to_the_bridge_crates_own_venue_string() {
    for venue in CexVenue::ALL {
        let (bridge_const, literal) = match venue {
            // Homed at the CRATE ROOT rather than in `spot` since ruling 8's feeds/exec seam,
            // for the reason aster's note below gives: `data` (the keyless kline REST) names
            // it too, and `spot` is now behind that crate's `exec` feature.
            CexVenue::Binance => (vike_binance::VENUE, "binance"),
            CexVenue::Bybit => (vike_bybit::perp::VENUE, "bybit"),
            CexVenue::Okx => (vike_okx::perp::VENUE, "okx"),
            // Homed in `urls` (the feed-plane module) rather than an exec module — aster's
            // canonical id lives beside its host table so BOTH planes can name it
            // (`crates/bridges/aster/src/urls.rs`'s `VENUE`).
            CexVenue::Aster => (vike_aster::urls::VENUE, "aster"),
        };
        assert_eq!(
            venue.slug(),
            bridge_const,
            "{venue:?}'s slug must be the venue string its OWN bridge crate declares — the \
                 feed arm calls that crate's pump, while `make_engine` keys the ExecutionClient \
                 and the credential prefix on this slug, so a disagreement mounts one venue's \
                 book against another venue's account"
        );
        assert_eq!(
            venue.slug(),
            literal,
            "{venue:?}'s slug is pinned VERBATIM here as well as against the bridge constant, \
                 so that changing both in step is still a deliberate two-place edit"
        );
    }
}

/// Every CEX venue's slug must be a venue `build_node` actually mounts an engine for, and the
/// slug must be the string that table keys on. Cheap, but it is the join between the FEED half
/// (this file) and the EXEC half (`crate::wired_markets::WIRED_MARKETS`): a typo'd slug would give a feed
/// with no engine behind it, and every order would vanish at `accepts_symbol`.
///
/// ⚠ This one CANNOT see a permuted slug — every string it checks against contains all three.
/// `cex_venue_slugs_are_pinned_to_the_bridge_crates_own_venue_string` above is that gate.
#[test]
fn every_cex_venue_slug_names_a_wired_market() {
    for venue in CexVenue::ALL {
        assert_wired_and_advertised(venue.slug());
    }
}

/// The plan gate ACCEPTS the wired pair for each of the three venues, and CARRIES the resolved
/// ceiling-decided (decision 0095) network verdict through to the feed block.
///
/// The verdict has to travel in the plan: `vars` is moved into the `NodeConfig` before the feed
/// block runs, so the announcement cannot re-read the flag for itself — the same reason
/// `VenuePlan::Hyperliquid` carries its resolved `Network`. A plan that dropped it would leave
/// the mount unable to say which network it is about to trade on.
#[test]
fn cex_plan_accepts_the_wired_symbol_and_carries_the_network() {
    for venue in CexVenue::ALL {
        let cfg = cex_cfg(venue);
        for mainnet in [false, true] {
            match cex_plan(venue, &cfg, mainnet) {
                Ok(VenuePlan::Cex { venue: v, mainnet: m }) => {
                    assert_eq!(v, venue, "the plan must name the venue it was asked about");
                    assert_eq!(
                        m,
                        mainnet,
                        "{} must carry the resolved mainnet verdict to the announcement — \
                             `vars` is gone by then",
                        venue.slug()
                    );
                }
                other => {
                    panic!("{} on its own wired symbol must plan, got {other:?}", venue.slug())
                }
            }
        }
    }
}

/// **A foreign symbol is REFUSED, not silently mounted.**
///
/// `vike_mount::make_engine` wires no `extra_symbols`, so
/// `vike_exec::ExecutionEngine::accepts_symbol` is plain equality: a mount on the wrong symbol
/// keeps its feed, keeps quoting, and has every order AND every fill dropped with no log line
/// anywhere. That is the failure this gate converts into a startup error, so the error must name
/// it.
#[test]
fn cex_plan_refuses_a_foreign_symbol() {
    for venue in CexVenue::ALL {
        let mut cfg = cex_cfg(venue);
        let wired = cfg.token_id.clone();
        cfg.token_id = format!("{wired}-NOT-THE-MOUNTED-ONE");
        let err = cex_plan(venue, &cfg, false).expect_err("a foreign symbol must be refused");
        assert!(err.contains("SILENTLY DROPPED"), "names the real failure mode: {err}");
        assert!(err.contains(&wired), "names the symbol build_node mounts: {err}");
    }
}

/// **A non-positive or non-finite tick size is REFUSED.**
///
/// `cfg.tick_size` is handed straight to `spawn_*_market_data`, where it sizes the `L2Book`
/// price grid — a plain `f64` parameter with no validation on the venue side. A `0.0` (the
/// value a profile that never set `tick_size` would produce if the default ever changed) or a
/// NaN builds a degenerate grid that raises no error and yields no usable top-of-book, so the
/// maker simply never quotes. It is also the maker's OWN grid, so one check covers both.
#[test]
fn cex_plan_refuses_an_unusable_tick_size() {
    for bad in [0.0, -0.5, f64::NAN, f64::INFINITY] {
        let mut cfg = cex_cfg(CexVenue::Okx);
        cfg.tick_size = bad;
        let err = match cex_plan(CexVenue::Okx, &cfg, false) {
            Ok(plan) => panic!(
                "tick_size {bad} must be refused BEFORE a book is built on it, but the gate \
                     planned {plan:?}"
            ),
            Err(e) => e,
        };
        assert!(
            err.contains("tick_size"),
            "the refusal must name the field an operator has to fix: {err}"
        );
    }
    // ...and a good one still passes, so the guard is not simply always-on.
    let mut cfg = cex_cfg(CexVenue::Okx);
    cfg.tick_size = 0.1;
    assert!(cex_plan(CexVenue::Okx, &cfg, false).is_ok(), "a real okx tick size must pass");
}

fn cex_bars_for(venue: CexVenue) -> CexBars {
    let sink: Arc<dyn LiveDataSink> = Arc::new(NullSink);
    match venue {
        CexVenue::Binance => CexBars::Binance(vike_binance::market_feed::Feeds::new(sink, || {})),
        CexVenue::Bybit => CexBars::Bybit(vike_bybit::market_feed::Feeds::new(sink, || {})),
        CexVenue::Okx => CexBars::Okx(vike_okx::market_feed::Feeds::new(sink, || {})),
        // The same `with_env(.., Live)` construction as `wire_venue_feeds`' aster arm (a
        // `Feeds` connects nothing until `subscribe_*`, so this stays network-free).
        CexVenue::Aster => CexBars::Aster(vike_aster::market_feed::Feeds::with_env(
            sink,
            || {},
            vike_bridge_core::credentials::Environment::Live,
        )),
    }
}

/// **The feed-status health map carries EXACTLY the mounted CEX venue's own row.**
///
/// Two halves, and both are load-bearing in opposite directions:
///
/// - a row MUST exist, or that venue's reconcile pass reads a blanket `Healthy` and keeps
///   reconciling against a feed that is known to be down;
/// - and no OTHER venue may appear, because the health gate can only ever SUPPRESS a pass — a
///   spurious row silently stops an unrelated venue from reconciling at all, and a suppressed
///   pass can stay suppressed (`reconcile_config::health_from_feed_status`'s doc).
///
/// The keys must also be the exact strings `ReconManager::should_reconcile` looks up, which is
/// what ties `CexBars::slug` to `CexVenue::slug` and to `crate::wired_markets::WIRED_MARKETS`.
#[test]
fn the_cex_health_map_carries_only_the_mounted_venues_own_row() {
    for venue in CexVenue::ALL {
        let bars = cex_bars_for(venue);
        assert_eq!(bars.slug(), venue.slug(), "CexBars::slug must match CexVenue::slug");

        let feeds = LiveFeeds::Cex { ticks: None, bars };
        let map = feeds.recon_feed_statuses();
        assert_eq!(
            map.keys().collect::<Vec<_>>(),
            vec![venue.slug()],
            "exactly one row — this venue's own — must be health-gated, got {:?}",
            map.keys().collect::<Vec<_>>()
        );
        // The handle is live: it is the same `Arc` the feed will publish its status through, so
        // the gate reads a real string rather than a detached copy.
        let status = Arc::clone(map.get(venue.slug()).expect("its own row"));
        *status.lock().expect("status mutex") = "disconnected".to_string();
        assert_eq!(
            reconcile_config::health_from_feed_status(
                &feeds.recon_feed_statuses()[venue.slug()].lock().expect("status").clone()
            ),
            vike_core::ReconHealth::Degraded,
            "a disconnected feed must degrade THIS venue's reconcile health"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// The mount ANNOUNCEMENT (`cex_arming`) — the decisions the feed block makes about what to tell
// the operator. `live_mount` itself cannot be called from a test (it is a `main.rs` function
// that spawns a core, opens two sockets and reads the credential store), which is why these
// decisions were extracted; before that, NOTHING exercised the block and both defects below
// shipped inside it.
// ---------------------------------------------------------------------------------------------

/// **The LIVE announcement must name the NETWORK.** "EXEC IS LIVE" is a materially different
/// statement on demo than on mainnet, and both states are reachable from the same profile by one
/// environment variable. The old line said neither.
#[test]
fn a_live_cex_mount_announces_which_network_it_trades_on() {
    for venue in CexVenue::ALL {
        let demo = cex_arming(venue, false, true);
        assert_eq!(demo.exec, "LIVE");
        // Aster spells its non-mainnet tier the way its OWN credential naming does
        // (`ASTER_TESTNET_*` — `load_aster_credentials`), so the log matches the keys the
        // operator actually provisioned; the flag venues keep their `DEMO` spelling.
        let expected = if venue == CexVenue::Aster { "TESTNET" } else { "DEMO" };
        assert_eq!(demo.network, expected, "{} non-mainnet ⇒ {expected}", venue.slug());
        assert_eq!(demo.remedy, None, "a live mount has nothing to remedy");

        let main = cex_arming(venue, true, true);
        assert_eq!(main.exec, "LIVE");
        assert_eq!(
            main.network,
            "MAINNET",
            "{} mainnet with live creds is REAL MONEY and the mount must say so",
            venue.slug()
        );
        assert_ne!(
            demo.network, main.network,
            "the two networks must be DISTINGUISHABLE in the log — this is the whole point"
        );
    }
}

/// **The PAPER remedy must be ACTIONABLE IN THE STATE IT IS PRINTED IN.**
///
/// ⚠ The defect this pins: the old warning advised `{VENUE}_DEMO_API_KEY` /
/// `{VENUE}_DEMO_API_SECRET` unconditionally. `vike_mount::make_engine` chooses the credential
/// TIER from the ceiling before it looks anything up (decision 0095) — `if mainnet {
/// load_credentials_from(venue, Environment::Live, vars) } else { … Demo … }` — so in the
/// `live`-ceiling + no-LIVE-keys state, which is exactly the state an operator hits while going
/// live, the advice names a tier that is never consulted. Following it produces the identical
/// PAPER mount and the identical warning, with nothing to distinguish the second attempt from
/// the first. An instruction that cannot work is worse than no instruction.
#[test]
fn the_paper_remedy_names_the_tier_make_engine_will_actually_read() {
    for venue in CexVenue::ALL {
        if venue == CexVenue::Aster {
            // Aster has no `{VENUE}_MAINNET` flag and no `_API_KEY` shape, so this test's
            // whole flag-tier vocabulary does not apply — its remedy has its own gate below
            // (`the_aster_remedy_names_the_agent_wallet_keys_and_the_live_first_hazard`).
            continue;
        }
        let upper = venue.slug().to_uppercase();

        // DEMO state: the demo tier IS what `make_engine` reads, so advise it.
        let demo = cex_arming(venue, false, false);
        assert_eq!(demo.exec, "PAPER");
        assert_eq!(demo.network, "DEMO");
        let demo_remedy = demo.remedy.expect("a paper mount must say how to arm it");
        assert!(
            demo_remedy.contains(&format!("{upper}_DEMO_API_KEY")),
            "unset flag ⇒ the DEMO tier is the one that arms exec: {demo_remedy}"
        );
        assert!(
            !demo_remedy.contains(&format!("{upper}_LIVE_API_KEY")),
            "and it must not send an operator to add real-money keys: {demo_remedy}"
        );

        // MAINNET state: the demo tier is NOT consulted. Advising it is the bug.
        let main = cex_arming(venue, true, false);
        assert_eq!(main.exec, "PAPER");
        assert_eq!(main.network, "MAINNET");
        let main_remedy = main.remedy.expect("a paper mount must say how to arm it");
        assert!(
            main_remedy.contains(&format!("{upper}_LIVE_API_KEY")),
            "a `live` ceiling ⇒ `make_engine` loads the LIVE tier, so that is the tier to name: \
                 {main_remedy}"
        );
        assert!(
            !main_remedy.contains(&format!("{upper}_DEMO_API_KEY")),
            "⚠ THE DEFECT: advising the DEMO tier under a `live` ceiling is advice that arms \
                 NOTHING — `make_engine` never consults it in this state: {main_remedy}"
        );
        assert!(
            main_remedy
                .contains(&format!("vike-cli config set policy.venues.{} demo", venue.slug())),
            "the other way out — dropping back to demo — must be named too, because an \
                 operator who has no live keys yet wants that one: {main_remedy}"
        );

        // The two states must not print the same sentence: the whole failure was one string
        // serving both.
        assert_ne!(
            demo_remedy,
            main_remedy,
            "{} prints the SAME remedy in both states, which is the defect",
            venue.slug()
        );
    }
}

/// **Aster's PAPER remedy must speak ITS credential model, not the flag venues'.**
///
/// The generic remedy vocabulary is wrong for aster three separate ways, and each wrong word
/// sends an operator hunting a value that does not exist: there is no `ASTER_DEMO_*` tier
/// (the non-mainnet spelling is `TESTNET`), there is no `_API_KEY`/`_API_SECRET` shape (the
/// venue discontinued HMAC keys — the credential is an agent-wallet `_USER`/`_PRIVATE_KEY`
/// pair, `vike_aster::signing::load_aster_credentials`), and there is no `ASTER_MAINNET` flag
/// to set or unset. The one hazard the remedy MUST carry instead: `make_engine` resolves the
/// LIVE tier FIRST, so provisioning `ASTER_LIVE_*` arms REAL-MONEY MAINNET exec — the
/// credential tier IS the network choice, UNDER the ceiling: only a `live`
/// `policy.venues.aster` lets the LIVE pair be tried at all (decision 0095).
///
/// Only the `mainnet = false` paper state is asserted because it is the only reachable one:
/// aster's mainnet verdict is "LIVE creds present" ([`cex_mainnet_enabled`]), and present LIVE
/// creds make exec LIVE — `remedy = None`.
#[test]
fn the_aster_remedy_names_the_agent_wallet_keys_and_the_live_first_hazard() {
    let r = cex_arming(CexVenue::Aster, false, false).remedy.expect("paper");
    assert!(
        r.contains("ASTER_TESTNET_USER") && r.contains("ASTER_TESTNET_PRIVATE_KEY"),
        "the SAFE tier to advise is testnet, in the agent-wallet key shape: {r}"
    );
    assert!(
        r.contains("ASTER_LIVE_USER") && r.contains("REAL-MONEY MAINNET"),
        "…and it must say what the OTHER tier arms, because LIVE-first resolution makes \
             adding those keys a real-money decision: {r}"
    );
    assert!(
        r.contains("policy.venues.aster") && r.contains("`live` ceiling"),
        "…but ONLY under a `live` ceiling: below it `mountable_tier_for_account` deletes the \
             LIVE attempt outright and reads the testnet pair alone, so a remedy that says \
             unconditionally 'adding these arms MAINNET' names a hazard the ceiling already \
             closed and hides the one row that opens it: {r}"
    );
    assert!(
        !r.contains("_API_KEY") && !r.contains("_API_SECRET"),
        "aster has no HMAC key shape; naming one sends the operator after a value that does \
             not exist: {r}"
    );
    // `"DEMO"` and not an `ASTER_DEMO*` key spelling, deliberately twice over: it is the
    // STRONGER ban (no DEMO-tier vocabulary at all, not merely no one key), and a whole-literal
    // `ASTER_`-prefixed spelling here would read as an env key to the settings-registry
    // scanner's literal sweep (`vike_model::scan::find_map_lookups`), demanding a registry row
    // for fixture data — the exact #1114 shape.
    assert!(
        !r.contains("DEMO") && !r.contains("UNSET ASTER_MAINNET"),
        "no DEMO tier and no MAINNET flag exist for aster — the flag venues' vocabulary is \
             exactly the unreachable advice this remedy exists to avoid: {r}"
    );
}

/// OKX v5 signs every request with a passphrase; binance and bybit use none (and aster's
/// agent-wallet model has no passphrase concept at all). The remedy must
/// name the keys that venue actually needs — no more (a key that venue has no use for sends an
/// operator hunting a value that does not exist) and no fewer (an OKX mount with key+secret and
/// no passphrase loads credentials, mounts LIVE, and then fails every signed call).
#[test]
fn the_remedy_names_the_passphrase_only_where_the_venue_signs_with_one() {
    for mainnet in [false, true] {
        let okx = cex_arming(CexVenue::Okx, mainnet, false).remedy.expect("paper");
        assert!(okx.contains("_API_PASSPHRASE"), "okx v5 requires a passphrase: {okx}");
        for venue in [CexVenue::Binance, CexVenue::Bybit, CexVenue::Aster] {
            let r = cex_arming(venue, mainnet, false).remedy.expect("paper");
            assert!(
                !r.contains("_API_PASSPHRASE"),
                "{} uses no passphrase; naming one sends the operator after a value that does \
                     not exist: {r}",
                venue.slug()
            );
        }
    }
}

/// **Every CEX venue drives BOTH maker verbs, bybit included.**
///
/// ⚠ The defect this pins: the mount used to log `quote_lane = "on_order_book"` for bybit and
/// `"on_quote_tick"` for the other two, on the claim that "bybit publishes no L1 quote lane at
/// all". That claim was FALSE. `spawn_bybit_market_data`'s `MdEvent::BookUpdated` arm sends
/// `ticks.book(..)` and then `quote_from_book(&book, symbol)` — a real `QuoteUpdate` on the same
/// core tick lane binance and okx use. The `venue_caps` row it cited (`live_data.quotes = false`)
/// describes the `DataClient::subscribe_quotes` seam, which this pump does not go through.
///
/// A false capability claim in a runtime log FIELD is the same class as a false `LIVE_CAPABLE`
/// row: an operator reads it as a measurement of the running system and debugs the wrong lane.
#[test]
fn every_cex_venue_announces_both_requote_lanes() {
    for venue in CexVenue::ALL {
        let arming = cex_arming(venue, false, true);
        assert!(
            arming.requote_lanes.contains("on_quote_tick"),
            "{} publishes a QuoteUpdate on the core tick lane, so it drives on_quote_tick: {}",
            venue.slug(),
            arming.requote_lanes
        );
        assert!(
            arming.requote_lanes.contains("on_order_book"),
            "{} publishes a BookUpdate too, so it drives on_order_book: {}",
            venue.slug(),
            arming.requote_lanes
        );
        // ⚠ The lanes are venue-INDEPENDENT. A per-venue lane string is exactly the shape the
        // false claim took, so equality across the roster is asserted rather than left implicit.
        assert_eq!(
            arming.requote_lanes,
            cex_arming(CexVenue::Binance, false, true).requote_lanes,
            "{} must announce the SAME lanes as binance — all three publish quote, trade and \
                 book",
            venue.slug()
        );
    }
}

/// The quote's PROVENANCE is still per-venue, and still worth logging — it is what an operator
/// checks when the quote lane is silent. bybit's is derived, so it is the one that can go quiet
/// while the book lane stays live (`quote_from_book` returns `None` on a one-sided book).
#[test]
fn the_quote_source_distinguishes_derived_from_native() {
    let bybit = cex_arming(CexVenue::Bybit, false, true).quote_source;
    assert!(
        bybit.contains("DERIVED") && bybit.contains("quote_from_book"),
        "bybit's quote is folded out of orderbook.50 by the pump: {bybit}"
    );
    for venue in [CexVenue::Binance, CexVenue::Okx, CexVenue::Aster] {
        let src = cex_arming(venue, false, true).quote_source;
        assert!(
            src.contains("native"),
            "{} decodes a native top-of-book channel: {src}",
            venue.slug()
        );
        assert_ne!(src, bybit, "provenance must still distinguish the venues");
    }
    // The fork and its template are the SAME channel on different host families — the field
    // must still say which one a silent quote lane should be debugged against.
    assert_ne!(
        cex_arming(CexVenue::Aster, false, true).quote_source,
        cex_arming(CexVenue::Binance, false, true).quote_source,
        "aster's row must be distinguishable from binance's"
    );
}

/// The hyperliquid and polymarket arms stay on the EMPTY map — byte-identically to before the
/// CEX arm existed. Pinned rather than assumed: the tempting "just collect every status handle
/// in scope" change would silently start suppressing reconcile passes on venues nobody assessed,
/// and it would look like a tidy-up in review.
#[test]
fn the_non_cex_arms_stay_on_the_empty_health_map() {
    let sink: Arc<dyn LiveDataSink> = Arc::new(NullSink);
    let hl = LiveFeeds::Hyperliquid(vike_hyperliquid::market_feed::Feeds::new(sink, || {}));
    assert!(
        hl.recon_feed_statuses().is_empty(),
        "hyperliquid keeps the exec-only-venue shape (every venue reads Healthy)"
    );
}

/// **The credential store is read ONCE per process, however many callers ask for it.**
///
/// A clean install found this as a doubled log line: with a 0644 store, the daemon printed the
/// identical `readable beyond its owner` WARN twice per start, because
/// `try_load_workspace_secrets_at` surfaces the store's permission finding on EVERY invocation
/// (deliberately — "no caller can forget to surface it") and four call sites each performed a
/// complete, independent load. A warning that repeats reads as two findings.
///
/// The doubled line was the symptom; the read was the defect. This asserts the fix at the
/// property, not at the log: after any call, [`CREDENTIALS`] is populated, so a later caller
/// takes the cached map instead of re-opening a plaintext file full of live venue secrets. Drop
/// the memoization and this fails — the `OnceLock` stays empty while the map still comes back.
///
/// ⚠ It reads the REAL store for this working directory, which under `cargo test` is
/// `crates/vike-tradehub/` — a crate directory with no `settings/` in it, so the load resolves
/// `Source::None` and an empty map. Nothing here asserts the CONTENTS, precisely so the test says
/// the same thing on a developer box, on CI, and on a production checkout.
#[test]
fn the_credential_store_is_read_once_however_many_callers_ask() {
    let first = workspace_credentials();
    assert!(
        CREDENTIALS.get().is_some(),
        "the store read must be memoized — an unmemoized `workspace_credentials` re-opens the \
             credential file, and re-emits its permission warning, once per caller"
    );
    let second = workspace_credentials();
    assert_eq!(first, second, "two callers must see the same credentials");
    assert!(
        std::ptr::eq(CREDENTIALS.get().unwrap(), CREDENTIALS.get().unwrap()),
        "one stored map, cloned per caller — not one load per caller"
    );
    // ⚠ ...and the VERDICT rides the same memo, which is the property that keeps the banner
    // honest: `workspace_credentials_checked` returns the pair from ONE open, so the map a
    // mount armed from and the health the ready banner renders can never describe two
    // different reads of two different stores.
    assert!(
        std::ptr::eq(credential_store_health(), &CREDENTIALS.get().unwrap().1),
        "the health must come from the memoized read, not from a second open"
    );
}
