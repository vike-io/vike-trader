//! **The options lane's STATE and every fold onto it** — one `Mutex`ed value shared by the poll
//! thread, the two feed closures and every connection asking `OptionsChain`, as the desktop's
//! `ToolData` was before the lane moved here.
//!
//! Every method is pure over the state (no I/O, no clock read: the caller passes `now`), so the
//! whole poll → fold → answer path is testable without a socket — this module's tests drive it
//! with scripted passes and feed rows. The folds themselves are `vike_options`'
//! (`apply_markprice_to_chains`, `apply_ticker_to_chains`), the selection of the 100 ms focus set is
//! `vike_options::front_expiry_focus_instruments`, and the index cadence is
//! `vike_options::index_refresh_due`: this module holds the lock around them and nothing else.
//!
//! ⚠ **No per-message logging here.** The ticker fold runs on 100 ms frames for ~40 instruments;
//! a log line per row would be the noise `crates/vike-core/CLAUDE.md` forbids in a fold. A pass's
//! failure is logged by the poll thread, once per pass.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use vike_marketdata::{OptionChainSet, OptionTicker};
use vike_options::OptionIndex;

use super::gateway::checked_expiry_ms;
use super::select_expiries;

/// The risk-free rate the folds re-derive greeks with: 0, the value the REST chain and the desktop's
/// folds always used (`vike_options`' greeks doc), so a streamed greek and a re-polled one agree.
pub const RISK_FREE: f64 = 0.0;

/// What the lane holds.
#[derive(Default)]
struct OptionChainStoreState {
    /// The last good bundle per underlying — a pass that fails for one underlying keeps its
    /// previous bundle (the last-good discipline the desktop poll kept).
    by_underlying: BTreeMap<String, OptionChainSet>,
    /// Every option and future by name, as Deribit's own instrument payload defines it — what the
    /// folds look a streamed row up in. Empty until the first index fetch lands; an empty index
    /// skips every streamed row.
    index: OptionIndex,
    /// Why the LAST pass produced nothing, cleared by the next one that produces anything — what a
    /// `NotReady` names when an underlying has never landed.
    last_error: Option<String>,
    /// The front expiry of each focus underlying when the focus set was last handed out (`None`
    /// before the first): the ticker feed is spawned on the first pass that yields a non-empty set,
    /// and again only when a front expiry ROLLS (the held one passed and the venue's next is the
    /// front), never because spot moved across a strike.
    focus_fronts: Option<Vec<String>>,
}

/// The shared state handle. Cloning it shares the ONE state.
#[derive(Clone, Default)]
pub struct OptionChainStore(Arc<Mutex<OptionChainStoreState>>);

impl OptionChainStore {
    /// An empty book: no bundle, an empty index, no pass yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// The lock, through a poisoned mutex rather than past it: a panicked fold leaves a state that
    /// is at worst one row stale, and refusing every later request over it would be worse.
    fn lock(&self) -> MutexGuard<'_, OptionChainStoreState> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Fold one successful REST pass in: each fetched underlying REPLACES its bundle (so an expiry the
    /// venue no longer lists goes, and a new one comes), every other keeps its last good one, and the
    /// last error clears. Returns the ticker FOCUS SET the first time a pass can yield a non-empty one
    /// — the front expiry's ±`focus_strikes` strikes of each of `focus_underlyings`
    /// (`vike_options::front_expiry_focus_instruments`) — and again when a focus underlying's front
    /// expiry ROLLED since the set was handed out (its instruments expired; the caller replaces the
    /// feed), and `None` on every other pass: spot moving across a strike opens no new feed.
    pub fn merge_pass(
        &self,
        fetched: BTreeMap<String, OptionChainSet>,
        focus_underlyings: &[&str],
        focus_strikes: usize,
    ) -> Option<Vec<String>> {
        let mut s = self.lock();
        for (u, bundle) in fetched {
            s.by_underlying.insert(u, bundle);
        }
        s.last_error = None;
        let fronts: Vec<String> = focus_underlyings
            .iter()
            .map(|u| s.by_underlying.get(*u).map(|b| b.default_expiry.clone()).unwrap_or_default())
            .collect();
        if s.focus_fronts.as_ref() == Some(&fronts) {
            return None;
        }
        let focus = vike_options::front_expiry_focus_instruments(
            &s.by_underlying,
            focus_underlyings,
            focus_strikes,
        );
        if focus.is_empty() {
            return None;
        }
        s.focus_fronts = Some(fronts);
        Some(focus)
    }

    /// Record a pass that produced NOTHING; every held bundle is kept.
    pub fn record_failure(&self, why: String) {
        self.lock().last_error = Some(why);
    }

    /// Whether the index should be refetched now — `vike_options::index_refresh_due` over the held
    /// index and grid. `since_last_attempt` is `None` before the first attempt.
    ///
    /// When a POPULATED index is due, it is due because the grid holds names it lacks (a new
    /// listing): that gap is logged here, once per refresh decision, at `debug` — never per row.
    pub fn index_refresh_due(&self, since_last_attempt: Option<Duration>) -> bool {
        let s = self.lock();
        let due = vike_options::index_refresh_due(&s.index, &s.by_underlying, since_last_attempt);
        if due
            && !s.index.is_empty()
            && let Some((missing, first)) = s.index.missing_from(&s.by_underlying)
        {
            tracing::debug!(
                missing,
                first = %first,
                "vike-datahub options: grid instruments absent from the option index (their \
                 streamed rows are skipped); refetching it"
            );
        }
        due
    }

    /// Replace the index (a fetch that failed keeps the last good one: the caller does not call
    /// this).
    pub fn set_index(&self, index: OptionIndex) {
        self.lock().index = index;
    }

    /// Fold a batch of `markprice.options` rows (mark + IV) onto the held grid —
    /// `vike_options::apply_markprice_to_chains`. Returns how many quotes changed.
    pub fn fold_markprice(&self, rows: &[OptionTicker], now: i64) -> usize {
        let mut guard = self.lock();
        let s = &mut *guard;
        vike_options::apply_markprice_to_chains(
            &mut s.by_underlying,
            &s.index,
            rows,
            now,
            RISK_FREE,
        )
    }

    /// Fold one `ticker` row (bid/ask, and mark/IV when carried) onto the held grid —
    /// `vike_options::apply_ticker_to_chains`. Returns how many quotes changed.
    pub fn fold_ticker(&self, row: &OptionTicker, now: i64) -> usize {
        let mut guard = self.lock();
        let s = &mut *guard;
        vike_options::apply_ticker_to_chains(
            &mut s.by_underlying,
            &s.index,
            std::slice::from_ref(row),
            now,
            RISK_FREE,
        )
    }

    /// `underlying`'s held bundle at `now_ms` narrowed by [`select_expiries`], or why there is none:
    /// the last pass's error when every pass so far failed, else that the first pass has not landed.
    ///
    /// ⚠ **An expiry whose instant (08:00 UTC of its date, the instant every recorded row carries) is
    /// at or before `now_ms` is never served**: it leaves the strip and the chains, and the front
    /// becomes the next one. A pass replaces the bundle with the venue's own listing, but a bundle
    /// kept through failed passes (a gatewayhub that is down across an expiry) would otherwise serve
    /// a chain that no longer trades as the front. A bundle with no expiry left is no bundle.
    pub fn snapshot(
        &self,
        underlying: &str,
        expiries: &[String],
        now_ms: i64,
    ) -> Result<OptionChainSet, String> {
        let s = self.lock();
        let Some(bundle) = s.by_underlying.get(underlying) else {
            return Err(match &s.last_error {
                Some(e) => format!("the last chain pass failed ({e})"),
                None => "the first chain pass has not landed yet".to_string(),
            });
        };
        // A date this cannot place is kept: nothing is dropped on a guess.
        let live = |date: &str| checked_expiry_ms(date).is_none_or(|at| at > now_ms);
        if bundle.expiries.iter().all(|e| live(&e.date)) && live(&bundle.default_expiry) {
            return Ok(select_expiries(bundle, expiries));
        }
        let kept: Vec<_> = bundle.expiries.iter().filter(|e| live(&e.date)).cloned().collect();
        // ISO dates sort as the calendar does: the earliest live one is the new front.
        let Some(front) = kept.iter().map(|e| e.date.clone()).min() else {
            return Err(format!(
                "every expiry held for {underlying} has passed; the next pass lists the venue's \
                 new ones{}",
                s.last_error
                    .as_ref()
                    .map(|e| format!(" (the last pass failed: {e})"))
                    .unwrap_or_default()
            ));
        };
        let live_bundle = OptionChainSet {
            default_expiry: if live(&bundle.default_expiry) {
                bundle.default_expiry.clone()
            } else {
                front
            },
            expiries: kept,
            chains: bundle
                .chains
                .iter()
                .filter(|(date, _)| live(date))
                .map(|(date, chain)| (date.clone(), chain.clone()))
                .collect(),
        };
        Ok(select_expiries(&live_bundle, expiries))
    }
}

#[cfg(test)]
mod tests {
    //! The fold-through path with no socket: a scripted REST pass seeds the book, scripted feed rows
    //! fold onto it, and the snapshot the verb would answer carries the folded values.

    use super::*;
    use vike_marketdata::{
        OptionChain, OptionChainExpiry, OptionChainStrike, OptionKind, OptionQuote,
        OptionUnderlyingKind,
    };
    use vike_model::{
        Contract, ContractOption, ContractOptionExerciseStyle, Instrument, Margining,
    };

    const EXPIRY: &str = "2026-06-26";
    const CALL: &str = "BTC-26JUN26-100000-C";

    /// A one-strike BTC chain (spot 100k) with a call and a put quote, coin-settled.
    fn seeded() -> BTreeMap<String, OptionChainSet> {
        let quote = |kind, name: &str| OptionQuote {
            bid: Some(500.0),
            ask: Some(600.0),
            mark: Some(550.0),
            iv: Some(0.5),
            instrument_name: Some(name.to_string()),
            ..OptionQuote::new(100_000.0, kind)
        };
        let chain = OptionChain {
            underlying: "BTC".into(),
            underlying_kind: OptionUnderlyingKind::Crypto,
            underlying_price: Some(100_000.0),
            expiry: OptionChainExpiry { date: EXPIRY.into(), dte: 30, label: "26 Jun".into() },
            asof_ms: 0,
            source: "deribit".into(),
            rows: vec![OptionChainStrike {
                strike: 100_000.0,
                call: Some(quote(OptionKind::Call, CALL)),
                put: Some(quote(OptionKind::Put, "BTC-26JUN26-100000-P")),
            }],
        };
        let bundle = OptionChainSet {
            default_expiry: EXPIRY.into(),
            expiries: vec![chain.expiry.clone()],
            chains: BTreeMap::from([(EXPIRY.to_string(), chain)]),
        };
        BTreeMap::from([("BTC".to_string(), bundle)])
    }

    /// The index Deribit's catalog would build for [`CALL`]: coin-settled (inverse), 08:00 UTC.
    fn index() -> OptionIndex {
        let call = Instrument::new(
            "deribit",
            CALL,
            "BTC",
            "BTC",
            Contract::Option(ContractOption {
                right: OptionKind::Call,
                strike: 100_000.0,
                expiry_ms: vike_options::expiry_ms(EXPIRY),
                style: ContractOptionExerciseStyle::European,
                margining: Margining::Inverse,
                settle: "BTC".to_string(),
                value_currency: Some("BTC".to_string()),
                underlying: Some("BTC".to_string()),
            }),
        );
        OptionIndex::from_instruments([&call])
    }

    /// A time well before [`EXPIRY`], so the greeks have time value.
    const NOW: i64 = 1_780_000_000_000;

    fn call(book: &OptionChainStore) -> OptionQuote {
        let snap = book.snapshot("BTC", &[], NOW).expect("held");
        snap.chains[EXPIRY].rows[0].call.clone().expect("call quote")
    }

    #[test]
    fn nothing_is_served_before_the_first_pass_and_a_failure_is_named() {
        let book = OptionChainStore::new();
        let err = book.snapshot("BTC", &[], NOW).expect_err("nothing held");
        assert!(err.contains("has not landed"), "{err}");
        book.record_failure("deribit book summary: timeout".into());
        let err = book.snapshot("BTC", &[], NOW).expect_err("still nothing held");
        assert!(err.contains("timeout"), "the last pass's error is named: {err}");
    }

    #[test]
    fn a_pass_seeds_the_book_and_hands_out_the_focus_set_once() {
        let book = OptionChainStore::new();
        let focus =
            book.merge_pass(seeded(), &["BTC", "ETH"], 10).expect("first pass: a focus set");
        assert!(focus.contains(&CALL.to_string()), "{focus:?}");
        assert!(
            book.merge_pass(seeded(), &["BTC", "ETH"], 10).is_none(),
            "the set is handed out once"
        );
        assert_eq!(book.snapshot("BTC", &[], NOW).expect("held").chains.len(), 1);
    }

    /// The month after [`EXPIRY`], and its tag in an instrument name.
    const LATER: (&str, &str) = ("2026-07-31", "31JUL26");

    /// BTC over `dates` (date, name tag): a one-strike chain each, spot 100k, the first the front.
    fn bundle_of(dates: &[(&str, &str)]) -> BTreeMap<String, OptionChainSet> {
        let chain = |date: &str, tag: &str| OptionChain {
            underlying: "BTC".into(),
            underlying_kind: OptionUnderlyingKind::Crypto,
            underlying_price: Some(100_000.0),
            expiry: OptionChainExpiry { date: date.into(), dte: 30, label: date.into() },
            asof_ms: 0,
            source: "deribit".into(),
            rows: vec![OptionChainStrike {
                strike: 100_000.0,
                call: Some(OptionQuote {
                    instrument_name: Some(format!("BTC-{tag}-100000-C")),
                    ..OptionQuote::new(100_000.0, OptionKind::Call)
                }),
                put: None,
            }],
        };
        let bundle = OptionChainSet {
            default_expiry: dates[0].0.into(),
            expiries: dates.iter().map(|(d, _)| chain(d, "x").expiry).collect(),
            chains: dates.iter().map(|(d, t)| ((*d).to_string(), chain(d, t))).collect(),
        };
        BTreeMap::from([("BTC".to_string(), bundle)])
    }

    /// **An expiry passes: it is never served again, the next one is the front, and with none left
    /// the bundle is no bundle** — read off a bundle no pass has replaced (a gatewayhub down across
    /// the expiry), at the exact 08:00 UTC instant and after it.
    #[test]
    fn a_passed_expiry_is_never_served_and_the_next_one_becomes_the_front() {
        let book = OptionChainStore::new();
        book.merge_pass(bundle_of(&[(EXPIRY, "26JUN26"), LATER]), &[], 10);
        book.record_failure("gatewayhub 127.0.0.1:1: connection refused".into());

        let before = book.snapshot("BTC", &[], NOW).expect("held");
        assert_eq!((before.default_expiry.as_str(), before.expiries.len()), (EXPIRY, 2));

        let passed = vike_options::expiry_ms(EXPIRY);
        let after = book.snapshot("BTC", &[EXPIRY.to_string()], passed).expect("the next one");
        assert_eq!(after.default_expiry, LATER.0, "the next expiry is the front");
        assert_eq!(after.expiries.iter().map(|e| e.date.as_str()).collect::<Vec<_>>(), [LATER.0]);
        assert!(after.chains.is_empty(), "the passed expiry asked for is omitted, never served");
        let front = book.snapshot("BTC", &[], passed).expect("the front");
        assert_eq!(front.chains.keys().collect::<Vec<_>>(), [LATER.0]);

        let why =
            book.snapshot("BTC", &[], vike_options::expiry_ms(LATER.0)).expect_err("nothing left");
        assert!(why.contains("has passed") && why.contains("connection refused"), "{why}");
    }

    /// **The ticker focus follows a ROLLED front expiry, and nothing else**: the same front hands out
    /// nothing, a pass where the venue dropped the passed front for the next one hands out the new
    /// front's instruments (the caller replaces the feed), and the pass after hands out nothing.
    #[test]
    fn the_focus_set_is_handed_out_again_only_when_the_front_expiry_rolls() {
        let book = OptionChainStore::new();
        let first = book
            .merge_pass(bundle_of(&[(EXPIRY, "26JUN26"), LATER]), &["BTC"], 10)
            .expect("the first set");
        assert_eq!(first, ["BTC-26JUN26-100000-C"]);
        assert!(book.merge_pass(bundle_of(&[(EXPIRY, "26JUN26"), LATER]), &["BTC"], 10).is_none());
        let rolled = book.merge_pass(bundle_of(&[LATER]), &["BTC"], 10).expect("the front rolled");
        assert_eq!(rolled, ["BTC-31JUL26-100000-C"]);
        assert!(book.merge_pass(bundle_of(&[LATER]), &["BTC"], 10).is_none(), "and once only");
    }

    #[test]
    fn a_failed_pass_keeps_the_last_good_bundle() {
        let book = OptionChainStore::new();
        book.merge_pass(seeded(), &[], 10);
        book.record_failure("deribit book summary: 502".into());
        assert!(
            book.snapshot("BTC", &[], NOW).is_ok(),
            "the last good bundle survives a failed pass"
        );
    }

    #[test]
    fn a_markprice_row_folds_through_the_index_and_rows_the_index_lacks_are_skipped() {
        let book = OptionChainStore::new();
        book.merge_pass(seeded(), &[], 10);
        let row = |symbol: &str| {
            let mut t = OptionTicker::new(NOW, symbol);
            t.mark = Some(0.01);
            t.mark_iv = Some(0.65);
            t
        };
        // An empty index skips EVERY row: nothing is ever parsed from a name.
        assert_eq!(book.fold_markprice(&[row(CALL)], NOW), 0);
        book.set_index(index());
        assert_eq!(book.fold_markprice(&[row(CALL), row("BTC-26JUN26-90000-C")], NOW), 1);
        let q = call(&book);
        assert_eq!(q.mark, Some(0.01 * 100_000.0), "coin mark scaled by the chain spot");
        assert_eq!(q.iv, Some(0.65), "the decimal IV stored verbatim");
        assert_eq!(q.bid, Some(500.0), "markprice never touches bid/ask");
    }

    #[test]
    fn a_ticker_row_sets_bid_and_ask_and_keeps_a_mark_it_does_not_carry() {
        let book = OptionChainStore::new();
        book.merge_pass(seeded(), &[], 10);
        book.set_index(index());
        let mut row = OptionTicker::new(NOW, CALL);
        row.bid = Some(0.004);
        assert_eq!(book.fold_ticker(&row, NOW), 1);
        let q = call(&book);
        assert_eq!(q.bid, Some(0.004 * 100_000.0));
        assert_eq!(q.ask, None, "an absent ask clears the stale one");
        assert_eq!(q.mark, Some(550.0), "a ticker without a mark keeps the held one");
    }

    #[test]
    fn the_index_is_due_at_once_and_then_on_its_own_cadence() {
        let book = OptionChainStore::new();
        assert!(book.index_refresh_due(None), "never tried: due");
        assert!(!book.index_refresh_due(Some(Duration::from_secs(1))), "empty, just tried: wait");
    }
}
