//! **One blocking call on a throwaway thread, waited for until a deadline** (decision 0141, amended
//! 2026-11-18): [`bounded`], the one spelling the desktop's market-data session and stored-catalog
//! load (`crates/vike-app-core/src/data/market_data_session/connection.rs`'s `bounded`,
//! `crates/vike-app-core/src/data/data_manager/load.rs`'s `bounded`) and the gatewayhub's credentialed open
//! (`crates/vike-gatewayhub/src/data/credentialed.rs`'s `bounded_in`) used to copy.
//!
//! The thread is ABANDONED when the deadline wins, never joined (the blocking wait is what is being
//! escaped): it finishes whenever its call lets go, and its late answer is dropped with the channel,
//! which closes whatever it holds (a socket, a client). That is safe only for a call that borrows
//! NOTHING from its caller, which the `'static` bound enforces.
//!
//! The thread's name, the deadline and the words of each refusal are the CALLER's: this function
//! owns only the spawn, the one-slot channel and the wait. A cap on abandoned threads still running
//! (the gatewayhub's handshake gauge) is the caller's too, held inside the call it hands in.
//!
//! **Its twin for a thread the caller already owns: [`join_within`]**, a join bounded by a time
//! limit, the one spelling a teardown uses after it has asked a thread to stop (the gatewayhub's
//! trade lease and account session, its client's trade link, the dukascopy bridge, the tradehub's
//! reconcile driver used to copy it). The stop signal and the words of a thread left running stay
//! with the caller. Its deadline's `Instant` read is a scheduling row of
//! `crates/vike-ops/tests/architecture/clock_pin.rs`'s `CLOCK_PIN`: it decides how long a teardown
//! waits, never what anything computes.

use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How often [`join_within`] looks whether the thread has ended.
const JOIN_POLL_STEP: Duration = Duration::from_millis(5);

/// Why [`bounded`] has no answer: three causes a caller words differently.
#[derive(Debug)]
pub enum ThreadNoAnswer {
    /// The OS refused the thread (`std::thread::Builder::spawn`'s error). The call never ran.
    NotStarted(std::io::Error),
    /// Nothing within the deadline. The thread keeps running and its answer is dropped when it ends.
    TimedOut,
    /// The thread ended without an answer before the deadline: the call PANICKED.
    Panicked,
}

/// **Run `call` on a new thread named `thread_name` and wait at most `within` for its answer.**
pub fn bounded<T: Send + 'static>(
    thread_name: &str,
    within: Duration,
    call: impl FnOnce() -> T + Send + 'static,
) -> Result<T, ThreadNoAnswer> {
    // ⚠ `sync_channel(1)`, so the helper's `send` NEVER blocks: a rendezvous channel would park the
    // abandoned thread for ever on the very path this function exists to bound.
    let (tx, rx) = mpsc::sync_channel::<T>(1);
    std::thread::Builder::new()
        .name(thread_name.to_string())
        .spawn(move || {
            // Receiver gone = the caller gave up; the late answer is dropped here.
            let _ = tx.send(call());
        })
        .map_err(ThreadNoAnswer::NotStarted)?;
    rx.recv_timeout(within).map_err(|e| match e {
        mpsc::RecvTimeoutError::Timeout => ThreadNoAnswer::TimedOut,
        mpsc::RecvTimeoutError::Disconnected => ThreadNoAnswer::Panicked,
    })
}

/// **Join `handle` if its thread ends within `within`.** `Ok` carries what `join` returned (its
/// `Err` is a panic the thread's hook already reported); `Err` hands the handle back while the
/// thread still runs, to wait again after a stronger stop or to drop, which abandons the thread to
/// end on its own.
pub fn join_within<T>(
    handle: JoinHandle<T>,
    within: Duration,
) -> Result<std::thread::Result<T>, JoinHandle<T>> {
    let deadline = Instant::now() + within;
    while !handle.is_finished() {
        if Instant::now() >= deadline {
            return Err(handle);
        }
        std::thread::sleep(JOIN_POLL_STEP);
    }
    Ok(handle.join())
}

#[cfg(test)]
#[path = "thread_tests.rs"]
mod tests;
