//! vike-stat — map the opt-in mmap counters file READ-ONLY and print the live core's key health
//! counters, WITHOUT attaching to the (possibly headless / remote) trader process (audit co9). The
//! trader writes the file only when `CoreConfig::counters_path` is set; this reads it. Protocol/
//! result stdout stays raw (repo logging rule for tools) — no `vike_log` init here.
//!
//!   cargo run -p vike-core --bin vike_stat -- <counters-file> [--watch] [--interval <secs>]
//!
//! The path may also come from $VIKE_COUNTERS_FILE (env reads live in the binary, not the library).
//! `--watch` re-reads on an interval (default 1 s); `--interval <secs>` sets the period (implies
//! `--watch`). A snapshot may be seen mid-update — the reader retries a seqlock and flags a rare
//! torn read; every counter is monotonic, so a torn value is still a valid point-in-time reading.

use std::time::Duration;

/// The usage text. Returned rather than printed, because the STREAM depends on why it is being
/// shown: an explicit `--help` is normal output (stdout — a user pipes it into a pager), while the
/// same text alongside an error message is a diagnostic (stderr). Printing it unconditionally to
/// stderr made `vike_stat --help | less` an empty page.
fn usage() -> &'static str {
    "usage: vike_stat <counters-file> [--watch] [--interval <secs>]\n\
     \n\
     reads the opt-in mmap counters file a live vike core mirrors (CoreConfig::counters_path).\n\
     the path may also come from $VIKE_COUNTERS_FILE.\n\
     \n\
       --watch            re-read every 1s until interrupted\n\
       --interval <secs>  re-read on a custom period (implies --watch)"
}

fn main() {
    let mut path: Option<String> = None;
    let mut watch = false;
    let mut interval: Option<Duration> = None;

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--watch" | "-w" => watch = true,
            "--interval" | "-i" => {
                let secs = args.next().and_then(|s| s.parse::<f64>().ok()).unwrap_or_else(|| {
                    eprintln!("--interval needs a number of seconds");
                    std::process::exit(2);
                });
                interval = Some(Duration::from_secs_f64(secs.max(0.05)));
                watch = true;
            }
            "-h" | "--help" => {
                let usage = usage();
                println!("{usage}");
                return;
            }
            _ if path.is_none() => path = Some(a),
            _ => {
                let usage = usage();
                eprintln!("unexpected argument: {a}\n{usage}");
                std::process::exit(2);
            }
        }
    }

    let path = path.or_else(|| std::env::var("VIKE_COUNTERS_FILE").ok()).unwrap_or_else(|| {
        let usage = usage();
        eprintln!("{usage}");
        std::process::exit(2);
    });
    let path = std::path::PathBuf::from(path);
    let period = interval.unwrap_or_else(|| Duration::from_secs(1));

    if !watch {
        // One-shot: a read error is a non-zero exit so a script can tell "no file / bad file".
        match vike_core::counters::read(&path) {
            Ok(report) => print_report(&path, &report),
            Err(e) => {
                eprintln!("vike_stat: cannot read {}: {e}", path.display());
                std::process::exit(1);
            }
        }
        return;
    }

    // Watch: re-read forever. A transient error (trader not up yet / rotating the file) is printed
    // but does NOT exit — the next tick retries.
    loop {
        match vike_core::counters::read(&path) {
            Ok(report) => print_report(&path, &report),
            Err(e) => eprintln!("vike_stat: cannot read {} (retrying): {e}", path.display()),
        }
        std::thread::sleep(period);
    }
}

fn print_report(path: &std::path::Path, r: &vike_core::CountersReport) {
    let flag = if r.clean { "clean" } else { "TORN-READ" };
    println!(
        "{}  v{} seq={} updated_ms={} [{}]",
        path.display(),
        r.version,
        r.seq,
        r.updated_ms,
        flag
    );
    for (name, value) in r.counters.named() {
        println!("  {name:<26} {value}");
    }
}
