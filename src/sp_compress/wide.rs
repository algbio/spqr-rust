#![allow(dead_code)]
#![allow(clippy::needless_range_loop)]

use crate::wide::{EdgeId, NodeId};
use std::collections::HashMap;
use std::time::Instant;

pub type ChildRef = u64;
pub type SpNodeId = u64;

pub const TAG_BIT: u64 = 0x8000_0000_0000_0000;
pub const PAYLOAD_MASK: u64 = 0x7FFF_FFFF_FFFF_FFFF;

pub const INVALID_SP_NODE: SpNodeId = u64::MAX;

#[inline(always)]
pub const fn make_child_edge(eid: EdgeId) -> ChildRef {
    eid.0
}

#[inline(always)]
pub const fn make_child_macro(mid: SpNodeId) -> ChildRef {
    mid | TAG_BIT
}

#[inline(always)]
pub const fn child_is_macro(c: ChildRef) -> bool {
    (c & TAG_BIT) != 0
}

#[inline(always)]
pub const fn child_is_edge(c: ChildRef) -> bool {
    (c & TAG_BIT) == 0
}

#[inline(always)]
pub const fn child_as_edge(c: ChildRef) -> EdgeId {
    EdgeId(c)
}

#[inline(always)]
pub const fn child_as_macro(c: ChildRef) -> SpNodeId {
    c & PAYLOAD_MASK
}

pub const SP_KIND_SERIES: u8 = 1;
pub const SP_KIND_PARALLEL: u8 = 2;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SpNode {
    pub kind: u8,
    pub _pad: [u8; 3],
    pub left: u64,
    pub right: u64,
    pub children_offset: u64,
    pub children_count: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct CoreEdge {
    pub u: u64,
    pub v: u64,
    pub child: ChildRef,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct CompressionStats {
    pub input_nodes: u64,
    pub input_edges: u64,
    pub core_nodes: u64,
    pub core_edges_count: u64,
    pub macro_count: u64,
    pub macro_series: u64,
    pub macro_parallel: u64,
    pub series_reductions: u64,
    pub parallel_reductions: u64,
    pub iterations: u64,

    pub fully_sp_reducible: u8,
}

#[derive(Clone, Debug)]
pub struct CompressionInput {
    pub n_nodes: u64,
    pub edges: Vec<InputEdge>,

    pub contractible: Vec<u8>,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct InputEdge {
    pub u: NodeId,
    pub v: NodeId,
    pub original_edge_id: EdgeId,
}

#[derive(Default)]
pub struct SpTree {
    pub macros: Vec<SpNode>,
    pub children: Vec<ChildRef>,
    pub core_edges: Vec<CoreEdge>,
    pub core_nodes: Vec<NodeId>,

    pub input_endpoints: Vec<[u64; 2]>,

    pub stats: CompressionStats,
}

impl SpTree {
    pub fn set_input_edges(&mut self, edges: &[InputEdge]) {
        self.input_endpoints.clear();
        self.input_endpoints.reserve(edges.len());
        for e in edges {
            self.input_endpoints.push([e.u.0, e.v.0]);
        }
    }

    pub fn for_each_original_edge<F: FnMut(EdgeId)>(&self, c: ChildRef, fn_: &mut F) {
        if child_is_edge(c) {
            fn_(child_as_edge(c));
            return;
        }
        let m = self.macros[child_as_macro(c) as usize];
        for i in 0..m.children_count {
            let cr = self.children[(m.children_offset + i) as usize];
            self.for_each_original_edge(cr, fn_);
        }
    }

    pub fn count_atomic_descendants(&self, c: ChildRef) -> u64 {
        if child_is_edge(c) {
            return 1;
        }
        let m = self.macros[child_as_macro(c) as usize];
        let mut total = 0;
        for i in 0..m.children_count {
            total += self.count_atomic_descendants(self.children[(m.children_offset + i) as usize]);
        }
        total
    }

    pub fn count_atomic_descendants_macro(&self, mid: SpNodeId) -> u64 {
        let m = self.macros[mid as usize];
        let mut total = 0;
        for i in 0..m.children_count {
            total += self.count_atomic_descendants(self.children[(m.children_offset + i) as usize]);
        }
        total
    }

    pub fn update_stats(&mut self) {
        self.stats.macro_count = self.macros.len() as u64;
        self.stats.macro_series = 0;
        self.stats.macro_parallel = 0;
        for m in &self.macros {
            if m.kind == SP_KIND_SERIES {
                self.stats.macro_series += 1;
            } else if m.kind == SP_KIND_PARALLEL {
                self.stats.macro_parallel += 1;
            }
        }
        self.stats.core_edges_count = self.core_edges.len() as u64;
        self.stats.core_nodes = self.core_nodes.len() as u64;
    }
}

pub struct CompressionResult {
    pub tree: SpTree,
    pub success: bool,
    pub error_message: Option<&'static str>,
}

pub const INVALID_ADJ: u64 = u64::MAX;

#[derive(Clone, Copy, Debug)]
pub struct AdjLink {
    pub edge_idx: u64,
    pub prev: u64,
    pub next: u64,
}

pub struct AdjStore {
    pub head: Vec<u64>,
    pub deg: Vec<u64>,
    pub pool: Vec<AdjLink>,
}

impl AdjStore {
    pub fn new() -> Self {
        AdjStore {
            head: Vec::new(),
            deg: Vec::new(),
            pool: Vec::new(),
        }
    }

    pub fn init(&mut self, n_nodes: u64, edges_capacity: usize) {
        self.head.clear();
        self.head.resize(n_nodes as usize, INVALID_ADJ);
        self.deg.clear();
        self.deg.resize(n_nodes as usize, 0);
        self.pool.clear();

        let reserve_hint = edges_capacity
            .saturating_mul(2)
            .saturating_add(16)
            .min(1 << 26);
        self.pool.reserve(reserve_hint);
    }

    #[inline]
    pub fn insert(&mut self, v: NodeId, edge_idx: u64) -> u64 {
        let idx = self.pool.len() as u64;
        let head_v = self.head[v.idx()];
        self.pool.push(AdjLink {
            edge_idx,
            prev: INVALID_ADJ,
            next: head_v,
        });
        if head_v != INVALID_ADJ {
            self.pool[head_v as usize].prev = idx;
        }
        self.head[v.idx()] = idx;
        self.deg[v.idx()] += 1;
        idx
    }

    #[inline]
    pub fn remove(&mut self, v: NodeId, adj_idx: u64) {
        let (prev, next) = {
            let an = &self.pool[adj_idx as usize];
            (an.prev, an.next)
        };
        if prev != INVALID_ADJ {
            self.pool[prev as usize].next = next;
        } else {
            self.head[v.idx()] = next;
        }
        if next != INVALID_ADJ {
            self.pool[next as usize].prev = prev;
        }

        let an = &mut self.pool[adj_idx as usize];
        an.prev = INVALID_ADJ;
        an.next = INVALID_ADJ;
        self.deg[v.idx()] -= 1;
    }

    #[inline]
    pub fn take_two(&self, v: NodeId) -> (u64, u64) {
        let cur = self.head[v.idx()];
        debug_assert!(cur != INVALID_ADJ);
        let e1 = self.pool[cur as usize].edge_idx;
        let nxt = self.pool[cur as usize].next;
        debug_assert!(nxt != INVALID_ADJ);
        let e2 = self.pool[nxt as usize].edge_idx;
        (e1, e2)
    }

    pub fn drop_storage(&mut self) {
        self.head = Vec::new();
        self.deg = Vec::new();
        self.pool = Vec::new();
    }
}

impl Default for AdjStore {
    fn default() -> Self {
        Self::new()
    }
}

pub const INVALID_PNODE: u64 = u64::MAX;
pub const INVALID_EDGE: EdgeId = EdgeId::INVALID;
const _: () = {
    let _ = INVALID_SP_NODE;
};

pub const PK_ATOMIC: u8 = 0;
pub const PK_SERIES: u8 = 1;
pub const PK_PARALLEL: u8 = 2;

#[derive(Clone, Copy, Debug)]
pub struct PNode {
    pub kind: u8,
    pub alive: bool,
    pub left_kid: u64,
    pub right_kid: u64,
    pub left: NodeId,
    pub right: NodeId,
    pub prev: u64,
    pub next: u64,
    pub edge_id: EdgeId,
}

impl Default for PNode {
    fn default() -> Self {
        PNode {
            kind: 0,
            alive: false,
            left_kid: INVALID_PNODE,
            right_kid: INVALID_PNODE,
            left: NodeId::INVALID,
            right: NodeId::INVALID,
            prev: INVALID_PNODE,
            next: INVALID_PNODE,
            edge_id: INVALID_EDGE,
        }
    }
}

pub struct PNodeArena {
    pub pool: Vec<PNode>,
}

impl PNodeArena {
    pub fn new() -> Self {
        PNodeArena { pool: Vec::new() }
    }

    pub fn reserve(&mut self, capacity: usize) {
        self.pool.reserve(capacity);
    }

    #[inline]
    pub fn make_atomic(&mut self, u: NodeId, v: NodeId, eid: EdgeId) -> u64 {
        let id = self.pool.len() as u64;
        self.pool.push(PNode {
            kind: PK_ATOMIC,
            alive: true,
            left_kid: INVALID_PNODE,
            right_kid: INVALID_PNODE,
            left: u,
            right: v,
            prev: INVALID_PNODE,
            next: INVALID_PNODE,
            edge_id: eid,
        });
        id
    }

    pub fn bulk_init_atomic(&mut self, edges: &[InputEdge]) -> u64 {
        let start = self.pool.len() as u64;
        self.pool.reserve(edges.len());
        for ie in edges {
            self.pool.push(PNode {
                kind: PK_ATOMIC,
                alive: true,
                left_kid: INVALID_PNODE,
                right_kid: INVALID_PNODE,
                left: ie.u,
                right: ie.v,
                prev: INVALID_PNODE,
                next: INVALID_PNODE,
                edge_id: ie.original_edge_id,
            });
        }
        start
    }

    pub fn make_series_pair(&mut self, left: NodeId, right: NodeId, kid_a: u64, kid_b: u64) -> u64 {
        self.pool[kid_a as usize].prev = INVALID_PNODE;
        self.pool[kid_a as usize].next = kid_b;
        self.pool[kid_b as usize].prev = kid_a;
        self.pool[kid_b as usize].next = INVALID_PNODE;

        let id = self.pool.len() as u64;
        self.pool.push(PNode {
            kind: PK_SERIES,
            alive: true,
            left_kid: kid_a,
            right_kid: kid_b,
            left,
            right,
            prev: INVALID_PNODE,
            next: INVALID_PNODE,
            edge_id: INVALID_EDGE,
        });
        id
    }

    pub fn make_parallel(&mut self, u: NodeId, v: NodeId, kids: &[u64]) -> u64 {
        let mut flat_kids = Vec::with_capacity(kids.len());
        self.make_parallel_with_scratch(u, v, kids, &mut flat_kids)
    }

    pub fn make_parallel_with_scratch(
        &mut self,
        u: NodeId,
        v: NodeId,
        kids: &[u64],
        flat_kids: &mut Vec<u64>,
    ) -> u64 {
        let id = self.pool.len() as u64;
        self.pool.push(PNode {
            kind: PK_PARALLEL,
            alive: true,
            left_kid: INVALID_PNODE,
            right_kid: INVALID_PNODE,
            left: u,
            right: v,
            prev: INVALID_PNODE,
            next: INVALID_PNODE,
            edge_id: INVALID_EDGE,
        });

        flat_kids.clear();
        flat_kids.reserve(kids.len());
        for &k in kids {
            if self.pool[k as usize].kind == PK_PARALLEL {
                let mut cc = self.pool[k as usize].left_kid;
                while cc != INVALID_PNODE {
                    flat_kids.push(cc);
                    cc = self.pool[cc as usize].next;
                }
                self.pool[k as usize].alive = false;
                self.pool[k as usize].left_kid = INVALID_PNODE;
                self.pool[k as usize].right_kid = INVALID_PNODE;
            } else {
                flat_kids.push(k);
            }
        }

        if flat_kids.is_empty() {
            self.pool[id as usize].alive = false;
            return id;
        }

        self.pool[id as usize].left_kid = flat_kids[0];
        self.pool[id as usize].right_kid = *flat_kids.last().unwrap();
        for i in 0..flat_kids.len() {
            let cur = flat_kids[i] as usize;
            self.pool[cur].prev = if i == 0 {
                INVALID_PNODE
            } else {
                flat_kids[i - 1]
            };
            self.pool[cur].next = if i + 1 == flat_kids.len() {
                INVALID_PNODE
            } else {
                flat_kids[i + 1]
            };
        }
        id
    }

    pub fn reverse_series_children(&mut self, series_pnode: u64) {
        let mut prev = INVALID_PNODE;
        let mut cur = self.pool[series_pnode as usize].left_kid;
        let old_first = cur;
        let old_last = self.pool[series_pnode as usize].right_kid;
        while cur != INVALID_PNODE {
            let nxt = self.pool[cur as usize].next;
            self.pool[cur as usize].next = prev;
            self.pool[cur as usize].prev = nxt;
            prev = cur;
            cur = nxt;
        }
        let n = &mut self.pool[series_pnode as usize];
        n.left_kid = old_last;
        n.right_kid = old_first;
        std::mem::swap(&mut n.left, &mut n.right);
    }

    #[inline]
    pub fn combine_series(
        &mut self,
        pivot: NodeId,
        left_endpoint: NodeId,
        right_endpoint: NodeId,
        kid_a: u64,
        kid_b: u64,
    ) -> u64 {
        let a_is_series = self.pool[kid_a as usize].kind == PK_SERIES;
        let b_is_series = self.pool[kid_b as usize].kind == PK_SERIES;

        if a_is_series {
            if self.pool[kid_a as usize].right == pivot {
            } else if self.pool[kid_a as usize].left == pivot {
                self.reverse_series_children(kid_a);
            }
        }

        if b_is_series {
            if self.pool[kid_b as usize].left == pivot {
            } else if self.pool[kid_b as usize].right == pivot {
                self.reverse_series_children(kid_b);
            }
        }

        if a_is_series && b_is_series {
            let a_last = self.pool[kid_a as usize].right_kid;
            let b_first = self.pool[kid_b as usize].left_kid;
            let b_last = self.pool[kid_b as usize].right_kid;

            self.pool[a_last as usize].next = b_first;
            self.pool[b_first as usize].prev = a_last;

            self.pool[kid_a as usize].right_kid = b_last;
            self.pool[kid_a as usize].left = left_endpoint;
            self.pool[kid_a as usize].right = right_endpoint;

            self.pool[kid_b as usize].alive = false;
            self.pool[kid_b as usize].left_kid = INVALID_PNODE;
            self.pool[kid_b as usize].right_kid = INVALID_PNODE;
            return kid_a;
        }

        if a_is_series && !b_is_series {
            let a_last = self.pool[kid_a as usize].right_kid;
            self.pool[a_last as usize].next = kid_b;
            self.pool[kid_b as usize].prev = a_last;
            self.pool[kid_b as usize].next = INVALID_PNODE;
            self.pool[kid_a as usize].right_kid = kid_b;
            self.pool[kid_a as usize].left = left_endpoint;
            self.pool[kid_a as usize].right = right_endpoint;
            return kid_a;
        }

        if !a_is_series && b_is_series {
            let b_first = self.pool[kid_b as usize].left_kid;
            self.pool[b_first as usize].prev = kid_a;
            self.pool[kid_a as usize].next = b_first;
            self.pool[kid_a as usize].prev = INVALID_PNODE;
            self.pool[kid_b as usize].left_kid = kid_a;
            self.pool[kid_b as usize].left = left_endpoint;
            self.pool[kid_b as usize].right = right_endpoint;
            return kid_b;
        }

        self.make_series_pair(left_endpoint, right_endpoint, kid_a, kid_b)
    }

    pub fn drop_storage(&mut self) {
        self.pool = Vec::new();
    }
}

impl Default for PNodeArena {
    fn default() -> Self {
        Self::new()
    }
}

pub type PairKey = u128;

#[inline(always)]
pub fn make_pair_key(a: NodeId, b: NodeId) -> PairKey {
    let (lo, hi) = if a.0 <= b.0 { (a.0, b.0) } else { (b.0, a.0) };
    ((lo as u128) << 64) | (hi as u128)
}

#[inline(always)]
pub fn pair_first(k: PairKey) -> NodeId {
    NodeId((k >> 64) as u64)
}
#[inline(always)]
pub fn pair_second(k: PairKey) -> NodeId {
    NodeId(k as u64)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Bucket {
    pub head: u64,
    pub count: u64,
}

pub struct WorkEdgeForBucket<'a> {
    pub bucket_next: &'a mut u64,
}

const BUCKET_DIRTY_BIT: u64 = 0x8000_0000_0000_0000;
const BUCKET_COUNT_MASK: u64 = 0x7FFF_FFFF_FFFF_FFFF;

impl Bucket {
    #[inline(always)]
    pub fn live_count(self) -> u64 {
        self.count & BUCKET_COUNT_MASK
    }

    #[inline(always)]
    pub fn set_live_count(&mut self, count: u64) {
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

    pub free_buckets: Vec<u64>,
    pub mask: usize,
    pub live: usize,
    pub cap: usize,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Slot {
    pub key: PairKey,
    pub value: u64,
}

impl FlatPairMap {
    pub const EMPTY: u64 = u64::MAX;
    pub const TOMBSTONE: u64 = u64::MAX - 1;
    pub const INDIRECT_FLAG: u64 = 0x8000_0000_0000_0000;
    pub const PAYLOAD_MASK: u64 = 0x7FFF_FFFF_FFFF_FFFF;

    #[inline(always)]
    pub fn is_empty(v: u64) -> bool {
        v == Self::EMPTY
    }
    #[inline(always)]
    pub fn is_tombstone(v: u64) -> bool {
        v == Self::TOMBSTONE
    }
    #[inline(always)]
    pub fn is_indirect(v: u64) -> bool {
        v != Self::EMPTY && (v & Self::INDIRECT_FLAG) != 0
    }
    #[inline(always)]
    pub fn is_single(v: u64) -> bool {
        v != Self::EMPTY && v != Self::TOMBSTONE && (v & Self::INDIRECT_FLAG) == 0
    }
    #[inline(always)]
    pub fn single_edge(v: u64) -> u64 {
        v
    }
    #[inline(always)]
    pub fn bucket_index(v: u64) -> u64 {
        v & Self::PAYLOAD_MASK
    }
    #[inline(always)]
    pub fn pack_indirect(bid: u64) -> u64 {
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
    pub fn hash_key(k: PairKey) -> u64 {
        let mut x = (k as u64) ^ ((k >> 64) as u64).rotate_left(32);
        x ^= x >> 33;
        x = x.wrapping_mul(0xff51afd7ed558ccd);
        x ^= x >> 33;
        x = x.wrapping_mul(0xc4ceb9fe1a85ec53);
        x ^= x >> 33;
        x
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

    fn insert_internal(&mut self, key: PairKey, value: u64) {
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
        bucket_next: u64,
        schedule_dirty: bool,
    },

    PromotedAndInserted {
        promoted_edge: u64,
        bucket_next: u64,
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
    pub fn on_seen(&mut self, key: PairKey, edge_idx: u64) -> OnSeenResult {
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
                        let b = self.buckets.len() as u64;
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
    pub fn find_bucket(&self, key: PairKey) -> Option<u64> {
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
    pub fn erase_pair(&mut self, key: PairKey) {
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
    pub fn mark_bucket_clean(&mut self, bucket_id: u64) {
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

#[derive(Default, Clone, Copy, Debug)]
pub(crate) struct CompressionTimings {
    pub t_input_edges_us: u64,
    pub t_init_work_us: u64,
    pub t_init_dirty_us: u64,
    pub t_reduce_series_us: u64,
    pub t_reduce_parallel_us: u64,
    pub t_materialize_us: u64,
    pub t_cleanup_us: u64,
    pub t_canon_series_us: u64,
    pub t_sort_core_edges_us: u64,
    pub t_collect_core_nodes_us: u64,
    pub t_stats_shrink_us: u64,
}

#[derive(Clone, Copy, Debug)]
struct WorkEdge {
    u: NodeId,
    v: NodeId,
    pnode: u64,
    adj_node_u: u64,
    adj_node_v: u64,
    bucket_next: u64,
}

#[inline(always)]
fn work_deactivate(w: &mut WorkEdge) {
    w.pnode = INVALID_PNODE;
}

pub fn compress_dense(input: &CompressionInput) -> CompressionResult {
    compress_borrowed_dense(input.n_nodes, &input.edges, &input.contractible)
}

pub fn compress_borrowed_dense(
    n_nodes: u64,
    input_edges: &[InputEdge],
    contractible: &[u8],
) -> CompressionResult {
    compress_borrowed_impl(n_nodes, input_edges, contractible, None)
}

pub(crate) fn compress_borrowed_dense_timed(
    n_nodes: u64,
    input_edges: &[InputEdge],
    contractible: &[u8],
) -> (CompressionResult, CompressionTimings) {
    let mut timings = CompressionTimings::default();
    let result = compress_borrowed_impl(n_nodes, input_edges, contractible, Some(&mut timings));
    (result, timings)
}

fn compress_borrowed_impl(
    n_nodes: u64,
    input_edges: &[InputEdge],
    contractible: &[u8],
    mut timings: Option<&mut CompressionTimings>,
) -> CompressionResult {
    macro_rules! add_timing {
        ($field:ident, $start:expr) => {
            if let Some(t) = timings.as_mut() {
                t.$field += $start.elapsed().as_micros() as u64;
            }
        };
    }

    let mut tree = SpTree::default();
    tree.stats.input_nodes = n_nodes;
    tree.stats.input_edges = input_edges.len() as u64;

    let t_input_edges = Instant::now();
    tree.set_input_edges(input_edges);
    add_timing!(t_input_edges_us, t_input_edges);

    if contractible.len() < n_nodes as usize {
        return CompressionResult {
            tree,
            success: false,
            error_message: Some("contractible mask shorter than n_nodes"),
        };
    }

    let n_nodes_usize = n_nodes as usize;
    let mut node_dirty: Vec<NodeId> = Vec::new();
    let mut node_in_dirty: Vec<u64> = vec![0; n_nodes_usize.div_ceil(64)];
    let mut pair_dirty: Vec<PairKey> = Vec::new();

    let t_init_work = Instant::now();

    let mut arena = PNodeArena::new();
    arena.reserve(input_edges.len() * 5 / 4 + 16);

    let mut adj = AdjStore::new();

    let mut pmap = FlatPairMap::new();
    let mut pmap_ready = false;

    let pnode_start = arena.bulk_init_atomic(input_edges);

    let mut edges: Vec<WorkEdge> = Vec::with_capacity(input_edges.len() * 5 / 4 + 16);
    adj.init(n_nodes, input_edges.len());

    for (k, ie) in input_edges.iter().enumerate() {
        let pnode_id = pnode_start + k as u64;
        let edge_idx = edges.len() as u64;
        let adj_u = adj.insert(ie.u, edge_idx);
        let adj_v = if ie.u != ie.v {
            adj.insert(ie.v, edge_idx)
        } else {
            INVALID_ADJ
        };
        edges.push(WorkEdge {
            u: ie.u,
            v: ie.v,
            pnode: pnode_id,
            adj_node_u: adj_u,
            adj_node_v: adj_v,
            bucket_next: u64::MAX,
        });

        enqueue_dirty_if_degree_two(
            ie.u,
            &adj,
            &mut node_dirty,
            &mut node_in_dirty,
            contractible,
            n_nodes_usize,
        );
        if ie.u != ie.v {
            enqueue_dirty_if_degree_two(
                ie.v,
                &adj,
                &mut node_dirty,
                &mut node_in_dirty,
                contractible,
                n_nodes_usize,
            );
        }
    }
    add_timing!(t_init_work_us, t_init_work);

    let t_init_dirty = Instant::now();
    add_timing!(t_init_dirty_us, t_init_dirty);

    let mut series_reductions: u64 = 0;
    let mut parallel_reductions: u64 = 0;

    let mut bucket_edges_buf: Vec<u64> = Vec::with_capacity(64);
    let mut kid_pnodes_buf: Vec<u64> = Vec::with_capacity(64);
    let mut flat_pnodes_buf: Vec<u64> = Vec::with_capacity(64);

    while !node_dirty.is_empty() || !pair_dirty.is_empty() || !pmap_ready {
        let t_reduce_series = Instant::now();
        while let Some(v) = node_dirty.pop() {
            let v_idx = v.idx();
            node_in_dirty[v_idx >> 6] &= !(1u64 << (v_idx & 63));

            if contractible[v_idx] == 0 {
                continue;
            }
            if adj.deg[v_idx] != 2 {
                continue;
            }

            let (e1_idx, e2_idx) = adj.take_two(v);
            if e1_idx == e2_idx {
                continue;
            }

            let (e1_p, e1_u, e1_v, e1_au, e1_av) = {
                let e = &edges[e1_idx as usize];
                (e.pnode, e.u, e.v, e.adj_node_u, e.adj_node_v)
            };
            if e1_p == INVALID_PNODE || e1_u == e1_v {
                continue;
            }

            let (e2_p, e2_u, e2_v, e2_au, e2_av) = {
                let e = &edges[e2_idx as usize];
                (e.pnode, e.u, e.v, e.adj_node_u, e.adj_node_v)
            };
            if e2_p == INVALID_PNODE || e2_u == e2_v {
                continue;
            }

            let a = if e1_u == v { e1_v } else { e1_u };
            let b = if e2_u == v { e2_v } else { e2_u };
            if a == v || b == v {
                continue;
            }

            let merged = arena.combine_series(v, a, b, e1_p, e2_p);

            adj.remove(e1_u, e1_au);
            if e1_u != e1_v {
                adj.remove(e1_v, e1_av);
            }
            adj.remove(e2_u, e2_au);
            if e2_u != e2_v {
                adj.remove(e2_v, e2_av);
            }
            work_deactivate(&mut edges[e1_idx as usize]);
            work_deactivate(&mut edges[e2_idx as usize]);

            add_new_edge(
                a,
                b,
                merged,
                &mut edges,
                &mut adj,
                &mut pmap,
                pmap_ready,
                &mut node_dirty,
                &mut node_in_dirty,
                &mut pair_dirty,
                contractible,
                n_nodes_usize,
            );
            series_reductions += 1;
        }
        add_timing!(t_reduce_series_us, t_reduce_series);

        let t_reduce_parallel = Instant::now();
        if !pmap_ready {
            rebuild_pair_map_from_active_edges(&mut pmap, &mut edges, &mut pair_dirty);
            pmap_ready = true;
        }
        while let Some(k) = pair_dirty.pop() {
            let bid = match bucket_compact(&mut pmap, &mut edges, k) {
                Some(b) => b,
                None => continue,
            };
            if pmap.buckets[bid as usize].live_count() < 2 {
                continue;
            }

            bucket_edges_buf.clear();
            let mut cur = pmap.buckets[bid as usize].head;
            while cur != u64::MAX {
                let e = &edges[cur as usize];
                let nxt = e.bucket_next;
                if e.pnode != INVALID_PNODE {
                    bucket_edges_buf.push(cur);
                }
                cur = nxt;
            }
            if bucket_edges_buf.len() < 2 {
                continue;
            }

            let a = pair_first(k);
            let c = pair_second(k);

            kid_pnodes_buf.clear();
            kid_pnodes_buf.reserve(bucket_edges_buf.len());
            for &idx in &bucket_edges_buf {
                kid_pnodes_buf.push(edges[idx as usize].pnode);
            }

            let merged =
                arena.make_parallel_with_scratch(a, c, &kid_pnodes_buf, &mut flat_pnodes_buf);

            for &idx in &bucket_edges_buf {
                let (eu, ev, eau, eav) = {
                    let e = &edges[idx as usize];
                    (e.u, e.v, e.adj_node_u, e.adj_node_v)
                };
                adj.remove(eu, eau);
                if eu != ev {
                    adj.remove(ev, eav);
                }
                work_deactivate(&mut edges[idx as usize]);
            }
            pmap.erase_pair(k);

            add_new_edge(
                a,
                c,
                merged,
                &mut edges,
                &mut adj,
                &mut pmap,
                true,
                &mut node_dirty,
                &mut node_in_dirty,
                &mut pair_dirty,
                contractible,
                n_nodes_usize,
            );
            parallel_reductions += 1;
        }
        add_timing!(t_reduce_parallel_us, t_reduce_parallel);
    }

    let t_materialize = Instant::now();
    let mut node_used: Vec<u64> = vec![0u64; n_nodes_usize.div_ceil(64)];

    tree.children.reserve(input_edges.len());

    let mut mat_stack: Vec<(u64, u8)> = Vec::with_capacity(64);
    let mut mat_resolved: Vec<ChildRef> = Vec::with_capacity(64);
    let mut mat_sort_keys: Vec<(u64, u64, ChildRef)> = Vec::with_capacity(64);

    for i in 0..edges.len() {
        let (epn, mut ce_u, mut ce_v) = {
            let e = &edges[i];
            if e.pnode == INVALID_PNODE {
                continue;
            }
            (e.pnode, e.u, e.v)
        };
        node_used[ce_u.idx() >> 6] |= 1u64 << (ce_u.idx() & 63);
        if ce_u != ce_v {
            node_used[ce_v.idx() >> 6] |= 1u64 << (ce_v.idx() & 63);
        }

        let root_ref = materialize(
            epn,
            &mut arena,
            &mut tree,
            &mut mat_stack,
            &mut mat_resolved,
            &mut mat_sort_keys,
        );

        if ce_u.0 > ce_v.0 {
            std::mem::swap(&mut ce_u, &mut ce_v);
        }
        tree.core_edges.push(CoreEdge {
            u: ce_u.0,
            v: ce_v.0,
            child: root_ref,
        });
    }
    add_timing!(t_materialize_us, t_materialize);

    let t_cleanup = Instant::now();
    arena.drop_storage();
    let _ = std::mem::take(&mut edges);
    adj.drop_storage();
    pmap.drop_storage();
    let _ = std::mem::take(&mut node_dirty);
    let _ = std::mem::take(&mut node_in_dirty);
    let _ = std::mem::take(&mut pair_dirty);
    add_timing!(t_cleanup_us, t_cleanup);

    let t_canon_series = Instant::now();
    canonize_series_orientation(&mut tree);
    add_timing!(t_canon_series_us, t_canon_series);

    let t_sort_core_edges = Instant::now();
    tree.core_edges.sort_unstable_by(|a, b| {
        a.u.cmp(&b.u)
            .then(a.v.cmp(&b.v))
            .then(a.child.cmp(&b.child))
    });
    add_timing!(t_sort_core_edges_us, t_sort_core_edges);

    let t_collect_core_nodes = Instant::now();
    for v_idx in 0..n_nodes_usize {
        if (node_used[v_idx >> 6] & (1u64 << (v_idx & 63))) != 0 {
            tree.core_nodes.push(NodeId(v_idx as u64));
        }
    }
    add_timing!(t_collect_core_nodes_us, t_collect_core_nodes);

    let t_stats_shrink = Instant::now();
    tree.stats.iterations = 1;
    tree.stats.series_reductions = series_reductions;
    tree.stats.parallel_reductions = parallel_reductions;
    tree.stats.fully_sp_reducible =
        if tree.core_edges.len() == 1 && tree.core_edges[0].u != tree.core_edges[0].v {
            1
        } else {
            0
        };

    tree.update_stats();

    add_timing!(t_stats_shrink_us, t_stats_shrink);

    CompressionResult {
        tree,
        success: true,
        error_message: None,
    }
}

#[inline(always)]
fn apply_on_seen(result: OnSeenResult, edge_idx: u64, edges: &mut [WorkEdge]) {
    match result {
        OnSeenResult::SingleStored => {}
        OnSeenResult::InsertedFirst { bucket_next, .. } => {
            edges[edge_idx as usize].bucket_next = bucket_next;
        }
        OnSeenResult::PromotedAndInserted {
            promoted_edge,
            bucket_next,
            ..
        } => {
            edges[promoted_edge as usize].bucket_next = u64::MAX;

            edges[edge_idx as usize].bucket_next = bucket_next;
        }
    }
}

#[inline]
fn enqueue_dirty_if_degree_two(
    w: NodeId,
    adj: &AdjStore,
    node_dirty: &mut Vec<NodeId>,
    node_in_dirty: &mut [u64],
    contractible: &[u8],
    n_nodes: usize,
) {
    let wi = w.idx();
    if wi >= n_nodes {
        return;
    }
    if contractible[wi] == 0 {
        return;
    }
    if adj.deg[wi] != 2 {
        return;
    }
    let bit = 1u64 << (wi & 63);
    if (node_in_dirty[wi >> 6] & bit) != 0 {
        return;
    }
    node_in_dirty[wi >> 6] |= bit;
    node_dirty.push(w);
}

fn rebuild_pair_map_from_active_edges(
    pmap: &mut FlatPairMap,
    edges: &mut [WorkEdge],
    pair_dirty: &mut Vec<PairKey>,
) {
    let active_non_loop = edges
        .iter()
        .filter(|e| e.pnode != INVALID_PNODE && e.u != e.v)
        .count();
    pmap.init(active_non_loop + 16);
    pair_dirty.clear();

    for idx in 0..edges.len() {
        let (active, u, v) = {
            let e = &mut edges[idx];
            e.bucket_next = u64::MAX;
            (e.pnode != INVALID_PNODE && e.u != e.v, e.u, e.v)
        };
        if !active {
            continue;
        }

        let k = make_pair_key(u, v);
        let r = pmap.on_seen(k, idx as u64);
        if r.schedule_dirty() {
            pair_dirty.push(k);
        }
        apply_on_seen(r, idx as u64, edges);
    }
}

#[inline]
#[allow(clippy::too_many_arguments)]
fn add_new_edge(
    u: NodeId,
    v: NodeId,
    pnode_id: u64,
    edges: &mut Vec<WorkEdge>,
    adj: &mut AdjStore,
    pmap: &mut FlatPairMap,
    track_pairs: bool,
    node_dirty: &mut Vec<NodeId>,
    node_in_dirty: &mut [u64],
    pair_dirty: &mut Vec<PairKey>,
    contractible: &[u8],
    n_nodes: usize,
) -> u64 {
    let idx = edges.len() as u64;
    let adj_u = adj.insert(u, idx);
    let adj_v = if u != v {
        adj.insert(v, idx)
    } else {
        INVALID_ADJ
    };
    edges.push(WorkEdge {
        u,
        v,
        pnode: pnode_id,
        adj_node_u: adj_u,
        adj_node_v: adj_v,
        bucket_next: u64::MAX,
    });

    if track_pairs && u != v {
        let k = make_pair_key(u, v);
        let r = pmap.on_seen(k, idx);
        if r.schedule_dirty() {
            pair_dirty.push(k);
        }
        apply_on_seen(r, idx, edges);
    }

    enqueue_dirty_if_degree_two(u, adj, node_dirty, node_in_dirty, contractible, n_nodes);
    if u != v {
        enqueue_dirty_if_degree_two(v, adj, node_dirty, node_in_dirty, contractible, n_nodes);
    }

    idx
}

#[inline]
fn bucket_compact(pmap: &mut FlatPairMap, edges: &mut [WorkEdge], k: PairKey) -> Option<u64> {
    let bid = pmap.find_bucket(k)?;
    pmap.mark_bucket_clean(bid);
    let bid_us = bid as usize;
    let mut cur = pmap.buckets[bid_us].head;
    let mut new_head: u64 = u64::MAX;
    let mut kept: u64 = 0;
    while cur != u64::MAX {
        let e = &mut edges[cur as usize];
        let nxt = e.bucket_next;
        if e.pnode != INVALID_PNODE {
            e.bucket_next = new_head;
            new_head = cur;
            kept += 1;
        }
        cur = nxt;
    }
    pmap.buckets[bid_us].head = new_head;
    pmap.buckets[bid_us].set_live_count(kept);
    if kept == 0 {
        pmap.erase_pair(k);
        return None;
    }
    Some(bid)
}

fn materialize(
    root_pnode: u64,
    arena: &mut PNodeArena,
    tree: &mut SpTree,
    mat_stack: &mut Vec<(u64, u8)>,
    mat_resolved: &mut Vec<ChildRef>,
    mat_sort_keys: &mut Vec<(u64, u64, ChildRef)>,
) -> ChildRef {
    mat_stack.clear();
    mat_stack.push((root_pnode, 0));

    while let Some(&top) = mat_stack.last() {
        let (p, phase) = top;

        let (kind, left_kid, left, right) = {
            let pn = &arena.pool[p as usize];
            (pn.kind, pn.left_kid, pn.left, pn.right)
        };

        if kind == PK_ATOMIC {
            mat_stack.pop();
            continue;
        }

        if phase == 0 {
            mat_stack.last_mut().unwrap().1 = 1;
            let mut c = left_kid;
            while c != INVALID_PNODE {
                mat_stack.push((c, 0));
                c = arena.pool[c as usize].next;
            }
            continue;
        }

        mat_resolved.clear();
        let mut c = left_kid;
        while c != INVALID_PNODE {
            let cn = &arena.pool[c as usize];
            let next = cn.next;
            if cn.kind == PK_ATOMIC {
                mat_resolved.push(make_child_edge(cn.edge_id));
            } else {
                let mm: SpNodeId = cn.edge_id.0;
                mat_resolved.push(make_child_macro(mm));
            }
            c = next;
        }

        if kind == PK_PARALLEL && mat_resolved.len() > 1 {
            let macros_snapshot: &[SpNode] = &tree.macros;
            let children_snapshot: &[ChildRef] = &tree.children;
            let first_edge_of = |r: ChildRef| -> u64 {
                if child_is_edge(r) {
                    return child_as_edge(r).0;
                }
                let mut cur = r;
                while child_is_macro(cur) {
                    let m = &macros_snapshot[child_as_macro(cur) as usize];
                    if m.children_count == 0 {
                        return EdgeId::INVALID.0;
                    }
                    cur = children_snapshot[m.children_offset as usize];
                }
                if child_is_edge(cur) {
                    child_as_edge(cur).0
                } else {
                    EdgeId::INVALID.0
                }
            };

            mat_sort_keys.clear();
            mat_sort_keys.reserve(mat_resolved.len());
            for &r in mat_resolved.iter() {
                let kind_key = if child_is_edge(r) {
                    0
                } else {
                    macros_snapshot[child_as_macro(r) as usize].kind as u64
                };
                mat_sort_keys.push((kind_key, first_edge_of(r), r));
            }
            mat_sort_keys.sort_unstable();
            mat_resolved.clear();
            mat_resolved.extend(mat_sort_keys.iter().map(|&(_, _, r)| r));
        }

        let children_offset = tree.children.len() as u64;
        for &cr in mat_resolved.iter() {
            tree.children.push(cr);
        }

        let m = SpNode {
            kind: if kind == PK_SERIES {
                SP_KIND_SERIES
            } else {
                SP_KIND_PARALLEL
            },
            _pad: [0; 3],
            left: left.0,
            right: right.0,
            children_offset,
            children_count: mat_resolved.len() as u64,
        };

        let new_mid = tree.macros.len() as SpNodeId;
        tree.macros.push(m);

        arena.pool[p as usize].edge_id = EdgeId(new_mid);

        mat_stack.pop();
    }

    let root = &arena.pool[root_pnode as usize];
    if root.kind == PK_ATOMIC {
        return make_child_edge(root.edge_id);
    }
    make_child_macro(root.edge_id.0)
}

fn canonize_series_orientation(tree: &mut SpTree) {
    fn child_first_edge_id(tree: &SpTree, c: ChildRef) -> EdgeId {
        if child_is_edge(c) {
            return child_as_edge(c);
        }
        let mut cur_macro = child_as_macro(c);
        loop {
            let m = tree.macros[cur_macro as usize];
            if m.children_count == 0 {
                return EdgeId::INVALID;
            }
            let first = tree.children[m.children_offset as usize];
            if child_is_edge(first) {
                return child_as_edge(first);
            }
            cur_macro = child_as_macro(first);
        }
    }

    for mid in 0..tree.macros.len() {
        let m = tree.macros[mid];
        if m.kind != SP_KIND_SERIES {
            continue;
        }

        let reverse_it = match m.left.cmp(&m.right) {
            std::cmp::Ordering::Greater => true,
            std::cmp::Ordering::Equal => {
                if m.children_count >= 2 {
                    let first_child = tree.children[m.children_offset as usize];
                    let last_child =
                        tree.children[(m.children_offset + m.children_count - 1) as usize];
                    let ef = child_first_edge_id(tree, first_child);
                    let el = child_first_edge_id(tree, last_child);
                    el < ef
                } else {
                    false
                }
            }
            std::cmp::Ordering::Less => false,
        };

        if reverse_it {
            let off = m.children_offset as usize;
            let cnt = m.children_count as usize;
            let mut a = 0;
            let mut b = cnt - 1;
            while a < b {
                tree.children.swap(off + a, off + b);
                a += 1;
                b -= 1;
            }
            let mref = &mut tree.macros[mid];
            std::mem::swap(&mut mref.left, &mut mref.right);
        }
    }
}

#[inline]
fn contractible_value(mask: &[u8], original: u64) -> u8 {
    if mask.is_empty() {
        return 0;
    }
    let Ok(idx) = usize::try_from(original) else {
        return 0;
    };
    mask.get(idx).copied().unwrap_or(0)
}

pub fn compress_borrowed_remapped(
    n_nodes: u64,
    input_edges: &[InputEdge],
    contractible: &[u8],
) -> CompressionResult {
    let mut tree = SpTree::default();
    tree.stats.input_nodes = n_nodes;
    tree.stats.input_edges = input_edges.len() as u64;
    tree.set_input_edges(input_edges);

    if input_edges.is_empty() {
        return CompressionResult {
            tree,
            success: true,
            error_message: None,
        };
    }

    let mut originals: Vec<u64> = Vec::with_capacity(input_edges.len().saturating_mul(2));
    for e in input_edges {
        if e.u.0 >= n_nodes || e.v.0 >= n_nodes {
            return CompressionResult {
                tree,
                success: false,
                error_message: Some("edge endpoint outside n_nodes"),
            };
        }
        originals.push(e.u.0);
        originals.push(e.v.0);
    }
    originals.sort_unstable();
    originals.dedup();

    let mut map: HashMap<u64, u64> = HashMap::with_capacity(originals.len().saturating_mul(2));
    for (idx, &node) in originals.iter().enumerate() {
        map.insert(node, idx as u64);
    }

    let mut dense_edges = Vec::with_capacity(input_edges.len());
    for e in input_edges {
        let u = *map.get(&e.u.0).expect("endpoint missing from dense remap");
        let v = *map.get(&e.v.0).expect("endpoint missing from dense remap");
        dense_edges.push(InputEdge {
            u: NodeId(u),
            v: NodeId(v),
            original_edge_id: e.original_edge_id,
        });
    }

    let dense_contractible = if contractible.len() == originals.len() {
        contractible.to_vec()
    } else {
        let mut dense_contractible = Vec::with_capacity(originals.len());
        for &node in &originals {
            dense_contractible.push(contractible_value(contractible, node));
        }
        dense_contractible
    };

    let mut result =
        compress_borrowed_dense(originals.len() as u64, &dense_edges, &dense_contractible);
    if !result.success {
        result.tree.stats.input_nodes = n_nodes;
        result.tree.stats.input_edges = input_edges.len() as u64;
        result.tree.set_input_edges(input_edges);
        return result;
    }

    for m in &mut result.tree.macros {
        m.left = originals[m.left as usize];
        m.right = originals[m.right as usize];
    }
    for ce in &mut result.tree.core_edges {
        ce.u = originals[ce.u as usize];
        ce.v = originals[ce.v as usize];
    }
    for v in &mut result.tree.core_nodes {
        *v = NodeId(originals[v.idx()]);
    }
    result.tree.set_input_edges(input_edges);
    result.tree.stats.input_nodes = n_nodes;
    result.tree.stats.input_edges = input_edges.len() as u64;
    result
}
