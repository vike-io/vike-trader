//! Shared per-venue types the daemon's two compilation units both need to name.
//!
//! [`CexVenue`] and [`VenuePlan`] used to live in the daemon binary alone. Moving the plan-construction
//! functions ([`crate::venue_plan::cex_plan`] and its siblings) into this crate's LIBRARY so this
//! crate's integration tests can reach them (an integration test cannot see a bin crate's private
//! items at all — `crates/vike-tradehub/CLAUDE.md`'s testable-internals rule) forced these two
//! types to follow: a library crate cannot name a type owned by the binary that depends on it —
//! the dependency edge runs the other way. The DISPATCH sites are `crates/vike-tradehub/src/feeds.rs`'s
//! `venue_feed_plan` / `wire_venue_feeds` (feed wiring) and `crates/vike-tradehub/src/venue_arming.rs`
//! (arming); only the type definitions live here.

/// The CENTRALIZED-EXCHANGE venues wired for live: binance, bybit, okx — and aster, the
/// binance-fork fourth (split-plane I9). One enum instead of near-identical
/// `VenuePlan`/`LiveFeeds` variant pairs, because all of them carry the SAME two-lane shape (the kline lane and the
/// tick pump, `crates/vike-tradehub/src/feeds/cex.rs`) and differ only in the venue they name.
///
/// Deliberately NOT a string: the venue is matched ONCE, in `live_runtime`'s allow-list gate, and
/// every later dispatch is on this closed enum — so a further CEX venue is a compile error at each
/// site that must handle it rather than a string comparison somebody forgets to add.
///
/// ⚠ **Aster is the REAL-MONEY-IN-PRACTICE member.** Its exec network is the account's own tier
/// (`AsterVenueAdapter` in `crates/bridges/aster/src/adapter.rs` — no `{VENUE}_MAINNET` flag exists for
/// it), and only a `live` aster account holds keys in practice. A testnet exists — the constraint is credentials, not endpoints
/// (`crates/bridges/aster/CLAUDE.md`'s correction record). See `venue_arming::cex_mainnet_enabled`
/// (which asks aster's registry row for the tier) and `venue_arming::cex_arming`'s aster remedy for
/// how that difference reaches the operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CexVenue {
    Binance,
    Bybit,
    Okx,
    Aster,
}

impl CexVenue {
    /// Every variant. Tests iterate THIS rather than hand-listing the three, so a fourth CEX venue
    /// joins every existing gate the moment its variant exists, instead of the day somebody
    /// remembers to extend a literal array in each test.
    ///
    /// ⚠ No longer `#[cfg(test)]`: that gate relied on `CexVenue` and its only consumer (the old
    /// bin's own `#[cfg(test)] mod tests`) being compiled together in ONE crate. Now that this type
    /// lives in the LIBRARY and `crates/vike-tradehub/tests/` reaches it as an external dependency, a
    /// `cfg(test)` here would only apply when the library itself is the test target — never when
    /// it is linked normally into an integration test's build — so the roster would vanish from
    /// exactly the tests it exists for. It is four `Copy` variants; compiling it unconditionally
    /// costs nothing.
    pub const ALL: [CexVenue; 4] =
        [CexVenue::Binance, CexVenue::Bybit, CexVenue::Okx, CexVenue::Aster];

    /// The venue slug — the SAME string `crates/vike-venues/src/venue_wiring.rs`'s `VENUE_WIRING`, `make_engine` and the
    /// reconcile feed-status map key on. One definition, so the feed and the engine cannot
    /// disagree about which venue this mount is.
    ///
    /// ⚠ **This table is PINNED against each bridge crate's OWN public venue string**
    /// (`cex_venue_slugs_are_pinned_to_the_bridge_crates_own_venue_string`), and that pin is not
    /// decoration. Every OTHER slug assertion in this file —
    /// `every_cex_venue_slug_names_a_started_market`, `cex_plan_accepts_the_wired_symbol`, the
    /// feed-status key — asks THIS function what it thinks and then checks the answer against
    /// itself, so every one of them stays green under any PERMUTATION of the three strings.
    /// MEASURED on the CI box: rewriting `CexVenue::Binance => "binance"` to `"bybit"` left the ENTIRE
    /// vike-tradehub suite passing — the declaration-pinning failure mode this repo has already been
    /// bitten by three times.
    ///
    /// The live consequence is not cosmetic. The slug is what `make_engine` keys the
    /// `ExecutionClient` and the venue's `account` rows (which arm it, and whose credentials it
    /// signs with) on,
    /// while the feed arm below dispatches on the ENUM — so a permuted slug subscribes one venue's
    /// book and signs against a DIFFERENT venue's account.
    pub fn slug(self) -> &'static str {
        match self {
            CexVenue::Binance => "binance",
            CexVenue::Bybit => "bybit",
            CexVenue::Okx => "okx",
            CexVenue::Aster => "aster",
        }
    }

    /// WHERE this venue's L1 quote comes from — a native top-of-book channel, or derived from the
    /// L2 book by the pump itself.
    ///
    /// ⚠ Provenance, NOT a capability difference: every CEX venue here publishes a `QuoteUpdate`
    /// onto the core tick lane, so all of them drive `on_quote_tick`. See `feeds/cex.rs`'s `spawn_cex_tick_pump` for the
    /// measurement and for the false claim this replaced.
    pub fn quote_source(self) -> &'static str {
        match self {
            CexVenue::Binance => "@bookTicker (native L1 channel)",
            CexVenue::Bybit => "orderbook.50 -> quote_from_book (DERIVED L1; no native channel)",
            CexVenue::Okx => "bbo-tbt (native L1 channel)",
            // The binance fork's same channel, on the USDⓈ-M futures combined stream — named
            // distinctly from binance's row so the log field still says WHICH host family a silent
            // quote lane should be debugged against.
            CexVenue::Aster => "@bookTicker (native L1 channel, USDⓈ-M futures stream)",
        }
    }
}

/// The network Hyperliquid's feed dials: the tier of the venue's armed accounts, in the datahub's
/// words. Hyperliquid is the one venue whose feed follows the account tier (decision 0095); the
/// strategy lane keys its client by it (`StratOpener::on_network`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HlNetwork {
    Mainnet,
    Testnet,
}

impl HlNetwork {
    /// The network an account's tier reaches (decision 0095, item 3 of 0119): `live` means MAINNET,
    /// every tier below it testnet. [`vike_model::AccountTier::Paper`] never reaches a bridge, and reads as `demo`
    /// does (decision 0129 item 5). The same mapping the bridge's own `Network::for_tier` states and
    /// `crates/vike-tradehub/tests/daemon/feed_network_table.rs` pins this one over every account tier.
    pub fn for_tier(tier: vike_model::AccountTier) -> Self {
        match tier {
            vike_model::AccountTier::Live => HlNetwork::Mainnet,
            vike_model::AccountTier::Demo | vike_model::AccountTier::Paper => HlNetwork::Testnet,
        }
    }

    /// The word the datahub's `StratOpen` carries: `None` for the main network (the request names
    /// no network, and the datahub's one main-network client serves it), `testnet` for the other.
    pub fn strat_network(self) -> Option<&'static str> {
        match self {
            HlNetwork::Mainnet => None,
            HlNetwork::Testnet => Some(vike_datahub_client::proto::STRAT_NETWORK_TESTNET),
        }
    }
}

/// Per-venue pre-build state resolved by the allow-list gate, so the venue string is matched ONCE
/// (before the core spawns — an unwired venue never wastes a core). The feed wiring then dispatches on
/// this, not the string. Hyperliquid carries its resolved testnet/mainnet network; Polymarket needs no
/// pre-build state (no testnet, and its exec gates live inside `assemble_runtime`); the CEX venues carry
/// which of the four they are; the CREDENTIALED-DATA venues (split-plane I9 — alpaca, ctrader, oanda
/// and ig) carry NOTHING: their feed is a credentialed stream at the gatewayhub, which resolves the
/// account itself (`AccountDirectory::data_feed_credentials`, read at the gatewayhub per open), so no
/// credential, label or tier is resolved here or crosses toward the datahub.
///
/// `Debug` so a gate's refusal can name what it planned instead — the allow-list tests assert on
/// the plan, and an opaque value makes a failure report say nothing. The plan holds no secret.
#[derive(Debug)]
pub enum VenuePlan {
    Hyperliquid(HlNetwork),
    Polymarket,
    /// Which of the four CEX venues, and whether the venue's registry row put its DEFAULT account
    /// on the live tier (mainnet), resolved at the gate (where `vars` is still alive — it is moved
    /// into the `RuntimeConfig` before the feed block runs). The mount ANNOUNCEMENT needs the network,
    /// and asking the row a second time later would be a second opinion rather than a report; see
    /// `venue_arming::cex_mainnet_enabled`.
    Cex {
        venue: CexVenue,
        mainnet: bool,
    },
    /// Alpaca (split-plane I9): the OAuth2 sandbox WS client, a credentialed stream at the gatewayhub.
    Alpaca,
    /// cTrader (split-plane I9): the DEMO-tier protobuf data socket, a credentialed stream at the
    /// gatewayhub (its connect-at-open failure refuses the start, as it did).
    Ctrader,
    /// OANDA (split-plane I9): the PRACTICE-tier pricing stream and candles poll, a credentialed
    /// stream at the gatewayhub.
    Oanda,
    /// deribit (split-plane I9). A UNIT variant, and the absence is the fact: this venue's four
    /// market lanes are KEYLESS public MAINNET reads on one hardcoded host, so there is no
    /// credential to carry and no network to resolve — unlike hyperliquid, which carries its
    /// testnet/mainnet verdict. Everything `wire_venue_feeds` (in `feeds.rs`) needs is in `cfgs`. The
    /// venue's EXEC network is not carried either, for the same reason it is not a choice: every
    /// authed socket the bridge opens is hardcoded testnet (`deribit_arming`, in `venue_arming.rs`,
    /// states it).
    Deribit,
    /// IG (split-plane I9): the Lightstreamer TLCP feed, a credentialed stream at the gatewayhub.
    Ig,
}

/// One RESOLVED mount (split-plane I10): a profile row lowered and its strategy constructed — the
/// unit `crates/vike-tradehub/src/tradehub_cli/profile.rs`'s `resolve_mounts` produces once per row (exactly
/// one for the historical single-mount profile) and the two mount arms consume. `row` stays
/// alongside the lowerings because the wire rows and the startup echo render from it
/// (`strategy_name` / `effective_params`).
///
/// Moved out of the old `main.rs` for the same reason `CexVenue`/`VenuePlan` did (main-split Task 3):
/// `crate::feeds::check_poly_token_intervals` (under `polymarket`) needs to name this type, and a library crate
/// cannot name a type owned by the binary that depends on it. `tradehub_cli.rs` still owns the
/// construction/destructuring sites (`resolve_mounts`, `mounts_wire_params`, its own
/// `#[cfg(test)]` suite and `tradehub_cli::tests::feed_splice`) — only the type definition lives here, so
/// every field is `pub`.
pub struct ResolvedAdapter {
    pub row: crate::config::DaemonProfile,
    pub cfg: vike_tradehub_core::MakerConfig,
    pub spec: vike_tradehub_core::MountSpec,
    pub strategy: Box<dyn vike_model::Strategy<vike_core::LiveBroker> + Send>,
}
