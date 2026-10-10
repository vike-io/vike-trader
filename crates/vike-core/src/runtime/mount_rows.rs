//! The mount-row list a publish hands the snapshot, REUSED while no row's numbers moved.
//!
//! A publish runs on every idle transition, so on a sporadic feed once per event, and until this
//! file existed each one built a fresh `Vec<MountView>` for every live mount (four `String`
//! clones a row) only to throw the previous snapshot's copy away. Almost always nothing in a row
//! had moved. [`CoreThread::published_mount_rows`] now computes each mount's NUMBERS (no
//! allocation), compares them with the rows the last publish carried, and hands the same
//! `Arc<[MountView]>` over again when every one is bit-identical; only a change rebuilds, through
//! `mount_views`, which stays the reference build.
//!
//! NOTHING IS SKIPPED, only the allocation: the valuation, the readiness read and the strategy's
//! `params()` run on every publish exactly as before. The one real risk is a field MISSING from
//! the comparison, a row that moved while the cache kept serving the old one. Two things hold
//! that line: [`CoreThread::mount_rows_unchanged`] destructures the cached `MountView` and
//! `MountBudget` EXHAUSTIVELY (a new field is a compile error until someone decides how it is
//! compared), and `crates/vike-core/src/runtime/tests/mount_rows.rs` has a test per field plus a
//! property test against a fresh build.

use super::*;

/// The list the last publish carried, and the [`CoreThread::mount_epoch`] it was built under.
pub(crate) struct MountRowCache {
    rows: Arc<[MountView]>,
    epoch: u64,
}

impl MountRowCache {
    /// The empty list at epoch `0`: what a core holds before its first publish, and what a core
    /// with no mount keeps handing out, so that core allocates nothing per publish. It is not the
    /// only empty list ever published: `CoreSnapshot::empty` (the fault snapshot, the placeholder
    /// before the first build) has its own, and so does a rebuild after the LAST mount went.
    pub(crate) fn empty() -> Self {
        MountRowCache { rows: Arc::new([]), epoch: 0 }
    }
}

/// `f64` equality on BITS: `NaN` equals itself and `-0.0` differs from `0.0`, so "equal" means "a
/// reader could not tell them apart".
fn same_bits(a: f64, b: f64) -> bool {
    a.to_bits() == b.to_bits()
}

fn same_opt_bits(a: Option<f64>, b: Option<f64>) -> bool {
    a.map(f64::to_bits) == b.map(f64::to_bits)
}

/// [`MountBudget`] equality on bits. The destructure is exhaustive on purpose: a field added to the
/// budget is a compile error here until it is compared.
fn same_budget(a: Option<MountBudget>, b: Option<MountBudget>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            let MountBudget { max_loss, max_notional, flatten_on_breach } = a;
            same_opt_bits(max_loss, b.max_loss)
                && same_opt_bits(max_notional, b.max_notional)
                && flatten_on_breach == b.flatten_on_breach
        }
        _ => false,
    }
}

impl<C: ExecutionClient> CoreThread<C> {
    /// The mount-row list for THIS publish: the cached one when [`Self::mount_rows_unchanged`]
    /// vouches for it (a refcount bump, no allocation), else a fresh `mount_views` build, cached
    /// for the next publish.
    ///
    /// Per event on a sporadic feed, so inside the gated core hop: see `mount_views` for why the
    /// gated harnesses (no mounts) reach little of it. With no mount the check is one epoch compare
    /// and an `is_empty`, and the list is the shared empty one.
    pub(crate) fn published_mount_rows(&mut self) -> Arc<[MountView]> {
        if self.mount_rows_unchanged() {
            return Arc::clone(&self.mount_rows.rows);
        }
        let rows: Arc<[MountView]> = self.mount_views().into();
        self.mount_rows = MountRowCache { rows: Arc::clone(&rows), epoch: self.mount_epoch };
        rows
    }

    /// Whether a fresh `mount_views` build would equal the cached list EXACTLY: every number on
    /// bits, `params` by `PartialEq` (see below), the structure by epoch and live-slot count.
    ///
    /// Walks the live slots in the order `mount_views` does and compares each against the cached
    /// row at the same position. `venue`/`symbol`/`interval` are not compared per row: a slot's
    /// triple never changes after it is mounted, so what could differ is WHICH slots are live, and
    /// that is a mount or unmount ([`Self::mount_epoch`], bumped by `recompute_mount_gates`) or a
    /// slot a strategy-hook panic left `None` (no recompute runs then, but the live count falls
    /// short of the cached length and the list is rebuilt).
    ///
    /// `mount_id` IS compared per row, although a slot's id never changes either: it is the row's
    /// identity (what `vike_exec::ParamsUpdate::mount_id` addresses), the compare is a string
    /// compare that allocates nothing, and it makes the row vouch for WHICH mount it describes on
    /// its own, independently of the epoch.
    ///
    /// ⚠ `params` is compared with `PartialEq`, the one field that cannot be compared on bits
    /// without serialising it: a knob that only flips the sign of a zero is not seen as a change,
    /// and a `NaN` knob never equals itself, so that mount rebuilds the list every publish (slower,
    /// never stale).
    fn mount_rows_unchanged(&self) -> bool {
        if self.mount_rows.epoch != self.mount_epoch {
            return false;
        }
        let rows = &self.mount_rows.rows;
        // The identity of `f64::sum` on an empty iterator, the starting point `mount_residual_view`'s
        // `.sum()` has: adding the rows' realized PnL to it in the same order gives the same bits.
        let mut attributed: f64 = std::iter::empty::<f64>().sum();
        let mut live = 0;
        for i in 0..self.mounts.len() {
            let Some(m) = self.mounts[i].as_ref() else { continue };
            let Some(row) = rows.get(live) else { return false };
            // Exhaustive on purpose (see the module doc): a new `MountView` field must be decided here.
            let MountView {
                kind,
                mount_id,
                venue: _,
                symbol: _,
                interval: _,
                ready,
                position,
                realized_pnl,
                unrealized_pnl,
                notional,
                budget,
                latched,
                params,
            } = row;
            let (now_position, now_realized, now_unrealized, now_notional) =
                self.mount_valuation(i);
            if *kind != vike_exec::MountRowKind::Mount
                || *mount_id != self.mount_ids[i]
                || *ready != (self.mount_states[i] == MountState::Ready)
                || !same_bits(*position, now_position)
                || !same_bits(*realized_pnl, now_realized)
                || !same_bits(*unrealized_pnl, now_unrealized)
                || !same_bits(*notional, now_notional)
                || !same_budget(*budget, self.mount_budget[i])
                || *latched != self.mount_latched[i]
                || *params != m.strategy.params()
            {
                return false;
            }
            attributed += now_realized;
            live += 1;
        }
        if live == 0 {
            return rows.is_empty();
        }
        // Exactly one trailing row, the residual: constant but for the realized PnL no mount owns
        // (`kind`, the empty strings, `ready: true`, the zeros, `None` and `false` never change).
        rows.len() == live + 1
            && rows[live].kind == vike_exec::MountRowKind::Residual
            && same_bits(rows[live].realized_pnl, self.account_net_realized() - attributed)
    }
}
