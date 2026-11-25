//! The stderr reader `child_env` and `refusal_reaches_stderr` both drain the daemon with
//! (`#[path]`-included by each binary).

use std::io::{BufRead, BufReader};
use std::process::Child;
use std::sync::mpsc::{Receiver, channel};

/// Every stderr line the daemon prints, as it prints it.
pub fn stderr_lines(child: &mut Child) -> Receiver<String> {
    let stderr = child.stderr.take().expect("stderr was piped");
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}
