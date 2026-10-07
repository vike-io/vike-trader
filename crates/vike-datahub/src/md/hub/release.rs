//! The END of a hold: close a session and release the keys it held. **No venue call anywhere in this
//! file** — a release only drops a refcount and, at zero, stamps the [`MD_LINGER`] deadline the
//! reconciler reads (`reconcile.rs`'s [`MdHub::reconcile`] is what eventually stops the venue
//! subscription).
//!
//! `close_session` is what a `SessionGuard`'s `Drop` ends in, so a panicking writer thread still
//! releases everything its session held; `release_key` is shared with `subscribe.rs`'s
//! [`MdHub::update`], whose remove path calls it, and is therefore `pub(super)`.

use super::*;

impl MdHub {
    /// End a session: release every key it held (stamping the [`MD_LINGER`] deadline at `now_ms`),
    /// close its mailbox, free its connection slot, and poke the reconciler.
    pub fn close_session(&self, id: MdSessionId, now_ms: i64) {
        let held = {
            let mut g = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
            match g.remove(&id) {
                Some(s) => {
                    s.mailbox.close();
                    s.keys
                }
                None => return,
            }
        };
        self.stream_conns.fetch_sub(1, Ordering::AcqRel);
        for key in held.into_keys() {
            self.release_key(&key, now_ms);
        }
        self.poke();
    }

    /// Drop one reference to a key. **No venue call.** At zero, stamp the [`MD_LINGER`] deadline.
    ///
    /// ⚠ **Callers must have removed the key from the session's own set FIRST** (both do:
    /// [`MdHub::close_session`] removes the whole session before it releases, and [`MdHub::update`]
    /// removes the entry before it calls here) — the depth refold below folds over what REMAINS, and
    /// a session still holding the key it is releasing would fold its own departing request back in.
    pub(super) fn release_key(&self, key: &MdKey, now_ms: i64) {
        // ⚠ SESSIONS FIRST, THEN KEYS — the order every path in this file takes them in. The fold is
        // computed and the guard DROPPED before `entry` touches `keys.read`, so this holds one lock
        // at a time and cannot invert against `acquire`.
        let want = {
            let sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
            max_requested_depth(&sessions, key)
        };
        let Some(entry) = self.entry(&key.venue, &key.symbol, key.lane) else { return };
        // SATURATING, never a bare `fetch_sub`: a decrement below zero would WRAP to `u32::MAX` and
        // pin the key live forever, which is the leak this whole RAII path exists to prevent.
        let prev = entry
            .subscribers
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| Some(n.saturating_sub(1)))
            .unwrap_or(0);
        if prev <= 1 && !entry.is_resident() {
            entry.zero_since.store(now_ms, Ordering::Release);
        }
        entry.settle_depth(want);
    }
}
