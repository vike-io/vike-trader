//! The MOUNT table of a portfolio run (`StrategyEngine::new_mounted`): several strategy files over this
//! one broker, each hearing its own bars, fills and order ends, as the live drain runs several mounts
//! over one account (`vike_core`'s `drive_strategy`).
//!
//! - **Empty on every other run.** `StrategyEngine::new` and `new_shared` never fill
//!   `OrderBrokerSimulator::mounts`, so every branch below tests `is_empty` first and a plain run is
//!   byte-identical.
//! - **The mount rides the order's identity.** `OrderBrokerSimulator::next_queue_id` stamps the current
//!   mount into bits 40.. of `OrderResting::qid` (mount 0, and every plain run, add nothing), a group
//!   leg's too (`GROUP_QUEUE_ID + (mount << 40) + n`), and `push_pending` mints any unstamped order of a
//!   mounted run, so every fill names the mount that placed its order (`OrderBrokerSimulator::mount_of`).
//!   Nothing in `vike-model` changes: `qid` is `#[serde(skip)]`.
//! - **A tag is the mount's own.** Inside the broker a tag is keyed `"{mount}\u{1f}{tag}"`
//!   (`OrderBrokerSimulator::tag_key`) at the seam's entry (`traits.rs`, `in_bar.rs`, an exit's name
//!   too), so two mounts may name the same tag; an untagged order's fill carries `"{mount}\u{1f}"`, the key
//!   an order-list leg under `""` is filed by too (`OrderBrokerSimulator::seam_tag_key`), so the mount's own
//!   `cancel("")` reaches it and no sibling's does. The
//!   composite (`crate::mounted`) strips the key before any strategy hears it, and no key reaches
//!   `dropped`, the record or the ledger log: refusal words keep the user's tag.
//! - **Reads and verbs are the mount's.** `""` names the mount's own slot (`OrderBrokerSimulator::own_slot`),
//!   an order on a symbol the mount does not trade is refused by name (`OrderBrokerSimulator::order_si`),
//!   `OrderBroker::symbols` answers the mount's names and an index read maps through
//!   `OrderBrokerSimulator::slot_of`. A read by NAME stays an account read, as live's `position()` is;
//!   `OrderBroker::closed_trades` is the mount's own ledger, as live's mount's is.
//! - **A mount's book is its attributed fills.** Each fill a mount's order made folds, as booked, into the
//!   mount's row of its slot by the live ledger's ONE fold (`vike_execution::MountAttribution::fold_fill`)
//!   and into the mount's closed-trade ledger (`OrderBrokerSimulator::attribute_fill`, before the strategy
//!   hears the fill, and before an OTO child the fill arms is judged on the book); the rows are valued by
//!   the live ledger's law (`MountAttribution::valued_at`) at each
//!   slot's price, summed in cross-section order (`OrderBrokerSimulator::mount_valuation`), and sampled
//!   into the mount's PnL curve beside every equity point. The account nets the mounts, as live's does;
//!   a fill no mount minted (a liquidation, a settlement) moves the account alone.
//! - **A mount's budget is judged as live judges it.** Its leverage cap refuses an opening order past it
//!   on its own book, the order netted into its own row and a covered reduce of that row passing
//!   (`OrderBrokerSimulator::leverage_refusal`, `vike_execution::risk::judge_mount_leverage`, each order
//!   alone, as live's drain judges it); its loss
//!   and notional arms are swept
//!   once a bar, after the margin call and before any strategy steps, by the one breach rule
//!   (`vike_execution::MountBudget::breached`, `OrderBrokerSimulator::sweep_mount_budgets`), and a
//!   breach latches the mount alone while its siblings trade on.

use std::borrow::Cow;

use vike_execution::{MountAttribution, MountBudget};
use vike_math::summation::CompensatedSum;
use vike_model::{Fill, MountLossAction, OrderDrop, OrderKind, OrderResting};
use vike_trade_ledger::TradeClosedLedger;

use super::state::OrderBrokerSimulator;

/// The bit the mount starts at in an order's `qid`: 2^40 orders a run, 2^22 mounts.
const MOUNT_SHIFT: u32 = 40;
/// Between the mount and the tag in a keyed tag (ASCII unit separator: no strategy writes it).
const TAG_KEY_SEPARATOR: char = '\u{1f}';
/// The words a latched mount's orders end in, and its new orders are dropped in.
const MOUNT_LATCHED: &str = "the mount's loss budget latched it";

/// One mount of a portfolio run, as the broker keeps it.
pub(crate) struct OrderBrokerSimulatorMount {
    /// The run's symbols the mount trades, its own first: what `OrderBroker::symbols` answers it.
    pub(crate) names: Vec<String>,
    /// Each of `names`' run slot.
    pub(crate) slots: Vec<usize>,
    /// Per run slot: whether the mount trades it (hears its bars, may place on it).
    pub(crate) hears: Vec<bool>,
    /// The budget the live mount states.
    pub(crate) budget: Option<MountBudget>,
    /// Per run slot: the mount's attributed position and PnL there (`legs` empty: a slot is one
    /// instrument of the run's one account).
    pub(crate) rows: Vec<MountAttribution>,
    /// The mount's own closed-trade ledger, configured as the account's.
    pub(crate) closed: TradeClosedLedger,
    /// The mount's realized net plus unrealized PnL at each equity point of the run.
    pub(crate) curve: Vec<f64>,
    /// When the mount's loss budget latched it, if it did.
    pub(crate) latched_at: Option<i64>,
    /// The opening orders its leverage cap refused.
    pub(crate) leverage_refusals: usize,
}

impl OrderBrokerSimulator {
    /// Fill the mount table: each mount's run symbols (its own first) and budget, in mount order.
    /// The LAST line of defence (the harness refuses in words first): no mount, a mount with no
    /// symbol, a symbol that is no slot of the run, more mounts than a `qid` holds.
    pub(crate) fn install_mounts(&mut self, mounts: Vec<(Vec<String>, Option<MountBudget>)>) {
        assert!(!mounts.is_empty(), "a portfolio run mounts at least one strategy");
        assert!(mounts.len() < 1 << 22, "a portfolio run holds fewer than 2^22 mounts");
        for (names, budget) in mounts {
            assert!(!names.is_empty(), "a mount trades at least its own symbol");
            let slot = |s: &String| {
                self.symbols
                    .iter()
                    .position(|x| x == s)
                    .unwrap_or_else(|| panic!("mounted symbol {s:?} is no slot of the run"))
            };
            let slots: Vec<usize> = names.iter().map(slot).collect();
            let mut hears = vec![false; self.symbols.len()];
            for &si in &slots {
                hears[si] = true;
            }
            self.mounts.push(OrderBrokerSimulatorMount {
                names,
                slots,
                hears,
                budget,
                rows: vec![MountAttribution::default(); self.symbols.len()],
                // the account's ledger before its first fill: the same options and account terms
                closed: self.closed_trades.clone(),
                curve: Vec::with_capacity(self.n),
                latched_at: None,
                leverage_refusals: 0,
            });
        }
    }

    /// A fill of the order the mount its tag names placed, as the account booked it: into that mount's
    /// row of its slot (`MountAttribution::fold_fill`, the live ledger's fold) and its closed-trade
    /// ledger, at the slot's contract value and the bar the account's ledger books it in. A fill no
    /// mount minted (no key) is no mount's. Before the strategy hears the fill
    /// (`StrategyEngine::fire_on_fill`), so its reads already hold it.
    pub(crate) fn attribute_fill(&mut self, fill: &Fill) {
        if self.mounts.is_empty() {
            return;
        }
        let Some((m, _)) = fill.tag.as_deref().and_then(Self::split_tag_key) else { return };
        let Some(si) = self.symbols.iter().position(|s| *s == fill.symbol) else { return };
        let value = self.value_of_index(si);
        let bar =
            self.bars[si].partition_point(|b| b.timestamp <= fill.timestamp).saturating_sub(1);
        let mount = &mut self.mounts[m];
        mount.rows[si].fold_fill(fill.side, fill.size, fill.price, value, fill.fee);
        mount.closed.apply(fill, "", bar, value, 1.0);
    }

    /// `(realized net of fees, unrealized, gross notional)` of mount `m`'s book now: each of its rows
    /// valued at its slot's price and contract value by the live ledger's law
    /// (`MountAttribution::valued_at`), each figure summed by `compensated_sum`'s rule in cross-section
    /// order.
    pub(crate) fn mount_valuation(&self, m: usize) -> (f64, f64, f64) {
        let mount = &self.mounts[m];
        let [mut realized, mut unrealized, mut notional] = [CompensatedSum::default(); 3];
        for &si in self.cross_section().iter().filter(|&&si| mount.hears[si]) {
            let price = self.symbol[si].price;
            let (r, u, g) = mount.rows[si].valued_at(price, self.value_of_index(si));
            realized.add(r);
            unrealized.add(u);
            notional.add(g);
        }
        (realized.total(), unrealized.total(), notional.total())
    }

    /// Each mount's PnL beside the equity point just taken, or in place of the last one when the run
    /// re-points its curve's last point (an end-of-run settlement). No-op on a plain run.
    pub(crate) fn sample_mount_curves(&mut self, repoint_last: bool) {
        for m in 0..self.mounts.len() {
            let (realized, unrealized, _) = self.mount_valuation(m);
            let curve = &mut self.mounts[m].curve;
            match curve.last_mut() {
                Some(last) if repoint_last => *last = realized + unrealized,
                _ => curve.push(realized + unrealized),
            }
        }
    }

    /// The live budget sweep (`vike_core`'s `sweep_mount_budgets_in`), once a bar after the margin call
    /// and before any strategy steps, not warmup-gated: each mount whose budget arms a latch and is not
    /// latched yet latches when, at this close, its loss (`-(realized net + unrealized)`, counted from
    /// nothing) or its gross notional breaches it (`MountBudget::breached`, the one rule). No-op on a
    /// plain run.
    pub(crate) fn sweep_mount_budgets(&mut self) {
        for m in 0..self.mounts.len() {
            let mount = &self.mounts[m];
            let armed = |b: &MountBudget| mount.latched_at.is_none() && b.is_active();
            let Some(budget) = mount.budget.filter(armed) else { continue };
            let (realized, unrealized, notional) = self.mount_valuation(m);
            if budget.breached(-(realized + unrealized), notional) {
                self.latch_mount(m, budget.loss_action);
            }
        }
    }

    /// Latch mount `m`: its orders cancelled (a tagged one's end heard, `Canceled` in
    /// [`MOUNT_LATCHED`]'s words; an untagged one off the book, a held one too), and with `Flatten` one
    /// reduce-only market order per row it holds, under the mount, which fills at the next open into its
    /// book; then `latched_at` now. From then on its new orders are dropped (`Self::latched_drop`).
    ///
    /// shortcut: one flatten, never offered again (live offers it at every sweep until the mount is
    /// flat), and an OTO child an untagged inherent verb parked stays parked; upgrade when a flatten can
    /// end short of flat in a run that matters.
    fn latch_mount(&mut self, m: usize, action: MountLossAction) {
        let own = |(key, &(si, qid)): (&String, &(usize, u64))| {
            let owner = Self::split_tag_key(key).map(|(owner, _)| owner);
            (owner == Some(m)).then(|| (key.clone(), si, qid))
        };
        let tagged: Vec<(String, usize, u64)> = self.order_tags.iter().filter_map(own).collect();
        for (key, si, qid) in tagged {
            self.cancel_queue_id_in(si, qid, &key, MOUNT_LATCHED);
        }
        for st in &mut self.symbol {
            st.pending.retain(|o| Self::mount_of(o.qid) != m);
            st.held.retain(|h| Self::mount_of(h.order.qid) != m);
        }
        if action == MountLossAction::Flatten {
            let hook = std::mem::replace(&mut self.mount, m);
            for si in 0..self.symbols.len() {
                let size = self.mounts[m].rows[si].size;
                if size != 0.0 {
                    let side = vike_model::closing_side(size);
                    let mut o = OrderResting::new(OrderKind::Market, side, size.abs());
                    o.reduce_only = true;
                    self.push_pending(si, o);
                }
            }
            self.mount = hook;
        }
        self.mounts[m].latched_at = Some(self.now);
    }

    /// Whether the current mount is latched (`Self::latch_mount`): then its new order on `symbol` of
    /// `qty` is dropped, one row in `dropped` and no event, as the live drain drops a latched mount's
    /// intents. `false` on a plain run.
    pub(super) fn latched_drop(&mut self, symbol: &str, qty: f64) -> bool {
        if self.mounts.get(self.mount).is_none_or(|m| m.latched_at.is_none()) {
            return false;
        }
        let symbol = match self.seam_si(symbol) {
            Some(si) => self.symbols[si].clone(),
            None => symbol.to_string(),
        };
        let reason = MOUNT_LATCHED.to_string();
        self.dropped.push(OrderDrop { symbol, reason, size: qty, weight: 0.0 });
        true
    }

    /// Each mount's figures, in mount order (`crate::StrategyEngineMountReport`).
    pub(crate) fn mount_reports(&self) -> Vec<crate::StrategyEngineMountReport> {
        let report = |m: &OrderBrokerSimulatorMount| {
            let row = |(name, &si): (&String, &usize)| (name.clone(), m.rows[si].clone());
            let closed = || m.closed.metrics(|symbol| self.ledger_mark(symbol));
            crate::StrategyEngineMountReport {
                rows: m.names.iter().zip(&m.slots).map(row).collect(),
                pnl_curve: m.curve.clone(),
                closed_trades: self.closed_trade_report.then(closed),
                latched_at: m.latched_at,
                leverage_refusals: m.leverage_refusals,
            }
        };
        self.mounts.iter().map(report).collect()
    }

    /// Whether this is a portfolio run (`StrategyEngine::new_mounted`).
    #[inline]
    pub(crate) fn is_mounted(&self) -> bool {
        !self.mounts.is_empty()
    }

    /// The mount whose order `qid` is (`0` for every order of a plain run).
    #[inline]
    pub(crate) fn mount_of(qid: u64) -> usize {
        ((qid & !super::groups::GROUP_QUEUE_ID) >> MOUNT_SHIFT) as usize
    }

    /// The current mount's bits of a new `qid` (`0` for mount 0 and every plain run).
    #[inline]
    pub(crate) fn mount_bits(&self) -> u64 {
        (self.mount as u64) << MOUNT_SHIFT
    }

    /// `tag` as the broker keys it: itself on a plain run and for an untagged order, else the current
    /// mount's `"{mount}\u{1f}{tag}"`.
    pub(crate) fn tag_key<'a>(&self, tag: &'a str) -> Cow<'a, str> {
        if self.mounts.is_empty() || tag.is_empty() {
            Cow::Borrowed(tag)
        } else {
            Cow::Owned(format!("{}{TAG_KEY_SEPARATOR}{tag}", self.mount))
        }
    }

    /// The key a seam verb files a list leg's `tag` under, or finds one by (`place_group`, `cancel`,
    /// `amend`): [`Self::tag_key`]'s, but in a portfolio run `""` is the current mount's untagged key, so
    /// a mount's own `cancel("")` reaches its own `""` leg and no sibling's, as live keys `""` by the
    /// mount (`vike_core`'s `tag_key`), and the leg's fill still reaches its mount.
    pub(crate) fn seam_tag_key<'a>(&self, tag: &'a str) -> Cow<'a, str> {
        if tag.is_empty() && self.is_mounted() {
            Cow::Owned(Self::untagged_key(self.mount))
        } else {
            self.tag_key(tag)
        }
    }

    /// The key an UNTAGGED order of the mount `mount` fills under.
    pub(crate) fn untagged_key(mount: usize) -> String {
        format!("{mount}{TAG_KEY_SEPARATOR}")
    }

    /// A keyed tag's mount and the user's tag (`""` for an untagged order), `None` for a tag no
    /// mount keyed.
    pub(crate) fn split_tag_key(key: &str) -> Option<(usize, &str)> {
        let (mount, tag) = key.split_once(TAG_KEY_SEPARATOR)?;
        Some((mount.parse().ok()?, tag))
    }

    /// The slot `""` names: the current mount's own symbol, `HFT_SI` on a plain run.
    #[inline]
    pub(crate) fn own_slot(&self) -> usize {
        match self.mounts.get(self.mount) {
            Some(m) => m.slots[0],
            None => Self::HFT_SI,
        }
    }

    /// The run slot of the current mount's symbol `index` (`OrderBroker`'s `_at` reads); `index` itself
    /// on a plain run. An index outside the mount's list answers `usize::MAX`, which every read refuses
    /// as it refuses an index outside the run's.
    #[inline]
    pub(crate) fn slot_of(&self, index: usize) -> usize {
        match self.mounts.get(self.mount) {
            Some(m) => m.slots.get(index).copied().unwrap_or(usize::MAX),
            None => index,
        }
    }

    /// The slot an ORDER on `symbol` goes on: `seam_si`'s, unless the current mount does not trade
    /// that symbol (`None`, refused by name).
    pub(super) fn order_si(&self, symbol: &str) -> Option<usize> {
        let si = self.seam_si(symbol)?;
        self.mounts.get(self.mount).is_none_or(|m| m.hears[si]).then_some(si)
    }
}
