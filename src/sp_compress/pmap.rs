use crate::NodeId;

pub type PairKey = u64;

#[inline(always)]
pub fn make_pair_key(a: NodeId, b: NodeId) -> PairKey {
    let (lo, hi) = if a.0 <= b.0 { (a.0, b.0) } else { (b.0, a.0) };
    ((lo as u64) << 32) | (hi as u64)
}

#[inline(always)]
pub fn pair_first(k: PairKey) -> NodeId {
    NodeId((k >> 32) as u32)
}
#[inline(always)]
pub fn pair_second(k: PairKey) -> NodeId {
    NodeId((k & 0xFFFF_FFFF) as u32)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Bucket {
    pub head: u32,
    pub count: u32,
}

pub struct WorkEdgeForBucket<'a> {
    pub bucket_next: &'a mut u32,
}

const BUCKET_DIRTY_BIT: u32 = 0x8000_0000;
const BUCKET_COUNT_MASK: u32 = 0x7FFF_FFFF;

impl Bucket {
    #[inline(always)]
    pub fn live_count(self) -> u32 {
        self.count & BUCKET_COUNT_MASK
    }

    #[inline(always)]
    pub fn set_live_count(&mut self, count: u32) {
        let dirty = self.count & BUCKET_DIRTY_BIT;
        self.count = dirty | (count & BUCKET_COUNT_MASK);
    }

    #[inline(always)]
    pub fn increment_count(&mut self) {
        let count = self.live_count();
        self.set_live_count(count + 1);
    }

    #[inline(always)]
    pub fn is_dirty(self) -> bool {
        (self.count & BUCKET_DIRTY_BIT) != 0
    }

    #[inline(always)]
    pub fn mark_dirty(&mut self) {
        self.count |= BUCKET_DIRTY_BIT;
    }

    #[inline(always)]
    pub fn mark_clean(&mut self) {
        self.count &= BUCKET_COUNT_MASK;
    }
}

pub struct FlatPairMap {
    pub slots: Vec<Slot>,

    pub buckets: Vec<Bucket>,

    pub free_buckets: Vec<u32>,
    pub mask: usize,
    pub live: usize,
    pub cap: usize,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Slot {
    pub key: u64,
    pub value: u32,
}

impl FlatPairMap {
    pub const EMPTY: u32 = u32::MAX;
    pub const TOMBSTONE: u32 = u32::MAX - 1;
    pub const INDIRECT_FLAG: u32 = 0x8000_0000;
    pub const PAYLOAD_MASK: u32 = 0x7FFF_FFFF;

    #[inline(always)]
    pub fn is_empty(v: u32) -> bool {
        v == Self::EMPTY
    }
    #[inline(always)]
    pub fn is_tombstone(v: u32) -> bool {
        v == Self::TOMBSTONE
    }
    #[inline(always)]
    pub fn is_indirect(v: u32) -> bool {
        v != Self::EMPTY && (v & Self::INDIRECT_FLAG) != 0
    }
    #[inline(always)]
    pub fn is_single(v: u32) -> bool {
        v != Self::EMPTY && v != Self::TOMBSTONE && (v & Self::INDIRECT_FLAG) == 0
    }
    #[inline(always)]
    pub fn single_edge(v: u32) -> u32 {
        v
    }
    #[inline(always)]
    pub fn bucket_index(v: u32) -> u32 {
        v & Self::PAYLOAD_MASK
    }
    #[inline(always)]
    pub fn pack_indirect(bid: u32) -> u32 {
        bid | Self::INDIRECT_FLAG
    }

    pub fn new() -> Self {
        FlatPairMap {
            slots: Vec::new(),
            buckets: Vec::new(),
            free_buckets: Vec::new(),
            mask: 0,
            live: 0,
            cap: 0,
        }
    }

    pub fn init(&mut self, expected_max_pairs: usize) {
        let mut target: usize = 16;
        let need = expected_max_pairs.saturating_mul(4) / 3 + 1;
        while target < need {
            target *= 2;
        }
        self.slots.clear();
        self.slots.resize(
            target,
            Slot {
                key: 0,
                value: Self::EMPTY,
            },
        );
        self.mask = target - 1;
        self.cap = target;
        self.live = 0;
        self.buckets.clear();
        self.free_buckets.clear();
    }

    #[inline(always)]
    pub fn hash_key(mut k: u64) -> u64 {
        k ^= k >> 33;
        k = k.wrapping_mul(0xff51afd7ed558ccdu64);
        k ^= k >> 33;
        k = k.wrapping_mul(0xc4ceb9fe1a85ec53u64);
        k ^= k >> 33;
        k
    }

    fn rehash(&mut self, new_cap: usize) {
        let old = std::mem::take(&mut self.slots);
        self.slots.resize(
            new_cap,
            Slot {
                key: 0,
                value: Self::EMPTY,
            },
        );
        self.mask = new_cap - 1;
        self.cap = new_cap;
        self.live = 0;
        for s in old.iter() {
            if Self::is_empty(s.value) || Self::is_tombstone(s.value) {
                continue;
            }
            self.insert_internal(s.key, s.value);
        }
    }

    fn insert_internal(&mut self, key: u64, value: u32) {
        let mut i = (Self::hash_key(key) as usize) & self.mask;
        loop {
            let v = self.slots[i].value;
            if Self::is_empty(v) || Self::is_tombstone(v) {
                break;
            }
            i = (i + 1) & self.mask;
        }
        self.slots[i].key = key;
        self.slots[i].value = value;
        self.live += 1;
    }
}

#[derive(Clone, Copy, Debug)]
pub enum OnSeenResult {
    SingleStored,

    InsertedFirst {
        bucket_next: u32,
        schedule_dirty: bool,
    },

    PromotedAndInserted {
        promoted_edge: u32,
        bucket_next: u32,
        schedule_dirty: bool,
    },
}

impl OnSeenResult {
    #[inline(always)]
    pub fn schedule_dirty(self) -> bool {
        match self {
            OnSeenResult::SingleStored => false,
            OnSeenResult::InsertedFirst { schedule_dirty, .. } => schedule_dirty,
            OnSeenResult::PromotedAndInserted { schedule_dirty, .. } => schedule_dirty,
        }
    }
}

impl FlatPairMap {
    #[inline]
    pub fn on_seen(&mut self, key: u64, edge_idx: u32) -> OnSeenResult {
        if self.live * 4 >= self.cap * 3 {
            self.rehash(self.cap * 2);
        }
        let mut i = (Self::hash_key(key) as usize) & self.mask;
        let mut first_tomb: usize = usize::MAX;
        loop {
            let s = self.slots[i];
            let v = s.value;
            if Self::is_empty(v) {
                break;
            }
            if Self::is_tombstone(v) {
                if first_tomb == usize::MAX {
                    first_tomb = i;
                }
            } else if s.key == key {
                if Self::is_single(v) {
                    let prev_edge = Self::single_edge(v);
                    let bid = if let Some(b) = self.free_buckets.pop() {
                        self.buckets[b as usize] = Bucket::default();
                        b
                    } else {
                        let b = self.buckets.len() as u32;
                        self.buckets.push(Bucket::default());
                        b
                    };
                    let b = &mut self.buckets[bid as usize];
                    b.head = edge_idx;
                    b.count = 2;
                    b.mark_dirty();
                    self.slots[i].value = Self::pack_indirect(bid);
                    return OnSeenResult::PromotedAndInserted {
                        promoted_edge: prev_edge,
                        bucket_next: prev_edge,
                        schedule_dirty: true,
                    };
                } else {
                    let bid = Self::bucket_index(v) as usize;
                    let b = &mut self.buckets[bid];
                    let cur_head = b.head;
                    b.head = edge_idx;
                    b.increment_count();
                    let schedule_dirty = !b.is_dirty() && b.live_count() >= 2;
                    if schedule_dirty {
                        b.mark_dirty();
                    }
                    return OnSeenResult::InsertedFirst {
                        bucket_next: cur_head,
                        schedule_dirty,
                    };
                }
            }
            i = (i + 1) & self.mask;
        }

        let insert_at = if first_tomb != usize::MAX {
            first_tomb
        } else {
            i
        };
        let slot = &mut self.slots[insert_at];
        slot.key = key;
        slot.value = edge_idx;
        if first_tomb == usize::MAX {
            self.live += 1;
        }
        OnSeenResult::SingleStored
    }

    #[inline]
    pub fn find_bucket(&self, key: u64) -> Option<u32> {
        let mut i = (Self::hash_key(key) as usize) & self.mask;
        loop {
            let v = self.slots[i].value;
            if Self::is_empty(v) {
                return None;
            }
            if !Self::is_tombstone(v) && self.slots[i].key == key {
                if Self::is_indirect(v) {
                    return Some(Self::bucket_index(v));
                }
                return None;
            }
            i = (i + 1) & self.mask;
        }
    }

    #[inline]
    pub fn erase_pair(&mut self, key: u64) {
        let mut i = (Self::hash_key(key) as usize) & self.mask;
        loop {
            let v = self.slots[i].value;
            if Self::is_empty(v) {
                return;
            }
            if !Self::is_tombstone(v) && self.slots[i].key == key {
                if Self::is_indirect(v) {
                    self.free_buckets.push(Self::bucket_index(v));
                }
                self.slots[i].value = Self::TOMBSTONE;
                return;
            }
            i = (i + 1) & self.mask;
        }
    }

    #[inline]
    pub fn mark_bucket_clean(&mut self, bucket_id: u32) {
        self.buckets[bucket_id as usize].mark_clean();
    }

    pub fn drop_storage(&mut self) {
        self.slots = Vec::new();
        self.buckets = Vec::new();
        self.free_buckets = Vec::new();
    }
}

impl Default for FlatPairMap {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NodeId;

    #[test]
    fn pair_key_canonical() {
        let k1 = make_pair_key(NodeId(3), NodeId(7));
        let k2 = make_pair_key(NodeId(7), NodeId(3));
        assert_eq!(k1, k2);
        assert_eq!(pair_first(k1), NodeId(3));
        assert_eq!(pair_second(k1), NodeId(7));
    }

    #[test]
    fn single_edge_no_bucket() {
        let mut pm = FlatPairMap::new();
        pm.init(16);
        let r = pm.on_seen(make_pair_key(NodeId(0), NodeId(1)), 0);
        assert!(matches!(r, OnSeenResult::SingleStored));

        assert_eq!(pm.buckets.len(), 0);
        assert!(pm
            .find_bucket(make_pair_key(NodeId(0), NodeId(1)))
            .is_none());
    }

    #[test]
    fn promotion_to_bucket() {
        let mut pm = FlatPairMap::new();
        pm.init(16);
        let k = make_pair_key(NodeId(0), NodeId(1));
        let r1 = pm.on_seen(k, 0);
        assert!(matches!(r1, OnSeenResult::SingleStored));
        let r2 = pm.on_seen(k, 1);
        assert!(matches!(
            r2,
            OnSeenResult::PromotedAndInserted {
                promoted_edge: 0,
                ..
            }
        ));

        assert_eq!(pm.buckets.len(), 1);
        let bid = pm.find_bucket(k).expect("must be indirect");
        let b = &pm.buckets[bid as usize];
        assert_eq!(b.live_count(), 2);

        let r3 = pm.on_seen(k, 2);
        assert!(matches!(r3, OnSeenResult::InsertedFirst { .. }));
        let bid = pm.find_bucket(k).unwrap();
        let b = &pm.buckets[bid as usize];
        assert_eq!(b.live_count(), 3);
    }

    #[test]
    fn dirty_schedule_is_deduplicated_until_clean() {
        let mut pm = FlatPairMap::new();
        pm.init(16);
        let k = make_pair_key(NodeId(0), NodeId(1));

        assert!(!pm.on_seen(k, 0).schedule_dirty());
        assert!(pm.on_seen(k, 1).schedule_dirty());
        assert!(!pm.on_seen(k, 2).schedule_dirty());

        let bid = pm.find_bucket(k).expect("must be indirect");
        pm.mark_bucket_clean(bid);

        assert!(pm.on_seen(k, 3).schedule_dirty());
    }

    #[test]
    fn erase_pair_releases_bucket() {
        let mut pm = FlatPairMap::new();
        pm.init(16);
        let k = make_pair_key(NodeId(0), NodeId(1));
        pm.on_seen(k, 0);
        pm.on_seen(k, 1);
        pm.erase_pair(k);
        assert_eq!(pm.free_buckets.len(), 1);
        assert!(pm.find_bucket(k).is_none());
    }
}
