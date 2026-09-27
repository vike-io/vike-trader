//! `addressing` — **how a caller NAMES an instrument at each venue's DATA path, and whether a bare
//! symbol at that venue can silently resolve to the wrong book.**
//!
//! Phase 1 (STEP 1) of `docs/decisions/0061-an-instrument-names-its-kind.md`: a per-venue table in
//! the playbook's sense — one row per [`vike_model::VENUES`] entry, each citing the adapter symbol
//! it was read from, a verbatim matrix pin, a completeness test over the roster, and a
//! `vike:new-venue:row` marker so `crates/vike-ops/tests/new_venue_gate.rs` scaffolds a row rather
//! than letting a new venue escape. **Nothing here changes behaviour** — it makes an existing,
//! undocumented reality answerable before anything runs.
//!
//! # ⚠ The fallback REFUSES. It does not answer permissive.
//!
//! 0061 names the shape this must not copy: [`crate::session_calendar_for`]'s own marker *"admits
//! nothing iterates the roster and an unclassified venue silently answers permissive — a kind table
//! that fails permissive is the measured bug wearing a type."* So [`VenueAddressing::UNCLASSIFIED`]
//! is what an unknown venue string gets, it addresses NO class, and its
//! [`BareSymbol::Unmeasured`] answers `true` to [`VenueAddressing::must_claim`]. A venue reaches a
//! permissive answer only by having a NAMED row that says so.
//!
//! # The three questions a row answers
//!
//! **(a) which classes the venue's DATA path can address** — read off that venue's
//! `CatalogProvider::asset_classes`, which is the set its own catalog mints and therefore the set a
//! caller can pick from. A venue with no data path at all declares an EMPTY set rather than
//! inheriting one.
//!
//! **(b) how a caller NAMES each** — [`Naming`]. The vike `.P` marker, the venue's own instrument
//! id, nothing-to-separate, or unaddressable.
//!
//! **(c) whether a BARE symbol is ambiguous** — [`BareSymbol`], and the definition is narrow and
//! operational: *can a bare (unsuffixed) symbol at this venue resolve, WITHOUT ERROR, to a book
//! other than the one the caller's spelling names?* A venue whose wrong spelling is REFUSED by the
//! venue is [`BareSymbol::Unambiguous`] — an error is a correct answer, and 0061's measurement 3 is
//! exactly that case on binance. Only a silent wrong book counts.
//!
//! # What this table is NOT
//!
//! It is not a routing table. `rest_category`, `rest_klines_base` and their siblings stay where they
//! are, because 0061's *"the vocabulary names the PRODUCT, never the venue's word for the route"* is
//! the whole reason a `linear`/`inverse`/`SWAP` word must not appear in this crate.
//!
//! Its production consumers read two different columns. `crates/bridges/bybit/src/data.rs`'s
//! `route_target` reads [`VenueAddressing::must_claim`] to decide whether a bare symbol needs
//! proving at all, and `crates/vike-datahub/src/server.rs`'s `refuse_unhonourable_class` reads both
//! [`VenueAddressing::addresses`] and [`Naming`]. The [`Naming`] column additionally became THE
//! authority for the `.P` question on 2026-09-19: [`crate::uses_perp_suffix`] derives from
//! [`Naming::PerpSuffix`] rather than carrying its own copy of the same three venue ids, and
//! [`crate::split_perp_at`] is what a bridge calls to ask it — so a new venue answers "does my
//! symbol carry the marker" exactly once, in this file, at its own row.

use vike_model::AssetClass;

/// How a caller separates a venue's products from each other in the symbol string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Naming {
    /// vike's own [`crate::PERP_SUFFIX`] marker: a bare symbol is the spot listing, a `.P`-suffixed
    /// one the derivative. The suffix is stripped at the wire seam by [`crate::split_perp`].
    PerpSuffix,
    /// The venue's own instrument id already names the product, so nothing is added or stripped —
    /// okx's dashed `instId`s, deribit's instrument names, hyperliquid's registry ids.
    VenueNative,
    /// The venue lists ONE product plane, so there is no second book for a symbol to be confused
    /// with and nothing to separate.
    NoDerivative,
    /// No data path in this workspace addresses this venue's instruments at all.
    Unaddressable,
}

/// Can a BARE symbol at this venue resolve, without error, to a book other than the one it names?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BareSymbol {
    /// No. Either the venue lists one plane, or its own ids are product-distinct, or the wrong
    /// spelling is REFUSED by the venue rather than answered from another book.
    Unambiguous,
    /// Yes — MEASURED, with the measurement cited at the row. A caller must supply a class.
    Ambiguous,
    /// Nobody has measured it. Treated as [`BareSymbol::Ambiguous`] by
    /// [`VenueAddressing::must_claim`] — the scaffolded answer for a fresh bridge and the answer for
    /// an unknown venue string, because a table that fails permissive is the bug this record fixes.
    Unmeasured,
}

/// One venue's row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VenueAddressing {
    /// (a) The classes this venue's own catalog mints, and therefore the ones a caller can name.
    pub classes: &'static [AssetClass],
    /// (b) How the caller separates them in the symbol string.
    pub naming: Naming,
    /// (c) Whether a bare symbol can silently mean another book.
    pub bare_symbol: BareSymbol,
}

impl VenueAddressing {
    /// The refusing fallback — see this module's doc. An unknown venue addresses nothing and must
    /// be claimed.
    pub const UNCLASSIFIED: Self =
        Self { classes: &[], naming: Naming::Unaddressable, bare_symbol: BareSymbol::Unmeasured };

    /// **Must a caller supply a class before a bare symbol at this venue may be routed?** `true`
    /// for anything not positively measured as unambiguous — the direction that fails CLOSED.
    #[must_use]
    pub const fn must_claim(&self) -> bool {
        !matches!(self.bare_symbol, BareSymbol::Unambiguous)
    }

    /// Can this venue's data path address `class` at all? An empty row answers `false` for
    /// everything, which is what makes [`Naming::Unaddressable`] mean something.
    #[must_use]
    pub fn addresses(&self, class: AssetClass) -> bool {
        self.classes.contains(&class)
    }
}

// The class sets, named so a row reads as a declaration rather than a literal. Each is the venue's
// own `CatalogProvider::asset_classes` return, cited at its row.
const CRYPTO_SPOT_AND_PERP: &[AssetClass] = &[AssetClass::CryptoSpot, AssetClass::CryptoPerp];
const FX_AND_CFD: &[AssetClass] = &[AssetClass::Fx, AssetClass::Cfd];

/// **How `venue` is addressed.** An unknown venue is [`VenueAddressing::UNCLASSIFIED`] — it
/// addresses nothing and must be claimed.
///
/// Every row cites the adapter symbol it was read from. When one of those modules changes what it
/// can address, its row here must move with it — `addressing_matrix_is_pinned` fails until it does.
#[must_use]
pub fn addressing_for(venue: &str) -> VenueAddressing {
    match venue {
        // `crates/bridges/binance/src/family/catalog.rs`'s `ASSET_CLASSES`;
        // `crates/bridges/binance/src/data.rs`'s `route_target` routes spot, USDⓈ-M and COIN-M.
        //
        // ⚠ **This venue addresses THREE books through TWO classes, and that is 0061's ruling
        // working rather than a gap.** Binance runs spot, USDⓈ-M futures (`fapi`) and COIN-M
        // (coin-margined/inverse) futures (`dapi`); a perpetual on either futures host is
        // `CryptoPerp`, because 0061 REFUSES to split that variant ("the vocabulary names the
        // PRODUCT, never the venue's word for the route"). So `classes` is unchanged by COIN-M
        // becoming reachable, and the class claim genuinely cannot say which futures book is meant
        // — `crates/bridges/binance/src/instruments.rs`, the venue's own listing, is that decision,
        // and it lives in the bridge exactly because this table is not a routing table.
        //
        // ⚠ Unambiguous DESPITE the same spot/perp string collision bybit has, and the reason is
        // now MEASURED directly rather than inferred from an absence (2026-09-16): binance's three
        // `symbol` sets are DISJOINT — `dapi ∩ fapi` and `dapi ∩ spot(Trading)` are both empty — so
        // a bare symbol names at most one book, and the spot host answers `-1121 Invalid symbol`
        // for a futures string rather than serving one. An error is a correct answer; only a silent
        // wrong book counts here. ⚠ The `pair` field is a different story and is NOT a name: dapi's
        // `pair` values collide with four live spot pairs (`BNBUSD`, `BTCUSD`, `ETHUSD`, `SOLUSD`),
        // which is why the workspace spells a COIN-M instrument `BTCUSD_PERP` and never `BTCUSD`.
        //
        // ⚠ What COIN-M did NOT buy: the PICKER. `crates/bridges/binance/src/catalog.rs` fetches
        // spot and fapi only, so no COIN-M instrument is minted by any path and this row's
        // `classes` over-promises what a user can SELECT, exactly as 0061 phase 4 describes. That
        // is stated in `crates/bridges/binance/CLAUDE.md` rather than papered over here.
        "binance" => VenueAddressing {
            classes: CRYPTO_SPOT_AND_PERP,
            naming: Naming::PerpSuffix,
            bare_symbol: BareSymbol::Unambiguous,
        },
        // `crates/bridges/bybit/src/catalog.rs`'s `BybitCatalog::asset_classes`;
        // `crates/bridges/bybit/src/data.rs`'s `range_target`.
        //
        // ⚠ **THE MEASURED ROW.** A bare symbol here CAN mean an inverse perpetual instead of the
        // spot pair it names, and the spot tape it resolves to is vestigial rather than absent —
        // which is what makes the failure silent. `crates/bridges/bybit/src/instruments.rs` carries
        // the predicate, the two counts that sized it, and the residual.
        "bybit" => VenueAddressing {
            classes: CRYPTO_SPOT_AND_PERP,
            naming: Naming::PerpSuffix,
            bare_symbol: BareSymbol::Ambiguous,
        },
        // `crates/bridges/okx/src/catalog.rs`'s `asset_classes`;
        // `crates/bridges/okx/src/data.rs`'s `REST_CANDLES` interpolates the caller's `instId`
        // VERBATIM. A dashed id (`BTC-USDT`, `BTC-USDT-SWAP`, `BTC-USDT-250627`) already names its
        // product, so there is no bare form to be ambiguous.
        "okx" => VenueAddressing {
            classes: &[
                AssetClass::CryptoSpot,
                AssetClass::CryptoPerp,
                AssetClass::CryptoFuture,
                AssetClass::Option,
            ],
            naming: Naming::VenueNative,
            bare_symbol: BareSymbol::Unambiguous,
        },
        // `crates/bridges/deribit/src/catalog.rs`'s `asset_classes`;
        // `crates/bridges/deribit/src/data.rs`'s module doc states it needs no
        // [`crate::PERP_SUFFIX`] handling because its instrument names
        // (`BTC-PERPETUAL`, `BTC-27JUN25`, `BTC-27JUN25-60000-C`) are already unambiguous.
        "deribit" => VenueAddressing {
            classes: &[AssetClass::Option, AssetClass::CryptoFuture, AssetClass::CryptoPerp],
            naming: Naming::VenueNative,
            bare_symbol: BareSymbol::Unambiguous,
        },
        // `crates/bridges/oanda/src/catalog.rs`'s `asset_classes`;
        // `crates/bridges/oanda/src/data.rs`'s `to_oanda_instrument` rewrites a six-character pair
        // into the venue's underscored spelling. One product plane — a CFD on this venue is the
        // same addressable instrument as the pair, not a second book.
        "oanda" => VenueAddressing {
            classes: FX_AND_CFD,
            naming: Naming::NoDerivative,
            bare_symbol: BareSymbol::Unambiguous,
        },
        // `crates/bridges/ig/src/catalog.rs`'s `asset_classes` (a `QueryBacked` provider, so it
        // enumerates nothing); `crates/bridges/ig/src/data.rs` takes an IG EPIC through the session
        // and maps prices to bars. An epic names one market outright.
        "ig" => VenueAddressing {
            classes: &[AssetClass::Fx, AssetClass::Cfd, AssetClass::Equity, AssetClass::Index],
            naming: Naming::VenueNative,
            bare_symbol: BareSymbol::Unambiguous,
        },
        // ⚠ `crates/bridges/fxcm` has NO `data.rs` at all — the crate is
        // `catalog`/`config`/`event_mapper`/`exec`/`loader`/`recon_client`/`shim`/`sys`, so no
        // historical or bar path in this workspace addresses this venue. The class set is EMPTY
        // rather than `asset_classes`' `[Fx, Cfd]`, which is what that provider offers the PICKER;
        // a picker entry a data path cannot fetch is the gap this column exists to show.
        "fxcm" => VenueAddressing {
            classes: &[],
            naming: Naming::Unaddressable,
            bare_symbol: BareSymbol::Unambiguous,
        },
        // `crates/bridges/dukascopy/src/catalog.rs`'s `asset_classes`;
        // `crates/bridges/dukascopy/src/data.rs` downloads the keyless per-hour `.bi5` archive for a
        // pair. One product plane, one archive per pair.
        "dukascopy" => VenueAddressing {
            classes: FX_AND_CFD,
            naming: Naming::NoDerivative,
            bare_symbol: BareSymbol::Unambiguous,
        },
        // `crates/bridges/polymarket/src/catalog.rs`'s `asset_classes`;
        // `crates/bridges/polymarket/src/data.rs` addresses an ERC-1155 outcome `token_id`. A token
        // id is a venue-native identity and names exactly one outcome book.
        "polymarket" => VenueAddressing {
            classes: &[AssetClass::PredictionMarket],
            naming: Naming::VenueNative,
            bare_symbol: BareSymbol::Unambiguous,
        },
        // ⚠ **THE SECOND MEASURED AMBIGUITY, and the one on an ORDER path.**
        // `crates/bridges/vike-ibkr` ships no `CatalogProvider` at all, so this row's class set is
        // read from `crates/bridges/vike-ibkr/src/contract.rs`'s `parse_simplified`, which has
        // exactly two arms: a `.IDEALPRO` third segment is CASH (fx), and EVERYTHING ELSE is an
        // equity — silently. So `ESZ5.GLOBEX.USD` is submitted as a stock. 0061 pins this as a
        // contradiction rather than fixing it; the row is what makes it answerable.
        "ibkr" => VenueAddressing {
            classes: &[AssetClass::Equity, AssetClass::Fx],
            naming: Naming::VenueNative,
            bare_symbol: BareSymbol::Ambiguous,
        },
        // `crates/bridges/ctrader/src/catalog.rs`'s `asset_classes`;
        // `crates/bridges/ctrader/src/data.rs` subscribes by the numeric symbol id
        // `crates/bridges/ctrader/src/symbols.rs` resolves. A registry id names one instrument.
        "ctrader" => VenueAddressing {
            classes: FX_AND_CFD,
            naming: Naming::VenueNative,
            bare_symbol: BareSymbol::Unambiguous,
        },
        // `crates/bridges/alpaca/src/catalog.rs`'s `asset_classes`;
        // `crates/bridges/alpaca/src/data.rs`'s `class_of` splits on a `/` in the symbol
        // (`BTC/USD` is crypto, `AAPL` is equity) and routes to a different stream host per side.
        // The two spellings cannot collide, so a bare symbol names one plane.
        //
        // ⚠ PINNED CONTRADICTION: that `class_of` returns a PRIVATE `AssetClass` enum declared in
        // that same file with two variants, shadowing this crate's by name in one workspace — 0061
        // names it as one of the four partial encodings of this axis.
        "alpaca" => VenueAddressing {
            classes: &[AssetClass::Equity, AssetClass::CryptoSpot],
            naming: Naming::VenueNative,
            bare_symbol: BareSymbol::Unambiguous,
        },
        // `crates/bridges/aster/src/catalog.rs` returns `vike_binance::family::catalog::
        // ASSET_CLASSES` verbatim; `crates/bridges/aster/src/data.rs`'s `range_target` is the
        // binance-family shape with its own host pair.
        //
        // ⚠ The one wrinkle, and it does NOT make the bare symbol ambiguous:
        // `crates/vike-catalog/src/symbol.rs`'s `USD1_SETTLED_SUFFIX` exists because two
        // identically-shaped strings can only be separated by the venue's own listing. That is a
        // SUFFIX question (WHICH perp), not a bare question (perp or spot) — a bare aster symbol
        // still means the spot listing.
        "aster" => VenueAddressing {
            classes: CRYPTO_SPOT_AND_PERP,
            naming: Naming::PerpSuffix,
            bare_symbol: BareSymbol::Unambiguous,
        },
        // `crates/bridges/hyperliquid/src/catalog.rs`'s `asset_classes`; its ids are the venue's own
        // (`BTC` is the perp, `HYPE/USDC` the spot pair, `test:ABC` a dex-qualified coin) and
        // `crates/bridges/hyperliquid/src/config.rs`'s `Product` lookup returns an ERROR rather than
        // a guess for a string it does not hold — which 0061 calls the target state this whole
        // record points at.
        "hyperliquid" => VenueAddressing {
            classes: CRYPTO_SPOT_AND_PERP,
            naming: Naming::VenueNative,
            bare_symbol: BareSymbol::Unambiguous,
        },
        // vike:new-venue:row // TODO(new-venue: {venue}): READ THE BRIDGE'S `data.rs` BEFORE KEEPING THIS. The
        // vike:new-venue:row // scaffolded row says the venue addresses NOTHING and that nobody has measured whether a
        // vike:new-venue:row // bare symbol can mean another book — both of which are REFUSING answers, which is the
        // vike:new-venue:row // only safe default (see this module's doc). Fill in `classes` from the venue's own
        // vike:new-venue:row // `CatalogProvider::asset_classes`, pick a `Naming`, and move `bare_symbol` off
        // vike:new-venue:row // `Unmeasured` only with a measurement cited here. Then pin the row in
        // vike:new-venue:row // `addressing_matrix_is_pinned`.
        // vike:new-venue:row "{venue}" => VenueAddressing::UNCLASSIFIED,
        _ => VenueAddressing::UNCLASSIFIED,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole matrix, pinned VERBATIM — the playbook's STEP 1 requirement. A row that changes
    /// without this copy changing is a silent behaviour change.
    ///
    /// ⚠ `#[rustfmt::skip]`: the last line of this literal is a `just new-venue` marker, and rustfmt
    /// re-indents such a marker once a generated row ending in a trailing `//` comment lands above
    /// it — `crates/vike-ops/tests/new_venue_gate.rs`'s
    /// `a_trailing_comment_marker_is_rustfmt_skipped_unless_a_recognised_sibling_follows`.
    #[rustfmt::skip]
    const PINNED: &[(&str, usize, Naming, BareSymbol)] = &[
        ("binance",     2, Naming::PerpSuffix,    BareSymbol::Unambiguous),
        ("bybit",       2, Naming::PerpSuffix,    BareSymbol::Ambiguous),
        ("okx",         4, Naming::VenueNative,   BareSymbol::Unambiguous),
        ("deribit",     3, Naming::VenueNative,   BareSymbol::Unambiguous),
        ("oanda",       2, Naming::NoDerivative,  BareSymbol::Unambiguous),
        ("ig",          4, Naming::VenueNative,   BareSymbol::Unambiguous),
        ("fxcm",        0, Naming::Unaddressable, BareSymbol::Unambiguous),
        ("dukascopy",   2, Naming::NoDerivative,  BareSymbol::Unambiguous),
        ("polymarket",  1, Naming::VenueNative,   BareSymbol::Unambiguous),
        ("ibkr",        2, Naming::VenueNative,   BareSymbol::Ambiguous),
        ("ctrader",     2, Naming::VenueNative,   BareSymbol::Unambiguous),
        ("alpaca",      2, Naming::VenueNative,   BareSymbol::Unambiguous),
        ("aster",       2, Naming::PerpSuffix,    BareSymbol::Unambiguous),
        ("hyperliquid", 2, Naming::VenueNative,   BareSymbol::Unambiguous),
        // vike:new-venue:row ("{venue}", 0, Naming::Unaddressable, BareSymbol::Unmeasured), // TODO(new-venue: {venue}): pin whatever the arm above declares
    ];

    #[test]
    fn addressing_matrix_is_pinned() {
        for (venue, class_count, naming, bare) in PINNED {
            let row = addressing_for(venue);
            assert_eq!(row.classes.len(), *class_count, "{venue}: class count drifted");
            assert_eq!(row.naming, *naming, "{venue}: naming drifted");
            assert_eq!(row.bare_symbol, *bare, "{venue}: bare-symbol verdict drifted");
        }
    }

    /// This module's own source, read at RUNTIME rather than `include_str!`ed — the idiom
    /// `crates/vike-model/src/venues.rs` uses for its roster derivation, and it keeps this file out
    /// of `crates/vike-ops/tests/compile_time_path_gate.rs`'s ratchet.
    fn own_source() -> String {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("addressing.rs");
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
    }

    /// Completeness vs the canonical roster, the playbook shape: **every roster venue has a NAMED
    /// arm, even when its value equals the fallback** — *"the named row is the declaration that the
    /// venue was CLASSIFIED, not forgotten"*. Adding a venue to `vike_model::VENUES` fails here
    /// until its row exists.
    ///
    /// ⚠ The check is a SOURCE scan and not `assert_ne!(row, UNCLASSIFIED)`, and the difference is
    /// the convention: a venue is entitled to a row whose VALUE equals the fallback (a fresh bridge
    /// addresses nothing and has been measured by nobody, which is precisely what the scaffolded row
    /// says). A value comparison cannot tell that row from an absent one, so it would refuse the
    /// scaffold's own correct output.
    #[test]
    fn every_roster_venue_has_a_named_row() {
        let src = own_source();
        for &venue in vike_model::VENUES {
            assert!(
                PINNED.iter().any(|(v, ..)| *v == venue),
                "roster venue {venue} has no pinned addressing row"
            );
            assert!(
                src.contains(&format!("\"{venue}\" => VenueAddressing {{"))
                    || src.contains(&format!("\"{venue}\" => VenueAddressing::")),
                "roster venue {venue} has no NAMED arm in `addressing_for` — it would fall through \
                 to the refusing fallback, and a named row is what proves a venue was classified \
                 rather than forgotten"
            );
        }
        assert_eq!(
            PINNED.len(),
            vike_model::VENUES.len(),
            "the pin carries a row for a venue that is not on the roster (or is short one)"
        );
    }

    /// The scan above must be able to FAIL — without this it answers "named" for a venue that is not
    /// there and the completeness test is green over an empty table.
    #[test]
    fn the_named_arm_scan_can_actually_fail() {
        let src = own_source();
        assert!(src.contains("\"bybit\" => VenueAddressing {"), "the scan cannot see a real arm");
        assert!(
            !src.contains("\"no-such-venue\" => VenueAddressing"),
            "the scan matches a venue that has no arm"
        );
    }

    /// ⚠ **The fallback REFUSES.** This is the assertion that separates this table from
    /// `session_calendar_for`'s fail-permissive shape, which 0061 names as the thing not to copy.
    #[test]
    fn an_unknown_venue_addresses_nothing_and_must_be_claimed() {
        let row = addressing_for("no-such-venue");
        assert_eq!(row, VenueAddressing::UNCLASSIFIED);
        assert!(row.must_claim(), "an unclassified venue must never answer permissive");
        assert!(row.classes.is_empty());
        for class in AssetClass::ALL {
            assert!(!row.addresses(*class), "an unclassified venue addresses nothing");
        }
    }

    /// `must_claim` is the refusing direction: only a POSITIVE unambiguous measurement lets a bare
    /// symbol through. Both other states demand a claim.
    #[test]
    fn must_claim_fails_closed() {
        let unmeasured = VenueAddressing {
            classes: CRYPTO_SPOT_AND_PERP,
            naming: Naming::PerpSuffix,
            bare_symbol: BareSymbol::Unmeasured,
        };
        assert!(unmeasured.must_claim());
        assert!(addressing_for("bybit").must_claim(), "the measured ambiguity demands a claim");
        assert!(addressing_for("ibkr").must_claim());
        assert!(!addressing_for("binance").must_claim());
        assert!(!addressing_for("okx").must_claim());
    }

    /// A venue that declares itself unaddressable must address no class — otherwise the column says
    /// two different things.
    #[test]
    fn an_unaddressable_venue_addresses_no_class() {
        for &venue in vike_model::VENUES {
            let row = addressing_for(venue);
            if row.naming == Naming::Unaddressable {
                assert!(
                    row.classes.is_empty(),
                    "{venue} declares Unaddressable but names {} class(es)",
                    row.classes.len()
                );
            } else {
                assert!(
                    !row.classes.is_empty(),
                    "{venue} names a Naming but addresses no class — say Unaddressable instead"
                );
            }
        }
    }

    /// `addresses` answers off the row's own set, and answers `false` for a class the venue does not
    /// mint. The bybit row is the one a route consults.
    #[test]
    fn addresses_answers_from_the_row() {
        let bybit = addressing_for("bybit");
        assert!(bybit.addresses(AssetClass::CryptoSpot));
        assert!(bybit.addresses(AssetClass::CryptoPerp));
        assert!(!bybit.addresses(AssetClass::CryptoFuture));
        assert!(!bybit.addresses(AssetClass::Equity));
        assert!(addressing_for("okx").addresses(AssetClass::Option));
        assert!(!addressing_for("fxcm").addresses(AssetClass::Fx));
    }
}
