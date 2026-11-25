//! [`TradeIdLedger`] against the std map it replaces: the same answer for every id, and no table
//! larger than one shard, through hundreds of splits.

use super::*;

/// Ids of mixed shapes (venue-like numbers, uuids-ish, short and long), deterministic.
fn ids(n: usize) -> Vec<String> {
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    (0..n)
        .map(|i| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            match i % 3 {
                0 => format!("{}", x % 1_000_000_000),
                1 => format!("{x:016x}-{i}"),
                _ => format!("t{i}"),
            }
        })
        .collect()
}

/// Every insert returns what std's does (the overwrite of a repeated id included), and afterwards
/// every id answers std's value, absent ids answer nothing, and iteration yields exactly std's set.
#[test]
fn answers_as_a_std_map_does_through_many_splits() {
    let all = ids(60_000);
    let mut ledger = TradeIdLedger::default();
    let mut std_map = HashMap::new();
    for (i, id) in all.iter().enumerate() {
        assert_eq!(ledger.insert(id, i), std_map.insert(id.clone(), i), "{id}");
        // A repeat of an earlier id overwrites its value, on both.
        if i % 7 == 0 {
            let again = &all[i / 2];
            assert_eq!(ledger.insert(again, i), std_map.insert(again.clone(), i), "{again}");
        }
    }
    assert!(ledger.shards.len() > 100, "precondition: many splits, got {}", ledger.shards.len());
    for id in &all {
        assert_eq!(ledger.get(id), std_map.get(id), "{id}");
        assert!(ledger.contains_key(id));
    }
    for absent in ["", "t60000", "nope", "0000000000000000-0"] {
        assert_eq!(ledger.get(absent), std_map.get(absent), "{absent}");
    }
    let mut got: Vec<(&str, &usize)> = ledger.iter().collect();
    let mut want: Vec<(&str, &usize)> = std_map.iter().map(|(id, v)| (id.as_str(), v)).collect();
    got.sort_unstable();
    want.sort_unstable();
    assert_eq!(got, want);
    let clone = ledger.clone();
    assert!(all.iter().all(|id| clone.get(id) == std_map.get(id)), "a clone answers the same");
    // Seeds (`extend` from owned ids) overwrite like std's `extend`.
    ledger.extend([(all[3].clone(), 7), ("seeded".to_string(), 8)]);
    std_map.extend([(all[3].clone(), 7), ("seeded".to_string(), 8)]);
    assert_eq!(ledger.get(&all[3]), std_map.get(&all[3]));
    assert_eq!(ledger.get("seeded"), Some(&8));
}

/// The hasher a shard map reads keys with hands back the STORED hash (turned), never the id: a key
/// placed by hash `h` hashes to `h.rotate_left(32)` whatever its bytes, and so does its lookup form.
#[test]
fn a_shard_map_hashes_a_key_by_its_stored_hash() {
    let build = BuildHasherDefault::<StoredHash>::default();
    let h = 0x0123_4567_89ab_cdef_u64;
    let stored = Keyed { hash: h, id: "anything".into() };
    assert_eq!(build.hash_one(&stored), h.rotate_left(32));
    let lookup: &dyn IdHashed = &(h, "other bytes");
    assert_eq!(build.hash_one(lookup), h.rotate_left(32));
}

/// The bound itself: whatever the ledger holds, no table has room for more than [`SHARD_IDS`],
/// so no insert rehashes more than that; `extend` and `collect` take the same path.
#[test]
fn no_table_has_room_for_more_than_one_shard() {
    let all = ids(130_000);
    let ledger: TradeIdLedger<u32> = all.iter().map(|id| (id.clone(), 0)).collect();
    assert_eq!(ledger.largest_table(), SHARD_IDS, "every full shard split at its limit");
    let distinct: std::collections::HashSet<&String> = all.iter().collect();
    assert_eq!(ledger.keys().count(), distinct.len());
    // Shards are on average over half full: the memory claim of the module doc (ln 2), with slack.
    let mean = distinct.len() as f64 / ledger.shards.len() as f64;
    assert!(mean >= SHARD_IDS as f64 / 2.0, "shards hold {mean:.1} ids on average");
}

/// MEASUREMENT, not a gate (Linux, release): what one split costs in time and in first-touch page
/// faults, on ledgers grown from empty to 200,000 ids on a thread of their own (a thread arena, as the
/// core's). Two passes on the same thread: the second runs on memory the first freed, so the gap
/// between them is what fresh memory costs. Prints one `SPLIT-COST` line per pass.
#[test]
#[ignore = "measurement: cargo test -p vike-execution --release --lib -- --ignored --nocapture measure_split_cost"]
fn measure_split_cost() {
    fn minflt() -> u64 {
        // The tenth field of the thread's `stat`, the seventh after the `)` that ends its name.
        let stat = std::fs::read_to_string("/proc/thread-self/stat").unwrap_or_default();
        stat.rsplit(')').next().and_then(|s| s.split_whitespace().nth(7)?.parse().ok()).unwrap_or(0)
    }
    fn pct(v: &mut [u64], p: f64) -> u64 {
        v.sort_unstable();
        v.get(((v.len() as f64 * p) as usize).min(v.len().saturating_sub(1))).copied().unwrap_or(0)
    }
    std::thread::spawn(|| {
        let all = ids(200_000);
        for pass in 1..=2 {
            let mut ledger: TradeIdLedger<(u64, u32)> = TradeIdLedger::default();
            let (mut split_ns, mut plain_ns, mut faults) = (Vec::new(), Vec::new(), 0);
            for id in &all {
                let hash = ledger.hasher.hash_one(id.as_str());
                let splits = ledger.shards[ledger.shard_of(hash)].ids.len() >= SHARD_IDS;
                let before = if splits { minflt() } else { 0 };
                let t = std::time::Instant::now();
                ledger.insert(id, (0, 0));
                let ns = t.elapsed().as_nanos() as u64;
                if splits {
                    faults += minflt() - before;
                    split_ns.push(ns);
                } else {
                    plain_ns.push(ns);
                }
            }
            let n = split_ns.len().max(1);
            println!(
                "SPLIT-COST pass={pass} shard_ids={SHARD_IDS} splits={} faults_per_split={:.2} \
                 split_ns p50={} p90={} p99={} max={} plain_ns p50={} p99={} p999={} max={}",
                split_ns.len(),
                faults as f64 / n as f64,
                pct(&mut split_ns, 0.5),
                pct(&mut split_ns, 0.9),
                pct(&mut split_ns, 0.99),
                pct(&mut split_ns, 1.0),
                pct(&mut plain_ns, 0.5),
                pct(&mut plain_ns, 0.99),
                pct(&mut plain_ns, 0.999),
                pct(&mut plain_ns, 1.0),
            );
        }
    })
    .join()
    .unwrap();
}
