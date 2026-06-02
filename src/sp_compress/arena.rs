use crate::sp_compress::types::INVALID_SP_NODE;
use crate::{EdgeId, NodeId};

pub const INVALID_PNODE: u32 = u32::MAX;
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
    pub left_kid: u32,
    pub right_kid: u32,
    pub left: NodeId,
    pub right: NodeId,
    pub prev: u32,
    pub next: u32,
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
    pub fn make_atomic(&mut self, u: NodeId, v: NodeId, eid: EdgeId) -> u32 {
        let id = self.pool.len() as u32;
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

    pub fn bulk_init_atomic(&mut self, edges: &[crate::sp_compress::types::InputEdge]) -> u32 {
        let start = self.pool.len() as u32;
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

    pub fn make_series_pair(&mut self, left: NodeId, right: NodeId, kid_a: u32, kid_b: u32) -> u32 {
        self.pool[kid_a as usize].prev = INVALID_PNODE;
        self.pool[kid_a as usize].next = kid_b;
        self.pool[kid_b as usize].prev = kid_a;
        self.pool[kid_b as usize].next = INVALID_PNODE;

        let id = self.pool.len() as u32;
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

    pub fn make_parallel(&mut self, u: NodeId, v: NodeId, kids: &[u32]) -> u32 {
        let mut flat_kids = Vec::with_capacity(kids.len());
        self.make_parallel_with_scratch(u, v, kids, &mut flat_kids)
    }

    pub fn make_parallel_with_scratch(
        &mut self,
        u: NodeId,
        v: NodeId,
        kids: &[u32],
        flat_kids: &mut Vec<u32>,
    ) -> u32 {
        let id = self.pool.len() as u32;
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

    pub fn reverse_series_children(&mut self, series_pnode: u32) {
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
        kid_a: u32,
        kid_b: u32,
    ) -> u32 {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn make_atomic_basic() {
        let mut a = PNodeArena::new();
        let id = a.make_atomic(NodeId(0), NodeId(1), EdgeId(7));
        assert_eq!(id, 0);
        assert_eq!(a.pool[0].kind, PK_ATOMIC);
        assert!(a.pool[0].alive);
        assert_eq!(a.pool[0].edge_id, EdgeId(7));
    }

    #[test]
    fn series_pair_chain() {
        let mut a = PNodeArena::new();
        let e1 = a.make_atomic(NodeId(0), NodeId(1), EdgeId(0));
        let e2 = a.make_atomic(NodeId(1), NodeId(2), EdgeId(1));
        let s = a.make_series_pair(NodeId(0), NodeId(2), e1, e2);
        assert_eq!(a.pool[s as usize].kind, PK_SERIES);
        assert_eq!(a.pool[s as usize].left_kid, e1);
        assert_eq!(a.pool[s as usize].right_kid, e2);
        assert_eq!(a.pool[e1 as usize].next, e2);
        assert_eq!(a.pool[e2 as usize].prev, e1);
    }

    #[test]
    fn parallel_inlines_parallel_kids() {
        let mut a = PNodeArena::new();
        let e1 = a.make_atomic(NodeId(0), NodeId(1), EdgeId(0));
        let e2 = a.make_atomic(NodeId(0), NodeId(1), EdgeId(1));
        let p1 = a.make_parallel(NodeId(0), NodeId(1), &[e1, e2]);

        let e3 = a.make_atomic(NodeId(0), NodeId(1), EdgeId(2));
        let p2 = a.make_parallel(NodeId(0), NodeId(1), &[p1, e3]);

        assert!(!a.pool[p1 as usize].alive);

        let mut count = 0;
        let mut cur = a.pool[p2 as usize].left_kid;
        while cur != INVALID_PNODE {
            count += 1;
            cur = a.pool[cur as usize].next;
        }
        assert_eq!(count, 3);
    }

    #[test]
    fn combine_series_atomic_atomic() {
        let mut a = PNodeArena::new();
        let e1 = a.make_atomic(NodeId(0), NodeId(1), EdgeId(0));
        let e2 = a.make_atomic(NodeId(1), NodeId(2), EdgeId(1));

        let s = a.combine_series(NodeId(1), NodeId(0), NodeId(2), e1, e2);
        assert_eq!(a.pool[s as usize].kind, PK_SERIES);
        assert_eq!(a.pool[s as usize].left, NodeId(0));
        assert_eq!(a.pool[s as usize].right, NodeId(2));
    }

    #[test]
    fn combine_series_absorbs_existing_series() {
        let mut a = PNodeArena::new();

        let e0 = a.make_atomic(NodeId(0), NodeId(1), EdgeId(0));
        let e1 = a.make_atomic(NodeId(1), NodeId(2), EdgeId(1));
        let s_existing = a.make_series_pair(NodeId(0), NodeId(2), e0, e1);

        let e2 = a.make_atomic(NodeId(2), NodeId(3), EdgeId(2));

        let s = a.combine_series(NodeId(2), NodeId(0), NodeId(3), s_existing, e2);

        assert_eq!(s, s_existing);
        assert_eq!(a.pool[s as usize].left, NodeId(0));
        assert_eq!(a.pool[s as usize].right, NodeId(3));

        let mut count = 0;
        let mut cur = a.pool[s as usize].left_kid;
        while cur != INVALID_PNODE {
            count += 1;
            cur = a.pool[cur as usize].next;
        }
        assert_eq!(count, 3);
    }

    #[test]
    fn combine_series_reorients() {
        let mut a = PNodeArena::new();

        let e0 = a.make_atomic(NodeId(2), NodeId(1), EdgeId(0));
        let e1 = a.make_atomic(NodeId(1), NodeId(0), EdgeId(1));
        let s_existing = a.make_series_pair(NodeId(2), NodeId(0), e0, e1);

        let e2 = a.make_atomic(NodeId(2), NodeId(3), EdgeId(2));

        let s = a.combine_series(NodeId(2), NodeId(0), NodeId(3), s_existing, e2);

        assert_eq!(a.pool[s as usize].left, NodeId(0));
        assert_eq!(a.pool[s as usize].right, NodeId(3));
    }
}
