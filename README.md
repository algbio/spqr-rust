# SPQR-tree-building

Rust library for computing SPQR trees of biconnected multigraphs via the Hopcroft-Tarjan triconnected components algorithm with corrections by Gutwenger & Mutzel (2001).

## Building an SPQR tree

Two entry points are provided:

- `build_spqr(graph) -> SpqrResult` handles self-loops (strips them before decomposition, returns them separately in `SpqrResult::self_loops`)
- `build_spqr_tree(graph) -> SpqrTree` for graphs known to have no self-loops (panics in debug mode otherwise)

Both expect a biconnected multigraph as input. The resulting `SpqrTree` contains `SpqrTreeNode`s of type S (polygon), P (bond), or R (triconnected), each carrying a `Skeleton` with real and virtual edges.

```rust
use spqr_tree_building::{build_spqr, Graph, NodeId};

let mut g = Graph::with_capacity(4, 6);
g.add_nodes(4);
g.add_edge(NodeId(0), NodeId(1));
g.add_edge(NodeId(0), NodeId(2));
g.add_edge(NodeId(0), NodeId(3));
g.add_edge(NodeId(1), NodeId(2));
g.add_edge(NodeId(1), NodeId(3));
g.add_edge(NodeId(2), NodeId(3));

let result = build_spqr(&g);
println!("{}", result.tree);
```

## Normalization

The tree returned by `build_spqr` / `build_spqr_tree` may contain adjacent nodes of the same type (S-S or P-P pairs). To obtain the canonical reduced SPQR tree:

```rust
let mut tree = build_spqr_tree(&g);
tree.normalize();
tree.compact(); 
```

`normalize` performs the logical merging of skeletons. `compact` is a subsequent cleanup pass that removes the emptied nodes from the internal `Vec` and reassigns all `TreeNodeId` references. They should always be called together.

## Verification

The `verify` module checks SPQR tree invariants (edge partition, skeleton correctness, S/P/R structural constraints, virtual edge pairing, tree connectivity, and optionally the reduced property):

```rust
use spqr_tree_building::verify::{verify_spqr_tree, verify_spqr_tree_with_options, VerifyOptions};

let report = verify_spqr_tree(&g, &tree); 
assert!(report.is_ok());

let report = verify_spqr_tree_with_options(&g, &tree, VerifyOptions { require_reduced: false });
```

## Output in `.spqr` format

The `spqr_format` module compute the decomposition to the [SPQR tree file format](https://github.com/sebschmi/SPQR-tree-file-format) (v0.4):

```rust
use spqr_tree_building::spqr_format::{to_spqr_string, write_spqr_format};

// To a String
let s = to_spqr_string(&g, &result);

// To a file
let mut f = std::fs::File::create("output.spqr").unwrap();
write_spqr_format(&mut f, &g, &result).unwrap();
```

## Testing

```bash
cargo test                                    
cargo test --release -- --ignored  # brute-force (10k random graphs)
SPQR_NUM_RANDOM=50000 cargo test --release -- --ignored
```

## References

- J. Hopcroft, R. Tarjan. *Dividing a Graph into Triconnected Components.* SIAM J. Comput., 2(3), 1973.
- C. Gutwenger, P. Mutzel. *A Linear Time Implementation of SPQR-Trees.* GD 2000, LNCS 1984, pp. 77-90, 2001.

## License

MIT