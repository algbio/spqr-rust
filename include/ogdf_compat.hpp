#pragma once
// spqr-rust C++ interface
//
// Two namespaces:
//   spqr             -> with G.source(e) syntax
//   spqr::ogdf_compat -> with e->source() syntax, like OGDF
// Pick spqr unless you're porting OGDF code and don't want to change every call site

#include "spqr_rust_wrapper.hpp"
#include <vector>
#include <memory>

namespace spqr {

struct node {
    uint32_t idx;
    
    constexpr node() : idx(UINT32_MAX) {}
    constexpr node(uint32_t i) : idx(i) {}
    constexpr node(std::nullptr_t) : idx(UINT32_MAX) {}  
    
    constexpr operator uint32_t() const { return idx; }
    constexpr uint32_t index() const { return idx; }
    
    constexpr bool operator!() const { return idx == UINT32_MAX; }
    constexpr explicit operator bool() const { return idx != UINT32_MAX; }
    constexpr bool operator==(std::nullptr_t) const { return idx == UINT32_MAX; }
    constexpr bool operator!=(std::nullptr_t) const { return idx != UINT32_MAX; }
    constexpr bool operator==(node o) const { return idx == o.idx; }
    constexpr bool operator!=(node o) const { return idx != o.idx; }
    constexpr bool operator<(node o) const { return idx < o.idx; }
};

struct edge {
    uint32_t idx;
    
    constexpr edge() : idx(UINT32_MAX) {}
    constexpr edge(uint32_t i) : idx(i) {}
    constexpr edge(std::nullptr_t) : idx(UINT32_MAX) {}
    
    constexpr operator uint32_t() const { return idx; }
    constexpr uint32_t index() const { return idx; }
    
    constexpr bool operator!() const { return idx == UINT32_MAX; }
    constexpr explicit operator bool() const { return idx != UINT32_MAX; }
    constexpr bool operator==(std::nullptr_t) const { return idx == UINT32_MAX; }
    constexpr bool operator!=(std::nullptr_t) const { return idx != UINT32_MAX; }
    constexpr bool operator==(edge o) const { return idx == o.idx; }
    constexpr bool operator!=(edge o) const { return idx != o.idx; }
    constexpr bool operator<(edge o) const { return idx < o.idx; }
};

constexpr node INVALID_NODE{UINT32_MAX};
constexpr edge INVALID_EDGE{UINT32_MAX};


template<typename T>
class NodeArray : public std::vector<T> {
public:
    NodeArray() = default;
    template<typename G> NodeArray(const G& g, const T& def = T()) : std::vector<T>(g.numberOfNodes(), def) {}
    NodeArray(size_t n, const T& def = T()) : std::vector<T>(n, def) {}
    template<typename G> void init(const G& g, const T& def = T()) { this->assign(g.numberOfNodes(), def); }
};

template<typename T>
class EdgeArray : public std::vector<T> {
public:
    EdgeArray() = default;
    template<typename G> EdgeArray(const G& g, const T& def = T()) : std::vector<T>(g.numberOfEdges(), def) {}
    EdgeArray(size_t n, const T& def = T()) : std::vector<T>(n, def) {}
    template<typename G> void init(const G& g, const T& def = T()) { this->assign(g.numberOfEdges(), def); }
};

// Graph

class Graph {
    std::unique_ptr<spqr_rust::RustGraph> g_;

public:
    struct NodesRange {
        const Graph* g;
        struct It { uint32_t i; constexpr node operator*() const { return node{i}; } constexpr It& operator++() { ++i; return *this; } constexpr bool operator!=(It o) const { return i != o.i; } };
        It begin() const { return {0}; }
        It end() const { return {g->numberOfNodes()}; }
    };
    struct EdgesRange {
        const Graph* g;
        struct It { uint32_t i; constexpr edge operator*() const { return edge{i}; } constexpr It& operator++() { ++i; return *this; } constexpr bool operator!=(It o) const { return i != o.i; } };
        It begin() const { return {0}; }
        It end() const { return {g->numberOfEdges()}; }
    };

    Graph() : g_(std::make_unique<spqr_rust::RustGraph>()), nodes{this}, edges{this} {}
    
    node newNode() { return node{g_->addNode()}; }
    edge newEdge(node u, node v) { return edge{g_->addEdge(u.idx, v.idx)}; }
    
    uint32_t numberOfNodes() const { return g_->numNodes(); }
    uint32_t numberOfEdges() const { return g_->numEdges(); }
    
    node firstNode() const { return numberOfNodes() > 0 ? node{0u} : INVALID_NODE; }
    
    // In this namespace you need the graph to get source/target (edge doesn't know its graph)
    node source(edge e) const { return node{g_->edgeSrc(e.idx)}; }
    node target(edge e) const { return node{g_->edgeDst(e.idx)}; }
    
    NodesRange nodes;
    EdgesRange edges;
    
    template<typename F>
    void forEachAdj(node v, F&& f) const {
        g_->forEachNeighbor(v.idx, [&](uint32_t n, uint32_t e) { f(node{n}, edge{e}); });
    }
    
    uint32_t degree(node v) const { return g_->degree(v.idx); }
    
    spqr_rust::RustGraph& raw() { return *g_; }
    const spqr_rust::RustGraph& raw() const { return *g_; }
};

// BCTree

class BCTree {
    std::unique_ptr<spqr_rust::RustBCTree> bc_;
    std::vector<bool> isCut_;

public:
    enum class BNodeType { BComp, CComp };
    enum class GNodeType { Normal, CutVertex };
    
    explicit BCTree(const Graph& g) : bc_(std::make_unique<spqr_rust::RustBCTree>(g.raw())) {
        isCut_.assign(g.numberOfNodes(), false);
        for (uint32_t v : bc_->cutVertices()) isCut_[v] = true;
    }
    
    uint32_t numberOfBComps() const { return bc_->numBlocks(); }
    uint32_t numberOfCComps() const { return bc_->numCutVertices(); }
    
    GNodeType typeOfGNode(node v) const { return isCut_[v.idx] ? GNodeType::CutVertex : GNodeType::Normal; }
    BNodeType typeOfBNode(node v) const { return v.idx < bc_->numBlocks() ? BNodeType::BComp : BNodeType::CComp; }
    
    std::vector<edge> hEdges(node bNode) const {
        if (bNode.idx >= bc_->numBlocks()) return {};
        auto raw = bc_->blockEdges(bNode.idx);
        std::vector<edge> r; r.reserve(raw.size());
        for (auto e : raw) r.push_back(edge{e});
        return r;
    }
    
    edge original(edge e) const { return e; }
    node repVertex(node v, node) const { return v; }
    node bcproper(node v) const { return v; }
    
    struct BCTreeGraph {
        uint32_t n;
        struct NodesRange { uint32_t n; struct It { uint32_t i; node operator*() const { return node{i}; } It& operator++() { ++i; return *this; } bool operator!=(It o) const { return i != o.i; } }; It begin() const { return {0}; } It end() const { return {n}; } };
        NodesRange nodes() const { return {n}; }
        uint32_t numberOfNodes() const { return n; }
    };
    BCTreeGraph bcTree() const { return {bc_->numBlocks() + bc_->numCutVertices()}; }
};

// StaticSPQRTree

using tree_node = node;

class TreeGraph {
    uint32_t n_ = 0;
    std::vector<uint32_t> parents_, src_, tgt_;
public:
    void build(uint32_t n, const std::vector<uint32_t>& parents) {
        n_ = n; parents_ = parents;
        src_.clear(); tgt_.clear();
        for (uint32_t i = 0; i < n; ++i)
            if (parents[i] != UINT32_MAX && parents[i] != i) { src_.push_back(parents[i]); tgt_.push_back(i); }
    }
    uint32_t numberOfNodes() const { return n_; }
    uint32_t numberOfEdges() const { return src_.size(); }
    node source(edge e) const { return node{src_[e.idx]}; }
    node target(edge e) const { return node{tgt_[e.idx]}; }
    
    struct NodesRange { uint32_t n; struct It { uint32_t i; node operator*() const { return node{i}; } It& operator++() { ++i; return *this; } bool operator!=(It o) const { return i != o.i; } }; It begin() const { return {0}; } It end() const { return {n}; } };
    struct EdgesRange { uint32_t n; struct It { uint32_t i; edge operator*() const { return edge{i}; } It& operator++() { ++i; return *this; } bool operator!=(It o) const { return i != o.i; } }; It begin() const { return {0}; } It end() const { return {n}; } };
    NodesRange nodes() const { return {n_}; }
    EdgesRange edges() const { return {uint32_t(src_.size())}; }
};

class StaticSPQRTree {
    std::unique_ptr<spqr_rust::RustSPQRResult> result_;
    spqr_rust::SpqrTreeFlatView view_;
    std::vector<uint32_t> parents_;
    TreeGraph tree_;
    
    void buildTree() {
        parents_.resize(view_.numNodes);
        for (uint32_t i = 0; i < view_.numNodes; ++i) parents_[i] = view_.nodeParents[i];
        tree_.build(view_.numNodes, parents_);
    }
    edge findVirtual(tree_node from, tree_node to) const {
        uint32_t s = view_.skeletonOffsets[from.idx], e = view_.skeletonOffsets[from.idx + 1];
        for (uint32_t i = s; i < e; ++i)
            if (view_.skeletonEdges[i].real_edge == UINT32_MAX && view_.skeletonEdges[i].twin_tree_node == to.idx)
                return edge{i - s};
        return INVALID_EDGE;
    }

public:
    enum class NodeType { SNode, PNode, RNode };
    using SkeletonEdge = ::SkeletonEdge;
    
    explicit StaticSPQRTree(const Graph& g) : result_(std::make_unique<spqr_rust::RustSPQRResult>(g.raw())), view_(*result_) { buildTree(); }
    
    tree_node rootNode() const { return node{0u}; }
    uint32_t numberOfNodes() const { return view_.numNodes; }
    NodeType typeOf(tree_node tn) const { return view_.nodeTypes[tn.idx] == 0 ? NodeType::SNode : view_.nodeTypes[tn.idx] == 1 ? NodeType::PNode : NodeType::RNode; }
    const TreeGraph& tree() const { return tree_; }
    tree_node parent(tree_node tn) const { return node{parents_[tn.idx]}; }
    
    class Skeleton {
        const StaticSPQRTree& t_; tree_node tn_; mutable std::unique_ptr<Graph> g_;
        void buildGraph() const {
            uint32_t nN = t_.view_.skeletonNumNodes[tn_.idx], off = t_.view_.skeletonOffsets[tn_.idx], end = t_.view_.skeletonOffsets[tn_.idx + 1];
            g_ = std::make_unique<Graph>();
            for (uint32_t i = 0; i < nN; ++i) g_->newNode();
            for (uint32_t i = off; i < end; ++i) { auto& se = t_.view_.skeletonEdges[i]; if (se.src < nN && se.dst < nN) g_->newEdge(node{se.src}, node{se.dst}); }
        }
        const SkeletonEdge* edgeAt(edge e) const { return &t_.view_.skeletonEdges[t_.view_.skeletonOffsets[tn_.idx] + e.idx]; }
    public:
        Skeleton(const StaticSPQRTree& t, tree_node tn) : t_(t), tn_(tn) {}
        const Graph& getGraph() const { if (!g_) buildGraph(); return *g_; }
        node original(node local) const { return node{t_.view_.nodeMapping[t_.view_.nodeMappingOffsets[tn_.idx] + local.idx]}; }
        bool isVirtual(edge e) const { return edgeAt(e)->real_edge == UINT32_MAX; }
        tree_node twinTreeNode(edge e) const { auto* se = edgeAt(e); return se->real_edge == UINT32_MAX ? node{se->twin_tree_node} : INVALID_NODE; }
        edge realEdge(edge e) const { auto* se = edgeAt(e); return se->real_edge != UINT32_MAX ? edge{se->real_edge} : INVALID_EDGE; }
    };
    
    Skeleton skeleton(tree_node tn) const { return Skeleton(*this, tn); }
    edge skeletonEdgeSrc(edge te) const { return findVirtual(tree_.source(te), tree_.target(te)); }
    edge skeletonEdgeTgt(edge te) const { return findVirtual(tree_.target(te), tree_.source(te)); }
};

using SPQRTree = StaticSPQRTree;

inline uint32_t connectedComponents(const Graph& g, NodeArray<int>& comp) {
    spqr_rust::RustConnectedComponents cc(g.raw());
    auto [data, len] = cc.componentsRaw();
    comp.assign(data, data + len);
    return cc.count();
}

// spqr::ogdf_compat

// Same API but edge carries a pointer to its graph, so e->source() works.
// Costs 12 extra bytes per edge

namespace ogdf_compat {

using node = spqr::node;
constexpr node INVALID_NODE = spqr::INVALID_NODE;

class Graph;

struct edge {
    uint32_t idx;
    const Graph* g;  
    
    constexpr edge() : idx(UINT32_MAX), g(nullptr) {}
    constexpr edge(uint32_t i) : idx(i), g(nullptr) {}
    constexpr edge(uint32_t i, const Graph* gr) : idx(i), g(gr) {}
    constexpr edge(std::nullptr_t) : idx(UINT32_MAX), g(nullptr) {}
    
    constexpr uint32_t index() const { return idx; }
    
    constexpr bool operator!() const { return idx == UINT32_MAX; }
    constexpr explicit operator bool() const { return idx != UINT32_MAX; }
    constexpr bool operator==(std::nullptr_t) const { return idx == UINT32_MAX; }
    constexpr bool operator!=(std::nullptr_t) const { return idx != UINT32_MAX; }
    constexpr bool operator==(edge o) const { return idx == o.idx; }
    constexpr bool operator!=(edge o) const { return idx != o.idx; }
    constexpr bool operator<(edge o) const { return idx < o.idx; }
    
    inline node source() const;
    inline node target() const;
    
    const edge* operator->() const { return this; }
};

const edge INVALID_EDGE{UINT32_MAX, nullptr};

struct adjEntry {
    node neighbor;
    edge e;
    node twinNode() const { return neighbor; }
    edge theEdge() const { return e; }
};

template<typename T> using NodeArray = spqr::NodeArray<T>;
template<typename T> using EdgeArray = spqr::EdgeArray<T>;

class Graph {
    std::unique_ptr<spqr_rust::RustGraph> g_;
    mutable std::vector<adjEntry> adjCache_;

public:
    struct NodesRange {
        const Graph* g;
        struct It { uint32_t i; node operator*() const { return node{i}; } It& operator++() { ++i; return *this; } bool operator!=(It o) const { return i != o.i; } };
        It begin() const { return {0}; }
        It end() const { return {g->numberOfNodes()}; }
    };
    struct EdgesRange {
        const Graph* g;
        struct It { uint32_t i; const Graph* g; edge operator*() const { return edge{i, g}; } It& operator++() { ++i; return *this; } bool operator!=(It o) const { return i != o.i; } };
        It begin() const { return {0, g}; }
        It end() const { return {g->numberOfEdges(), g}; }
    };

    Graph() : g_(std::make_unique<spqr_rust::RustGraph>()), nodes{this}, edges{this} {}
    
    node newNode() { return node{g_->addNode()}; }
    edge newEdge(node u, node v) { return edge{g_->addEdge(u.idx, v.idx), this}; }
    
    uint32_t numberOfNodes() const { return g_->numNodes(); }
    uint32_t numberOfEdges() const { return g_->numEdges(); }
    
    node firstNode() const { return numberOfNodes() > 0 ? node{0u} : INVALID_NODE; }
    
    node source(edge e) const { return node{g_->edgeSrc(e.idx)}; }
    node target(edge e) const { return node{g_->edgeDst(e.idx)}; }
    
    NodesRange nodes;
    EdgesRange edges;
    
    template<typename F>
    void forEachAdj(node v, F&& f) const {
        g_->forEachNeighbor(v.idx, [&](uint32_t n, uint32_t e) { f(node{n}, edge{e, this}); });
    }
    
    const std::vector<adjEntry>& getAdjEntries(node v) const {
        adjCache_.clear();
        g_->forEachNeighbor(v.idx, [this](uint32_t n, uint32_t e) {
            adjCache_.push_back({node{n}, edge{e, this}});
        });
        return adjCache_;
    }
    
    uint32_t degree(node v) const { return g_->degree(v.idx); }
    
    spqr_rust::RustGraph& raw() { return *g_; }
    const spqr_rust::RustGraph& raw() const { return *g_; }
};

inline node edge::source() const { return g->source(*this); }
inline node edge::target() const { return g->target(*this); }

// BCTree / StaticSPQRTree for ogdf_compat
// (mostly copy-paste from above, but using ogdf_compat::edge)

class BCTree {
    std::unique_ptr<spqr_rust::RustBCTree> bc_;
    std::vector<bool> isCut_;
public:
    enum class BNodeType { BComp, CComp };
    enum class GNodeType { Normal, CutVertex };
    
    explicit BCTree(const Graph& g) : bc_(std::make_unique<spqr_rust::RustBCTree>(g.raw())) {
        isCut_.assign(g.numberOfNodes(), false);
        for (uint32_t v : bc_->cutVertices()) isCut_[v] = true;
    }
    
    uint32_t numberOfBComps() const { return bc_->numBlocks(); }
    uint32_t numberOfCComps() const { return bc_->numCutVertices(); }
    GNodeType typeOfGNode(node v) const { return isCut_[v.idx] ? GNodeType::CutVertex : GNodeType::Normal; }
    BNodeType typeOfBNode(node v) const { return v.idx < bc_->numBlocks() ? BNodeType::BComp : BNodeType::CComp; }
    
    std::vector<edge> hEdges(node bNode) const {
        if (bNode.idx >= bc_->numBlocks()) return {};
        auto raw = bc_->blockEdges(bNode.idx);
        std::vector<edge> r; r.reserve(raw.size());
        for (auto e : raw) r.push_back(edge{e});
        return r;
    }
    edge original(edge e) const { return e; }
    node repVertex(node v, node) const { return v; }
    node bcproper(node v) const { return v; }
    
    struct BCTreeGraph {
        uint32_t n;
        struct NodesRange { uint32_t n; struct It { uint32_t i; node operator*() const { return node{i}; } It& operator++() { ++i; return *this; } bool operator!=(It o) const { return i != o.i; } }; It begin() const { return {0}; } It end() const { return {n}; } };
        NodesRange nodes() const { return {n}; }
        uint32_t numberOfNodes() const { return n; }
    };
    BCTreeGraph bcTree() const { return {bc_->numBlocks() + bc_->numCutVertices()}; }
};

using tree_node = node;

class TreeGraph {
    uint32_t n_ = 0;
    std::vector<uint32_t> parents_, src_, tgt_;
public:
    void build(uint32_t n, const std::vector<uint32_t>& parents) {
        n_ = n; parents_ = parents; src_.clear(); tgt_.clear();
        for (uint32_t i = 0; i < n; ++i) if (parents[i] != UINT32_MAX && parents[i] != i) { src_.push_back(parents[i]); tgt_.push_back(i); }
    }
    uint32_t numberOfNodes() const { return n_; }
    uint32_t numberOfEdges() const { return src_.size(); }
    node source(edge e) const { return node{src_[e.idx]}; }
    node target(edge e) const { return node{tgt_[e.idx]}; }
    struct NodesRange { uint32_t n; struct It { uint32_t i; node operator*() const { return node{i}; } It& operator++() { ++i; return *this; } bool operator!=(It o) const { return i != o.i; } }; It begin() const { return {0}; } It end() const { return {n}; } };
    struct EdgesRange { uint32_t n; struct It { uint32_t i; edge operator*() const { return edge{i}; } It& operator++() { ++i; return *this; } bool operator!=(It o) const { return i != o.i; } }; It begin() const { return {0}; } It end() const { return {n}; } };
    NodesRange nodes() const { return {n_}; }
    EdgesRange edges() const { return {uint32_t(src_.size())}; }
};

class StaticSPQRTree {
    std::unique_ptr<spqr_rust::RustSPQRResult> result_;
    spqr_rust::SpqrTreeFlatView view_;
    std::vector<uint32_t> parents_;
    TreeGraph tree_;
    
    void buildTree() { parents_.resize(view_.numNodes); for (uint32_t i = 0; i < view_.numNodes; ++i) parents_[i] = view_.nodeParents[i]; tree_.build(view_.numNodes, parents_); }
    edge findVirtual(tree_node from, tree_node to) const {
        uint32_t s = view_.skeletonOffsets[from.idx], e = view_.skeletonOffsets[from.idx + 1];
        for (uint32_t i = s; i < e; ++i) if (view_.skeletonEdges[i].real_edge == UINT32_MAX && view_.skeletonEdges[i].twin_tree_node == to.idx) return edge{i - s};
        return INVALID_EDGE;
    }
public:
    enum class NodeType { SNode, PNode, RNode };
    using SkeletonEdge = ::SkeletonEdge;
    
    explicit StaticSPQRTree(const Graph& g) : result_(std::make_unique<spqr_rust::RustSPQRResult>(g.raw())), view_(*result_) { buildTree(); }
    
    tree_node rootNode() const { return node{0u}; }
    uint32_t numberOfNodes() const { return view_.numNodes; }
    NodeType typeOf(tree_node tn) const { return view_.nodeTypes[tn.idx] == 0 ? NodeType::SNode : view_.nodeTypes[tn.idx] == 1 ? NodeType::PNode : NodeType::RNode; }
    const TreeGraph& tree() const { return tree_; }
    tree_node parent(tree_node tn) const { return node{parents_[tn.idx]}; }
    
    class Skeleton {
        const StaticSPQRTree& t_; tree_node tn_; mutable std::unique_ptr<Graph> g_;
        void buildGraph() const {
            uint32_t nN = t_.view_.skeletonNumNodes[tn_.idx], off = t_.view_.skeletonOffsets[tn_.idx], end = t_.view_.skeletonOffsets[tn_.idx + 1];
            g_ = std::make_unique<Graph>();
            for (uint32_t i = 0; i < nN; ++i) g_->newNode();
            for (uint32_t i = off; i < end; ++i) { auto& se = t_.view_.skeletonEdges[i]; if (se.src < nN && se.dst < nN) g_->newEdge(node{se.src}, node{se.dst}); }
        }
        const SkeletonEdge* edgeAt(edge e) const { return &t_.view_.skeletonEdges[t_.view_.skeletonOffsets[tn_.idx] + e.idx]; }
    public:
        Skeleton(const StaticSPQRTree& t, tree_node tn) : t_(t), tn_(tn) {}
        const Graph& getGraph() const { if (!g_) buildGraph(); return *g_; }
        node original(node local) const { return node{t_.view_.nodeMapping[t_.view_.nodeMappingOffsets[tn_.idx] + local.idx]}; }
        bool isVirtual(edge e) const { return edgeAt(e)->real_edge == UINT32_MAX; }
        tree_node twinTreeNode(edge e) const { auto* se = edgeAt(e); return se->real_edge == UINT32_MAX ? node{se->twin_tree_node} : INVALID_NODE; }
        edge realEdge(edge e) const { auto* se = edgeAt(e); return se->real_edge != UINT32_MAX ? edge{se->real_edge} : INVALID_EDGE; }
    };
    
    Skeleton skeleton(tree_node tn) const { return Skeleton(*this, tn); }
    edge skeletonEdgeSrc(edge te) const { return findVirtual(tree_.source(te), tree_.target(te)); }
    edge skeletonEdgeTgt(edge te) const { return findVirtual(tree_.target(te), tree_.source(te)); }
};

using SPQRTree = StaticSPQRTree;

inline uint32_t connectedComponents(const Graph& g, NodeArray<int>& comp) {
    spqr_rust::RustConnectedComponents cc(g.raw());
    auto [data, len] = cc.componentsRaw();
    comp.assign(data, data + len);
    return cc.count();
}

} 
} 