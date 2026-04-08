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
    let prefix = match tree.node(tid).node_type {
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
    for (_, nd) in tree.iter() {
        for &orig in &nd.skeleton.node_to_original {
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

    for (tid, nd) in tree.iter() {
        let type_char = match nd.node_type {
            SpqrNodeType::S => "S",
            SpqrNodeType::P => "P",
            SpqrNodeType::R => "R",
        };
        let name = tree_node_name(tree, tid);
        write!(w, "{} {} {}", type_char, name, blk)?;
        let mut orig_nodes = BTreeSet::new();
        for &orig in &nd.skeleton.node_to_original {
            orig_nodes.insert(orig.0);
        }
        for v in &orig_nodes {
            write!(w, " {}", node_name(NodeId(*v)))?;
        }
        writeln!(w)?;
    }
    writeln!(w)?;

    let mut v_count: usize = 0;
    for (tid, nd) in tree.iter() {
        for se in &nd.skeleton.edges {
            if !se.twin_tree_node.is_valid() {
                continue;
            }
            if tid.0 >= se.twin_tree_node.0 {
                continue;
            }

            let name = virtual_edge_name(v_count);
            v_count += 1;
            let node_a = nd.skeleton.node_to_original[se.src.idx()];
            let node_b = nd.skeleton.node_to_original[se.dst.idx()];
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
}
