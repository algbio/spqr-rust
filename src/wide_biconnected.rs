//! Biconnected blocks for graphs whose node and edge IDs use u64.

use crate::spqr_thread_count;
use std::ops::Range;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::thread;

const PARALLEL_CSR_MIN_EDGES: usize = 4_000_000;
const PARALLEL_CSR_EDGES_PER_WORKER: usize = 2_000_000;
const DFS_PACKED_MAX: u64 = (1u64 << 40) - 1;
const CSR_PACKED_MAX: u64 = (1u64 << 33) - 1;
const NARROW_DFS_MIN_SAVINGS: u128 = 3 << 30;
const U33_MAX: u64 = (1u64 << 33) - 1;
const U34_MAX: u64 = (1u64 << 34) - 1;

#[derive(Clone, Debug, Default)]
struct PackedU40 {
    low: Vec<u32>,
    high: Vec<u8>,
}

impl PackedU40 {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            low: Vec::with_capacity(capacity),
            high: Vec::with_capacity(capacity),
        }
    }

    fn with_len(len: usize) -> Self {
        Self {
            low: vec![0; len],
            high: vec![0; len],
        }
    }

    #[inline]
    fn len(&self) -> usize {
        self.low.len()
    }

    #[inline]
    fn is_empty(&self) -> bool {
        self.low.is_empty()
    }

    #[inline]
    fn capacity(&self) -> usize {
        self.low.capacity().min(self.high.capacity())
    }

    fn try_reserve(&mut self, additional: usize) {
        let _ = self.low.try_reserve(additional);
        let _ = self.high.try_reserve(additional);
    }

    #[inline]
    fn push(&mut self, value: u64) {
        debug_assert!(value <= DFS_PACKED_MAX);
        self.low.push(value as u32);
        self.high.push((value >> 32) as u8);
    }

    #[inline]
    fn get(&self, index: usize) -> u64 {
        self.low[index] as u64 | ((self.high[index] as u64) << 32)
    }

    #[inline]
    fn set(&mut self, index: usize, value: u64) {
        debug_assert!(value <= DFS_PACKED_MAX);
        self.low[index] = value as u32;
        self.high[index] = (value >> 32) as u8;
    }

    #[inline]
    fn pop(&mut self) -> u64 {
        let index = self.len() - 1;
        let value = self.get(index);
        self.low.pop();
        self.high.pop();
        value
    }

    #[inline]
    fn first(&self) -> Option<u64> {
        (!self.is_empty()).then(|| self.get(0))
    }

    fn reverse(&mut self) {
        self.low.reverse();
        self.high.reverse();
    }
}

#[derive(Clone, Debug, Default)]
struct PackedU32Tail<const HIGH_BITS: usize> {
    low: Vec<u32>,
    high: Vec<u64>,
    tail: u64,
    tail_len: usize,
}

impl<const HIGH_BITS: usize> PackedU32Tail<HIGH_BITS> {
    const VALUES_PER_WORD: usize = 64 / HIGH_BITS;
    const HIGH_MASK: u64 = (1u64 << HIGH_BITS) - 1;
    const MAX_VALUE: u64 = (1u64 << (32 + HIGH_BITS)) - 1;

    #[inline]
    fn len(&self) -> usize {
        self.low.len()
    }

    #[cfg(test)]
    #[inline]
    fn get(&self, index: usize) -> u64 {
        let word = index / Self::VALUES_PER_WORD;
        let tail_start = self.low.len() - self.tail_len;
        let high = if index < tail_start {
            self.high[word]
        } else {
            self.tail
        };
        let shift = (index % Self::VALUES_PER_WORD) * HIGH_BITS;
        self.low[index] as u64 | ((high >> shift & Self::HIGH_MASK) << 32)
    }

    #[inline]
    fn last(&self) -> u64 {
        let high = if self.tail_len == 0 {
            *self.high.last().expect("packed tail is empty")
        } else {
            self.tail
        };
        let tail_len = if self.tail_len == 0 {
            Self::VALUES_PER_WORD
        } else {
            self.tail_len
        };
        let shift = (tail_len - 1) * HIGH_BITS;
        self.low.last().copied().expect("packed tail is empty") as u64
            | ((high >> shift & Self::HIGH_MASK) << 32)
    }

    #[inline]
    fn push(&mut self, value: u64) {
        debug_assert!(value <= Self::MAX_VALUE);
        self.low.push(value as u32);
        self.tail |= (value >> 32) << (self.tail_len * HIGH_BITS);
        self.tail_len += 1;
        if self.tail_len == Self::VALUES_PER_WORD {
            self.high.push(self.tail);
            self.tail = 0;
            self.tail_len = 0;
        }
    }

    #[inline]
    fn pop(&mut self) -> u64 {
        if self.tail_len == 0 {
            self.tail = self.high.pop().expect("packed tail is empty");
            self.tail_len = Self::VALUES_PER_WORD;
        }
        let shift = (self.tail_len - 1) * HIGH_BITS;
        let value = self.low.last().copied().expect("packed tail is empty") as u64
            | ((self.tail >> shift & Self::HIGH_MASK) << 32);
        self.tail &= !(Self::HIGH_MASK << shift);
        self.tail_len -= 1;
        self.low.pop();
        value
    }
}

fn uses_narrow_dfs(max_value: u64, length: usize, high_bits: usize) -> bool {
    let maximum = match high_bits {
        1 => U33_MAX,
        2 => U34_MAX,
        _ => return false,
    };
    if max_value > maximum {
        return false;
    }
    let values = length as u128;
    let values_per_word = (64 / high_bits) as u128;
    let narrow = values * 4 + (values + values_per_word - 1) / values_per_word * 8;
    values * 5 >= narrow && values * 5 - narrow >= NARROW_DFS_MIN_SAVINGS
}

#[derive(Clone, Debug)]
enum IdVec {
    Plain(Vec<u64>),
    Packed(PackedU40),
}

impl IdVec {
    fn new(packed: bool) -> Self {
        if packed {
            Self::Packed(PackedU40::default())
        } else {
            Self::Plain(Vec::new())
        }
    }

    fn with_capacity(packed: bool, capacity: usize) -> Self {
        if packed {
            Self::Packed(PackedU40::with_capacity(capacity))
        } else {
            Self::Plain(Vec::with_capacity(capacity))
        }
    }

    fn try_reserve(&mut self, additional: usize) {
        match self {
            Self::Plain(values) => {
                let _ = values.try_reserve(additional);
            }
            Self::Packed(values) => values.try_reserve(additional),
        }
    }

    #[inline]
    fn len(&self) -> usize {
        match self {
            Self::Plain(values) => values.len(),
            Self::Packed(values) => values.len(),
        }
    }

    #[inline]
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline]
    fn capacity(&self) -> usize {
        match self {
            Self::Plain(values) => values.capacity(),
            Self::Packed(values) => values.capacity(),
        }
    }

    #[inline]
    fn push(&mut self, value: u64) {
        match self {
            Self::Plain(values) => values.push(value),
            Self::Packed(values) => values.push(value),
        }
    }

    #[inline]
    fn get(&self, index: usize) -> u64 {
        match self {
            Self::Plain(values) => values[index],
            Self::Packed(values) => values.get(index),
        }
    }

    #[inline]
    fn pop(&mut self) -> Option<u64> {
        match self {
            Self::Plain(values) => values.pop(),
            Self::Packed(values) => (!values.is_empty()).then(|| values.pop()),
        }
    }

    #[inline]
    fn first(&self) -> Option<u64> {
        match self {
            Self::Plain(values) => values.first().copied(),
            Self::Packed(values) => values.first(),
        }
    }

    fn reverse(&mut self) {
        match self {
            Self::Plain(values) => values.reverse(),
            Self::Packed(values) => values.reverse(),
        }
    }

    fn plain(&self) -> Option<&[u64]> {
        match self {
            Self::Plain(values) => Some(values),
            Self::Packed(_) => None,
        }
    }

    fn packed(&self) -> Option<(&[u32], &[u8])> {
        match self {
            Self::Plain(_) => None,
            Self::Packed(values) => Some((&values.low, &values.high)),
        }
    }
}

#[derive(Debug)]
enum DenseU40 {
    Plain(Vec<u64>),
    Packed(PackedU40),
}

impl DenseU40 {
    fn zeroed(len: usize) -> Self {
        if len as u64 <= DFS_PACKED_MAX {
            Self::Packed(PackedU40::with_len(len))
        } else {
            Self::Plain(vec![0; len])
        }
    }

    fn from_offsets(values: Vec<u64>) -> Self {
        if values.last().copied().unwrap_or(0) > DFS_PACKED_MAX {
            return Self::Plain(values);
        }
        let mut packed = PackedU40::with_len(values.len());
        for (index, value) in values.into_iter().enumerate() {
            packed.set(index, value);
        }
        Self::Packed(packed)
    }

    #[inline]
    fn get(&self, index: usize) -> u64 {
        match self {
            Self::Plain(values) => values[index],
            Self::Packed(values) => values.get(index),
        }
    }

    #[inline]
    fn set(&mut self, index: usize, value: u64) {
        match self {
            Self::Plain(values) => values[index] = value,
            Self::Packed(values) => values.set(index, value),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum EdgeColumn<'a> {
    Plain(&'a [u64]),
    Packed { low: &'a [u32], high: &'a [u8] },
}

impl EdgeColumn<'_> {
    #[inline]
    fn len(self) -> usize {
        match self {
            Self::Plain(values) => values.len(),
            Self::Packed { low, high } => low.len().min(high.len()),
        }
    }

    #[inline]
    fn get(self, index: usize) -> u64 {
        match self {
            Self::Plain(values) => values[index],
            Self::Packed { low, high } => low[index] as u64 | ((high[index] as u64) << 32),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WideBCTreeError {
    EdgeArrayLengthMismatch,
    NodeCountTooLarge,
    TooManyIncidences,
    EndpointOutOfRange { edge: u64, node: u64 },
}

#[derive(Debug)]
enum CsrColumn {
    Plain(Vec<u64>),
    Atomic32(Vec<AtomicU32>),
    AtomicPacked {
        low: Vec<AtomicU32>,
        high: Vec<AtomicU64>,
    },
}

impl CsrColumn {
    #[inline]
    fn len(&self) -> usize {
        match self {
            Self::Plain(values) => values.len(),
            Self::Atomic32(values) => values.len(),
            Self::AtomicPacked { low, .. } => low.len(),
        }
    }

    #[inline]
    fn get(&self, index: usize) -> u64 {
        match self {
            Self::Plain(values) => values[index],
            Self::Atomic32(values) => values[index].load(Ordering::Relaxed) as u64,
            Self::AtomicPacked { low, high } => {
                let value = low[index].load(Ordering::Relaxed) as u64;
                let high_bit = high[index / 64].load(Ordering::Relaxed) >> (index % 64) & 1;
                value | (high_bit << 32)
            }
        }
    }

    fn set_atomic(&self, index: usize, value: u64) {
        match self {
            Self::Plain(_) => unreachable!("plain CSR column is sorted directly"),
            Self::Atomic32(values) => values[index].store(value as u32, Ordering::Relaxed),
            Self::AtomicPacked { low, high } => {
                low[index].store(value as u32, Ordering::Relaxed);
                let mask = 1u64 << (index % 64);
                if value > u32::MAX as u64 {
                    high[index / 64].fetch_or(mask, Ordering::Relaxed);
                } else {
                    high[index / 64].fetch_and(!mask, Ordering::Relaxed);
                }
            }
        }
    }

    fn swap_atomic(&self, left: usize, right: usize) {
        if left == right {
            return;
        }
        let left_value = self.get(left);
        let right_value = self.get(right);
        self.set_atomic(left, right_value);
        self.set_atomic(right, left_value);
    }

    fn sort_adjacency(&mut self, offsets: &DenseU40, nodes: usize, workers: usize) {
        if let Self::Plain(values) = self {
            for node in 0..nodes {
                let begin = offsets.get(node) as usize;
                let end = offsets.get(node + 1) as usize;
                values[begin..end].sort_unstable();
            }
            return;
        }

        let column: &CsrColumn = self;
        let workers = workers.min(nodes).max(1);
        if workers == 1 {
            sort_atomic_adjacency_range(column, offsets, 0, nodes);
            return;
        }
        thread::scope(|scope| {
            for worker in 0..workers {
                let begin = nodes * worker / workers;
                let end = nodes * (worker + 1) / workers;
                scope.spawn(move || sort_atomic_adjacency_range(column, offsets, begin, end));
            }
        });
    }
}

fn sort_atomic_adjacency_range(column: &CsrColumn, offsets: &DenseU40, begin: usize, end: usize) {
    for node in begin..end {
        let first = offsets.get(node) as usize;
        let length = offsets.get(node + 1) as usize - first;
        heap_sort_atomic(column, first, length);
    }
}

fn heap_sort_atomic(column: &CsrColumn, begin: usize, length: usize) {
    if length < 2 {
        return;
    }
    for root in (0..length / 2).rev() {
        sift_down_atomic(column, begin, root, length);
    }
    for end in (1..length).rev() {
        column.swap_atomic(begin, begin + end);
        sift_down_atomic(column, begin, 0, end);
    }
}

fn sift_down_atomic(column: &CsrColumn, begin: usize, mut root: usize, length: usize) {
    loop {
        let left = root * 2 + 1;
        if left >= length {
            return;
        }
        let right = left + 1;
        let child = if right < length && column.get(begin + left) < column.get(begin + right) {
            right
        } else {
            left
        };
        if column.get(begin + root) >= column.get(begin + child) {
            return;
        }
        column.swap_atomic(begin + root, begin + child);
        root = child;
    }
}

#[derive(Debug)]
pub struct WideCsrGraph<'a> {
    num_nodes: u64,
    offsets: DenseU40,
    edge_ids: CsrColumn,
    src: EdgeColumn<'a>,
    dst: EdgeColumn<'a>,
}

impl<'a> WideCsrGraph<'a> {
    pub fn from_edge_arrays(
        num_nodes: u64,
        src: &'a [u64],
        dst: &'a [u64],
    ) -> Result<Self, WideBCTreeError> {
        let workers = if src.len() < PARALLEL_CSR_MIN_EDGES {
            1
        } else {
            csr_worker_count(src.len(), spqr_thread_count())
        };
        Self::from_columns(
            num_nodes,
            EdgeColumn::Plain(src),
            EdgeColumn::Plain(dst),
            workers,
        )
    }

    #[cfg(test)]
    fn from_edge_arrays_with_workers(
        num_nodes: u64,
        src: &'a [u64],
        dst: &'a [u64],
        workers: usize,
    ) -> Result<Self, WideBCTreeError> {
        Self::from_columns(
            num_nodes,
            EdgeColumn::Plain(src),
            EdgeColumn::Plain(dst),
            workers,
        )
    }

    pub(crate) fn from_packed_edge_arrays(
        num_nodes: u64,
        src_low: &'a [u32],
        src_high: &'a [u8],
        dst_low: &'a [u32],
        dst_high: &'a [u8],
        workers: usize,
    ) -> Result<Self, WideBCTreeError> {
        let workers = csr_worker_count(src_low.len(), workers);
        Self::from_columns(
            num_nodes,
            EdgeColumn::Packed {
                low: src_low,
                high: src_high,
            },
            EdgeColumn::Packed {
                low: dst_low,
                high: dst_high,
            },
            workers,
        )
    }

    fn from_edge_arrays_ordered(
        num_nodes: u64,
        src: &'a [u64],
        dst: &'a [u64],
    ) -> Result<Self, WideBCTreeError> {
        let workers = csr_worker_count(src.len(), spqr_thread_count());
        let mut graph = Self::from_columns(
            num_nodes,
            EdgeColumn::Plain(src),
            EdgeColumn::Plain(dst),
            workers,
        )?;
        graph.sort_adjacency_by_edge_id(workers);
        Ok(graph)
    }

    fn from_packed_edge_arrays_ordered(
        num_nodes: u64,
        src_low: &'a [u32],
        src_high: &'a [u8],
        dst_low: &'a [u32],
        dst_high: &'a [u8],
        workers: usize,
    ) -> Result<Self, WideBCTreeError> {
        let workers = csr_worker_count(src_low.len(), workers);
        let mut graph = Self::from_columns(
            num_nodes,
            EdgeColumn::Packed {
                low: src_low,
                high: src_high,
            },
            EdgeColumn::Packed {
                low: dst_low,
                high: dst_high,
            },
            workers,
        )?;
        graph.sort_adjacency_by_edge_id(workers);
        Ok(graph)
    }

    fn sort_adjacency_by_edge_id(&mut self, workers: usize) {
        self.edge_ids
            .sort_adjacency(&self.offsets, self.num_nodes as usize, workers);
    }

    fn from_columns(
        num_nodes: u64,
        src: EdgeColumn<'a>,
        dst: EdgeColumn<'a>,
        workers: usize,
    ) -> Result<Self, WideBCTreeError> {
        if src.len() != dst.len() {
            return Err(WideBCTreeError::EdgeArrayLengthMismatch);
        }

        let node_count =
            usize::try_from(num_nodes).map_err(|_| WideBCTreeError::NodeCountTooLarge)?;
        let workers = workers.min(src.len().max(1));
        if workers <= 1 {
            return Self::from_edge_arrays_serial(num_nodes, node_count, src, dst);
        }
        Self::from_edge_arrays_parallel(num_nodes, node_count, src, dst, workers)
    }

    fn from_edge_arrays_serial(
        num_nodes: u64,
        node_count: usize,
        src: EdgeColumn<'a>,
        dst: EdgeColumn<'a>,
    ) -> Result<Self, WideBCTreeError> {
        let offset_len = node_count
            .checked_add(1)
            .ok_or(WideBCTreeError::NodeCountTooLarge)?;
        let mut offsets = vec![0u64; offset_len];

        for edge_idx in 0..src.len() {
            let u = src.get(edge_idx);
            let v = dst.get(edge_idx);
            let edge = edge_idx as u64;
            let u_idx = checked_node_index(num_nodes, edge, u)?;
            let v_idx = checked_node_index(num_nodes, edge, v)?;
            if u_idx == v_idx {
                continue;
            }
            offsets[u_idx + 1] = offsets[u_idx + 1]
                .checked_add(1)
                .ok_or(WideBCTreeError::TooManyIncidences)?;
            offsets[v_idx + 1] = offsets[v_idx + 1]
                .checked_add(1)
                .ok_or(WideBCTreeError::TooManyIncidences)?;
        }

        for node in 1..offsets.len() {
            offsets[node] = offsets[node]
                .checked_add(offsets[node - 1])
                .ok_or(WideBCTreeError::TooManyIncidences)?;
        }

        let adjacency_len =
            usize::try_from(offsets[node_count]).map_err(|_| WideBCTreeError::TooManyIncidences)?;
        let mut edge_ids = vec![0u64; adjacency_len];
        let mut next = offsets[..node_count].to_vec();

        for edge_idx in 0..src.len() {
            let u = src.get(edge_idx);
            let v = dst.get(edge_idx);
            let edge = edge_idx as u64;
            let u_idx = checked_node_index(num_nodes, edge, u)?;
            let v_idx = checked_node_index(num_nodes, edge, v)?;
            if u_idx == v_idx {
                continue;
            }
            write_adjacency(&mut next, &mut edge_ids, u_idx, edge);
            write_adjacency(&mut next, &mut edge_ids, v_idx, edge);
        }
        drop(next);

        let offsets = DenseU40::from_offsets(offsets);
        Ok(Self {
            num_nodes,
            offsets,
            edge_ids: CsrColumn::Plain(edge_ids),
            src,
            dst,
        })
    }

    fn from_edge_arrays_parallel(
        num_nodes: u64,
        node_count: usize,
        src: EdgeColumn<'a>,
        dst: EdgeColumn<'a>,
        workers: usize,
    ) -> Result<Self, WideBCTreeError> {
        let counts: Vec<AtomicU32> = (0..node_count).map(|_| AtomicU32::new(0)).collect();
        let first_invalid = AtomicUsize::new(usize::MAX);
        let degree_overflow = AtomicUsize::new(0);
        let edge_chunk_len = src.len().div_ceil(workers);

        thread::scope(|scope| {
            for chunk_index in 0..workers {
                let edge_start = chunk_index * edge_chunk_len;
                let edge_end = (edge_start + edge_chunk_len).min(src.len());
                if edge_start == edge_end {
                    continue;
                }
                let src = src;
                let dst = dst;
                let counts = &counts;
                let first_invalid = &first_invalid;
                let degree_overflow = &degree_overflow;
                scope.spawn(move || {
                    for edge_index in edge_start..edge_end {
                        let u = src.get(edge_index);
                        let v = dst.get(edge_index);
                        if u >= num_nodes || v >= num_nodes {
                            first_invalid.fetch_min(edge_index, Ordering::Relaxed);
                            break;
                        }
                        if u == v {
                            continue;
                        }
                        if counts[u as usize].fetch_add(1, Ordering::Relaxed) == u32::MAX {
                            degree_overflow.store(1, Ordering::Relaxed);
                        }
                        if counts[v as usize].fetch_add(1, Ordering::Relaxed) == u32::MAX {
                            degree_overflow.store(1, Ordering::Relaxed);
                        }
                    }
                });
            }
        });

        let invalid_edge = first_invalid.load(Ordering::Relaxed);
        if invalid_edge != usize::MAX {
            return Err(invalid_endpoint_error(num_nodes, src, dst, invalid_edge));
        }
        if degree_overflow.load(Ordering::Relaxed) != 0 {
            drop(counts);
            return Self::from_edge_arrays_parallel_wide(num_nodes, node_count, src, dst, workers);
        }

        if src.len() as u64 > CSR_PACKED_MAX {
            drop(counts);
            return Self::from_edge_arrays_serial(num_nodes, node_count, src, dst);
        }
        let (offsets, adjacency_len) = prefix_counts_parallel_u32(&counts, workers)?;

        if src.len() <= u32::MAX as usize {
            let edge_ids: Vec<AtomicU32> = (0..adjacency_len).map(|_| AtomicU32::new(0)).collect();
            thread::scope(|scope| {
                for chunk_index in 0..workers {
                    let edge_start = chunk_index * edge_chunk_len;
                    let edge_end = (edge_start + edge_chunk_len).min(src.len());
                    if edge_start == edge_end {
                        continue;
                    }
                    let src = src;
                    let dst = dst;
                    let counts = &counts;
                    let offsets = &offsets;
                    let edge_ids = &edge_ids;
                    scope.spawn(move || {
                        for edge_index in edge_start..edge_end {
                            let u = src.get(edge_index);
                            let v = dst.get(edge_index);
                            if u == v {
                                continue;
                            }
                            let edge = edge_index as u64;
                            write_atomic_adjacency_u32(counts, offsets, edge_ids, u as usize, edge);
                            write_atomic_adjacency_u32(counts, offsets, edge_ids, v as usize, edge);
                        }
                    });
                }
            });
            drop(counts);
            return Ok(Self {
                num_nodes,
                offsets: DenseU40::Packed(offsets),
                edge_ids: CsrColumn::Atomic32(edge_ids),
                src,
                dst,
            });
        }

        let edge_ids: Vec<AtomicU32> = (0..adjacency_len).map(|_| AtomicU32::new(0)).collect();
        let high: Vec<AtomicU64> = (0..adjacency_len.div_ceil(64))
            .map(|_| AtomicU64::new(0))
            .collect();
        thread::scope(|scope| {
            for chunk_index in 0..workers {
                let edge_start = chunk_index * edge_chunk_len;
                let edge_end = (edge_start + edge_chunk_len).min(src.len());
                if edge_start == edge_end {
                    continue;
                }
                let src = src;
                let dst = dst;
                let counts = &counts;
                let offsets = &offsets;
                let edge_ids = &edge_ids;
                let high = &high;
                scope.spawn(move || {
                    for edge_index in edge_start..edge_end {
                        let u = src.get(edge_index);
                        let v = dst.get(edge_index);
                        if u == v {
                            continue;
                        }
                        let edge = edge_index as u64;
                        write_atomic_adjacency_u32_packed(
                            counts, offsets, edge_ids, high, u as usize, edge,
                        );
                        write_atomic_adjacency_u32_packed(
                            counts, offsets, edge_ids, high, v as usize, edge,
                        );
                    }
                });
            }
        });
        drop(counts);
        Ok(Self {
            num_nodes,
            offsets: DenseU40::Packed(offsets),
            edge_ids: CsrColumn::AtomicPacked {
                low: edge_ids,
                high,
            },
            src,
            dst,
        })
    }

    fn from_edge_arrays_parallel_wide(
        num_nodes: u64,
        node_count: usize,
        src: EdgeColumn<'a>,
        dst: EdgeColumn<'a>,
        workers: usize,
    ) -> Result<Self, WideBCTreeError> {
        let counts: Vec<AtomicU64> = (0..node_count).map(|_| AtomicU64::new(0)).collect();
        let edge_chunk_len = src.len().div_ceil(workers);

        thread::scope(|scope| {
            for chunk_index in 0..workers {
                let edge_start = chunk_index * edge_chunk_len;
                let edge_end = (edge_start + edge_chunk_len).min(src.len());
                if edge_start == edge_end {
                    continue;
                }
                let src = src;
                let dst = dst;
                let counts = &counts;
                scope.spawn(move || {
                    for edge_index in edge_start..edge_end {
                        let u = src.get(edge_index);
                        let v = dst.get(edge_index);
                        if u == v {
                            continue;
                        }
                        counts[u as usize].fetch_add(1, Ordering::Relaxed);
                        counts[v as usize].fetch_add(1, Ordering::Relaxed);
                    }
                });
            }
        });

        let (offsets, adjacency_len) = prefix_counts_parallel(&counts, workers)?;
        if src.len() as u64 > CSR_PACKED_MAX {
            drop(counts);
            return Self::from_edge_arrays_serial(num_nodes, node_count, src, dst);
        }
        if src.len() <= u32::MAX as usize {
            let edge_ids: Vec<AtomicU32> = (0..adjacency_len).map(|_| AtomicU32::new(0)).collect();
            thread::scope(|scope| {
                for chunk_index in 0..workers {
                    let edge_start = chunk_index * edge_chunk_len;
                    let edge_end = (edge_start + edge_chunk_len).min(src.len());
                    if edge_start == edge_end {
                        continue;
                    }
                    let src = src;
                    let dst = dst;
                    let counts = &counts;
                    let edge_ids = &edge_ids;
                    scope.spawn(move || {
                        for edge_index in edge_start..edge_end {
                            let u = src.get(edge_index);
                            let v = dst.get(edge_index);
                            if u == v {
                                continue;
                            }
                            let edge = edge_index as u64;
                            write_atomic_adjacency(counts, edge_ids, u as usize, edge);
                            write_atomic_adjacency(counts, edge_ids, v as usize, edge);
                        }
                    });
                }
            });
            drop(counts);
            let offsets = DenseU40::from_offsets(offsets);
            return Ok(Self {
                num_nodes,
                offsets,
                edge_ids: CsrColumn::Atomic32(edge_ids),
                src,
                dst,
            });
        }

        let edge_ids: Vec<AtomicU32> = (0..adjacency_len).map(|_| AtomicU32::new(0)).collect();
        let high: Vec<AtomicU64> = (0..adjacency_len.div_ceil(64))
            .map(|_| AtomicU64::new(0))
            .collect();
        thread::scope(|scope| {
            for chunk_index in 0..workers {
                let edge_start = chunk_index * edge_chunk_len;
                let edge_end = (edge_start + edge_chunk_len).min(src.len());
                if edge_start == edge_end {
                    continue;
                }
                let src = src;
                let dst = dst;
                let counts = &counts;
                let edge_ids = &edge_ids;
                let high = &high;
                scope.spawn(move || {
                    for edge_index in edge_start..edge_end {
                        let u = src.get(edge_index);
                        let v = dst.get(edge_index);
                        if u == v {
                            continue;
                        }
                        let edge = edge_index as u64;
                        write_atomic_adjacency_packed(counts, edge_ids, high, u as usize, edge);
                        write_atomic_adjacency_packed(counts, edge_ids, high, v as usize, edge);
                    }
                });
            }
        });
        drop(counts);
        let offsets = DenseU40::from_offsets(offsets);
        Ok(Self {
            num_nodes,
            offsets,
            edge_ids: CsrColumn::AtomicPacked {
                low: edge_ids,
                high,
            },
            src,
            dst,
        })
    }

    #[inline]
    pub fn num_nodes(&self) -> u64 {
        self.num_nodes
    }

    #[inline]
    pub fn num_edges(&self) -> u64 {
        self.src.len() as u64
    }

    #[inline]
    pub(crate) fn adjacency_range(&self, node: usize) -> Range<usize> {
        let start = usize::try_from(self.offsets.get(node)).expect("CSR offset exceeds usize");
        let end = usize::try_from(self.offsets.get(node + 1)).expect("CSR offset exceeds usize");
        start..end
    }

    #[inline]
    pub(crate) fn adjacency_at(&self, node: usize, index: usize) -> (u64, u64) {
        let edge = self.edge_ids.get(index);
        let (source, target) = self.edge_endpoints(edge);
        let node = node as u64;
        (if source == node { target } else { source }, edge)
    }

    #[inline]
    pub(crate) fn edge_endpoints(&self, edge: u64) -> (u64, u64) {
        let index = usize::try_from(edge).expect("edge ID exceeds usize");
        (self.src.get(index), self.dst.get(index))
    }

    #[inline]
    fn non_loop_edge_count(&self) -> usize {
        self.edge_ids.len() / 2
    }
}

fn csr_worker_count(edge_count: usize, configured_workers: usize) -> usize {
    if edge_count < PARALLEL_CSR_MIN_EDGES {
        return 1;
    }
    configured_workers
        .min(edge_count.div_ceil(PARALLEL_CSR_EDGES_PER_WORKER))
        .max(1)
}

fn invalid_endpoint_error(
    num_nodes: u64,
    src: EdgeColumn<'_>,
    dst: EdgeColumn<'_>,
    edge: usize,
) -> WideBCTreeError {
    let node = if src.get(edge) >= num_nodes {
        src.get(edge)
    } else {
        dst.get(edge)
    };
    WideBCTreeError::EndpointOutOfRange {
        edge: edge as u64,
        node,
    }
}

fn prefix_counts_parallel(
    counts: &[AtomicU64],
    workers: usize,
) -> Result<(Vec<u64>, usize), WideBCTreeError> {
    let node_count = counts.len();
    let offset_len = node_count
        .checked_add(1)
        .ok_or(WideBCTreeError::NodeCountTooLarge)?;
    let mut offsets = vec![0u64; offset_len];
    if node_count == 0 {
        return Ok((offsets, 0));
    }

    let workers = workers.min(node_count);
    let chunk_len = node_count.div_ceil(workers);
    let mut partials = Vec::with_capacity(workers);
    thread::scope(|scope| {
        let mut handles = Vec::with_capacity(workers);
        for (chunk_index, offset_chunk) in offsets[..node_count].chunks_mut(chunk_len).enumerate() {
            let count_start = chunk_index * chunk_len;
            let count_chunk = &counts[count_start..count_start + offset_chunk.len()];
            handles.push(scope.spawn(move || {
                let mut total = 0u64;
                for (offset, count) in offset_chunk.iter_mut().zip(count_chunk) {
                    let degree = count.load(Ordering::Relaxed);
                    *offset = total;
                    total = total
                        .checked_add(degree)
                        .ok_or(WideBCTreeError::TooManyIncidences)?;
                }
                Ok::<_, WideBCTreeError>(total)
            }));
        }
        for handle in handles {
            partials.push(handle.join().expect("CSR prefix worker panicked")?);
        }
        Ok::<_, WideBCTreeError>(())
    })?;

    let mut bases = Vec::with_capacity(partials.len());
    let mut total = 0u64;
    for chunk_total in partials {
        bases.push(total);
        total = total
            .checked_add(chunk_total)
            .ok_or(WideBCTreeError::TooManyIncidences)?;
    }

    thread::scope(|scope| {
        for ((offset_chunk, count_chunk), base) in offsets[..node_count]
            .chunks_mut(chunk_len)
            .zip(counts.chunks(chunk_len))
            .zip(bases)
        {
            scope.spawn(move || {
                for (offset, count) in offset_chunk.iter_mut().zip(count_chunk) {
                    *offset += base;
                    count.store(*offset, Ordering::Relaxed);
                }
            });
        }
    });
    offsets[node_count] = total;

    let adjacency_len = usize::try_from(total).map_err(|_| WideBCTreeError::TooManyIncidences)?;
    Ok((offsets, adjacency_len))
}

fn prefix_counts_parallel_u32(
    counts: &[AtomicU32],
    workers: usize,
) -> Result<(PackedU40, usize), WideBCTreeError> {
    let node_count = counts.len();
    let offset_len = node_count
        .checked_add(1)
        .ok_or(WideBCTreeError::NodeCountTooLarge)?;
    let mut offsets = PackedU40::with_len(offset_len);
    if node_count == 0 {
        return Ok((offsets, 0));
    }

    let workers = workers.min(node_count).max(1);
    let chunk_len = node_count.div_ceil(workers);
    let mut partials = Vec::with_capacity(workers);
    thread::scope(|scope| {
        let mut handles = Vec::with_capacity(workers);
        for (chunk_index, (low_chunk, high_chunk)) in offsets.low[..node_count]
            .chunks_mut(chunk_len)
            .zip(offsets.high[..node_count].chunks_mut(chunk_len))
            .enumerate()
        {
            let count_start = chunk_index * chunk_len;
            let count_chunk = &counts[count_start..count_start + low_chunk.len()];
            handles.push(scope.spawn(move || {
                let mut total = 0u64;
                for ((low, high), count) in low_chunk.iter_mut().zip(high_chunk).zip(count_chunk) {
                    *low = total as u32;
                    *high = (total >> 32) as u8;
                    total = total
                        .checked_add(u64::from(count.load(Ordering::Relaxed)))
                        .ok_or(WideBCTreeError::TooManyIncidences)?;
                }
                Ok::<_, WideBCTreeError>(total)
            }));
        }
        for handle in handles {
            partials.push(handle.join().expect("CSR prefix worker panicked")?);
        }
        Ok::<_, WideBCTreeError>(())
    })?;

    let mut bases = Vec::with_capacity(partials.len());
    let mut total = 0u64;
    for chunk_total in partials {
        bases.push(total);
        total = total
            .checked_add(chunk_total)
            .ok_or(WideBCTreeError::TooManyIncidences)?;
    }

    thread::scope(|scope| {
        for ((low_chunk, high_chunk), base) in offsets.low[..node_count]
            .chunks_mut(chunk_len)
            .zip(offsets.high[..node_count].chunks_mut(chunk_len))
            .zip(bases)
        {
            scope.spawn(move || {
                for (low, high) in low_chunk.iter_mut().zip(high_chunk) {
                    let offset = u64::from(*low) | (u64::from(*high) << 32);
                    let offset = offset + base;
                    *low = offset as u32;
                    *high = (offset >> 32) as u8;
                }
            });
        }
    });
    offsets.set(node_count, total);

    let adjacency_len = usize::try_from(total).map_err(|_| WideBCTreeError::TooManyIncidences)?;
    Ok((offsets, adjacency_len))
}

fn checked_node_index(num_nodes: u64, edge: u64, node: u64) -> Result<usize, WideBCTreeError> {
    if node >= num_nodes {
        return Err(WideBCTreeError::EndpointOutOfRange { edge, node });
    }
    usize::try_from(node).map_err(|_| WideBCTreeError::NodeCountTooLarge)
}

fn write_adjacency(next: &mut [u64], edge_ids: &mut [u64], node: usize, edge: u64) {
    let index = usize::try_from(next[node]).expect("CSR offset exceeds usize");
    edge_ids[index] = edge;
    next[node] += 1;
}

fn write_atomic_adjacency(cursors: &[AtomicU64], edge_ids: &[AtomicU32], node: usize, edge: u64) {
    let index = usize::try_from(cursors[node].fetch_add(1, Ordering::Relaxed))
        .expect("CSR offset exceeds usize");
    edge_ids[index].store(edge as u32, Ordering::Relaxed);
}

fn write_atomic_adjacency_u32(
    cursors: &[AtomicU32],
    offsets: &PackedU40,
    edge_ids: &[AtomicU32],
    node: usize,
    edge: u64,
) {
    let remaining = cursors[node].fetch_sub(1, Ordering::Relaxed);
    debug_assert!(remaining != 0);
    let index = usize::try_from(offsets.get(node) + u64::from(remaining - 1))
        .expect("CSR offset exceeds usize");
    edge_ids[index].store(edge as u32, Ordering::Relaxed);
}

fn write_atomic_adjacency_packed(
    cursors: &[AtomicU64],
    edge_ids: &[AtomicU32],
    high: &[AtomicU64],
    node: usize,
    edge: u64,
) {
    let index = usize::try_from(cursors[node].fetch_add(1, Ordering::Relaxed))
        .expect("CSR offset exceeds usize");
    edge_ids[index].store(edge as u32, Ordering::Relaxed);
    if edge > u32::MAX as u64 {
        high[index / 64].fetch_or(1u64 << (index % 64), Ordering::Relaxed);
    }
}

fn write_atomic_adjacency_u32_packed(
    cursors: &[AtomicU32],
    offsets: &PackedU40,
    edge_ids: &[AtomicU32],
    high: &[AtomicU64],
    node: usize,
    edge: u64,
) {
    let remaining = cursors[node].fetch_sub(1, Ordering::Relaxed);
    debug_assert!(remaining != 0);
    let index = usize::try_from(offsets.get(node) + u64::from(remaining - 1))
        .expect("CSR offset exceeds usize");
    edge_ids[index].store(edge as u32, Ordering::Relaxed);
    if edge > u32::MAX as u64 {
        high[index / 64].fetch_or(1u64 << (index % 64), Ordering::Relaxed);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct WideBlock {
    pub node_start: u64,
    pub node_count: u64,
    pub edge_start: u64,
    pub edge_count: u64,
}

#[derive(Clone, Debug)]
pub struct WideBCTree {
    blocks: Vec<WideBlock>,
    block_nodes_flat: Option<IdVec>,
    identity_node_prefix: u64,
    block_edges_flat: IdVec,
    identity_edge_prefix: u64,
    cut_vertices: Vec<u64>,
    is_cut: Vec<u64>,
    num_nodes: u64,
    pub num_components: u64,
}

impl WideBCTree {
    pub fn build(graph: &WideCsrGraph<'_>) -> Self {
        Self::build_with_storage(graph, false, true, false, false)
    }

    fn build_with_storage(
        graph: &WideCsrGraph<'_>,
        packed: bool,
        store_block_nodes: bool,
        preserve_edge_order: bool,
        skip_bridges: bool,
    ) -> Self {
        if graph.num_nodes == 0 {
            return Self {
                blocks: Vec::new(),
                block_nodes_flat: store_block_nodes.then(|| IdVec::new(packed)),
                identity_node_prefix: 0,
                block_edges_flat: IdVec::new(packed),
                identity_edge_prefix: 0,
                cut_vertices: Vec::new(),
                is_cut: Vec::new(),
                num_nodes: 0,
                num_components: 0,
            };
        }

        let mut builder = WideBCTreeBuilder::new(
            graph,
            packed,
            store_block_nodes,
            preserve_edge_order,
            skip_bridges,
        );
        builder.build();
        builder.into_tree()
    }

    pub fn from_edge_arrays(
        num_nodes: u64,
        src: &[u64],
        dst: &[u64],
    ) -> Result<Self, WideBCTreeError> {
        let graph = WideCsrGraph::from_edge_arrays(num_nodes, src, dst)?;
        Ok(Self::build(&graph))
    }

    pub(crate) fn from_edge_arrays_ordered(
        num_nodes: u64,
        src: &[u64],
        dst: &[u64],
    ) -> Result<Self, WideBCTreeError> {
        let graph = WideCsrGraph::from_edge_arrays_ordered(num_nodes, src, dst)?;
        Ok(Self::build_with_storage(&graph, false, true, true, false))
    }

    pub(crate) fn from_packed_edge_arrays(
        num_nodes: u64,
        src_low: &[u32],
        src_high: &[u8],
        dst_low: &[u32],
        dst_high: &[u8],
        workers: usize,
    ) -> Result<Self, WideBCTreeError> {
        let graph = WideCsrGraph::from_packed_edge_arrays(
            num_nodes, src_low, src_high, dst_low, dst_high, workers,
        )?;
        let packed = num_nodes <= DFS_PACKED_MAX && graph.num_edges() <= DFS_PACKED_MAX;
        Ok(Self::build_with_storage(&graph, packed, true, false, false))
    }

    pub(crate) fn from_packed_edge_arrays_ordered(
        num_nodes: u64,
        src_low: &[u32],
        src_high: &[u8],
        dst_low: &[u32],
        dst_high: &[u8],
        workers: usize,
    ) -> Result<Self, WideBCTreeError> {
        let graph = WideCsrGraph::from_packed_edge_arrays_ordered(
            num_nodes, src_low, src_high, dst_low, dst_high, workers,
        )?;
        let packed = num_nodes <= DFS_PACKED_MAX && graph.num_edges() <= DFS_PACKED_MAX;
        Ok(Self::build_with_storage(&graph, packed, true, true, false))
    }

    pub(crate) fn from_packed_edge_arrays_compact(
        num_nodes: u64,
        src_low: &[u32],
        src_high: &[u8],
        dst_low: &[u32],
        dst_high: &[u8],
        workers: usize,
    ) -> Result<Self, WideBCTreeError> {
        let graph = WideCsrGraph::from_packed_edge_arrays(
            num_nodes, src_low, src_high, dst_low, dst_high, workers,
        )?;
        let packed = num_nodes <= DFS_PACKED_MAX && graph.num_edges() <= DFS_PACKED_MAX;
        Ok(Self::build_with_storage(
            &graph, packed, false, false, false,
        ))
    }

    pub(crate) fn from_edge_arrays_cyclic(
        num_nodes: u64,
        src: &[u64],
        dst: &[u64],
    ) -> Result<Self, WideBCTreeError> {
        let graph = WideCsrGraph::from_edge_arrays(num_nodes, src, dst)?;
        Ok(Self::build_with_storage(&graph, false, true, false, true))
    }

    pub(crate) fn from_packed_edge_arrays_cyclic_compact(
        num_nodes: u64,
        src_low: &[u32],
        src_high: &[u8],
        dst_low: &[u32],
        dst_high: &[u8],
        workers: usize,
    ) -> Result<Self, WideBCTreeError> {
        let graph = WideCsrGraph::from_packed_edge_arrays(
            num_nodes, src_low, src_high, dst_low, dst_high, workers,
        )?;
        let packed = num_nodes <= DFS_PACKED_MAX && graph.num_edges() <= DFS_PACKED_MAX;
        Ok(Self::build_with_storage(&graph, packed, false, false, true))
    }

    #[inline]
    pub fn num_blocks(&self) -> usize {
        self.blocks.len()
    }

    #[inline]
    pub fn num_cut_vertices(&self) -> usize {
        self.cut_vertices.len()
    }

    #[inline]
    pub fn is_biconnected(&self) -> bool {
        self.num_components == 1 && self.blocks.len() == 1 && self.cut_vertices.is_empty()
    }

    #[inline]
    pub fn is_cut_vertex(&self, node: u64) -> bool {
        let Ok(node) = usize::try_from(node) else {
            return false;
        };
        if node >= self.num_nodes as usize {
            return false;
        }
        let word = node / 64;
        let bit = node % 64;
        (self.is_cut[word] & (1u64 << bit)) != 0
    }

    #[inline]
    pub fn block(&self, index: usize) -> &WideBlock {
        &self.blocks[index]
    }

    #[inline]
    pub fn block_nodes(&self, index: usize) -> &[u64] {
        let block = self.block(index);
        let start = block.node_start as usize;
        let end = start + block.node_count as usize;
        &self
            .block_nodes_flat
            .as_ref()
            .expect("compact BC-tree does not store block nodes")
            .plain()
            .expect("packed BC-tree nodes do not have a u64 slice")[start..end]
    }

    #[inline]
    pub fn block_edges(&self, index: usize) -> &[u64] {
        let block = self.block(index);
        let start = block.edge_start as usize;
        let end = start + block.edge_count as usize;
        &self
            .block_edges_flat
            .plain()
            .expect("packed BC-tree edges do not have a u64 slice")[start..end]
    }

    #[inline]
    pub fn cut_vertices(&self) -> &[u64] {
        &self.cut_vertices
    }

    #[inline]
    pub fn total_block_nodes(&self) -> u64 {
        self.blocks
            .last()
            .map_or(0, |block| block.node_start + block.node_count)
    }

    #[inline]
    pub fn total_block_edges(&self) -> u64 {
        self.identity_edge_prefix + self.block_edges_flat.len() as u64
    }

    #[inline]
    pub fn block_node_offset(&self, index: usize) -> u64 {
        if index == self.blocks.len() {
            self.total_block_nodes()
        } else {
            self.blocks[index].node_start
        }
    }

    #[inline]
    pub fn block_edge_offset(&self, index: usize) -> u64 {
        if index == self.blocks.len() {
            self.total_block_edges()
        } else {
            self.blocks[index].edge_start
        }
    }

    #[inline]
    pub fn nodes_flat(&self) -> &[u64] {
        self.block_nodes_flat
            .as_ref()
            .expect("compact BC-tree does not store block nodes")
            .plain()
            .expect("packed BC-tree nodes do not have a u64 slice")
    }

    #[inline]
    pub fn edges_flat(&self) -> &[u64] {
        self.block_edges_flat
            .plain()
            .expect("packed BC-tree edges do not have a u64 slice")
    }

    #[inline]
    pub(crate) fn node_at(&self, index: usize) -> u64 {
        let index = index as u64;
        if index < self.identity_node_prefix {
            index
        } else {
            self.block_nodes_flat
                .as_ref()
                .expect("compact BC-tree does not store block nodes")
                .get((index - self.identity_node_prefix) as usize)
        }
    }

    #[inline]
    pub(crate) fn edge_at(&self, index: usize) -> u64 {
        let index = index as u64;
        if index < self.identity_edge_prefix {
            index
        } else {
            self.block_edges_flat
                .get((index - self.identity_edge_prefix) as usize)
        }
    }

    pub(crate) fn nodes_flat_u64(&self) -> Option<&[u64]> {
        let nodes = self.block_nodes_flat.as_ref()?;
        (self.identity_node_prefix == 0)
            .then(|| nodes.plain())
            .flatten()
    }

    #[inline]
    pub(crate) fn stores_block_nodes(&self) -> bool {
        self.block_nodes_flat.is_some()
    }

    pub(crate) fn edges_flat_u64(&self) -> Option<&[u64]> {
        (self.identity_edge_prefix == 0)
            .then(|| self.block_edges_flat.plain())
            .flatten()
    }

    pub(crate) fn nodes_flat_u40(&self) -> Option<(&[u32], &[u8])> {
        self.block_nodes_flat.as_ref()?.packed()
    }

    pub(crate) fn identity_node_prefix(&self) -> u64 {
        self.identity_node_prefix
    }

    pub(crate) fn identity_edge_prefix(&self) -> u64 {
        self.identity_edge_prefix
    }

    pub(crate) fn edges_flat_u40(&self) -> Option<(&[u32], &[u8])> {
        self.block_edges_flat.packed()
    }
}

#[derive(Clone, Copy)]
struct DfsFrame {
    node: usize,
    next: usize,
    parent_edge: u64,
    low: u64,
}

#[derive(Clone, Copy)]
struct DfsAncestor {
    next: usize,
    low: u64,
}

enum DfsStack {
    Plain(Vec<DfsAncestor>),
    PackedU40 {
        next: PackedU40,
        low: PackedU40,
    },
    PackedU33 {
        next: PackedU32Tail<1>,
        low: PackedU32Tail<1>,
    },
    PackedU34 {
        next: PackedU32Tail<2>,
        low: PackedU32Tail<1>,
    },
}

impl DfsStack {
    fn new(graph: &WideCsrGraph<'_>) -> Self {
        let packed =
            graph.num_nodes <= DFS_PACKED_MAX && graph.edge_ids.len() as u64 <= DFS_PACKED_MAX;
        if !packed {
            return Self::Plain(Vec::new());
        }
        let maximum_length = graph.num_nodes as usize;
        if uses_narrow_dfs(graph.num_nodes, maximum_length, 1) {
            if uses_narrow_dfs(graph.edge_ids.len() as u64, maximum_length, 1) {
                return Self::PackedU33 {
                    next: PackedU32Tail::default(),
                    low: PackedU32Tail::default(),
                };
            }
            if uses_narrow_dfs(graph.edge_ids.len() as u64, maximum_length, 2) {
                return Self::PackedU34 {
                    next: PackedU32Tail::default(),
                    low: PackedU32Tail::default(),
                };
            }
        }
        Self::PackedU40 {
            next: PackedU40::default(),
            low: PackedU40::default(),
        }
    }

    #[inline]
    fn is_empty(&self) -> bool {
        match self {
            Self::Plain(values) => values.is_empty(),
            Self::PackedU40 { next, .. } => next.is_empty(),
            Self::PackedU33 { next, .. } => next.len() == 0,
            Self::PackedU34 { next, .. } => next.len() == 0,
        }
    }

    #[inline]
    fn last_next(&self) -> usize {
        match self {
            Self::Plain(values) => values.last().expect("DFS stack is empty").next,
            Self::PackedU40 { next, .. } => next.get(next.len() - 1) as usize,
            Self::PackedU33 { next, .. } => next.last() as usize,
            Self::PackedU34 { next, .. } => next.last() as usize,
        }
    }

    #[inline]
    fn push(&mut self, frame: DfsAncestor) {
        match self {
            Self::Plain(values) => values.push(frame),
            Self::PackedU40 { next, low } => {
                next.push(frame.next as u64);
                low.push(frame.low);
            }
            Self::PackedU33 { next, low } => {
                next.push(frame.next as u64);
                low.push(frame.low);
            }
            Self::PackedU34 { next, low } => {
                next.push(frame.next as u64);
                low.push(frame.low);
            }
        }
    }

    #[inline]
    fn pop(&mut self) -> DfsAncestor {
        match self {
            Self::Plain(values) => values.pop().expect("DFS stack is empty"),
            Self::PackedU40 { next, low } => DfsAncestor {
                next: next.pop() as usize,
                low: low.pop(),
            },
            Self::PackedU33 { next, low } => DfsAncestor {
                next: next.pop() as usize,
                low: low.pop(),
            },
            Self::PackedU34 { next, low } => DfsAncestor {
                next: next.pop() as usize,
                low: low.pop(),
            },
        }
    }
}

struct WideBCTreeBuilder<'graph, 'edges> {
    graph: &'graph WideCsrGraph<'edges>,
    disc: DenseU40,
    time: u64,
    edge_stack: IdVec,
    blocks: Vec<WideBlock>,
    block_nodes_flat: Option<IdVec>,
    pending_block_nodes: u64,
    identity_node_prefix: u64,
    block_edges_flat: IdVec,
    identity_edge_prefix: u64,
    is_cut: Vec<u64>,
    node_marks: Vec<u64>,
    num_components: u64,
    packed: bool,
    preserve_edge_order: bool,
    skip_bridges: bool,
}

impl<'graph, 'edges> WideBCTreeBuilder<'graph, 'edges> {
    fn new(
        graph: &'graph WideCsrGraph<'edges>,
        packed: bool,
        store_block_nodes: bool,
        preserve_edge_order: bool,
        skip_bridges: bool,
    ) -> Self {
        let node_count = graph.num_nodes as usize;
        let edge_count = graph.num_edges() as usize;
        let blocks = Vec::new();
        let mut block_nodes_flat = store_block_nodes.then(|| IdVec::new(packed));
        let mut block_edges_flat = IdVec::new(packed);
        if let Some(nodes) = block_nodes_flat.as_mut() {
            nodes.try_reserve(
                node_count
                    .saturating_add(edge_count)
                    .min(edge_count.saturating_mul(2)),
            );
        }
        block_edges_flat.try_reserve(edge_count);
        Self {
            graph,
            disc: DenseU40::zeroed(node_count),
            time: 1,
            edge_stack: IdVec::with_capacity(packed, graph.non_loop_edge_count().min(1 << 20)),
            blocks,
            block_nodes_flat,
            pending_block_nodes: 0,
            identity_node_prefix: 0,
            block_edges_flat,
            identity_edge_prefix: 0,
            is_cut: vec![0; node_count.div_ceil(64)],
            node_marks: vec![0; node_count.div_ceil(64)],
            num_components: 0,
            packed,
            preserve_edge_order,
            skip_bridges,
        }
    }

    fn build(&mut self) {
        for root in 0..self.graph.num_nodes as usize {
            if self.disc.get(root) != 0 {
                continue;
            }
            self.num_components += 1;
            let visited_all_nodes = self.dfs_iterative(root);
            debug_assert!(self.edge_stack.is_empty());
            if visited_all_nodes {
                break;
            }
            if self.edge_stack.capacity() > 1 << 20 {
                self.edge_stack = IdVec::with_capacity(self.packed, 1 << 20);
            }
        }
        self.disc = DenseU40::Plain(Vec::new());
        self.node_marks = Vec::new();
        self.append_self_loop_blocks();
    }

    fn dfs_iterative(&mut self, root: usize) -> bool {
        self.visit(root);
        let range = self.graph.adjacency_range(root);
        let mut end = range.end;
        let mut stack = DfsStack::new(self.graph);
        let mut frame = DfsFrame {
            node: root,
            next: range.start,
            parent_edge: u64::MAX,
            low: self.disc.get(root),
        };
        let mut root_children = 0u64;

        loop {
            if frame.next < end {
                let u = frame.node;
                let parent_edge = frame.parent_edge;
                let (v, edge) = self.graph.adjacency_at(u, frame.next);
                frame.next += 1;
                let v = v as usize;

                if u == v {
                    continue;
                }
                let disc_v = self.disc.get(v);
                if disc_v == 0 {
                    self.edge_stack.push(edge);
                    self.visit(v);
                    if parent_edge == u64::MAX {
                        root_children += 1;
                    }
                    stack.push(DfsAncestor {
                        next: frame.next,
                        low: frame.low,
                    });
                    let range = self.graph.adjacency_range(v);
                    frame = DfsFrame {
                        node: v,
                        next: range.start,
                        parent_edge: edge,
                        low: self.disc.get(v),
                    };
                    end = range.end;
                } else if edge != parent_edge && disc_v < self.disc.get(u) {
                    frame.low = frame.low.min(disc_v);
                    self.edge_stack.push(edge);
                }
                continue;
            }

            let u = frame.node;
            if frame.parent_edge == u64::MAX {
                if root_children > 1 {
                    self.mark_cut(u);
                }
                break;
            }

            let child_edge = frame.parent_edge;
            let (edge_u, edge_v) = self.graph.edge_endpoints(child_edge);
            let parent = if edge_u as usize == u {
                edge_v as usize
            } else {
                debug_assert_eq!(edge_v as usize, u);
                edge_u as usize
            };
            let parent_frame = stack.pop();
            let parent_edge = if stack.is_empty() {
                u64::MAX
            } else {
                self.graph.edge_ids.get(stack.last_next() - 1)
            };
            let parent_is_root = parent_edge == u64::MAX;
            let low = frame.low;
            let separates_block = low >= self.disc.get(parent);
            frame = DfsFrame {
                node: parent,
                next: parent_frame.next,
                parent_edge,
                low: parent_frame.low.min(low),
            };
            end = usize::try_from(self.graph.offsets.get(parent + 1))
                .expect("CSR offset exceeds usize");
            if separates_block {
                let can_finish_with_one_block = parent_is_root
                    && root_children == 1
                    && self.num_components == 1
                    && self.blocks.is_empty()
                    && self.block_edges_flat.is_empty()
                    && self.edge_stack.first() == Some(child_edge)
                    && self.time.checked_sub(1) == Some(self.graph.num_nodes)
                    && !self.preserve_edge_order
                    && (!self.skip_bridges || self.edge_stack.len() > 1)
                    && (frame.next..end).all(|index| {
                        let (neighbor, _) = self.graph.adjacency_at(parent, index);
                        self.disc.get(neighbor as usize) != 0
                    });
                if can_finish_with_one_block {
                    drop(stack);
                    self.disc = DenseU40::Plain(Vec::new());
                    if self.packed {
                        self.node_marks = Vec::new();
                        self.extract_complete_packed_block(child_edge);
                    } else {
                        self.extract_block(child_edge);
                    }
                    return true;
                }
                self.extract_block(child_edge);
                if !parent_is_root {
                    self.mark_cut(parent);
                }
            }
        }
        false
    }

    fn visit(&mut self, node: usize) {
        self.disc.set(node, self.time);
        self.time = self.time.checked_add(1).expect("DFS timestamp overflow");
    }

    #[inline(always)]
    fn push_finished_block(&mut self, node_start: u64, edge_start: u64) {
        let node_count = self.current_block_node_count(node_start);
        let block = self.blocks.len() as u64;
        self.blocks.push(WideBlock {
            node_start,
            node_count,
            edge_start,
            edge_count: self.total_block_edges() - edge_start,
        });
        self.finish_block_nodes(node_start, block);
    }

    fn extract_block(&mut self, tree_edge: u64) {
        if self.skip_bridges
            && !self.edge_stack.is_empty()
            && self.edge_stack.get(self.edge_stack.len() - 1) == tree_edge
        {
            self.edge_stack.pop();
            return;
        }
        let node_start = self.total_block_nodes();
        let edge_start = self.total_block_edges();
        debug_assert_eq!(self.pending_block_nodes, 0);

        if self.blocks.is_empty()
            && self.block_edges_flat.is_empty()
            && self.edge_stack.first() == Some(tree_edge)
        {
            std::mem::swap(&mut self.edge_stack, &mut self.block_edges_flat);
            self.block_edges_flat.reverse();
            let edge_end = self.block_edges_flat.len();
            for index in edge_start as usize..edge_end {
                let edge = self.block_edges_flat.get(index);
                let (u, v) = self.graph.edge_endpoints(edge);
                self.collect_block_node(u as usize);
                self.collect_block_node(v as usize);
            }
            self.push_finished_block(node_start, edge_start);
            return;
        }

        loop {
            let edge = self.edge_stack.pop().expect("missing DFS tree edge");
            self.block_edges_flat.push(edge);
            let (u, v) = self.graph.edge_endpoints(edge);
            self.collect_block_node(u as usize);
            self.collect_block_node(v as usize);
            if edge == tree_edge {
                break;
            }
        }

        self.push_finished_block(node_start, edge_start);
    }

    fn extract_complete_packed_block(&mut self, tree_edge: u64) {
        let edge_start = self.total_block_edges();
        debug_assert!(self.blocks.is_empty());
        debug_assert!(self.block_nodes_flat.as_ref().map_or(true, IdVec::is_empty));
        debug_assert_eq!(self.pending_block_nodes, 0);
        debug_assert_eq!(self.identity_node_prefix, 0);
        debug_assert_eq!(self.edge_stack.first(), Some(tree_edge));

        self.identity_node_prefix = self.graph.num_nodes;
        if self.graph.non_loop_edge_count() as u64 == self.graph.num_edges() {
            self.edge_stack = IdVec::new(self.packed);
            self.block_edges_flat = IdVec::new(self.packed);
            self.identity_edge_prefix = self.graph.num_edges();
            self.blocks.push(WideBlock {
                node_start: 0,
                node_count: self.graph.num_nodes,
                edge_start,
                edge_count: self.graph.num_edges(),
            });
            return;
        }

        std::mem::swap(&mut self.edge_stack, &mut self.block_edges_flat);
        self.block_edges_flat.reverse();
        self.blocks.push(WideBlock {
            node_start: 0,
            node_count: self.graph.num_nodes,
            edge_start,
            edge_count: self.total_block_edges() - edge_start,
        });
    }

    fn append_self_loop_blocks(&mut self) {
        for edge in 0..self.graph.num_edges() {
            let (u, v) = self.graph.edge_endpoints(edge);
            if u != v {
                continue;
            }
            let node_start = self.total_block_nodes();
            let edge_start = self.total_block_edges();
            if let Some(nodes) = self.block_nodes_flat.as_mut() {
                nodes.push(u);
            }
            self.block_edges_flat.push(edge);
            self.blocks.push(WideBlock {
                node_start,
                node_count: 1,
                edge_start,
                edge_count: 1,
            });
        }
    }

    #[inline]
    fn mark_cut(&mut self, node: usize) {
        self.is_cut[node / 64] |= 1u64 << (node % 64);
    }

    #[inline]
    fn collect_block_node(&mut self, node: usize) {
        let word = node / 64;
        let mask = 1u64 << (node % 64);
        if self.node_marks[word] & mask != 0 {
            return;
        }
        self.node_marks[word] |= mask;
        self.pending_block_nodes += 1;
        if let Some(nodes) = self.block_nodes_flat.as_mut() {
            nodes.push(node as u64);
        }
    }

    fn total_block_nodes(&self) -> u64 {
        self.blocks
            .last()
            .map_or(0, |block| block.node_start + block.node_count)
    }

    fn total_block_edges(&self) -> u64 {
        self.identity_edge_prefix + self.block_edges_flat.len() as u64
    }

    fn current_block_node_count(&self, node_start: u64) -> u64 {
        self.block_nodes_flat
            .as_ref()
            .map_or(self.pending_block_nodes, |nodes| {
                self.identity_node_prefix + nodes.len() as u64 - node_start
            })
    }

    fn finish_block_nodes(&mut self, node_start: u64, block: u64) {
        let stored_start = usize::try_from(node_start - self.identity_node_prefix)
            .expect("block node offset exceeds usize");
        let node_count = self.current_block_node_count(node_start) as usize;
        if self.block_nodes_flat.is_some() {
            for local_node in 0..node_count {
                let node = self
                    .block_nodes_flat
                    .as_ref()
                    .expect("missing BC-tree block nodes")
                    .get(stored_start + local_node) as usize;
                self.clear_block_node(node);
            }
        } else {
            let block_record = self.blocks[block as usize];
            let edge_end = block_record.edge_start + block_record.edge_count;
            let mut local_node = 0;
            for index in block_record.edge_start..edge_end {
                let edge = self.block_edge_at(index);
                let (source, target) = self.graph.edge_endpoints(edge);
                for node in [source as usize, target as usize] {
                    if self.clear_block_node(node) {
                        local_node += 1;
                    }
                }
            }
            assert_eq!(
                local_node as usize, node_count,
                "BC-tree block node count changed"
            );
        }
        self.pending_block_nodes = 0;
    }

    #[inline]
    fn clear_block_node(&mut self, node: usize) -> bool {
        let word = node / 64;
        let mask = 1u64 << (node % 64);
        if self.node_marks[word] & mask == 0 {
            return false;
        }
        self.node_marks[word] &= !mask;
        true
    }

    #[inline]
    fn block_edge_at(&self, index: u64) -> u64 {
        if index < self.identity_edge_prefix {
            index
        } else {
            self.block_edges_flat
                .get((index - self.identity_edge_prefix) as usize)
        }
    }

    fn into_tree(self) -> WideBCTree {
        let mut cut_vertices = Vec::new();
        for (word_index, &word) in self.is_cut.iter().enumerate() {
            let mut word = word;
            while word != 0 {
                let bit = word.trailing_zeros() as usize;
                cut_vertices.push((word_index * 64 + bit) as u64);
                word &= word - 1;
            }
        }

        WideBCTree {
            blocks: self.blocks,
            block_nodes_flat: self.block_nodes_flat,
            identity_node_prefix: self.identity_node_prefix,
            block_edges_flat: self.block_edges_flat,
            identity_edge_prefix: self.identity_edge_prefix,
            cut_vertices,
            is_cut: self.is_cut,
            num_nodes: self.graph.num_nodes,
            num_components: self.num_components,
        }
    }
}

