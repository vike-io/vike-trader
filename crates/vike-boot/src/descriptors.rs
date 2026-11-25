//! **No INHERITED descriptor reaches a child.** At process start every descriptor above stderr that this
//! process did not open itself (a pipe, a lock, anything its parent left without close-on-exec) is MARKED
//! close-on-exec, so it closes at the next `exec` and never reaches a process this one starts.
//!
//! Why it exists: a strategy process refuses to apply its jail while it holds any descriptor above
//! stderr (`crates/vike-sandbox/src/lib.rs`'s `StrategyProcessPolicy`, decision 0281's item 5), and Rust's
//! std opens everything close-on-exec, so a hub leaks none of its OWN. A descriptor the hub INHERITED is
//! different: std never touched it, so it rode into every child. CI's test runner hands its processes two
//! such pipes, and every Rust strategy was refused there (PR #3188, 2026-11-14). The owner's ruling of
//! 2026-11-14 ("1", option 3 of that report): every hub marks them at start, and the sandbox's own refusal
//! STAYS, as the second line.
//!
//! It MARKS and never closes: a descriptor this process inherited is still this process's to use (none is
//! today: no socket activation, no supervisor pipe, no `LISTEN_FDS`; the sidecars' pipes are std's and
//! already close-on-exec). Linux only (it reads `/proc/self/fd`); elsewhere a no-op.
//!
//! ⚠ **This file is the crate's one audited `unsafe` site** (the owner allowed exactly one, 2026-11-14):
//! naming a descriptor by its NUMBER is `unsafe` by Rust's I/O-safety design, and no crate in the tree
//! wraps `close_range` or a raw-number `fcntl`. `crates/vike-ops/tests/hygiene/unsafe_and_toolchain_gate/pins.rs`
//! pins the site and the crate's `deny` posture.

/// Mark every inherited descriptor above stderr close-on-exec; return how many needed it (a descriptor
/// already close-on-exec, every one std opened, is left alone and not counted). Call it ONCE, first,
/// before the process starts a thread or a child: [`crate::boot`] does, and a root that does not boot
/// (the backtesthub) calls it itself.
#[cfg(target_os = "linux")]
#[expect(
    unsafe_code,
    reason = "a descriptor named by its number has no safe borrow; the owner's one site, 2026-11-14"
)]
pub fn mark_inherited_close_on_exec() -> usize {
    use rustix::io::{FdFlags, fcntl_getfd, fcntl_setfd};
    use std::os::fd::BorrowedFd;

    // The numbers first, then the marks: the listing's own directory descriptor is open while it is
    // read and is std's (already close-on-exec), so it is never counted.
    let Ok(entries) = std::fs::read_dir("/proc/self/fd") else { return 0 };
    let numbers: Vec<i32> = entries
        .filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
        .filter(|fd| *fd > 2)
        .collect();
    let mut marked = 0;
    for raw in numbers {
        // SAFETY: the kernel listed `raw` as open in this process a moment ago; the borrow lives for the
        // two `fcntl` calls below and no longer, and nothing is read or written through it. This runs at
        // start, before the process starts a thread, so nothing closes it in between; were one closed and
        // its number reused anyway, the worst outcome is a close-on-exec flag on a descriptor std opened
        // (where it is already set) or an `EBADF` this loop ignores.
        let fd = unsafe { BorrowedFd::borrow_raw(raw) };
        let Ok(flags) = fcntl_getfd(fd) else { continue };
        if !flags.contains(FdFlags::CLOEXEC) && fcntl_setfd(fd, flags | FdFlags::CLOEXEC).is_ok() {
            marked += 1;
        }
    }
    marked
}

/// Off Linux there is no `/proc/self/fd` to read and no strategy process to protect: nothing to mark.
#[cfg(not(target_os = "linux"))]
pub fn mark_inherited_close_on_exec() -> usize {
    0
}

/// The one INFO line a root logs for [`mark_inherited_close_on_exec`]'s count, or `None` when nothing was
/// inherited (the common case: a hub under systemd inherits none). Counts only, never a path.
pub fn marked_line(marked: usize) -> Option<String> {
    (marked > 0).then(|| {
        format!(
            "inherited descriptors: {marked} above stderr marked close-on-exec (no child inherits them)"
        )
    })
}

#[cfg(test)]
#[path = "descriptors_tests.rs"]
mod descriptors_tests;
