//! The CEX live arm, its health map and the mount announcement (`cex_arming`).

use super::*;

// ---------------------------------------------------------------------------------------------
// The CEX (binance/bybit/okx) live arm.
// ---------------------------------------------------------------------------------------------

/// A `MakerConfig` for `venue` on the symbol `assemble_runtime` actually mounts it on — the same
/// lowering `DaemonProfile::to_maker_config` produces for a CEX profile.
fn cex_cfg(venue: CexVenue) -> MakerConfig {
    wired_1m_cfg(venue.slug(), 0.5, 0.001)
}

/// **The slug table, pinned against an authority that is NOT `slug()` itself.**
///
/// ⚠ This is the missing gate, and its absence was MEASURED: on the CI box, rewriting
/// `CexVenue::Binance => "binance"` to `"bybit"` left the ENTIRE vike-tradehub suite GREEN.
/// Every other slug assertion here — `every_cex_venue_slug_names_a_started_market`,
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
                 and the venue's account rows on this slug, so a disagreement mounts one venue's \
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

/// Every CEX venue's slug must be a venue `assemble_runtime` actually mounts an engine for, and the
/// slug must be the string that table keys on. Cheap, but it is the join between the FEED half
/// (this file) and the EXEC half (`vike_tradehub_core::wired_markets`): a typo'd slug would give a feed
/// with no engine behind it, and every order would vanish at `accepts_symbol`.
///
/// ⚠ This one CANNOT see a permuted slug — every string it checks against contains all three.
/// `cex_venue_slugs_are_pinned_to_the_bridge_crates_own_venue_string` above is that gate.
#[test]
fn every_cex_venue_slug_names_a_started_market() {
    for venue in CexVenue::ALL {
        assert_wired_and_advertised(venue.slug());
    }
}

/// The plan gate ACCEPTS the wired pair for each of the three venues, and CARRIES the resolved
/// tier-decided (decisions 0095 and 0119) network verdict through to the feed block.
///
/// The verdict has to travel in the plan: `vars` is moved into the `RuntimeConfig` before the feed
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
/// `vike_tradehub_core::make_engine` wires no `extra_symbols`, so
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
        assert!(err.contains(&wired), "names the symbol assemble_runtime mounts: {err}");
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
            vike_model::AccountTier::Live,
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
/// what ties `CexBars::slug` to `CexVenue::slug` and to `vike_tradehub_core::wired_markets`.
#[test]
fn the_cex_health_map_carries_only_the_started_venues_own_row() {
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
            vike_tradehub_core::ReconHealth::Degraded,
            "a disconnected feed must degrade THIS venue's reconcile health"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// The mount ANNOUNCEMENT (`cex_arming`) — the decisions the feed block makes about what to tell
// the operator. `live_runtime` itself cannot be called from a test (it is a `main.rs` function
// that spawns a core, opens two sockets and reads the credential store), which is why these
// decisions were extracted; before that, NOTHING exercised the block and both defects below
// shipped inside it.
// ---------------------------------------------------------------------------------------------

/// **The LIVE announcement must name the NETWORK.** "EXEC IS LIVE" is a materially different
/// statement on demo than on mainnet, and both states are reachable from the same profile by one
/// environment variable. The old line said neither.
#[test]
fn a_live_cex_start_announces_which_network_it_trades_on() {
    for venue in CexVenue::ALL {
        let demo = cex_arming(venue, false, true);
        assert_eq!(demo.exec, "LIVE");
        // Aster spells its non-mainnet network `TESTNET`, the venue's own word for it; the flag
        // venues keep their `DEMO` spelling.
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
/// ⚠ The defect this pins: the old warning advised the DEMO key set unconditionally. The network
/// is the account's own tier (decisions 0095 and 0119) and an account holds ONE credential set, at
/// that one tier (decision 0156 item 6) — so in the `live`-tier + incomplete-set state, which is
/// exactly the state an operator hits while going live, advice to make a demo account arms nothing
/// for THIS account (a live account never trades demo). Following it produces the identical PAPER
/// mount and the identical warning, with nothing to distinguish the second attempt from the first.
/// An instruction that cannot work is worse than no instruction.
///
/// The remedy names the account's FIELDS and the verb that writes them, never a composed credential
/// name: no name carries a tier any more.
#[test]
fn the_paper_remedy_names_the_tier_make_engine_will_actually_read() {
    for venue in CexVenue::ALL {
        if venue == CexVenue::Aster {
            // Aster has no `{VENUE}_MAINNET` flag and no `API_KEY` field, so this test's
            // whole flag-venue vocabulary does not apply — its remedy has its own gate below
            // (`the_aster_remedy_names_the_agent_wallet_keys_and_the_live_first_hazard`).
            continue;
        }
        let slug = venue.slug();
        let composed = format!("{}_", slug.to_uppercase());

        // DEMO state: the account is below `live`, so its fields arm demo — advise them, and the
        // account row they belong to (a credential write never creates one).
        let demo = cex_arming(venue, false, false);
        assert_eq!(demo.exec, "PAPER");
        assert_eq!(demo.network, "DEMO");
        let demo_remedy = demo.remedy.expect("a paper mount must say how to arm it");
        assert!(
            demo_remedy.contains("API_KEY + API_SECRET")
                && demo_remedy.contains(&format!("vike-cli secrets set --venue {slug} <FIELD>")),
            "the account's fields, and the verb that writes them: {demo_remedy}"
        );
        assert!(
            demo_remedy
                .contains(&format!("vike-cli secrets account add --venue {slug} --tier demo"))
                && demo_remedy.contains("vike-cli secrets accounts")
                && demo_remedy.contains("vike-cli secrets account activate --id <N>"),
            "the fields alone do not arm an absent, inactive or paper-tier row, so the account \
                 verbs are part of the remedy (decisions 0119 and 0156): {demo_remedy}"
        );
        assert!(
            !demo_remedy.contains(&composed),
            "no credential NAME is composed any more: {demo_remedy}"
        );

        // MAINNET state: the account is `live`, so its fields arm MAINNET and nothing else.
        // Advising a demo account is the bug.
        let main = cex_arming(venue, true, false);
        assert_eq!(main.exec, "PAPER");
        assert_eq!(main.network, "MAINNET");
        let main_remedy = main.remedy.expect("a paper mount must say how to arm it");
        assert!(
            main_remedy.contains("tier is `live`") && main_remedy.contains("MAINNET exec"),
            "a `live` tier ⇒ the account's fields arm MAINNET, so that is what to say: \
                 {main_remedy}"
        );
        assert!(
            !main_remedy.contains("account add"),
            "⚠ THE DEFECT: advising a new demo account to a `live` account's operator arms \
                 NOTHING for this account: {main_remedy}"
        );
        assert!(
            main_remedy.contains("vike-cli secrets account set-tier --id <N> --tier demo"),
            "the other way out — dropping the account back to demo — must be named too, \
                 because an operator who has no live keys yet wants that one: {main_remedy}"
        );
        assert!(
            !main_remedy.contains("policy.venues"),
            "the per-venue ceiling is gone (decision 0119); naming it is unreachable advice: \
                 {main_remedy}"
        );
        assert!(!main_remedy.contains(&composed), "no composed name: {main_remedy}");

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
/// The generic remedy vocabulary is wrong for aster two separate ways, and each wrong word sends
/// an operator hunting a value that does not exist: there is no `API_KEY`/`API_SECRET` field (the
/// venue discontinued HMAC keys — the credential is an agent-wallet `USER`/`PRIVATE_KEY` pair,
/// `vike_aster::signing::load_aster_credentials`), and there is no `ASTER_MAINNET` flag to set or
/// unset. The one hazard the remedy MUST carry instead: the ACCOUNT's tier is the network choice
/// (decisions 0095 and 0119), and a `live` aster account arms REAL-MONEY MAINNET exec at the next
/// restart.
///
/// Both paper states are reachable: `mainnet = false` (the account is below `live`), and
/// `mainnet = true` with an incomplete set — a `live` aster account does NOT RUN, never testnet (it
/// holds ONE key set at its ONE tier), and [`cex_mainnet_enabled`] still answers `live` for it
/// (`LiveCredentialsAbsent`).
#[test]
fn the_aster_remedy_names_the_agent_wallet_keys_and_the_live_first_hazard() {
    // No composed credential name of either tier. Spelled with `concat!`: a whole-literal
    // `ASTER_`-prefixed spelling would read as an env key to the settings-registry scanner's
    // literal sweep (`vike_model::scan::find_map_lookups`) — the exact #1114 shape.
    let composed = [concat!("ASTER", "_TESTNET"), concat!("ASTER", "_LIVE")];

    let r = cex_arming(CexVenue::Aster, false, false).remedy.expect("paper");
    assert!(
        r.contains("USER + PRIVATE_KEY") && r.contains("SIGNER optional"),
        "the account's fields, in the agent-wallet shape: {r}"
    );
    assert!(
        r.contains("vike-cli secrets set --venue aster <FIELD>")
            && r.contains("vike-cli secrets account add --venue aster --tier demo"),
        "…the verb that writes them, and the one that creates the demo account: {r}"
    );
    assert!(
        r.contains("REAL-MONEY MAINNET"),
        "…and it must say what the OTHER tier arms, because moving the account to `live` is a \
             real-money decision: {r}"
    );
    assert!(
        r.contains("vike-cli secrets account set-tier --id <N> --tier live")
            && !r.contains("policy.venues"),
        "…and the account verb that moves the tier, never the deleted per-venue ceiling: {r}"
    );

    // A `live` aster account with an incomplete set: its fields arm mainnet and nothing else, and
    // the way down to testnet is the account's tier.
    let live = cex_arming(CexVenue::Aster, true, false).remedy.expect("paper");
    assert!(
        live.contains("USER + PRIVATE_KEY") && live.contains("REAL-MONEY MAINNET"),
        "a `live` account's fields arm mainnet, so that is what to say: {live}"
    );
    assert!(
        !live.contains("account add")
            && live.contains("vike-cli secrets account set-tier --id <N> --tier demo"),
        "advising a new testnet account to a `live` account is the unreachable advice; the way \
             to testnet is the tier: {live}"
    );
    assert!(!live.contains("API_KEY") && !live.contains("DEMO"), "{live}");
    assert!(
        !r.contains("API_KEY") && !r.contains("API_SECRET"),
        "aster has no HMAC key shape; naming one sends the operator after a value that does \
             not exist: {r}"
    );
    assert!(
        !r.contains("DEMO") && !r.contains("UNSET ASTER_MAINNET"),
        "no DEMO network and no MAINNET flag exist for aster — the flag venues' vocabulary is \
             exactly the unreachable advice this remedy exists to avoid: {r}"
    );
    for name in composed {
        assert!(!r.contains(name) && !live.contains(name), "no composed name ({name}): {r}");
    }
}

/// OKX v5 signs every request with a passphrase; binance and bybit use none (and aster's
/// agent-wallet model has no passphrase concept at all). The remedy must
/// name the fields that venue actually needs — no more (a field that venue has no use for sends an
/// operator hunting a value that does not exist) and no fewer (an OKX mount with key+secret and
/// no passphrase is refused by its loader, `vike_secrets::credentials::api_credentials`).
#[test]
fn the_remedy_names_the_passphrase_only_where_the_venue_signs_with_one() {
    for mainnet in [false, true] {
        let okx = cex_arming(CexVenue::Okx, mainnet, false).remedy.expect("paper");
        assert!(okx.contains("API_PASSPHRASE"), "okx v5 requires a passphrase: {okx}");
        for venue in [CexVenue::Binance, CexVenue::Bybit, CexVenue::Aster] {
            let r = cex_arming(venue, mainnet, false).remedy.expect("paper");
            assert!(
                !r.contains("API_PASSPHRASE"),
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
    let hl = LiveFeeds::Hyperliquid(vike_hyperliquid::feeds::market_feed::Feeds::new(sink, || {}));
    assert!(
        hl.recon_feed_statuses().is_empty(),
        "hyperliquid keeps the exec-only-venue shape (every venue reads Healthy)"
    );
}

/// **The credential store is read ONCE per process, however many callers ask for it.**
///
/// A clean install found this as a doubled log line: with a 0644 store, the daemon printed the
/// identical `readable beyond its owner` WARN twice per start, because
/// the old whole-map loader surfaced the store's permission finding on EVERY invocation
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
    // honest: `workspace_credentials_checked` returns both from ONE open, so the map a
    // mount armed from and the health the ready banner renders can never describe two
    // different reads of two different stores.
    assert!(
        std::ptr::eq(credential_store_health(), &CREDENTIALS.get().unwrap().health),
        "the health must come from the memoized read, not from a second open"
    );
}
