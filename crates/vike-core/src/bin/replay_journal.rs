//! "Send me your journal": deterministically replay a journal dir and verify its determinism fence.
//! The fence verifies EXEC-LANE determinism — that re-folding the journaled exec lane from the first
//! checkpoint reproduces the recorded final order/account state. Protocol/result stdout stays raw
//! (repo logging rule for tools).
//!
//!   cargo run -p vike-core --bin replay_journal -- <journal-dir>

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: replay_journal <journal-dir>");
        std::process::exit(2);
    });
    match vike_core::replay::replay_offline(std::path::Path::new(&dir)) {
        Ok(out) => {
            // `records` is the TOTAL source journal record count (Cmds + Snaps), not the re-folded
            // tail length — label it honestly.
            println!("journal records  : {}", out.records);
            println!("snaps verified   : {}", out.snaps_compared);
            println!("final state hash : {:016x}", out.final_hash);
            for e in &out.engines {
                println!(
                    "engine {}/{}: {} open orders, {} positions, balance {}",
                    e.venue,
                    e.symbol,
                    e.registry.len(),
                    e.account.positions.len(),
                    e.account.balance
                );
            }
        }
        Err(e) => {
            eprintln!("REPLAY FAILED: {e:?}");
            std::process::exit(1);
        }
    }
}
