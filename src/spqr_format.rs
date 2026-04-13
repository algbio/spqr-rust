use crate::{EdgeId, Graph, NodeId, SpqrNodeType, SpqrResult, SpqrTree, TreeNodeId};
use std::collections::BTreeSet;
use std::fmt;
use std::io::{self, Write};

const FORMAT_VERSION: &str = "v0.4";
const FORMAT_URL: &str = "https://github.com/sebschmi/SPQR-tree-file-format";

fn node_name(id: NodeId) -> String {
    format!("N{}", id.0)
}
fn edge_name(id: EdgeId) -> String {
    format!("E{}", id.0)
}
fn component_name(id: usize) -> String {
    format!("G{}", id)
}
fn block_name(id: usize) -> String {
    format!("B{}", id)
}
fn virtual_edge_name(id: usize) -> String {
    format!("V{}", id)
}

fn tree_node_name(tree: &SpqrTree, tid: TreeNodeId) -> String {
    let prefix = match tree.node_type(tid) {
        SpqrNodeType::S => "S",
        SpqrNodeType::P => "P",
        SpqrNodeType::R => "R",
    };
    format!("{}{}", prefix, tid.0)
}

pub fn write_spqr_format<W: Write>(
    w: &mut W,
    graph: &Graph,
    result: &SpqrResult,
) -> io::Result<()> {
    write_spqr_format_inner(w, graph, &result.tree, &result.self_loops)
}

pub fn write_spqr_tree_format<W: Write>(
    w: &mut W,
    graph: &Graph,
    tree: &SpqrTree,
) -> io::Result<()> {
    write_spqr_format_inner(w, graph, tree, &[])
}

pub fn to_spqr_string(graph: &Graph, result: &SpqrResult) -> String {
    let mut buf = Vec::new();
    write_spqr_format(&mut buf, graph, result).expect("write to Vec<u8> should not fail");
    String::from_utf8(buf).expect("output is valid UTF-8")
}

pub fn tree_to_spqr_string(graph: &Graph, tree: &SpqrTree) -> String {
    let mut buf = Vec::new();
    write_spqr_tree_format(&mut buf, graph, tree).expect("write to Vec<u8> should not fail");
    String::from_utf8(buf).expect("output is valid UTF-8")
}

fn write_spqr_format_inner<W: Write>(
    w: &mut W,
    graph: &Graph,
    tree: &SpqrTree,
    self_loops: &[EdgeId],
) -> io::Result<()> {
    let n = graph.num_nodes();
    let m = graph.num_edges();

    writeln!(w, "H {} {}", FORMAT_VERSION, FORMAT_URL)?;
    writeln!(w)?;

    let comp = component_name(0);
    write!(w, "G {}", comp)?;
    for v in 0..n {
        write!(w, " {}", node_name(NodeId(v as u32)))?;
    }
    writeln!(w)?;
    writeln!(w)?;

    if !self_loops.is_empty() {
        for &eid in self_loops {
            let e = graph.edge(eid);
            writeln!(
                w,
                "E {} {} {} {}",
                edge_name(eid),
                comp,
                node_name(e.src),
                node_name(e.dst)
            )?;
        }
        writeln!(w)?;
    }

    if tree.is_empty() {
        return Ok(());
    }

    let mut block_nodes = BTreeSet::new();
    for tid in tree.iter() {
        for &orig in tree.node_mapping_slice(tid) {
            block_nodes.insert(orig.0);
        }
    }

    let block_has_spqr = block_nodes.len() >= 3;

    let blk = block_name(0);
    write!(w, "B {} {}", blk, comp)?;
    for &v in &block_nodes {
        write!(w, " {}", node_name(NodeId(v)))?;
    }
    writeln!(w)?;
    writeln!(w)?;

    if !block_has_spqr {
        for i in 0..m {
            let eid = EdgeId(i as u32);
            if tree.tree_node_of_edge(eid).is_valid() {
                let e = graph.edge(eid);
                writeln!(
                    w,
                    "E {} {} {} {}",
                    edge_name(eid),
                    blk,
                    node_name(e.src),
                    node_name(e.dst)
                )?;
            }
        }
        return Ok(());
    }

    for tid in tree.iter() {
        let type_char = match tree.node_type(tid) {
            SpqrNodeType::S => "S",
            SpqrNodeType::P => "P",
            SpqrNodeType::R => "R",
        };
        let name = tree_node_name(tree, tid);
        write!(w, "{} {} {}", type_char, name, blk)?;
        let mut orig_nodes = BTreeSet::new();
        for &orig in tree.node_mapping_slice(tid) {
            orig_nodes.insert(orig.0);
        }
        for v in &orig_nodes {
            write!(w, " {}", node_name(NodeId(*v)))?;
        }
        writeln!(w)?;
    }
    writeln!(w)?;

    let mut v_count: usize = 0;
    for tid in tree.iter() {
        let node_to_original = tree.node_mapping_slice(tid);
        for se in tree.skeleton_edges_slice(tid) {
            if !se.twin_tree_node.is_valid() {
                continue;
            }
            if tid.0 >= se.twin_tree_node.0 {
                continue;
            }

            let name = virtual_edge_name(v_count);
            v_count += 1;
            let node_a = node_to_original[se.src.idx()];
            let node_b = node_to_original[se.dst.idx()];
            writeln!(
                w,
                "V {} {} {} {} {}",
                name,
                tree_node_name(tree, tid),
                tree_node_name(tree, se.twin_tree_node),
                node_name(node_a),
                node_name(node_b)
            )?;
        }
    }
    writeln!(w)?;

    for i in 0..m {
        let eid = EdgeId(i as u32);
        let tid = tree.tree_node_of_edge(eid);
        if !tid.is_valid() {
            continue;
        }
        let e = graph.edge(eid);
        writeln!(
            w,
            "E {} {} {} {}",
            edge_name(eid),
            tree_node_name(tree, tid),
            node_name(e.src),
            node_name(e.dst)
        )?;
    }

    Ok(())
}

pub struct SpqrFormatDisplay<'a> {
    pub graph: &'a Graph,
    pub result: &'a SpqrResult,
}

impl<'a> fmt::Display for SpqrFormatDisplay<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&to_spqr_string(self.graph, self.result))
    }
}

/// Parsed representation of a .spqr file for validation
#[derive(Debug, Default)]
pub struct ParsedSpqrFormat {
    pub version: String,
    pub graph_nodes: Vec<u32>,
    pub blocks: Vec<ParsedBlock>,
    pub spqr_nodes: Vec<ParsedSpqrNode>,
    pub virtual_edges: Vec<ParsedVirtualEdge>,
    pub real_edges: Vec<ParsedRealEdge>,
    pub self_loop_edges: Vec<ParsedRealEdge>,
}

#[derive(Debug)]
pub struct ParsedBlock {
    pub name: String,
    pub component: String,
    pub nodes: Vec<u32>,
}

#[derive(Debug)]
pub struct ParsedSpqrNode {
    pub name: String,
    pub node_type: char,
    pub block: String,
    pub nodes: Vec<u32>,
}

#[derive(Debug)]
pub struct ParsedVirtualEdge {
    pub name: String,
    pub node1: String,
    pub node2: String,
    pub pole1: u32,
    pub pole2: u32,
}

#[derive(Debug)]
pub struct ParsedRealEdge {
    pub name: String,
    pub edge_id: u32,
    pub container: String,
    pub src: u32,
    pub dst: u32,
}

fn parse_node_id(s: &str) -> Option<u32> {
    s.strip_prefix('N').and_then(|n| n.parse().ok())
}

fn parse_edge_id(s: &str) -> Option<u32> {
    s.strip_prefix('E').and_then(|n| n.parse().ok())
}

/// Parse a .spqr format string and return structured data
pub fn parse_spqr_format(input: &str) -> Result<ParsedSpqrFormat, String> {
    let mut result = ParsedSpqrFormat::default();

    for line in input.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }

        match parts[0] {
            "H" if parts.len() >= 2 => {
                result.version = parts[1].to_string();
            }
            "G" if parts.len() >= 2 => {
                for &node_str in &parts[2..] {
                    if let Some(id) = parse_node_id(node_str) {
                        result.graph_nodes.push(id);
                    }
                }
            }
            "B" if parts.len() >= 3 => {
                let nodes: Vec<u32> = parts[3..].iter().filter_map(|s| parse_node_id(s)).collect();
                result.blocks.push(ParsedBlock {
                    name: parts[1].to_string(),
                    component: parts[2].to_string(),
                    nodes,
                });
            }
            "S" | "P" | "R" if parts.len() >= 3 => {
                let nodes: Vec<u32> = parts[3..].iter().filter_map(|s| parse_node_id(s)).collect();
                result.spqr_nodes.push(ParsedSpqrNode {
                    name: parts[1].to_string(),
                    node_type: parts[0].chars().next().unwrap(),
                    block: parts[2].to_string(),
                    nodes,
                });
            }
            "V" if parts.len() >= 6 => {
                let pole1 = parse_node_id(parts[4]).ok_or("Invalid pole1")?;
                let pole2 = parse_node_id(parts[5]).ok_or("Invalid pole2")?;
                result.virtual_edges.push(ParsedVirtualEdge {
                    name: parts[1].to_string(),
                    node1: parts[2].to_string(),
                    node2: parts[3].to_string(),
                    pole1,
                    pole2,
                });
            }
            "E" if parts.len() >= 5 => {
                let edge_id = parse_edge_id(parts[1]).ok_or("Invalid edge id")?;
                let src = parse_node_id(parts[3]).ok_or("Invalid src")?;
                let dst = parse_node_id(parts[4]).ok_or("Invalid dst")?;
                let edge = ParsedRealEdge {
                    name: parts[1].to_string(),
                    edge_id,
                    container: parts[2].to_string(),
                    src,
                    dst,
                };
                // Self-loops are assigned to component (G0), not block or SPQR node
                if edge.container.starts_with('G') && src == dst {
                    result.self_loop_edges.push(edge);
                } else {
                    result.real_edges.push(edge);
                }
            }
            _ => {}
        }
    }

    Ok(result)
}

/// Validate parsed .spqr format against the original graph
pub fn validate_spqr_format(
    parsed: &ParsedSpqrFormat,
    graph: &Graph,
    result: &SpqrResult,
) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();

    // Check version
    if parsed.version != FORMAT_VERSION {
        errors.push(format!(
            "Version mismatch: expected {}, got {}",
            FORMAT_VERSION, parsed.version
        ));
    }

    // Check graph nodes
    if parsed.graph_nodes.len() != graph.num_nodes() {
        errors.push(format!(
            "Graph node count mismatch: expected {}, got {}",
            graph.num_nodes(),
            parsed.graph_nodes.len()
        ));
    }

    // Check all real edges are present
    let non_self_loop_count = graph.num_edges() - result.self_loops.len();
    if parsed.real_edges.len() != non_self_loop_count {
        errors.push(format!(
            "Real edge count mismatch: expected {}, got {}",
            non_self_loop_count,
            parsed.real_edges.len()
        ));
    }

    // Check self-loop count
    if parsed.self_loop_edges.len() != result.self_loops.len() {
        errors.push(format!(
            "Self-loop count mismatch: expected {}, got {}",
            result.self_loops.len(),
            parsed.self_loop_edges.len()
        ));
    }

    // Verify each real edge matches the graph
    for edge in &parsed.real_edges {
        if edge.edge_id as usize >= graph.num_edges() {
            errors.push(format!("Edge id {} out of bounds", edge.edge_id));
            continue;
        }
        let orig = graph.edge(EdgeId(edge.edge_id));
        let (orig_src, orig_dst) = (orig.src.0, orig.dst.0);
        // Edges can be in either direction
        if !((edge.src == orig_src && edge.dst == orig_dst)
            || (edge.src == orig_dst && edge.dst == orig_src))
        {
            errors.push(format!(
                "Edge E{} endpoints mismatch: expected ({}, {}), got ({}, {})",
                edge.edge_id, orig_src, orig_dst, edge.src, edge.dst
            ));
        }
    }

    // Verify SPQR node count matches tree
    if !result.tree.is_empty() {
        // Only check if tree has nodes and block has >= 3 nodes
        let block_nodes: std::collections::BTreeSet<u32> = result
            .tree
            .iter()
            .flat_map(|tid| result.tree.node_mapping_slice(tid).iter().map(|n| n.0))
            .collect();

        if block_nodes.len() >= 3 {
            if parsed.spqr_nodes.len() != result.tree.len() {
                errors.push(format!(
                    "SPQR node count mismatch: expected {}, got {}",
                    result.tree.len(),
                    parsed.spqr_nodes.len()
                ));
            }

            // Verify SPQR node types
            for (i, tid) in result.tree.iter().enumerate() {
                let expected_type = match result.tree.node_type(tid) {
                    SpqrNodeType::S => 'S',
                    SpqrNodeType::P => 'P',
                    SpqrNodeType::R => 'R',
                };
                if let Some(parsed_node) = parsed.spqr_nodes.get(i) {
                    if parsed_node.node_type != expected_type {
                        errors.push(format!(
                            "SPQR node {} type mismatch: expected {}, got {}",
                            parsed_node.name, expected_type, parsed_node.node_type
                        ));
                    }
                }
            }

            // Count virtual edges in tree
            let mut expected_virtual = 0;
            for tid in result.tree.iter() {
                for se in result.tree.skeleton_edges_slice(tid) {
                    if se.twin_tree_node.is_valid() && tid.0 < se.twin_tree_node.0 {
                        expected_virtual += 1;
                    }
                }
            }
            if parsed.virtual_edges.len() != expected_virtual {
                errors.push(format!(
                    "Virtual edge count mismatch: expected {}, got {}",
                    expected_virtual,
                    parsed.virtual_edges.len()
                ));
            }
        }
    }

    // Verify virtual edges reference valid SPQR nodes
    let spqr_node_names: std::collections::HashSet<&str> =
        parsed.spqr_nodes.iter().map(|n| n.name.as_str()).collect();
    for ve in &parsed.virtual_edges {
        if !spqr_node_names.contains(ve.node1.as_str()) {
            errors.push(format!(
                "Virtual edge {} references unknown SPQR node {}",
                ve.name, ve.node1
            ));
        }
        if !spqr_node_names.contains(ve.node2.as_str()) {
            errors.push(format!(
                "Virtual edge {} references unknown SPQR node {}",
                ve.name, ve.node2
            ));
        }
        // Verify poles are valid graph nodes
        if ve.pole1 as usize >= graph.num_nodes() {
            errors.push(format!(
                "Virtual edge {} pole1 {} out of bounds",
                ve.name, ve.pole1
            ));
        }
        if ve.pole2 as usize >= graph.num_nodes() {
            errors.push(format!(
                "Virtual edge {} pole2 {} out of bounds",
                ve.name, ve.pole2
            ));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{build_spqr, build_spqr_tree, Graph, NodeId};

    fn make_k4() -> Graph {
        let mut g = Graph::with_capacity(4, 6);
        g.add_nodes(4);
        g.add_edge(NodeId(0), NodeId(1));
        g.add_edge(NodeId(0), NodeId(2));
        g.add_edge(NodeId(0), NodeId(3));
        g.add_edge(NodeId(1), NodeId(2));
        g.add_edge(NodeId(1), NodeId(3));
        g.add_edge(NodeId(2), NodeId(3));
        g
    }

    fn make_cycle(n: usize) -> Graph {
        let mut g = Graph::with_capacity(n, n);
        g.add_nodes(n);
        for i in 0..n {
            g.add_edge(NodeId(i as u32), NodeId(((i + 1) % n) as u32));
        }
        g
    }

    fn make_bond() -> Graph {
        let mut g = Graph::with_capacity(2, 3);
        g.add_nodes(2);
        g.add_edge(NodeId(0), NodeId(1));
        g.add_edge(NodeId(0), NodeId(1));
        g.add_edge(NodeId(0), NodeId(1));
        g
    }

    #[test]
    fn test_k4_format() {
        let g = make_k4();
        let res = build_spqr(&g);
        let s = to_spqr_string(&g, &res);
        println!(" K4 \n{}", s);
        assert!(s.starts_with("H v0.4"));
        assert!(s.contains("G G0 N0 N1 N2 N3"));
        assert!(s.contains("B B0 G0"));
        assert!(s.contains("R R0 B0"));
        for i in 0..6 {
            assert!(s.contains(&format!("E E{}", i)), "missing E{}", i);
        }
    }

    #[test]
    fn test_cycle_format() {
        let g = make_cycle(5);
        let res = build_spqr(&g);
        let s = to_spqr_string(&g, &res);
        println!(" Cycle 5 \n{}", s);
        assert!(s.starts_with("H v0.4"));
        assert!(s.contains("S S0 B0"));
    }

    #[test]
    fn test_bond_format() {
        let g = make_bond();
        let res = build_spqr(&g);
        let s = to_spqr_string(&g, &res);
        println!(" Bond \n{}", s);
        assert!(s.starts_with("H v0.4"));
        assert!(s.contains("B B0 G0 N0 N1"));
        assert!(s.contains("E E0 B0 N0 N1"));
    }

    #[test]
    fn test_self_loops_format() {
        let mut g = Graph::with_capacity(3, 5);
        g.add_nodes(3);
        g.add_edge(NodeId(0), NodeId(1));
        g.add_edge(NodeId(1), NodeId(2));
        g.add_edge(NodeId(2), NodeId(0));
        g.add_edge(NodeId(0), NodeId(0));
        g.add_edge(NodeId(1), NodeId(1));
        let res = build_spqr(&g);
        let s = to_spqr_string(&g, &res);
        println!(" Self loops \n{}", s);
        assert!(s.contains("E E3 G0 N0 N0"));
        assert!(s.contains("E E4 G0 N1 N1"));
    }

    #[test]
    fn test_only_self_loops_format() {
        let mut g = Graph::with_capacity(1, 2);
        g.add_nodes(1);
        g.add_edge(NodeId(0), NodeId(0));
        g.add_edge(NodeId(0), NodeId(0));
        let res = build_spqr(&g);
        let s = to_spqr_string(&g, &res);
        println!(" Only self loops \n{}", s);
        assert!(s.contains("E E0 G0 N0 N0"));
        assert!(s.contains("E E1 G0 N0 N0"));
        assert!(!s.contains("B "));
    }

    #[test]
    fn test_single_edge_format() {
        let mut g = Graph::with_capacity(2, 1);
        g.add_nodes(2);
        g.add_edge(NodeId(0), NodeId(1));
        let res = build_spqr(&g);
        let s = to_spqr_string(&g, &res);
        println!(" Single edge \n{}", s);
        assert!(s.contains("B B0 G0 N0 N1"));
        assert!(s.contains("E E0 B0 N0 N1"));
    }

    #[test]
    fn test_tree_only_format() {
        let g = make_k4();
        let tree = build_spqr_tree(&g);
        let s = tree_to_spqr_string(&g, &tree);
        println!("K4 tree only n{}", s);
        assert!(s.starts_with("H v0.4"));
        assert!(!s.contains("Self-loop"));
    }

    #[test]
    fn test_two_triangles_format() {
        let mut g = Graph::with_capacity(4, 5);
        g.add_nodes(4);
        g.add_edge(NodeId(0), NodeId(1));
        g.add_edge(NodeId(1), NodeId(2));
        g.add_edge(NodeId(2), NodeId(0));
        g.add_edge(NodeId(0), NodeId(3));
        g.add_edge(NodeId(3), NodeId(1));
        let res = build_spqr(&g);
        let s = to_spqr_string(&g, &res);
        println!(" Two triangles \n{}", s);
        assert!(s.contains("B B0 G0"));
        assert!(s.contains("V V"));
        for i in 0..5 {
            assert!(s.contains(&format!("E E{} ", i)), "missing E{}", i);
        }
    }

    // validation tests

    fn validate_roundtrip(g: &Graph, label: &str) {
        let res = build_spqr(g);
        let s = to_spqr_string(g, &res);
        let parsed = parse_spqr_format(&s).expect("Failed to parse .spqr format");
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
    fn test_roundtrip_k4() {
        let g = make_k4();
        validate_roundtrip(&g, "K4");
    }

    #[test]
    fn test_roundtrip_cycle() {
        for n in 3..=10 {
            let g = make_cycle(n);
            validate_roundtrip(&g, &format!("Cycle{}", n));
        }
    }

    #[test]
    fn test_roundtrip_bond() {
        let g = make_bond();
        validate_roundtrip(&g, "Bond");
    }

    #[test]
    fn test_roundtrip_self_loops() {
        let mut g = Graph::with_capacity(3, 5);
        g.add_nodes(3);
        g.add_edge(NodeId(0), NodeId(1));
        g.add_edge(NodeId(1), NodeId(2));
        g.add_edge(NodeId(2), NodeId(0));
        g.add_edge(NodeId(0), NodeId(0)); // self-loop
        g.add_edge(NodeId(1), NodeId(1)); // self-loop
        validate_roundtrip(&g, "SelfLoops");
    }

    #[test]
    fn test_roundtrip_complete_graphs() {
        for n in 3..=8 {
            let m = n * (n - 1) / 2;
            let mut g = Graph::with_capacity(n, m);
            g.add_nodes(n);
            for u in 0..n {
                for v in (u + 1)..n {
                    g.add_edge(NodeId(u as u32), NodeId(v as u32));
                }
            }
            validate_roundtrip(&g, &format!("K{}", n));
        }
    }

    #[test]
    fn test_roundtrip_two_triangles() {
        let mut g = Graph::with_capacity(4, 5);
        g.add_nodes(4);
        g.add_edge(NodeId(0), NodeId(1));
        g.add_edge(NodeId(1), NodeId(2));
        g.add_edge(NodeId(2), NodeId(0));
        g.add_edge(NodeId(0), NodeId(3));
        g.add_edge(NodeId(3), NodeId(1));
        validate_roundtrip(&g, "TwoTriangles");
    }

    #[test]
    fn test_roundtrip_single_edge() {
        let mut g = Graph::with_capacity(2, 1);
        g.add_nodes(2);
        g.add_edge(NodeId(0), NodeId(1));
        validate_roundtrip(&g, "SingleEdge");
    }

    #[test]
    fn test_roundtrip_parallel_edges() {
        let mut g = Graph::with_capacity(3, 6);
        g.add_nodes(3);
        // Triangle with double edges
        g.add_edge(NodeId(0), NodeId(1));
        g.add_edge(NodeId(0), NodeId(1));
        g.add_edge(NodeId(1), NodeId(2));
        g.add_edge(NodeId(1), NodeId(2));
        g.add_edge(NodeId(2), NodeId(0));
        g.add_edge(NodeId(2), NodeId(0));
        validate_roundtrip(&g, "ParallelEdges");
    }

    #[test]
    fn test_parse_spqr_format_basic() {
        let input = r#"H v0.4 https://github.com/sebschmi/SPQR-tree-file-format

G G0 N0 N1 N2 N3

B B0 G0 N0 N1 N2 N3

R R0 B0 N0 N1 N2 N3

E E0 R0 N0 N1
E E1 R0 N0 N2
E E2 R0 N0 N3
E E3 R0 N1 N2
E E4 R0 N1 N3
E E5 R0 N2 N3
"#;
        let parsed = parse_spqr_format(input).expect("Failed to parse");
        assert_eq!(parsed.version, "v0.4");
        assert_eq!(parsed.graph_nodes.len(), 4);
        assert_eq!(parsed.blocks.len(), 1);
        assert_eq!(parsed.spqr_nodes.len(), 1);
        assert_eq!(parsed.spqr_nodes[0].node_type, 'R');
        assert_eq!(parsed.real_edges.len(), 6);
    }

    #[test]
    fn test_parse_spqr_format_with_virtual() {
        let input = r#"H v0.4 https://github.com/sebschmi/SPQR-tree-file-format

G G0 N0 N1 N2 N3

B B0 G0 N0 N1 N2 N3

S S0 B0 N0 N1 N2
S S1 B0 N0 N1 N3

V V0 S0 S1 N0 N1

E E0 S0 N0 N1
E E1 S0 N1 N2
E E2 S0 N2 N0
E E3 S1 N0 N3
E E4 S1 N3 N1
"#;
        let parsed = parse_spqr_format(input).expect("Failed to parse");
        assert_eq!(parsed.spqr_nodes.len(), 2);
        assert_eq!(parsed.virtual_edges.len(), 1);
        assert_eq!(parsed.virtual_edges[0].node1, "S0");
        assert_eq!(parsed.virtual_edges[0].node2, "S1");
        assert_eq!(parsed.virtual_edges[0].pole1, 0);
        assert_eq!(parsed.virtual_edges[0].pole2, 1);
    }
}
