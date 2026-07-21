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
