//! **ONE rotating OAuth grant, shared by every socket of this process that holds it** — the
//! in-process stand-in for the single session owner the planned `vike-gatewayhub` will be.
//!
//! # The defect this closes
//!
//! cTrader ROTATES the refresh token on every refresh and terminates the sessions that hold the
//! superseded access token (`ProtoOAAccountsTokenInvalidatedEvent`'s wire doc lists "token was
//! refreshed" among its triggers). Inside ONE `vike-tradehub` THREE sockets hold one account's
//! grant: the exec actor (`crate::adapter::live_runtime_for_account`), the data actor
//! (`vike-tradehub`'s `wire_ctrader_feed`) and the dedicated reconcile client
//! (`crate::recon_client::CtraderReconClient`). Each was built from its OWN copy of the pair, and:
//!
//! * the exec actor refreshed and SAVED; the data actor refreshed and saved NOTHING (its config
//!   carried no rotation home); the reconcile client kept its frozen copy and never refreshed;
//! * whichever refreshed first left the others holding a spent refresh token. At the lapse of one
//!   access token both actors receive the invalidation at once and both spend the SAME refresh
//!   token: one of them is refused and stops (`try_refresh_and_reauth` returns `false`), and when
//!   the loser is the exec actor the winner's pair was never written, so the store keeps the dead
//!   one and the next restart needs `ctrader_authorize`;
//! * a daemon started on an already-lapsed access token healed it on the exec socket first, and the
//!   data socket — built from the account snapshot taken BEFORE that heal — then spent the refresh
//!   token the exec socket had just spent: the first start failed and a restart worked.
//!
//! # The mechanism
//!
//! A [`SharedGrant`] holds the CURRENT pair of one grant, a generation counter that moves on every
//! rotation, and the rotation home. Every socket holds the cell and the generation it last saw
//! (`ConnConfig::grant` / `ConnConfig::grant_seen`). [`SharedGrant::renew`] is the ONLY place the
//! refresh token is spent, and it runs under the cell's rotation gate:
//!
//! 1. a socket whose generation is BEHIND the cell's was invalidated by a sibling's rotation: it
//!    ADOPTS the cell's pair and spends nothing;
//! 2. a socket that is current spends the cell's refresh token; on success the cell moves to the
//!    new pair, then the pair is saved through `crate::token_store::persist` (the one existing
//!    writer — this module adds no store write of its own), and only then is the gate released.
//!
//! From that, inside one process: **no socket ever presents a refresh token this process already
//! spent** (the token is read, spent and replaced inside one gate hold), **rotation N is in the
//! store before rotation N+1 can start** (the save is inside the gate), and **a refused refresh
//! changes neither the cell nor the store**. The store therefore never holds an older pair than the
//! cell except when the save itself fails, which `conn/session.rs`'s `save_rotation` logs as the
//! persisted refresh always did.
//!
//! The gate and the cell's state are TWO locks on purpose. The gate is held across the OAuth round
//! trip (bounded: `vike_bridge_core::http`'s 30 s agent) and the save; the state is held only for
//! the instants it is read or replaced. Only a socket that needs to SPEND waits on the gate — a
//! reader ([`SharedGrant::snapshot`], [`SharedGrant::join`], `Debug`) never waits on the network, so
//! a rotation of one account cannot stall the start of another. A reader that looks mid-rotation
//! gets the pair about to be superseded; its next refusal goes through `renew`, which waits and
//! adopts.
//!
//! # How separately built configs find their cell
//!
//! The exec start, the data feed and the reconcile client are built by different code from
//! different snapshots of the account's credentials. [`SharedGrant::join`] matches by LINEAGE: a
//! cell remembers every refresh token it has held (a few, `RETIRED_KEPT`), so a socket built from
//! the stale boot snapshot still lands in the cell its sibling already rotated, and its first
//! [`ConnConfig::sync_grant`](super::ConnConfig::sync_grant) moves it onto the current pair.
//! Different grants have different refresh tokens and never meet.
//!
//! ⚠ A cell lives for the PROCESS, not for its last socket. A heal that succeeded and was followed
//! by a failed retry drops the exec socket with its cell's only holder; a data socket built from
//! the boot snapshot a moment later must still land in that cell (and adopt the healed pair), not in
//! a fresh one that would spend the token the heal already spent. A process holds a handful of
//! grants at most, so the registry is not a leak.
//!
//! # What it deliberately does not do
//!
//! It does not touch the venue's rules, the heartbeat cadence or the proactive refresh margin, and
//! it adds no heuristic: the invalidation event does not say WHICH session it ended, so a stray
//! event for the session a rotation just replaced still costs one more rotation, exactly as before
//! (see `crates/bridges/ctrader/CLAUDE.md`). It is also in-process only: a second process spending
//! the same refresh token is the problem only the gatewayhub's single session owner removes, and
//! when `vike-gatewayhub` owns the sessions this registry is deleted with the tradehub's sockets.
//!
//! A socket waiting on the gate while a sibling's refresh FAILS then makes its own attempt with the
//! same refresh token (nothing rotated, so it is still the cell's current one): a failed refresh is
//! retried by the next socket that needs one, as it was when each socket refreshed alone. The worst
//! wait is therefore two bounded OAuth round trips, where one socket alone waited for one.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::oauth::Token;
use crate::token_store::{TokenPersist, expires_at_ms};

use super::session::save_rotation;

/// How many SUPERSEDED refresh tokens a cell remembers for [`SharedGrant::join`]'s lineage match.
/// A process rotates a grant about once per token lifetime (weeks), so a handful covers every
/// snapshot a socket could still be holding; the oldest is dropped past this.
const RETIRED_KEPT: usize = 8;

/// Every cell of this process, for the process's life (see the module doc). Locked only by
/// [`SharedGrant::join`], and only for the instants it reads each cell's state.
static REGISTRY: Mutex<Vec<Arc<SharedGrant>>> = Mutex::new(Vec::new());

/// A copy of a cell's current pair, as of `generation`. `Debug` redacts both tokens.
#[derive(Clone)]
pub struct Grant {
    pub access_token: String,
    pub refresh_token: String,
    /// Wall-clock ms at which `access_token` lapses; `None` until this process has rotated once
    /// (the store carries the pair, not its issue time).
    pub expires_at_ms: Option<i64>,
    /// The cell's rotation count when this copy was taken.
    pub generation: u64,
}

impl std::fmt::Debug for Grant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Grant")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("expires_at_ms", &self.expires_at_ms)
            .field("generation", &self.generation)
            .finish()
    }
}

/// What [`SharedGrant::renew`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Renewal {
    /// A sibling had already rotated: the caller took the cell's pair and spent nothing.
    Adopted,
    /// The caller spent the cell's refresh token; the new pair is in the cell and, when the cell has
    /// a rotation home, in the credential store.
    Refreshed,
}

struct State {
    access: String,
    refresh: String,
    expires_at_ms: Option<i64>,
    generation: u64,
    /// Refresh tokens this cell has held before `refresh`, oldest first, at most [`RETIRED_KEPT`].
    retired: Vec<String>,
    persist: Option<TokenPersist>,
}

impl State {
    fn grant(&self) -> Grant {
        Grant {
            access_token: self.access.clone(),
            refresh_token: self.refresh.clone(),
            expires_at_ms: self.expires_at_ms,
            generation: self.generation,
        }
    }

    /// Has this cell ever held `refresh` — now or before?
    fn holds(&self, refresh: &str) -> bool {
        self.refresh == refresh || self.retired.iter().any(|r| r == refresh)
    }

    fn rotate(&mut self, token: &Token, now_ms: i64) {
        let spent = std::mem::replace(&mut self.refresh, token.refresh_token.clone());
        self.retired.push(spent);
        if self.retired.len() > RETIRED_KEPT {
            self.retired.remove(0);
        }
        self.access = token.access_token.clone();
        self.expires_at_ms = expires_at_ms(now_ms, token.expires_in);
        self.generation += 1;
    }
}

/// One grant's current pair, shared by the sockets of this process. See the module doc.
pub struct SharedGrant {
    /// Held by [`SharedGrant::renew`] across the OAuth call and the save. Nothing else takes it.
    gate: Mutex<()>,
    /// Held only for the instants the pair is read or replaced; never across I/O.
    state: Mutex<State>,
}

impl std::fmt::Debug for SharedGrant {
    /// The generation and whether a rotation home is armed — never a token. `try_lock`, so printing
    /// a config from anywhere (even inside a rotation) cannot deadlock.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut shown = f.debug_struct("SharedGrant");
        match self.state.try_lock() {
            Ok(st) => shown
                .field("generation", &st.generation)
                .field("persists", &st.persist.is_some())
                .finish(),
            Err(_) => shown.finish_non_exhaustive(),
        }
    }
}

impl SharedGrant {
    /// A cell in NO registry — what a test builds to drive [`SharedGrant::renew`] alone. A socket
    /// finds its cell with [`SharedGrant::join`].
    #[must_use]
    pub fn new(access: &str, refresh: &str, persist: Option<&TokenPersist>) -> Arc<SharedGrant> {
        Arc::new(SharedGrant {
            gate: Mutex::new(()),
            state: Mutex::new(State {
                access: access.to_string(),
                refresh: refresh.to_string(),
                expires_at_ms: None,
                generation: 0,
                retired: Vec::new(),
                persist: persist.cloned(),
            }),
        })
    }

    /// **The cell for the grant `refresh` belongs to**, created from `(access, refresh)` when this
    /// process has none yet. Returns the cell and the generation the CALLER's pair is from: the
    /// cell's current one when `refresh` is its current token, one behind it when `refresh` is a
    /// superseded token the cell remembers (so the caller's first `sync_grant` adopts the current
    /// pair).
    ///
    /// A cell with no rotation home takes the first one a joiner brings, so the data feed
    /// (given the daemon's home) and the exec start (given the same one) agree whichever builds
    /// first. A joiner's `access` never overwrites a cell that already exists.
    #[must_use]
    pub fn join(
        access: &str,
        refresh: &str,
        persist: Option<&TokenPersist>,
    ) -> (Arc<SharedGrant>, u64) {
        let mut registry = REGISTRY.lock().unwrap_or_else(PoisonError::into_inner);
        for cell in registry.iter() {
            let mut st = cell.lock();
            if !st.holds(refresh) {
                continue;
            }
            if st.persist.is_none() {
                st.persist = persist.cloned();
            }
            let seen =
                if st.refresh == refresh { st.generation } else { st.generation.saturating_sub(1) };
            drop(st);
            return (cell.clone(), seen);
        }
        let cell = SharedGrant::new(access, refresh, persist);
        registry.push(cell.clone());
        (cell, 0)
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // A panic inside a refresh closure must not wedge every other socket's token handling.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The cell's current pair. Never waits for a rotation in flight (see the module doc).
    #[must_use]
    pub fn snapshot(&self) -> Grant {
        self.lock().grant()
    }

    /// **The one place the refresh token is spent.** `seen` is the generation the caller's pair is
    /// from; `now` reads the wall clock (called once, AFTER the round trip, so the expiry and the
    /// change-journal stamp are the instant the venue answered); `refresh` performs the OAuth round
    /// trip with the refresh token it is handed.
    ///
    /// A caller behind the cell adopts the cell's pair ([`Renewal::Adopted`]) and `refresh` is NOT
    /// called. A current caller gets `refresh(cell's token)`: on `Ok` the cell rotates, the pair is
    /// saved to the rotation home (a failed save is logged and does not fail the renewal — the
    /// running sockets hold a working pair), and the cell's new pair is returned
    /// ([`Renewal::Refreshed`]); on `Err` nothing changes and the error is returned for the caller
    /// to word (the callers never interpolate it: the token endpoint's URL embeds the secrets).
    ///
    /// The gate is held across `refresh` and the save: that is what makes the spend-and-store a
    /// single step to every sibling that needs to spend. See the module doc for the worst wait.
    ///
    /// # Errors
    /// Whatever `refresh` returned.
    pub fn renew<E>(
        &self,
        seen: u64,
        now: impl FnOnce() -> i64,
        refresh: impl FnOnce(&str) -> Result<Token, E>,
    ) -> Result<(Renewal, Grant), E> {
        let _gate = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
        let spend = {
            let st = self.lock();
            if st.generation > seen {
                return Ok((Renewal::Adopted, st.grant()));
            }
            st.refresh.clone()
        };
        let token = refresh(&spend)?;
        let now_ms = now();
        let (grant, persist) = {
            let mut st = self.lock();
            st.rotate(&token, now_ms);
            (st.grant(), st.persist.clone())
        };
        match persist {
            Some(persist) => save_rotation(&persist, &token, now_ms),
            None => tracing::debug!(
                target: "ctrader",
                "token refreshed; no credential store was resolved for this mount, so the new pair \
                 lives only as long as this process"
            ),
        }
        Ok((Renewal::Refreshed, grant))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;
    use crate::conn::ConnConfig;

    const T: i64 = 1_787_356_800_000;

    fn token(access: &str, refresh: &str) -> Token {
        Token {
            access_token: access.to_string(),
            refresh_token: refresh.to_string(),
            expires_in: 2_592_000,
        }
    }

    // ⚠ The tests that need a credential STORE (a rotation reaching it, a refusal leaving it
    // alone) live in `crates/bridges/ctrader/tests/shared_grant.rs`: they seed a database with the
    // record-less writer bodies, and `credential_writer_gate` reads every `crates/*/src` file,
    // inline test modules included, as a caller of a writer.

    /// THE property: a socket whose pair a sibling already replaced adopts the sibling's pair and
    /// the refresh token is spent exactly ONCE — the second spend of one refresh token is the
    /// failure the venue punishes.
    #[test]
    fn a_stale_socket_adopts_the_siblings_pair_and_spends_nothing() {
        let cell = SharedGrant::new("AT0", "RT0", None);
        let spent = AtomicUsize::new(0);
        let rotate = |r: &str| -> Result<Token, ()> {
            assert_eq!(r, "RT0", "the cell spends ITS current token");
            spent.fetch_add(1, Ordering::SeqCst);
            Ok(token("AT1", "RT1"))
        };

        let (how, first) = cell.renew(0, || T, rotate).unwrap();
        assert_eq!(how, Renewal::Refreshed);
        assert_eq!((first.access_token.as_str(), first.refresh_token.as_str()), ("AT1", "RT1"));
        assert_eq!(first.generation, 1);
        assert_eq!(first.expires_at_ms, expires_at_ms(T, 2_592_000));

        // The second socket still holds generation 0: it must NOT reach the venue.
        let (how, second) = cell
            .renew(0, || T + 1, |_| -> Result<Token, ()> { panic!("a stale socket spent a token") })
            .unwrap();
        assert_eq!(how, Renewal::Adopted);
        assert_eq!(second.refresh_token, "RT1");
        assert_eq!(spent.load(Ordering::SeqCst), 1);

        // Once current, a socket IS allowed to spend again (its adopted token was refused).
        let (how, third) = cell
            .renew(
                second.generation,
                || T + 2,
                |r| -> Result<Token, ()> {
                    assert_eq!(r, "RT1");
                    Ok(token("AT2", "RT2"))
                },
            )
            .unwrap();
        assert_eq!((how, third.generation), (Renewal::Refreshed, 2));
    }

    /// A refused refresh leaves the cell exactly as it was, and the clock is never read.
    #[test]
    fn a_refused_refresh_changes_nothing() {
        let cell = SharedGrant::new("AT0", "RT0", None);
        let refused = cell.renew(
            0,
            || -> i64 { panic!("the clock is read after a SUCCESSFUL round trip only") },
            |_| Err::<Token, _>("refused"),
        );
        assert_eq!(refused.unwrap_err(), "refused");
        let snap = cell.snapshot();
        assert_eq!((snap.generation, snap.refresh_token.as_str()), (0, "RT0"));
    }

    /// Eight sockets invalidated at once — the lapse of one access token — spend ONE refresh token
    /// between them and all end on the same new pair.
    #[test]
    fn simultaneous_invalidations_spend_one_refresh_token() {
        let cell = SharedGrant::new("AT0", "RT0", None);
        let spent = Arc::new(AtomicUsize::new(0));
        let start = Arc::new(std::sync::Barrier::new(8));

        let threads: Vec<_> = (0..8)
            .map(|_| {
                let (cell, spent, start) = (cell.clone(), spent.clone(), start.clone());
                std::thread::spawn(move || {
                    start.wait();
                    cell.renew(
                        0,
                        || T,
                        |_| {
                            spent.fetch_add(1, Ordering::SeqCst);
                            // Long enough that every sibling reaches the gate while this one holds it.
                            std::thread::sleep(Duration::from_millis(60));
                            Ok::<_, ()>(token("AT1", "RT1"))
                        },
                    )
                    .unwrap()
                })
            })
            .collect();
        let outcomes: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();

        assert_eq!(spent.load(Ordering::SeqCst), 1, "the refresh token was spent more than once");
        assert_eq!(outcomes.iter().filter(|(how, _)| *how == Renewal::Refreshed).count(), 1);
        assert!(outcomes.iter().all(|(_, g)| g.refresh_token == "RT1" && g.generation == 1));
    }

    /// A rotation in flight stalls NO reader: while one socket sits in the OAuth round trip, a
    /// snapshot, a `join` of the same grant and the `Debug` of the cell all return at once — a
    /// rotation of one account must not hold up the start of another, nor a sibling's reconnect.
    #[test]
    fn readers_never_wait_for_a_rotation_in_flight() {
        let (cell, _) = SharedGrant::join("AT-flight-0", "RT-flight-0", None);
        let (in_round_trip, entered) = mpsc::channel();
        let (finish, finished) = mpsc::channel::<()>();
        let rotating = {
            let cell = cell.clone();
            std::thread::spawn(move || {
                cell.renew(
                    0,
                    || T,
                    |_| {
                        in_round_trip.send(()).unwrap();
                        finished.recv().unwrap();
                        Ok::<_, ()>(token("AT-flight-1", "RT-flight-1"))
                    },
                )
                .unwrap()
            })
        };
        entered.recv_timeout(Duration::from_secs(5)).expect("the rotation reached the round trip");

        // The readers run on their own thread so a regression is a failed assertion, not a hang.
        let (done, readers_done) = mpsc::channel();
        let cell_for_readers = cell.clone();
        std::thread::spawn(move || {
            let snapshot = cell_for_readers.snapshot();
            let (joined, seen) = SharedGrant::join("AT-flight-0", "RT-flight-0", None);
            let shown = format!("{cell_for_readers:?}");
            done.send((snapshot.generation, Arc::ptr_eq(&joined, &cell_for_readers), seen, shown))
                .unwrap();
        });
        let (generation, same_cell, seen, shown) = readers_done
            .recv_timeout(Duration::from_secs(5))
            .expect("a reader waited for the OAuth round trip");
        finish.send(()).unwrap();
        rotating.join().unwrap();

        assert_eq!(generation, 0, "mid-rotation a reader sees the pair about to be superseded");
        assert!(same_cell && seen == 0);
        assert!(!shown.contains("flight"), "a token reached Debug: {shown}");
    }

    /// The lineage match: sockets built from the BOOT snapshot land in the cell a sibling already
    /// rotated, and one `sync_grant` later hold the current pair. (Unique token strings: the
    /// registry is process-wide and `cargo test` runs the unit tests of this binary as threads.)
    #[test]
    fn a_socket_built_from_the_boot_snapshot_joins_the_rotated_cell_and_syncs() {
        let (exec_cell, exec_seen) = SharedGrant::join("AT-join-0", "RT-join-0", None);
        assert_eq!(exec_seen, 0);
        exec_cell.renew(0, || T, |_| Ok::<_, ()>(token("AT-join-1", "RT-join-1"))).unwrap();

        // The data feed's config, from the pre-rotation snapshot.
        let (data_cell, data_seen) = SharedGrant::join("AT-join-0", "RT-join-0", None);
        assert!(Arc::ptr_eq(&exec_cell, &data_cell), "one grant, one cell");
        assert_eq!(data_seen, 0, "the snapshot is one rotation behind the cell");

        let mut data = ConnConfig::new("host", 1, "cid", "secret", "AT-join-0");
        data.refresh_token = Some("RT-join-0".to_string());
        data.grant = Some(data_cell);
        data.grant_seen = data_seen;
        assert!(data.sync_grant(), "behind the cell, so it moves");
        assert_eq!(data.access_token, "AT-join-1");
        assert_eq!(data.refresh_token.as_deref(), Some("RT-join-1"));
        assert_eq!(data.grant_seen, 1);
        assert_eq!(data.token_expires_at_ms, expires_at_ms(T, 2_592_000));
        assert!(!data.sync_grant(), "already current: nothing to do");
    }

    /// The cell outlives its sockets: a heal that succeeded and whose retry then failed drops the
    /// exec socket (the cell's only holder), and the data feed — built from the boot snapshot a
    /// moment later — must still land in the healed cell instead of spending the spent token in a
    /// fresh one.
    #[test]
    fn a_cell_outlives_the_sockets_that_held_it() {
        let (healed, _) = SharedGrant::join("AT-outlive-0", "RT-outlive-0", None);
        healed.renew(0, || T, |_| Ok::<_, ()>(token("AT-outlive-1", "RT-outlive-1"))).unwrap();
        drop(healed); // the exec socket, and with it every handle but the registry's, is gone

        let (later, seen) = SharedGrant::join("AT-outlive-0", "RT-outlive-0", None);
        assert_eq!(later.snapshot().refresh_token, "RT-outlive-1");
        assert_eq!(seen, 0, "the stale snapshot is behind the healed cell");
        later
            .renew(
                seen,
                || T,
                |_| -> Result<Token, ()> { panic!("the spent token was spent again") },
            )
            .expect("it adopts");
    }

    /// Different grants never meet, and a config with no cell is left alone by `sync_grant`.
    #[test]
    fn different_grants_get_different_cells() {
        let (a, _) = SharedGrant::join("AT-split-a", "RT-split-a", None);
        let (b, _) = SharedGrant::join("AT-split-b", "RT-split-b", None);
        assert!(!Arc::ptr_eq(&a, &b));

        let mut bare = ConnConfig::new("host", 1, "cid", "secret", "AT");
        assert!(!bare.sync_grant());
        assert_eq!(bare.access_token, "AT");
    }

    /// The lineage is bounded: past `RETIRED_KEPT` rotations the oldest token is forgotten.
    #[test]
    fn the_lineage_is_bounded() {
        let cell = SharedGrant::new("AT0", "RT0", None);
        let mut generation = 0;
        for n in 1..=(RETIRED_KEPT as u64 + 2) {
            let (_, g) = cell
                .renew(
                    generation,
                    || T,
                    |_| Ok::<_, ()>(token(&format!("AT{n}"), &format!("RT{n}"))),
                )
                .unwrap();
            generation = g.generation;
        }
        let st = cell.lock();
        assert_eq!(st.retired.len(), RETIRED_KEPT);
        assert!(!st.holds("RT0") && !st.holds("RT1"), "the oldest are forgotten");
        assert!(st.holds("RT2") && st.holds(&st.refresh.clone()));
    }

    /// Neither tokens nor the home leak through `Debug`.
    #[test]
    fn debug_never_prints_a_token() {
        let cell = SharedGrant::new("AT-secret-access", "RT-secret-refresh", None);
        let shown = format!("{cell:?} {:?}", cell.snapshot());
        assert!(!shown.contains("secret"), "a token reached Debug: {shown}");
    }
}
