//! The per-ACCOUNT exec badge and each venue's arming disclosure (`CexArming` and its producers).

use std::collections::HashSet;

#[cfg(doc)]
use super::tier::cex_mainnet_enabled;
use crate::CexVenue;

// -------------------------------------------------------------------------------------------
// The exec badge is per ACCOUNT, not per venue
// -------------------------------------------------------------------------------------------

/// The `exec` badge for a venue whose EVERY account is on the paper book.
pub(crate) const EXEC_PAPER: &str = "PAPER";

/// The `exec` badge for a venue whose mount account armed real exec and which has no other.
pub(crate) const EXEC_LIVE: &str = "LIVE";

/// The `exec` badge for a venue whose MOUNT account is paper while a LABELLED account of it armed
/// real exec in this same process.
///
/// ⚠ It deliberately does not begin with `PAPER`. `docs/ops/tradehub-the CI box.md` teaches operators
/// to grep this field, and a badge spelled `PAPER (…)` would answer a `exec=PAPER` grep for a
/// venue that IS signing real orders from another account — the under-claim this constant exists
/// to end, wearing a longer string.
pub(crate) const EXEC_OTHER_ACCOUNT_LIVE: &str = "OTHER-ACCOUNT-LIVE";

/// The `exec` badge for a venue where the mount account AND at least one labelled account armed.
/// Begins with `LIVE`, so an `exec=LIVE` grep still finds it.
pub(crate) const EXEC_LIVE_MULTI_ACCOUNT: &str = "LIVE-MULTI-ACCOUNT";

/// **Which of this venue's OTHER accounts armed real exec** — the route keys of every NON-mount
/// account of `venue` that appears in `build_node`'s `live_venues` record, sorted.
///
/// ⚠ **The membership test is `VenueArming::route_key`, and nothing here parses one.**
/// `live_venues` is keyed per ACCOUNT (`vike_mount::armed_live_venues`' own doc), and its labelled
/// entries are `venue#LABEL` — a spelling `vike_model::accounts::account_keys::AccountRef::route_key` owns.
/// Splitting those strings on `#` here would be a second reader of that grammar, and a second
/// reader is what goes stale when the first one changes; folding the ROWS through the same
/// renderer the mount recorded under cannot.
///
/// EMPTY on a box with one account per venue — every row there is a default-account row — so every
/// caller below is byte-identical there.
pub(crate) fn other_live_accounts(
    venue: &str,
    arming: &[vike_config::VenueArming],
    live_venues: &HashSet<String>,
) -> Vec<String> {
    let mut out: Vec<String> = arming
        .iter()
        .filter(|row| row.venue == venue && !row.is_default_account())
        .map(|row| row.route_key())
        .filter(|key| live_venues.contains(key))
        .collect();
    out.sort();
    out
}

/// The four-way `exec` badge, from the two facts that actually differ.
///
/// `mount_live` is the DEFAULT account's — the account every strategy mount on this venue trades
/// through, because `vike_core`'s order path resolves `RouteKey::sole_account_of(venue)`. It is
/// therefore the fact the LIVE/PAPER sentences below are about, and it stays the input to every
/// `*_arming` producer's remedy. `others_live` is the venue-wide fact the badge used to be blind
/// to.
pub(crate) fn exec_badge(mount_live: bool, others_live: bool) -> &'static str {
    match (mount_live, others_live) {
        (true, false) => EXEC_LIVE,
        (true, true) => EXEC_LIVE_MULTI_ACCOUNT,
        (false, false) => EXEC_PAPER,
        (false, true) => EXEC_OTHER_ACCOUNT_LIVE,
    }
}

/// **Correct one venue's announcement for the accounts the venue-keyed badge could not see.**
///
/// The badge used to be `live_venues.contains(slug)` — the DEFAULT account's route key — so a box
/// whose only armed `bybit` account was a labelled one announced `exec = PAPER` beside
/// `NO ORDER FROM THIS MOUNT WILL EVER REACH THE VENUE`, while that process held a live
/// authenticated bybit session and signed real orders through it. Under-claiming, but false.
///
/// ⚠ `others` EMPTY returns `base` **unchanged**, field for field — which is every box with no
/// `[accounts]` table, so this function is a no-op there by construction rather than by care.
///
/// What it does NOT do is promote the PAPER *sentences* to live ones. The mount's own orders
/// really do go to the paper book in that state (`RouteKey::sole_account_of`), so the remedy is
/// still the remedy for arming THIS mount — it is merely prefixed with the fact that makes the
/// paper verdict a statement about one account rather than about the venue.
pub(crate) fn with_other_live_accounts(
    base: CexArming,
    venue: &str,
    others: Vec<String>,
) -> CexArming {
    if others.is_empty() {
        return base;
    }
    let mount_live = base.exec == EXEC_LIVE;
    let named = others.join(", ");
    let remedy = base.remedy.map(|r| {
        format!(
            "⚠ {venue} is ALREADY LIVE in this process on another ACCOUNT ({named}) — the paper \
             verdict above is about the account THIS mount trades ({venue}), not about the venue, \
             and orders from that other account are REAL. To arm this mount too: {r}"
        )
    });
    CexArming { exec: exec_badge(mount_live, true), other_live: others, remedy, ..base }
}

/// What the CEX arm ANNOUNCES at mount: which network the exec side bound to, whether it is a real
/// venue client or the paper exchange, which maker verbs the pump drives, and — when it is PAPER —
/// the remedy that will ACTUALLY arm it.
///
/// (Since split-plane I9 the CREDENTIALED-DATA arms reuse this struct for the same disclosure —
/// [`alpaca_arming`], [`ctrader_arming`] and [`oanda_arming`] are the producers, each documenting
/// where its facts diverge from the CEX vocabulary. The field docs below speak CEX because that is
/// where the defects they record shipped.)
///
/// ⚠ Split out of the feed block and made PURE so it can be tested. `live_mount` is a `main.rs`
/// function that spawns a core, opens two sockets and reads the credential store, so no test can
/// call it; before this existed, nothing exercised the block at all and both of the defects below
/// shipped inside it.
///
/// [`CexArming::remedy`] is the whole reason this is a function rather than two inline `format!`s —
/// see that field.
///
/// ⚠ Its `exec` badge is corrected for the venue's OTHER accounts by
/// [`with_other_live_accounts`] before it is announced — see [`other_live_accounts`].
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CexArming {
    /// What `build_node` ACTUALLY mounted, taken from its own `live_venues` record rather than
    /// from what was configured. One of the four [`exec_badge`] strings — `"LIVE"` / `"PAPER"` on
    /// a box with one account per venue, and never anything else there.
    pub exec: &'static str,
    /// `"MAINNET"` or `"DEMO"` (aster spells its non-mainnet tier `"TESTNET"`, matching its own
    /// credential naming) — WHICH NETWORK the exec side bound to: the DEFAULT account's tier as the
    /// venue's registry row answers it ([`cex_mainnet_enabled`]; decision 0095 made the ceiling the
    /// network for binance/bybit/okx, and aster's row runs its LIVE-first credential chain under
    /// that ceiling).
    ///
    /// ⚠ The LIVE announcement used to omit this entirely: it said "orders from this mount are REAL
    /// orders" without ever saying whether that meant the demo venue or real money. Both states are
    /// reachable and they differ by one environment variable.
    pub network: &'static str,
    /// The maker verbs this venue's pump actually drives. Every CEX venue here publishes quote,
    /// trade and book, so this is the same string for each — stated as a FIELD rather than assumed, because
    /// the claim it replaces was a per-venue one that was false (see [`CexVenue::quote_source`]).
    pub requote_lanes: &'static str,
    /// Where the L1 quote comes from — [`CexVenue::quote_source`].
    pub quote_source: &'static str,
    /// What the operator must change to arm real exec, or `None` when it is already LIVE.
    ///
    /// ⚠ **This is state-dependent, and the version it replaces was UNREACHABLE ADVICE.** The old
    /// PAPER branch said, unconditionally, "add `{VENUE}_DEMO_API_KEY` / `{VENUE}_DEMO_API_SECRET`".
    /// But `make_engine` picks the credential TIER from the ceiling before it looks anything up
    /// (decision 0095) — `let creds = if mainnet { load_credentials_from(venue, Environment::Live, vars) } else {
    /// … Environment::Demo … }` — so with `policy.venues.{venue} = "live"`, a `{VENUE}_DEMO_*` key
    /// set is never consulted and adding one arms NOTHING. An operator following that instruction
    /// gets the identical PAPER mount and the identical warning, with no way to tell why. Naming the
    /// tier that will actually be read (and the ceiling that chose it) is the difference between a
    /// warning and a wild goose chase.
    pub remedy: Option<String>,
    /// The route keys of this venue's OTHER accounts that armed real exec — [`other_live_accounts`].
    ///
    /// ⚠ **Always EMPTY out of the six producers**, and filled only by
    /// [`with_other_live_accounts`], which is where the venue-wide record is consulted. The
    /// producers answer from what THIS mount resolved; a second account is not a fact any of them
    /// is holding, and giving each one the whole `live_venues` record so it could look would be six
    /// readers of a grammar with one owner.
    pub other_live: Vec<String>,
}

/// Resolve [`CexArming`] from the three facts the mount knows. Pure: no env, no store, no clock.
///
/// ⚠ **Aster diverges from the flag venues on every string below, and each divergence is a fact
/// about `make_engine`, not a style choice**: its non-mainnet tier is spelled TESTNET (not DEMO),
/// its keys are the agent-wallet `_USER`/`_PRIVATE_KEY` pair (classic `_API_KEY`/`_API_SECRET` was
/// discontinued by the venue — `vike_aster::signing::load_aster_credentials` is the naming
/// authority, `_SIGNER` optional), and it has NO `{VENUE}_MAINNET` flag: the credential TIER an
/// operator provisions IS the network choice, resolved LIVE-FIRST under a `live` ceiling and
/// TESTNET-only below it (decision 0095). A remedy naming
/// `ASTER_DEMO_API_KEY` or "UNSET ASTER_MAINNET" would be exactly the unreachable-advice defect
/// this function exists to prevent.
/// **WHERE a credential goes.** The remedy names the VERB: `vike-cli secrets set` writes the
/// settings database, the only credential store, and `vike-cli secrets path` prints where it is.
fn add_to_the_store(keys: &str) -> String {
    format!(
        "add {keys} — `vike-cli secrets set <NAME>` writes the settings database, the only \
         credential store, and `vike-cli secrets path` prints where it is"
    )
}

pub(crate) fn cex_arming(venue: CexVenue, mainnet: bool, exec_live: bool) -> CexArming {
    let upper = venue.slug().to_uppercase();
    let is_aster = matches!(venue, CexVenue::Aster);
    // The tier `make_engine` will ACTUALLY consult, as the venue's registry row answers it
    // ([`cex_mainnet_enabled`]: the ceiling for binance/bybit/okx, decision 0095, and for aster the
    // LIVE-first credential resolution under that ceiling) — not the tier the operator may have
    // provisioned. See `CexArming::remedy`.
    let tier = if mainnet {
        "LIVE"
    } else if is_aster {
        "TESTNET"
    } else {
        "DEMO"
    };
    let keys = if is_aster {
        format!(
            "{upper}_{tier}_USER + {upper}_{tier}_PRIVATE_KEY (agent wallet; \
                 {upper}_{tier}_SIGNER optional)"
        )
    } else {
        // The exact key spellings `vike_bridge_core::credentials::load_credentials_from` builds
        // (`{VENUE}_{TIER}_API_KEY` / `_API_SECRET`, plus `_API_PASSPHRASE` where the venue signs
        // with one). OKX v5 requires the passphrase on every signed request; binance and bybit use
        // none, and naming a key that venue has no use for would send an operator looking for a
        // value that does not exist.
        let mut k = format!("{upper}_{tier}_API_KEY + {upper}_{tier}_API_SECRET");
        if matches!(venue, CexVenue::Okx) {
            k.push_str(&format!(" + {upper}_{tier}_API_PASSPHRASE"));
        }
        k
    };
    let remedy = if exec_live {
        None
    } else if is_aster {
        // Reachable only when NO aster tier the ceiling permits resolves: `mainnet` here means
        // "LIVE creds present AND a `live` ceiling to try them under" (the aster row's `resolve`,
        // asked through [`cex_mainnet_enabled`]), and that combination makes exec LIVE — so a
        // PAPER aster mount always advises the TESTNET tier and must say, prominently, what the
        // other tier arms and under WHICH ceiling. Below `live`, the aster bridge's
        // `mountable_tier_for_account` deletes the LIVE attempt outright:
        // ASTER_LIVE_* sitting in the store arms nothing there, so an unconditional "adding these
        // arms MAINNET" would name a hazard the ceiling has not opened and hide the row that does.
        Some(format!(
            "{store} — ⚠ aster has NO {upper}_MAINNET flag: the credential TIER is the network \
             choice, UNDER the ceiling. Below a `live` ceiling (`policy.venues.{slug}`) \
             `make_engine` reads the TESTNET pair alone; under a `live` ceiling it resolves \
             LIVE-FIRST, so adding {upper}_LIVE_USER + {upper}_LIVE_PRIVATE_KEY instead arms \
             REAL-MONEY MAINNET exec",
            store = add_to_the_store(&keys),
            slug = venue.slug()
        ))
    } else if mainnet {
        Some(format!(
            "{store} — ⚠ the ceiling `policy.venues.{slug}` is `live`, so `make_engine` loads the \
             LIVE (mainnet) key set ONLY and a {upper}_DEMO_* key set arms NOTHING; to mount this \
             venue on DEMO instead: `vike-cli config set policy.venues.{slug} demo`",
            store = add_to_the_store(&keys),
            slug = venue.slug()
        ))
    } else {
        Some(add_to_the_store(&keys))
    };
    CexArming {
        exec: exec_badge(exec_live, false),
        other_live: Vec::new(),
        network: if mainnet {
            "MAINNET"
        } else if is_aster {
            "TESTNET"
        } else {
            "DEMO"
        },
        requote_lanes: "on_quote_tick + on_order_book",
        quote_source: venue.quote_source(),
        remedy,
    }
}

/// The arming disclosure for the ALPACA arm — the credentialed-data reuse of [`CexArming`]
/// (split-plane I9). Pure, like [`cex_arming`], so the announcement's claims are testable without
/// a socket. Every field is an alpaca FACT, not a style choice:
///
/// - `network` is always `"SANDBOX"`: the exec mount (`crates/bridges/alpaca/src/mount.rs`'s
///   `AlpacaVenueMount`) loads its config at `Environment::Demo` unconditionally — there is no
///   `ALPACA_MAINNET` flag, and
///   a LIVE-tier flip is a deliberate code change. (Alpaca says "sandbox", not "demo" —
///   `vike_alpaca::alpaca_tier` — so the log speaks the tier the keys are actually named under.)
/// - `requote_lanes` is `on_quote_tick` ONLY: alpaca declares no book lane
///   (`live_data.book = false` — `subscribe_book` is a caps refusal), so the maker's
///   `on_order_book` verb never fires here. Stated because assuming the CEX pair would be the
///   false-claim shape [`CexVenue::quote_source`]'s doc records.
/// - the PAPER remedy exists for honesty but names its own near-unreachability: [`crate::venue_plan::alpaca_plan`]
///   already refused the mount when the SANDBOX trio was absent, and with the trio present
///   `AlpacaExecutionClient::spawn` is INFALLIBLE at mount (auth failures surface later, in the
///   actor) — so `exec = PAPER` here means the exec gate and the feed gate READ DIFFERENT ANSWERS
///   from one map, which is a bug to report, not a key to add. ⚠ ONE DECLARED EXCEPTION: a
///   profile with `data_only = true` makes the two gates read different maps ON PURPOSE
///   ([`vike_mount::startup::withhold_venue_credentials`]) — the wire arm then announces the declaration through
///   [`data_only_arming`] instead of this remedy, so the "bug to report" text is never printed
///   for a state the operator asked for.
pub(crate) fn alpaca_arming(exec_live: bool) -> CexArming {
    let upper = "alpaca".to_uppercase();
    let tier = vike_alpaca::alpaca_tier(vike_bridge_core::credentials::Environment::Demo);
    CexArming {
        exec: exec_badge(exec_live, false),
        other_live: Vec::new(),
        network: tier,
        requote_lanes: "on_quote_tick",
        quote_source: "IEX equities WS (native L1 quote channel, SANDBOX data hosts)",
        remedy: (!exec_live).then(|| {
            format!(
                "the feed gate resolved the {upper}_{tier}_CLIENT_ID/_CLIENT_SECRET/_ACCOUNT_ID \
                 trio (this mount could not have started otherwise) yet `build_node` recorded no \
                 live {upper} exec — the two gates read ONE credential map, so this state is a \
                 daemon bug to report, not a missing key; orders go to the paper book until it is \
                 resolved"
            )
        }),
    }
}

/// The arming disclosure for the CTRADER arm (split-plane I9) — same [`CexArming`] reuse and the
/// same pure-function argument as [`alpaca_arming`]. The ctrader facts:
///
/// - `network` is always `"DEMO"`: cTrader's mount (`crates/bridges/ctrader/src/mount.rs`'s
///   `CtraderVenueMount`) resolves the DEMO-tier grant unconditionally — no `CTRADER_MAINNET` flag
///   exists, and the LIVE token tier is a deliberate code change.
/// - `requote_lanes` is `on_quote_tick` ONLY (no trade-print stream, no book lane — both are caps
///   refusals on `CtraderData`).
/// - the PAPER remedy IS reachable, and differently from every credential remedy above:
///   [`crate::venue_plan::ctrader_plan`] proved the DEMO tokens present, but ctrader's exec client connects
///   SYNCHRONOUSLY inside `make_engine` and a connect/auth failure DEMOTES that venue to paper
///   for the session (dropping its recon handle) — while this feed arm's own, later data connect
///   can still have succeeded. The remedy is therefore a RESTART (retry the exec handshake), not
///   a key. ⚠ ONE DECLARED EXCEPTION: under a `data_only = true` profile no handshake was even
///   attempted ([`vike_mount::startup::withhold_venue_credentials`] kept the tokens from `make_engine`) and a restart
///   would change nothing — the wire arm announces the declaration through
///   [`data_only_arming`] instead of this remedy.
pub(crate) fn ctrader_arming(exec_live: bool) -> CexArming {
    CexArming {
        exec: exec_badge(exec_live, false),
        other_live: Vec::new(),
        network: "DEMO",
        requote_lanes: "on_quote_tick",
        quote_source: "SPOT_EVENT bid/ask over the dedicated protobuf socket (native L1 channel, \
                       demo host)",
        remedy: (!exec_live).then(|| {
            "the DEMO tokens are present (this feed's own socket authenticated with them), but \
             ctrader exec connects SYNCHRONOUSLY at mount and its connect/auth FAILED, so \
             `make_engine` demoted ctrader to PAPER for this session and dropped its recon \
             handle — restart the daemon to retry the exec handshake; until then no order from \
             this mount reaches the venue"
                .to_string()
        }),
    }
}

/// The arming disclosure for the OANDA arm (split-plane I9) — the same [`CexArming`] reuse and the
/// same pure-function argument as [`alpaca_arming`]. The oanda facts:
///
/// - `network` is always `"PRACTICE"`, which is the VENUE's own word for the environment
///   `vike_oanda::mount::OandaVenueMount` binds when it arms: the only tier it ever resolves is
///   Demo, and `vike_oanda::oanda_hosts(Demo)` is the `api-fxpractice` / `stream-fxpractice` pair.
///   There is no `OANDA_MAINNET` flag; the fxTrade tier and its hosts are implemented and tested
///   with NO caller anywhere in this workspace (`crates/bridges/oanda/CLAUDE.md` records that as a
///   live trap), so a live flip is a deliberate code change rather than a config one.
///   ⚠ The CREDENTIAL tier is spelled `DEMO` (`OANDA_DEMO_*`) while the NETWORK is spelled
///   practice — one environment, two vocabularies, both the venue's. The disclosure says
///   `PRACTICE` because the field answers "which network did exec bind", and
///   [`CexArming::remedy`] names the keys, so neither word has to carry the other's job.
/// - `requote_lanes` is `on_quote_tick` ONLY: oanda declares no book lane and no trade tape
///   (`live_data.book`/`.trades` are both false — `subscribe_book`/`subscribe_trades` are caps
///   refusals), so the maker's `on_order_book` verb never fires here. Claiming the CEX pair would
///   be the false-lanes defect [`CexVenue::quote_source`]'s doc records.
/// - the PAPER remedy is alpaca's, not ctrader's, and for alpaca's reason: [`crate::venue_plan::oanda_plan`] already
///   refused the mount when the token/account pair was absent, and with them present
///   `OandaExecutionClient::spawn` is INFALLIBLE at mount (a bad token surfaces later, inside the
///   exec thread's REST calls) — so `exec = PAPER` here means the feed gate and the exec gate read
///   DIFFERENT answers out of one credential map, which is a bug to report rather than a key to
///   add. ⚠ ONE DECLARED EXCEPTION: a `data_only = true` profile makes the gates read different
///   maps ON PURPOSE ([`vike_mount::startup::withhold_venue_credentials`]) — the wire arm then announces the
///   declaration through [`data_only_arming`] instead of this remedy.
pub(crate) fn oanda_arming(exec_live: bool) -> CexArming {
    let upper = "oanda".to_uppercase();
    let tier = vike_bridge_core::credentials::Environment::Demo.as_str();
    CexArming {
        exec: exec_badge(exec_live, false),
        other_live: Vec::new(),
        network: "PRACTICE",
        requote_lanes: "on_quote_tick",
        quote_source: "/pricing/stream chunked-HTTP line stream (native L1 bid/ask, fxPractice \
                       stream host; no market-data WS exists on this venue)",
        remedy: (!exec_live).then(|| {
            format!(
                "the feed gate resolved the {upper}_{tier}_API_KEY/_ACCOUNT_ID pair (this mount \
                 could not have started otherwise) yet `build_node` recorded no live {upper} \
                 exec — the two gates read ONE credential map, so this state is a daemon bug to \
                 report, not a missing key; orders go to the paper book until it is resolved"
            )
        }),
    }
}

/// The arming disclosure for the DERIBIT arm (split-plane I9) — the same [`CexArming`] reuse and
/// the same pure-function argument as [`cex_arming`], but every field diverges from the CEX
/// vocabulary and each divergence is a `vike_mount::make_engine` fact:
///
/// - `network` is always `"TESTNET"`, and unlike every venue above it is not a resolution at all.
///   Deribit was never one of decision 0095's four ceiling-selects-mainnet venues
///   (binance/bybit/okx/hyperliquid only), so no `DERIBIT_MAINNET` read exists anywhere; `cex_mainnet_enabled` has no deribit arm so
///   `make_engine` always loads the DEMO key tier, and every authed socket
///   `crates/bridges/deribit/src/exec.rs` opens is the hardcoded `TESTNET_WS`/`TESTNET_REST` pair.
///   `transport.rs`'s `MAINNET_REST` is referenced by nothing in the workspace: there is no path
///   to deribit MAINNET execution in this tree, by flag or by credential tier, so LIVE-named keys
///   in the store are never read: a store holding only `DERIBIT_LIVE_*` answers paper with
///   `PaperCause::LiveTierNotWired` (`crates/bridges/deribit/src/mount_tests.rs`'s
///   `a_live_key_set_alone_arms_nothing`;
///   `crates/bridges/deribit/CLAUDE.md`'s two-networks section is the authority).
///   ⚠ The CREDENTIAL tier is spelled `DEMO` while the NETWORK is TESTNET — one environment, two
///   vocabularies, exactly the split [`oanda_arming`] documents. The field answers "which network
///   did exec bind"; [`CexArming::remedy`] names the keys.
/// - `requote_lanes` is the full CEX pair, and here it is EARNED rather than inherited:
///   `vike_model::venues::venue_caps::DERIBIT` declares `book: true` and this arm really subscribes the
///   lossless `book.{inst}.100ms` lane onto `LiveDataSink::book`, so `on_order_book` genuinely
///   fires. (`depth: false` is the row that stays refused — see [`crate::feeds::wire_venue_feeds`]' arm.)
/// - `quote_source` names the MAINNET public host, because the two halves of this venue point at
///   different networks and a disclosure that said only "TESTNET" would be a half-truth about
///   where the prices come from.
/// - the PAPER remedy is the CEX one — `add {VENUE}_{TIER}_API_KEY + _API_SECRET` — and it is
///   fully reachable here, unlike the credentialed-data arms' "report a bug": nothing gated the
///   feed on credentials, so a keyless deribit mount with no keys in the store is the ORDINARY
///   unconfigured state. There is no `{VENUE}_MAINNET` caveat to attach, because the flag does not
///   exist for this venue — naming one would be the unreachable-advice defect
///   [`CexArming::remedy`] records.
pub(crate) fn deribit_arming(exec_live: bool) -> CexArming {
    let upper = "deribit".to_uppercase();
    let tier = vike_bridge_core::credentials::Environment::Demo.as_str();
    CexArming {
        exec: exec_badge(exec_live, false),
        other_live: Vec::new(),
        network: "TESTNET",
        requote_lanes: "on_quote_tick + on_order_book",
        quote_source: "quote.{instrument} + book.{instrument}.100ms JSON-RPC channels (native \
                       venue-throttled L1 and a change_id-chained L2 book, KEYLESS public MAINNET \
                       host — the network exec does NOT bind)",
        remedy: (!exec_live).then(|| {
            format!(
                "{store} — ⚠ this venue has NO {upper}_MAINNET flag and no mainnet exec path at \
                 all, so those keys arm TESTNET exec (test.deribit.com) while the feed above keeps \
                 reading the MAINNET book",
                store = add_to_the_store(&format!(
                    "{upper}_{tier}_API_KEY + {upper}_{tier}_API_SECRET"
                ))
            )
        }),
    }
}

/// The arming disclosure for the IG arm (split-plane I9) — the same [`CexArming`] reuse and the
/// same pure-function argument as [`alpaca_arming`]. The IG facts:
///
/// - `network` is always `"DEMO"`, which is both the credential tier and the gateway:
///   `vike_ig::mount::IgVenueMount` resolves `load_ig_config_for_account(Demo, …)`
///   unconditionally and `vike_ig::ig_rest_base(Demo)` is the demo dealing gateway. No `IG_MAINNET`
///   flag exists — IG was never one of decision 0095's four ceiling-selects-mainnet venues — and
///   `Environment::Live` has an
///   implementation with NO caller anywhere in this workspace — so `IG_LIVE_*` keys in the store
///   configure nothing and the venue silently stays paper (`crates/bridges/ig/CLAUDE.md` records
///   that trap). Unlike oanda there is no second vocabulary here: the tier and the network are the
///   same word.
/// - `requote_lanes` is `on_quote_tick` ONLY, and this one is STRUCTURAL rather than a wiring
///   choice: `vike_model::venues::venue_caps::IG` declares `trades: false` and `book: false` because IG is
///   a DEALER venue publishing its own two-sided price — there is no public tape and no L2 ladder
///   to subscribe. It is also why IG stays DEFERRED in
///   `crates/vike-bridge-core/tests/market_data_conformance.rs` even now that it has a live feed:
///   that harness checks three L2-BOOK invariants and there is no book here for them to hold on.
///   Claiming the CEX pair would be the false-lanes defect [`CexVenue::quote_source`]'s doc
///   records.
/// - the PAPER remedy is alpaca's, not ctrader's, and for alpaca's reason: [`crate::venue_plan::ig_plan`] already
///   refused the mount when the trio was absent, and with it present `IgExecutionClient::spawn` is
///   INFALLIBLE at mount (its own module doc says so — an `ExecActor` plus a background stream
///   thread, each self-gating on its own login, with no blocking startup handshake and therefore
///   no connect-failure paper demotion). So `exec = PAPER` here means the feed gate and the exec
///   gate read DIFFERENT answers out of one credential map, which is a bug to report rather than a
///   key to add. ⚠ ONE DECLARED EXCEPTION: a `data_only = true` profile makes the gates read
///   different maps ON PURPOSE ([`vike_mount::startup::withhold_venue_credentials`]) — the wire arm then announces the
///   declaration through [`data_only_arming`] instead of this remedy.
pub(crate) fn ig_arming(exec_live: bool) -> CexArming {
    let upper = "ig".to_uppercase();
    let tier = vike_bridge_core::credentials::Environment::Demo.as_str();
    CexArming {
        exec: exec_badge(exec_live, false),
        other_live: Vec::new(),
        network: tier,
        requote_lanes: "on_quote_tick",
        quote_source: "MARKET:{epic} Lightstreamer TLCP MERGE item (the dealer's own two-sided \
                       BID/OFFER, demo gateway; this venue publishes no ladder and no trade tape)",
        remedy: (!exec_live).then(|| {
            format!(
                "the feed gate resolved the {upper}_{tier}_API_KEY/_IDENTIFIER/_PASSWORD trio \
                 (this mount could not have started otherwise) yet `build_node` recorded no live \
                 {upper} exec — the two gates read ONE credential map, so this state is a daemon \
                 bug to report, not a missing key; orders go to the paper book until it is resolved"
            )
        }),
    }
}

/// The arming disclosure for a DECLARED data-plane-only mount (the `data_only` profile key) —
/// the venue's own PAPER disclosure with ONLY the remedy replaced: `base` is that venue's
/// `*_arming(false)` (so `network`/`requote_lanes`/`quote_source` stay the venue facts their own
/// functions own, never a second copy), and the remedy names the DECLARATION instead of the
/// venue's ordinary paper cause.
///
/// Exists because every credentialed-data venue's ordinary paper remedy is FALSE advice on the
/// declared path: alpaca/oanda/ig say "the two gates read ONE credential map, so this state is a
/// daemon bug to report" — but on a declared mount the gates deliberately read DIFFERENT maps
/// (the plan resolved before [`vike_mount::startup::withhold_venue_credentials`] ran) and the state is exactly what the
/// operator asked for; ctrader says "restart the daemon to retry the exec handshake" — but no
/// handshake failed and a restart repeats the withhold. Pure, like [`cex_arming`], so the
/// announcement's claims are testable without a socket.
pub(crate) fn data_only_arming(base: CexArming, venue: &str) -> CexArming {
    let upper = venue.to_uppercase();
    CexArming {
        remedy: Some(format!(
            "this profile DECLARES `data_only = true` for {venue}: the daemon WITHHELD the \
             {upper}_* credentials from the exec mount, so the workspace live gate (absent \
             credentials ⇒ paper) kept exec on the paper book BY DECLARATION while the \
             credentialed feed above authenticates from the same store — a data-only mount, not a \
             bug; remove `data_only` from the profile to arm demo exec from those credentials"
        )),
        ..base
    }
}
