/**
 * @file spqr_rust_wrapper.hpp
 * @brief C++ wrapper
 */

#ifndef SPQR_RUST_WRAPPER_HPP
#define SPQR_RUST_WRAPPER_HPP

#include "spqr_tree.h"
#include <vector>
#include <memory>
#include <stdexcept>
#include <string>
#include <functional>

namespace spqr_rust {

class RustGraph;
class RustSPQRTree;
class RustBCTree;
class RustConnectedComponents;

class RustGraph {
public:
    RustGraph(uint32_t nodeCapacity = 0, uint32_t edgeCapacity = 0)
        : ptr_(spqr_graph_new(nodeCapacity, edgeCapacity)) {
        if (!ptr_) throw std::bad_alloc();
    }

    static RustGraph fromRaw(Graph* ptr) {
        RustGraph g;
        spqr_graph_free(g.ptr_);
        g.ptr_ = ptr;
        return g;
    }

    static RustGraph fromEdges(uint32_t numNodes, const uint32_t* edges, uint32_t numEdges) {
        Graph* g = spqr_graph_from_edges(numNodes, edges, numEdges);
        if (!g) throw std::bad_alloc();
        return fromRaw(g);
    }

    static RustGraph fromArrays(uint32_t numNodes, const uint32_t* src, 
                                 const uint32_t* dst, uint32_t numEdges) {
        Graph* g = spqr_graph_from_arrays(numNodes, src, dst, numEdges);
        if (!g) throw std::bad_alloc();
        return fromRaw(g);
    }

    static RustGraph fromVectors(const std::vector<uint32_t>& src,
                                  const std::vector<uint32_t>& dst,
                                  uint32_t numNodes) {
        if (src.size() != dst.size()) throw std::invalid_argument("src and dst must have same size");
        return fromArrays(numNodes, src.data(), dst.data(), static_cast<uint32_t>(src.size()));
    }

    ~RustGraph() {
        if (ptr_) spqr_graph_free(ptr_);
    }

    RustGraph(RustGraph&& other) noexcept : ptr_(other.ptr_) { other.ptr_ = nullptr; }
    RustGraph& operator=(RustGraph&& other) noexcept {
        if (this != &other) {
            if (ptr_) spqr_graph_free(ptr_);
            ptr_ = other.ptr_;
            other.ptr_ = nullptr;
        }
        return *this;
    }
    RustGraph(const RustGraph&) = delete;
    RustGraph& operator=(const RustGraph&) = delete;

    uint32_t addNodes(uint32_t count) {
        return spqr_graph_add_nodes(ptr_, count);
    }

    uint32_t addEdge(uint32_t u, uint32_t v) {
        return spqr_graph_add_edge(ptr_, u, v);
    }

    void addEdgesBatch(const std::vector<std::pair<uint32_t, uint32_t>>& edges) {
        static_assert(sizeof(std::pair<uint32_t, uint32_t>) == 2 * sizeof(uint32_t),
                      "pair layout must be compact");
        spqr_graph_add_edges_batch(ptr_, 
            reinterpret_cast<const uint32_t*>(edges.data()), 
            static_cast<uint32_t>(edges.size()));
    }

    void addEdgesBatchFlat(const uint32_t* edges, uint32_t count) {
        spqr_graph_add_edges_batch(ptr_, edges, count);
    }

    void addEdgesBatchArrays(const uint32_t* src, const uint32_t* dst, uint32_t count) {
        for (uint32_t i = 0; i < count; ++i) {
            spqr_graph_add_edge(ptr_, src[i], dst[i]);
        }
    }

    uint32_t numNodes() const { return spqr_graph_num_nodes(ptr_); }
    uint32_t numEdges() const { return spqr_graph_num_edges(ptr_); }
    uint32_t edgeSrc(uint32_t edgeId) const { return spqr_graph_edge_src(ptr_, edgeId); }
    uint32_t edgeDst(uint32_t edgeId) const { return spqr_graph_edge_dst(ptr_, edgeId); }
    uint32_t degree(uint32_t node) const { return spqr_graph_degree(ptr_, node); }

    uint32_t adjCursor(uint32_t node) const {
        return spqr_graph_adj_cursor(ptr_, node);
    }

    bool adjNext(uint32_t cursor, uint32_t& neighbor, uint32_t& edge, uint32_t& nextCursor) const {
        return spqr_graph_adj_next(ptr_, cursor, &neighbor, &edge, &nextCursor);
    }

    template<typename F>
    void forEachNeighbor(uint32_t node, F&& callback) const {
        uint32_t cursor = spqr_graph_adj_cursor(ptr_, node);
        uint32_t neighbor, edge, next;
        while (spqr_graph_adj_next(ptr_, cursor, &neighbor, &edge, &next)) {
            callback(neighbor, edge);
            cursor = next;
        }
    }

    Graph* raw() { return ptr_; }
    const Graph* raw() const { return ptr_; }

private:
    Graph* ptr_;
};

/**
 * @brief SPQR node type enum
 */
enum class SPQRNodeType : uint8_t {
    S = SPQR_NODE_TYPE_S,// S
    P = SPQR_NODE_TYPE_P,// P
    R = SPQR_NODE_TYPE_R// R
};


class RustSPQRResult {
public:
    explicit RustSPQRResult(const RustGraph& graph)
        : result_(spqr_build(graph.raw())) {
        if (!result_) throw std::runtime_error("Failed to build SPQR tree");
    }

    ~RustSPQRResult() {
        if (result_) spqr_result_free(result_);
    }

    RustSPQRResult(RustSPQRResult&& other) noexcept : result_(other.result_) { other.result_ = nullptr; }
    RustSPQRResult& operator=(RustSPQRResult&&) = delete;
    RustSPQRResult(const RustSPQRResult&) = delete;
    RustSPQRResult& operator=(const RustSPQRResult&) = delete;

    const SpqrTree* tree() const { return spqr_result_tree(result_); }

    std::vector<uint32_t> selfLoops() const {
        uint32_t len;
        const uint32_t* data = spqr_result_self_loops(result_, &len);
        return std::vector<uint32_t>(data, data + len);
    }

    uint32_t treeLen() const { return spqr_tree_len(tree()); }
    uint32_t treeRoot() const { return spqr_tree_root(tree()); }

    SPQRNodeType nodeType(uint32_t nodeId) const {
        return static_cast<SPQRNodeType>(spqr_tree_node_type(tree(), nodeId));
    }

    uint32_t nodeParent(uint32_t nodeId) const {
        return spqr_tree_node_parent(tree(), nodeId);
    }

    std::vector<uint32_t> nodeChildren(uint32_t nodeId) const {
        uint32_t len;
        const uint32_t* data = spqr_tree_node_children(tree(), nodeId, &len);
        return std::vector<uint32_t>(data, data + len);
    }

    uint32_t skeletonNumEdges(uint32_t nodeId) const {
        return spqr_tree_skeleton_num_edges(tree(), nodeId);
    }

    uint32_t skeletonNumNodes(uint32_t nodeId) const {
        return spqr_tree_skeleton_num_nodes(tree(), nodeId);
    }

    std::pair<uint32_t, uint32_t> skeletonPoles(uint32_t nodeId) const {
        uint32_t p1, p2;
        spqr_tree_skeleton_poles(tree(), nodeId, &p1, &p2);
        return {p1, p2};
    }

    SkeletonEdgeInfo skeletonEdge(uint32_t nodeId, uint32_t edgeIdx) const {
        SkeletonEdgeInfo info;
        spqr_tree_skeleton_edge(tree(), nodeId, edgeIdx, &info);
        return info;
    }

    uint32_t skeletonOriginalNode(uint32_t treeNodeId, uint32_t localNode) const {
        return spqr_tree_skeleton_original_node(tree(), treeNodeId, localNode);
    }

    uint32_t nodeOfEdge(uint32_t edgeId) const {
        return spqr_tree_node_of_edge(tree(), edgeId);
    }

    void countByType(uint32_t& sCount, uint32_t& pCount, uint32_t& rCount) const {
        spqr_tree_count_by_type(tree(), &sCount, &pCount, &rCount);
    }

    std::string toFormatString(const RustGraph& graph) const {
        char* s = spqr_format_to_string(graph.raw(), result_);
        if (!s) return "";
        std::string result(s);
        spqr_string_free(s);
        return result;
    }

private:
    SpqrResult* result_;
};


class SPQRTreeExport {
public:
    explicit SPQRTreeExport(const RustSPQRResult& result) {
        const SpqrTree* tree = result.tree();
        
        uint32_t totalChildren, totalSkelEdges;
        spqr_tree_get_sizes(tree, &numNodes_, &totalChildren, &totalSkelEdges);
        
        nodeTypes.resize(numNodes_);
        nodeParents.resize(numNodes_);
        childrenOffsets.resize(numNodes_ + 1);
        children.resize(totalChildren);
        skeletonOffsets.resize(numNodes_ + 1);
        skeletonSrc.resize(totalSkelEdges);
        skeletonDst.resize(totalSkelEdges);
        skeletonRealEdge.resize(totalSkelEdges);
        skeletonIsVirtual.resize(totalSkelEdges);
        
        spqr_tree_bulk_export(tree,
            nodeTypes.data(),
            nodeParents.data(),
            childrenOffsets.data(),
            children.data(),
            skeletonOffsets.data(),
            skeletonSrc.data(),
            skeletonDst.data(),
            skeletonRealEdge.data(),
            skeletonIsVirtual.data());
    }
    
    SPQRTreeExport(SPQRTreeExport&&) = default;
    SPQRTreeExport& operator=(SPQRTreeExport&&) = default;
    SPQRTreeExport(const SPQRTreeExport&) = delete;
    SPQRTreeExport& operator=(const SPQRTreeExport&) = delete;
    
    uint32_t numNodes() const { return numNodes_; }
    
    uint8_t nodeType(uint32_t i) const { return nodeTypes[i]; }
    uint32_t nodeParent(uint32_t i) const { return nodeParents[i]; }
    
    uint32_t childrenBegin(uint32_t i) const { return childrenOffsets[i]; }
    uint32_t childrenEnd(uint32_t i) const { return childrenOffsets[i + 1]; }
    uint32_t numChildren(uint32_t i) const { return childrenEnd(i) - childrenBegin(i); }
    
    uint32_t skeletonBegin(uint32_t i) const { return skeletonOffsets[i]; }
    uint32_t skeletonEnd(uint32_t i) const { return skeletonOffsets[i + 1]; }
    uint32_t numSkeletonEdges(uint32_t i) const { return skeletonEnd(i) - skeletonBegin(i); }
    
    static std::vector<uint32_t> exportEdgeMapping(const RustSPQRResult& result, uint32_t numEdges) {
        std::vector<uint32_t> mapping(numEdges);
        spqr_tree_edge_mapping_bulk(result.tree(), numEdges, mapping.data());
        return mapping;
    }

    static std::pair<const uint32_t*, uint32_t> edgeMappingZeroCopy(const RustSPQRResult& result) {
        uint32_t len;
        const uint32_t* ptr = spqr_tree_edge_mapping_raw(result.tree(), &len);
        return {ptr, len};
    }
    
    std::vector<uint8_t> nodeTypes;
    std::vector<uint32_t> nodeParents;
    std::vector<uint32_t> childrenOffsets;
    std::vector<uint32_t> children;
    std::vector<uint32_t> skeletonOffsets;
    std::vector<uint32_t> skeletonSrc;
    std::vector<uint32_t> skeletonDst;
    std::vector<uint32_t> skeletonRealEdge;
    std::vector<uint8_t> skeletonIsVirtual;
    
private:
    uint32_t numNodes_;
};


class SpqrTreeFlatView {
public:
    explicit SpqrTreeFlatView(const RustSPQRResult& result)
        : tree_(result.tree()) {
        if (!tree_) throw std::runtime_error("No SPQR tree in result");
        
        spqr_tree_info(tree_, &numNodes, &root);
        
        nodeTypes = spqr_tree_node_types_raw(tree_);
        nodeParents = spqr_tree_node_parents_raw(tree_);
        childrenOffsets = spqr_tree_children_offsets_raw(tree_);
        children = spqr_tree_children_raw(tree_, &numChildren);
        skeletonOffsets = spqr_tree_skeleton_offsets_raw(tree_);
        skeletonEdges = spqr_tree_skeleton_edges_raw(tree_, &numSkeletonEdges);
        spqr_tree_node_mapping_raw(tree_, &nodeMappingOffsets, &nodeMapping, &numNodeMapping);
        skeletonNumNodes = spqr_tree_skeleton_num_nodes_raw(tree_);
        
        edgeToTreeNode = spqr_tree_edge_mapping_raw(tree_, &numEdges);
    }
    
    ~SpqrTreeFlatView() = default;
    
    SpqrTreeFlatView(SpqrTreeFlatView&& other) noexcept = default;
    SpqrTreeFlatView(const SpqrTreeFlatView&) = delete;
    SpqrTreeFlatView& operator=(const SpqrTreeFlatView&) = delete;
    SpqrTreeFlatView& operator=(SpqrTreeFlatView&&) = delete;
    
    uint32_t numNodes;
    uint32_t root;
    
    const uint8_t* nodeTypes;       
    const uint32_t* nodeParents;    
    
    const uint32_t* childrenOffsets; 
    const uint32_t* children;        
    uint32_t numChildren;
    
    const uint32_t* skeletonOffsets; 
    const SkeletonEdge* skeletonEdges;
    uint32_t numSkeletonEdges;
    
    const uint32_t* skeletonNumNodes;     
    
    const uint32_t* nodeMappingOffsets;   
    const uint32_t* nodeMapping;          
    uint32_t numNodeMapping;
    
    const uint32_t* edgeToTreeNode; 
    uint32_t numEdges;
    
private:
    const SpqrTree* tree_;
};


class RustBCTree {
public:
    explicit RustBCTree(const RustGraph& graph)
        : bc_(spqr_bc_tree_build(graph.raw())) {
        if (!bc_) throw std::runtime_error("Failed to build BC tree");
    }

    ~RustBCTree() {
        if (bc_) spqr_bc_tree_free(bc_);
    }

    RustBCTree(RustBCTree&& other) noexcept : bc_(other.bc_) { other.bc_ = nullptr; }
    RustBCTree& operator=(RustBCTree&&) = delete;
    RustBCTree(const RustBCTree&) = delete;
    RustBCTree& operator=(const RustBCTree&) = delete;

    uint32_t numBlocks() const { return spqr_bc_num_blocks(bc_); }
    uint32_t numCutVertices() const { return spqr_bc_num_cut_vertices(bc_); }
    bool isBiconnected() const { return spqr_bc_is_biconnected(bc_); }
    bool isCutVertex(uint32_t node) const { return spqr_bc_is_cut_vertex(bc_, node); }

    std::vector<uint32_t> blockNodes(uint32_t blockIdx) const {
        uint32_t len;
        const uint32_t* data = spqr_bc_block_nodes(bc_, blockIdx, &len);
        return std::vector<uint32_t>(data, data + len);
    }

    std::vector<uint32_t> blockEdges(uint32_t blockIdx) const {
        uint32_t len;
        const uint32_t* data = spqr_bc_block_edges(bc_, blockIdx, &len);
        return std::vector<uint32_t>(data, data + len);
    }

    std::vector<uint32_t> cutVertices() const {
        uint32_t len;
        const uint32_t* data = spqr_bc_cut_vertices(bc_, &len);
        return std::vector<uint32_t>(data, data + len);
    }

    BCTree* raw() { return bc_; }
    const BCTree* raw() const { return bc_; }

private:
    BCTree* bc_;
};


class BCTreeExport {
public:
    explicit BCTreeExport(const RustBCTree& bc) {
        // Get sizes
        uint32_t totalNodes, totalEdges;
        spqr_bc_get_sizes(bc.raw(), &numBlocks_, &totalNodes, &totalEdges);
        
        // Allocate
        blockNodeOffsets.resize(numBlocks_ + 1);
        blockNodes.resize(totalNodes);
        blockEdgeOffsets.resize(numBlocks_ + 1);
        blockEdges.resize(totalEdges);
        
        spqr_bc_bulk_export(bc.raw(),
            blockNodeOffsets.data(),
            blockNodes.data(),
            blockEdgeOffsets.data(),
            blockEdges.data());
    }
    
    uint32_t numBlocks() const { return numBlocks_; }
    
    uint32_t blockNodesBegin(uint32_t i) const { return blockNodeOffsets[i]; }
    uint32_t blockNodesEnd(uint32_t i) const { return blockNodeOffsets[i + 1]; }
    uint32_t numBlockNodes(uint32_t i) const { return blockNodesEnd(i) - blockNodesBegin(i); }
    
    uint32_t blockEdgesBegin(uint32_t i) const { return blockEdgeOffsets[i]; }
    uint32_t blockEdgesEnd(uint32_t i) const { return blockEdgeOffsets[i + 1]; }
    uint32_t numBlockEdges(uint32_t i) const { return blockEdgesEnd(i) - blockEdgesBegin(i); }
    
    std::vector<uint32_t> blockNodeOffsets;
    std::vector<uint32_t> blockNodes;
    std::vector<uint32_t> blockEdgeOffsets;
    std::vector<uint32_t> blockEdges;
    
private:
    uint32_t numBlocks_;
};


class BCTreeZeroCopy {
public:
    explicit BCTreeZeroCopy(const RustBCTree& bc) {
        blocks = reinterpret_cast<const BCBlock*>(
            spqr_bc_blocks_raw(bc.raw(), &numBlocks));
        nodesFlat = spqr_bc_nodes_flat_raw(bc.raw(), &numNodesFlat);
        edgesFlat = spqr_bc_edges_flat_raw(bc.raw(), &numEdgesFlat);
    }
    
    const BCBlock* blocks;
    const uint32_t* nodesFlat;
    const uint32_t* edgesFlat;
    uint32_t numBlocks;
    uint32_t numNodesFlat;
    uint32_t numEdgesFlat;
};

class RustConnectedComponents {
public:
    explicit RustConnectedComponents(const RustGraph& graph)
        : cc_(spqr_connected_components(graph.raw())) {
        if (!cc_) throw std::runtime_error("Failed to compute connected components");
    }

    ~RustConnectedComponents() {
        if (cc_) spqr_cc_free(cc_);
    }

    RustConnectedComponents(RustConnectedComponents&& other) noexcept : cc_(other.cc_) { other.cc_ = nullptr; }
    RustConnectedComponents& operator=(RustConnectedComponents&&) = delete;
    RustConnectedComponents(const RustConnectedComponents&) = delete;
    RustConnectedComponents& operator=(const RustConnectedComponents&) = delete;

    uint32_t count() const { return spqr_cc_count(cc_); }

    uint32_t componentOf(uint32_t node) const {
        return spqr_cc_component_of(cc_, node);
    }

    uint32_t countIn(uint32_t componentId) const {
        return spqr_cc_count_in(cc_, componentId);
    }

    /** Get raw component array (zero-copy) */
    std::pair<const uint32_t*, uint32_t> componentsRaw() const {
        uint32_t len;
        const uint32_t* data = spqr_cc_components_raw(cc_, &len);
        return {data, len};
    }

private:
    CCResult* cc_;
};


inline RustSPQRResult buildSPQR(const RustGraph& graph) {
    return RustSPQRResult(graph);
}

inline RustBCTree buildBCTree(const RustGraph& graph) {
    return RustBCTree(graph);
}

inline RustConnectedComponents computeCC(const RustGraph& graph) {
    return RustConnectedComponents(graph);
}

}
#endif 