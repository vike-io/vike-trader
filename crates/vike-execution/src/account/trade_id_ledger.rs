//! **The trade-id dedup sets' map: one that never rehashes all its ids in one insert.**
//!
//! A std `HashMap` doubles its table when it is 7/8 full and moves EVERY entry inside the insert
//! that crossed the line, hashing each id's bytes again. For a set that grows by one id per distinct
//! fill for the life of the process, on the core thread, that is a stall that doubles each time:
//! measured 7-48 ms on the latency box (load 40-150) at an account's 57,345th fill (7/8 of 65,536), then at
//! 114,689, and so on (the `fx-*` latency variants show it at hop 57345).
//!
//! [`TradeIdLedger`] is extendible hashing over std maps. A keyed hash's low bits pick a SLOT, a
//! slot names a SHARD, and a shard is a std map that never holds more than [`SHARD_IDS`]: the insert
//! that would overfill one SPLITS it in two by the next hash bit (doubling the slot table first when
//! the shard already uses every bit the table has). So the most one insert moves is one shard's ids,
//! plus, on a doubling, a copy of the slot table (4 bytes a slot, a few slots a shard: tens of KB at
//! a million ids), whatever the ledger holds. A split rewrites only the slots of the shard it splits,
//! and the slot table and the shard list are reserved for about a million ids at the first split, so
//! neither is reallocated and copied under the core before then.
//!
//! **A move never reads an id.** Every key carries the hash that placed it ([`Keyed`]), and the
//! shards' maps hash a key by reading that number back ([`StoredHash`]), so a split (or a young
//! shard's own growth) copies entries without touching the ids' bytes, which sit scattered on the
//! heap. A lookup by `&str` hashes the id once, as std does, and borrows it as a `(hash, id)` pair
//! ([`IdHashed`]), so it allocates nothing.
//!
//! **What a split costs, measured, and why the shard is 448 ids** (the latency box, lanes' cores; the
//! `measure_split_cost` test beside this file, a ledger grown to 200,000 ids on its own thread, load
//! 25-35): the cost follows the bytes a split touches, the old table's cold lines, the new tables and
//! the first touch of fresh memory, not a fixed cost. At 448 ids a split is 18-20 µs at the median
//! (6 page faults; 7.5-16 µs on memory a freed ledger left), once per ~390 distinct fills; at 224,
//! ~11 µs once per ~190; at 56, 5.6-6 µs once per ~43. The cost per id is the same at every size; the
//! size only decides where it lands: in the latency harness's `fx-4sym` / `fx-mount-foreign` (every
//! hop folds a new fill) 448 raises p999 (6-36 µs to 39-58 µs) and leaves p99 at the base's, while 56
//! and 112 raise p99 to 15-18 µs because their splits pass 1% of the hops. Their max went from 7-48 ms
//! (hop 57345) to the box's noise; the gated variants fold no fill and did not move. A first version
//! that rehashed the strings paid over 100 µs for about half of its splits.
//!
//! **The same answers as the std map.** An id lives in exactly one shard, the one its hash names,
//! and a split moves exactly the ids whose next bit is set together with every slot that names them,
//! so membership, the stored value and a repeated insert's overwrite are std's. ITERATION order is
//! not, and std's was random too: every reader that writes the ids out (the `Snapshot`, `state_hash`)
//! sorts them first (`fill_window::windowed`).
//!
//! **Memory, roughly the same.** A key is 24 bytes, a `String`'s size (the hash plus a `Box<str>`,
//! whose heap block is the id's exact length), so a bucket is what it was. A split builds both halves
//! at 512 buckets (448 ids at 7/8, the fill at which std would double), so a shard is 7/16 to 7/8
//! full: the range a single std table sweeps between doublings, and on average as full (extendible
//! hashing's mean is ln 2 of capacity; a std table's, over its sawtooth, is the same). On top: 40
//! bytes a shard and 4 a slot, under half a byte an id, and from the first split the reserved room of
//! the two lists (224 KB of address space, touched page by page as they fill). A ledger that never
//! reaches 448 ids is one std map, as before.

use std::borrow::Borrow;
use std::collections::HashMap;
use std::hash::{BuildHasher, BuildHasherDefault, Hash, Hasher, RandomState};

/// The most ids one shard holds: 7/8 of 512 buckets, the fill at which a std map would double.
/// The insert that would pass it splits the shard instead, so it bounds what one insert can move.
/// Measured against 56, 112 and 224 (module doc): a smaller shard splits cheaper but often enough to
/// reach p99.
pub(crate) const SHARD_IDS: usize = 448;

/// The shard list's room, reserved at the first split: about a million ids at the mean fill (ln 2,
/// ~0.69, of [`SHARD_IDS`]).
const RESERVE_SHARDS: usize = (1_000_000 * 100 / (SHARD_IDS * 69)).next_power_of_two();

/// The slot table's room, reserved with [`RESERVE_SHARDS`]: an extendible-hashing slot table runs
/// to a few slots a shard.
const RESERVE_SLOTS: usize = 4 * RESERVE_SHARDS;

/// The deepest a shard splits. A shard that shares this many hash bits (a slot table of 16M slots,
/// billions of ids, or a hash that does not spread) grows as a std map does instead of splitting.
/// Below 25, so the slot bits never reach the bits a shard's map reads ([`StoredHash`]).
const MAX_DEPTH: u32 = 24;

/// A map from trade id to `V` whose growth moves at most one shard's ids per insert (module doc).
#[derive(Clone)]
pub(crate) struct TradeIdLedger<V> {
    /// Keys the hash whose low bits pick an id's slot. Fixed for the ledger's life (a clone copies it).
    hasher: RandomState,
    /// `2^depth` entries: slot `hash & (len - 1)` holds the index of the shard that keeps the id.
    slots: Vec<u32>,
    shards: Vec<Shard<V>>,
}

#[derive(Clone)]
struct Shard<V> {
    /// How many low hash bits every id here shares (the slot table has at least as many).
    depth: u32,
    ids: Table<V>,
}

type Table<V> = HashMap<Keyed, V, BuildHasherDefault<StoredHash>>;

/// A stored id with the ledger's hash of it, so a move re-places it without reading the id.
#[derive(Clone)]
struct Keyed {
    hash: u64,
    id: Box<str>,
}

/// The one view a lookup and a stored key share: the hash places it, the id decides equality (equal
/// ids have equal hashes, the ledger's hasher being fixed). A trait object, so a stored [`Keyed`] can
/// lend it and a lookup can build one from a borrowed `&str` (the std `Borrow` rule: both hash and
/// compare alike).
trait IdHashed {
    fn placed_by(&self) -> u64;
    fn id(&self) -> &str;
}

impl IdHashed for Keyed {
    fn placed_by(&self) -> u64 {
        self.hash
    }
    fn id(&self) -> &str {
        &self.id
    }
}

impl IdHashed for (u64, &str) {
    fn placed_by(&self) -> u64 {
        self.0
    }
    fn id(&self) -> &str {
        self.1
    }
}

impl<'a> Borrow<dyn IdHashed + 'a> for Keyed {
    fn borrow(&self) -> &(dyn IdHashed + 'a) {
        self
    }
}

impl Hash for dyn IdHashed + '_ {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.placed_by());
    }
}

impl PartialEq for dyn IdHashed + '_ {
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }
}

impl Eq for dyn IdHashed + '_ {}

impl Hash for Keyed {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.hash);
    }
}

impl PartialEq for Keyed {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for Keyed {}

/// A shard map's hasher: it hands back the stored hash, turned by 32 bits so the map's bucket bits
/// (the low ones) and its tag bits (the top seven) come from bits the slot table never uses (it uses
/// the low [`MAX_DEPTH`] at most). Every key form hashes as one `write_u64`.
#[derive(Default)]
struct StoredHash(u64);

impl Hasher for StoredHash {
    fn write_u64(&mut self, hash: u64) {
        self.0 = hash.rotate_left(32);
    }

    /// Never reached (both key forms write one `u64`); still a hash, so a stray caller stays correct.
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

impl<V> Default for TradeIdLedger<V> {
    fn default() -> Self {
        TradeIdLedger {
            hasher: RandomState::new(),
            slots: vec![0],
            shards: vec![Shard { depth: 0, ids: Table::default() }],
        }
    }
}

impl<V> TradeIdLedger<V> {
    fn shard_of(&self, hash: u64) -> usize {
        self.slots[(hash & (self.slots.len() as u64 - 1)) as usize] as usize
    }

    pub(crate) fn get(&self, id: &str) -> Option<&V> {
        let hash = self.hasher.hash_one(id);
        self.shards[self.shard_of(hash)].ids.get(&(hash, id) as &dyn IdHashed)
    }

    pub(crate) fn contains_key(&self, id: &str) -> bool {
        self.get(id).is_some()
    }

    /// std's `insert`, the id copied in: the old value when `id` was there (its key kept), `None` when
    /// it was not.
    pub(crate) fn insert(&mut self, id: &str, value: V) -> Option<V> {
        self.insert_boxed(id.into(), value)
    }

    fn insert_boxed(&mut self, id: Box<str>, value: V) -> Option<V> {
        let hash = self.hasher.hash_one(&*id);
        loop {
            let s = self.shard_of(hash);
            let shard = &mut self.shards[s];
            if shard.ids.len() < SHARD_IDS
                || shard.depth >= MAX_DEPTH
                || shard.ids.contains_key(&(hash, &*id) as &dyn IdHashed)
            {
                return shard.ids.insert(Keyed { hash, id }, value);
            }
            // Full: split it, and look again (the half the id belongs to may still be full).
            self.split(s, hash);
        }
    }

    /// Split shard `s`, the one `hash` names, by its next hash bit: the ids with the bit clear stay at
    /// `s`, the others move to a new shard, and every slot of `s` with the bit set names the new one.
    /// Both halves are built fresh at [`SHARD_IDS`] room, so neither carries the old table's deleted
    /// entries; the entries move by their stored hash, no id is read.
    #[cold]
    #[inline(never)]
    fn split(&mut self, s: usize, hash: u64) {
        if self.shards.len() == 1 {
            // The first split: a ledger past one shard keeps growing, so give the two lists their room
            // for about a million ids now (address space; a page is touched when a slot or a shard
            // first lands on it) rather than reallocating and copying them under the core later.
            self.shards.reserve(RESERVE_SHARDS);
            self.slots.reserve(RESERVE_SLOTS);
        }
        let depth = self.shards[s].depth;
        if 1usize << depth == self.slots.len() {
            self.slots.extend_from_within(..);
        }
        let bit = 1u64 << depth;
        let room = || Table::with_capacity_and_hasher(SHARD_IDS, BuildHasherDefault::default());
        let (mut low, mut high) = (room(), room());
        for (key, value) in std::mem::take(&mut self.shards[s].ids) {
            if (key.hash & bit) == 0 {
                low.insert(key, value);
            } else {
                high.insert(key, value);
            }
        }
        // At most 2^MAX_DEPTH shards, so the index fits.
        let new = self.shards.len() as u32;
        self.shards[s] = Shard { depth: depth + 1, ids: low };
        self.shards.push(Shard { depth: depth + 1, ids: high });
        // The slots naming `s` are the ones whose low `depth` bits are its ids' (the splitting id's);
        // of them, those with `bit` set now name the new shard. Only they are visited, not the table.
        let step = 1usize << (depth + 1);
        let mut slot = (hash & (bit - 1)) as usize | bit as usize;
        while slot < self.slots.len() {
            debug_assert_eq!(self.slots[slot], s as u32, "slot {slot} must name the split shard");
            self.slots[slot] = new;
            slot += step;
        }
    }

    /// Every `(id, value)`, shard by shard: no order to rely on (module doc).
    pub(crate) fn iter(&self) -> impl Iterator<Item = (&str, &V)> {
        self.shards.iter().flat_map(|s| s.ids.iter().map(|(key, value)| (&*key.id, value)))
    }

    pub(crate) fn keys(&self) -> impl Iterator<Item = &str> {
        self.iter().map(|(id, _)| id)
    }

    /// Test seam: how many ids the largest ONE table has room for, which bounds what one insert
    /// can rehash.
    #[cfg(test)]
    pub(crate) fn largest_table(&self) -> usize {
        self.shards.iter().map(|s| s.ids.capacity()).max().unwrap_or(0)
    }
}

impl<V> Extend<(String, V)> for TradeIdLedger<V> {
    fn extend<I: IntoIterator<Item = (String, V)>>(&mut self, iter: I) {
        for (id, value) in iter {
            self.insert_boxed(id.into_boxed_str(), value);
        }
    }
}

impl<V> FromIterator<(String, V)> for TradeIdLedger<V> {
    fn from_iter<I: IntoIterator<Item = (String, V)>>(iter: I) -> Self {
        let mut ledger = Self::default();
        ledger.extend(iter);
        ledger
    }
}

/// Prints as a map of ids, as the std map it replaces did.
impl<V: std::fmt::Debug> std::fmt::Debug for TradeIdLedger<V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

#[cfg(test)]
#[path = "trade_id_ledger_tests.rs"]
mod trade_id_ledger_tests;
