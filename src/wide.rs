#![allow(clippy::needless_range_loop)]
#![allow(dead_code)]

use crate::{spqr_thread_count, CANONICALIZE_ROOT_ENABLED};
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;

use std::collections::HashMap;
use std::fmt;
use std::time::Instant;

const PARALLEL_GRAPH_MIN_EDGES: usize = 4_000_000;
const NODE_PACKED_MAX: u64 = (1u64 << 40) - 1;
const WORK_GRAPH_SHRINK_MIN_SAVINGS: u128 = 1 << 30;

fn packed_half_edge_count(num_edges: usize) -> Option<usize> {
    let half_edges = num_edges.checked_mul(2)?;
    (half_edges <= isize::MAX as usize / std::mem::size_of::<u32>()).then_some(half_edges)
}

#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u64);
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EdgeId(pub u64);
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TreeNodeId(pub u64);

impl Default for NodeId {
    fn default() -> Self {
        NodeId::INVALID
    }
}
impl Default for EdgeId {
    fn default() -> Self {
        EdgeId::INVALID
    }
}
impl Default for TreeNodeId {
    fn default() -> Self {
        TreeNodeId::INVALID
    }
}

impl fmt::Debug for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}", self.0)
    }
}
impl fmt::Debug for EdgeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "e{}", self.0)
    }
}
impl fmt::Debug for TreeNodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "t{}", self.0)
    }
}

pub const INVALID: u64 = u64::MAX;

impl NodeId {
    pub const INVALID: NodeId = NodeId(INVALID);
    #[inline(always)]
    pub fn is_valid(self) -> bool {
        self.0 != INVALID
    }
    #[inline(always)]
    pub fn idx(self) -> usize {
        self.0 as usize
    }
}
impl EdgeId {
    pub const INVALID: EdgeId = EdgeId(INVALID);
    #[inline(always)]
    pub fn is_valid(self) -> bool {
        self.0 != INVALID
    }
    #[inline(always)]
    pub fn idx(self) -> usize {
        self.0 as usize
    }
}
impl TreeNodeId {
    pub const INVALID: TreeNodeId = TreeNodeId(INVALID);
    #[inline(always)]
    pub fn is_valid(self) -> bool {
        self.0 != INVALID
    }
    #[inline(always)]
    pub fn idx(self) -> usize {
        self.0 as usize
    }
}

trait PackedNode: Copy + Send {
    fn pack(value: u64) -> Self;
    fn unpack(value: Self) -> NodeId;
}

impl PackedNode for u32 {
    #[inline(always)]
    fn pack(value: u64) -> Self {
        debug_assert!(value < u32::MAX as u64);
        value as u32
    }

    #[inline(always)]
    fn unpack(value: Self) -> NodeId {
        NodeId(value as u64)
    }
}

impl PackedNode for u64 {
    #[inline(always)]
    fn pack(value: u64) -> Self {
        value
    }

    #[inline(always)]
    fn unpack(value: Self) -> NodeId {
        NodeId(value)
    }
}

#[inline(always)]
fn compact_interleaved<T: PackedNode>(
    values: &mut Vec<T>,
    old_edge_count: usize,
    keep: &mut impl FnMut(usize) -> bool,
    synthetic: &[(u64, u64, u64)],
    emit: &mut impl FnMut(u64, u64, u64),
) {
    let mut output = 0;
    for edge in 0..old_edge_count {
        if !keep(edge) {
            continue;
        }
        let input = edge * 2;
        let target = values[input];
        let source = values[input + 1];
        let output_index = output * 2;
        values[output_index] = target;
        values[output_index + 1] = source;
        emit(edge as u64, T::unpack(source).0, T::unpack(target).0);
        output += 1;
    }
    values.truncate(output * 2);
    for &(source, target, virtual_id) in synthetic {
        values.push(T::pack(target));
        values.push(T::pack(source));
        emit(virtual_id, source, target);
    }
}

#[derive(Clone)]
enum NodeColumn {
    Compact(Vec<u32>),
    Packed {
        low: Vec<u32>,
        high: Vec<u8>,
    },
    SplitPacked {
        source_low: Vec<u32>,
        source_high: Vec<u8>,
        target_low: Vec<u32>,
        target_high: Vec<u8>,
        target_ready: bool,
    },
    Wide(Vec<u64>),
}

impl NodeColumn {
    fn worth_shrinking(len: usize, capacity: usize, unused_bytes: u128) -> bool {
        (len as u128) * 4 <= (capacity as u128) * 3 && unused_bytes >= WORK_GRAPH_SHRINK_MIN_SAVINGS
    }

    fn shrink_compacted(&mut self) {
        match self {
            Self::Compact(values) => {
                let unused = (values.capacity() - values.len()) as u128 * 4;
                if Self::worth_shrinking(values.len(), values.capacity(), unused) {
                    values.shrink_to_fit();
                }
            }
            Self::Packed { low, high } => {
                let unused = (low.capacity() - low.len()) as u128 * 4
                    + (high.capacity() - high.len()) as u128;
                if Self::worth_shrinking(low.len(), low.capacity(), unused) {
                    low.shrink_to_fit();
                    high.shrink_to_fit();
                }
            }
            Self::SplitPacked {
                source_low,
                source_high,
                target_low,
                target_high,
                ..
            } => {
                let unused = (source_low.capacity() - source_low.len()) as u128 * 4
                    + (source_high.capacity() - source_high.len()) as u128
                    + (target_low.capacity() - target_low.len()) as u128 * 4
                    + (target_high.capacity() - target_high.len()) as u128;
                if Self::worth_shrinking(source_low.len(), source_low.capacity(), unused) {
                    source_low.shrink_to_fit();
                    source_high.shrink_to_fit();
                    target_low.shrink_to_fit();
                    target_high.shrink_to_fit();
                }
            }
            Self::Wide(values) => {
                let unused = (values.capacity() - values.len()) as u128 * 8;
                if Self::worth_shrinking(values.len(), values.capacity(), unused) {
                    values.shrink_to_fit();
                }
            }
        }
    }

    fn with_capacity(num_nodes: usize, num_edges: usize) -> Self {
        let capacity = num_edges.checked_mul(2).expect("wide graph is too large");
        if num_nodes <= u32::MAX as usize {
            Self::Compact(Vec::with_capacity(capacity))
        } else {
            Self::Wide(Vec::with_capacity(capacity))
        }
    }

    fn with_len(num_nodes: usize, len: usize) -> Self {
        if num_nodes <= u32::MAX as usize {
            Self::Compact(vec![0; len])
        } else {
            Self::Wide(vec![0; len])
        }
    }

    #[inline(always)]
    fn len(&self) -> usize {
        match self {
            Self::Compact(values) => values.len(),
            Self::Packed { low, .. } => low.len(),
            Self::SplitPacked { source_low, .. } => source_low
                .len()
                .checked_mul(2)
                .expect("split edge storage is too large"),
            Self::Wide(values) => values.len(),
        }
    }

    #[inline(always)]
    fn reserve(&mut self, additional_edges: usize) {
        match self {
            Self::Compact(values) => values.reserve(additional_edges * 2),
            Self::Packed { low, high } => {
                low.reserve(additional_edges * 2);
                high.reserve(additional_edges * 2);
            }
            Self::SplitPacked { .. } => panic!("cannot reserve split edge storage"),
            Self::Wide(values) => values.reserve(additional_edges * 2),
        }
    }

    #[inline(always)]
    fn push(&mut self, value: NodeId) {
        match self {
            Self::Compact(values) => values.push(<u32 as PackedNode>::pack(value.0)),
            Self::Packed { low, high } => {
                debug_assert!(value.0 <= NODE_PACKED_MAX);
                low.push(value.0 as u32);
                high.push((value.0 >> 32) as u8);
            }
            Self::SplitPacked { .. } => panic!("cannot append to split edge storage"),
            Self::Wide(values) => values.push(value.0),
        }
    }

    #[inline(always)]
    fn get(&self, index: usize) -> NodeId {
        match self {
            Self::Compact(values) => <u32 as PackedNode>::unpack(values[index]),
            Self::Packed { low, high } => NodeId(low[index] as u64 | ((high[index] as u64) << 32)),
            Self::SplitPacked {
                source_low,
                source_high,
                target_low,
                target_high,
                target_ready,
            } => {
                assert!(*target_ready, "split edge storage is incomplete");
                let edge = index / 2;
                if index & 1 == 0 {
                    NodeId(target_low[edge] as u64 | ((target_high[edge] as u64) << 32))
                } else {
                    NodeId(source_low[edge] as u64 | ((source_high[edge] as u64) << 32))
                }
            }
            Self::Wide(values) => <u64 as PackedNode>::unpack(values[index]),
        }
    }

    fn packed_with_len(len: usize) -> Self {
        Self::Packed {
            low: vec![0; len],
            high: vec![0; len],
        }
    }

    fn split_packed_with_source_len(len: usize) -> Self {
        Self::SplitPacked {
            source_low: vec![0; len],
            source_high: vec![0; len],
            target_low: Vec::new(),
            target_high: Vec::new(),
            target_ready: false,
        }
    }

    fn split_packed_source_low_mut(&mut self) -> Option<&mut [u32]> {
        match self {
            Self::SplitPacked { source_low, .. } => Some(source_low),
            _ => None,
        }
    }

    fn split_packed_source_high_mut(&mut self) -> Option<&mut [u8]> {
        match self {
            Self::SplitPacked { source_high, .. } => Some(source_high),
            _ => None,
        }
    }

    fn allocate_split_packed_target(&mut self) -> bool {
        match self {
            Self::SplitPacked {
                source_low,
                target_low,
                target_high,
                target_ready,
                ..
            } if !*target_ready => {
                target_low.resize(source_low.len(), 0);
                target_high.resize(source_low.len(), 0);
                *target_ready = true;
                true
            }
            _ => false,
        }
    }

    fn split_packed_target_low_mut(&mut self) -> Option<&mut [u32]> {
        match self {
            Self::SplitPacked {
                target_low,
                target_ready: true,
                ..
            } => Some(target_low),
            _ => None,
        }
    }

    fn split_packed_target_high_mut(&mut self) -> Option<&mut [u8]> {
        match self {
            Self::SplitPacked {
                target_high,
                target_ready: true,
                ..
            } => Some(target_high),
            _ => None,
        }
    }

    fn packed_parts(&self) -> Option<(&[u32], &[u8])> {
        match self {
            Self::Packed { low, high } if low.len() == high.len() => Some((low, high)),
            _ => None,
        }
    }

    fn split_packed_parts(&self) -> Option<(&[u32], &[u8], &[u32], &[u8])> {
        match self {
            Self::SplitPacked {
                source_low,
                source_high,
                target_low,
                target_high,
                target_ready: true,
            } if source_low.len() == source_high.len()
                && source_low.len() == target_low.len()
                && source_low.len() == target_high.len() =>
            {
                Some((source_low, source_high, target_low, target_high))
            }
            _ => None,
        }
    }

    fn compact_edges(
        &mut self,
        old_edge_count: usize,
        mut keep: impl FnMut(usize) -> bool,
        synthetic: &[(u64, u64, u64)],
        mut emit: impl FnMut(u64, u64, u64),
    ) {
        match self {
            Self::Compact(values) => {
                compact_interleaved(values, old_edge_count, &mut keep, synthetic, &mut emit)
            }
            Self::Packed { low, high } => {
                let mut output = 0;
                for edge in 0..old_edge_count {
                    if !keep(edge) {
                        continue;
                    }
                    let input = edge * 2;
                    let output_index = output * 2;
                    let target_low = low[input];
                    let target_high = high[input];
                    let source_low = low[input + 1];
                    let source_high = high[input + 1];
                    low[output_index] = target_low;
                    high[output_index] = target_high;
                    low[output_index + 1] = source_low;
                    high[output_index + 1] = source_high;
                    emit(
                        edge as u64,
                        source_low as u64 | ((source_high as u64) << 32),
                        target_low as u64 | ((target_high as u64) << 32),
                    );
                    output += 1;
                }
                low.truncate(output * 2);
                high.truncate(output * 2);
                for &(source, target, virtual_id) in synthetic {
                    low.push(target as u32);
                    high.push((target >> 32) as u8);
                    low.push(source as u32);
                    high.push((source >> 32) as u8);
                    emit(virtual_id, source, target);
                }
            }
            Self::SplitPacked {
                source_low,
                source_high,
                target_low,
                target_high,
                target_ready,
            } => {
                assert!(*target_ready, "split edge storage is incomplete");
                let mut output = 0;
                for edge in 0..old_edge_count {
                    if !keep(edge) {
                        continue;
                    }
                    let kept_source_low = source_low[edge];
                    let kept_source_high = source_high[edge];
                    let kept_target_low = target_low[edge];
                    let kept_target_high = target_high[edge];
                    source_low[output] = kept_source_low;
                    source_high[output] = kept_source_high;
                    target_low[output] = kept_target_low;
                    target_high[output] = kept_target_high;
                    emit(
                        edge as u64,
                        kept_source_low as u64 | ((kept_source_high as u64) << 32),
                        kept_target_low as u64 | ((kept_target_high as u64) << 32),
                    );
                    output += 1;
                }
                source_low.truncate(output);
                source_high.truncate(output);
                target_low.truncate(output);
                target_high.truncate(output);
                for &(source, target, virtual_id) in synthetic {
                    source_low.push(source as u32);
                    source_high.push((source >> 32) as u8);
                    target_low.push(target as u32);
                    target_high.push((target >> 32) as u8);
                    emit(virtual_id, source, target);
                }
            }
            Self::Wide(values) => {
                compact_interleaved(values, old_edge_count, &mut keep, synthetic, &mut emit)
            }
        }
        self.shrink_compacted();
    }
}

enum HeadColumn {
    Plain(Vec<u64>),
    Atomic(Vec<AtomicU64>),
}

impl Clone for HeadColumn {
    fn clone(&self) -> Self {
        match self {
            Self::Plain(values) => Self::Plain(values.clone()),
            Self::Atomic(values) => Self::Atomic(
                values
                    .iter()
                    .map(|value| AtomicU64::new(value.load(Ordering::Relaxed)))
                    .collect(),
            ),
        }
    }
}

impl HeadColumn {
    #[inline(always)]
    fn len(&self) -> usize {
        match self {
            Self::Plain(values) => values.len(),
            Self::Atomic(values) => values.len(),
        }
    }

    #[inline(always)]
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline(always)]
    fn get(&self, index: usize) -> u64 {
        match self {
            Self::Plain(values) => values[index],
            Self::Atomic(values) => values[index].load(Ordering::Relaxed),
        }
    }

    #[inline(always)]
    fn set(&mut self, index: usize, value: u64) {
        match self {
            Self::Plain(values) => values[index] = value,
            Self::Atomic(values) => values[index].store(value, Ordering::Relaxed),
        }
    }

    fn push(&mut self, value: u64) {
        match self {
            Self::Plain(values) => values.push(value),
            Self::Atomic(values) => values.push(AtomicU64::new(value)),
        }
    }

    fn resize(&mut self, len: usize, value: u64) {
        match self {
            Self::Plain(values) => values.resize(len, value),
            Self::Atomic(values) => values.resize_with(len, || AtomicU64::new(value)),
        }
    }

    fn clear(&mut self) {
        *self = Self::Plain(Vec::new());
    }
}

const NEXT_PACKED_MAX: u64 = (1u64 << 34) - 1;

enum NextColumn {
    Plain(Vec<u64>),
    Packed { low: Vec<u32>, high: Vec<AtomicU64> },
}

impl Clone for NextColumn {
    fn clone(&self) -> Self {
        match self {
            Self::Plain(values) => Self::Plain(values.clone()),
            Self::Packed { low, high } => Self::Packed {
                low: low.clone(),
                high: high
                    .iter()
                    .map(|value| AtomicU64::new(value.load(Ordering::Relaxed)))
                    .collect(),
            },
        }
    }
}

impl NextColumn {
    fn with_capacity(num_edges: usize) -> Self {
        let capacity = num_edges.checked_mul(2).expect("wide graph is too large");
        if capacity as u64 > NEXT_PACKED_MAX {
            Self::Plain(Vec::with_capacity(capacity))
        } else if capacity <= u32::MAX as usize {
            Self::Plain(Vec::with_capacity(capacity))
        } else {
            Self::Packed {
                low: Vec::with_capacity(capacity),
                high: Vec::with_capacity(capacity.div_ceil(32)),
            }
        }
    }

    #[inline(always)]
    fn len(&self) -> usize {
        match self {
            Self::Plain(values) => values.len(),
            Self::Packed { low, .. } => low.len(),
        }
    }

    #[inline(always)]
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline(always)]
    fn get(&self, index: usize) -> u64 {
        match self {
            Self::Plain(values) => values[index],
            Self::Packed { low, high } => {
                let value = low[index] as u64;
                let shift = (index % 32) * 2;
                let high_bits = high[index / 32].load(Ordering::Relaxed) >> shift & 3;
                let value = value | (high_bits << 32);
                if value == NEXT_PACKED_MAX {
                    INVALID
                } else {
                    value
                }
            }
        }
    }

    #[inline(always)]
    fn set(&mut self, index: usize, value: u64) {
        match self {
            Self::Plain(values) => values[index] = value,
            Self::Packed { low, high } => {
                let value = if value == INVALID {
                    NEXT_PACKED_MAX
                } else {
                    value
                };
                low[index] = value as u32;
                debug_assert!(value <= NEXT_PACKED_MAX);
                let shift = (index % 32) * 2;
                let mask = 3u64 << shift;
                let bits = ((value >> 32) & 3) << shift;
                let word = &high[index / 32];
                let mut current = word.load(Ordering::Relaxed);
                loop {
                    let updated = (current & !mask) | bits;
                    match word.compare_exchange_weak(
                        current,
                        updated,
                        Ordering::Relaxed,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => break,
                        Err(observed) => current = observed,
                    }
                }
            }
        }
    }

    fn push(&mut self, value: u64) {
        if self.len() as u64 >= NEXT_PACKED_MAX {
            self.promote_to_plain();
        }
        match self {
            Self::Plain(values) => values.push(value),
            Self::Packed { low, high } => {
                let value = if value == INVALID {
                    NEXT_PACKED_MAX
                } else {
                    value
                };
                let index = low.len();
                low.push(value as u32);
                if index % 32 == 0 {
                    high.push(AtomicU64::new(0));
                }
                debug_assert!(value <= NEXT_PACKED_MAX);
                high[index / 32]
                    .fetch_or(((value >> 32) & 3) << ((index % 32) * 2), Ordering::Relaxed);
            }
        }
    }

    fn reserve(&mut self, additional_edges: usize) {
        let additional = additional_edges
            .checked_mul(2)
            .expect("wide graph is too large");
        if self.len() as u64 > NEXT_PACKED_MAX.saturating_sub(additional as u64) {
            self.promote_to_plain();
        }
        match self {
            Self::Plain(values) => values.reserve(additional),
            Self::Packed { low, high } => {
                low.reserve(additional);
                high.reserve(additional.div_ceil(32));
            }
        }
    }

    fn promote_to_plain(&mut self) {
        let old = std::mem::replace(self, Self::Plain(Vec::new()));
        *self = match old {
            Self::Plain(values) => Self::Plain(values),
            Self::Packed { low, high } => {
                let packed = Self::Packed { low, high };
                let values = (0..packed.len()).map(|index| packed.get(index)).collect();
                Self::Plain(values)
            }
        };
    }

    fn clear(&mut self) {
        *self = Self::Plain(Vec::new());
    }
}

const U40_MAX: u64 = (1u64 << 40) - 1;
const PAYLOAD_SKELETON_PACK_MIN: usize = 1 << 20;
const PAYLOAD_SKELETON_MIN_SAVINGS: u128 = 256 << 20;
const TREE_PEEL_MIN_NODES: usize = 1 << 28;

trait U64Column: Sized {
    const RADIX_PASSES: usize;

    fn with_capacity(capacity: usize) -> Self;
    fn filled(len: usize, value: u64) -> Self;
    fn len(&self) -> usize;
    fn value(&self, index: usize) -> u64;
    fn set_value(&mut self, index: usize, value: u64);
    fn push_value(&mut self, value: u64);
    fn pop_value(&mut self) -> Option<u64>;
    fn last_value(&self) -> Option<u64>;
    fn clear_values(&mut self);
    fn resize_values(&mut self, len: usize, value: u64);
    #[inline(always)]
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl U64Column for Vec<u64> {
    const RADIX_PASSES: usize = 4;

    #[inline]
    fn with_capacity(capacity: usize) -> Self {
        Vec::with_capacity(capacity)
    }

    #[inline]
    fn filled(len: usize, value: u64) -> Self {
        vec![value; len]
    }

    #[inline(always)]
    fn len(&self) -> usize {
        self.as_slice().len()
    }

    #[inline(always)]
    fn value(&self, index: usize) -> u64 {
        self[index]
    }

    #[inline(always)]
    fn set_value(&mut self, index: usize, value: u64) {
        self[index] = value;
    }

    #[inline(always)]
    fn push_value(&mut self, value: u64) {
        self.push(value);
    }

    #[inline(always)]
    fn pop_value(&mut self) -> Option<u64> {
        self.pop()
    }

    #[inline(always)]
    fn last_value(&self) -> Option<u64> {
        self.last().copied()
    }

    #[inline]
    fn clear_values(&mut self) {
        self.clear();
    }

    #[inline]
    fn resize_values(&mut self, len: usize, value: u64) {
        self.resize(len, value);
    }
}

struct PackedU40Column {
    low: Vec<u32>,
    high: Vec<u8>,
}

impl PackedU40Column {
    #[inline(always)]
    fn encode(value: u64) -> u64 {
        if value == INVALID {
            U40_MAX
        } else {
            assert!(value < U40_MAX, "value does not fit in a 40-bit column");
            value
        }
    }

    #[inline(always)]
    fn decode(value: u64) -> u64 {
        if value == U40_MAX {
            INVALID
        } else {
            value
        }
    }
}

impl U64Column for PackedU40Column {
    const RADIX_PASSES: usize = 3;

    fn with_capacity(capacity: usize) -> Self {
        Self {
            low: Vec::with_capacity(capacity),
            high: Vec::with_capacity(capacity),
        }
    }

    fn filled(len: usize, value: u64) -> Self {
        let value = Self::encode(value);
        Self {
            low: vec![value as u32; len],
            high: vec![(value >> 32) as u8; len],
        }
    }

    #[inline(always)]
    fn len(&self) -> usize {
        self.low.len()
    }

    #[inline(always)]
    fn value(&self, index: usize) -> u64 {
        Self::decode(self.low[index] as u64 | ((self.high[index] as u64) << 32))
    }

    #[inline(always)]
    fn set_value(&mut self, index: usize, value: u64) {
        let value = Self::encode(value);
        self.low[index] = value as u32;
        self.high[index] = (value >> 32) as u8;
    }

    #[inline(always)]
    fn push_value(&mut self, value: u64) {
        let value = Self::encode(value);
        self.low.push(value as u32);
        self.high.push((value >> 32) as u8);
    }

    #[inline(always)]
    fn pop_value(&mut self) -> Option<u64> {
        let high = self.high.pop()?;
        let low = self.low.pop().expect("packed column length mismatch");
        Some(Self::decode(low as u64 | ((high as u64) << 32)))
    }

    #[inline(always)]
    fn last_value(&self) -> Option<u64> {
        (!self.low.is_empty()).then(|| self.value(self.low.len() - 1))
    }

    fn clear_values(&mut self) {
        self.low.clear();
        self.high.clear();
    }

    fn resize_values(&mut self, len: usize, value: u64) {
        let value = Self::encode(value);
        self.low.resize(len, value as u32);
        self.high.resize(len, (value >> 32) as u8);
    }
}

trait NodeMappingStorage: Sized {
    fn empty() -> Self;
    fn node_count(&self) -> usize;
    fn push_node(&mut self, node: NodeId);
}

impl NodeMappingStorage for Vec<NodeId> {
    #[inline]
    fn empty() -> Self {
        Vec::new()
    }

    #[inline(always)]
    fn node_count(&self) -> usize {
        Vec::len(self)
    }

    #[inline(always)]
    fn push_node(&mut self, node: NodeId) {
        self.push(node);
    }
}

impl NodeMappingStorage for PackedU40Column {
    #[inline]
    fn empty() -> Self {
        Self::with_capacity(0)
    }

    #[inline(always)]
    fn node_count(&self) -> usize {
        self.low.len()
    }

    #[inline(always)]
    fn push_node(&mut self, node: NodeId) {
        self.push_value(node.0);
    }
}

trait I64Column: Sized {
    fn with_capacity(capacity: usize) -> Self;
    fn filled(len: usize, value: i64) -> Self;
    fn len(&self) -> usize;
    fn value(&self, index: usize) -> i64;
    fn set_value(&mut self, index: usize, value: i64);
    fn push_value(&mut self, value: i64);

    #[inline(always)]
    fn add_value(&mut self, index: usize, value: i64) {
        self.set_value(index, self.value(index) + value);
    }
}

impl I64Column for Vec<i64> {
    #[inline]
    fn with_capacity(capacity: usize) -> Self {
        Vec::with_capacity(capacity)
    }

    #[inline]
    fn filled(len: usize, value: i64) -> Self {
        vec![value; len]
    }

    #[inline(always)]
    fn len(&self) -> usize {
        self.as_slice().len()
    }

    #[inline(always)]
    fn value(&self, index: usize) -> i64 {
        self[index]
    }

    #[inline(always)]
    fn set_value(&mut self, index: usize, value: i64) {
        self[index] = value;
    }

    #[inline(always)]
    fn push_value(&mut self, value: i64) {
        self.push(value);
    }
}

struct PackedI40Column {
    low: Vec<u32>,
    high: Vec<u8>,
}

impl PackedI40Column {
    const SIGN: u64 = 1u64 << 39;

    #[inline(always)]
    fn encode(value: i64) -> u64 {
        assert!(
            value >= -(1i64 << 39) && value < (1i64 << 39),
            "value does not fit in a signed 40-bit column"
        );
        (value as u64) & U40_MAX
    }

    #[inline(always)]
    fn decode(value: u64) -> i64 {
        if value & Self::SIGN == 0 {
            value as i64
        } else {
            (value | !U40_MAX) as i64
        }
    }
}

impl I64Column for PackedI40Column {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            low: Vec::with_capacity(capacity),
            high: Vec::with_capacity(capacity),
        }
    }

    fn filled(len: usize, value: i64) -> Self {
        let value = Self::encode(value);
        Self {
            low: vec![value as u32; len],
            high: vec![(value >> 32) as u8; len],
        }
    }

    #[inline(always)]
    fn len(&self) -> usize {
        self.low.len()
    }

    #[inline(always)]
    fn value(&self, index: usize) -> i64 {
        Self::decode(self.low[index] as u64 | ((self.high[index] as u64) << 32))
    }

    #[inline(always)]
    fn set_value(&mut self, index: usize, value: i64) {
        let value = Self::encode(value);
        self.low[index] = value as u32;
        self.high[index] = (value >> 32) as u8;
    }

    #[inline(always)]
    fn push_value(&mut self, value: i64) {
        let value = Self::encode(value);
        self.low.push(value as u32);
        self.high.push((value >> 32) as u8);
    }
}

struct CountColumn {
    values: Vec<u8>,
    overflow: HashMap<usize, u64>,
}

impl CountColumn {
    const OVERFLOW: u8 = u8::MAX;

    fn zeros(len: usize) -> Self {
        Self {
            values: vec![0; len],
            overflow: HashMap::new(),
        }
    }

    fn with_capacity(capacity: usize) -> Self {
        Self {
            values: Vec::with_capacity(capacity),
            overflow: HashMap::new(),
        }
    }

    #[inline(always)]
    fn value(&self, index: usize) -> i64 {
        let value = self.values[index];
        if value != Self::OVERFLOW {
            value as i64
        } else {
            i64::try_from(
                *self
                    .overflow
                    .get(&index)
                    .expect("missing count overflow value"),
            )
            .expect("count overflow exceeds i64")
        }
    }

    #[inline(always)]
    fn set_value(&mut self, index: usize, value: i64) {
        assert!(value >= 0, "negative count");
        let value = value as u64;
        if value < Self::OVERFLOW as u64 {
            if self.values[index] == Self::OVERFLOW {
                self.overflow
                    .remove(&index)
                    .expect("missing count overflow value");
            }
            self.values[index] = value as u8;
        } else {
            if self.values[index] == Self::OVERFLOW {
                *self
                    .overflow
                    .get_mut(&index)
                    .expect("missing count overflow value") = value;
            } else {
                self.values[index] = Self::OVERFLOW;
                self.overflow.insert(index, value);
            }
        }
    }

    #[inline(always)]
    fn push_value(&mut self, value: i64) {
        assert!(value >= 0, "negative count");
        let index = self.values.len();
        let value = value as u64;
        if value < Self::OVERFLOW as u64 {
            self.values.push(value as u8);
        } else {
            self.values.push(Self::OVERFLOW);
            self.overflow.insert(index, value);
        }
    }

    #[inline(always)]
    fn add_value(&mut self, index: usize, delta: i64) {
        let value = self.values[index];
        if value != Self::OVERFLOW {
            let updated = (value as i64).checked_add(delta).expect("count overflow");
            assert!(updated >= 0, "negative count");
            if updated < Self::OVERFLOW as i64 {
                self.values[index] = updated as u8;
            } else {
                self.values[index] = Self::OVERFLOW;
                self.overflow.insert(index, updated as u64);
            }
            return;
        }

        let updated;
        {
            let current = self
                .overflow
                .get_mut(&index)
                .expect("missing count overflow value");
            updated = i64::try_from(*current)
                .expect("count overflow exceeds i64")
                .checked_add(delta)
                .expect("count overflow");
            assert!(updated >= 0, "negative count");
            if updated >= Self::OVERFLOW as i64 {
                *current = updated as u64;
                return;
            }
        }
        self.values[index] = updated as u8;
        self.overflow.remove(&index);
    }
}

const STACK_PACKED_MAX: u64 = (1u64 << 40) - 2;
const TRICONN_PACKED_MIN_SAVINGS: u128 = 24u128 << 30;
const COMPONENT_MERGE_PACKED_MIN_SAVINGS: u128 = 8u128 << 30;

enum StackValues {
    Plain(Vec<i64>),
    Packed { low: Vec<u32>, high: Vec<u8> },
}

impl StackValues {
    fn new(capacity: usize, packed: bool) -> Self {
        let mut values = if packed {
            Self::Packed {
                low: Vec::with_capacity(capacity.max(1)),
                high: Vec::with_capacity(capacity.max(1)),
            }
        } else {
            Self::Plain(Vec::with_capacity(capacity.max(1)))
        };
        values.ensure_slot(0);
        values
    }

    #[inline(always)]
    fn ensure_slot(&mut self, index: usize) {
        match self {
            Self::Plain(values) => {
                if values.len() <= index {
                    values.push(0);
                }
            }
            Self::Packed { low, high } => {
                if low.len() <= index {
                    low.push(0);
                    high.push(0);
                }
            }
        }
    }

    #[inline(always)]
    fn get(&self, index: usize) -> i64 {
        match self {
            Self::Plain(values) => values[index],
            Self::Packed { low, high } => {
                let value = low[index] as u64 | ((high[index] as u64) << 32);
                if value == STACK_PACKED_MAX + 1 {
                    -1
                } else {
                    value as i64
                }
            }
        }
    }

    #[inline(always)]
    fn set(&mut self, index: usize, value: i64) {
        match self {
            Self::Plain(values) => values[index] = value,
            Self::Packed { low, high } => {
                let encoded = if value < 0 {
                    STACK_PACKED_MAX + 1
                } else {
                    let encoded = value as u64;
                    debug_assert!(encoded <= STACK_PACKED_MAX);
                    encoded
                };
                low[index] = encoded as u32;
                high[index] = (encoded >> 32) as u8;
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Edge {
    pub src: NodeId,
    pub dst: NodeId,
}

#[derive(Clone)]
pub struct Graph {
    node_count: usize,
    heads: HeadColumn,
    targets: NodeColumn,
    next: NextColumn,
}

impl Graph {
    pub fn with_capacity(n: usize, m: usize) -> Self {
        Graph {
            node_count: 0,
            heads: HeadColumn::Plain(Vec::with_capacity(n)),
            targets: NodeColumn::with_capacity(n, m),
            next: NextColumn::with_capacity(m),
        }
    }

    pub fn from_edge_arrays(num_nodes: usize, src: &[u64], dst: &[u64]) -> Self {
        debug_assert_eq!(src.len(), dst.len());
        let num_edges = src.len();
        if num_edges >= PARALLEL_GRAPH_MIN_EDGES && spqr_thread_count() > 1 {
            return Self::from_edge_arrays_parallel(num_nodes, src, dst);
        }

        let mut heads = vec![INVALID; num_nodes];
        let mut targets = NodeColumn::with_capacity(num_nodes, num_edges);
        let mut next = NextColumn::with_capacity(num_edges);
        for i in 0..num_edges {
            let u = src[i];
            let v = dst[i];
            let idx_uv = targets.len() as u64;
            let idx_vu = idx_uv + 1;
            targets.push(NodeId(v));
            next.push(heads[u as usize]);
            heads[u as usize] = idx_uv;
            targets.push(NodeId(u));
            next.push(heads[v as usize]);
            heads[v as usize] = idx_vu;
        }
        Graph {
            node_count: num_nodes,
            heads: HeadColumn::Plain(heads),
            targets,
            next,
        }
    }

    fn fill_parallel_targets<T: PackedNode>(
        targets: &mut [T],
        next: &mut [u64],
        src: &[u64],
        dst: &[u64],
        atomic_heads: &[AtomicU64],
    ) {
        let workers = spqr_thread_count().min(src.len()).max(1);
        let edge_chunk_len = src.len().div_ceil(workers);
        thread::scope(|scope| {
            let chunks = src
                .chunks(edge_chunk_len)
                .zip(dst.chunks(edge_chunk_len))
                .zip(targets.chunks_mut(edge_chunk_len * 2))
                .zip(next.chunks_mut(edge_chunk_len * 2));
            for (chunk_index, (((src_chunk, dst_chunk), target_chunk), next_chunk)) in
                chunks.enumerate()
            {
                let edge_start = chunk_index * edge_chunk_len;
                let atomic_heads = &atomic_heads;
                scope.spawn(move || {
                    for (offset, (&u, &v)) in src_chunk.iter().zip(dst_chunk).enumerate() {
                        let uv = offset * 2;
                        let vu = uv + 1;
                        let next_uv = atomic_heads[u as usize]
                            .swap((edge_start * 2 + uv) as u64, Ordering::Relaxed);
                        let next_vu = atomic_heads[v as usize]
                            .swap((edge_start * 2 + vu) as u64, Ordering::Relaxed);
                        target_chunk[uv] = T::pack(v);
                        next_chunk[uv] = next_uv;
                        target_chunk[vu] = T::pack(u);
                        next_chunk[vu] = next_vu;
                    }
                });
            }
        });
    }

    fn fill_parallel_targets_packed<T: PackedNode>(
        targets: &mut [T],
        next: &mut [u32],
        high: &[AtomicU64],
        src: &[u64],
        dst: &[u64],
        atomic_heads: &[AtomicU64],
    ) {
        let workers = spqr_thread_count().min(src.len()).max(1);
        let edge_chunk_len = src.len().div_ceil(workers);
        thread::scope(|scope| {
            let chunks = src
                .chunks(edge_chunk_len)
                .zip(dst.chunks(edge_chunk_len))
                .zip(targets.chunks_mut(edge_chunk_len * 2))
                .zip(next.chunks_mut(edge_chunk_len * 2));
            for (chunk_index, (((src_chunk, dst_chunk), target_chunk), next_chunk)) in
                chunks.enumerate()
            {
                let edge_start = chunk_index * edge_chunk_len;
                let atomic_heads = &atomic_heads;
                let high = &high;
                scope.spawn(move || {
                    for (offset, (&u, &v)) in src_chunk.iter().zip(dst_chunk).enumerate() {
                        let uv = offset * 2;
                        let vu = uv + 1;
                        let global_uv = edge_start * 2 + uv;
                        let global_vu = global_uv + 1;
                        let next_uv =
                            atomic_heads[u as usize].swap(global_uv as u64, Ordering::Relaxed);
                        let next_vu =
                            atomic_heads[v as usize].swap(global_vu as u64, Ordering::Relaxed);
                        target_chunk[uv] = T::pack(v);
                        next_chunk[uv] = next_uv as u32;
                        target_chunk[vu] = T::pack(u);
                        next_chunk[vu] = next_vu as u32;
                        high[global_uv / 32].fetch_or(
                            ((next_uv >> 32) & 3) << ((global_uv % 32) * 2),
                            Ordering::Relaxed,
                        );
                        high[global_vu / 32].fetch_or(
                            ((next_vu >> 32) & 3) << ((global_vu % 32) * 2),
                            Ordering::Relaxed,
                        );
                    }
                });
            }
        });
    }

    fn from_edge_arrays_parallel(num_nodes: usize, src: &[u64], dst: &[u64]) -> Self {
        let num_edges = src.len();
        let atomic_heads: Vec<AtomicU64> =
            (0..num_nodes).map(|_| AtomicU64::new(INVALID)).collect();
        let half_edge_count = num_edges.checked_mul(2).expect("wide graph is too large");
        let mut targets = NodeColumn::with_len(num_nodes, half_edge_count);
        let next = if half_edge_count as u64 > NEXT_PACKED_MAX {
            let mut next = vec![INVALID; half_edge_count];
            match &mut targets {
                NodeColumn::Compact(values) => {
                    Self::fill_parallel_targets(values, &mut next, src, dst, &atomic_heads)
                }
                NodeColumn::Wide(values) => {
                    Self::fill_parallel_targets(values, &mut next, src, dst, &atomic_heads)
                }
                NodeColumn::Packed { .. } | NodeColumn::SplitPacked { .. } => unreachable!(),
            }
            NextColumn::Plain(next)
        } else if half_edge_count <= u32::MAX as usize {
            let mut next = vec![INVALID; half_edge_count];
            match &mut targets {
                NodeColumn::Compact(values) => {
                    Self::fill_parallel_targets(values, &mut next, src, dst, &atomic_heads)
                }
                NodeColumn::Wide(values) => {
                    Self::fill_parallel_targets(values, &mut next, src, dst, &atomic_heads)
                }
                NodeColumn::Packed { .. } | NodeColumn::SplitPacked { .. } => unreachable!(),
            }
            NextColumn::Plain(next)
        } else {
            let mut next = vec![u32::MAX; half_edge_count];
            let high: Vec<AtomicU64> = (0..half_edge_count.div_ceil(32))
                .map(|_| AtomicU64::new(0))
                .collect();
            match &mut targets {
                NodeColumn::Compact(values) => Self::fill_parallel_targets_packed(
                    values,
                    &mut next,
                    &high,
                    src,
                    dst,
                    &atomic_heads,
                ),
                NodeColumn::Wide(values) => Self::fill_parallel_targets_packed(
                    values,
                    &mut next,
                    &high,
                    src,
                    dst,
                    &atomic_heads,
                ),
                NodeColumn::Packed { .. } | NodeColumn::SplitPacked { .. } => unreachable!(),
            }
            NextColumn::Packed { low: next, high }
        };

        Graph {
            node_count: num_nodes,
            heads: HeadColumn::Atomic(atomic_heads),
            targets,
            next,
        }
    }

    pub fn from_edge_pairs(num_nodes: usize, pairs: &[u64]) -> Self {
        debug_assert_eq!(pairs.len() % 2, 0);
        let num_edges = pairs.len() / 2;
        let mut heads = vec![INVALID; num_nodes];
        let mut targets = NodeColumn::with_capacity(num_nodes, num_edges);
        let mut next = NextColumn::with_capacity(num_edges);
        for i in 0..num_edges {
            let u = pairs[i * 2];
            let v = pairs[i * 2 + 1];
            let idx_uv = targets.len() as u64;
            let idx_vu = idx_uv + 1;
            targets.push(NodeId(v));
            next.push(heads[u as usize]);
            heads[u as usize] = idx_uv;
            targets.push(NodeId(u));
            next.push(heads[v as usize]);
            heads[v as usize] = idx_vu;
        }
        Graph {
            node_count: num_nodes,
            heads: HeadColumn::Plain(heads),
            targets,
            next,
        }
    }
    pub fn add_node(&mut self) -> NodeId {
        let id = NodeId(self.node_count as u64);
        self.node_count += 1;
        self.heads.push(INVALID);
        id
    }
    pub fn add_nodes(&mut self, n: usize) -> Vec<NodeId> {
        let start = self.node_count as u64;
        self.node_count += n;
        self.heads.resize(self.heads.len() + n, INVALID);
        (start..start + n as u64).map(NodeId).collect()
    }
    pub fn add_nodes_fast(&mut self, n: usize) {
        self.node_count += n;
        self.heads.resize(self.heads.len() + n, INVALID);
    }
    pub fn add_edges_flat(&mut self, pairs: &[u64]) {
        let num_edges = pairs.len() / 2;
        self.targets.reserve(num_edges);
        self.next.reserve(num_edges);
        for i in 0..num_edges {
            let u = NodeId(pairs[i * 2]);
            let v = NodeId(pairs[i * 2 + 1]);
            let idx_uv = self.targets.len() as u64;
            let idx_vu = idx_uv + 1;
            self.targets.push(v);
            self.next.push(self.heads.get(u.idx()));
            self.heads.set(u.idx(), idx_uv);
            self.targets.push(u);
            self.next.push(self.heads.get(v.idx()));
            self.heads.set(v.idx(), idx_vu);
        }
    }
    pub(crate) fn release_adjacency(&mut self) {
        self.heads.clear();
        self.next.clear();
    }

    fn compact_work_graph<L: U64Column>(
        &mut self,
        consumed: &[bool],
        self_loops: SelfLoopFlags<'_>,
        synthetic: &[(u64, u64, u64)],
    ) -> L {
        let old_edge_count = self.num_edges();
        assert_eq!(consumed.len(), old_edge_count);
        let retained = (0..old_edge_count)
            .filter(|&edge| !consumed[edge] && !self_loops.is_loop(edge))
            .count();
        let new_edge_count = retained
            .checked_add(synthetic.len())
            .expect("wide graph is too large");
        assert!(new_edge_count <= old_edge_count);

        self.release_adjacency();
        let mut labels = L::with_capacity(new_edge_count);
        let mut heads = vec![INVALID; self.node_count];
        let mut next = NextColumn::with_capacity(new_edge_count);
        self.targets.compact_edges(
            old_edge_count,
            |edge| !consumed[edge] && !self_loops.is_loop(edge),
            synthetic,
            |label, source, target| {
                let first = labels
                    .len()
                    .checked_mul(2)
                    .expect("wide graph is too large");
                next.push(heads[source as usize]);
                heads[source as usize] = first as u64;
                next.push(heads[target as usize]);
                heads[target as usize] = first as u64 + 1;
                labels.push_value(label);
            },
        );
        assert_eq!(self.num_edges(), new_edge_count);
        self.heads = HeadColumn::Plain(heads);
        self.next = next;
        labels
    }

    fn release_edge_storage(&mut self) {
        self.targets = NodeColumn::Compact(Vec::new());
    }

    pub(crate) fn packed_edge_storage(&self) -> Option<(&[u32], &[u8])> {
        self.targets.packed_parts()
    }

    pub(crate) fn split_packed_edge_storage(&self) -> Option<(&[u32], &[u8], &[u32], &[u8])> {
        self.targets.split_packed_parts()
    }

    pub(crate) fn with_edge_storage(num_nodes: usize, num_edges: usize) -> Option<Self> {
        let half_edge_count = num_edges.checked_mul(2)?;
        Some(Graph {
            node_count: num_nodes,
            heads: HeadColumn::Plain(Vec::new()),
            targets: NodeColumn::Wide(vec![0; half_edge_count]),
            next: NextColumn::Plain(Vec::new()),
        })
    }

    pub(crate) fn with_packed_edge_storage(num_nodes: usize, num_edges: usize) -> Option<Self> {
        if num_nodes as u64 > NODE_PACKED_MAX {
            return None;
        }
        let half_edge_count = packed_half_edge_count(num_edges)?;
        Some(Graph {
            node_count: num_nodes,
            heads: HeadColumn::Plain(Vec::new()),
            targets: NodeColumn::packed_with_len(half_edge_count),
            next: NextColumn::Plain(Vec::new()),
        })
    }

    pub(crate) fn with_split_packed_edge_storage(
        num_nodes: usize,
        num_edges: usize,
    ) -> Option<Self> {
        if num_nodes as u64 > NODE_PACKED_MAX {
            return None;
        }
        packed_half_edge_count(num_edges)?;
        Some(Graph {
            node_count: num_nodes,
            heads: HeadColumn::Plain(Vec::new()),
            targets: NodeColumn::split_packed_with_source_len(num_edges),
            next: NextColumn::Plain(Vec::new()),
        })
    }

    pub(crate) fn edge_storage_mut(&mut self) -> Option<&mut [u64]> {
        if !self.heads.is_empty() || !self.next.is_empty() {
            return None;
        }
        match &mut self.targets {
            NodeColumn::Wide(values) => Some(values),
            NodeColumn::Compact(_) | NodeColumn::Packed { .. } | NodeColumn::SplitPacked { .. } => {
                None
            }
        }
    }

    pub(crate) fn packed_edge_storage_low_mut(&mut self) -> Option<&mut [u32]> {
        if !self.heads.is_empty() || !self.next.is_empty() {
            return None;
        }
        match &mut self.targets {
            NodeColumn::Packed { low, .. } => Some(low),
            _ => None,
        }
    }

    pub(crate) fn packed_edge_storage_high_mut(&mut self) -> Option<&mut [u8]> {
        if !self.heads.is_empty() || !self.next.is_empty() {
            return None;
        }
        match &mut self.targets {
            NodeColumn::Packed { high, .. } => Some(high),
            _ => None,
        }
    }

    pub(crate) fn split_packed_edge_storage_source_low_mut(&mut self) -> Option<&mut [u32]> {
        if !self.heads.is_empty() || !self.next.is_empty() {
            return None;
        }
        self.targets.split_packed_source_low_mut()
    }

    pub(crate) fn split_packed_edge_storage_source_high_mut(&mut self) -> Option<&mut [u8]> {
        if !self.heads.is_empty() || !self.next.is_empty() {
            return None;
        }
        self.targets.split_packed_source_high_mut()
    }

    pub(crate) fn allocate_split_packed_edge_storage_target(&mut self) -> bool {
        if !self.heads.is_empty() || !self.next.is_empty() {
            return false;
        }
        self.targets.allocate_split_packed_target()
    }

    pub(crate) fn split_packed_edge_storage_target_low_mut(&mut self) -> Option<&mut [u32]> {
        if !self.heads.is_empty() || !self.next.is_empty() {
            return None;
        }
        self.targets.split_packed_target_low_mut()
    }

    pub(crate) fn split_packed_edge_storage_target_high_mut(&mut self) -> Option<&mut [u8]> {
        if !self.heads.is_empty() || !self.next.is_empty() {
            return None;
        }
        self.targets.split_packed_target_high_mut()
    }

    pub(crate) fn finalize_edge_storage(&mut self) -> bool {
        if !self.heads.is_empty() || !self.next.is_empty() {
            return false;
        }
        if matches!(&self.targets, NodeColumn::SplitPacked { .. })
            && self.targets.split_packed_parts().is_none()
        {
            return false;
        }
        let Some((heads, next)) = build_wide_adjacency(self.node_count, &self.targets) else {
            return false;
        };
        self.heads = heads;
        self.next = next;
        true
    }
    #[inline]
    pub fn num_nodes(&self) -> usize {
        self.node_count
    }
    #[inline]
    pub fn num_edges(&self) -> usize {
        self.targets.len() / 2
    }
    pub fn add_edge(&mut self, u: NodeId, v: NodeId) -> EdgeId {
        let eid = EdgeId(self.num_edges() as u64);
        let idx_uv = self.targets.len() as u64;
        let idx_vu = idx_uv + 1;
        self.targets.push(v);
        self.next.push(self.heads.get(u.idx()));
        self.heads.set(u.idx(), idx_uv);
        self.targets.push(u);
        self.next.push(self.heads.get(v.idx()));
        self.heads.set(v.idx(), idx_vu);
        eid
    }
    #[inline]
    pub fn edge(&self, eid: EdgeId) -> Edge {
        let first = eid.idx() * 2;
        Edge {
            src: self.targets.get(first + 1),
            dst: self.targets.get(first),
        }
    }
    pub fn neighbors(&self, u: NodeId) -> NeighborIter<'_> {
        NeighborIter {
            graph: self,
            current: self.heads.get(u.idx()),
        }
    }
    pub fn degree(&self, u: NodeId) -> usize {
        self.neighbors(u).count()
    }
    /// reverse all adjacency lists so iteration order matches insertion order
    pub fn reverse_adj_lists(&mut self) {
        for v in 0..self.heads.len() {
            let mut prev = INVALID;
            let mut cur = self.heads.get(v);
            while cur != INVALID {
                let next = self.next.get(cur as usize);
                self.next.set(cur as usize, prev);
                prev = cur;
                cur = next;
            }
            self.heads.set(v, prev);
        }
    }
    #[inline(always)]
    pub fn adj_cursor(&self, u: NodeId) -> u64 {
        self.heads.get(u.idx())
    }
    #[inline(always)]
    pub fn adj_next(&self, cursor: u64) -> Option<(NodeId, EdgeId, u64)> {
        if cursor == INVALID {
            return None;
        }
        Some((
            self.targets.get(cursor as usize),
            EdgeId(cursor / 2),
            self.next.get(cursor as usize),
        ))
    }
}

trait TargetColumn: Sync {
    fn len(&self) -> usize;
    fn get(&self, index: usize) -> u64;
}

impl TargetColumn for NodeColumn {
    #[inline(always)]
    fn len(&self) -> usize {
        NodeColumn::len(self)
    }

    #[inline(always)]
    fn get(&self, index: usize) -> u64 {
        NodeColumn::get(self, index).0
    }
}

fn build_wide_adjacency<T: TargetColumn + ?Sized>(
    node_count: usize,
    targets: &T,
) -> Option<(HeadColumn, NextColumn)> {
    if targets.len() % 2 != 0 {
        return None;
    }
    let edge_count = targets.len() / 2;
    let valid = if edge_count >= PARALLEL_GRAPH_MIN_EDGES && spqr_thread_count() > 1 {
        let invalid = AtomicBool::new(false);
        let workers = spqr_thread_count().min(edge_count).max(1);
        let edge_chunk_len = edge_count.div_ceil(workers);
        thread::scope(|scope| {
            for chunk_index in 0..workers {
                let start = chunk_index * edge_chunk_len;
                let end = (start + edge_chunk_len).min(edge_count);
                let invalid = &invalid;
                scope.spawn(move || {
                    if (start..end).any(|edge_id| {
                        let first = edge_id * 2;
                        targets.get(first) >= node_count as u64
                            || targets.get(first + 1) >= node_count as u64
                    }) {
                        invalid.store(true, Ordering::Relaxed);
                    }
                });
            }
        });
        !invalid.load(Ordering::Relaxed)
    } else {
        !(0..edge_count).any(|edge_id| {
            let first = edge_id * 2;
            targets.get(first) >= node_count as u64 || targets.get(first + 1) >= node_count as u64
        })
    };
    if !valid {
        return None;
    }

    if edge_count >= PARALLEL_GRAPH_MIN_EDGES && spqr_thread_count() > 1 {
        let atomic_heads: Vec<AtomicU64> =
            (0..node_count).map(|_| AtomicU64::new(INVALID)).collect();
        let workers = spqr_thread_count().min(edge_count).max(1);
        let edge_chunk_len = edge_count.div_ceil(workers);
        if targets.len() as u64 <= u32::MAX as u64 || targets.len() as u64 > NEXT_PACKED_MAX {
            let mut next = vec![INVALID; targets.len()];
            thread::scope(|scope| {
                for (chunk_index, next_chunk) in next.chunks_mut(edge_chunk_len * 2).enumerate() {
                    let edge_start = chunk_index * edge_chunk_len;
                    let atomic_heads = &atomic_heads;
                    scope.spawn(move || {
                        for (offset, pair_next) in next_chunk.chunks_exact_mut(2).enumerate() {
                            let first = (edge_start + offset) * 2;
                            let dst = targets.get(first);
                            let src = targets.get(first + 1);
                            pair_next[0] =
                                atomic_heads[src as usize].swap(first as u64, Ordering::Relaxed);
                            pair_next[1] = atomic_heads[dst as usize]
                                .swap((first + 1) as u64, Ordering::Relaxed);
                        }
                    });
                }
            });
            Some((HeadColumn::Atomic(atomic_heads), NextColumn::Plain(next)))
        } else {
            let mut next = vec![u32::MAX; targets.len()];
            let high: Vec<AtomicU64> = (0..targets.len().div_ceil(32))
                .map(|_| AtomicU64::new(0))
                .collect();
            thread::scope(|scope| {
                for (chunk_index, next_chunk) in next.chunks_mut(edge_chunk_len * 2).enumerate() {
                    let edge_start = chunk_index * edge_chunk_len;
                    let atomic_heads = &atomic_heads;
                    let high = &high;
                    scope.spawn(move || {
                        for (offset, pair_next) in next_chunk.chunks_exact_mut(2).enumerate() {
                            let first = (edge_start + offset) * 2;
                            let dst = targets.get(first);
                            let src = targets.get(first + 1);
                            let next_uv =
                                atomic_heads[src as usize].swap(first as u64, Ordering::Relaxed);
                            let next_vu = atomic_heads[dst as usize]
                                .swap((first + 1) as u64, Ordering::Relaxed);
                            pair_next[0] = next_uv as u32;
                            pair_next[1] = next_vu as u32;
                            high[first / 32].fetch_or(
                                ((next_uv >> 32) & 3) << ((first % 32) * 2),
                                Ordering::Relaxed,
                            );
                            let reverse = first + 1;
                            high[reverse / 32].fetch_or(
                                ((next_vu >> 32) & 3) << ((reverse % 32) * 2),
                                Ordering::Relaxed,
                            );
                        }
                    });
                }
            });
            Some((
                HeadColumn::Atomic(atomic_heads),
                NextColumn::Packed { low: next, high },
            ))
        }
    } else {
        let mut heads = vec![INVALID; node_count];
        if targets.len() as u64 <= u32::MAX as u64 || targets.len() as u64 > NEXT_PACKED_MAX {
            let mut next = vec![INVALID; targets.len()];
            for edge_id in 0..edge_count {
                let first = edge_id * 2;
                let dst = targets.get(first);
                let src = targets.get(first + 1);
                next[first] = heads[src as usize];
                heads[src as usize] = first as u64;
                next[first + 1] = heads[dst as usize];
                heads[dst as usize] = (first + 1) as u64;
            }
            Some((HeadColumn::Plain(heads), NextColumn::Plain(next)))
        } else {
            let mut next = vec![u32::MAX; targets.len()];
            let high: Vec<AtomicU64> = (0..targets.len().div_ceil(32))
                .map(|_| AtomicU64::new(0))
                .collect();
            for edge_id in 0..edge_count {
                let first = edge_id * 2;
                let dst = targets.get(first);
                let src = targets.get(first + 1);
                let next_uv = heads[src as usize];
                heads[src as usize] = first as u64;
                let next_vu = heads[dst as usize];
                heads[dst as usize] = (first + 1) as u64;
                next[first] = next_uv as u32;
                next[first + 1] = next_vu as u32;
                high[first / 32].fetch_or(
                    ((next_uv >> 32) & 3) << ((first % 32) * 2),
                    Ordering::Relaxed,
                );
                let reverse = first + 1;
                high[reverse / 32].fetch_or(
                    ((next_vu >> 32) & 3) << ((reverse % 32) * 2),
                    Ordering::Relaxed,
                );
            }
            Some((
                HeadColumn::Plain(heads),
                NextColumn::Packed { low: next, high },
            ))
        }
    }
}

trait GraphAccess: Deref<Target = Graph> {
    #[inline(always)]
    fn as_graph(&self) -> &Graph {
        &**self
    }

    fn release_adjacency(&mut self);

    fn release_edge_storage(&mut self) {}

    fn compact_work_graph<L: U64Column>(
        &mut self,
        _consumed: &[bool],
        _self_loops: SelfLoopFlags<'_>,
        _synthetic: &[(u64, u64, u64)],
    ) -> Option<L> {
        None
    }
}

struct GraphRef<'a>(&'a Graph);

impl Deref for GraphRef<'_> {
    type Target = Graph;

    fn deref(&self) -> &Self::Target {
        self.0
    }
}

impl GraphAccess for GraphRef<'_> {
    #[inline(always)]
    fn release_adjacency(&mut self) {}
}

struct GraphMut<'a>(&'a mut Graph);

impl Deref for GraphMut<'_> {
    type Target = Graph;

    fn deref(&self) -> &Self::Target {
        self.0
    }
}

impl GraphAccess for GraphMut<'_> {
    #[inline(always)]
    fn release_adjacency(&mut self) {
        self.0.release_adjacency();
    }

    fn release_edge_storage(&mut self) {
        self.0.release_edge_storage();
    }

    fn compact_work_graph<L: U64Column>(
        &mut self,
        consumed: &[bool],
        self_loops: SelfLoopFlags<'_>,
        synthetic: &[(u64, u64, u64)],
    ) -> Option<L> {
        Some(self.0.compact_work_graph(consumed, self_loops, synthetic))
    }
}

pub struct NeighborIter<'a> {
    graph: &'a Graph,
    current: u64,
}
impl<'a> Iterator for NeighborIter<'a> {
    type Item = (NodeId, EdgeId);
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        if self.current == INVALID {
            return None;
        }
        let edge_id = EdgeId(self.current / 2);
        let target = self.graph.targets.get(self.current as usize);
        self.current = self.graph.next.get(self.current as usize);
        Some((target, edge_id))
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpqrNodeType {
    S,
    P,
    R,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct SkeletonEdge {
    pub src: NodeId,
    pub dst: NodeId,
    pub real_edge: EdgeId,
    pub virtual_id: u64,
    pub twin_tree_node: TreeNodeId,
    pub twin_edge_idx: u64,
}

impl Default for SkeletonEdge {
    fn default() -> Self {
        SkeletonEdge {
            src: NodeId::INVALID,
            dst: NodeId::INVALID,
            real_edge: EdgeId::INVALID,
            virtual_id: INVALID,
            twin_tree_node: TreeNodeId::INVALID,
            twin_edge_idx: INVALID,
        }
    }
}

trait SkeletonEdgeStorage {
    fn with_capacity(capacity: usize) -> Self;
    fn len(&self) -> usize;
    fn push(&mut self, edge: SkeletonEdge);
    fn get(&self, index: usize) -> SkeletonEdge;
    fn pair_virtual(
        &mut self,
        first: usize,
        second: usize,
        first_tree: TreeNodeId,
        first_edge: u64,
        second_tree: TreeNodeId,
        second_edge: u64,
    );
    fn clear_virtual(&mut self, index: usize);
}

impl SkeletonEdgeStorage for Vec<SkeletonEdge> {
    fn with_capacity(capacity: usize) -> Self {
        Vec::with_capacity(capacity)
    }

    #[inline(always)]
    fn len(&self) -> usize {
        Vec::len(self)
    }

    #[inline(always)]
    fn push(&mut self, edge: SkeletonEdge) {
        Vec::push(self, edge);
    }

    #[inline(always)]
    fn get(&self, index: usize) -> SkeletonEdge {
        self[index]
    }

    fn pair_virtual(
        &mut self,
        first: usize,
        second: usize,
        first_tree: TreeNodeId,
        first_edge: u64,
        second_tree: TreeNodeId,
        second_edge: u64,
    ) {
        self[first].twin_tree_node = second_tree;
        self[first].twin_edge_idx = second_edge;
        self[second].twin_tree_node = first_tree;
        self[second].twin_edge_idx = first_edge;
    }

    fn clear_virtual(&mut self, index: usize) {
        self[index].virtual_id = INVALID;
        self[index].twin_tree_node = TreeNodeId::INVALID;
        self[index].twin_edge_idx = INVALID;
    }
}

const SKELETON_EDGE_REAL: u8 = 0;
const SKELETON_EDGE_PENDING: u8 = 1;
const SKELETON_EDGE_FIRST: u8 = 2;
const SKELETON_EDGE_SECOND: u8 = 3;
const SKELETON_EDGE_EMPTY: u8 = 4;

#[repr(C)]
#[derive(Clone, Copy)]
struct PackedPayloadSkeletonEdge {
    low: u64,
    high: u64,
}

const _: () = assert!(std::mem::size_of::<PackedPayloadSkeletonEdge>() == 16);

impl PackedPayloadSkeletonEdge {
    #[inline(always)]
    fn new(edge: SkeletonEdge) -> Self {
        let (identity, kind) = if edge.real_edge.is_valid() {
            assert!(edge.virtual_id == INVALID);
            (edge.real_edge.0, SKELETON_EDGE_REAL)
        } else if edge.virtual_id != INVALID {
            (edge.virtual_id, SKELETON_EDGE_PENDING)
        } else {
            (INVALID, SKELETON_EDGE_EMPTY)
        };
        let src = PackedU40Column::encode(edge.src.0);
        let dst = PackedU40Column::encode(edge.dst.0);
        let identity = PackedU40Column::encode(identity);
        Self {
            low: src | ((dst & 0x00ff_ffff) << 40),
            high: (dst >> 24) | (identity << 16) | ((kind as u64) << 56),
        }
    }

    #[inline(always)]
    fn src(self) -> NodeId {
        NodeId(PackedU40Column::decode(self.low & U40_MAX))
    }

    #[inline(always)]
    fn dst(self) -> NodeId {
        NodeId(PackedU40Column::decode(
            (self.low >> 40) | ((self.high & 0xffff) << 24),
        ))
    }

    #[inline(always)]
    fn identity(self) -> u64 {
        PackedU40Column::decode((self.high >> 16) & U40_MAX)
    }

    #[inline(always)]
    fn kind(self) -> u8 {
        (self.high >> 56) as u8
    }

    #[inline(always)]
    fn set_kind(&mut self, kind: u8) {
        self.high = (self.high & ((1u64 << 56) - 1)) | ((kind as u64) << 56);
    }

    #[inline(always)]
    fn clear(&mut self) {
        self.high = (self.high & !(U40_MAX << 16)) | (U40_MAX << 16);
        self.set_kind(SKELETON_EDGE_EMPTY);
    }
}

struct PackedVirtualPairs {
    first_virtual: u64,
    first_tree: PackedU40Column,
    first_edge: PackedU40Column,
    second_tree: PackedU40Column,
    second_edge: PackedU40Column,
}

impl PackedVirtualPairs {
    fn empty() -> Self {
        Self {
            first_virtual: 0,
            first_tree: PackedU40Column::with_capacity(0),
            first_edge: PackedU40Column::with_capacity(0),
            second_tree: PackedU40Column::with_capacity(0),
            second_edge: PackedU40Column::with_capacity(0),
        }
    }

    #[inline(always)]
    fn index(&self, virtual_id: u64) -> usize {
        let index = usize::try_from(
            virtual_id
                .checked_sub(self.first_virtual)
                .expect("virtual edge is below its pair table"),
        )
        .expect("virtual edge pair index exceeds usize");
        assert!(index < self.first_tree.len());
        index
    }
}

trait PayloadPairColumn: U64Column {
    fn into_payload_pairs(
        first_virtual: u64,
        first_tree: Self,
        first_edge: Self,
        second_tree: Self,
        second_edge: Self,
    ) -> Option<PackedVirtualPairs>;
}

impl PayloadPairColumn for Vec<u64> {
    fn into_payload_pairs(
        _first_virtual: u64,
        _first_tree: Self,
        _first_edge: Self,
        _second_tree: Self,
        _second_edge: Self,
    ) -> Option<PackedVirtualPairs> {
        None
    }
}

impl PayloadPairColumn for PackedU40Column {
    fn into_payload_pairs(
        first_virtual: u64,
        first_tree: Self,
        first_edge: Self,
        second_tree: Self,
        second_edge: Self,
    ) -> Option<PackedVirtualPairs> {
        Some(PackedVirtualPairs {
            first_virtual,
            first_tree,
            first_edge,
            second_tree,
            second_edge,
        })
    }
}

struct PackedSkeletonEdges {
    edges: Vec<PackedPayloadSkeletonEdge>,
}

impl PackedSkeletonEdges {
    #[inline(always)]
    fn edge(&self, index: usize, pairs: Option<&PackedVirtualPairs>) -> SkeletonEdge {
        let edge = self.edges[index];
        let (real_edge, virtual_id, twin_tree_node, twin_edge_idx) = match edge.kind() {
            SKELETON_EDGE_REAL => (
                EdgeId(edge.identity()),
                INVALID,
                TreeNodeId::INVALID,
                INVALID,
            ),
            SKELETON_EDGE_PENDING => (
                EdgeId::INVALID,
                edge.identity(),
                TreeNodeId::INVALID,
                INVALID,
            ),
            SKELETON_EDGE_EMPTY => (EdgeId::INVALID, INVALID, TreeNodeId::INVALID, INVALID),
            SKELETON_EDGE_FIRST | SKELETON_EDGE_SECOND => {
                let pairs = pairs.expect("missing virtual edge pairs");
                let pair = pairs.index(edge.identity());
                if edge.kind() == SKELETON_EDGE_FIRST {
                    (
                        EdgeId::INVALID,
                        edge.identity(),
                        TreeNodeId(pairs.second_tree.value(pair)),
                        pairs.second_edge.value(pair),
                    )
                } else {
                    (
                        EdgeId::INVALID,
                        edge.identity(),
                        TreeNodeId(pairs.first_tree.value(pair)),
                        pairs.first_edge.value(pair),
                    )
                }
            }
            _ => panic!("invalid packed skeleton edge state"),
        };
        SkeletonEdge {
            src: edge.src(),
            dst: edge.dst(),
            real_edge,
            virtual_id,
            twin_tree_node,
            twin_edge_idx,
        }
    }
}

impl SkeletonEdgeStorage for PackedSkeletonEdges {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            edges: Vec::with_capacity(capacity),
        }
    }

    #[inline(always)]
    fn len(&self) -> usize {
        self.edges.len()
    }

    #[inline(always)]
    fn push(&mut self, edge: SkeletonEdge) {
        self.edges.push(PackedPayloadSkeletonEdge::new(edge));
    }

    #[inline(always)]
    fn get(&self, index: usize) -> SkeletonEdge {
        self.edge(index, None)
    }

    fn pair_virtual(
        &mut self,
        first: usize,
        second: usize,
        _first_tree: TreeNodeId,
        _first_edge: u64,
        _second_tree: TreeNodeId,
        _second_edge: u64,
    ) {
        assert_eq!(self.edges[first].kind(), SKELETON_EDGE_PENDING);
        assert_eq!(self.edges[second].kind(), SKELETON_EDGE_PENDING);
        assert_eq!(self.edges[first].identity(), self.edges[second].identity());
        self.edges[first].set_kind(SKELETON_EDGE_FIRST);
        self.edges[second].set_kind(SKELETON_EDGE_SECOND);
    }

    fn clear_virtual(&mut self, index: usize) {
        assert_eq!(self.edges[index].kind(), SKELETON_EDGE_PENDING);
        self.edges[index].clear();
    }
}

enum PayloadSkeletonEdges {
    Plain(Vec<SkeletonEdge>),
    Packed {
        edges: PackedSkeletonEdges,
        pairs: PackedVirtualPairs,
    },
}

pub(crate) struct PackedSkeletonColumns<'a> {
    pub edge_words: *const u64,
    pub edge_count: usize,
    pub first_virtual: u64,
    pub first_tree_low: &'a [u32],
    pub first_tree_high: &'a [u8],
    pub first_edge_low: &'a [u32],
    pub first_edge_high: &'a [u8],
    pub second_tree_low: &'a [u32],
    pub second_tree_high: &'a [u8],
    pub second_edge_low: &'a [u32],
    pub second_edge_high: &'a [u8],
}

impl PayloadSkeletonEdges {
    fn len(&self) -> usize {
        match self {
            Self::Plain(edges) => edges.len(),
            Self::Packed { edges, .. } => edges.len(),
        }
    }

    fn get(&self, index: usize) -> SkeletonEdge {
        match self {
            Self::Plain(edges) => edges[index],
            Self::Packed { edges, pairs } => edges.edge(index, Some(pairs)),
        }
    }

    fn plain_ptr(&self) -> *const SkeletonEdge {
        match self {
            Self::Plain(edges) => edges.as_ptr(),
            Self::Packed { .. } => std::ptr::null(),
        }
    }

    fn packed_columns(&self) -> Option<PackedSkeletonColumns<'_>> {
        let Self::Packed { edges, pairs } = self else {
            return None;
        };
        let pair_count = pairs.first_tree.len();
        if pairs.first_tree.high.len() != pair_count
            || pairs.first_edge.len() != pair_count
            || pairs.first_edge.high.len() != pair_count
            || pairs.second_tree.len() != pair_count
            || pairs.second_tree.high.len() != pair_count
            || pairs.second_edge.len() != pair_count
            || pairs.second_edge.high.len() != pair_count
        {
            return None;
        }
        Some(PackedSkeletonColumns {
            edge_words: edges.edges.as_ptr().cast::<u64>(),
            edge_count: edges.edges.len(),
            first_virtual: pairs.first_virtual,
            first_tree_low: &pairs.first_tree.low,
            first_tree_high: &pairs.first_tree.high,
            first_edge_low: &pairs.first_edge.low,
            first_edge_high: &pairs.first_edge.high,
            second_tree_low: &pairs.second_tree.low,
            second_tree_high: &pairs.second_tree.high,
            second_edge_low: &pairs.second_edge.low,
            second_edge_high: &pairs.second_edge.high,
        })
    }
}

enum PayloadNodeMapping {
    Plain(Vec<NodeId>),
    Packed(PackedU40Column),
}

impl PayloadNodeMapping {
    fn len(&self) -> usize {
        match self {
            Self::Plain(values) => values.len(),
            Self::Packed(values) => values.len(),
        }
    }

    fn plain_ptr(&self) -> *const NodeId {
        match self {
            Self::Plain(values) => values.as_ptr(),
            Self::Packed(_) => std::ptr::null(),
        }
    }

    fn packed_parts(&self) -> Option<(&[u32], &[u8])> {
        match self {
            Self::Packed(values) if values.low.len() == values.high.len() => {
                Some((&values.low, &values.high))
            }
            _ => None,
        }
    }

    fn copy_u64(&self, first: usize, out: &mut [u64]) {
        match self {
            Self::Plain(values) => {
                let end = first + out.len();
                for (target, value) in out.iter_mut().zip(&values[first..end]) {
                    *target = value.0;
                }
            }
            Self::Packed(values) => {
                for (offset, target) in out.iter_mut().enumerate() {
                    *target = values.value(first + offset);
                }
            }
        }
    }

    #[cfg(test)]
    fn into_plain(self) -> Vec<NodeId> {
        match self {
            Self::Plain(values) => values,
            Self::Packed(values) => (0..values.len())
                .map(|index| NodeId(values.value(index)))
                .collect(),
        }
    }
}

pub struct SpqrPayloadTree {
    pub(crate) root: TreeNodeId,
    pub(crate) node_types: Vec<SpqrNodeType>,
    pub(crate) node_parents: Vec<TreeNodeId>,
    pub(crate) children_offsets: Vec<u64>,
    pub(crate) children: Vec<TreeNodeId>,
    pub(crate) skeleton_offsets: Vec<u64>,
    skeleton_edges: PayloadSkeletonEdges,
    pub(crate) node_mapping_offsets: Vec<u64>,
    node_mapping: PayloadNodeMapping,
    pub(crate) skeleton_num_nodes: Vec<u64>,
}

impl SpqrPayloadTree {
    pub(crate) fn len(&self) -> usize {
        self.node_types.len()
    }

    pub(crate) fn skeleton_edge_count(&self) -> usize {
        self.skeleton_edges.len()
    }

    pub(crate) fn skeleton_edge(&self, index: usize) -> SkeletonEdge {
        self.skeleton_edges.get(index)
    }

    pub(crate) fn plain_skeleton_edges(&self) -> *const SkeletonEdge {
        self.skeleton_edges.plain_ptr()
    }

    pub(crate) fn packed_skeleton_columns(&self) -> Option<PackedSkeletonColumns<'_>> {
        self.skeleton_edges.packed_columns()
    }

    pub(crate) fn node_mapping_count(&self) -> usize {
        self.node_mapping.len()
    }

    pub(crate) fn plain_node_mapping(&self) -> *const NodeId {
        self.node_mapping.plain_ptr()
    }

    pub(crate) fn packed_node_mapping(&self) -> Option<(&[u32], &[u8])> {
        self.node_mapping.packed_parts()
    }

    pub(crate) fn copy_node_mapping_u64(&self, first: usize, out: &mut [u64]) {
        self.node_mapping.copy_u64(first, out);
    }

    #[cfg(test)]
    fn into_tree(self) -> SpqrTree {
        let skeleton_edges = (0..self.skeleton_edges.len())
            .map(|index| self.skeleton_edges.get(index))
            .collect();
        SpqrTree {
            root: self.root,
            node_types: self.node_types,
            node_parents: self.node_parents,
            children_offsets: self.children_offsets,
            children: self.children,
            skeleton_offsets: self.skeleton_offsets,
            skeleton_edges,
            node_mapping_offsets: self.node_mapping_offsets,
            node_mapping: self.node_mapping.into_plain(),
            skeleton_num_nodes: self.skeleton_num_nodes,
            edge_to_tree_node: Vec::new(),
            min_real_per_node: Vec::new(),
        }
    }
}

pub struct SkeletonView<'a> {
    pub num_nodes: u64,
    pub edges: &'a [SkeletonEdge],
    pub node_to_original: &'a [NodeId],
}

impl<'a> SkeletonView<'a> {
    pub fn poles(&self) -> (NodeId, NodeId) {
        (self.node_to_original[0], self.node_to_original[1])
    }
    pub fn num_edges(&self) -> usize {
        self.edges.len()
    }
}

pub struct SpqrTreeNodeView<'a> {
    pub node_type: SpqrNodeType,
    pub skeleton: SkeletonView<'a>,
    pub parent: TreeNodeId,
    pub children: &'a [TreeNodeId],
}

#[derive(Clone, PartialEq, Eq)]
pub struct SpqrTree {
    pub root: TreeNodeId,
    pub node_types: Vec<SpqrNodeType>,
    pub node_parents: Vec<TreeNodeId>,
    pub children_offsets: Vec<u64>,
    pub children: Vec<TreeNodeId>,
    pub skeleton_offsets: Vec<u64>,
    pub skeleton_edges: Vec<SkeletonEdge>,
    pub node_mapping_offsets: Vec<u64>,
    pub node_mapping: Vec<NodeId>,
    pub skeleton_num_nodes: Vec<u64>,
    pub edge_to_tree_node: Vec<TreeNodeId>,
    pub min_real_per_node: Vec<u64>,
}

impl SpqrTree {
    pub fn len(&self) -> usize {
        self.node_types.len()
    }

    pub fn is_empty(&self) -> bool {
        self.node_types.is_empty()
    }

    #[inline]
    pub fn node_type(&self, id: TreeNodeId) -> SpqrNodeType {
        self.node_types[id.idx()]
    }

    #[inline]
    pub fn parent(&self, id: TreeNodeId) -> TreeNodeId {
        self.node_parents[id.idx()]
    }

    #[inline]
    pub fn children_slice(&self, id: TreeNodeId) -> &[TreeNodeId] {
        let start = self.children_offsets[id.idx()] as usize;
        let end = self.children_offsets[id.idx() + 1] as usize;
        &self.children[start..end]
    }

    #[inline]
    pub fn skeleton_edges_slice(&self, id: TreeNodeId) -> &[SkeletonEdge] {
        let start = self.skeleton_offsets[id.idx()] as usize;
        let end = self.skeleton_offsets[id.idx() + 1] as usize;
        &self.skeleton_edges[start..end]
    }

    #[inline]
    pub fn skeleton_edges_slice_mut(&mut self, id: TreeNodeId) -> &mut [SkeletonEdge] {
        let start = self.skeleton_offsets[id.idx()] as usize;
        let end = self.skeleton_offsets[id.idx() + 1] as usize;
        &mut self.skeleton_edges[start..end]
    }

    #[inline]
    pub fn skeleton_edge_mut(
        &mut self,
        tree_node: TreeNodeId,
        edge_idx: usize,
    ) -> &mut SkeletonEdge {
        let start = self.skeleton_offsets[tree_node.idx()] as usize;
        &mut self.skeleton_edges[start + edge_idx]
    }

    #[inline]
    pub fn node_mapping_slice(&self, id: TreeNodeId) -> &[NodeId] {
        let start = self.node_mapping_offsets[id.idx()] as usize;
        let end = self.node_mapping_offsets[id.idx() + 1] as usize;
        &self.node_mapping[start..end]
    }

    #[inline]
    pub fn skeleton_num_nodes(&self, id: TreeNodeId) -> u64 {
        self.skeleton_num_nodes[id.idx()]
    }

    pub fn node(&self, id: TreeNodeId) -> SpqrTreeNodeView<'_> {
        SpqrTreeNodeView {
            node_type: self.node_types[id.idx()],
            skeleton: SkeletonView {
                num_nodes: self.skeleton_num_nodes[id.idx()],
                edges: self.skeleton_edges_slice(id),
                node_to_original: self.node_mapping_slice(id),
            },
            parent: self.node_parents[id.idx()],
            children: self.children_slice(id),
        }
    }

    pub fn tree_node_of_edge(&self, eid: EdgeId) -> TreeNodeId {
        self.edge_to_tree_node[eid.idx()]
    }

    pub fn count_by_type(&self) -> (usize, usize, usize) {
        let (mut s, mut p, mut r) = (0, 0, 0);
        for &t in &self.node_types {
            match t {
                SpqrNodeType::S => s += 1,
                SpqrNodeType::P => p += 1,
                SpqrNodeType::R => r += 1,
            }
        }
        (s, p, r)
    }

    pub fn iter(&self) -> impl Iterator<Item = TreeNodeId> + '_ {
        (0..self.len()).map(|i| TreeNodeId(i as u64))
    }

    fn empty(num_edges: usize) -> Self {
        SpqrTree {
            root: TreeNodeId::INVALID,
            node_types: Vec::new(),
            node_parents: Vec::new(),
            children_offsets: vec![0],
            children: Vec::new(),
            skeleton_offsets: vec![0],
            skeleton_edges: Vec::new(),
            node_mapping_offsets: vec![0],
            node_mapping: Vec::new(),
            skeleton_num_nodes: Vec::new(),
            edge_to_tree_node: vec![TreeNodeId::INVALID; num_edges],
            min_real_per_node: Vec::new(),
        }
    }

    fn single_node(
        num_edges: usize,
        node_type: SpqrNodeType,
        num_skel_nodes: u64,
        edges: Vec<SkeletonEdge>,
        node_to_original: Vec<NodeId>,
    ) -> Self {
        let mut edge_to_tree_node = vec![TreeNodeId::INVALID; num_edges];
        let mut min_real: u64 = u64::MAX;
        for edge in &edges {
            if edge.real_edge.is_valid() {
                edge_to_tree_node[edge.real_edge.idx()] = TreeNodeId(0);
                if edge.real_edge.0 < min_real {
                    min_real = edge.real_edge.0;
                }
            }
        }

        SpqrTree {
            root: TreeNodeId(0),
            node_types: vec![node_type],
            node_parents: vec![TreeNodeId::INVALID],
            children_offsets: vec![0, 0],
            children: Vec::new(),
            skeleton_offsets: vec![0, edges.len() as u64],
            skeleton_edges: edges,
            node_mapping_offsets: vec![0, node_to_original.len() as u64],
            node_mapping: node_to_original,
            skeleton_num_nodes: vec![num_skel_nodes],
            edge_to_tree_node,
            min_real_per_node: vec![min_real],
        }
    }
}

/// result of an SPQR decomposition.
///
/// any self-loops present in the input graph are collected in self_loops
///
/// for self loop edges, tree.tree_node_of_edge() returns TreeNodeId::INVALID.
pub struct SpqrResult {
    pub tree: SpqrTree,
    /// Selfloop edges (v,v) stripped before decomposition
    pub self_loops: Vec<EdgeId>,
}

pub(crate) struct SpqrPayloadResult {
    pub(crate) tree: SpqrPayloadTree,
}

struct AssembledSpqrTree<E, M> {
    root: TreeNodeId,
    node_types: Vec<SpqrNodeType>,
    node_parents: Vec<TreeNodeId>,
    children_offsets: Vec<u64>,
    children: Vec<TreeNodeId>,
    skeleton_offsets: Vec<u64>,
    skeleton_edges: E,
    node_mapping_offsets: Vec<u64>,
    node_mapping: M,
    skeleton_num_nodes: Vec<u64>,
    edge_to_tree_node: Vec<TreeNodeId>,
    min_real_per_node: Vec<u64>,
    payload_pairs: Option<PackedVirtualPairs>,
}

impl AssembledSpqrTree<Vec<SkeletonEdge>, Vec<NodeId>> {
    fn into_tree(self) -> SpqrTree {
        SpqrTree {
            root: self.root,
            node_types: self.node_types,
            node_parents: self.node_parents,
            children_offsets: self.children_offsets,
            children: self.children,
            skeleton_offsets: self.skeleton_offsets,
            skeleton_edges: self.skeleton_edges,
            node_mapping_offsets: self.node_mapping_offsets,
            node_mapping: self.node_mapping,
            skeleton_num_nodes: self.skeleton_num_nodes,
            edge_to_tree_node: self.edge_to_tree_node,
            min_real_per_node: self.min_real_per_node,
        }
    }

    fn into_payload_tree(self) -> SpqrPayloadTree {
        SpqrPayloadTree {
            root: self.root,
            node_types: self.node_types,
            node_parents: self.node_parents,
            children_offsets: self.children_offsets,
            children: self.children,
            skeleton_offsets: self.skeleton_offsets,
            skeleton_edges: PayloadSkeletonEdges::Plain(self.skeleton_edges),
            node_mapping_offsets: self.node_mapping_offsets,
            node_mapping: PayloadNodeMapping::Plain(self.node_mapping),
            skeleton_num_nodes: self.skeleton_num_nodes,
        }
    }
}

impl AssembledSpqrTree<PackedSkeletonEdges, PackedU40Column> {
    fn into_payload_tree(self) -> SpqrPayloadTree {
        let pairs = self.payload_pairs.unwrap_or_else(PackedVirtualPairs::empty);
        SpqrPayloadTree {
            root: self.root,
            node_types: self.node_types,
            node_parents: self.node_parents,
            children_offsets: self.children_offsets,
            children: self.children,
            skeleton_offsets: self.skeleton_offsets,
            skeleton_edges: PayloadSkeletonEdges::Packed {
                edges: self.skeleton_edges,
                pairs,
            },
            node_mapping_offsets: self.node_mapping_offsets,
            node_mapping: PayloadNodeMapping::Packed(self.node_mapping),
            skeleton_num_nodes: self.skeleton_num_nodes,
        }
    }
}

struct SpqrTreeBuilder<E, M, const TRACK_EDGE_MAPPING: bool> {
    node_types: Vec<SpqrNodeType>,
    node_parents: Vec<TreeNodeId>,
    skeleton_num_nodes: Vec<u64>,
    skeleton_offsets: Vec<u64>,
    skeleton_edges: E,
    node_mapping_offsets: Vec<u64>,
    node_mapping: M,
    edge_to_tree_node: Vec<TreeNodeId>,
    min_real_per_node: Vec<u64>,
}

impl<E: SkeletonEdgeStorage, M: NodeMappingStorage, const TRACK_EDGE_MAPPING: bool>
    SpqrTreeBuilder<E, M, TRACK_EDGE_MAPPING>
{
    #[inline]
    fn new(num_edges: usize, nodes: usize, skeleton_edges: usize) -> Self {
        let mut skeleton_offsets = Vec::with_capacity(nodes + 1);
        skeleton_offsets.push(0);
        let mut node_mapping_offsets = Vec::with_capacity(nodes + 1);
        node_mapping_offsets.push(0);
        SpqrTreeBuilder {
            node_types: Vec::with_capacity(nodes),
            node_parents: Vec::with_capacity(nodes),
            skeleton_num_nodes: Vec::with_capacity(nodes),
            skeleton_offsets,
            skeleton_edges: E::with_capacity(skeleton_edges),
            node_mapping_offsets,
            node_mapping: M::empty(),
            edge_to_tree_node: if TRACK_EDGE_MAPPING {
                vec![TreeNodeId::INVALID; num_edges]
            } else {
                Vec::new()
            },
            min_real_per_node: if TRACK_EDGE_MAPPING {
                Vec::with_capacity(nodes)
            } else {
                Vec::new()
            },
        }
    }

    #[inline(always)]
    fn begin_node(&mut self, node_type: SpqrNodeType) -> TreeNodeId {
        let tid = TreeNodeId(self.node_types.len() as u64);
        self.node_types.push(node_type);
        self.node_parents.push(TreeNodeId::INVALID);
        self.skeleton_num_nodes.push(0);
        if TRACK_EDGE_MAPPING {
            self.min_real_per_node.push(u64::MAX);
        }
        tid
    }

    #[inline(always)]
    fn push_edge(&mut self, tid: TreeNodeId, edge: SkeletonEdge) {
        if edge.real_edge.is_valid() {
            if TRACK_EDGE_MAPPING {
                self.edge_to_tree_node[edge.real_edge.idx()] = tid;
                let minimum = &mut self.min_real_per_node[tid.idx()];
                *minimum = (*minimum).min(edge.real_edge.0);
            }
        }
        self.skeleton_edges.push(edge);
    }

    #[inline(always)]
    fn finish_node(&mut self, tid: TreeNodeId, num_nodes: u64) {
        self.skeleton_num_nodes[tid.idx()] = num_nodes;
        self.skeleton_offsets.push(self.skeleton_edges.len() as u64);
        self.node_mapping_offsets
            .push(self.node_mapping.node_count() as u64);
    }

    #[inline(always)]
    fn skeleton_edge(&self, tree_node: TreeNodeId, edge_idx: usize) -> SkeletonEdge {
        let start = self.skeleton_offsets[tree_node.idx()] as usize;
        self.skeleton_edges.get(start + edge_idx)
    }

    fn pair_virtual(
        &mut self,
        first_tree: TreeNodeId,
        first_edge: usize,
        second_tree: TreeNodeId,
        second_edge: usize,
    ) {
        let first = self.skeleton_offsets[first_tree.idx()] as usize + first_edge;
        let second = self.skeleton_offsets[second_tree.idx()] as usize + second_edge;
        self.skeleton_edges.pair_virtual(
            first,
            second,
            first_tree,
            first_edge as u64,
            second_tree,
            second_edge as u64,
        );
    }

    fn clear_virtual(&mut self, tree_node: TreeNodeId, edge_idx: usize) {
        let index = self.skeleton_offsets[tree_node.idx()] as usize + edge_idx;
        self.skeleton_edges.clear_virtual(index);
    }

    #[inline(always)]
    fn skeleton_edges_len(&self, tree_node: TreeNodeId) -> usize {
        let start = self.skeleton_offsets[tree_node.idx()] as usize;
        let end = self.skeleton_offsets[tree_node.idx() + 1] as usize;
        end - start
    }

    #[inline(always)]
    fn num_nodes(&self) -> usize {
        self.node_types.len()
    }

    fn finalize_with_children(
        self,
        root: TreeNodeId,
        children_offsets: Vec<u64>,
        children: Vec<TreeNodeId>,
        payload_pairs: Option<PackedVirtualPairs>,
    ) -> AssembledSpqrTree<E, M> {
        AssembledSpqrTree {
            root,
            node_types: self.node_types,
            node_parents: self.node_parents,
            children_offsets,
            children,
            skeleton_offsets: self.skeleton_offsets,
            skeleton_edges: self.skeleton_edges,
            node_mapping_offsets: self.node_mapping_offsets,
            node_mapping: self.node_mapping,
            skeleton_num_nodes: self.skeleton_num_nodes,
            edge_to_tree_node: self.edge_to_tree_node,
            min_real_per_node: self.min_real_per_node,
            payload_pairs,
        }
    }

    fn finalize_empty(self) -> AssembledSpqrTree<E, M> {
        AssembledSpqrTree {
            root: TreeNodeId::INVALID,
            node_types: Vec::new(),
            node_parents: Vec::new(),
            children_offsets: vec![0],
            children: Vec::new(),
            skeleton_offsets: vec![0],
            skeleton_edges: E::with_capacity(0),
            node_mapping_offsets: vec![0],
            node_mapping: M::empty(),
            skeleton_num_nodes: Vec::new(),
            edge_to_tree_node: self.edge_to_tree_node,
            min_real_per_node: Vec::new(),
            payload_pairs: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StackEdge {
    src: u64,
    dst: u64,
    eid: u64,
}

const STACK_EDGE_PACK_MIN: usize = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PackedStackEdge {
    low: u64,
    high: u64,
}

impl PackedStackEdge {
    #[inline(always)]
    fn encode(value: u64) -> u64 {
        if value == INVALID {
            U40_MAX
        } else {
            assert!(value < U40_MAX, "stack edge value does not fit in 40 bits");
            value
        }
    }

    #[inline(always)]
    fn decode(value: u64) -> u64 {
        if value == U40_MAX {
            INVALID
        } else {
            value
        }
    }

    #[inline(always)]
    fn fits(edge: StackEdge) -> bool {
        [edge.src, edge.dst, edge.eid]
            .into_iter()
            .all(|value| value == INVALID || value < U40_MAX)
    }

    #[inline(always)]
    fn pack(edge: StackEdge) -> Self {
        let src = Self::encode(edge.src);
        let dst = Self::encode(edge.dst);
        let eid = Self::encode(edge.eid);
        Self {
            low: src | ((dst & 0x00ff_ffff) << 40),
            high: (dst >> 24) | (eid << 16),
        }
    }

    #[inline(always)]
    fn unpack(self) -> StackEdge {
        StackEdge {
            src: Self::decode(self.low & U40_MAX),
            dst: Self::decode((self.low >> 40) | ((self.high & 0xffff) << 24)),
            eid: Self::decode((self.high >> 16) & U40_MAX),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum StackEdges {
    Plain(Vec<StackEdge>),
    Packed(Vec<PackedStackEdge>),
}

impl Default for StackEdges {
    fn default() -> Self {
        Self::Plain(Vec::new())
    }
}

impl From<Vec<StackEdge>> for StackEdges {
    fn from(edges: Vec<StackEdge>) -> Self {
        if edges.len() >= STACK_EDGE_PACK_MIN && edges.iter().copied().all(PackedStackEdge::fits) {
            Self::Packed(edges.into_iter().map(PackedStackEdge::pack).collect())
        } else {
            Self::Plain(edges)
        }
    }
}

impl StackEdges {
    fn take_plain(&mut self) -> Vec<StackEdge> {
        match std::mem::take(self) {
            Self::Plain(edges) => edges,
            Self::Packed(edges) => edges.into_iter().map(PackedStackEdge::unpack).collect(),
        }
    }
}

trait ComponentEdges: Clone + fmt::Debug + PartialEq + Eq + Default + Send + Sync {
    const PREFERS_PACKED_REMAINDER: bool;

    fn with_capacity(capacity: usize) -> Self;
    fn from_edges(edges: Vec<StackEdge>) -> Self;
    fn from_packed_edges(edges: Vec<PackedStackEdge>) -> Self;
    fn len(&self) -> usize;
    fn get(&self, index: usize) -> StackEdge;
    fn push(&mut self, edge: StackEdge);
    fn swap_remove(&mut self, index: usize) -> StackEdge;
    fn retain(&mut self, keep: impl FnMut(&StackEdge) -> bool);
    fn map_in_place(&mut self, map: impl FnMut(StackEdge) -> StackEdge);
    fn append(&mut self, other: &mut Self);
    fn compact(&mut self);
    fn for_each(&self, visit: impl FnMut(StackEdge));
    fn for_each_indexed(&self, visit: impl FnMut(usize, StackEdge));

    #[inline(always)]
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl ComponentEdges for Vec<StackEdge> {
    const PREFERS_PACKED_REMAINDER: bool = false;

    #[inline]
    fn with_capacity(capacity: usize) -> Self {
        Vec::with_capacity(capacity)
    }

    #[inline(always)]
    fn from_edges(edges: Vec<StackEdge>) -> Self {
        edges
    }

    #[inline(always)]
    fn from_packed_edges(edges: Vec<PackedStackEdge>) -> Self {
        edges.into_iter().map(PackedStackEdge::unpack).collect()
    }

    #[inline(always)]
    fn len(&self) -> usize {
        Vec::len(self)
    }

    #[inline(always)]
    fn get(&self, index: usize) -> StackEdge {
        self[index]
    }

    #[inline(always)]
    fn push(&mut self, edge: StackEdge) {
        Vec::push(self, edge);
    }

    #[inline(always)]
    fn swap_remove(&mut self, index: usize) -> StackEdge {
        Vec::swap_remove(self, index)
    }

    #[inline(always)]
    fn retain(&mut self, keep: impl FnMut(&StackEdge) -> bool) {
        Vec::retain(self, keep);
    }

    #[inline(always)]
    fn map_in_place(&mut self, mut map: impl FnMut(StackEdge) -> StackEdge) {
        for edge in self {
            *edge = map(*edge);
        }
    }

    #[inline(always)]
    fn append(&mut self, other: &mut Self) {
        Vec::append(self, other);
    }

    #[inline(always)]
    fn compact(&mut self) {}

    #[inline(always)]
    fn for_each(&self, mut visit: impl FnMut(StackEdge)) {
        for &edge in self {
            visit(edge);
        }
    }

    #[inline(always)]
    fn for_each_indexed(&self, mut visit: impl FnMut(usize, StackEdge)) {
        for (index, &edge) in self.iter().enumerate() {
            visit(index, edge);
        }
    }
}

impl ComponentEdges for StackEdges {
    const PREFERS_PACKED_REMAINDER: bool = true;

    #[inline]
    fn with_capacity(capacity: usize) -> Self {
        Self::Packed(Vec::with_capacity(capacity))
    }

    #[inline(always)]
    fn from_edges(edges: Vec<StackEdge>) -> Self {
        Self::from(edges)
    }

    #[inline(always)]
    fn from_packed_edges(edges: Vec<PackedStackEdge>) -> Self {
        Self::Packed(edges)
    }

    #[inline(always)]
    fn len(&self) -> usize {
        match self {
            Self::Plain(edges) => edges.len(),
            Self::Packed(edges) => edges.len(),
        }
    }

    #[inline(always)]
    fn get(&self, index: usize) -> StackEdge {
        match self {
            Self::Plain(edges) => edges[index],
            Self::Packed(edges) => edges[index].unpack(),
        }
    }

    #[inline(always)]
    fn push(&mut self, edge: StackEdge) {
        match self {
            Self::Plain(edges) => edges.push(edge),
            Self::Packed(edges) if PackedStackEdge::fits(edge) => {
                edges.push(PackedStackEdge::pack(edge));
            }
            Self::Packed(_) => {
                let mut plain = self.take_plain();
                plain.push(edge);
                *self = Self::Plain(plain);
            }
        }
    }

    #[inline(always)]
    fn swap_remove(&mut self, index: usize) -> StackEdge {
        match self {
            Self::Plain(edges) => edges.swap_remove(index),
            Self::Packed(edges) => edges.swap_remove(index).unpack(),
        }
    }

    #[inline(always)]
    fn retain(&mut self, mut keep: impl FnMut(&StackEdge) -> bool) {
        match self {
            Self::Plain(edges) => edges.retain(keep),
            Self::Packed(edges) => edges.retain(|edge| keep(&edge.unpack())),
        }
    }

    #[inline(always)]
    fn map_in_place(&mut self, mut map: impl FnMut(StackEdge) -> StackEdge) {
        match self {
            Self::Plain(edges) => {
                for edge in edges {
                    *edge = map(*edge);
                }
            }
            Self::Packed(edges) => {
                let mut overflow = None;
                for (index, edge) in edges.iter_mut().enumerate() {
                    let mapped = map(edge.unpack());
                    if !PackedStackEdge::fits(mapped) {
                        overflow = Some((index, mapped));
                        break;
                    }
                    *edge = PackedStackEdge::pack(mapped);
                }
                if let Some((index, mapped)) = overflow {
                    let packed = match std::mem::take(self) {
                        Self::Packed(edges) => edges,
                        Self::Plain(_) => unreachable!(),
                    };
                    let mut plain = packed
                        .into_iter()
                        .map(PackedStackEdge::unpack)
                        .collect::<Vec<_>>();
                    plain[index] = mapped;
                    for edge in &mut plain[index + 1..] {
                        *edge = map(*edge);
                    }
                    *self = Self::Plain(plain);
                }
            }
        }
    }

    #[inline(always)]
    fn append(&mut self, other: &mut Self) {
        let right = std::mem::take(other);
        match (std::mem::take(self), right) {
            (Self::Plain(mut left), Self::Plain(mut right)) => {
                left.append(&mut right);
                *self = Self::Plain(left);
            }
            (Self::Packed(mut left), Self::Packed(mut right)) => {
                left.append(&mut right);
                *self = Self::Packed(left);
            }
            (Self::Plain(mut left), Self::Packed(right)) => {
                left.extend(right.into_iter().map(PackedStackEdge::unpack));
                *self = Self::Plain(left);
            }
            (Self::Packed(left), Self::Plain(mut right)) => {
                if right.iter().copied().all(PackedStackEdge::fits) {
                    let mut packed = left;
                    packed.extend(right.drain(..).map(PackedStackEdge::pack));
                    *self = Self::Packed(packed);
                } else {
                    let mut plain = left
                        .into_iter()
                        .map(PackedStackEdge::unpack)
                        .collect::<Vec<_>>();
                    plain.append(&mut right);
                    *self = Self::Plain(plain);
                }
            }
        }
    }

    #[inline(always)]
    fn compact(&mut self) {
        let should_pack = match self {
            Self::Plain(edges) => {
                edges.len() >= STACK_EDGE_PACK_MIN
                    && edges.iter().copied().all(PackedStackEdge::fits)
            }
            Self::Packed(_) => false,
        };
        if should_pack {
            let plain = self.take_plain();
            *self = Self::Packed(plain.into_iter().map(PackedStackEdge::pack).collect());
        }
    }

    #[inline(always)]
    fn for_each(&self, mut visit: impl FnMut(StackEdge)) {
        match self {
            Self::Plain(edges) => {
                for &edge in edges {
                    visit(edge);
                }
            }
            Self::Packed(edges) => {
                for &edge in edges {
                    visit(edge.unpack());
                }
            }
        }
    }

    #[inline(always)]
    fn for_each_indexed(&self, mut visit: impl FnMut(usize, StackEdge)) {
        match self {
            Self::Plain(edges) => {
                for (index, &edge) in edges.iter().enumerate() {
                    visit(index, edge);
                }
            }
            Self::Packed(edges) => {
                for (index, &edge) in edges.iter().enumerate() {
                    visit(index, edge.unpack());
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SplitComponent<CE: ComponentEdges = Vec<StackEdge>> {
    edges: CE,
    pole_a: u64,
    pole_b: u64,
}

impl<CE: ComponentEdges> SplitComponent<CE> {
    fn new(edges: Vec<StackEdge>, pole_a: u64, pole_b: u64) -> Self {
        Self {
            edges: CE::from_edges(edges),
            pole_a,
            pole_b,
        }
    }

    fn from_array<const N: usize>(edges: [StackEdge; N], pole_a: u64, pole_b: u64) -> Self {
        let mut storage = CE::with_capacity(N);
        for edge in edges {
            storage.push(edge);
        }
        Self {
            edges: storage,
            pole_a,
            pole_b,
        }
    }
}

#[inline]
fn next_virtual_id(next_virtual: &mut u64) -> u64 {
    let vid = *next_virtual;
    assert!(vid != INVALID, "wide SPQR virtual edge id overflow");
    *next_virtual = (*next_virtual)
        .checked_add(1)
        .expect("wide SPQR virtual edge id overflow");
    vid
}

#[derive(Default, Clone, Copy, Debug)]
pub(crate) struct SpqrRawBuildTimings {
    pub t_self_loop_scan_us: u64,
    pub t_tree_total_us: u64,
    pub t_precheck_us: u64,
    pub t_split_multi_edges_us: u64,
    pub t_work_graph_us: u64,
    pub t_triconn_us: u64,
    pub t_relabel_us: u64,
    pub t_combine_us: u64,
    pub t_merge_us: u64,
    pub t_assemble_us: u64,

    pub c_multi_components: u64,
    pub c_triconn_components: u64,
    pub c_precombine_components: u64,
    pub c_combined_components: u64,
    pub c_merged_components: u64,
    pub c_merged_real_edges: u64,
    pub c_merged_virtual_incidences: u64,
    pub c_virtual_id_span: u64,
    pub c_tree_nodes: u64,
    pub c_tree_edges: u64,
    pub c_tree_skeleton_edges: u64,
    pub c_tree_virtual_incidences: u64,
}

#[derive(Clone, Copy)]
struct SelfLoopFlags<'a> {
    flags: Option<&'a [bool]>,
}

impl<'a> SelfLoopFlags<'a> {
    #[inline(always)]
    fn none() -> Self {
        Self { flags: None }
    }

    #[inline(always)]
    fn from_slice(flags: &'a [bool]) -> Self {
        Self { flags: Some(flags) }
    }

    #[inline(always)]
    fn is_loop(self, idx: usize) -> bool {
        match self.flags {
            Some(flags) => flags[idx],
            None => false,
        }
    }

    #[inline]
    fn count(self) -> usize {
        match self.flags {
            Some(flags) => flags.iter().filter(|&&b| b).count(),
            None => 0,
        }
    }

    #[inline]
    fn any(self) -> bool {
        match self.flags {
            Some(flags) => flags.iter().any(|&b| b),
            None => false,
        }
    }
}

trait TreeAssembly {
    type Tree;

    fn from_tree(tree: SpqrTree) -> Self::Tree;
    fn release_input_graph<G: GraphAccess>(_graph: &mut G) {}
    fn release_input_edges<G: GraphAccess>(_graph: &mut G) {}
    fn assemble<CE: ComponentEdges>(
        node_count: usize,
        edge_count: usize,
        components: Vec<SplitComponent<CE>>,
        component_types: Vec<SpqrNodeType>,
        next_virtual: u64,
        track_edge_mapping: bool,
    ) -> Self::Tree;
    fn counts(tree: &Self::Tree) -> (usize, usize, usize, usize);
}

struct GeneralTreeAssembly;

impl TreeAssembly for GeneralTreeAssembly {
    type Tree = SpqrTree;

    fn from_tree(tree: SpqrTree) -> Self::Tree {
        tree
    }

    fn assemble<CE: ComponentEdges>(
        node_count: usize,
        edge_count: usize,
        components: Vec<SplitComponent<CE>>,
        component_types: Vec<SpqrNodeType>,
        next_virtual: u64,
        track_edge_mapping: bool,
    ) -> Self::Tree {
        assemble_spqr_tree(
            node_count,
            edge_count,
            components,
            component_types,
            next_virtual,
            track_edge_mapping,
        )
    }

    fn counts(tree: &Self::Tree) -> (usize, usize, usize, usize) {
        (
            tree.len(),
            tree.children.len(),
            tree.skeleton_edges.len(),
            tree.skeleton_edges
                .iter()
                .filter(|edge| edge.virtual_id != INVALID)
                .count(),
        )
    }
}

struct PayloadTreeAssembly;

impl TreeAssembly for PayloadTreeAssembly {
    type Tree = SpqrPayloadTree;

    fn from_tree(tree: SpqrTree) -> Self::Tree {
        SpqrPayloadTree {
            root: tree.root,
            node_types: tree.node_types,
            node_parents: tree.node_parents,
            children_offsets: tree.children_offsets,
            children: tree.children,
            skeleton_offsets: tree.skeleton_offsets,
            skeleton_edges: PayloadSkeletonEdges::Plain(tree.skeleton_edges),
            node_mapping_offsets: tree.node_mapping_offsets,
            node_mapping: PayloadNodeMapping::Plain(tree.node_mapping),
            skeleton_num_nodes: tree.skeleton_num_nodes,
        }
    }

    fn release_input_edges<G: GraphAccess>(graph: &mut G) {
        graph.release_edge_storage();
    }

    fn release_input_graph<G: GraphAccess>(graph: &mut G) {
        graph.release_adjacency();
        graph.release_edge_storage();
    }

    fn assemble<CE: ComponentEdges>(
        node_count: usize,
        edge_count: usize,
        components: Vec<SplitComponent<CE>>,
        component_types: Vec<SpqrNodeType>,
        next_virtual: u64,
        track_edge_mapping: bool,
    ) -> Self::Tree {
        debug_assert!(!track_edge_mapping);
        assemble_spqr_payload_tree(
            node_count,
            edge_count,
            components,
            component_types,
            next_virtual,
        )
    }

    fn counts(tree: &Self::Tree) -> (usize, usize, usize, usize) {
        let skeleton_edges = tree.skeleton_edge_count();
        let virtual_edges = (0..skeleton_edges)
            .filter(|&index| tree.skeleton_edge(index).virtual_id != INVALID)
            .count();
        (
            tree.len(),
            tree.children.len(),
            skeleton_edges,
            virtual_edges,
        )
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MultiEdgeMode {
    General,
    KnownSimple,
    Bounded,
}

/// Build an SPQR tree
///
/// we returns here an SpqrResult whose tree is a SPQR tree and whose self_loops contains any (v,v) edges found in the input
pub fn build_spqr(graph: &Graph) -> SpqrResult {
    let m = graph.num_edges();

    let mut self_loops: Vec<EdgeId> = Vec::new();
    let mut is_self_loop = vec![false; m];
    for i in 0..m {
        let e = graph.edge(EdgeId(i as u64));
        if e.src == e.dst {
            is_self_loop[i] = true;
            self_loops.push(EdgeId(i as u64));
        }
    }

    let mut tree = build_spqr_tree_filtered(graph, &is_self_loop);
    tree.canonicalize_root();
    tree.canonicalize_skeleton_node_order();
    tree.canonicalize_skeleton_edge_orientation();
    tree.move_root_to_zero();
    SpqrResult { tree, self_loops }
}

pub(crate) fn build_spqr_releasing(graph: &mut Graph) -> SpqrResult {
    let m = graph.num_edges();
    let mut self_loops = Vec::new();
    let mut is_self_loop = vec![false; m];
    for i in 0..m {
        let e = graph.edge(EdgeId(i as u64));
        if e.src == e.dst {
            is_self_loop[i] = true;
            self_loops.push(EdgeId(i as u64));
        }
    }

    let mut tree = build_spqr_tree_filtered_releasing_impl(
        graph,
        SelfLoopFlags::from_slice(&is_self_loop),
        None,
        MultiEdgeMode::General,
        true,
    );
    tree.canonicalize_root();
    tree.canonicalize_skeleton_node_order();
    tree.canonicalize_skeleton_edge_orientation();
    tree.move_root_to_zero();
    SpqrResult { tree, self_loops }
}

pub(crate) fn build_spqr_payload_releasing(graph: &mut Graph) -> SpqrPayloadResult {
    let m = graph.num_edges();
    let mut is_self_loop = vec![false; m];
    for i in 0..m {
        let edge = graph.edge(EdgeId(i as u64));
        if edge.src == edge.dst {
            is_self_loop[i] = true;
        }
    }

    let tree = build_spqr_payload_tree_filtered_releasing_impl(
        graph,
        SelfLoopFlags::from_slice(&is_self_loop),
        None,
        if m >= PARALLEL_GRAPH_MIN_EDGES {
            MultiEdgeMode::Bounded
        } else {
            MultiEdgeMode::General
        },
    );
    SpqrPayloadResult { tree }
}

pub(crate) fn build_spqr_raw(graph: &Graph) -> SpqrResult {
    let m = graph.num_edges();

    let mut self_loops: Vec<EdgeId> = Vec::new();
    let mut is_self_loop = vec![false; m];
    for i in 0..m {
        let e = graph.edge(EdgeId(i as u64));
        if e.src == e.dst {
            is_self_loop[i] = true;
            self_loops.push(EdgeId(i as u64));
        }
    }

    let tree = build_spqr_tree_filtered(graph, &is_self_loop);
    SpqrResult { tree, self_loops }
}

pub(crate) fn build_spqr_raw_no_self_loops(graph: &Graph) -> SpqrResult {
    debug_assert!((0..graph.num_edges()).all(|i| {
        let e = graph.edge(EdgeId(i as u64));
        e.src != e.dst
    }));
    let tree =
        build_spqr_tree_filtered_impl(graph, SelfLoopFlags::none(), None, MultiEdgeMode::General);
    SpqrResult {
        tree,
        self_loops: Vec::new(),
    }
}

pub(crate) fn build_spqr_raw_no_multi_edges(graph: &Graph) -> SpqrResult {
    let m = graph.num_edges();
    let mut self_loops: Vec<EdgeId> = Vec::new();
    let mut is_self_loop = vec![false; m];
    for i in 0..m {
        let e = graph.edge(EdgeId(i as u64));
        if e.src == e.dst {
            is_self_loop[i] = true;
            self_loops.push(EdgeId(i as u64));
        }
    }

    let tree = build_spqr_tree_filtered_impl(
        graph,
        SelfLoopFlags::from_slice(&is_self_loop),
        None,
        MultiEdgeMode::KnownSimple,
    );
    SpqrResult { tree, self_loops }
}

pub(crate) fn build_spqr_raw_timed(graph: &Graph) -> (SpqrResult, SpqrRawBuildTimings) {
    let m = graph.num_edges();
    let mut timings = SpqrRawBuildTimings::default();

    let t0 = Instant::now();
    let mut self_loops: Vec<EdgeId> = Vec::new();
    let mut is_self_loop = vec![false; m];
    for i in 0..m {
        let e = graph.edge(EdgeId(i as u64));
        if e.src == e.dst {
            is_self_loop[i] = true;
            self_loops.push(EdgeId(i as u64));
        }
    }
    timings.t_self_loop_scan_us = t0.elapsed().as_micros() as u64;

    let t1 = Instant::now();
    let tree = build_spqr_tree_filtered_impl(
        graph,
        SelfLoopFlags::from_slice(&is_self_loop),
        Some(&mut timings),
        MultiEdgeMode::General,
    );
    timings.t_tree_total_us = t1.elapsed().as_micros() as u64;

    (SpqrResult { tree, self_loops }, timings)
}

pub(crate) fn build_spqr_raw_no_self_loops_timed(
    graph: &Graph,
) -> (SpqrResult, SpqrRawBuildTimings) {
    debug_assert!((0..graph.num_edges()).all(|i| {
        let e = graph.edge(EdgeId(i as u64));
        e.src != e.dst
    }));
    let mut timings = SpqrRawBuildTimings::default();
    let t1 = Instant::now();
    let tree = build_spqr_tree_filtered_impl(
        graph,
        SelfLoopFlags::none(),
        Some(&mut timings),
        MultiEdgeMode::General,
    );
    timings.t_tree_total_us = t1.elapsed().as_micros() as u64;

    (
        SpqrResult {
            tree,
            self_loops: Vec::new(),
        },
        timings,
    )
}

pub(crate) fn build_spqr_raw_no_multi_edges_timed(
    graph: &Graph,
) -> (SpqrResult, SpqrRawBuildTimings) {
    let m = graph.num_edges();
    let mut timings = SpqrRawBuildTimings::default();

    let t0 = Instant::now();
    let mut self_loops: Vec<EdgeId> = Vec::new();
    let mut is_self_loop = vec![false; m];
    for i in 0..m {
        let e = graph.edge(EdgeId(i as u64));
        if e.src == e.dst {
            is_self_loop[i] = true;
            self_loops.push(EdgeId(i as u64));
        }
    }
    timings.t_self_loop_scan_us = t0.elapsed().as_micros() as u64;

    let t1 = Instant::now();
    let tree = build_spqr_tree_filtered_impl(
        graph,
        SelfLoopFlags::from_slice(&is_self_loop),
        Some(&mut timings),
        MultiEdgeMode::KnownSimple,
    );
    timings.t_tree_total_us = t1.elapsed().as_micros() as u64;

    (SpqrResult { tree, self_loops }, timings)
}

/// Build an SPQR tree from a graph known to contain no self loops
///
/// we set panics in debug mode if a self loop is found.  For graphs that may contain self loops, use build_spqr instead.
pub fn build_spqr_tree(graph: &Graph) -> SpqrTree {
    let m = graph.num_edges();
    debug_assert!(
        (0..m).all(|i| {
            let e = graph.edge(EdgeId(i as u64));
            e.src != e.dst
        }),
        "Graph contains self-loops; use build_spqr() instead"
    );
    build_spqr_tree_filtered_impl(graph, SelfLoopFlags::none(), None, MultiEdgeMode::General)
}

fn build_spqr_tree_filtered(graph: &Graph, is_self_loop: &[bool]) -> SpqrTree {
    build_spqr_tree_filtered_impl(
        graph,
        SelfLoopFlags::from_slice(is_self_loop),
        None,
        MultiEdgeMode::General,
    )
}

fn build_spqr_tree_filtered_impl(
    graph: &Graph,
    self_loops: SelfLoopFlags<'_>,
    timings: Option<&mut SpqrRawBuildTimings>,
    multi_edges: MultiEdgeMode,
) -> SpqrTree {
    let mut view = GraphRef(graph);
    build_spqr_tree_filtered_view::<_, GeneralTreeAssembly>(
        &mut view,
        self_loops,
        timings,
        multi_edges,
        true,
    )
}

fn build_spqr_tree_filtered_releasing_impl(
    graph: &mut Graph,
    self_loops: SelfLoopFlags<'_>,
    timings: Option<&mut SpqrRawBuildTimings>,
    multi_edges: MultiEdgeMode,
    track_edge_mapping: bool,
) -> SpqrTree {
    let mut view = GraphMut(graph);
    build_spqr_tree_filtered_view::<_, GeneralTreeAssembly>(
        &mut view,
        self_loops,
        timings,
        multi_edges,
        track_edge_mapping,
    )
}

fn build_spqr_payload_tree_filtered_releasing_impl(
    graph: &mut Graph,
    self_loops: SelfLoopFlags<'_>,
    timings: Option<&mut SpqrRawBuildTimings>,
    multi_edges: MultiEdgeMode,
) -> SpqrPayloadTree {
    let mut view = GraphMut(graph);
    build_spqr_tree_filtered_view::<_, PayloadTreeAssembly>(
        &mut view,
        self_loops,
        timings,
        multi_edges,
        false,
    )
}

fn build_spqr_tree_filtered_view<G: GraphAccess, A: TreeAssembly>(
    graph: &mut G,
    self_loops: SelfLoopFlags<'_>,
    timings: Option<&mut SpqrRawBuildTimings>,
    multi_edges: MultiEdgeMode,
    track_edge_mapping: bool,
) -> A::Tree {
    let compact_labels = graph.num_nodes() as u64 <= U40_MAX
        && (graph.num_edges() as u64) <= U40_MAX / 2
        && graph.num_edges() >= PARALLEL_GRAPH_MIN_EDGES;
    let compact_storage = compact_labels
        && (graph.num_edges() > u32::MAX as usize
            || triconn_packed_columns_save_enough(
                graph.num_nodes(),
                graph.num_edges(),
                graph.num_edges() as u64,
            ));
    if compact_storage {
        build_spqr_tree_filtered_view_impl::<G, A, StackEdges, PackedU40Column>(
            graph,
            self_loops,
            timings,
            multi_edges,
            track_edge_mapping,
        )
    } else if graph.num_edges() > u32::MAX as usize {
        build_spqr_tree_filtered_view_impl::<G, A, StackEdges, Vec<u64>>(
            graph,
            self_loops,
            timings,
            multi_edges,
            track_edge_mapping,
        )
    } else {
        build_spqr_tree_filtered_view_impl::<G, A, Vec<StackEdge>, Vec<u64>>(
            graph,
            self_loops,
            timings,
            multi_edges,
            track_edge_mapping,
        )
    }
}

#[inline(always)]
fn finish_spqr_tree<G: GraphAccess, A: TreeAssembly>(graph: &mut G, tree: SpqrTree) -> A::Tree {
    A::release_input_graph(graph);
    A::from_tree(tree)
}

fn build_spqr_tree_filtered_view_impl<
    G: GraphAccess,
    A: TreeAssembly,
    CE: ComponentEdges,
    L: U64Column,
>(
    graph: &mut G,
    self_loops: SelfLoopFlags<'_>,
    mut timings: Option<&mut SpqrRawBuildTimings>,
    multi_edges: MultiEdgeMode,
    track_edge_mapping: bool,
) -> A::Tree {
    macro_rules! add_timing {
        ($field:ident, $start:expr) => {
            if let Some(t) = timings.as_mut() {
                t.$field += $start.elapsed().as_micros() as u64;
            }
        };
    }

    let t_precheck = Instant::now();
    let n = graph.num_nodes();
    let m = graph.num_edges();
    let m_real = m - self_loops.count();

    if n == 0 || m_real == 0 {
        add_timing!(t_precheck_us, t_precheck);
        return finish_spqr_tree::<G, A>(graph, SpqrTree::empty(m));
    }
    if n == 1 {
        add_timing!(t_precheck_us, t_precheck);
        return finish_spqr_tree::<G, A>(graph, SpqrTree::empty(m));
    }
    if m_real == 1 {
        let mut eid_real = 0;
        for i in 0..m {
            if !self_loops.is_loop(i) {
                eid_real = i;
                break;
            }
        }
        let e = graph.edge(EdgeId(eid_real as u64));
        let edges = vec![SkeletonEdge {
            src: NodeId(0),
            dst: NodeId(1),
            real_edge: EdgeId(eid_real as u64),
            virtual_id: INVALID,
            twin_tree_node: TreeNodeId::INVALID,
            twin_edge_idx: INVALID,
        }];
        add_timing!(t_precheck_us, t_precheck);
        return finish_spqr_tree::<G, A>(
            graph,
            SpqrTree::single_node(m, SpqrNodeType::P, 2, edges, vec![e.src, e.dst]),
        );
    }

    // Count distinct non self loop endpoints
    let mut has_non_loop_node = [false, false];
    let mut all_between_01 = true;
    for i in 0..m {
        if self_loops.is_loop(i) {
            continue;
        }
        let e = graph.edge(EdgeId(i as u64));
        let (a, b) = (e.src.0.min(e.dst.0), e.src.0.max(e.dst.0));
        if a == 0 && b == 1 {
            has_non_loop_node[0] = true;
            has_non_loop_node[1] = true;
        } else {
            all_between_01 = false;
            break;
        }
    }
    if n == 2 || (all_between_01 && has_non_loop_node[0]) {
        add_timing!(t_precheck_us, t_precheck);
        return finish_spqr_tree::<G, A>(graph, build_parallel_case(graph.as_graph(), self_loops));
    }

    if let Some(tree) = try_build_simple_cycle(graph.as_graph(), self_loops) {
        add_timing!(t_precheck_us, t_precheck);
        return finish_spqr_tree::<G, A>(graph, tree);
    }
    add_timing!(t_precheck_us, t_precheck);

    let mut next_virtual = m as u64;
    let multi_comps: Vec<SplitComponent<CE>>;
    let mut owned_wg: Option<Graph> = None;
    let mut weid_to_label = L::with_capacity(0);
    let relabel_edges;

    if multi_edges == MultiEdgeMode::KnownSimple && m_real == m {
        multi_comps = Vec::new();
        relabel_edges = false;
    } else {
        let t_split = Instant::now();
        let split = match multi_edges {
            MultiEdgeMode::KnownSimple => MultiEdgeSplit::Simple,
            MultiEdgeMode::Bounded => split_multi_edges_bounded::<CE>(
                graph.as_graph(),
                &mut next_virtual,
            )
            .unwrap_or_else(|| {
                let (components, synthetic, consumed) =
                    split_multi_edges::<CE>(graph.as_graph(), &mut next_virtual, self_loops);
                MultiEdgeSplit::Split(components, synthetic, consumed)
            }),
            MultiEdgeMode::General => {
                let (components, synthetic, consumed) =
                    split_multi_edges::<CE>(graph.as_graph(), &mut next_virtual, self_loops);
                MultiEdgeSplit::Split(components, synthetic, consumed)
            }
        };
        add_timing!(t_split_multi_edges_us, t_split);

        let split = match split {
            MultiEdgeSplit::Simple if m_real == m => None,
            MultiEdgeSplit::Simple => Some((Vec::new(), Vec::new(), vec![false; m])),
            MultiEdgeSplit::Split(components, synthetic, consumed) => {
                Some((components, synthetic, consumed))
            }
        };
        if split.is_none() {
            multi_comps = Vec::new();
            relabel_edges = false;
        } else {
            let (split_multi_comps, synthetic, consumed) = split.unwrap();
            multi_comps = split_multi_comps;
            if let Some(t) = timings.as_mut() {
                t.c_multi_components = multi_comps.len() as u64;
            }
            if m_real == m && multi_comps.is_empty() {
                relabel_edges = false;
            } else {
                let t_work_graph = Instant::now();
                if let Some(labels) =
                    graph.compact_work_graph::<L>(&consumed, self_loops, &synthetic)
                {
                    weid_to_label = labels;
                } else {
                    let real_count = (0..m)
                        .filter(|&i| !consumed[i] && !self_loops.is_loop(i))
                        .count();
                    let total = real_count + synthetic.len();
                    let mut work = Graph::with_capacity(n, total);
                    work.add_nodes(n);
                    weid_to_label = L::with_capacity(total);
                    for i in 0..m {
                        if !consumed[i] && !self_loops.is_loop(i) {
                            let e = graph.edge(EdgeId(i as u64));
                            work.add_edge(e.src, e.dst);
                            weid_to_label.push_value(i as u64);
                        }
                    }
                    for &(a, b, vid) in &synthetic {
                        work.add_edge(NodeId(a), NodeId(b));
                        weid_to_label.push_value(vid);
                    }
                    owned_wg = Some(work);
                }
                add_timing!(t_work_graph_us, t_work_graph);
                relabel_edges = true;
            }
        }
    }

    let (wg_m, ref_weid) = {
        let wg = owned_wg.as_ref().unwrap_or(graph.as_graph());
        let wg_m = wg.num_edges();
        let mut found = EdgeId::INVALID;
        for i in 0..wg_m {
            let e = wg.edge(EdgeId(i as u64));
            if e.src != e.dst {
                found = EdgeId(i as u64);
                break;
            }
        }
        (wg_m, found)
    };
    let used_triconn = n >= 3 && wg_m >= 3 && ref_weid.is_valid();

    if owned_wg.is_some() {
        graph.release_adjacency();
        A::release_input_edges(graph);
    }

    let empty_consumed: Vec<bool> = Vec::new();
    let t_triconn = Instant::now();
    let mut wcomps = if used_triconn {
        if let Some(work) = owned_wg.as_mut() {
            let mut work_view = GraphMut(work);
            triconn_decompose_view::<CE, _>(
                &mut work_view,
                ref_weid,
                &mut next_virtual,
                &empty_consumed,
            )
        } else {
            triconn_decompose_view::<CE, _>(graph, ref_weid, &mut next_virtual, &empty_consumed)
        }
    } else {
        let wg = owned_wg.as_ref().unwrap_or(graph.as_graph());
        let edges: Vec<StackEdge> = (0..wg_m)
            .map(|i| {
                let e = wg.edge(EdgeId(i as u64));
                StackEdge {
                    src: e.src.0,
                    dst: e.dst.0,
                    eid: i as u64,
                }
            })
            .collect();
        if edges.is_empty() {
            Vec::new()
        } else {
            let (pa, pb) = (edges[0].src, edges[0].dst);
            vec![SplitComponent::new(edges, pa, pb)]
        }
    };
    drop(owned_wg);
    if let Some(t) = timings.as_mut() {
        t.c_triconn_components = wcomps.len() as u64;
        t.c_precombine_components = t.c_multi_components.saturating_add(wcomps.len() as u64);
    }
    add_timing!(t_triconn_us, t_triconn);

    let t_relabel = Instant::now();
    if relabel_edges {
        for comp in &mut wcomps {
            comp.edges.map_in_place(|mut se| {
                let i = se.eid as usize;
                if i < weid_to_label.len() {
                    se.eid = weid_to_label.value(i);
                }
                se
            });
        }
    }
    drop(weid_to_label);
    add_timing!(t_relabel_us, t_relabel);
    A::release_input_edges(graph);

    let t_combine = Instant::now();
    let mut all = combine_components(multi_comps, wcomps, &mut next_virtual);
    if let Some(t) = timings.as_mut() {
        t.c_combined_components = all.len() as u64;
    }
    add_timing!(t_combine_us, t_combine);

    let t_merge = Instant::now();
    let component_types = merge_same_type_components(&mut all, n, m);
    if let Some(t) = timings.as_mut() {
        t.c_merged_components = all.len() as u64;
        t.c_virtual_id_span = next_virtual.saturating_sub(m as u64);
        let mut real_edges = 0u64;
        let mut virtual_incidences = 0u64;
        for comp in &all {
            comp.edges.for_each(|e| {
                if (e.eid as usize) < m {
                    real_edges = real_edges.saturating_add(1);
                } else {
                    virtual_incidences = virtual_incidences.saturating_add(1);
                }
            });
        }
        t.c_merged_real_edges = real_edges;
        t.c_merged_virtual_incidences = virtual_incidences;
    }
    add_timing!(t_merge_us, t_merge);

    let t_assemble = Instant::now();
    let tree = A::assemble(n, m, all, component_types, next_virtual, track_edge_mapping);
    if let Some(t) = timings.as_mut() {
        let (nodes, edges, skeleton_edges, virtual_edges) = A::counts(&tree);
        t.c_tree_nodes = nodes as u64;
        t.c_tree_edges = edges as u64;
        t.c_tree_skeleton_edges = skeleton_edges as u64;
        t.c_tree_virtual_incidences = virtual_edges as u64;
    }
    add_timing!(t_assemble_us, t_assemble);
    tree
}

fn build_parallel_case(graph: &Graph, self_loops: SelfLoopFlags<'_>) -> SpqrTree {
    let m = graph.num_edges();
    let mut edges = Vec::new();
    let mut edge_to_tree_node = vec![TreeNodeId::INVALID; m];
    let mut min_real: u64 = u64::MAX;

    for i in 0..m {
        if self_loops.is_loop(i) {
            continue;
        }
        let e = graph.edge(EdgeId(i as u64));
        edges.push(SkeletonEdge {
            src: if e.src == NodeId(0) {
                NodeId(0)
            } else {
                NodeId(1)
            },
            dst: if e.src == NodeId(0) {
                NodeId(1)
            } else {
                NodeId(0)
            },
            real_edge: EdgeId(i as u64),
            virtual_id: INVALID,
            twin_tree_node: TreeNodeId::INVALID,
            twin_edge_idx: INVALID,
        });
        edge_to_tree_node[i] = TreeNodeId(0);
        if (i as u64) < min_real {
            min_real = i as u64;
        }
    }

    SpqrTree {
        root: TreeNodeId(0),
        node_types: vec![SpqrNodeType::P],
        node_parents: vec![TreeNodeId::INVALID],
        children_offsets: vec![0, 0],
        children: Vec::new(),
        skeleton_offsets: vec![0, edges.len() as u64],
        skeleton_edges: edges,
        node_mapping_offsets: vec![0, 2],
        node_mapping: vec![NodeId(0), NodeId(1)],
        skeleton_num_nodes: vec![2],
        edge_to_tree_node,
        min_real_per_node: vec![min_real],
    }
}

pub static FAST_CYCLE_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static FAST_CYCLE_HITS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn try_build_simple_cycle(graph: &Graph, self_loops: SelfLoopFlags<'_>) -> Option<SpqrTree> {
    FAST_CYCLE_CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let n = graph.num_nodes();
    let m = graph.num_edges();

    if n < 3 || m != n {
        return None;
    }
    // No self-loops allowed on the simple-cycle path.
    if self_loops.any() {
        return None;
    }
    // Every vertex has degree exactly 2.
    for v in 0..n {
        if graph.degree(NodeId(v as u64)) != 2 {
            return None;
        }
    }

    // walk the cycle starting at vertex 0
    let mut order: Vec<u64> = Vec::with_capacity(n);
    let mut edge_order: Vec<u64> = Vec::with_capacity(n);
    let mut visited = vec![false; n];

    let mut current: u64 = 0;
    let mut prev_edge: u64 = u64::MAX;
    visited[0] = true;
    order.push(0);

    for step in 0..n {
        // Pick the incident edge that isn'tprev_edge
        let mut next_node: u64 = u64::MAX;
        let mut next_edge: u64 = u64::MAX;
        for (nb, eid) in graph.neighbors(NodeId(current)) {
            if eid.0 != prev_edge {
                next_node = nb.0;
                next_edge = eid.0;
                break;
            }
        }
        if next_edge == u64::MAX {
            return None;
        }
        edge_order.push(next_edge);

        if step == n - 1 {
            if next_node != 0 {
                return None;
            }
            break;
        }
        if (next_node as usize) >= n || visited[next_node as usize] {
            return None;
        }
        visited[next_node as usize] = true;
        order.push(next_node);
        prev_edge = next_edge;
        current = next_node;
    }

    debug_assert_eq!(order.len(), n);
    debug_assert_eq!(edge_order.len(), n);

    let mut edges: Vec<SkeletonEdge> = Vec::with_capacity(n);
    for i in 0..n {
        edges.push(SkeletonEdge {
            src: NodeId(i as u64),
            dst: NodeId(((i + 1) % n) as u64),
            real_edge: EdgeId(edge_order[i]),
            virtual_id: INVALID,
            twin_tree_node: TreeNodeId::INVALID,
            twin_edge_idx: INVALID,
        });
    }
    let node_mapping: Vec<NodeId> = order.into_iter().map(NodeId).collect();

    FAST_CYCLE_HITS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Some(SpqrTree::single_node(
        m,
        SpqrNodeType::S,
        n as u64,
        edges,
        node_mapping,
    ))
}

trait ParallelPairEdge: Copy + Ord + Eq + Send {
    fn from_edge(a: u64, b: u64, eid: u64) -> Self;
    fn endpoints(self) -> (u64, u64);
    fn eid(self) -> u64;
    fn same_pair(self, other: Self) -> bool;
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct ParallelPairRun<R: ParallelPairEdge> {
    first: R,
    chunk: usize,
    start: usize,
    end: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct FullPairEdge {
    key: (u64, u64),
    eid: u64,
}

impl ParallelPairEdge for FullPairEdge {
    #[inline]
    fn from_edge(a: u64, b: u64, eid: u64) -> Self {
        Self {
            key: (a.min(b), a.max(b)),
            eid,
        }
    }

    #[inline]
    fn endpoints(self) -> (u64, u64) {
        self.key
    }

    #[inline]
    fn eid(self) -> u64 {
        self.eid
    }

    #[inline]
    fn same_pair(self, other: Self) -> bool {
        self.key == other.key
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct CompactPairEdge {
    key: u64,
    eid: u64,
}

impl ParallelPairEdge for CompactPairEdge {
    #[inline]
    fn from_edge(a: u64, b: u64, eid: u64) -> Self {
        let lo = a.min(b);
        let hi = a.max(b);
        debug_assert!(hi <= u32::MAX as u64);
        Self {
            key: (lo << 32) | hi,
            eid,
        }
    }

    #[inline]
    fn endpoints(self) -> (u64, u64) {
        (self.key >> 32, self.key & u32::MAX as u64)
    }

    #[inline]
    fn eid(self) -> u64 {
        self.eid
    }

    #[inline]
    fn same_pair(self, other: Self) -> bool {
        self.key == other.key
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct PackedU40PairEdge(u128);

const _: () = assert!(std::mem::size_of::<PackedU40PairEdge>() == 16);

impl ParallelPairEdge for PackedU40PairEdge {
    #[inline]
    fn from_edge(a: u64, b: u64, eid: u64) -> Self {
        let lo = a.min(b);
        let hi = a.max(b);
        debug_assert!(lo < U40_MAX && hi < U40_MAX && eid < U40_MAX);
        Self(((lo as u128) << 80) | ((hi as u128) << 40) | eid as u128)
    }

    #[inline]
    fn endpoints(self) -> (u64, u64) {
        (
            (self.0 >> 80) as u64,
            ((self.0 >> 40) & U40_MAX as u128) as u64,
        )
    }

    #[inline]
    fn eid(self) -> u64 {
        (self.0 & U40_MAX as u128) as u64
    }

    #[inline]
    fn same_pair(self, other: Self) -> bool {
        self.0 >> 40 == other.0 >> 40
    }
}

#[allow(clippy::type_complexity)]
fn split_multi_edges_with_record<R: ParallelPairEdge, CE: ComponentEdges>(
    graph: &Graph,
    next_virtual: &mut u64,
    self_loops: SelfLoopFlags<'_>,
) -> (Vec<SplitComponent<CE>>, Vec<(u64, u64, u64)>, Vec<bool>) {
    let threads = spqr_thread_count().min(graph.num_edges().max(1));
    split_multi_edges_with_record_threads::<R, CE>(graph, next_virtual, self_loops, threads)
}

#[allow(clippy::type_complexity)]
fn split_multi_edges_with_record_threads<R: ParallelPairEdge, CE: ComponentEdges>(
    graph: &Graph,
    next_virtual: &mut u64,
    self_loops: SelfLoopFlags<'_>,
    threads: usize,
) -> (Vec<SplitComponent<CE>>, Vec<(u64, u64, u64)>, Vec<bool>) {
    fn emit_parallel_group<CE: ComponentEdges>(
        a: u64,
        b: u64,
        eids: impl IntoIterator<Item = u64>,
        edge_count: usize,
        graph: &Graph,
        next_virtual: &mut u64,
        p_comps: &mut Vec<SplitComponent<CE>>,
        consumed: &mut [bool],
        synthetic: &mut Vec<(u64, u64, u64)>,
    ) {
        if edge_count < 2 {
            return;
        }
        let vid = next_virtual_id(next_virtual);
        let mut edges = CE::with_capacity(
            edge_count
                .checked_add(1)
                .expect("parallel edge group is too large"),
        );
        for eid in eids {
            let e = graph.edge(EdgeId(eid));
            edges.push(StackEdge {
                src: e.src.0,
                dst: e.dst.0,
                eid,
            });
            consumed[eid as usize] = true;
        }
        edges.push(StackEdge {
            src: a,
            dst: b,
            eid: vid,
        });
        p_comps.push(SplitComponent {
            edges,
            pole_a: a,
            pole_b: b,
        });
        synthetic.push((a, b, vid));
    }

    let m = graph.num_edges();
    let mut pairs: Vec<R> = Vec::with_capacity(m);
    for i in 0..m {
        if self_loops.is_loop(i) {
            continue;
        }
        let e = graph.edge(EdgeId(i as u64));
        pairs.push(R::from_edge(e.src.0, e.dst.0, i as u64));
    }
    let mut p_comps = Vec::new();
    let mut consumed = vec![false; m];
    let mut synthetic = Vec::new();

    const MIN_PAR_SORT_EDGES: usize = 1_000_000;
    let threads = threads.min(pairs.len().max(1));
    if threads > 1 && pairs.len() >= MIN_PAR_SORT_EDGES {
        let chunk = pairs.len().div_ceil(threads);
        thread::scope(|scope| {
            for chunk_pairs in pairs.chunks_mut(chunk) {
                scope.spawn(move || {
                    chunk_pairs.sort_unstable();
                });
            }
        });

        let mut ranges = Vec::new();
        let mut start = 0usize;
        while start < pairs.len() {
            let end = (start + chunk).min(pairs.len());
            ranges.push((start, end));
            start = end;
        }

        let run_end = |start: usize, end: usize| {
            let pair = pairs[start];
            let mut next = start + 1;
            while next < end && pair.same_pair(pairs[next]) {
                next += 1;
            }
            next
        };

        let mut heap = std::collections::BinaryHeap::new();
        for (chunk_id, &(start, end)) in ranges.iter().enumerate() {
            if start < end {
                heap.push(std::cmp::Reverse(ParallelPairRun {
                    first: pairs[start],
                    chunk: chunk_id,
                    start,
                    end: run_end(start, end),
                }));
            }
        }

        let mut runs = Vec::with_capacity(ranges.len());
        while let Some(std::cmp::Reverse(first)) = heap.peek().copied() {
            runs.clear();
            while let Some(std::cmp::Reverse(run)) = heap.peek().copied() {
                if !first.first.same_pair(run.first) {
                    break;
                }
                runs.push(heap.pop().expect("parallel run heap is empty").0);
            }

            let edge_count = runs
                .iter()
                .try_fold(0usize, |count, run| count.checked_add(run.end - run.start))
                .expect("parallel edge group is too large");
            if edge_count >= 2 {
                let (a, b) = first.first.endpoints();
                emit_parallel_group(
                    a,
                    b,
                    runs.iter()
                        .flat_map(|run| pairs[run.start..run.end].iter().map(|edge| edge.eid())),
                    edge_count,
                    graph,
                    next_virtual,
                    &mut p_comps,
                    &mut consumed,
                    &mut synthetic,
                );
            }

            for run in runs.drain(..) {
                let (_, chunk_end) = ranges[run.chunk];
                if run.end < chunk_end {
                    heap.push(std::cmp::Reverse(ParallelPairRun {
                        first: pairs[run.end],
                        chunk: run.chunk,
                        start: run.end,
                        end: run_end(run.end, chunk_end),
                    }));
                }
            }
        }
    } else {
        pairs.sort_unstable();
        let mut i = 0usize;
        while i < pairs.len() {
            let pair = pairs[i];
            let start = i;
            i += 1;
            while i < pairs.len() && pair.same_pair(pairs[i]) {
                i += 1;
            }
            if i - start >= 2 {
                let (a, b) = pair.endpoints();
                emit_parallel_group(
                    a,
                    b,
                    pairs[start..i].iter().map(|edge| edge.eid()),
                    i - start,
                    graph,
                    next_virtual,
                    &mut p_comps,
                    &mut consumed,
                    &mut synthetic,
                );
            }
        }
    }

    (p_comps, synthetic, consumed)
}

fn split_multi_edges<CE: ComponentEdges>(
    graph: &Graph,
    next_virtual: &mut u64,
    self_loops: SelfLoopFlags<'_>,
) -> (Vec<SplitComponent<CE>>, Vec<(u64, u64, u64)>, Vec<bool>) {
    if graph.num_nodes() <= u32::MAX as usize {
        split_multi_edges_with_record::<CompactPairEdge, CE>(graph, next_virtual, self_loops)
    } else if graph.num_nodes() as u64 <= U40_MAX && graph.num_edges() as u64 <= U40_MAX {
        split_multi_edges_with_record::<PackedU40PairEdge, CE>(graph, next_virtual, self_loops)
    } else {
        split_multi_edges_with_record::<FullPairEdge, CE>(graph, next_virtual, self_loops)
    }
}

enum MultiEdgeSplit<CE: ComponentEdges> {
    Simple,
    Split(Vec<SplitComponent<CE>>, Vec<(u64, u64, u64)>, Vec<bool>),
}

const BOUNDED_MULTI_EDGES_PER_NODE: usize = 4096;

fn bounded_forward_edges(graph: &Graph, node: usize, edges: &mut Vec<(u64, u64)>) -> bool {
    edges.clear();
    for (neighbor, edge) in graph.neighbors(NodeId(node as u64)) {
        if neighbor.0 <= node as u64 {
            continue;
        }
        if edges.len() == BOUNDED_MULTI_EDGES_PER_NODE {
            return false;
        }
        edges.push((neighbor.0, edge.0));
    }
    edges.sort_unstable();
    true
}

fn split_multi_edges_bounded<CE: ComponentEdges>(
    graph: &Graph,
    next_virtual: &mut u64,
) -> Option<MultiEdgeSplit<CE>> {
    let nodes = graph.num_nodes();
    let workers = spqr_thread_count().min(nodes.max(1));
    let base = nodes / workers;
    let extra = nodes % workers;
    let complete = AtomicBool::new(true);
    let mut group_counts = vec![0usize; workers];

    thread::scope(|scope| {
        for (worker, count) in group_counts.iter_mut().enumerate() {
            let complete = &complete;
            scope.spawn(move || {
                let begin = base * worker + worker.min(extra);
                let end = begin + base + usize::from(worker < extra);
                let mut edges = Vec::with_capacity(BOUNDED_MULTI_EDGES_PER_NODE);
                for node in begin..end {
                    if !complete.load(Ordering::Relaxed) {
                        return;
                    }
                    if !bounded_forward_edges(graph, node, &mut edges) {
                        complete.store(false, Ordering::Relaxed);
                        return;
                    }
                    let mut first = 0;
                    while first < edges.len() {
                        let mut last = first + 1;
                        while last < edges.len() && edges[last].0 == edges[first].0 {
                            last += 1;
                        }
                        if last - first > 1 {
                            *count = count.checked_add(1).expect("too many parallel edge groups");
                        }
                        first = last;
                    }
                }
            });
        }
    });
    if !complete.load(Ordering::Relaxed) {
        return None;
    }

    let mut group_total = 0usize;
    let mut first_virtual = Vec::with_capacity(workers);
    for count in &group_counts {
        first_virtual.push(
            (*next_virtual)
                .checked_add(group_total as u64)
                .expect("wide SPQR virtual edge id overflow"),
        );
        group_total = group_total
            .checked_add(*count)
            .expect("too many parallel edge groups");
    }
    if group_total == 0 {
        return Some(MultiEdgeSplit::Simple);
    }
    *next_virtual = (*next_virtual)
        .checked_add(group_total as u64)
        .expect("wide SPQR virtual edge id overflow");

    let mut output: Vec<Option<(Vec<SplitComponent<CE>>, Vec<(u64, u64, u64)>)>> =
        (0..workers).map(|_| None).collect();
    thread::scope(|scope| {
        for (worker, slot) in output.iter_mut().enumerate() {
            let mut virtual_id = first_virtual[worker];
            let expected = group_counts[worker];
            scope.spawn(move || {
                let begin = base * worker + worker.min(extra);
                let end = begin + base + usize::from(worker < extra);
                let mut components = Vec::with_capacity(expected);
                let mut synthetic = Vec::with_capacity(expected);
                let mut edges = Vec::with_capacity(BOUNDED_MULTI_EDGES_PER_NODE);
                for node in begin..end {
                    assert!(bounded_forward_edges(graph, node, &mut edges));
                    let mut first = 0;
                    while first < edges.len() {
                        let mut last = first + 1;
                        while last < edges.len() && edges[last].0 == edges[first].0 {
                            last += 1;
                        }
                        if last - first > 1 {
                            let target = edges[first].0;
                            let mut component_edges = CE::with_capacity(last - first + 1);
                            for &(_, edge_id) in &edges[first..last] {
                                let edge = graph.edge(EdgeId(edge_id));
                                component_edges.push(StackEdge {
                                    src: edge.src.0,
                                    dst: edge.dst.0,
                                    eid: edge_id,
                                });
                            }
                            component_edges.push(StackEdge {
                                src: node as u64,
                                dst: target,
                                eid: virtual_id,
                            });
                            components.push(SplitComponent {
                                edges: component_edges,
                                pole_a: node as u64,
                                pole_b: target,
                            });
                            synthetic.push((node as u64, target, virtual_id));
                            virtual_id += 1;
                        }
                        first = last;
                    }
                }
                debug_assert_eq!(components.len(), expected);
                *slot = Some((components, synthetic));
            });
        }
    });

    let mut components = Vec::with_capacity(group_total);
    let mut synthetic = Vec::with_capacity(group_total);
    for item in output {
        let (mut local_components, mut local_synthetic) = item.unwrap();
        components.append(&mut local_components);
        synthetic.append(&mut local_synthetic);
    }
    let mut consumed = vec![false; graph.num_edges()];
    for component in &components {
        component.edges.for_each_indexed(|_, edge| {
            if edge.eid < graph.num_edges() as u64 {
                consumed[edge.eid as usize] = true;
            }
        });
    }
    Some(MultiEdgeSplit::Split(components, synthetic, consumed))
}

trait VirtualValueColumn: Sized {
    fn virtual_with_capacity(capacity: usize) -> Self;
    fn virtual_value(&self, index: usize) -> u64;
    fn push_virtual_value(&mut self, value: u64);
    fn set_virtual_value(&mut self, index: usize, value: u64);
}

impl VirtualValueColumn for Vec<u32> {
    fn virtual_with_capacity(capacity: usize) -> Self {
        Vec::with_capacity(capacity)
    }

    #[inline(always)]
    fn virtual_value(&self, index: usize) -> u64 {
        if self[index] == u32::MAX {
            INVALID
        } else {
            self[index] as u64
        }
    }

    #[inline(always)]
    fn push_virtual_value(&mut self, value: u64) {
        self.push(if value == INVALID {
            u32::MAX
        } else {
            u32::try_from(value).expect("node id does not fit in compact column")
        });
    }

    #[inline(always)]
    fn set_virtual_value(&mut self, index: usize, value: u64) {
        self[index] = if value == INVALID {
            u32::MAX
        } else {
            u32::try_from(value).expect("node id does not fit in compact column")
        };
    }
}

impl<T: U64Column> VirtualValueColumn for T {
    #[inline(always)]
    fn virtual_with_capacity(capacity: usize) -> Self {
        U64Column::with_capacity(capacity)
    }

    #[inline(always)]
    fn virtual_value(&self, index: usize) -> u64 {
        U64Column::value(self, index)
    }

    #[inline(always)]
    fn push_virtual_value(&mut self, value: u64) {
        U64Column::push_value(self, value);
    }

    #[inline(always)]
    fn set_virtual_value(&mut self, index: usize, value: u64) {
        U64Column::set_value(self, index, value);
    }
}

fn radix_pass_u64_pairs<C: U64Column>(
    from_keys: &C,
    from_values: &C,
    to_keys: &mut C,
    to_values: &mut C,
    shift: u32,
) {
    const RADIX: usize = 1 << 16;
    let mut counts = [0usize; RADIX];
    for index in 0..from_keys.len() {
        let key = from_keys.value(index);
        counts[((key >> shift) as usize) & (RADIX - 1)] += 1;
    }
    let mut next = 0usize;
    for count in &mut counts {
        let end = next + *count;
        *count = next;
        next = end;
    }
    for index in 0..from_keys.len() {
        let bucket = ((from_keys.value(index) >> shift) as usize) & (RADIX - 1);
        let output = counts[bucket];
        counts[bucket] += 1;
        to_keys.set_value(output, from_keys.value(index));
        to_values.set_value(output, from_values.value(index));
    }
}

fn radix_sort_u64_pairs<C: U64Column>(
    keys: &mut C,
    values: &mut C,
    tmp_keys: &mut C,
    tmp_values: &mut C,
) {
    for pass in 0..C::RADIX_PASSES {
        let shift = (pass * 16) as u32;
        if pass % 2 == 0 {
            radix_pass_u64_pairs(keys, values, tmp_keys, tmp_values, shift);
        } else {
            radix_pass_u64_pairs(tmp_keys, tmp_values, keys, values, shift);
        }
    }
    if C::RADIX_PASSES % 2 != 0 {
        std::mem::swap(keys, tmp_keys);
        std::mem::swap(values, tmp_values);
    }
}

fn invert_permutation<C: I64Column>(values: &mut C) {
    for start in 0..values.len() {
        if values.value(start) < 0 {
            continue;
        }
        let mut current = start;
        let mut next = values.value(current) as usize - 1;
        while next != start {
            let next_next = values.value(next) as usize - 1;
            values.set_value(next, -(current as i64) - 1);
            current = next;
            next = next_next;
        }
        values.set_value(start, -(current as i64) - 1);
    }
    for index in 0..values.len() {
        values.set_value(index, -values.value(index) - 1);
    }
}

fn triconn_decompose_view<CE: ComponentEdges, G: GraphAccess>(
    graph: &mut G,
    reference_eid: EdgeId,
    next_virtual: &mut u64,
    consumed: &[bool],
) -> Vec<SplitComponent<CE>> {
    let compact_nodes = graph.num_nodes() <= u32::MAX as usize;
    let compact_edges = graph.num_edges() <= u32::MAX as usize;
    if compact_nodes
        && compact_edges
        && triconn_packed_columns_save_enough(graph.num_nodes(), graph.num_edges(), *next_virtual)
    {
        triconn_decompose_impl::<PackedU40Column, PackedI40Column, Vec<u32>, CE, G>(
            graph,
            reference_eid,
            next_virtual,
            consumed,
        )
    } else if compact_nodes && compact_edges {
        triconn_decompose_impl::<Vec<u64>, Vec<i64>, Vec<u32>, CE, G>(
            graph,
            reference_eid,
            next_virtual,
            consumed,
        )
    } else if compact_nodes && triconn_u40_fits(graph.num_nodes(), graph.num_edges(), *next_virtual)
    {
        triconn_decompose_impl::<PackedU40Column, PackedI40Column, Vec<u32>, CE, G>(
            graph,
            reference_eid,
            next_virtual,
            consumed,
        )
    } else if triconn_u40_fits(graph.num_nodes(), graph.num_edges(), *next_virtual) {
        triconn_decompose_impl::<PackedU40Column, PackedI40Column, PackedU40Column, CE, G>(
            graph,
            reference_eid,
            next_virtual,
            consumed,
        )
    } else if compact_nodes {
        triconn_decompose_impl::<Vec<u64>, Vec<i64>, Vec<u32>, CE, G>(
            graph,
            reference_eid,
            next_virtual,
            consumed,
        )
    } else {
        triconn_decompose_impl::<Vec<u64>, Vec<i64>, Vec<u64>, CE, G>(
            graph,
            reference_eid,
            next_virtual,
            consumed,
        )
    }
}

fn triconn_u40_fits(nodes: usize, edges: usize, next_virtual: u64) -> bool {
    let Some(work) = nodes
        .checked_add(edges)
        .and_then(|value| value.checked_mul(2))
        .and_then(|value| value.checked_add(2))
    else {
        return false;
    };
    let Some(last_virtual) = next_virtual.checked_add(work as u64) else {
        return false;
    };
    let Some(max_phi) = (nodes as u64)
        .checked_mul(3)
        .and_then(|value| value.checked_add(2))
    else {
        return false;
    };
    (work as u64) < U40_MAX && last_virtual < U40_MAX && max_phi < (1u64 << 39)
}

fn triconn_packed_columns_save_enough(nodes: usize, edges: usize, next_virtual: u64) -> bool {
    if !triconn_u40_fits(nodes, edges, next_virtual) {
        return false;
    }
    let values = (nodes as u128) * 6 + (edges as u128) * 7;
    values * 3 >= TRICONN_PACKED_MIN_SAVINGS
}

fn triconn_decompose_impl<
    C: U64Column,
    S: I64Column,
    V: VirtualValueColumn,
    CE: ComponentEdges,
    G: GraphAccess,
>(
    graph: &mut G,
    reference_eid: EdgeId,
    next_virtual: &mut u64,
    consumed: &[bool],
) -> Vec<SplitComponent<CE>> {
    let n = graph.num_nodes();
    let m = graph.num_edges();
    assert!(reference_eid.is_valid() && reference_eid.idx() < m);

    let mut virtual_src = V::virtual_with_capacity(0);
    let mut virtual_dst = V::virtual_with_capacity(0);
    let mut virtual_orig = C::with_capacity(0);
    let mut me_etype = vec![0u8; m];
    let mut me_start: Vec<bool> = Vec::with_capacity(m);
    let mut me_reversed: Vec<bool> = Vec::with_capacity(m);
    let mut virtual_adj_v = V::virtual_with_capacity(0);
    let mut me_adj_p = C::with_capacity(m);
    let mut me_hi_slot = C::with_capacity(m);

    macro_rules! me_src {
        ($edge:expr) => {{
            let edge_index = $edge as usize;
            if edge_index < m {
                let first = edge_index * 2;
                if me_reversed[edge_index] {
                    graph.targets.get(first).0
                } else {
                    graph.targets.get(first + 1).0
                }
            } else {
                virtual_src.virtual_value(edge_index - m)
            }
        }};
    }
    macro_rules! me_dst {
        ($edge:expr) => {{
            let edge_index = $edge as usize;
            if edge_index < m {
                let first = edge_index * 2;
                if me_reversed[edge_index] {
                    graph.targets.get(first + 1).0
                } else {
                    graph.targets.get(first).0
                }
            } else {
                virtual_dst.virtual_value(edge_index - m)
            }
        }};
    }
    macro_rules! me_orig {
        ($edge:expr) => {{
            let edge_index = $edge as usize;
            if edge_index < m {
                edge_index as u64
            } else {
                virtual_orig.value(edge_index - m)
            }
        }};
    }
    macro_rules! adj_v {
        ($edge:expr) => {{
            let edge_index = $edge as usize;
            if edge_index < m {
                me_src!($edge)
            } else {
                virtual_adj_v.virtual_value(edge_index - m)
            }
        }};
    }

    macro_rules! new_edge {
        ($src:expr, $dst:expr, $orig:expr, $et:expr) => {{
            let i = me_etype.len() as u64;
            debug_assert!(i as usize >= m);
            virtual_src.push_virtual_value($src);
            virtual_dst.push_virtual_value($dst);
            virtual_orig.push_value($orig);
            virtual_adj_v.push_virtual_value(INVALID);
            me_etype.push($et);
            me_start.push(false);
            me_adj_p.push_value(INVALID);
            me_hi_slot.push_value(INVALID);
            i
        }};
    }

    let mut al_edge = C::with_capacity(m);
    let mut al_next = C::with_capacity(m);
    let mut al_prev = C::with_capacity(m);
    let mut hp_values = S::with_capacity(m);
    let mut hp_next = C::with_capacity(m);
    let mut hp_deleted: Vec<bool> = Vec::with_capacity(m);

    let mut degree = CountColumn::zeros(n);
    let mut tree_arc = C::filled(n, INVALID);
    let mut newnum = S::filled(n, 0);
    let mut lp1 = S::filled(n, 0);
    let mut lp2 = S::filled(n, 0);
    let mut nd_arr = S::filled(n, 1);

    for v in 0..n {
        degree.set_value(v, graph.degree(NodeId(v as u64)) as i64);
    }
    for i in 0..consumed.len().min(m) {
        if consumed[i] {
            let e = graph.edge(EdgeId(i as u64));
            degree.add_value(e.src.idx(), -1);
            degree.add_value(e.dst.idx(), -1);
        }
    }

    let mut number = S::filled(n, 0);
    {
        let mut nc = 0i64;
        let mut seen = vec![false; m];
        for i in 0..consumed.len().min(m) {
            if consumed[i] {
                seen[i] = true;
                me_etype[i] = 3;
            }
        }
        let s0 = 0u64;
        nc += 1;
        number.set_value(s0 as usize, nc);
        lp1.set_value(s0 as usize, nc);
        lp2.set_value(s0 as usize, nc);
        struct F {
            v: u64,
            he: u64,
        }
        let mut stk = vec![F {
            v: s0,
            he: graph.heads.get(s0 as usize),
        }];
        while let Some(fr) = stk.last_mut() {
            let v = fr.v;
            if fr.he == INVALID {
                stk.pop();
                if let Some(p) = stk.last() {
                    let pv = p.v as usize;
                    nd_arr.add_value(pv, nd_arr.value(v as usize));
                    let (a, b) = (lp1.value(v as usize), lp2.value(v as usize));
                    match a.cmp(&lp1.value(pv)) {
                        std::cmp::Ordering::Less => {
                            lp2.set_value(pv, std::cmp::min(lp1.value(pv), b));
                            lp1.set_value(pv, a);
                        }
                        std::cmp::Ordering::Equal => {
                            lp2.set_value(pv, std::cmp::min(lp2.value(pv), b));
                        }
                        std::cmp::Ordering::Greater => {
                            lp2.set_value(pv, std::cmp::min(lp2.value(pv), a));
                        }
                    }
                }
                continue;
            }
            let he_index = fr.he;
            fr.he = graph.next.get(he_index as usize);
            let w = graph.targets.get(he_index as usize).0;
            let ei = (he_index / 2) as usize;
            if seen[ei] {
                continue;
            }
            seen[ei] = true;
            if number.value(w as usize) == 0 {
                me_etype[ei] = 1;
                nc += 1;
                number.set_value(w as usize, nc);
                lp1.set_value(w as usize, nc);
                lp2.set_value(w as usize, nc);
                tree_arc.set_value(w as usize, ei as u64);
                stk.push(F {
                    v: w,
                    he: graph.heads.get(w as usize),
                });
            } else {
                me_etype[ei] = 2;
                let nw = number.value(w as usize);
                match nw.cmp(&lp1.value(v as usize)) {
                    std::cmp::Ordering::Less => {
                        lp2.set_value(v as usize, lp1.value(v as usize));
                        lp1.set_value(v as usize, nw);
                    }
                    std::cmp::Ordering::Equal => {}
                    std::cmp::Ordering::Greater => {
                        lp2.set_value(v as usize, std::cmp::min(lp2.value(v as usize), nw));
                    }
                }
            }
        }
        assert!(nc as usize == n, "not connected: {} / {}", nc, n);
    }

    me_start.resize(m, false);
    me_reversed.resize(m, false);
    for i in 0..m {
        let e = graph.edge(EdgeId(i as u64));
        let up = number.value(e.dst.idx()) > number.value(e.src.idx());
        me_reversed[i] = (up && me_etype[i] == 2) || (!up && me_etype[i] == 1);
    }
    graph.release_adjacency();

    let maxb = 3 * n as i64 + 2;
    let mut oadj_offsets = C::filled(n + 1, 0);
    let phi_for = |i: usize| -> Option<usize> {
        let etype = me_etype[i];
        if etype == 0 || etype == 3 {
            return None;
        }
        let w = me_dst!(i) as usize;
        let source = me_src!(i) as usize;
        let phi = if etype == 2 {
            3 * number.value(w) + 1
        } else if lp2.value(w) < number.value(source) {
            3 * lp1.value(w)
        } else {
            3 * lp1.value(w) + 2
        };
        (phi >= 1 && phi <= maxb).then_some(phi as usize)
    };

    me_adj_p.clear_values();
    me_hi_slot.clear_values();
    for i in 0..m {
        if let Some(phi) = phi_for(i) {
            me_adj_p.push_value(phi as u64);
            me_hi_slot.push_value(i as u64);
            oadj_offsets.set_value(
                me_src!(i) as usize + 1,
                oadj_offsets.value(me_src!(i) as usize + 1) + 1,
            );
        }
    }

    let total_edges = me_adj_p.len();

    for i in 0..n {
        oadj_offsets.set_value(i + 1, oadj_offsets.value(i + 1) + oadj_offsets.value(i));
    }

    al_prev.resize_values(total_edges, 0);
    al_next.resize_values(total_edges, 0);
    radix_sort_u64_pairs(&mut me_adj_p, &mut me_hi_slot, &mut al_prev, &mut al_next);

    let mut end = total_edges;
    while end != 0 {
        let phi = me_adj_p.value(end - 1);
        let mut start = end - 1;
        while start != 0 && me_adj_p.value(start - 1) == phi {
            start -= 1;
        }
        for idx in (start..end).rev() {
            let ei = me_hi_slot.value(idx);
            let src = me_src!(ei) as usize;
            let offset = oadj_offsets.value(src + 1) - 1;
            oadj_offsets.set_value(src + 1, offset);
            al_prev.set_value(offset as usize, ei);
        }
        end = start;
    }
    for i in 1..n {
        oadj_offsets.set_value(i, oadj_offsets.value(i + 1));
    }
    oadj_offsets.set_value(n, total_edges as u64);

    me_adj_p.clear_values();
    me_adj_p.resize_values(me_etype.len(), INVALID);
    me_hi_slot.clear_values();
    me_hi_slot.resize_values(me_etype.len(), INVALID);
    al_next.clear_values();

    let mut hp_head = C::filled(n, INVALID);
    al_next.resize_values(n, INVALID);
    {
        let mut nc = n as i64;
        let mut np = true;
        let s0 = 0u64;
        newnum.set_value(s0 as usize, nc - nd_arr.value(s0 as usize) + 1);
        struct PF {
            v: u64,
            idx: usize,
            pend: bool,
        }
        let mut pfs = vec![PF {
            v: s0,
            idx: 0,
            pend: false,
        }];
        while let Some(fr) = pfs.last_mut() {
            if fr.pend {
                fr.pend = false;
                nc -= 1;
            }
            let v = fr.v as usize;
            let oadj_len = (oadj_offsets.value(v + 1) - oadj_offsets.value(v)) as usize;
            if fr.idx >= oadj_len {
                pfs.pop();
                continue;
            }
            let ei = al_prev.value(oadj_offsets.value(v) as usize + fr.idx) as usize;
            fr.idx += 1;
            let w = me_dst!(ei);
            if np {
                np = false;
                me_start[ei] = true;
            }
            if me_etype[ei] == 1 {
                fr.pend = true;
                newnum.set_value(w as usize, nc - nd_arr.value(w as usize) + 1);
                pfs.push(PF {
                    v: w,
                    idx: 0,
                    pend: false,
                });
            } else {
                let slot = hp_values.len() as u64;
                hp_values.push_value(newnum.value(fr.v as usize));
                hp_next.push_value(INVALID);
                hp_deleted.push(false);
                let heap_node = w as usize;
                let tail = al_next.value(heap_node);
                if tail != INVALID {
                    hp_next.set_value(tail as usize, slot);
                } else {
                    hp_head.set_value(heap_node, slot);
                }
                al_next.set_value(heap_node, slot);
                me_hi_slot.set_value(ei, slot);
                np = true;
            }
        }
    }
    al_next.clear_values();

    invert_permutation(&mut number);
    macro_rules! node_at {
        ($rank:expr) => {
            number.value(($rank as usize) - 1) as u64
        };
    }
    macro_rules! father_of {
        ($node:expr) => {{
            let edge = tree_arc.value($node as usize);
            if edge == INVALID {
                -1i64
            } else {
                me_src!(edge) as i64
            }
        }};
    }
    let mut ah_count = CountColumn::with_capacity(n);
    for v in 0..n {
        let first = newnum.value(node_at!(lp1.value(v)) as usize);
        let second = newnum.value(node_at!(lp2.value(v)) as usize);
        lp1.set_value(v, first);
        lp2.set_value(v, second);
    }
    for v in 0..n {
        number.set_value(newnum.value(v) as usize - 1, v as i64);
    }
    let mut ah_head = C::filled(n, INVALID);
    for v in 0..n {
        let begin = oadj_offsets.value(v) as usize;
        let end = oadj_offsets.value(v + 1) as usize;
        ah_count.push_value((end - begin) as i64);
        let mut tail = INVALID;
        for idx in begin..end {
            let ei = al_prev.value(idx);
            let slot = al_edge.len() as u64;
            al_edge.push_value(ei);
            al_next.push_value(INVALID);
            al_prev.set_value(idx, tail);
            if tail != INVALID {
                al_next.set_value(tail as usize, slot);
            } else {
                ah_head.set_value(v, slot);
            }
            tail = slot;
            if ei >= m as u64 {
                virtual_adj_v.set_virtual_value(ei as usize - m, v as u64);
            }
            me_adj_p.set_value(ei as usize, slot);
        }
    }
    drop(oadj_offsets);

    macro_rules! high {
        ($v:expr) => {{
            let __vi = $v as usize;
            loop {
                let __head = hp_head.value(__vi);
                if __head == INVALID || !hp_deleted[__head as usize] {
                    break;
                }
                hp_head.set_value(__vi, hp_next.value(__head as usize));
            }
            let __head = hp_head.value(__vi);
            if __head == INVALID {
                0i64
            } else {
                hp_values.value(__head as usize)
            }
        }};
    }

    macro_rules! adj_front {
        ($v:expr) => {{
            let h = ah_head.value($v as usize);
            if h == INVALID {
                None
            } else {
                Some((al_edge.value(h as usize), h))
            }
        }};
    }
    macro_rules! adj_count {
        ($v:expr) => {
            ah_count.value($v as usize)
        };
    }
    macro_rules! next_slot {
        ($after:expr) => {{
            let ns = al_next.value($after as usize);
            if ns == INVALID {
                None
            } else {
                let __ei = al_edge.value(ns as usize);
                Some((ns, __ei))
            }
        }};
    }

    macro_rules! del_adj {
        ($ei:expr) => {
            let __s = me_adj_p.value($ei as usize);
            if __s != INVALID {
                let __v = adj_v!($ei) as usize;
                let __prev = al_prev.value(__s as usize);
                let __next = al_next.value(__s as usize);
                if __prev != INVALID {
                    al_next.set_value(__prev as usize, __next);
                } else {
                    ah_head.set_value(__v, __next);
                }
                if __next != INVALID {
                    al_prev.set_value(__next as usize, __prev);
                }
                ah_count.add_value(__v, -1);
            }
        };
    }
    macro_rules! del_adj_slot {
        ($v:expr, $slot:expr) => {{
            let __v2 = $v as usize;
            let __s2 = $slot as usize;
            let __prev = al_prev.value(__s2);
            let __next = al_next.value(__s2);
            if __prev != INVALID {
                al_next.set_value(__prev as usize, __next);
            } else {
                ah_head.set_value(__v2, __next);
            }
            if __next != INVALID {
                al_prev.set_value(__next as usize, __prev);
            }
            ah_count.add_value(__v2, -1);
        }};
    }
    macro_rules! del_high {
        ($ei:expr) => {
            let slot = me_hi_slot.value($ei as usize);
            if slot != INVALID && (slot as usize) < hp_values.len() {
                hp_deleted[slot as usize] = true;
            }
        };
    }
    macro_rules! replace_adj {
        ($v:expr, $slot:expr, $new_ei:expr) => {
            al_edge.set_value($slot as usize, $new_ei);
            virtual_adj_v.set_virtual_value($new_ei as usize - m, $v);
            me_adj_p.set_value($new_ei as usize, $slot);
        };
    }
    macro_rules! se {
        ($ei:expr) => {
            StackEdge {
                src: me_src!($ei),
                dst: me_dst!($ei),
                eid: me_orig!($ei),
            }
        };
    }

    let tsz = 2 * (m + n) + 2;
    let initial_stack = tsz.min(1 << 20);
    let packed_stack = n as u64 > u32::MAX as u64 && n as u64 <= STACK_PACKED_MAX;
    let mut th = StackValues::new(initial_stack, packed_stack);
    let mut ta = StackValues::new(initial_stack, packed_stack);
    let mut tb = StackValues::new(initial_stack, packed_stack);
    let mut top: usize = 0;
    ta.set(0, -1);

    macro_rules! push_stack_slot {
        () => {{
            if top + 1 >= tsz {
                panic!("SPQR stack is too large");
            }
            top += 1;
            th.ensure_slot(top);
            ta.ensure_slot(top);
            tb.ensure_slot(top);
        }};
    }

    let mut estack = C::with_capacity(m + n);
    let mut comps: Vec<SplitComponent<CE>> = Vec::new();

    struct PS {
        v: u64,
        outv: i64,
        cur: u64,
        ei: u64,
        after: bool,
    }

    let s0 = 0u64;
    let (fei, fpos) = adj_front!(s0).expect("start vertex has no adj");
    let mut cs: Vec<PS> = vec![PS {
        v: s0,
        outv: adj_count!(s0),
        cur: fpos,
        ei: fei,
        after: false,
    }];

    while !cs.is_empty() {
        let idx = cs.len() - 1;

        if !cs[idx].after && me_etype[cs[idx].ei as usize] == 1 {
            let ei = cs[idx].ei;
            let w = me_dst!(ei);
            let vn = newnum.value(cs[idx].v as usize);
            if me_start[ei as usize] {
                if ta.get(top) > lp1.value(w as usize) {
                    let mut y = 0i64;
                    let mut bv;
                    loop {
                        y = std::cmp::max(y, th.get(top));
                        bv = tb.get(top);
                        top -= 1;
                        if ta.get(top) <= lp1.value(w as usize) {
                            break;
                        }
                    }
                    push_stack_slot!();
                    th.set(top, y);
                    ta.set(top, lp1.value(w as usize));
                    tb.set(top, bv);
                } else {
                    push_stack_slot!();
                    th.set(top, newnum.value(w as usize) + nd_arr.value(w as usize) - 1);
                    ta.set(top, lp1.value(w as usize));
                    tb.set(top, vn);
                }
                push_stack_slot!();
                ta.set(top, -1);
            }
            cs[idx].after = true;
            if let Some((ce, cp)) = adj_front!(w) {
                cs.push(PS {
                    v: w,
                    outv: adj_count!(w),
                    cur: cp,
                    ei: ce,
                    after: false,
                });
            }
            continue;
        } else if cs[idx].after {
            let v = cs[idx].v;
            let vn = newnum.value(v as usize);
            let itp = cs[idx].cur;
            let tei = cs[idx].ei;
            let mut w = me_dst!(tei);
            let mut wn = newnum.value(w as usize);

            estack.push_value(tree_arc.value(w as usize));

            while vn != 1
                && (ta.get(top) == vn
                    || (degree.value(w as usize) == 2
                        && adj_front!(w)
                            .map_or(false, |(fe, _)| newnum.value(me_dst!(fe) as usize) > wn)))
            {
                let a = ta.get(top);
                let b = tb.get(top);
                if a == vn && father_of!(node_at!(b)) == node_at!(a) as i64 {
                    top -= 1;
                } else {
                    let mut eab: Option<u64> = None;

                    if degree.value(w as usize) == 2
                        && adj_front!(w)
                            .map_or(false, |(fe, _)| newnum.value(me_dst!(fe) as usize) > wn)
                    {
                        let e1 = estack.pop_value().unwrap();
                        let e2 = estack.pop_value().unwrap();
                        del_adj!(e2);
                        let x = me_dst!(e2);
                        degree.add_value(x as usize, -1);
                        degree.add_value(v as usize, -1);
                        let vid = next_virtual_id(next_virtual);
                        let ev = new_edge!(v, x, vid, 1);
                        comps.push(SplitComponent::from_array(
                            [
                                se!(e1),
                                se!(e2),
                                StackEdge {
                                    src: v,
                                    dst: x,
                                    eid: vid,
                                },
                            ],
                            v,
                            x,
                        ));
                        if let Some(et) = estack.last_value() {
                            if me_src!(et) == x && me_dst!(et) == v {
                                let eab2 = estack.pop_value().unwrap();
                                del_adj!(eab2);
                                del_high!(eab2);
                                eab = Some(eab2);
                            }
                        }
                        let mut cur_virt = ev;
                        let cur_vid = vid;
                        if let Some(eab_v) = eab {
                            let vid2 = next_virtual_id(next_virtual);
                            let nv2 = new_edge!(v, x, vid2, 1);
                            comps.push(SplitComponent::from_array(
                                [
                                    se!(eab_v),
                                    StackEdge {
                                        src: v,
                                        dst: x,
                                        eid: cur_vid,
                                    },
                                    StackEdge {
                                        src: v,
                                        dst: x,
                                        eid: vid2,
                                    },
                                ],
                                v,
                                x,
                            ));
                            degree.add_value(x as usize, -1);
                            degree.add_value(v as usize, -1);
                            cur_virt = nv2;
                        }
                        estack.push_value(cur_virt);
                        replace_adj!(v, itp, cur_virt);
                        degree.add_value(x as usize, 1);
                        degree.add_value(v as usize, 1);
                        tree_arc.set_value(x as usize, cur_virt);
                        me_etype[cur_virt as usize] = 1;
                        w = x;
                        wn = newnum.value(w as usize);
                    } else {
                        let h = th.get(top);
                        top -= 1;
                        let mut ce = CE::with_capacity(0);
                        while let Some(et) = estack.last_value() {
                            let nx = newnum.value(me_src!(et) as usize);
                            let ny = newnum.value(me_dst!(et) as usize);
                            if !(a <= nx && nx <= h && a <= ny && ny <= h) {
                                break;
                            }
                            if (nx == a && ny == b) || (ny == a && nx == b) {
                                let eab2 = estack.pop_value().unwrap();
                                del_adj!(eab2);
                                del_high!(eab2);
                                eab = Some(eab2);
                            } else {
                                let eh = estack.pop_value().unwrap();
                                if !(adj_v!(eh) == v && me_adj_p.value(eh as usize) == itp) {
                                    del_adj!(eh);
                                    del_high!(eh);
                                }
                                ce.push(se!(eh));
                                degree.add_value(me_src!(eh) as usize, -1);
                                degree.add_value(me_dst!(eh) as usize, -1);
                            }
                        }
                        let pa = node_at!(a);
                        let pb = node_at!(b);
                        let vid = next_virtual_id(next_virtual);
                        let ev = new_edge!(pa, pb, vid, 1);
                        ce.push(StackEdge {
                            src: pa,
                            dst: pb,
                            eid: vid,
                        });
                        comps.push(SplitComponent {
                            edges: ce,
                            pole_a: pa,
                            pole_b: pb,
                        });
                        let x = pb;
                        let mut cur_virt = ev;
                        let cur_vid = vid;
                        if let Some(eab_v) = eab {
                            let vid2 = next_virtual_id(next_virtual);
                            let nv2 = new_edge!(v, x, vid2, 1);
                            comps.push(SplitComponent::from_array(
                                [
                                    se!(eab_v),
                                    StackEdge {
                                        src: v,
                                        dst: x,
                                        eid: cur_vid,
                                    },
                                    StackEdge {
                                        src: v,
                                        dst: x,
                                        eid: vid2,
                                    },
                                ],
                                v,
                                x,
                            ));
                            degree.add_value(x as usize, -1);
                            degree.add_value(v as usize, -1);
                            cur_virt = nv2;
                        }
                        estack.push_value(cur_virt);
                        replace_adj!(v, itp, cur_virt);
                        degree.add_value(x as usize, 1);
                        degree.add_value(v as usize, 1);
                        tree_arc.set_value(x as usize, cur_virt);
                        me_etype[cur_virt as usize] = 1;
                        w = x;
                        wn = newnum.value(w as usize);
                    }
                }
            }

            if lp2.value(w as usize) >= vn
                && lp1.value(w as usize) < vn
                && (father_of!(v) != s0 as i64 || cs[idx].outv >= 2)
            {
                let l1 = lp1.value(w as usize);
                let mut ce = CE::with_capacity(0);
                let mut xx = 0i64;
                let mut yy = 0i64;
                while let Some(et) = estack.last_value() {
                    xx = newnum.value(me_src!(et) as usize);
                    yy = newnum.value(me_dst!(et) as usize);
                    let descendants = nd_arr.value(w as usize);
                    if !((wn <= xx && xx < wn + descendants) || (wn <= yy && yy < wn + descendants))
                    {
                        break;
                    }
                    let eh = estack.pop_value().unwrap();
                    del_high!(eh);
                    ce.push(se!(eh));
                    degree.add_value(node_at!(xx) as usize, -1);
                    degree.add_value(node_at!(yy) as usize, -1);
                }
                let pl = node_at!(l1);
                let vid = next_virtual_id(next_virtual);
                let mut ev = new_edge!(v, pl, vid, 1);
                let cur_vid = vid;
                ce.push(StackEdge {
                    src: v,
                    dst: pl,
                    eid: vid,
                });
                comps.push(SplitComponent {
                    edges: ce,
                    pole_a: v,
                    pole_b: pl,
                });

                if (xx == vn && yy == l1) || (yy == vn && xx == l1) {
                    if let Some(eh) = estack.pop_value() {
                        if !(adj_v!(eh) == v && me_adj_p.value(eh as usize) == itp) {
                            del_adj!(eh);
                        }
                        let vid2 = next_virtual_id(next_virtual);
                        let nv2 = new_edge!(v, pl, vid2, 1);
                        comps.push(SplitComponent::from_array(
                            [
                                se!(eh),
                                StackEdge {
                                    src: v,
                                    dst: pl,
                                    eid: cur_vid,
                                },
                                StackEdge {
                                    src: v,
                                    dst: pl,
                                    eid: vid2,
                                },
                            ],
                            v,
                            pl,
                        ));
                        me_hi_slot.set_value(nv2 as usize, me_hi_slot.value(eh as usize));
                        degree.add_value(v as usize, -1);
                        degree.add_value(pl as usize, -1);
                        ev = nv2;
                        me_etype[nv2 as usize] = 1;
                    }
                }

                if pl as i64 != father_of!(v) {
                    estack.push_value(ev);
                    replace_adj!(v, itp, ev);
                    if me_hi_slot.value(ev as usize) == INVALID && high!(pl) < vn {
                        let slot = hp_values.len() as u64;
                        hp_values.push_value(vn);
                        hp_next.push_value(hp_head.value(pl as usize));
                        hp_deleted.push(false);
                        hp_head.set_value(pl as usize, slot);
                        me_hi_slot.set_value(ev as usize, slot);
                    }
                    degree.add_value(v as usize, 1);
                    degree.add_value(pl as usize, 1);
                } else {
                    del_adj_slot!(v, itp);
                    let tav = tree_arc.value(v as usize);
                    let vid2 = next_virtual_id(next_virtual);
                    let nv2 = new_edge!(pl, v, vid2, 1);
                    comps.push(SplitComponent::from_array(
                        [
                            StackEdge {
                                src: v,
                                dst: pl,
                                eid: cur_vid,
                            },
                            StackEdge {
                                src: pl,
                                dst: v,
                                eid: vid2,
                            },
                            se!(tav),
                        ],
                        pl,
                        v,
                    ));
                    tree_arc.set_value(v as usize, nv2);
                    me_etype[nv2 as usize] = 1;
                    if adj_v!(tav) != INVALID {
                        replace_adj!(adj_v!(tav), me_adj_p.value(tav as usize), nv2);
                    }
                }
            }

            if me_start[tei as usize] {
                while ta.get(top) != -1 {
                    top -= 1;
                }
                top -= 1;
            }
            while ta.get(top) != -1 && tb.get(top) != vn && high!(v) > th.get(top) {
                top -= 1;
            }

            cs[idx].outv -= 1;
            cs[idx].after = false;
        } else {
            let ei = cs[idx].ei;
            let vn = newnum.value(cs[idx].v as usize);
            let wn = newnum.value(me_dst!(ei) as usize);
            if me_start[ei as usize] {
                if ta.get(top) > wn {
                    let mut y = 0i64;
                    let mut bv;
                    loop {
                        y = std::cmp::max(y, th.get(top));
                        bv = tb.get(top);
                        top -= 1;
                        if ta.get(top) <= wn {
                            break;
                        }
                    }
                    push_stack_slot!();
                    th.set(top, y);
                    ta.set(top, wn);
                    tb.set(top, bv);
                } else {
                    push_stack_slot!();
                    th.set(top, vn);
                    ta.set(top, wn);
                    tb.set(top, vn);
                }
            }
            estack.push_value(ei);
        }

        let idx = cs.len() - 1;
        if let Some((np, ne)) = next_slot!(cs[idx].cur) {
            cs[idx].cur = np;
            cs[idx].ei = ne;
        } else {
            cs.pop();
        }
    }

    drop(number);
    drop(cs);
    drop(th);
    drop(ta);
    drop(tb);

    drop(virtual_adj_v);
    drop(me_etype);
    drop(me_start);
    drop(me_adj_p);
    drop(me_hi_slot);
    drop(al_edge);
    drop(al_next);
    drop(al_prev);
    drop(hp_values);
    drop(hp_next);
    drop(hp_deleted);
    drop(degree);
    drop(tree_arc);
    drop(newnum);
    drop(lp1);
    drop(lp2);
    drop(nd_arr);
    drop(hp_head);
    drop(ah_count);
    drop(ah_head);

    if !estack.is_empty() {
        let first = se!(estack.last_value().expect("empty SPQR remainder"));
        let (pa, pb) = (first.src, first.dst);
        let edges = if CE::PREFERS_PACKED_REMAINDER {
            let mut packed = Vec::with_capacity(estack.len());
            let mut plain: Option<Vec<StackEdge>> = None;
            while let Some(ei) = estack.pop_value() {
                let edge = se!(ei);
                if let Some(edges) = plain.as_mut() {
                    edges.push(edge);
                } else if PackedStackEdge::fits(edge) {
                    packed.push(PackedStackEdge::pack(edge));
                } else {
                    let mut edges: Vec<StackEdge> = std::mem::take(&mut packed)
                        .into_iter()
                        .map(PackedStackEdge::unpack)
                        .collect();
                    edges.push(edge);
                    plain = Some(edges);
                }
            }
            match plain {
                Some(edges) => CE::from_edges(edges),
                None => CE::from_packed_edges(packed),
            }
        } else {
            let mut edges = Vec::with_capacity(estack.len());
            while let Some(ei) = estack.pop_value() {
                edges.push(se!(ei));
            }
            CE::from_edges(edges)
        };
        comps.push(SplitComponent {
            edges,
            pole_a: pa,
            pole_b: pb,
        });
    }

    comps
}

fn combine_components<CE: ComponentEdges>(
    multi: Vec<SplitComponent<CE>>,
    work: Vec<SplitComponent<CE>>,
    next_virtual: &mut u64,
) -> Vec<SplitComponent<CE>> {
    combine_components_parallel(multi, work, next_virtual)
}

fn combine_one_component<CE: ComponentEdges>(
    comp: SplitComponent<CE>,
    next_virtual: &mut u64,
) -> Vec<SplitComponent<CE>> {
    let mut parts = split_internal_parallels(comp, next_virtual);
    parts.reverse();
    parts
}

struct CombinedComponentBatch<CE: ComponentEdges> {
    parts: Vec<SplitComponent<CE>>,
    local_virtuals: u64,
}

fn combine_components_parallel<CE: ComponentEdges>(
    multi: Vec<SplitComponent<CE>>,
    work: Vec<SplitComponent<CE>>,
    next_virtual: &mut u64,
) -> Vec<SplitComponent<CE>> {
    const MIN_PAR_COMBINE_COMPONENTS: usize = 1024;

    let n = work.len();
    let threads = spqr_thread_count().min(n.max(1));
    if threads <= 1 || n < MIN_PAR_COMBINE_COMPONENTS {
        let mut out = multi;
        for comp in work.into_iter().rev() {
            out.extend(combine_one_component(comp, next_virtual));
        }
        return out;
    }

    let chunk = n.div_ceil(threads);
    let mut chunks: Vec<Vec<(usize, SplitComponent<CE>)>> = Vec::new();
    let mut cur: Vec<(usize, SplitComponent<CE>)> = Vec::with_capacity(chunk);
    for (idx, comp) in work.into_iter().enumerate() {
        cur.push((idx, comp));
        if cur.len() == chunk {
            chunks.push(cur);
            cur = Vec::with_capacity(chunk);
        }
    }
    if !cur.is_empty() {
        chunks.push(cur);
    }

    let local_base = *next_virtual;
    let mut joined = Vec::with_capacity(chunks.len());
    thread::scope(|scope| {
        let mut handles = Vec::with_capacity(chunks.len());
        for chunk_jobs in chunks {
            handles.push(scope.spawn(move || {
                let mut local_results = Vec::with_capacity(chunk_jobs.len());
                for (idx, comp) in chunk_jobs {
                    let mut local_next = local_base;
                    let parts = combine_one_component(comp, &mut local_next);
                    local_results.push((
                        idx,
                        CombinedComponentBatch {
                            parts,
                            local_virtuals: local_next - local_base,
                        },
                    ));
                }
                local_results
            }));
        }
        for handle in handles {
            joined.push(
                handle
                    .join()
                    .expect("parallel SPQR combine worker panicked"),
            );
        }
    });

    let mut batches: Vec<Option<CombinedComponentBatch<CE>>> = (0..n).map(|_| None).collect();
    for local_results in joined {
        for (idx, batch) in local_results {
            batches[idx] = Some(batch);
        }
    }

    let mut bases = vec![0u64; n];
    let mut next = *next_virtual;
    for idx in (0..n).rev() {
        bases[idx] = next;
        let count = batches[idx]
            .as_ref()
            .expect("missing parallel SPQR combine batch")
            .local_virtuals;
        next = next
            .checked_add(count)
            .expect("wide SPQR local virtual id remap overflow");
    }
    *next_virtual = next;

    let mut out = multi;
    for idx in (0..n).rev() {
        let mut batch = batches[idx]
            .take()
            .expect("missing parallel SPQR combine batch");
        let base = bases[idx];
        for comp in &mut batch.parts {
            comp.edges.map_in_place(|mut edge| {
                if edge.eid >= local_base {
                    edge.eid = base
                        .checked_add(edge.eid - local_base)
                        .expect("wide SPQR local virtual id remap overflow");
                }
                edge
            });
        }
        out.extend(batch.parts);
    }
    out
}

fn split_internal_parallels<CE: ComponentEdges>(
    comp: SplitComponent<CE>,
    next_virtual: &mut u64,
) -> Vec<SplitComponent<CE>> {
    if comp.edges.len() <= 64 {
        let mut verts = [0u64; 128];
        let mut vert_len = 0usize;
        comp.edges.for_each(|e| {
            if !verts[..vert_len].contains(&e.src) {
                verts[vert_len] = e.src;
                vert_len += 1;
            }
            if !verts[..vert_len].contains(&e.dst) {
                verts[vert_len] = e.dst;
                vert_len += 1;
            }
        });
        if vert_len <= 2 {
            return vec![comp];
        }

        let mut pairs = [(0u64, 0u64); 64];
        comp.edges.for_each_indexed(|idx, e| {
            pairs[idx] = if e.src <= e.dst {
                (e.src, e.dst)
            } else {
                (e.dst, e.src)
            };
        });
        let pair_slice = &mut pairs[..comp.edges.len()];
        pair_slice.sort_unstable();
        let has_internal_parallel = pair_slice.windows(2).any(|w| w[0] == w[1]);
        if !has_internal_parallel {
            return vec![comp];
        }
    }

    let mut vertices = [0u64; 3];
    let mut vertex_count = 0usize;
    let mut fits_u40 = true;
    comp.edges.for_each(|e| {
        fits_u40 &= PackedStackEdge::fits(e);
        for vertex in [e.src, e.dst] {
            if vertex_count < vertices.len() && !vertices[..vertex_count].contains(&vertex) {
                vertices[vertex_count] = vertex;
                vertex_count += 1;
            }
        }
    });
    if vertex_count <= 2 {
        return vec![comp];
    }

    if fits_u40 && (comp.edges.len() as u64) < U40_MAX {
        split_internal_parallels_sorted::<PackedU40Column, CE>(comp, next_virtual)
    } else {
        split_internal_parallels_sorted::<Vec<u64>, CE>(comp, next_virtual)
    }
}

fn radix_pass_component_indices<C: U64Column, CE: ComponentEdges>(
    comp: &SplitComponent<CE>,
    from: &C,
    to: &mut C,
    shift: u32,
    use_max_endpoint: bool,
) {
    const RADIX: usize = 1 << 16;
    let mut counts = [0usize; RADIX];
    for index in 0..from.len() {
        let edge = comp.edges.get(from.value(index) as usize);
        let endpoint = if use_max_endpoint {
            edge.src.max(edge.dst)
        } else {
            edge.src.min(edge.dst)
        };
        counts[((endpoint >> shift) as usize) & (RADIX - 1)] += 1;
    }
    let mut next = 0usize;
    for count in &mut counts {
        let end = next + *count;
        *count = next;
        next = end;
    }
    for index in 0..from.len() {
        let edge = comp.edges.get(from.value(index) as usize);
        let endpoint = if use_max_endpoint {
            edge.src.max(edge.dst)
        } else {
            edge.src.min(edge.dst)
        };
        let bucket = ((endpoint >> shift) as usize) & (RADIX - 1);
        let output = counts[bucket];
        counts[bucket] += 1;
        to.set_value(output, from.value(index));
    }
}

fn radix_sort_component_indices<C: U64Column, CE: ComponentEdges>(
    comp: &SplitComponent<CE>,
    order: &mut C,
    scratch: &mut C,
) {
    let mut in_order = true;
    for use_max_endpoint in [true, false] {
        for pass in 0..C::RADIX_PASSES {
            let shift = (pass * 16) as u32;
            if in_order {
                radix_pass_component_indices(comp, order, scratch, shift, use_max_endpoint);
            } else {
                radix_pass_component_indices(comp, scratch, order, shift, use_max_endpoint);
            }
            in_order = !in_order;
        }
    }
    debug_assert!(in_order);
}

fn split_internal_parallels_sorted<C: U64Column, CE: ComponentEdges>(
    comp: SplitComponent<CE>,
    next_virtual: &mut u64,
) -> Vec<SplitComponent<CE>> {
    let edge_count = comp.edges.len();
    let mut order = C::filled(edge_count, 0);
    let mut scratch = C::filled(edge_count, 0);
    for index in 0..edge_count {
        order.set_value(index, index as u64);
    }
    radix_sort_component_indices(&comp, &mut order, &mut scratch);

    let mut pos = 0usize;
    let mut has_parallel = false;
    while pos < edge_count {
        let first = comp.edges.get(order.value(pos) as usize);
        let pair = (first.src.min(first.dst), first.src.max(first.dst));
        let mut end = pos + 1;
        while end < edge_count {
            let edge = comp.edges.get(order.value(end) as usize);
            if (edge.src.min(edge.dst), edge.src.max(edge.dst)) != pair {
                break;
            }
            end += 1;
        }
        if end - pos >= 2 {
            has_parallel = true;
            break;
        }
        pos = end;
    }
    if !has_parallel {
        return vec![comp];
    }

    drop(scratch);
    let mut result: Vec<SplitComponent<CE>> = Vec::new();
    let mut remainder_edges = CE::with_capacity(edge_count);
    let mut pos = 0usize;
    while pos < edge_count {
        let first = comp.edges.get(order.value(pos) as usize);
        let a = first.src.min(first.dst);
        let b = first.src.max(first.dst);
        let mut end = pos + 1;
        while end < edge_count {
            let edge = comp.edges.get(order.value(end) as usize);
            if edge.src.min(edge.dst) != a || edge.src.max(edge.dst) != b {
                break;
            }
            end += 1;
        }
        if end - pos >= 2 {
            let vid = next_virtual_id(next_virtual);
            let mut bond_edges = CE::with_capacity(end - pos + 1);
            for index in pos..end {
                bond_edges.push(comp.edges.get(order.value(index) as usize));
            }
            bond_edges.push(StackEdge {
                src: a,
                dst: b,
                eid: vid,
            });
            result.push(SplitComponent {
                edges: bond_edges,
                pole_a: a,
                pole_b: b,
            });
            remainder_edges.push(StackEdge {
                src: a,
                dst: b,
                eid: vid,
            });
        } else {
            remainder_edges.push(first);
        }
        pos = end;
    }
    result.push(SplitComponent {
        edges: remainder_edges,
        pole_a: comp.pole_a,
        pole_b: comp.pole_b,
    });
    result
}

const CLASSIFY_HASH_NODE_BUDGET: usize = 32 * 1024 * 1024;
const CLASSIFY_HASH_NODE_LIMIT: usize = 1024 * 1024;
const DENSE_DEGREE_MASK: u8 = 0b11;
const DENSE_GENERATION_LIMIT: u8 = 0b0011_1111;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ComponentClass {
    Known(SpqrNodeType),
    NeedsDense,
}

fn classify_component_info<CE: ComponentEdges>(
    comp: &SplitComponent<CE>,
    real_edge_count: u64,
) -> (SpqrNodeType, Option<u64>) {
    let mut max_virtual = None;
    if comp.edges.len() <= 64 {
        let mut deg = [(0u64, 0u64); 128];
        let mut deg_len = 0usize;
        comp.edges.for_each(|e| {
            if e.eid >= real_edge_count {
                max_virtual = Some(max_virtual.map_or(e.eid, |value: u64| value.max(e.eid)));
            }
            for node in [e.src, e.dst] {
                if let Some(pos) = deg[..deg_len].iter().position(|&(v, _)| v == node) {
                    deg[pos].1 = deg[pos].1.saturating_add(1);
                } else {
                    deg[deg_len] = (node, 1);
                    deg_len += 1;
                }
            }
        });
        let v = deg_len;
        let e = comp.edges.len();
        if v == 2 && e >= 2 {
            return (SpqrNodeType::P, max_virtual);
        }
        if e == v && e >= 3 && deg[..deg_len].iter().all(|&(_, d)| d == 2) {
            return (SpqrNodeType::S, max_virtual);
        }
        return (SpqrNodeType::R, max_virtual);
    }

    let mut deg = Some(HashMap::<u64, u64>::new());
    let mut high_degree = false;
    for index in 0..comp.edges.len() {
        let edge = comp.edges.get(index);
        if edge.eid >= real_edge_count {
            max_virtual = Some(max_virtual.map_or(edge.eid, |value: u64| value.max(edge.eid)));
        }
        if let Some(degrees) = deg.as_mut() {
            for node in [edge.src, edge.dst] {
                let degree = degrees.entry(node).or_default();
                *degree += 1;
                high_degree |= *degree > 2;
            }
            if high_degree && degrees.len() > 2 {
                deg = None;
            }
        }
    }
    let Some(deg) = deg else {
        return (SpqrNodeType::R, max_virtual);
    };
    let v = deg.len();
    let e = comp.edges.len();
    if v == 2 && e >= 2 {
        return (SpqrNodeType::P, max_virtual);
    }
    if e == v && e >= 3 && deg.values().all(|&d| d == 2) {
        return (SpqrNodeType::S, max_virtual);
    }
    (SpqrNodeType::R, max_virtual)
}

fn classify_component_info_bounded<CE: ComponentEdges>(
    comp: &SplitComponent<CE>,
    real_edge_count: u64,
    hash_node_limit: usize,
) -> (ComponentClass, Option<u64>) {
    if comp.edges.len() <= hash_node_limit / 2 {
        let (node_type, max_virtual) = classify_component_info(comp, real_edge_count);
        return (ComponentClass::Known(node_type), max_virtual);
    }

    let mut max_virtual = None;
    let mut deg = Some(HashMap::<u64, u64>::new());
    let mut high_degree = false;
    let mut needs_dense = false;
    for index in 0..comp.edges.len() {
        let edge = comp.edges.get(index);
        if edge.eid >= real_edge_count {
            max_virtual = Some(max_virtual.map_or(edge.eid, |value: u64| value.max(edge.eid)));
        }
        let mut discard_degrees = false;
        if let Some(degrees) = deg.as_mut() {
            for node in [edge.src, edge.dst] {
                if let Some(degree) = degrees.get_mut(&node) {
                    *degree += 1;
                    high_degree |= *degree > 2;
                } else if high_degree && degrees.len() >= 2 {
                    discard_degrees = true;
                    break;
                } else if degrees.len() == hash_node_limit {
                    needs_dense = true;
                    discard_degrees = true;
                    break;
                } else {
                    degrees.insert(node, 1);
                }
            }
            if high_degree && degrees.len() > 2 {
                discard_degrees = true;
            }
        }
        if discard_degrees {
            deg = None;
        }
    }
    if needs_dense {
        return (ComponentClass::NeedsDense, max_virtual);
    }
    let Some(deg) = deg else {
        return (ComponentClass::Known(SpqrNodeType::R), max_virtual);
    };
    let v = deg.len();
    let e = comp.edges.len();
    if v == 2 && e >= 2 {
        return (ComponentClass::Known(SpqrNodeType::P), max_virtual);
    }
    if e == v && e >= 3 && deg.values().all(|&d| d == 2) {
        return (ComponentClass::Known(SpqrNodeType::S), max_virtual);
    }
    (ComponentClass::Known(SpqrNodeType::R), max_virtual)
}

fn classify_deferred_components<CE: ComponentEdges>(
    comps: &[SplitComponent<CE>],
    node_count: usize,
    deferred: &[usize],
    out: &mut [SpqrNodeType],
) {
    let mut marks = Vec::new();
    marks
        .try_reserve_exact(node_count)
        .expect("cannot allocate dense SPQR classifier");
    marks.resize(node_count, 0u8);

    let mut generation = 1u8;
    for &component_index in deferred {
        if generation > DENSE_GENERATION_LIMIT {
            marks.fill(0);
            generation = 1;
        }
        let stamp = generation << 2;
        let mut nodes = 0usize;
        let mut nodes_not_degree_two = 0usize;
        let comp = &comps[component_index];
        comp.edges.for_each(|edge| {
            for node in [edge.src, edge.dst] {
                let node = node as usize;
                debug_assert!(node < node_count);
                let mark = &mut marks[node];
                if *mark >> 2 != generation {
                    *mark = stamp | 1;
                    nodes += 1;
                    nodes_not_degree_two += 1;
                } else {
                    match *mark & DENSE_DEGREE_MASK {
                        1 => {
                            *mark = stamp | 2;
                            nodes_not_degree_two -= 1;
                        }
                        2 => {
                            *mark = stamp | 3;
                            nodes_not_degree_two += 1;
                        }
                        _ => {}
                    }
                }
            }
        });
        out[component_index] = if nodes == 2 && comp.edges.len() >= 2 {
            SpqrNodeType::P
        } else if comp.edges.len() == nodes && nodes >= 3 && nodes_not_degree_two == 0 {
            SpqrNodeType::S
        } else {
            SpqrNodeType::R
        };
        generation += 1;
    }
}

fn classify_components_parallel<CE: ComponentEdges>(
    comps: &[SplitComponent<CE>],
    node_count: usize,
    real_edge_count: usize,
) -> (Vec<SpqrNodeType>, Option<u64>) {
    const MIN_PAR_COMPONENTS: usize = 4096;
    let n = comps.len();
    let threads = spqr_thread_count().min(n.max(1));
    let hash_node_limit = (CLASSIFY_HASH_NODE_BUDGET / threads).clamp(2, CLASSIFY_HASH_NODE_LIMIT);
    let mut out = vec![SpqrNodeType::R; n];
    let mut deferred = Vec::new();
    let mut max_virtual = None;
    if threads <= 1 || n < MIN_PAR_COMPONENTS {
        for (index, comp) in comps.iter().enumerate() {
            let (classification, component_max) =
                classify_component_info_bounded(comp, real_edge_count as u64, hash_node_limit);
            match classification {
                ComponentClass::Known(node_type) => out[index] = node_type,
                ComponentClass::NeedsDense => deferred.push(index),
            }
            if let Some(value) = component_max {
                max_virtual = Some(max_virtual.map_or(value, |current: u64| current.max(value)));
            }
        }
    } else {
        let chunk = n.div_ceil(threads);
        thread::scope(|scope| {
            let mut handles = Vec::with_capacity(threads);
            for (chunk_index, (out_chunk, comp_chunk)) in
                out.chunks_mut(chunk).zip(comps.chunks(chunk)).enumerate()
            {
                let first = chunk_index * chunk;
                handles.push(scope.spawn(move || {
                    let mut local_deferred = Vec::new();
                    let mut local_max = None;
                    for (offset, (dst, comp)) in
                        out_chunk.iter_mut().zip(comp_chunk.iter()).enumerate()
                    {
                        let (classification, component_max) = classify_component_info_bounded(
                            comp,
                            real_edge_count as u64,
                            hash_node_limit,
                        );
                        match classification {
                            ComponentClass::Known(node_type) => *dst = node_type,
                            ComponentClass::NeedsDense => local_deferred.push(first + offset),
                        }
                        if let Some(value) = component_max {
                            local_max =
                                Some(local_max.map_or(value, |current: u64| current.max(value)));
                        }
                    }
                    (local_deferred, local_max)
                }));
            }
            for handle in handles {
                let (local_deferred, local_max) = handle
                    .join()
                    .expect("parallel SPQR classification worker panicked");
                deferred.extend(local_deferred);
                if let Some(value) = local_max {
                    max_virtual =
                        Some(max_virtual.map_or(value, |current: u64| current.max(value)));
                }
            }
        });
    }
    if !deferred.is_empty() {
        classify_deferred_components(comps, node_count, &deferred, &mut out);
    }
    (out, max_virtual)
}

fn merge_same_type_components<CE: ComponentEdges>(
    comps: &mut Vec<SplitComponent<CE>>,
    node_count: usize,
    m: usize,
) -> Vec<SpqrNodeType> {
    let (ctype, max_virtual_eid) = classify_components_parallel(comps, node_count, m);

    let Some(max_virtual_eid) = max_virtual_eid else {
        let mut new_comps = Vec::with_capacity(comps.len());
        let mut new_types = Vec::with_capacity(comps.len());
        for (mut comp, ty) in std::mem::take(comps).into_iter().zip(ctype) {
            if !comp.edges.is_empty() {
                comp.edges.compact();
                new_comps.push(comp);
                new_types.push(ty);
            }
        }
        *comps = new_comps;
        return new_types;
    };

    let virtual_count = (max_virtual_eid as usize) - m + 1;
    let compact_columns = (comps.len() as u64) < U40_MAX
        && (virtual_count as u64) < U40_MAX
        && (m > u32::MAX as usize
            || (virtual_count as u128) * 6 >= COMPONENT_MERGE_PACKED_MIN_SAVINGS);
    if compact_columns {
        merge_same_type_components_impl::<PackedU40Column, CE>(comps, m, ctype, virtual_count)
    } else {
        merge_same_type_components_impl::<Vec<u64>, CE>(comps, m, ctype, virtual_count)
    }
}

fn merge_same_type_components_impl<C: U64Column, CE: ComponentEdges>(
    comps: &mut Vec<SplitComponent<CE>>,
    m: usize,
    ctype: Vec<SpqrNodeType>,
    virtual_count: usize,
) -> Vec<SpqrNodeType> {
    let mut comp1 = C::filled(virtual_count, INVALID);
    let mut comp2 = C::filled(virtual_count, INVALID);

    for (ci, comp) in comps.iter().enumerate() {
        comp.edges.for_each(|e| {
            if (e.eid as usize) >= m {
                let idx = (e.eid as usize) - m;
                if comp1.value(idx) == INVALID {
                    comp1.set_value(idx, ci as u64);
                } else {
                    comp2.set_value(idx, ci as u64);
                }
            }
        });
    }

    let mut visited = vec![false; comps.len()];

    for i in 0..comps.len() {
        visited[i] = true;
        if comps[i].edges.is_empty() {
            continue;
        }

        let ti = ctype[i];
        if ti != SpqrNodeType::P && ti != SpqrNodeType::S {
            continue;
        }

        let mut ei = 0;
        while ei < comps[i].edges.len() {
            let eid = comps[i].edges.get(ei).eid;
            if (eid as usize) < m {
                ei += 1;
                continue;
            }
            let vidx = (eid as usize) - m;
            if vidx >= virtual_count {
                ei += 1;
                continue;
            }

            let c1 = comp1.value(vidx);
            let c2 = comp2.value(vidx);
            let j = match (c1, c2) {
                (a, b)
                    if a != INVALID && b != INVALID && a as usize == i && !visited[b as usize] =>
                {
                    b as usize
                }
                (a, b)
                    if a != INVALID && b != INVALID && b as usize == i && !visited[a as usize] =>
                {
                    a as usize
                }
                _ => {
                    ei += 1;
                    continue;
                }
            };

            if comps[j].edges.is_empty() || ctype[j] != ti {
                ei += 1;
                continue;
            }

            visited[j] = true;

            let mut j_edges = std::mem::take(&mut comps[j].edges);
            j_edges.retain(|e| e.eid != eid);

            j_edges.for_each(|e| {
                if (e.eid as usize) >= m {
                    let idx = (e.eid as usize) - m;
                    if idx < virtual_count {
                        if comp1.value(idx) == j as u64 {
                            comp1.set_value(idx, i as u64);
                        }
                        if comp2.value(idx) == j as u64 {
                            comp2.set_value(idx, i as u64);
                        }
                    }
                }
            });

            comps[i].edges.swap_remove(ei);
            comps[i].edges.append(&mut j_edges);
        }
    }

    drop(comp1);
    drop(comp2);
    drop(visited);
    let mut new_comps = Vec::with_capacity(comps.len());
    let mut new_types = Vec::with_capacity(comps.len());
    for (mut comp, ty) in std::mem::take(comps).into_iter().zip(ctype) {
        if !comp.edges.is_empty() {
            comp.edges.compact();
            new_comps.push(comp);
            new_types.push(ty);
        }
    }
    *comps = new_comps;
    new_types
}

fn assemble_spqr_tree<CE: ComponentEdges>(
    node_count: usize,
    edge_count: usize,
    components: Vec<SplitComponent<CE>>,
    component_types: Vec<SpqrNodeType>,
    next_virtual: u64,
    track_edge_mapping: bool,
) -> SpqrTree {
    if (node_count <= u32::MAX as usize && edge_count <= u32::MAX as usize)
        || components.len() as u64 >= U40_MAX
    {
        if track_edge_mapping {
            assemble_spqr_tree_impl::<Vec<u64>, Vec<SkeletonEdge>, Vec<NodeId>, true, CE>(
                node_count,
                edge_count,
                components,
                component_types,
                next_virtual,
            )
            .into_tree()
        } else {
            assemble_spqr_tree_impl::<Vec<u64>, Vec<SkeletonEdge>, Vec<NodeId>, false, CE>(
                node_count,
                edge_count,
                components,
                component_types,
                next_virtual,
            )
            .into_tree()
        }
    } else if track_edge_mapping {
        assemble_spqr_tree_impl::<PackedU40Column, Vec<SkeletonEdge>, Vec<NodeId>, true, CE>(
            node_count,
            edge_count,
            components,
            component_types,
            next_virtual,
        )
        .into_tree()
    } else {
        assemble_spqr_tree_impl::<PackedU40Column, Vec<SkeletonEdge>, Vec<NodeId>, false, CE>(
            node_count,
            edge_count,
            components,
            component_types,
            next_virtual,
        )
        .into_tree()
    }
}

fn compact_payload_saves_enough(
    edge_count: usize,
    skeleton_edges: usize,
    next_virtual: u64,
) -> bool {
    let virtual_ids = next_virtual.saturating_sub(edge_count as u64) as u128;
    let plain = 48u128 * skeleton_edges as u128;
    let packed = 16u128 * skeleton_edges as u128 + 20u128 * virtual_ids;
    plain.saturating_sub(packed) >= PAYLOAD_SKELETON_MIN_SAVINGS
}

fn assemble_spqr_payload_tree<CE: ComponentEdges>(
    node_count: usize,
    edge_count: usize,
    components: Vec<SplitComponent<CE>>,
    component_types: Vec<SpqrNodeType>,
    next_virtual: u64,
) -> SpqrPayloadTree {
    let skeleton_edges = components
        .iter()
        .try_fold(0usize, |total, component| {
            total.checked_add(component.edges.len())
        })
        .expect("too many SPQR skeleton edges");
    const U40_MAX_USIZE: usize = U40_MAX as usize;
    const PACKED_LIMIT: usize = U40_MAX_USIZE - 1;
    if node_count < PACKED_LIMIT
        && edge_count < PACKED_LIMIT
        && components.len() < PACKED_LIMIT
        && skeleton_edges < PACKED_LIMIT
        && skeleton_edges >= PAYLOAD_SKELETON_PACK_MIN
        && compact_payload_saves_enough(edge_count, skeleton_edges, next_virtual)
        && next_virtual < U40_MAX
    {
        assemble_spqr_tree_impl::<PackedU40Column, PackedSkeletonEdges, PackedU40Column, false, CE>(
            node_count,
            edge_count,
            components,
            component_types,
            next_virtual,
        )
        .into_payload_tree()
    } else {
        assemble_spqr_tree_impl::<Vec<u64>, Vec<SkeletonEdge>, Vec<NodeId>, false, CE>(
            node_count,
            edge_count,
            components,
            component_types,
            next_virtual,
        )
        .into_payload_tree()
    }
}

#[inline(always)]
fn local_node<C: U64Column, M: NodeMappingStorage>(
    node: u64,
    seen: &mut [u32],
    local: &mut C,
    stamp: u32,
    mapping: &mut M,
    node_count: &mut u64,
) -> u64 {
    let index = node as usize;
    if index < seen.len() && seen[index] == stamp {
        return local.value(index);
    }
    let result = *node_count;
    *node_count += 1;
    if index < seen.len() {
        seen[index] = stamp;
        local.set_value(index, result);
    }
    mapping.push_node(NodeId(node));
    result
}

fn peel_component_tree<C: U64Column>(
    num_nodes: usize,
    pair_count: usize,
    first_tree: &C,
    second_tree: &C,
    parents: &mut [TreeNodeId],
) -> Option<(C, C)> {
    peel_component_tree_with_min_nodes(
        num_nodes,
        pair_count,
        first_tree,
        second_tree,
        parents,
        TREE_PEEL_MIN_NODES,
    )
}

fn peel_component_tree_with_min_nodes<C: U64Column>(
    num_nodes: usize,
    pair_count: usize,
    first_tree: &C,
    second_tree: &C,
    parents: &mut [TreeNodeId],
    min_nodes: usize,
) -> Option<(C, C)> {
    if num_nodes < min_nodes
        || num_nodes >= U40_MAX as usize
        || pair_count.checked_add(1) != Some(num_nodes)
    {
        return None;
    }

    parents.fill(TreeNodeId(0));
    let mut degree = C::filled(num_nodes, 0);
    for pair in 0..second_tree.len() {
        if second_tree.value(pair) == INVALID {
            continue;
        }
        let first = first_tree.value(pair) as usize;
        let second = second_tree.value(pair) as usize;
        if first >= num_nodes || second >= num_nodes || first == second {
            parents.fill(TreeNodeId::INVALID);
            return None;
        }
        let Some(first_degree) = degree.value(first).checked_add(1) else {
            parents.fill(TreeNodeId::INVALID);
            return None;
        };
        let Some(second_degree) = degree.value(second).checked_add(1) else {
            parents.fill(TreeNodeId::INVALID);
            return None;
        };
        degree.set_value(first, first_degree);
        degree.set_value(second, second_degree);
        parents[first].0 ^= second as u64;
        parents[second].0 ^= first as u64;
    }

    let mut leaves = C::with_capacity(num_nodes.min(1 << 20));
    for node in 1..num_nodes {
        if degree.value(node) == 1 {
            leaves.push_value(node as u64);
        }
    }

    let mut removed = 0usize;
    while let Some(node) = leaves.pop_value() {
        let node = node as usize;
        if node >= num_nodes || degree.value(node) != 1 {
            parents.fill(TreeNodeId::INVALID);
            return None;
        }
        let parent = parents[node].0 as usize;
        if parent >= num_nodes || parent == node {
            parents.fill(TreeNodeId::INVALID);
            return None;
        }
        let parent_degree = degree.value(parent);
        if parent_degree == 0 {
            parents.fill(TreeNodeId::INVALID);
            return None;
        }
        degree.set_value(node, 0);
        degree.set_value(parent, parent_degree - 1);
        parents[parent].0 ^= node as u64;
        parents[node] = TreeNodeId(parent as u64);
        removed += 1;
        if parent != 0 && parent_degree == 2 {
            leaves.push_value(parent as u64);
        }
    }

    if removed != pair_count || degree.value(0) != 0 {
        parents.fill(TreeNodeId::INVALID);
        return None;
    }
    parents[0] = TreeNodeId::INVALID;
    Some((degree, leaves))
}

fn assemble_spqr_tree_impl<
    C: U64Column + PayloadPairColumn,
    E: SkeletonEdgeStorage,
    M: NodeMappingStorage,
    const TRACK_EDGE_MAPPING: bool,
    CE: ComponentEdges,
>(
    node_count: usize,
    edge_count: usize,
    components: Vec<SplitComponent<CE>>,
    component_types: Vec<SpqrNodeType>,
    next_virtual: u64,
) -> AssembledSpqrTree<E, M> {
    let m = edge_count;
    let base = m as u64;

    let skeleton_edges = components
        .iter()
        .try_fold(0usize, |total, component| {
            total.checked_add(component.edges.len())
        })
        .expect("too many SPQR skeleton edges");
    let mut builder =
        SpqrTreeBuilder::<E, M, TRACK_EDGE_MAPPING>::new(m, components.len(), skeleton_edges);
    assert_eq!(component_types.len(), components.len());
    let mut dense_seen = vec![0u32; node_count];
    let mut dense_local = C::filled(node_count, 0);
    let mut dense_stamp = 1u32;

    for (comp, nt) in components.into_iter().zip(component_types) {
        let mut component_nodes = 0;
        local_node(
            comp.pole_a,
            &mut dense_seen,
            &mut dense_local,
            dense_stamp,
            &mut builder.node_mapping,
            &mut component_nodes,
        );
        local_node(
            comp.pole_b,
            &mut dense_seen,
            &mut dense_local,
            dense_stamp,
            &mut builder.node_mapping,
            &mut component_nodes,
        );
        let tid = builder.begin_node(nt);
        comp.edges.for_each(|edge| {
            let ls_val = local_node(
                edge.src,
                &mut dense_seen,
                &mut dense_local,
                dense_stamp,
                &mut builder.node_mapping,
                &mut component_nodes,
            );
            let ld_val = local_node(
                edge.dst,
                &mut dense_seen,
                &mut dense_local,
                dense_stamp,
                &mut builder.node_mapping,
                &mut component_nodes,
            );
            let ls = NodeId(ls_val);
            let ld = NodeId(ld_val);
            let is_real = (edge.eid as usize) < m;
            builder.push_edge(
                tid,
                SkeletonEdge {
                    src: ls,
                    dst: ld,
                    real_edge: if is_real {
                        EdgeId(edge.eid)
                    } else {
                        EdgeId::INVALID
                    },
                    virtual_id: if is_real { INVALID } else { edge.eid },
                    twin_tree_node: TreeNodeId::INVALID,
                    twin_edge_idx: INVALID,
                },
            );
        });
        builder.finish_node(tid, component_nodes);
        dense_stamp = dense_stamp.wrapping_add(1);
        if dense_stamp == 0 {
            dense_seen.fill(0);
            dense_stamp = 1;
        }
    }

    drop(dense_local);
    drop(dense_seen);

    let num_nodes = builder.num_nodes();
    if num_nodes == 0 {
        return builder.finalize_empty();
    }

    let num_virtual = next_virtual.saturating_sub(base) as usize;
    let mut first_tree = C::filled(num_virtual, INVALID);
    let mut first_edge = C::filled(num_virtual, INVALID);
    let mut second_tree = C::filled(num_virtual, INVALID);
    let mut second_edge = C::filled(num_virtual, INVALID);

    for ti in 0..num_nodes {
        for ei in 0..builder.skeleton_edges_len(TreeNodeId(ti as u64)) {
            let vid = builder.skeleton_edge(TreeNodeId(ti as u64), ei).virtual_id;
            if vid == INVALID {
                continue;
            }
            assert!(vid >= base, "virtual_id {} < base {}", vid, base);
            let idx = (vid - base) as usize;
            assert!(idx < num_virtual, "virtual_id {} out of range", vid);
            let tj = first_tree.value(idx);
            if tj != INVALID {
                assert_eq!(
                    second_tree.value(idx),
                    INVALID,
                    "virtual edge appears more than twice"
                );
                let ej = first_edge.value(idx);
                let ta = TreeNodeId(ti as u64);
                let tb = TreeNodeId(tj);
                second_tree.set_value(idx, ti as u64);
                second_edge.set_value(idx, ei as u64);
                builder.pair_virtual(tb, ej as usize, ta, ei);
            } else {
                first_tree.set_value(idx, ti as u64);
                first_edge.set_value(idx, ei as u64);
            }
        }
    }

    let mut pair_count = 0usize;
    for idx in 0..num_virtual {
        if first_tree.value(idx) != INVALID && second_tree.value(idx) == INVALID {
            let tree_node = TreeNodeId(first_tree.value(idx));
            let edge_index = first_edge.value(idx) as usize;
            builder.clear_virtual(tree_node, edge_index);
        } else if second_tree.value(idx) != INVALID {
            assert_ne!(first_tree.value(idx), INVALID);
            pair_count += 1;
        }
    }

    let root = TreeNodeId(0);
    let peeled = peel_component_tree(
        num_nodes,
        pair_count,
        &first_tree,
        &second_tree,
        &mut builder.node_parents,
    );
    if peeled.is_none() {
        let mut tree_adj_count = C::filled(num_nodes, 0);
        for pair in 0..num_virtual {
            let second = second_tree.value(pair);
            if second == INVALID {
                continue;
            }
            let first = first_tree.value(pair) as usize;
            tree_adj_count.set_value(first, tree_adj_count.value(first) + 1);
            tree_adj_count.set_value(second as usize, tree_adj_count.value(second as usize) + 1);
        }
        let mut tree_adj_offsets: Vec<u64> = vec![0; num_nodes + 1];
        for i in 0..num_nodes {
            tree_adj_offsets[i + 1] = tree_adj_offsets[i] + tree_adj_count.value(i);
        }
        let tree_adj_total = tree_adj_offsets[num_nodes] as usize;
        let mut tree_adj_flat = C::filled(tree_adj_total, INVALID);
        let mut tree_adj_write = C::with_capacity(num_nodes);
        for &offset in &tree_adj_offsets[..num_nodes] {
            tree_adj_write.push_value(offset);
        }
        for pair in 0..num_virtual {
            if second_tree.value(pair) == INVALID {
                continue;
            }
            let first = first_tree.value(pair) as usize;
            let second = second_tree.value(pair) as usize;
            assert!(first != second);
            tree_adj_flat.set_value(tree_adj_write.value(first) as usize, second as u64);
            tree_adj_write.set_value(first, tree_adj_write.value(first) + 1);
            tree_adj_flat.set_value(tree_adj_write.value(second) as usize, first as u64);
            tree_adj_write.set_value(second, tree_adj_write.value(second) + 1);
        }
        drop(tree_adj_write);
        drop(tree_adj_count);

        let parents = &mut builder.node_parents;
        let mut st = vec![root];
        parents[0] = root;
        while let Some(v) = st.pop() {
            for idx in tree_adj_offsets[v.idx()] as usize..tree_adj_offsets[v.idx() + 1] as usize {
                let u = TreeNodeId(tree_adj_flat.value(idx));
                if parents[u.idx()].is_valid() {
                    continue;
                }
                parents[u.idx()] = v;
                st.push(u);
            }
        }
        drop(st);
        drop(tree_adj_flat);
        drop(tree_adj_offsets);

        for parent in parents.iter_mut() {
            if !parent.is_valid() {
                *parent = root;
            }
        }
        parents[0] = TreeNodeId::INVALID;
    }

    let payload_pairs =
        C::into_payload_pairs(base, first_tree, first_edge, second_tree, second_edge);
    let (mut children_count, mut children_write) = match peeled {
        Some((counts, mut scratch)) => {
            scratch.clear_values();
            (counts, scratch)
        }
        None => (C::filled(num_nodes, 0), C::with_capacity(num_nodes)),
    };
    for &parent in &builder.node_parents {
        if parent.is_valid() {
            let index = parent.idx();
            children_count.set_value(index, children_count.value(index) + 1);
        }
    }
    let mut children_offsets: Vec<u64> = vec![0; num_nodes + 1];
    for i in 0..num_nodes {
        children_offsets[i + 1] = children_offsets[i] + children_count.value(i);
        children_write.push_value(children_offsets[i]);
    }
    drop(children_count);
    let mut children_flat: Vec<TreeNodeId> =
        vec![TreeNodeId::INVALID; children_offsets[num_nodes] as usize];
    for (node, &parent) in builder.node_parents.iter().enumerate() {
        if parent.is_valid() {
            let index = parent.idx();
            children_flat[children_write.value(index) as usize] = TreeNodeId(node as u64);
            children_write.set_value(index, children_write.value(index) + 1);
        }
    }

    builder.finalize_with_children(root, children_offsets, children_flat, payload_pairs)
}

#[inline]
fn mix64(state: u64, x: u64) -> u64 {
    let mut h = state ^ x;
    h = h.wrapping_mul(0x100000001b3);
    h ^= h >> 33;
    h = h.wrapping_mul(0xc2b2ae3d27d4eb4f);
    h ^= h >> 29;
    h
}

impl SpqrTree {
    pub fn canonicalize_skeleton_node_order(&mut self) {
        let n = self.len();
        if n == 0 {
            return;
        }
        for tn in 0..n {
            let nm_s = self.node_mapping_offsets[tn] as usize;
            let nm_e = self.node_mapping_offsets[tn + 1] as usize;
            let k = nm_e - nm_s;
            if k <= 1 {
                continue;
            }
            let mut perm: Vec<u64> = (0..k as u64).collect();
            perm.sort_by_key(|&old_local| self.node_mapping[nm_s + old_local as usize].0);
            let mut inv = vec![0u64; k];
            for new_local in 0..k {
                inv[perm[new_local] as usize] = new_local as u64;
            }
            if (0..k).all(|i| perm[i] == i as u64) {
                continue;
            }
            let new_nm: Vec<NodeId> = (0..k)
                .map(|new_local| self.node_mapping[nm_s + perm[new_local] as usize])
                .collect();
            self.node_mapping[nm_s..(nm_s + k)].copy_from_slice(&new_nm[..k]);
            let s = self.skeleton_offsets[tn] as usize;
            let e = self.skeleton_offsets[tn + 1] as usize;
            for i in s..e {
                let edge = &mut self.skeleton_edges[i];
                edge.src = NodeId(inv[edge.src.0 as usize]);
                edge.dst = NodeId(inv[edge.dst.0 as usize]);
            }
        }
    }

    pub fn canonicalize_skeleton_edge_orientation(&mut self) {
        for edge in &mut self.skeleton_edges {
            if edge.src.0 > edge.dst.0 {
                std::mem::swap(&mut edge.src, &mut edge.dst);
            }
        }
    }

    pub fn move_root_to_zero(&mut self) {
        let n = self.len();
        if n == 0 {
            return;
        }
        let r = self.root.idx();
        if r == 0 {
            return;
        }
        self.node_types.swap(0, r);
        self.skeleton_num_nodes.swap(0, r);
        if self.min_real_per_node.len() == n {
            self.min_real_per_node.swap(0, r);
        }

        let mut new_nm_offsets = vec![0u64; n + 1];
        let mut new_nm: Vec<NodeId> = Vec::with_capacity(self.node_mapping.len());
        let perm = |i: usize| -> usize {
            if i == 0 {
                r
            } else if i == r {
                0
            } else {
                i
            }
        };
        for i in 0..n {
            let src_i = perm(i);
            let s = self.node_mapping_offsets[src_i] as usize;
            let e = self.node_mapping_offsets[src_i + 1] as usize;
            new_nm.extend_from_slice(&self.node_mapping[s..e]);
            new_nm_offsets[i + 1] = new_nm.len() as u64;
        }
        self.node_mapping_offsets = new_nm_offsets;
        self.node_mapping = new_nm;

        let mut new_sk_offsets = vec![0u64; n + 1];
        let mut new_sk: Vec<SkeletonEdge> = Vec::with_capacity(self.skeleton_edges.len());
        for i in 0..n {
            let src_i = perm(i);
            let s = self.skeleton_offsets[src_i] as usize;
            let e = self.skeleton_offsets[src_i + 1] as usize;
            new_sk.extend_from_slice(&self.skeleton_edges[s..e]);
            new_sk_offsets[i + 1] = new_sk.len() as u64;
        }
        self.skeleton_offsets = new_sk_offsets;
        self.skeleton_edges = new_sk;

        let mut new_ch_offsets = vec![0u64; n + 1];
        let mut new_ch: Vec<TreeNodeId> = Vec::with_capacity(self.children.len());
        for i in 0..n {
            let src_i = perm(i);
            let s = self.children_offsets[src_i] as usize;
            let e = self.children_offsets[src_i + 1] as usize;
            new_ch.extend_from_slice(&self.children[s..e]);
            new_ch_offsets[i + 1] = new_ch.len() as u64;
        }
        self.children_offsets = new_ch_offsets;
        self.children = new_ch;

        self.node_parents.swap(0, r);
        for p in &mut self.node_parents {
            if !p.is_valid() {
                continue;
            }
            let pi = p.idx();
            if pi == 0 {
                *p = TreeNodeId(r as u64);
            } else if pi == r {
                *p = TreeNodeId(0);
            }
        }

        for c in &mut self.children {
            if !c.is_valid() {
                continue;
            }
            let ci = c.idx();
            if ci == 0 {
                *c = TreeNodeId(r as u64);
            } else if ci == r {
                *c = TreeNodeId(0);
            }
        }

        for e in &mut self.skeleton_edges {
            if !e.twin_tree_node.is_valid() {
                continue;
            }
            let ti = e.twin_tree_node.idx();
            if ti == 0 {
                e.twin_tree_node = TreeNodeId(r as u64);
            } else if ti == r {
                e.twin_tree_node = TreeNodeId(0);
            }
        }

        for tn in &mut self.edge_to_tree_node {
            if !tn.is_valid() {
                continue;
            }
            let ti = tn.idx();
            if ti == 0 {
                *tn = TreeNodeId(r as u64);
            } else if ti == r {
                *tn = TreeNodeId(0);
            }
        }

        self.root = TreeNodeId(0);
    }

    pub fn recompute_min_real_per_node(&mut self) {
        let n = self.len();
        self.min_real_per_node = vec![u64::MAX; n];
        for tn in 0..n {
            let s = self.skeleton_offsets[tn] as usize;
            let e = self.skeleton_offsets[tn + 1] as usize;
            let mut local_min = u64::MAX;
            for i in s..e {
                let re = self.skeleton_edges[i].real_edge;
                if re.is_valid() && re.0 < local_min {
                    local_min = re.0;
                }
            }
            self.min_real_per_node[tn] = local_min;
        }
    }

    fn reroot(&mut self, new_root: TreeNodeId) {
        let n = self.len();
        let mut current = new_root;
        let mut parent = TreeNodeId::INVALID;
        loop {
            let old_parent = self.node_parents[current.idx()];
            self.node_parents[current.idx()] = parent;
            if !old_parent.is_valid() || old_parent == current {
                break;
            }
            parent = current;
            current = old_parent;
        }
        self.root = new_root;

        let mut write = vec![0u64; n];
        for tn in 0..n {
            let parent = self.node_parents[tn];
            if parent.is_valid() {
                write[parent.idx()] += 1;
            }
        }
        self.children_offsets.resize(n + 1, 0);
        self.children_offsets[0] = 0;
        for i in 0..n {
            self.children_offsets[i + 1] = self.children_offsets[i] + write[i];
        }
        self.children
            .resize(self.children_offsets[n] as usize, TreeNodeId::INVALID);
        write.copy_from_slice(&self.children_offsets[..n]);
        for tn in 0..n {
            let parent = self.node_parents[tn];
            if parent.is_valid() {
                self.children[write[parent.idx()] as usize] = TreeNodeId(tn as u64);
                write[parent.idx()] += 1;
            }
        }
    }

    pub fn canonicalize_root(&mut self) {
        if !CANONICALIZE_ROOT_ENABLED.load(Ordering::Relaxed) {
            return;
        }
        let n = self.len();
        if n == 0 {
            return;
        }

        let min_real: &[u64] = if self.min_real_per_node.len() == n {
            &self.min_real_per_node
        } else {
            self.recompute_min_real_per_node();
            &self.min_real_per_node
        };

        let mut new_root = 0usize;
        for tn in 1..n {
            if min_real[tn] < min_real[new_root]
                || (min_real[tn] == min_real[new_root] && tn < new_root)
            {
                new_root = tn;
            }
        }

        let new_root_id = TreeNodeId(new_root as u64);
        if self.root == new_root_id {
            return;
        }

        self.reroot(new_root_id);

        self.canonicalize_skeleton_edge_order();
    }

    fn canonicalize_skeleton_edge_order(&mut self) {
        let n = self.len();
        if n == 0 {
            return;
        }

        for _iter in 0..32 {
            let hashes = self.compute_canonical_hashes();

            let mut any_change = false;
            for tn in 0..n {
                let s = self.skeleton_offsets[tn] as usize;
                let e = self.skeleton_offsets[tn + 1] as usize;
                let k = e - s;
                if k <= 1 {
                    continue;
                }

                #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
                struct Key {
                    is_virtual: u8,
                    a: u64,
                    b: u64,
                }
                let mut keyed: Vec<(u64, Key)> = (0..k as u64)
                    .map(|i| {
                        let edge = &self.skeleton_edges[s + i as usize];
                        let key = if edge.real_edge.is_valid() {
                            Key {
                                is_virtual: 0,
                                a: edge.real_edge.0,
                                b: 0,
                            }
                        } else {
                            Key {
                                is_virtual: 1,
                                a: hashes[edge.twin_tree_node.idx()],
                                b: edge.twin_edge_idx,
                            }
                        };
                        (i, key)
                    })
                    .collect();
                keyed.sort_by_key(|(_, k)| *k);

                let mut new_pos = vec![0u64; k];
                for (new_idx, (old_idx, _)) in keyed.iter().enumerate() {
                    new_pos[*old_idx as usize] = new_idx as u64;
                }
                let is_identity = (0..k).all(|i| new_pos[i] == i as u64);
                if is_identity {
                    continue;
                }
                any_change = true;

                let new_edges: Vec<SkeletonEdge> = keyed
                    .iter()
                    .map(|(old_idx, _)| self.skeleton_edges[s + *old_idx as usize])
                    .collect();
                self.skeleton_edges[s..(s + k)].copy_from_slice(&new_edges[..k]);

                for new_idx in 0..k {
                    let edge = self.skeleton_edges[s + new_idx];
                    if !edge.real_edge.is_valid() {
                        let twin_tn = edge.twin_tree_node.idx();
                        let twin_idx = edge.twin_edge_idx as usize;
                        let twin_so = self.skeleton_offsets[twin_tn] as usize;
                        self.skeleton_edges[twin_so + twin_idx].twin_edge_idx = new_idx as u64;
                    }
                }
            }
            if !any_change {
                break;
            }
        }
    }

    fn compute_canonical_hashes(&self) -> Vec<u64> {
        let n = self.len();
        let mut hashes: Vec<u64> = vec![0u64; n];

        for tn in 0..n {
            let s = self.skeleton_offsets[tn] as usize;
            let e = self.skeleton_offsets[tn + 1] as usize;
            let mut real_eids: Vec<u64> = Vec::new();
            let mut n_virt: u64 = 0;
            for i in s..e {
                let ed = &self.skeleton_edges[i];
                if ed.real_edge.is_valid() {
                    real_eids.push(ed.real_edge.0);
                } else {
                    n_virt += 1;
                }
            }
            real_eids.sort();
            let mut h: u64 = 0xcbf29ce484222325;
            let ty_byte = match self.node_types[tn] {
                SpqrNodeType::S => 0u8,
                SpqrNodeType::P => 1,
                SpqrNodeType::R => 2,
            };
            h = mix64(h, ty_byte as u64);
            h = mix64(h, n_virt);
            for r in &real_eids {
                h = mix64(h, *r);
            }
            hashes[tn] = h;
        }

        for _iter in 0..16 {
            let mut new_hashes = hashes.clone();
            for tn in 0..n {
                let s = self.skeleton_offsets[tn] as usize;
                let e = self.skeleton_offsets[tn + 1] as usize;
                let mut neigh_hashes: Vec<u64> = Vec::new();
                for i in s..e {
                    let ed = &self.skeleton_edges[i];
                    if !ed.real_edge.is_valid() {
                        neigh_hashes.push(hashes[ed.twin_tree_node.idx()]);
                    }
                }
                neigh_hashes.sort();
                let mut h = hashes[tn];
                for nh in &neigh_hashes {
                    h = mix64(h, *nh);
                }
                new_hashes[tn] = h;
            }
            if new_hashes == hashes {
                break;
            }
            hashes = new_hashes;
        }
        hashes
    }

    pub fn normalize(&mut self) {
        let n = self.len();
        if n == 0 {
            return;
        }

        let mut parent_set: Vec<u64> = (0..n as u64).collect();

        fn find(p: &mut [u64], x: u64) -> u64 {
            let mut r = x;
            while p[r as usize] != r {
                r = p[r as usize];
            }
            let mut cur = x;
            while p[cur as usize] != r {
                let next = p[cur as usize];
                p[cur as usize] = r;
                cur = next;
            }
            r
        }

        for i in 0..n {
            let p_id = self.node_parents[i];
            if !p_id.is_valid() || p_id.idx() == i {
                continue;
            }
            let pi = p_id.idx();

            let t = self.node_types[i];
            if t != SpqrNodeType::S && t != SpqrNodeType::P {
                continue;
            }
            if self.node_types[pi] != t {
                continue;
            }

            let i_num = self.skeleton_offsets[i + 1] - self.skeleton_offsets[i];
            if i_num == 0 {
                continue;
            }
            let p_num = self.skeleton_offsets[pi + 1] - self.skeleton_offsets[pi];
            if p_num == 0 {
                continue;
            }

            let r_x = find(&mut parent_set, i as u64);
            let r_p = find(&mut parent_set, pi as u64);
            if r_x != r_p {
                parent_set[r_x as usize] = r_p;
            }
        }

        let mut absorbed_into: Vec<Option<TreeNodeId>> = vec![None; n];
        let mut any_absorbed = false;
        for i in 0..n {
            let r = find(&mut parent_set, i as u64) as usize;
            if r != i {
                absorbed_into[i] = Some(TreeNodeId(r as u64));
                any_absorbed = true;
            }
        }

        if !any_absorbed {
            return;
        }

        self.rebuild_with_merges(&absorbed_into);
    }

    fn rebuild_with_merges(&mut self, absorbed_into: &[Option<TreeNodeId>]) {
        let n = self.len();

        let mut old_to_new: Vec<TreeNodeId> = vec![TreeNodeId::INVALID; n];
        let mut new_idx = 0u64;
        for i in 0..n {
            if absorbed_into[i].is_none() {
                old_to_new[i] = TreeNodeId(new_idx);
                new_idx += 1;
            }
        }

        for i in 0..n {
            if let Some(parent) = absorbed_into[i] {
                old_to_new[i] = old_to_new[parent.idx()];
            }
        }

        let new_count = new_idx as usize;
        if new_count == n {
            return;
        }

        let mut new_local_maps: Vec<std::collections::HashMap<u64, u64>> =
            vec![std::collections::HashMap::new(); new_count];

        for i in 0..n {
            let new_i = old_to_new[i].idx();
            let map_start = self.node_mapping_offsets[i] as usize;
            let map_end = self.node_mapping_offsets[i + 1] as usize;

            for local_idx in 0..(map_end - map_start) {
                let orig_node = self.node_mapping[map_start + local_idx].0;
                let next_idx = new_local_maps[new_i].len() as u64;
                new_local_maps[new_i].entry(orig_node).or_insert(next_idx);
            }
        }

        let mut local_remaps: Vec<Vec<u64>> = Vec::with_capacity(n);
        for i in 0..n {
            let new_i = old_to_new[i].idx();
            let map_start = self.node_mapping_offsets[i] as usize;
            let map_end = self.node_mapping_offsets[i + 1] as usize;
            let num_local = map_end - map_start;

            let mut remap = vec![0u64; num_local];
            for local_idx in 0..num_local {
                let orig_node = self.node_mapping[map_start + local_idx].0;
                remap[local_idx] = new_local_maps[new_i][&orig_node];
            }
            local_remaps.push(remap);
        }

        let mut edge_counts: Vec<u64> = vec![0; new_count];
        let mut child_counts: Vec<u64> = vec![0; new_count];

        for i in 0..n {
            let new_i = old_to_new[i].idx();

            let edge_start = self.skeleton_offsets[i] as usize;
            let edge_end = self.skeleton_offsets[i + 1] as usize;
            for ei in edge_start..edge_end {
                let e = &self.skeleton_edges[ei];
                if e.twin_tree_node.is_valid() {
                    let twin_new = old_to_new[e.twin_tree_node.idx()];
                    if twin_new == TreeNodeId(new_i as u64) {
                        continue;
                    }
                }
                edge_counts[new_i] += 1;
            }

            if absorbed_into[i].is_none() {
                let cs = self.children_offsets[i] as usize;
                let ce = self.children_offsets[i + 1] as usize;
                for ci in cs..ce {
                    let child = self.children[ci];
                    if child.is_valid() && absorbed_into[child.idx()].is_none() {
                        child_counts[new_i] += 1;
                    }
                }
            }
        }

        for j in 0..n {
            if let Some(parent_tid) = absorbed_into[j] {
                let new_i = old_to_new[parent_tid.idx()].idx();
                let cs = self.children_offsets[j] as usize;
                let ce = self.children_offsets[j + 1] as usize;
                for ci in cs..ce {
                    let child = self.children[ci];
                    if child.is_valid() && absorbed_into[child.idx()].is_none() {
                        child_counts[new_i] += 1;
                    }
                }
            }
        }

        let total_edges: usize = edge_counts.iter().map(|&x| x as usize).sum();
        let total_children: usize = child_counts.iter().map(|&x| x as usize).sum();

        let mut new_skeleton_offsets: Vec<u64> = Vec::with_capacity(new_count + 1);
        let mut new_children_offsets: Vec<u64> = Vec::with_capacity(new_count + 1);
        let mut new_mapping_offsets: Vec<u64> = Vec::with_capacity(new_count + 1);

        new_skeleton_offsets.push(0);
        new_children_offsets.push(0);
        new_mapping_offsets.push(0);

        for i in 0..new_count {
            new_skeleton_offsets.push(new_skeleton_offsets[i] + edge_counts[i]);
            new_children_offsets.push(new_children_offsets[i] + child_counts[i]);
            new_mapping_offsets.push(new_mapping_offsets[i] + new_local_maps[i].len() as u64);
        }

        let total_mapping = new_mapping_offsets[new_count] as usize;

        let mut new_node_types: Vec<SpqrNodeType> = vec![SpqrNodeType::R; new_count];
        let mut new_node_parents: Vec<TreeNodeId> = vec![TreeNodeId::INVALID; new_count];
        let mut new_skeleton_num_nodes: Vec<u64> = vec![0; new_count];

        let mut new_skeleton_edges: Vec<SkeletonEdge> = vec![SkeletonEdge::default(); total_edges];
        let mut new_children: Vec<TreeNodeId> = vec![TreeNodeId::INVALID; total_children];
        let mut new_node_mapping: Vec<NodeId> = vec![NodeId::INVALID; total_mapping];

        let mut edge_write_pos: Vec<u64> = new_skeleton_offsets[..new_count].to_vec();
        let mut child_write_pos: Vec<u64> = new_children_offsets[..new_count].to_vec();

        let mut old_to_new_edge_idx: std::collections::HashMap<(u64, u64), u64> =
            std::collections::HashMap::new();
        {
            let mut counters: Vec<u64> = vec![0u64; new_count];
            for i in 0..n {
                let new_i = old_to_new[i].idx();
                let edge_start = self.skeleton_offsets[i] as usize;
                let edge_end = self.skeleton_offsets[i + 1] as usize;
                for ei in edge_start..edge_end {
                    let e = &self.skeleton_edges[ei];
                    if e.twin_tree_node.is_valid() {
                        let twin_new = old_to_new[e.twin_tree_node.idx()];
                        if twin_new == TreeNodeId(new_i as u64) {
                            continue;
                        }
                    }
                    old_to_new_edge_idx
                        .insert((i as u64, (ei - edge_start) as u64), counters[new_i]);
                    counters[new_i] += 1;
                }
            }
        }

        for new_i in 0..new_count {
            let map = &new_local_maps[new_i];
            let map_start = new_mapping_offsets[new_i] as usize;

            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_by_key(|(_, &idx)| idx);

            for (orig_node, &new_local) in entries {
                new_node_mapping[map_start + new_local as usize] = NodeId(*orig_node);
            }

            new_skeleton_num_nodes[new_i] = map.len() as u64;
        }

        for i in 0..n {
            if absorbed_into[i].is_some() {
                continue;
            }

            let new_i = old_to_new[i].idx();

            new_node_types[new_i] = self.node_types[i];
            let parent = self.node_parents[i];
            new_node_parents[new_i] = if parent.is_valid() {
                old_to_new[parent.idx()]
            } else {
                TreeNodeId::INVALID
            };

            let cs = self.children_offsets[i] as usize;
            let ce = self.children_offsets[i + 1] as usize;
            for ci in cs..ce {
                let child = self.children[ci];
                if child.is_valid() && absorbed_into[child.idx()].is_none() {
                    let pos = child_write_pos[new_i] as usize;
                    new_children[pos] = old_to_new[child.idx()];
                    child_write_pos[new_i] += 1;
                }
            }
        }

        for j in 0..n {
            if let Some(parent_tid) = absorbed_into[j] {
                let new_i = old_to_new[parent_tid.idx()].idx();
                let cs = self.children_offsets[j] as usize;
                let ce = self.children_offsets[j + 1] as usize;
                for ci in cs..ce {
                    let child = self.children[ci];
                    if child.is_valid() && absorbed_into[child.idx()].is_none() {
                        let pos = child_write_pos[new_i] as usize;
                        new_children[pos] = old_to_new[child.idx()];
                        child_write_pos[new_i] += 1;
                    }
                }
            }
        }

        for i in 0..n {
            let new_i = old_to_new[i].idx();
            let edge_start = self.skeleton_offsets[i] as usize;
            let edge_end = self.skeleton_offsets[i + 1] as usize;
            let remap = &local_remaps[i];

            for ei in edge_start..edge_end {
                let mut e = self.skeleton_edges[ei];

                if e.twin_tree_node.is_valid() {
                    let twin_new = old_to_new[e.twin_tree_node.idx()];
                    if twin_new == TreeNodeId(new_i as u64) {
                        continue;
                    }
                    e.twin_tree_node = twin_new;

                    let twin_old_tid = self.skeleton_edges[ei].twin_tree_node.0;
                    let twin_old_eidx = self.skeleton_edges[ei].twin_edge_idx;
                    if let Some(&new_eidx) = old_to_new_edge_idx.get(&(twin_old_tid, twin_old_eidx))
                    {
                        e.twin_edge_idx = new_eidx;
                    }
                }

                let old_src = e.src.0 as usize;
                let old_dst = e.dst.0 as usize;
                if old_src < remap.len() && old_dst < remap.len() {
                    e.src = NodeId(remap[old_src]);
                    e.dst = NodeId(remap[old_dst]);
                }

                let pos = edge_write_pos[new_i] as usize;
                new_skeleton_edges[pos] = e;
                edge_write_pos[new_i] += 1;
            }
        }

        for tn in &mut self.edge_to_tree_node {
            if tn.is_valid() {
                *tn = old_to_new[tn.idx()];
            }
        }

        if self.root.is_valid() {
            self.root = old_to_new[self.root.idx()];
        }

        self.node_types = new_node_types;
        self.node_parents = new_node_parents;
        self.skeleton_offsets = new_skeleton_offsets;
        self.skeleton_edges = new_skeleton_edges;
        self.node_mapping_offsets = new_mapping_offsets;
        self.node_mapping = new_node_mapping;
        self.skeleton_num_nodes = new_skeleton_num_nodes;
        self.children_offsets = new_children_offsets;
        self.children = new_children;

        self.recompute_min_real_per_node();
    }

    pub fn compact(&mut self) {
        let n = self.len();
        if n == 0 {
            return;
        }

        let mut is_alive: Vec<bool> = vec![false; n];
        for i in 0..n {
            let num_edges = self.skeleton_offsets[i + 1] - self.skeleton_offsets[i];
            is_alive[i] = num_edges > 0;
        }

        let mut old_to_new: Vec<TreeNodeId> = vec![TreeNodeId::INVALID; n];
        let mut new_idx = 0u64;
        for i in 0..n {
            if is_alive[i] {
                old_to_new[i] = TreeNodeId(new_idx);
                new_idx += 1;
            }
        }

        if new_idx as usize == n {
            return;
        }

        let new_count = new_idx as usize;

        let mut new_node_types: Vec<SpqrNodeType> = Vec::with_capacity(new_count);
        let mut new_node_parents: Vec<TreeNodeId> = Vec::with_capacity(new_count);
        let mut new_skeleton_num_nodes: Vec<u64> = Vec::with_capacity(new_count);
        let mut new_skeleton_offsets: Vec<u64> = vec![0];
        let mut new_skeleton_edges: Vec<SkeletonEdge> = Vec::new();
        let mut new_node_mapping_offsets: Vec<u64> = vec![0];
        let mut new_node_mapping: Vec<NodeId> = Vec::new();
        let mut new_children_offsets: Vec<u64> = vec![0];
        let mut new_children: Vec<TreeNodeId> = Vec::new();

        for i in 0..n {
            if !is_alive[i] {
                continue;
            }

            new_node_types.push(self.node_types[i]);

            let parent = self.node_parents[i];
            new_node_parents.push(if parent.is_valid() {
                old_to_new[parent.idx()]
            } else {
                TreeNodeId::INVALID
            });

            new_skeleton_num_nodes.push(self.skeleton_num_nodes[i]);

            let edge_start = self.skeleton_offsets[i] as usize;
            let edge_end = self.skeleton_offsets[i + 1] as usize;
            for ei in edge_start..edge_end {
                let mut e = self.skeleton_edges[ei];
                if e.twin_tree_node.is_valid() {
                    e.twin_tree_node = old_to_new[e.twin_tree_node.idx()];
                }
                new_skeleton_edges.push(e);
            }
            new_skeleton_offsets.push(new_skeleton_edges.len() as u64);

            let map_start = self.node_mapping_offsets[i] as usize;
            let map_end = self.node_mapping_offsets[i + 1] as usize;
            new_node_mapping.extend_from_slice(&self.node_mapping[map_start..map_end]);
            new_node_mapping_offsets.push(new_node_mapping.len() as u64);

            let children_start = self.children_offsets[i] as usize;
            let children_end = self.children_offsets[i + 1] as usize;
            for ci in children_start..children_end {
                let child = self.children[ci];
                if child.is_valid() && is_alive[child.idx()] {
                    new_children.push(old_to_new[child.idx()]);
                }
            }
            new_children_offsets.push(new_children.len() as u64);
        }

        for tn in &mut self.edge_to_tree_node {
            if tn.is_valid() && old_to_new[tn.idx()].is_valid() {
                *tn = old_to_new[tn.idx()];
            } else if tn.is_valid() {
                *tn = TreeNodeId::INVALID;
            }
        }

        if self.root.is_valid() {
            self.root = old_to_new[self.root.idx()];
        }

        self.node_types = new_node_types;
        self.node_parents = new_node_parents;
        self.skeleton_num_nodes = new_skeleton_num_nodes;
        self.skeleton_offsets = new_skeleton_offsets;
        self.skeleton_edges = new_skeleton_edges;
        self.node_mapping_offsets = new_node_mapping_offsets;
        self.node_mapping = new_node_mapping;
        self.children_offsets = new_children_offsets;
        self.children = new_children;
    }
}

impl fmt::Display for SpqrTree {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "SPQR Tree ({} nodes):", self.len())?;
        for i in 0..self.len() {
            let t = match self.node_types[i] {
                SpqrNodeType::S => "S",
                SpqrNodeType::P => "P",
                SpqrNodeType::R => "R",
            };
            let num_edges = self.skeleton_offsets[i + 1] - self.skeleton_offsets[i];
            let num_children = self.children_offsets[i + 1] - self.children_offsets[i];
            let map_start = self.node_mapping_offsets[i] as usize;
            let map_end = self.node_mapping_offsets[i + 1] as usize;
            let poles = if map_end - map_start >= 2 {
                (
                    self.node_mapping[map_start],
                    self.node_mapping[map_start + 1],
                )
            } else {
                (NodeId::INVALID, NodeId::INVALID)
            };
            writeln!(
                f,
                "  [{}] {}: {} edges, {} children, poles={:?}",
                i, t, num_edges, num_children, poles
            )?;
        }
        Ok(())
    }
}

