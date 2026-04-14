//! SPQR tree computation

use spqr_rust::{build_spqr, Graph, NodeId};
use std::cmp::Reverse;
use std::collections::HashMap;
use std::env;
use std::fs::File;
use std::io::{BufRead, BufReader};

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() < 2 {
        eprintln!("Usage: {} <graph_file>", args[0]);
        eprintln!("Graph file format: lines of 'node1 node2' (names or 0-indexed numbers)");
        std::process::exit(1);
    }

    let filename = &args[1];
    let file = File::open(filename).expect("Cannot open file");
    let reader = BufReader::new(file);

    let mut nodes: HashMap<String, u32> = HashMap::new();
    let mut next_id: u32 = 0;
    let mut edges: Vec<(u32, u32)> = Vec::new();

    for line in reader.lines() {
        let line = line.expect("IO error");
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }

        let u_name = parts[0].to_string();
        let v_name = parts[1].to_string();

        let u = *nodes.entry(u_name).or_insert_with(|| {
            let id = next_id;
            next_id += 1;
            id
        });

        let v = *nodes.entry(v_name).or_insert_with(|| {
            let id = next_id;
            next_id += 1;
            id
        });

        edges.push((u, v));
    }

    let n = next_id as usize;
    let m = edges.len();

    println!("Graph: {} nodes, {} edges", n, m);

    if n == 0 {
        println!("Empty graph");
        return;
    }

    let mut graph = Graph::with_capacity(n, m);
    graph.add_nodes_fast(n);
    for (u, v) in &edges {
        graph.add_edge(NodeId(*u), NodeId(*v));
    }

    let mut component = vec![u32::MAX; n];
    let mut num_components = 0u32;

    for start in 0..n {
        if component[start] != u32::MAX {
            continue;
        }

        let mut queue = std::collections::VecDeque::new();
        queue.push_back(start);
        component[start] = num_components;

        while let Some(u) = queue.pop_front() {
            for (neighbor, _edge) in graph.neighbors(NodeId(u as u32)) {
                let v = neighbor.0 as usize;
                if component[v] == u32::MAX {
                    component[v] = num_components;
                    queue.push_back(v);
                }
            }
        }

        num_components += 1;
    }

    println!("Connected components: {}", num_components);

    let mut components: Vec<Vec<u32>> = vec![Vec::new(); num_components as usize];
    for (i, &c) in component.iter().enumerate() {
        components[c as usize].push(i as u32);
    }

    components.sort_by_key(|c| Reverse(c.len()));

    if let Some(comp) = components.first() {
        if comp.len() < 2 {
            println!(
                "Largest component has {} node(s), SPQR tree requires at least 2",
                comp.len()
            );
            return;
        }

        println!("Largest component: {} nodes", comp.len());

        let mut node_map: HashMap<u32, u32> = HashMap::new();
        for (new_id, &old_id) in comp.iter().enumerate() {
            node_map.insert(old_id, new_id as u32);
        }

        let mut sub_edges: Vec<(u32, u32)> = Vec::new();
        for (u, v) in &edges {
            if let (Some(&new_u), Some(&new_v)) = (node_map.get(u), node_map.get(v)) {
                sub_edges.push((new_u, new_v));
            }
        }

        let sub_n = comp.len();
        let mut subgraph = Graph::with_capacity(sub_n, sub_edges.len());
        subgraph.add_nodes_fast(sub_n);
        for (u, v) in &sub_edges {
            subgraph.add_edge(NodeId(*u), NodeId(*v));
        }

        // Build SPQR tree
        let result = build_spqr(&subgraph);
        let spqr = &result.tree;

        println!("SPQR tree nodes: {}", spqr.len());

        let (s_count, p_count, r_count) = spqr.count_by_type();
        println!(
            "  S-nodes: {}, P-nodes: {}, R-nodes: {}",
            s_count, p_count, r_count
        );

        if spqr.len() <= 20 {
            println!("\nTree structure:");
            let output = spqr_rust::spqr_format::to_spqr_string(&subgraph, &result);
            println!("{}", output);
        }
    }
}
