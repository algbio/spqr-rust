//! SPQR tree verifier.
//!
//! Checks invariants:
//! 1. every real edge in exactly one skeleton (self loops excluded)
//! 2. Real edge endpoints match original graph
//! 3. S nodes are simple cycles
//! 4. P nodes are bonds (2 nodes, >= 3 edges)
//! 5. Rnodes are simple and triconnected
//! 6. skeletons have >= 3 edges
//! 7. No adjacent same-type (S-S or P-P)
//! 8. Virtual edges paired in exactly 2 skeletons
//! 9. Twin pointers are reciprocal
//! 10. Shared virtual edge poles map to same original vertices
//! 11. Tree is connected

use crate::*;
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Clone, Debug)]
pub struct VerifyError {
    pub check: &'static str,
    pub detail: String,
}
impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.check, self.detail)
    }
}

#[derive(Clone, Debug)]
pub struct VerifyReport {
    pub errors: Vec<VerifyError>,
}

#[derive(Clone, Copy, Debug)]
pub struct VerifyOptions {
    pub require_reduced: bool,
}
impl Default for VerifyOptions {
    fn default() -> Self {
        Self {
            require_reduced: true,
        }
    }
}

impl VerifyReport {
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
}

pub fn verify_spqr_tree(graph: &Graph, tree: &SpqrTree) -> VerifyReport {
    verify_spqr_tree_with_options(graph, tree, VerifyOptions::default())
}

fn err(check: &'static str, detail: String) -> VerifyError {
    VerifyError { check, detail }
}

pub fn verify_spqr_tree_with_options(
    graph: &Graph,
    tree: &SpqrTree,
    opt: VerifyOptions,
) -> VerifyReport {
    let mut errors = Vec::new();
    let m = graph.num_edges();
    let tlen = tree.len();

    if tlen == 0 {
        let non_loop_count = (0..m)
            .filter(|&i| {
                let e = graph.edge(EdgeId(i as u32));
                e.src != e.dst
            })
            .count();
        if non_loop_count > 0 {
            errors.push(err(
                "empty_tree",
                format!(
                    "Tree empty but graph has {} non-self-loop edges",
                    non_loop_count
                ),
            ));
        }
        return VerifyReport { errors };
    }

    let is_trivial = tlen == 1;

    // 1. Every non self loop real edge in exactly one skeleton
    {
        let mut cnt = vec![0u32; m];
        for tid in tree.iter() {
            for edge in tree.skeleton_edges_slice(tid) {
                if edge.real_edge.is_valid() {
                    let eid = edge.real_edge.idx();
                    if eid < m {
                        cnt[eid] += 1;
                    } else if errors.len() < 20 {
                        errors.push(err(
                            "edge_partition",
                            format!("Real edge {} out of range (m={})", eid, m),
                        ));
                    }
                }
            }
        }
        for (eid, &c) in cnt.iter().enumerate() {
            let e = graph.edge(EdgeId(eid as u32));
            let is_self_loop = e.src == e.dst;
            if is_self_loop {
                // Self loops should NOT appear in any skeleton
                if c > 0 && errors.len() < 20 {
                    errors.push(err(
                        "edge_partition",
                        format!(
                            "Self-loop edge {} found in {} skeletons (should be 0)",
                            eid, c
                        ),
                    ));
                }
            } else if c == 0 && errors.len() < 20 {
                errors.push(err("edge_partition", format!("Real edge {} missing", eid)));
            } else if c > 1 && errors.len() < 20 {
                errors.push(err(
                    "edge_partition",
                    format!("Real edge {} in {} skeletons", eid, c),
                ));
            }
        }
    }

    // 2. Real edge endpoints match original graph
    for tid in tree.iter() {
        let n2o = tree.node_mapping_slice(tid);
        for edge in tree.skeleton_edges_slice(tid) {
            if !edge.real_edge.is_valid() {
                continue;
            }
            let eid = edge.real_edge.idx();
            if eid >= m {
                continue;
            }
            let orig = graph.edge(EdgeId(eid as u32));
            let (si, di) = (edge.src.idx(), edge.dst.idx());
            if si >= n2o.len() || di >= n2o.len() {
                continue;
            }
            let (ms, md) = (n2o[si], n2o[di]);
            let ok = (ms == orig.src && md == orig.dst) || (ms == orig.dst && md == orig.src);
            if !ok && errors.len() < 20 {
                errors.push(err(
                    "real_edge_endpoints",
                    format!(
                        "{:?} edge {}: mapped ({:?},{:?}) but orig ({:?},{:?})",
                        tid, eid, ms, md, orig.src, orig.dst
                    ),
                ));
            }
        }
    }

    // 3-6 node type constraints
    for tid in tree.iter() {
        let edges = tree.skeleton_edges_slice(tid);
        let ne = edges.len();
        let mut deg: HashMap<u32, u32> = HashMap::new();
        for e in edges {
            *deg.entry(e.src.0).or_default() += 1;
            *deg.entry(e.dst.0).or_default() += 1;
        }
        let nv = deg.len();
        let node_type = tree.node_type(tid);

        if !is_trivial && ne < 3 && errors.len() < 20 {
            errors.push(err(
                "skeleton_size",
                format!("{:?} {:?}: {} edges (need ≥3)", node_type, tid, ne),
            ));
        }

        match node_type {
            SpqrNodeType::S => {
                if ne != nv && errors.len() < 20 {
                    errors.push(err(
                        "s_node_cycle",
                        format!("S {:?}: |E|={} ≠ |V|={}", tid, ne, nv),
                    ));
                }
                for (&v, &d) in &deg {
                    if d != 2 && errors.len() < 20 {
                        errors.push(err(
                            "s_node_cycle",
                            format!("S {:?}: node {} deg {}", tid, v, d),
                        ));
                        break;
                    }
                }
                if ne == nv && nv >= 3 && !is_skeleton_connected(edges) && errors.len() < 20 {
                    errors.push(err("s_node_cycle", format!("S {:?}: not connected", tid)));
                }
            }
            SpqrNodeType::P => {
                if !is_trivial && nv != 2 && errors.len() < 20 {
                    errors.push(err(
                        "p_node_bond",
                        format!("P {:?}: {} nodes (need 2)", tid, nv),
                    ));
                }
            }
            SpqrNodeType::R => {
                let mut pairs: HashSet<(u32, u32)> = HashSet::new();
                let mut has_dup = false;
                for e in edges {
                    let (a, b) = if e.src.0 <= e.dst.0 {
                        (e.src.0, e.dst.0)
                    } else {
                        (e.dst.0, e.src.0)
                    };
                    if !pairs.insert((a, b)) && !has_dup && errors.len() < 20 {
                        errors.push(err(
                            "r_node_simple",
                            format!("R {:?}: multi-edge ({},{})", tid, a, b),
                        ));
                        has_dup = true;
                    }
                }
                for (&v, &d) in &deg {
                    if d < 3 && errors.len() < 20 {
                        errors.push(err(
                            "r_node_rigid",
                            format!("R {:?}: node {} deg {} (need ≥3)", tid, v, d),
                        ));
                        break;
                    }
                }
                if nv >= 4 && !has_dup && !is_triconnected(edges) && errors.len() < 20 {
                    errors.push(err(
                        "r_node_triconnected",
                        format!("R {:?}: {} nodes, {} edges NOT triconnected", tid, nv, ne),
                    ));
                }
            }
        }
    }

    // 7. No adjacent same type
    if opt.require_reduced {
        for tid in tree.iter() {
            let tp = tree.node_type(tid);
            if tp != SpqrNodeType::S && tp != SpqrNodeType::P {
                continue;
            }
            let parent = tree.parent(tid);
            if parent.is_valid()
                && parent.idx() < tlen
                && tree.node_type(parent) == tp
                && errors.len() < 20
            {
                errors.push(err(
                    "adjacent_same_type",
                    format!("{:?}: {:?} and parent {:?}", tp, tid, parent),
                ));
            }
        }
    }

    // 8-9 virtual edge pairing
    {
        let mut virt_locs: HashMap<u32, usize> = HashMap::new();
        for tid in tree.iter() {
            for edge in tree.skeleton_edges_slice(tid) {
                if edge.virtual_id != INVALID && !edge.real_edge.is_valid() {
                    *virt_locs.entry(edge.virtual_id).or_default() += 1;
                }
            }
        }
        for (&vid, &count) in &virt_locs {
            if count != 2 && errors.len() < 20 {
                errors.push(err(
                    "virtual_pairing",
                    format!("Virtual {} in {} skeletons", vid, count),
                ));
            }
        }
        for tid in tree.iter() {
            let edges = tree.skeleton_edges_slice(tid);
            for (ei, edge) in edges.iter().enumerate() {
                if !edge.twin_tree_node.is_valid() {
                    continue;
                }
                let tt = edge.twin_tree_node;
                if tt.idx() >= tlen {
                    continue;
                }
                let tei = edge.twin_edge_idx as usize;
                let twin_edges = tree.skeleton_edges_slice(tt);
                if tei >= twin_edges.len() {
                    continue;
                }
                let te = &twin_edges[tei];
                if (te.twin_tree_node != tid || te.twin_edge_idx != ei as u32) && errors.len() < 20
                {
                    errors.push(err(
                        "virtual_twin_reciprocal",
                        format!(
                            "{:?}[{}] -> ({:?},{}) but back ({:?},{})",
                            tid, ei, tt, tei, te.twin_tree_node, te.twin_edge_idx
                        ),
                    ));
                }
            }
        }
    }

    // 10. Pole consistency
    for tid in tree.iter() {
        let edges = tree.skeleton_edges_slice(tid);
        let na = tree.node_mapping_slice(tid);
        for (ei, edge) in edges.iter().enumerate() {
            if !edge.twin_tree_node.is_valid() {
                continue;
            }
            let tt = edge.twin_tree_node;
            if tt.idx() >= tlen {
                continue;
            }
            let tei = edge.twin_edge_idx as usize;
            let twin_edges = tree.skeleton_edges_slice(tt);
            if tei >= twin_edges.len() {
                continue;
            }
            let te = &twin_edges[tei];
            let nb = tree.node_mapping_slice(tt);
            let (sa, da, sb, db) = (edge.src.idx(), edge.dst.idx(), te.src.idx(), te.dst.idx());
            if sa < na.len() && da < na.len() && sb < nb.len() && db < nb.len() {
                let (a0, a1) = (na[sa], na[da]);
                let (b0, b1) = (nb[sb], nb[db]);
                if !((a0 == b0 && a1 == b1) || (a0 == b1 && a1 == b0)) && errors.len() < 20 {
                    errors.push(err(
                        "pole_consistency",
                        format!(
                            "{:?}[{}] ({:?},{:?}) vs {:?}[{}] ({:?},{:?})",
                            tid, ei, a0, a1, tt, tei, b0, b1
                        ),
                    ));
                }
            }
        }
    }

    // 11. Tree connectivity
    if tlen > 1 {
        let root = tree.root.idx();
        let mut visited = vec![false; tlen];
        let mut queue = VecDeque::new();
        if root < tlen {
            visited[root] = true;
            queue.push_back(root);
        }
        while let Some(u) = queue.pop_front() {
            let _tid = TreeNodeId(u as u32);
            let children_start = tree.children_offsets[u] as usize;
            let children_end = tree.children_offsets[u + 1] as usize;
            for ci in children_start..children_end {
                let child = tree.children[ci];
                if child.is_valid() && child.idx() < tlen && !visited[child.idx()] {
                    visited[child.idx()] = true;
                    queue.push_back(child.idx());
                }
            }
            let parent = tree.node_parents[u];
            if parent.is_valid() && parent.idx() < tlen && !visited[parent.idx()] {
                visited[parent.idx()] = true;
                queue.push_back(parent.idx());
            }
        }
        let reachable = visited.iter().filter(|&&x| x).count();
        if reachable != tlen {
            errors.push(err(
                "tree_connected",
                format!("{}/{} reachable from root", reachable, tlen),
            ));
        }
    }

    VerifyReport { errors }
}

fn is_skeleton_connected(edges: &[SkeletonEdge]) -> bool {
    if edges.is_empty() {
        return true;
    }
    let mut adj: HashMap<u32, Vec<u32>> = HashMap::new();
    for e in edges {
        adj.entry(e.src.0).or_default().push(e.dst.0);
        adj.entry(e.dst.0).or_default().push(e.src.0);
    }
    let start = edges[0].src.0;
    let mut vis: HashSet<u32> = HashSet::new();
    let mut q = VecDeque::new();
    vis.insert(start);
    q.push_back(start);
    while let Some(u) = q.pop_front() {
        if let Some(nbrs) = adj.get(&u) {
            for &w in nbrs {
                if vis.insert(w) {
                    q.push_back(w);
                }
            }
        }
    }
    vis.len() == adj.len()
}

/// Brute-force triconnectivity check
fn is_triconnected(edges: &[SkeletonEdge]) -> bool {
    let mut node_map: HashMap<u32, usize> = HashMap::new();
    let mut next_id = 0usize;
    for e in edges {
        if let std::collections::hash_map::Entry::Vacant(e) = node_map.entry(e.src.0) {
            e.insert(next_id);
            next_id += 1;
        }
        if let std::collections::hash_map::Entry::Vacant(e) = node_map.entry(e.dst.0) {
            e.insert(next_id);
            next_id += 1;
        }
    }
    let n = next_id;
    if n < 4 {
        return true;
    }

    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for e in edges {
        let u = node_map[&e.src.0];
        let v = node_map[&e.dst.0];
        adj[u].push(v);
        adj[v].push(u);
    }

    for skip in 0..n {
        let start = (0..n).find(|&v| v != skip);
        if start.is_none() {
            continue;
        }
        let start = start.unwrap();

        let mut vis = vec![false; n];
        vis[skip] = true;
        vis[start] = true;
        let mut q = VecDeque::new();
        q.push_back(start);
        while let Some(u) = q.pop_front() {
            for &w in &adj[u] {
                if !vis[w] {
                    vis[w] = true;
                    q.push_back(w);
                }
            }
        }
        if vis.iter().filter(|&&x| x).count() < n {
            return false;
        }

        let mut disc = vec![-1i32; n];
        let mut low = vec![0i32; n];
        let mut timer = 0i32;
        disc[skip] = -2;
        disc[start] = timer;
        low[start] = timer;
        timer += 1;

        struct F {
            v: usize,
            idx: usize,
            children: i32,
            par: i32,
            par_edge_used: bool,
        }
        let mut stk = vec![F {
            v: start,
            idx: 0,
            children: 0,
            par: -1,
            par_edge_used: false,
        }];
        let mut has_art = false;

        while let Some(frame) = stk.last_mut() {
            let u = frame.v;
            if frame.idx >= adj[u].len() {
                let children = frame.children;
                stk.pop();
                if let Some(parent) = stk.last_mut() {
                    low[parent.v] = std::cmp::min(low[parent.v], low[u]);
                    if low[u] >= disc[parent.v] && parent.par >= 0 {
                        has_art = true;
                        break;
                    }
                } else if children >= 2 {
                    has_art = true;
                    break;
                }
                continue;
            }
            let w = adj[u][frame.idx];
            frame.idx += 1;
            if w == skip {
                continue;
            }
            if disc[w] == -1 {
                disc[w] = timer;
                low[w] = timer;
                timer += 1;
                if frame.par < 0 {
                    frame.children += 1;
                }
                stk.push(F {
                    v: w,
                    idx: 0,
                    children: 0,
                    par: u as i32,
                    par_edge_used: false,
                });
            } else if disc[w] >= 0 {
                if w == frame.par as usize && !frame.par_edge_used {
                    frame.par_edge_used = true;
                } else {
                    low[u] = std::cmp::min(low[u], disc[w]);
                }
            }
        }
        if has_art {
            return false;
        }
    }
    true
}
