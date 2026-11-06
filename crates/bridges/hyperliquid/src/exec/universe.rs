//! The exec universe's reload: a coin listed AFTER the start is placeable without a restart.
//!
//! The exec thread and the user-data pump share one [`SharedInstruments`] (the start's `meta` +
//! `spotMeta`). Before this module a coin Hyperliquid listed later passed the control door (whose facts
//! are read fresh) and was refused `unknown symbol X` by exec until `account.active` went off and on.
//!
//! [`Universe::for_symbols`] is the one seam, called by the actor before it dispatches an order, a
//! modify or an adoption: when the CURRENT universe lacks any named symbol it re-reads the universe
//! through the exchange's own transport, at most once per [`RELOAD_MIN_INTERVAL`] (success or failure,
//! so a typo repeated in a loop costs the venue one read a minute), and publishes the result to both
//! readers. A symbol the fresh universe still lacks is refused by the caller in its own words.
//!
//! ⚠ **A reload only GROWS the universe.** A fresh read smaller than the current one (a half answer:
//! `meta` and `spotMeta` tolerate missing sections by design) is dropped with a warning, because
//! publishing it would turn every coin the start knew into `unknown symbol`. A delisted coin keeps its
//! array slot (`isDelisted`), so a healthy read is never smaller.
//!
//! ⚠ **A reload never MOVES an asset id.** An order is signed by a coin's asset id, which is its array
//! position, so a fresh read in which a coin the start knows sits at another position is dropped with the
//! same warning (`keeps_asset_ids`): the exec thread is single and an order already built carries its id
//! from the universe it was built in, but the next order on the moved coin would trade another asset.
//!
//! This is a READ (`meta`, `spotMeta`): it runs on every tier, unlike the margin guard (`margin.rs`),
//! which writes and runs on the testnet only.
//! The reload takes the HIP-3 setting the start was loaded with ([`HyperliquidInstruments::hip3`]).
//! Not reloaded here: a cancel by the venue's own `cloid` of a coin the universe lacks (the node's
//! `recon cancel`), which still refuses in words.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::base::consts::VENUE;
use crate::feeds::instruments::{HyperliquidInstruments, SharedInstruments};

use super::order_wire::HlExchange;

/// The shortest gap between two re-reads. `meta` and `spotMeta` weigh 20 each against the venue's
/// per-IP window the funding poll, the recon reads and the orders share; one pair a minute is noise
/// beside it, and a burst of orders naming an unlisted coin cannot make more.
pub(super) const RELOAD_MIN_INTERVAL: Duration = Duration::from_secs(60);

/// Whether every symbol both universes name keeps its asset id. An asset id is an ARRAY POSITION and an
/// order is signed by it, so a swap that moved one coin would send the next order on it to another asset.
/// (A symbol only one side names is a listing or a rename, not a move: it does not count.)
fn keeps_asset_ids(current: &HyperliquidInstruments, fresh: &HyperliquidInstruments) -> bool {
    fresh
        .symbology()
        .iter()
        .all(|f| current.symbology().by_symbol(&f.symbol).is_none_or(|c| c.asset_id == f.asset_id))
}

/// The exec thread's handle on the shared universe and its reload clock.
pub(super) struct Universe {
    shared: SharedInstruments,
    last_attempt: Option<Instant>,
}

impl Universe {
    pub(super) fn new(shared: SharedInstruments) -> Self {
        Self { shared, last_attempt: None }
    }

    /// The universe as of now, with no reload.
    pub(super) fn current(&self) -> Arc<HyperliquidInstruments> {
        self.shared.current()
    }

    /// The universe to judge `symbols` against: the current one, re-read first (once, rate-limited)
    /// when it lacks any of them.
    pub(super) fn for_symbols<X: HlExchange>(
        &mut self,
        exchange: &X,
        symbols: &[&str],
    ) -> Arc<HyperliquidInstruments> {
        self.for_symbols_at(Instant::now(), exchange, symbols)
    }

    /// [`Self::for_symbols`] at an explicit instant (the tests' clock).
    pub(super) fn for_symbols_at<X: HlExchange>(
        &mut self,
        now: Instant,
        exchange: &X,
        symbols: &[&str],
    ) -> Arc<HyperliquidInstruments> {
        let current = self.shared.current();
        let lacks = symbols.iter().any(|s| current.symbology().by_symbol(s).is_none());
        let due = self
            .last_attempt
            .is_none_or(|t| now.saturating_duration_since(t) >= RELOAD_MIN_INTERVAL);
        if !lacks || !due {
            return current;
        }
        self.last_attempt = Some(now);
        // A builder-dex coin (`dex:COIN`) the universe lacks brings its dex in with the reload, flag or no flag
        // (decision 0218): only THAT dex's `meta` is read, and the dexes a former reload added stay.
        let mut dexes = current.dexes().to_vec();
        for symbol in symbols {
            if let Some((dex, _)) = symbol.split_once(':')
                && current.symbology().by_symbol(symbol).is_none()
                && !dexes.iter().any(|d| d == dex)
                && dexes.len() < vike_exec::MAX_EXTRA_SYMBOLS
            {
                dexes.push(dex.to_string());
            }
        }
        match HyperliquidInstruments::load_with_dexes(
            |body| exchange.info(body),
            None,
            current.hip3(),
            &dexes,
        ) {
            Ok(fresh) if fresh.len() >= current.len() && keeps_asset_ids(&current, &fresh) => {
                tracing::info!(
                    venue = VENUE,
                    was = current.len(),
                    now = fresh.len(),
                    "an order named a coin the start's universe lacks; re-read meta and spotMeta and published the new universe"
                );
                self.shared.replace(fresh);
                self.shared.current()
            }
            Ok(fresh) => {
                tracing::warn!(
                    venue = VENUE,
                    was = current.len(),
                    now = fresh.len(),
                    "the re-read universe is SMALLER than the one this start holds, or moves a coin's asset id; keeping the start's"
                );
                current
            }
            Err(e) => {
                tracing::warn!(venue = VENUE, error = %e, "the universe re-read failed; the start's universe stands");
                current
            }
        }
    }
}
