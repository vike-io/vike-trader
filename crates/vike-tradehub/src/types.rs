//! Shared per-venue types the daemon's two compilation units both need to name.
//!
//! [`CexVenue`] and [`VenuePlan`] used to live in `main.rs` alone. Moving the plan-construction
//! functions ([`crate::venue_plan::cex_plan`] and its siblings) into this crate's LIBRARY so this
//! crate's integration tests can reach them (an integration test cannot see a bin crate's private
//! items at all — `crates/vike-tradehub/CLAUDE.md`'s testable-internals rule) forced these two
//! types to follow: a library crate cannot name a type owned by the binary that depends on it —
//! the dependency edge runs the other way. `main.rs` still owns every DISPATCH site (feed wiring,
//! arming, its own test suite); only the type definitions moved here.

/// The CENTRALIZED-EXCHANGE venues wired for live: binance, bybit, okx — and aster, the
/// binance-fork fourth (split-plane I9). One enum instead of near-identical
/// `VenuePlan`/`LiveFeeds` variant pairs, because all of them carry the SAME
/// two-feed shape (see `CexTicks`, in `main.rs`) and differ only in which crate's function is called.
///
/// Deliberately NOT a string: the venue is matched ONCE, in `live_mount`'s allow-list gate, and
/// every later dispatch is on this closed enum — so a further CEX venue is a compile error at each
/// site that must handle it rather than a string comparison somebody forgets to add.
///
/// ⚠ **Aster is the REAL-MONEY-IN-PRACTICE member.** Its exec tier is credential-resolved
/// MAINNET-FIRST (`vike_mount::make_engine`'s `("aster", _)` arm prefers `ASTER_LIVE_*` over
/// `ASTER_TESTNET_*` — no `{VENUE}_MAINNET` flag exists for it), and only the LIVE tier is
/// configured in practice. A testnet exists — the constraint is credentials, not endpoints
/// (`crates/bridges/aster/CLAUDE.md`'s correction record). See `cex_mainnet_enabled`'s aster arm
/// and `cex_arming`'s aster remedy (both in `main.rs`) for how that difference reaches the operator.
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
    /// ⚠ No longer `#[cfg(test)]`: that gate relied on `CexVenue` and its only consumer
    /// (`main.rs`'s own `#[cfg(test)] mod tests`) being compiled together in ONE crate. Now that
    /// this type lives in the LIBRARY and `main.rs`'s tests reach it as an external dependency, a
    /// `cfg(test)` here would only apply when the library itself is the test target — never when
    /// it is linked normally into the binary's own test build — so the roster would vanish from
    /// exactly the tests it exists for. It is four `Copy` variants; compiling it unconditionally
    /// costs nothing.
    pub const ALL: [CexVenue; 4] =
        [CexVenue::Binance, CexVenue::Bybit, CexVenue::Okx, CexVenue::Aster];

    /// The venue slug — the SAME string `vike_run::WIRED_MARKETS`, `make_engine` and the
    /// reconcile feed-status map key on. One definition, so the feed and the engine cannot
    /// disagree about which venue this mount is.
    ///
    /// ⚠ **This table is PINNED against each bridge crate's OWN public venue string**
    /// (`cex_venue_slugs_are_pinned_to_the_bridge_crates_own_venue_string`), and that pin is not
    /// decoration. Every OTHER slug assertion in this file —
    /// `every_cex_venue_slug_names_a_wired_market`, `cex_plan_accepts_the_wired_symbol`, the
    /// feed-status key — asks THIS function what it thinks and then checks the answer against
    /// itself, so every one of them stays green under any PERMUTATION of the three strings.
    /// MEASURED on the CI box: rewriting `CexVenue::Binance => "binance"` to `"bybit"` left the ENTIRE
    /// vike-tradehub suite passing — the declaration-pinning failure mode this repo has already been
    /// bitten by three times.
    ///
    /// The live consequence is not cosmetic. The slug is what `make_engine` keys the
    /// `ExecutionClient`, the credential prefix and the `{VENUE}_MAINNET` switch on, while the feed
    /// arm below dispatches on the ENUM — so a permuted slug subscribes one venue's book and signs
    /// against a DIFFERENT venue's account.
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
    /// onto the core tick lane, so all of them drive `on_quote_tick`. See `CexTicks` (in `main.rs`) for the
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

/// Per-venue pre-build state resolved by the allow-list gate, so the venue string is matched ONCE
/// (before the core spawns — an unwired venue never wastes a core). The feed wiring then dispatches on
/// this, not the string. Hyperliquid carries its resolved testnet/mainnet network; Polymarket needs no
/// pre-build state (no testnet, and its exec gates live inside `build_node`); the CEX venues carry
/// which of the four they are; the CREDENTIALED-DATA venues (split-plane I9 — alpaca, ctrader and
/// oanda, i.e. every variant below holding a config) carry the resolved DATA credentials
/// themselves, because their feed cannot be built without them and `vars` is moved into the
/// `NodeConfig` before the feed block runs.
///
/// `Debug` so a gate's refusal can name what it planned instead — the allow-list tests assert on
/// the plan, and an opaque value makes a failure report say nothing. ⚠ Every credential-carrying
/// variant stays `Debug`-safe by CONSTRUCTION, not by luck: `AlpacaConfig`, `CtraderConfig` and
/// `OandaConfig` each implement a MANUAL `Debug` that redacts the secret fields (their own crates'
/// tests pin it), which is precisely why the variants hold those types rather than raw key
/// strings.
#[derive(Debug)]
pub enum VenuePlan {
    Hyperliquid(vike_hyperliquid::config::Network),
    #[cfg(feature = "polymarket")]
    Polymarket,
    /// Which of the three CEX venues, and the `{VENUE}_MAINNET` verdict resolved at the gate (where
    /// `vars` is still alive — it is moved into the `NodeConfig` before the feed block runs). The
    /// mount ANNOUNCEMENT needs the network, and reading the flag a second time later would be a
    /// second opinion rather than a report; see `cex_mainnet_enabled` (in `main.rs`).
    Cex {
        venue: CexVenue,
        mainnet: bool,
    },
    /// Alpaca (split-plane I9): the SANDBOX-tier OAuth2 config the DATA connection authenticates
    /// with, resolved by [`crate::venue_plan::alpaca_plan`] via `vike_alpaca::load_alpaca_config_from(Demo, vars)` —
    /// the SAME loader + tier `vike_mount::make_engine`'s `("alpaca", _)` exec arm reads, from the
    /// same one vars map, so the feed and exec cannot resolve different credentials — except by
    /// DECLARATION: a `data_only = true` profile has `withhold_exec_credentials` (in `main.rs`) strip the
    /// venue's keys from the map AFTER this plan resolved, so exec reads absence (the paper
    /// fallback) while this variant carries the feed's credentials. Boxed to keep
    /// the enum small (`clippy::large_enum_variant`).
    Alpaca(Box<vike_alpaca::AlpacaConfig>),
    /// cTrader (split-plane I9): the DEMO-tier OAuth config the dedicated DATA socket's
    /// `connect_and_auth` handshake uses, resolved by [`crate::venue_plan::ctrader_plan`] via
    /// `vike_ctrader::config::CtraderConfig::from_vars(Demo, vars)` — again the exec arm's own
    /// loader + tier over the same map.
    Ctrader(Box<vike_ctrader::config::CtraderConfig>),
    /// OANDA (split-plane I9): the PRACTICE-tier Bearer session BOTH feed lanes authenticate with
    /// — the chunked-HTTP pricing stream and the candles REST poll alike — resolved by
    /// [`crate::venue_plan::oanda_plan`] via `vike_oanda::load_oanda_config_from(Environment::Demo, vars)`, again the
    /// exec arm's own loader and tier over the same one map. Boxed like its two siblings: the
    /// variant carries a bearer token, and a lopsided enum would be paid at every plan site
    /// (`clippy::large_enum_variant`'s argument, applied before it bites).
    ///
    /// `Debug`-safe by CONSTRUCTION, exactly like `AlpacaConfig`/`CtraderConfig`:
    /// `vike_oanda::OandaConfig` implements a MANUAL `Debug` that prints the account id and the
    /// REST base and never the token (its own crate's `gate_hosts_and_no_token_leak` pins it),
    /// which is why this variant holds that type rather than the raw string.
    Oanda(Box<vike_oanda::OandaConfig>),
    /// deribit (split-plane I9). A UNIT variant, and the absence is the fact: this venue's four
    /// market lanes are KEYLESS public MAINNET reads on one hardcoded host, so there is no
    /// credential to carry past the `vars` move and no network to resolve — unlike hyperliquid,
    /// which carries its testnet/mainnet verdict, and unlike the three credentialed-data variants
    /// above. Everything `wire_venue_feeds` (in `main.rs`) needs is in `cfgs`. The venue's EXEC network is not
    /// carried either, for the same reason it is not a choice: every authed socket the bridge
    /// opens is hardcoded testnet (`deribit_arming`, in `main.rs`, states it).
    Deribit,
    /// IG (split-plane I9): the DEMO-gateway login BOTH the feed's Lightstreamer sessions and the
    /// exec side authenticate with, resolved by [`crate::venue_plan::ig_plan`] via
    /// `vike_ig::load_ig_config_from(Environment::Demo, vars)` — again the exec arm's own loader
    /// and tier over the same one map. Boxed like its siblings: the variant carries an api key,
    /// an identifier and a password, and a lopsided enum would be paid at every plan site
    /// (`clippy::large_enum_variant`'s argument, applied before it bites).
    ///
    /// `Debug`-safe by CONSTRUCTION, exactly like `AlpacaConfig`/`CtraderConfig`/`OandaConfig`:
    /// `vike_ig::IgConfig` implements a MANUAL `Debug` that prints the REST base and NONE of the
    /// three secrets (its own crate's `gate_and_no_secret_leak` pins it), which is why this
    /// variant holds that type rather than raw strings.
    Ig(Box<vike_ig::IgConfig>),
}

/// One RESOLVED mount (split-plane I10): a profile row lowered and its strategy constructed — the
/// unit `main` produces once per row (exactly one for the historical single-mount profile) and the
/// two mount arms consume. `row` stays alongside the lowerings because the wire rows and the
/// startup echo render from it (`strategy_name` / `effective_params`).
///
/// Moved out of `main.rs` for the same reason `CexVenue`/`VenuePlan` did (main-split Task 3):
/// [`crate::feeds::check_poly_token_intervals`] needs to name this type, and a library crate
/// cannot name a type owned by the binary that depends on it. `main.rs` still owns every
/// construction/destructuring site (`main`'s mount-resolution loop, `mounts_wire_params`, its own
/// `#[cfg(test)]` suite and `feed_splice_seam_tests`) — only the type definition moved here, so
/// every field is `pub`.
pub struct ResolvedMount {
    pub row: crate::config::DaemonProfile,
    pub cfg: vike_run::MakerMountConfig,
    pub spec: vike_run::MountSpec,
    pub strategy: Box<dyn vike_model::Strategy<vike_core::LiveBroker> + Send>,
}
