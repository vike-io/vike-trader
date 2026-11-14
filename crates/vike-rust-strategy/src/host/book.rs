//! The L2-book cursor: one flattened `L2Book` lent to a strategy file per `on_order_book` dispatch.

use std::ffi::c_void;

use crate::abi::BookVTable;

use super::guarded;

// ---- the BOOK cursor (ABI_VERSION 3) ----------------------------------------------------------
//
// Not generic: the payload type is the concrete `L2Book`, so one `static` vtable serves every `B`
// and none of `broker_vtable`'s `TypeId` registry is needed here.

/// The host's flattened view of ONE `L2Book`, built once per `on_order_book` dispatch and lent to
/// the strategy file through [`BookRef::ctx`].
///
/// ⚠ **Why `ctx` is this and not the `&L2Book` itself.** `L2Book`'s sides are `BTreeMap`s, which
/// have no index — a literal per-index cursor over one would be `iter().nth(i)`, O(i), so a full
/// walk would be O(levels²) node steps to deliver O(levels) values. Flattening once here makes
/// [`BookVTable::level_at`] O(1) and the whole dispatch O(levels). `abi.rs`'s cursor section
/// states the resulting per-call cost end to end.
pub(super) struct BookCursor {
    tick_size: f64,
    last_sequence: u64,
    /// Best first: highest bid first.
    bids: Vec<crate::abi::L2BookLevelAbi>,
    /// Best first: lowest ask first.
    asks: Vec<crate::abi::L2BookLevelAbi>,
}

impl BookCursor {
    pub(super) fn of(book: &vike_marketdata::L2Book) -> Self {
        // `top_n` is the ONE public read that yields both sides ordered best-first; asking for the
        // deeper side's length gives every level of both, since it takes at most `n` per side.
        let depth = book.bid_levels().max(book.ask_levels());
        let (bids, asks) = book.top_n(depth);
        let flatten = |v: Vec<vike_marketdata::L2BookLevel>| {
            v.into_iter()
                .map(|l| crate::abi::L2BookLevelAbi { price: l.price, qty: l.qty })
                .collect::<Vec<_>>()
        };
        BookCursor {
            tick_size: book.tick_size,
            last_sequence: book.last_sequence,
            bids: flatten(bids),
            asks: flatten(asks),
        }
    }

    fn side(&self, which: i32) -> &[crate::abi::L2BookLevelAbi] {
        if which >= 0 { &self.bids } else { &self.asks }
    }
}

/// Recover the `&BookCursor` a book thunk's erased `ctx` was built from — the `broker_mut!`
/// argument, one rung down: a safe `fn(*mut c_void) -> &'a BookCursor` would let its caller pick
/// any `'a`, so the act is expanded inline at each call site instead.
macro_rules! book_cursor {
    ($ctx:expr) => {{
        // SAFETY: every caller of a book thunk builds `$ctx` as `&cursor as *const BookCursor as
        // *mut c_void` in the SAME call frame that then invokes the strategy file
        // (`StrategyFileHost::on_order_book`, further down) — a live, immovable `BookCursor` for
        // exactly the duration of the strategy file call currently unwinding back through this thunk.
        // The strategy file ABI is synchronous, so nothing can drop it concurrently.
        unsafe { &*($ctx as *const BookCursor) }
    }};
}

extern "C" fn t_book_tick_size(ctx: *mut c_void) -> f64 {
    guarded("BookVTable::tick_size", 0.0, || book_cursor!(ctx).tick_size)
}
extern "C" fn t_book_last_seq(ctx: *mut c_void) -> u64 {
    guarded("BookVTable::last_sequence", 0, || book_cursor!(ctx).last_sequence)
}
extern "C" fn t_book_side_len(ctx: *mut c_void, side: i32) -> usize {
    guarded("BookVTable::side_len", 0, || book_cursor!(ctx).side(side).len())
}
extern "C" fn t_book_level_at(
    ctx: *mut c_void,
    side: i32,
    i: usize,
    out: *mut crate::abi::L2BookLevelAbi,
) -> bool {
    guarded("BookVTable::level_at", false, || match book_cursor!(ctx).side(side).get(i) {
        Some(level) => {
            // SAFETY: `out` is the live, aligned `*mut L2BookLevelAbi` the guest passed for exactly
            // this call — the contract `BookVTable::level_at`'s own doc states.
            unsafe { *out = *level };
            true
        }
        None => false,
    })
}

/// The one book vtable, shared by every `B`. A plain `static` rather than
/// [`broker_vtable`]'s leaked registry because none of these thunks is generic — the footgun that
/// registry exists for (a function-local `static` in a generic fn being ONE instance across every
/// monomorphisation) cannot arise where there is nothing to monomorphise.
pub(super) static BOOK_VTABLE: BookVTable = BookVTable {
    tick_size: t_book_tick_size,
    last_sequence: t_book_last_seq,
    side_len: t_book_side_len,
    level_at: t_book_level_at,
};
