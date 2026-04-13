//! Brute-force SPQR tree verification on random biconnected multigraphs
//!
//! Run with: "cargo test --release -- --ignored"
//!
//! Configure the number of graphs via environment variables:
//!
//! - SPQR_NUM_RANDOM=50000 cargo test --release -- --ignored
//! - SPQR_NUM_LARGE=2000 cargo test --release brute_force_large -- --ignored
//!
//! Defaults: SPQR_NUM_RANDOM = 10000, SPQR_NUM_LARGE=500

use rand::prelude::*;
use rand::SeedableRng;
use spqr_rust::verify::{verify_spqr_tree_with_options, VerifyOptions};
use spqr_rust::*;

fn env_count(var: &str, default: usize) -> usize {
    std::env::var(var)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn random_biconnected_multigraph(rng: &mut impl Rng, n: usize, extra: usize) -> Graph {
    assert!(n >= 2);
    let mut perm: Vec<u32> = (0..n as u32).collect();
    perm.shuffle(rng);
    let mut g = Graph::with_capacity(n, n + extra);
    g.add_nodes(n);
    for i in 0..n {
        g.add_edge(NodeId(perm[i]), NodeId(perm[(i + 1) % n]));
    }
    let mut added = 0;
    let mut attempts = 0;
    while added < extra && attempts < extra * 10 {
        attempts += 1;
        let u = rng.gen_range(0..n as u32);
        let v = rng.gen_range(0..n as u32);
        if u == v {
            continue;
        }
        g.add_edge(NodeId(u), NodeId(v));
        added += 1;
    }
    g
}

fn random_biconnected_multigraph_with_self_loops(
    rng: &mut impl Rng,
    n: usize,
    extra: usize,
    self_loops: usize,
) -> Graph {
    assert!(n >= 2);
    let mut perm: Vec<u32> = (0..n as u32).collect();
    perm.shuffle(rng);
    let mut g = Graph::with_capacity(n, n + extra + self_loops);
    g.add_nodes(n);
    for i in 0..n {
        g.add_edge(NodeId(perm[i]), NodeId(perm[(i + 1) % n]));
    }
    let mut added = 0;
    let mut attempts = 0;
    while added < extra && attempts < extra * 10 {
        attempts += 1;
        let u = rng.gen_range(0..n as u32);
        let v = rng.gen_range(0..n as u32);
        if u == v {
            continue;
        }
        g.add_edge(NodeId(u), NodeId(v));
        added += 1;
    }
    for _ in 0..self_loops {
        let v = rng.gen_range(0..n as u32);
        g.add_edge(NodeId(v), NodeId(v));
    }
    g
}

/// no self loops/multiedges
fn random_biconnected_simple(rng: &mut impl Rng, n: usize, extra: usize) -> Graph {
    assert!(n >= 2);
    let mut perm: Vec<u32> = (0..n as u32).collect();
    perm.shuffle(rng);
    let mut g = Graph::with_capacity(n, n + extra);
    g.add_nodes(n);
    let mut existing = std::collections::HashSet::new();
    for i in 0..n {
        let u = perm[i];
        let v = perm[(i + 1) % n];
        let (a, b) = if u <= v { (u, v) } else { (v, u) };
        existing.insert((a, b));
        g.add_edge(NodeId(u), NodeId(v));
    }
    let mut attempts = 0;
    let mut added = 0;
    while added < extra && attempts < extra * 10 {
        attempts += 1;
        let u = rng.gen_range(0..n as u32);
        let v = rng.gen_range(0..n as u32);
        if u == v {
            continue;
        }
        let (a, b) = if u <= v { (u, v) } else { (v, u) };
        if existing.contains(&(a, b)) {
            continue;
        }
        existing.insert((a, b));
        g.add_edge(NodeId(u), NodeId(v));
        added += 1;
    }
    g
}

/// Verify all SPQR invariants on a self loop free graph
fn check_spqr(g: &Graph, label: &str) {
    let tree = build_spqr_tree(g);

    for i in 0..g.num_edges() {
        assert!(
            tree.tree_node_of_edge(EdgeId(i as u32)).is_valid(),
            "[{}] Edge {} not mapped to any tree node",
            label,
            i
        );
    }

    let report_raw = verify_spqr_tree_with_options(
        g,
        &tree,
        VerifyOptions {
            require_reduced: false,
        },
    );
    assert!(
        report_raw.is_ok(),
        "[{}] Pre-normalize verification failed (n={}, m={}):\n{}",
        label,
        g.num_nodes(),
        g.num_edges(),
        report_raw
            .errors
            .iter()
            .map(|e| format!("  {}", e))
            .collect::<Vec<_>>()
            .join("\n")
    );

    let mut tree = tree;
    tree.normalize();
    tree.compact();

    let report_reduced = verify_spqr_tree_with_options(
        g,
        &tree,
        VerifyOptions {
            require_reduced: true,
        },
    );
    assert!(
        report_reduced.is_ok(),
        "[{}] Post-normalize verification failed (n={}, m={}):\n{}",
        label,
        g.num_nodes(),
        g.num_edges(),
        report_reduced
            .errors
            .iter()
            .map(|e| format!("  {}", e))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Verify SPQR invariants on a graph that may contain self loops
fn check_spqr_with_self_loops(g: &Graph, label: &str) {
    let res = build_spqr(g);

    let expected_self_loops: usize = (0..g.num_edges())
        .filter(|&i| {
            let e = g.edge(EdgeId(i as u32));
            e.src == e.dst
        })
        .count();
    assert_eq!(
        res.self_loops.len(),
        expected_self_loops,
        "[{}] Self-loop count mismatch: got {}, expected {}",
        label,
        res.self_loops.len(),
        expected_self_loops
    );

    for &eid in &res.self_loops {
        assert!(
            !res.tree.tree_node_of_edge(eid).is_valid(),
            "[{}] Self-loop {:?} should not be mapped to a tree node",
            label,
            eid
        );
    }

    let non_loop_count = g.num_edges() - expected_self_loops;
    if non_loop_count > 0 {
        for i in 0..g.num_edges() {
            let e = g.edge(EdgeId(i as u32));
            if e.src != e.dst {
                assert!(
                    res.tree.tree_node_of_edge(EdgeId(i as u32)).is_valid(),
                    "[{}] Non-self-loop edge {} not mapped to any tree node",
                    label,
                    i
                );
            }
        }
    }

    let report_raw = verify_spqr_tree_with_options(
        g,
        &res.tree,
        VerifyOptions {
            require_reduced: false,
        },
    );
    assert!(
        report_raw.is_ok(),
        "[{}] Pre-normalize verification failed (n={}, m={}, self_loops={}):\n{}",
        label,
        g.num_nodes(),
        g.num_edges(),
        res.self_loops.len(),
        report_raw
            .errors
            .iter()
            .map(|e| format!("  {}", e))
            .collect::<Vec<_>>()
            .join("\n")
    );

    if !res.tree.is_empty() {
        let mut tree = res.tree;
        tree.normalize();
        tree.compact();

        let report_reduced = verify_spqr_tree_with_options(
            g,
            &tree,
            VerifyOptions {
                require_reduced: true,
            },
        );
        assert!(
            report_reduced.is_ok(),
            "[{}] Post-normalize verification failed (n={}, m={}, self_loops={}):\n{}",
            label,
            g.num_nodes(),
            g.num_edges(),
            expected_self_loops,
            report_reduced
                .errors
                .iter()
                .map(|e| format!("  {}", e))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}

#[test]
#[ignore]
fn brute_force_random_biconnected_multigraphs() {
    let count = env_count("SPQR_NUM_RANDOM", 10_000);
    let mut rng = StdRng::seed_from_u64(0xDEAD_BEEF);
    for i in 0..count {
        let n = rng.gen_range(2..=50);
        let extra = rng.gen_range(0..=80);
        let g = random_biconnected_multigraph(&mut rng, n, extra);
        check_spqr(
            &g,
            &format!("multi#{} n={} m={}", i, g.num_nodes(), g.num_edges()),
        );
    }
    eprintln!(
        "brute_force_random_biconnected_multigraphs: {} passed",
        count
    );
}

#[test]
#[ignore]
fn brute_force_random_biconnected_simple() {
    let count = env_count("SPQR_NUM_RANDOM", 10_000);
    let mut rng = StdRng::seed_from_u64(0xCAFE_BABE);
    for i in 0..count {
        let n = rng.gen_range(2..=50);
        let extra = rng.gen_range(0..=60);
        let g = random_biconnected_simple(&mut rng, n, extra);
        check_spqr(
            &g,
            &format!("simple#{} n={} m={}", i, g.num_nodes(), g.num_edges()),
        );
    }
    eprintln!("brute_force_random_biconnected_simple: {} passed", count);
}

#[test]
#[ignore]
fn brute_force_random_with_self_loops() {
    let count = env_count("SPQR_NUM_RANDOM", 10_000);
    let mut rng = StdRng::seed_from_u64(0xBAAD_F00D);
    for i in 0..count {
        let n = rng.gen_range(2..=50);
        let extra = rng.gen_range(0..=60);
        let loops = rng.gen_range(0..=20);
        let g = random_biconnected_multigraph_with_self_loops(&mut rng, n, extra, loops);
        check_spqr_with_self_loops(
            &g,
            &format!("selfloop#{} n={} m={}", i, g.num_nodes(), g.num_edges()),
        );
    }
    eprintln!("brute_force_random_with_self_loops: {} passed", count);
}

#[test]
#[ignore]
fn brute_force_small_exhaustive() {
    let mut count = 0;
    for n in 3..=6usize {
        let mut cycle_edges = std::collections::HashSet::new();
        for i in 0..n {
            let (a, b) = (i as u32, ((i + 1) % n) as u32);
            let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
            cycle_edges.insert((lo, hi));
        }
        let mut extras: Vec<(u32, u32)> = Vec::new();
        for u in 0..n as u32 {
            for v in (u + 1)..n as u32 {
                if !cycle_edges.contains(&(u, v)) {
                    extras.push((u, v));
                }
            }
        }
        let k = extras.len();
        for mask in 0..(1u64 << k) {
            let mut g = Graph::with_capacity(n, n + k);
            g.add_nodes(n);
            for i in 0..n {
                g.add_edge(NodeId(i as u32), NodeId(((i + 1) % n) as u32));
            }
            for (bit, &(u, v)) in extras.iter().enumerate() {
                if mask & (1u64 << bit) != 0 {
                    g.add_edge(NodeId(u), NodeId(v));
                }
            }
            check_spqr(&g, &format!("exhaust_n{}_mask{:#x}", n, mask));
            count += 1;
        }
    }
    eprintln!("brute_force_small_exhaustive: {} graphs verified", count);
}

#[test]
#[ignore]
fn brute_force_complete_graphs() {
    for n in 2..=12usize {
        let m = n * (n - 1) / 2;
        let mut g = Graph::with_capacity(n, m);
        g.add_nodes(n);
        for u in 0..n {
            for v in (u + 1)..n {
                g.add_edge(NodeId(u as u32), NodeId(v as u32));
            }
        }
        check_spqr(&g, &format!("K{}", n));
    }
    eprintln!("brute_force_complete_graphs: K2..K12 passed");
}

#[test]
#[ignore]
fn brute_force_large_random() {
    let count = env_count("SPQR_NUM_LARGE", 500);
    let mut rng = StdRng::seed_from_u64(0x1234_5678);
    for i in 0..count {
        let n = rng.gen_range(50..=300);
        let extra = rng.gen_range(n..=3 * n);
        let g = random_biconnected_multigraph(&mut rng, n, extra);
        check_spqr(
            &g,
            &format!("large#{} n={} m={}", i, g.num_nodes(), g.num_edges()),
        );
    }
    eprintln!("brute_force_large_random: {} passed", count);
}

#[test]
#[ignore]
fn brute_force_large_with_self_loops() {
    let count = env_count("SPQR_NUM_LARGE", 500);
    let mut rng = StdRng::seed_from_u64(0xDEAD_CAFE);
    for i in 0..count {
        let n = rng.gen_range(50..=300);
        let extra = rng.gen_range(n..=3 * n);
        let loops = rng.gen_range(0..=n);
        let g = random_biconnected_multigraph_with_self_loops(&mut rng, n, extra, loops);
        check_spqr_with_self_loops(
            &g,
            &format!("large_sl#{} n={} m={}", i, g.num_nodes(), g.num_edges()),
        );
    }
    eprintln!("brute_force_large_with_self_loops: {} passed", count);
}

/// Verify .spqr format
fn check_spqr_format(g: &Graph, label: &str) {
    use spqr_rust::spqr_format::{parse_spqr_format, to_spqr_string, validate_spqr_format};

    let res = spqr_rust::build_spqr(g);
    let s = to_spqr_string(g, &res);
    let parsed = parse_spqr_format(&s)
        .unwrap_or_else(|e| panic!("[{}] Failed to parse .spqr format: {}\n\n{}", label, e, s));
    if let Err(errors) = validate_spqr_format(&parsed, g, &res) {
        panic!(
            "[{}] Format validation failed:\n  {}\n\nGenerated format:\n{}",
            label,
            errors.join("\n  "),
            s
        );
    }
}

#[test]
#[ignore]
fn brute_force_spqr_format_random() {
    let count = env_count("SPQR_NUM_RANDOM", 10_000);
    let mut rng = StdRng::seed_from_u64(0xF0F0_F0F0);
    for i in 0..count {
        let n = rng.gen_range(2..=50);
        let extra = rng.gen_range(0..=80);
        let g = random_biconnected_multigraph(&mut rng, n, extra);
        check_spqr_format(
            &g,
            &format!("format#{} n={} m={}", i, g.num_nodes(), g.num_edges()),
        );
    }
    eprintln!("brute_force_spqr_format_random: {} passed", count);
}

#[test]
#[ignore]
fn brute_force_spqr_format_with_self_loops() {
    let count = env_count("SPQR_NUM_RANDOM", 10_000);
    let mut rng = StdRng::seed_from_u64(0xABCD_EF01);
    for i in 0..count {
        let n = rng.gen_range(2..=50);
        let extra = rng.gen_range(0..=60);
        let loops = rng.gen_range(0..=20);
        let g = random_biconnected_multigraph_with_self_loops(&mut rng, n, extra, loops);
        check_spqr_format(
            &g,
            &format!("format_sl#{} n={} m={}", i, g.num_nodes(), g.num_edges()),
        );
    }
    eprintln!("brute_force_spqr_format_with_self_loops: {} passed", count);
}
