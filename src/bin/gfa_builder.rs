use spqr_rust::sp_compress::{compress_and_build_spqr_borrowed, InputEdge};
use spqr_rust::{build_spqr, BCTree, EdgeId, Graph, NodeId};
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::env;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Error, ErrorKind};
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Compress,
    Raw,
    Both,
}

fn usage() -> ! {
    eprintln!(
        "usage: gfa_builder [--mode compress|raw|both] <graph.gfa>\n\
         aliases: --compress, --raw, --both"
    );
    std::process::exit(2);
}

fn parse_args() -> (Mode, String) {
    let mut mode = Mode::Compress;
    let mut path = None;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => usage(),
            "--compress" => mode = Mode::Compress,
            "--raw" | "--no-compress" | "--nocompress" => mode = Mode::Raw,
            "--both" => mode = Mode::Both,
            "--mode" => {
                let Some(value) = args.next() else {
                    usage();
                };
                mode = match value.as_str() {
                    "compress" | "compressed" | "sp-compress" => Mode::Compress,
                    "raw" | "off" | "no-compress" | "nocompress" => Mode::Raw,
                    "both" => Mode::Both,
                    _ => usage(),
                };
            }
            _ if arg.starts_with('-') => usage(),
            _ => {
                if path.replace(arg).is_some() {
                    usage();
                }
            }
        }
    }
    let Some(path) = path else {
        usage();
    };
    (mode, path)
}

fn rss_kb() -> u64 {
    let Ok(s) = std::fs::read_to_string("/proc/self/status") else {
        return 0;
    };
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            return rest
                .split_whitespace()
                .next()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
        }
    }
    0
}

fn invalid_data(msg: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidData, msg.into())
}

fn parse_u64(bytes: &[u8]) -> io::Result<u64> {
    let mut v = 0u64;
    if bytes.is_empty() {
        return Err(invalid_data("empty numeric id"));
    }
    for &b in bytes {
        if !b.is_ascii_digit() {
            return Err(invalid_data(format!(
                "non-numeric id '{}'",
                String::from_utf8_lossy(bytes)
            )));
        }
        v = v
            .checked_mul(10)
            .and_then(|v| v.checked_add((b - b'0') as u64))
            .ok_or_else(|| {
                invalid_data(format!(
                    "numeric id exceeds u64: '{}'",
                    String::from_utf8_lossy(bytes)
                ))
            })?;
    }
    Ok(v)
}

fn field(line: &[u8], wanted: usize) -> Option<&[u8]> {
    let mut start = 0usize;
    let mut idx = 0usize;
    for i in 0..=line.len() {
        if i == line.len() || line[i] == b'\t' || line[i] == b'\n' || line[i] == b'\r' {
            if idx == wanted {
                return Some(&line[start..i]);
            }
            idx += 1;
            start = i + 1;
        }
    }
    None
}

#[derive(Default)]
struct DenseIds32 {
    map: HashMap<u64, u32>,
}

impl DenseIds32 {
    fn get_or_insert(&mut self, label: u64) -> io::Result<u32> {
        let next = self.map.len();
        match self.map.entry(label) {
            Entry::Occupied(entry) => Ok(*entry.get()),
            Entry::Vacant(entry) => {
                if next >= u32::MAX as usize {
                    return Err(invalid_data(
                        "too many linked nodes for the current u32 SPQR backend",
                    ));
                }
                let id = next as u32;
                entry.insert(id);
                Ok(id)
            }
        }
    }
    fn len(&self) -> u32 {
        self.map.len() as u32
    }
}

struct ParsedU32 {
    n_nodes: u32,
    segments: u64,
    links: u64,
    edges: Vec<(u32, u32)>,
}

struct ParsedWide {
    n_nodes: u64,
    segments: u64,
    links: u64,
    edges: Vec<spqr_rust::sp_compress::wide::InputEdge>,
}

enum ParsedGraph {
    U32(ParsedU32),
    Wide(ParsedWide),
}

fn switch_to_dense_u32(edges: &mut [(u32, u32)], dense: &mut DenseIds32) -> io::Result<()> {
    for (u, v) in edges {
        let u_label = u64::from(*u) + 1;
        let v_label = u64::from(*v) + 1;
        *u = dense.get_or_insert(u_label)?;
        *v = dense.get_or_insert(v_label)?;
    }
    Ok(())
}

fn convert_u32_edges_to_wide(
    edges: Vec<(u32, u32)>,
) -> Vec<spqr_rust::sp_compress::wide::InputEdge> {
    edges
        .into_iter()
        .enumerate()
        .map(|(i, (u, v))| spqr_rust::sp_compress::wide::InputEdge {
            u: spqr_rust::wide::NodeId(u as u64),
            v: spqr_rust::wide::NodeId(v as u64),
            original_edge_id: spqr_rust::wide::EdgeId(i as u64),
        })
        .collect()
}

fn parse_gfa(path: &str) -> io::Result<ParsedGraph> {
    let file = File::open(path)?;
    let mut reader = BufReader::with_capacity(1 << 20, file);
    let mut line = Vec::with_capacity(4096);
    let mut max_id = 0u64;
    let mut segments = 0u64;
    let mut links = 0u64;
    let mut edges32: Vec<(u32, u32)> = Vec::new();
    let mut edges64: Option<Vec<spqr_rust::sp_compress::wide::InputEdge>> = None;

    while reader.read_until(b'\n', &mut line)? != 0 {
        if line.first() == Some(&b'S') && line.get(1) == Some(&b'\t') {
            if let Some(raw) = field(&line, 1) {
                let id = parse_u64(raw)?;
                max_id = max_id.max(id);
                segments += 1;
            }
        } else if line.first() == Some(&b'L') && line.get(1) == Some(&b'\t') {
            if let (Some(raw_u), Some(raw_v)) = (field(&line, 1), field(&line, 3)) {
                let u = parse_u64(raw_u)?;
                let v = parse_u64(raw_v)?;
                if u > 0 && v > 0 {
                    max_id = max_id.max(u).max(v);
                    links += 1;
                    if edges64.is_none()
                        && (links > u32::MAX as u64
                            || max_id > u32::MAX as u64
                            || u > u32::MAX as u64
                            || v > u32::MAX as u64)
                    {
                        let old = std::mem::take(&mut edges32);
                        edges64 = Some(convert_u32_edges_to_wide(old));
                    }
                    if let Some(edges) = edges64.as_mut() {
                        edges.push(spqr_rust::sp_compress::wide::InputEdge {
                            u: spqr_rust::wide::NodeId(u - 1),
                            v: spqr_rust::wide::NodeId(v - 1),
                            original_edge_id: spqr_rust::wide::EdgeId(links - 1),
                        });
                    } else {
                        edges32.push(((u - 1) as u32, (v - 1) as u32));
                    }
                }
            }
        }
        line.clear();
    }

    if let Some(edges) = edges64 {
        return Ok(ParsedGraph::Wide(ParsedWide {
            n_nodes: max_id,
            segments,
            links,
            edges,
        }));
    }
    let n_nodes = if max_id > u32::MAX as u64 {
        let mut dense32 = DenseIds32::default();
        switch_to_dense_u32(&mut edges32, &mut dense32)?;
        dense32.len()
    } else {
        u32::try_from(max_id).map_err(|_| {
            invalid_data("GFA contains more than u32::MAX segment IDs but no linked nodes to remap")
        })?
    };
    Ok(ParsedGraph::U32(ParsedU32 {
        n_nodes,
        segments,
        links,
        edges: edges32,
    }))
}

fn remap_block_endpoint(node: NodeId, remap: &mut [u32], next: &mut u32) -> io::Result<u32> {
    let slot = &mut remap[node.idx()];
    if *slot == u32::MAX {
        if *next == u32::MAX {
            return Err(invalid_data(
                "biconnected block has too many nodes for the current u32 SPQR backend",
            ));
        }
        *slot = *next;
        *next += 1;
    }
    Ok(*slot)
}

fn run_u32(mode: Mode, parsed: ParsedU32) -> io::Result<()> {
    let mut graph = Graph::with_capacity(parsed.n_nodes as usize, parsed.edges.len());
    graph.add_nodes_fast(parsed.n_nodes as usize);
    for &(u, v) in &parsed.edges {
        graph.add_edge(NodeId(u), NodeId(v));
    }
    println!(
        "read_graph backend=u32 segments={} links={} nodes={} edges={} rss_kb={}",
        parsed.segments,
        parsed.links,
        graph.num_nodes(),
        graph.num_edges(),
        rss_kb()
    );

    let t = Instant::now();
    let bc = BCTree::build(&graph);
    let best = bc
        .iter_blocks()
        .max_by_key(|(_, b)| (b.edge_count, b.node_count))
        .map(|(i, _)| i);
    let Some(best) = best else {
        println!("bc seconds={:.3} blocks=0 cut_vertices=0 best_block=NA block_nodes=0 block_edges=0 rss_kb={}", t.elapsed().as_secs_f64(), rss_kb());
        return Ok(());
    };
    let block = bc.block(best);
    println!("bc seconds={:.3} blocks={} cut_vertices={} best_block={} block_nodes={} block_edges={} rss_kb={}",
        t.elapsed().as_secs_f64(), bc.num_blocks(), bc.num_cut_vertices(), best, block.node_count, block.edge_count, rss_kb());

    let t = Instant::now();
    let mut remap = vec![u32::MAX; graph.num_nodes()];
    let mut input_edges = Vec::with_capacity(block.edge_count as usize);
    let mut block_nodes = 0u32;
    for (i, eid) in bc.block_edges(best).iter().enumerate() {
        if i >= u32::MAX as usize {
            return Err(invalid_data(
                "biconnected block has too many edges for the current u32 SPQR backend",
            ));
        }
        let e = graph.edge(*eid);
        let u = remap_block_endpoint(e.src, &mut remap, &mut block_nodes)?;
        let v = remap_block_endpoint(e.dst, &mut remap, &mut block_nodes)?;
        input_edges.push(InputEdge {
            u: NodeId(u),
            v: NodeId(v),
            original_edge_id: EdgeId(i as u32),
        });
    }
    println!(
        "remap seconds={:.3} block_nodes={} block_edges={} rss_kb={}",
        t.elapsed().as_secs_f64(),
        block_nodes,
        block.edge_count,
        rss_kb()
    );

    if mode == Mode::Compress || mode == Mode::Both {
        let contractible = vec![1u8; block_nodes as usize];
        let t = Instant::now();
        let result = compress_and_build_spqr_borrowed(block_nodes, &input_edges, &contractible);
        let stats = result.stats();
        println!("builder_compress backend=u32 seconds={:.3} core_nodes={} core_edges={} macros={} series={} parallel={} full_sp={} rss_kb={}",
            t.elapsed().as_secs_f64(), stats.core_nodes, stats.core_edges_count, stats.macro_count, stats.macro_series, stats.macro_parallel, stats.fully_sp_reducible, rss_kb());
    }

    if mode == Mode::Raw || mode == Mode::Both {
        let t = Instant::now();
        let mut block_graph = Graph::with_capacity(block_nodes as usize, input_edges.len());
        block_graph.add_nodes_fast(block_nodes as usize);
        for e in &input_edges {
            block_graph.add_edge(e.u, e.v);
        }
        println!(
            "raw_graph seconds={:.3} nodes={} edges={} rss_kb={}",
            t.elapsed().as_secs_f64(),
            block_graph.num_nodes(),
            block_graph.num_edges(),
            rss_kb()
        );
        let t = Instant::now();
        let result = build_spqr(&block_graph);
        println!("builder_raw backend=u32 seconds={:.3} tree_nodes={} skeleton_edges={} self_loops={} rss_kb={}",
            t.elapsed().as_secs_f64(), result.tree.len(), result.tree.skeleton_edges.len(), result.self_loops.len(), rss_kb());
    }
    Ok(())
}

#[inline]
fn bit_get(bits: &[u64], idx: usize) -> bool {
    (bits[idx >> 6] & (1u64 << (idx & 63))) != 0
}

#[inline]
fn bit_set(bits: &mut [u64], idx: usize) {
    bits[idx >> 6] |= 1u64 << (idx & 63);
}

#[inline]
fn add_incident(inc0: &mut [u64], inc1: &mut [u64], node: usize, edge: u64) -> io::Result<()> {
    if inc0[node] == u64::MAX {
        inc0[node] = edge;
    } else if inc1[node] == u64::MAX {
        inc1[node] = edge;
    } else {
        return Err(invalid_data(
            "degree-2 incident map saw more than two incident edges",
        ));
    }
    Ok(())
}

#[inline]
fn other_incident(inc0: &[u64], inc1: &[u64], node: usize, edge: u64) -> Option<u64> {
    let a = inc0[node];
    let b = inc1[node];
    if a == edge && b != u64::MAX {
        Some(b)
    } else if b == edge && a != u64::MAX {
        Some(a)
    } else {
        None
    }
}

fn run_wide_degree2_stats(parsed: &ParsedWide) -> io::Result<()> {
    let n = usize::try_from(parsed.n_nodes)
        .map_err(|_| invalid_data("wide graph node count exceeds usize"))?;
    let t = Instant::now();
    let mut degree = vec![0u8; n];
    for e in &parsed.edges {
        let u = e.u.idx();
        let v = e.v.idx();
        if u >= n || v >= n {
            return Err(invalid_data("edge endpoint outside node range"));
        }
        degree[u] = degree[u].saturating_add(1).min(3);
        if u != v {
            degree[v] = degree[v].saturating_add(1).min(3);
        }
    }
    println!(
        "degree seconds={:.3} rss_kb={}",
        t.elapsed().as_secs_f64(),
        rss_kb()
    );

    let t = Instant::now();
    let mut inc0 = vec![u64::MAX; n];
    let mut inc1 = vec![u64::MAX; n];
    for (i, e) in parsed.edges.iter().enumerate() {
        let eid = i as u64;
        let u = e.u.idx();
        let v = e.v.idx();
        if u == v {
            continue;
        }
        if degree[u] == 2 {
            add_incident(&mut inc0, &mut inc1, u, eid)?;
        }
        if degree[v] == 2 {
            add_incident(&mut inc0, &mut inc1, v, eid)?;
        }
    }
    println!(
        "incidents seconds={:.3} rss_kb={}",
        t.elapsed().as_secs_f64(),
        rss_kb()
    );

    let t = Instant::now();
    let mut visited = vec![0u64; parsed.edges.len().div_ceil(64)];
    let mut core_node_bits = vec![0u64; n.div_ceil(64)];
    let mut visited_count = 0u64;
    let mut core_edges = 0u64;
    let mut self_loops = 0u64;
    let mut series_macros = 0u64;
    let mut series_reductions = 0u64;
    let mut degree2_cycle_components = 0u64;

    for start_eid in 0..parsed.edges.len() {
        if bit_get(&visited, start_eid) {
            continue;
        }
        let e = parsed.edges[start_eid];
        let u = e.u.idx();
        let v = e.v.idx();
        if u == v {
            bit_set(&mut visited, start_eid);
            visited_count += 1;
            self_loops += 1;
            core_edges += 1;
            bit_set(&mut core_node_bits, u);
            continue;
        }

        let du = degree[u];
        let dv = degree[v];
        if du == 2 && dv == 2 {
            let start_node = u;
            let mut curr = v;
            let mut curr_eid = start_eid as u64;
            let mut path_len = 0u64;
            let mut valid_cycle = true;
            loop {
                let ei = curr_eid as usize;
                if bit_get(&visited, ei) {
                    valid_cycle = false;
                    break;
                }
                bit_set(&mut visited, ei);
                visited_count += 1;
                path_len += 1;
                if curr == start_node {
                    break;
                }
                if degree[curr] != 2 {
                    valid_cycle = false;
                    break;
                }
                let Some(next_eid) = other_incident(&inc0, &inc1, curr, curr_eid) else {
                    valid_cycle = false;
                    break;
                };
                let ne = parsed.edges[next_eid as usize];
                let a = ne.u.idx();
                let b = ne.v.idx();
                curr = if a == curr {
                    b
                } else if b == curr {
                    a
                } else {
                    valid_cycle = false;
                    break;
                };
                curr_eid = next_eid;
            }
            if valid_cycle && path_len > 0 {
                degree2_cycle_components += 1;
                core_edges += 1;
                series_macros += 1;
                series_reductions = series_reductions.saturating_add(path_len.saturating_sub(1));
                bit_set(&mut core_node_bits, start_node);
            }
            continue;
        }

        let (start_core, mut curr, mut curr_eid) = if du != 2 {
            (u, v, start_eid as u64)
        } else {
            (v, u, start_eid as u64)
        };
        let mut path_len = 0u64;
        loop {
            let ei = curr_eid as usize;
            if bit_get(&visited, ei) {
                break;
            }
            bit_set(&mut visited, ei);
            visited_count += 1;
            path_len += 1;
            if degree[curr] != 2 {
                core_edges += 1;
                bit_set(&mut core_node_bits, start_core);
                bit_set(&mut core_node_bits, curr);
                if path_len > 1 {
                    series_macros += 1;
                    series_reductions = series_reductions.saturating_add(path_len - 1);
                }
                break;
            }
            let Some(next_eid) = other_incident(&inc0, &inc1, curr, curr_eid) else {
                core_edges += 1;
                bit_set(&mut core_node_bits, start_core);
                bit_set(&mut core_node_bits, curr);
                break;
            };
            let ne = parsed.edges[next_eid as usize];
            let a = ne.u.idx();
            let b = ne.v.idx();
            curr = if a == curr {
                b
            } else if b == curr {
                a
            } else {
                break;
            };
            curr_eid = next_eid;
        }
    }

    let core_nodes: u64 = core_node_bits.iter().map(|w| w.count_ones() as u64).sum();
    println!("builder_compress backend=u64_degree2_stats seconds={:.3} visited_edges={} core_nodes={} core_edges={} macros={} series={} parallel=NA self_loops={} degree2_cycles={} series_reductions={} full_sp={} rss_kb={}",
        t.elapsed().as_secs_f64(),
        visited_count,
        core_nodes,
        core_edges,
        series_macros,
        series_macros,
        self_loops,
        degree2_cycle_components,
        series_reductions,
        if core_edges == 1 { 1 } else { 0 },
        rss_kb());
    if visited_count != parsed.edges.len() as u64 {
        return Err(invalid_data(format!(
            "degree2 stats visited {} of {} edges",
            visited_count,
            parsed.edges.len()
        )));
    }
    Ok(())
}

fn run_wide(mode: Mode, parsed: ParsedWide) -> io::Result<()> {
    println!(
        "read_graph backend=u64 segments={} links={} nodes={} edges={} rss_kb={}",
        parsed.segments,
        parsed.links,
        parsed.n_nodes,
        parsed.edges.len(),
        rss_kb()
    );
    if mode == Mode::Raw || mode == Mode::Both {
        return Err(invalid_data(
            "raw wide SPQR for full GFA builder is not implemented; use --mode compress",
        ));
    }
    run_wide_degree2_stats(&parsed)
}

fn main() -> io::Result<()> {
    let (mode, path) = parse_args();
    let t = Instant::now();
    let parsed = parse_gfa(&path)?;
    println!(
        "parse seconds={:.3} rss_kb={}",
        t.elapsed().as_secs_f64(),
        rss_kb()
    );
    match parsed {
        ParsedGraph::U32(p) => run_u32(mode, p),
        ParsedGraph::Wide(p) => run_wide(mode, p),
    }
}
