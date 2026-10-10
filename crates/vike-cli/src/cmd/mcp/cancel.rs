//! [`CancelToken`]: how `notifications/cancelled` reaches a daemon tool that is blocked on its socket.
//!
//! A daemon tool (`run_backtest`, `run_sweep`, `run_walk_forward`) runs on a worker thread and spends
//! its life inside ONE blocking `read_frame` on the compute-daemon connection; there is no point in
//! between at which it could poll a flag. So cancelling it means closing that socket from the event
//! loop's thread: the worker's read then fails, and the daemon sees its peer go. The token is the
//! hand-off between the two threads — the worker [`register`](CancelToken::register)s a clone of its
//! connection once it has one, and the loop calls [`cancel`](CancelToken::cancel).
//!
//! ⚠ **The race this type exists for: the cancel can arrive BEFORE the worker has connected.** The
//! loop reads `notifications/cancelled` while the worker is still dialling or handshaking, so there
//! is no stream to shut down yet. The flag survives that: a later `register` is REFUSED, the worker
//! drops its fresh connection without sending the request, and the cancelled call never reaches the
//! daemon at all. Without the flag the cancel would be lost and the run would go ahead unwatched.
//!
//! The worker reads [`is_cancelled`](CancelToken::is_cancelled) after its tool returns: a socket error
//! on a cancelled token is the cancel, not a tool failure, and the loop drops the outcome either way
//! (a cancelled request gets no response).

use std::sync::{Arc, Mutex, PoisonError};

/// Something a cancel can shut down — in production a [`vike_datahub_client::CancelHandle`], the
/// compute-daemon connection's clone. A trait so the token's own tests can count shutdowns without a
/// socket.
pub(super) trait Shutdown: Send {
    /// Close the connection both ways. Idempotent and infallible: a socket the peer already closed
    /// has nothing left to shut.
    fn shutdown(&self);
}

impl Shutdown for vike_datahub_client::CancelHandle {
    fn shutdown(&self) {
        vike_datahub_client::CancelHandle::shutdown(self);
    }
}

/// The cancel already happened; the caller must not use the connection it tried to register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Cancelled;

#[derive(Default)]
struct State {
    cancelled: bool,
    stream: Option<Box<dyn Shutdown>>,
}

/// One in-flight daemon tool's cancel switch, shared by the event loop (which cancels) and the
/// worker (which registers its connection). Cloning shares the switch.
#[derive(Clone, Default)]
pub(super) struct CancelToken {
    state: Arc<Mutex<State>>,
}

impl CancelToken {
    /// A token nobody has cancelled, holding no connection.
    pub(super) fn new() -> Self {
        Self::default()
    }

    /// Hand the token the connection a cancel must shut down. REFUSED with [`Cancelled`] when the
    /// cancel came first (the module doc's race): the caller then drops its connection unused.
    pub(super) fn register(&self, stream: Box<dyn Shutdown>) -> Result<(), Cancelled> {
        let mut state = self.lock();
        if state.cancelled {
            return Err(Cancelled);
        }
        state.stream = Some(stream);
        Ok(())
    }

    /// Cancel: set the flag and shut the registered connection down, once. A second call does
    /// nothing — the connection was taken by the first.
    pub(super) fn cancel(&self) {
        let mut state = self.lock();
        state.cancelled = true;
        if let Some(stream) = state.stream.take() {
            stream.shutdown();
        }
    }

    /// Has [`cancel`](Self::cancel) been called?
    pub(super) fn is_cancelled(&self) -> bool {
        self.lock().cancelled
    }

    /// Are these two handles to the SAME switch? The loop uses it to tell a worker's `Done` from a
    /// later request that reused the id after a cancel.
    pub(super) fn is_same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.state, &other.state)
    }

    /// The lock, poison or not: the state is a flag and an `Option`, both valid after any panic, and
    /// a poisoned cancel switch that stopped cancelling would leave the daemon computing.
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// A stream that counts how often it was shut down.
    struct Counting(Arc<AtomicUsize>);

    impl Shutdown for Counting {
        fn shutdown(&self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn counting() -> (Box<dyn Shutdown>, Arc<AtomicUsize>) {
        let count = Arc::new(AtomicUsize::new(0));
        (Box::new(Counting(Arc::clone(&count))), count)
    }

    #[test]
    fn a_cancel_before_register_refuses_the_connection() {
        let token = CancelToken::new();
        token.cancel();
        let (stream, count) = counting();
        assert_eq!(token.register(stream), Err(Cancelled), "the late connection is refused");
        assert!(token.is_cancelled());
        assert_eq!(count.load(Ordering::SeqCst), 0, "a refused stream is the caller's to drop");
    }

    #[test]
    fn register_then_cancel_shuts_the_stream_down_once() {
        let token = CancelToken::new();
        let (stream, count) = counting();
        token.register(stream).expect("nobody cancelled yet");
        assert!(!token.is_cancelled());
        token.cancel();
        assert!(token.is_cancelled());
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_second_cancel_is_a_no_op() {
        let token = CancelToken::new();
        let (stream, count) = counting();
        token.register(stream).expect("nobody cancelled yet");
        let clone = token.clone();
        token.cancel();
        clone.cancel();
        assert!(clone.is_cancelled(), "a clone shares the switch");
        assert_eq!(count.load(Ordering::SeqCst), 1, "the stream is shut down exactly once");
    }
}
