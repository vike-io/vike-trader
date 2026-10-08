//! The `ETXTBSY` retry budget and errno probe, for tests that plant an executable and then exec it.
//!
//! A test binary's cases run as threads of ONE process, and a fork in any other thread hands its
//! child a duplicate of the descriptor that just wrote the planted file; `O_CLOEXEC` closes that
//! duplicate at `execve`, not at fork, so for a few microseconds the file is open for writing
//! somewhere on the box and exec'ing it fails with `ETXTBSY`. Each caller owns its own cure and
//! its own argument for it — `crates/vike-cli/tests/common/mod.rs`'s `output_retrying_etxtbsy`
//! re-runs a command whose exec was refused, `crates/vike-ops/tests/common/mod.rs`'s `plant_exec`
//! settles the file with one guarded probe exec — and only the numbers and the errno test they both
//! retry by live here.
//!
//! ⚠ The predicate is the errno and NOTHING else: a missing, unreadable or non-executable program
//! is a real failure that each caller reports on the first attempt, never retried into a timeout.

use std::time::Duration;

/// `ETXTBSY`, "Text file busy" — the ONE errno either caller retries on, and the whole of its
/// matching rule.
pub const ETXTBSY: i32 = 26;

/// How many times an exec may be attempted before the caller gives up and panics.
pub const ATTEMPTS: u32 = 8;

/// The first inter-attempt sleep; each later one doubles. The race clears as soon as the forking
/// thread's child reaches `execve`, which is microseconds away — the doubling is there for a box
/// under CI load, not because the window is long.
pub const FIRST_BACKOFF: Duration = Duration::from_millis(5);

/// The total time a caller can spend ASLEEP before it gives up: the sum of the `ATTEMPTS - 1`
/// sleeps between attempts.
///
/// Derived rather than written down, so `crates/vike-cli/tests/planted_binary_retry.rs` can state
/// "promptly" and "it really did retry" as fractions of the real budget, and an exhaustion panic
/// can name the real budget, instead of a number that rots the first time a constant above is tuned.
pub fn retry_budget() -> Duration {
    let mut total = Duration::ZERO;
    let mut backoff = FIRST_BACKOFF;
    for _ in 1..ATTEMPTS {
        total += backoff;
        backoff *= 2;
    }
    total
}

/// THIS process tried to exec a planted file and the kernel said [`ETXTBSY`].
#[cfg(unix)]
pub fn spawn_error_is_etxtbsy(e: &std::io::Error) -> bool {
    e.raw_os_error() == Some(ETXTBSY)
}

/// Non-unix: `ETXTBSY` is a POSIX rule about a file open for write, and Windows has no equivalent
/// for `Command::new` to hit — so nothing is ever retried there. The planting cases are all
/// `#[cfg(unix)]`; this exists so the runners that call it, which are not, compile and behave
/// identically on the Windows compile witness.
#[cfg(not(unix))]
pub fn spawn_error_is_etxtbsy(_e: &std::io::Error) -> bool {
    false
}
