//! Lossless cold row blocks with explicitly owned decode caches.
//!
//! Rows use the grid storage order: newest first. References returned by `row`
//! remain valid until exclusive access is regained. Cache reclamation therefore
//! requires `&mut self`; it never evicts behind an outstanding shared reference.

use std::collections::{HashMap, VecDeque};
use std::hash::{BuildHasherDefault, Hash, Hasher};
use std::sync::{Arc, OnceLock};

use super::{GridCell, Row};
use crate::term::cell::ResetDiscriminant;

mod lz;
use crate::term::cell::{Cell, CellExtra, Flags, Hyperlink};
use crate::vte::ansi::{Color, NamedColor, Rgb};

const MAX_BLOCK_ROWS: usize = 64;
const BLOCK_CELL_BYTES: usize = 128 * 1024;

/// The codec is a typed storage operation, never a second terminal parser.
#[derive(Debug)]
pub struct RowCodec<T> {
    /// Returns the payload and the narrowest width every row can reflow to
    /// without discarding content, or `None` when any row soft-wraps.
    encode: EncodeRows<T>,
    decode: fn(&[u8]) -> Vec<Row<T>>,
    resize_row: fn(&mut Row<T>, usize),
    /// Decompresses a payload once for single-row access by `reset_row`.
    index: fn(&[u8]) -> EvictIndex<T>,
    /// The row at `row` of an indexed payload, already reset to `template`:
    /// exactly `Row::reset` of the decoded row, without materializing cells
    /// that the reset overwrites. `None` when the encoded width is not
    /// `columns`, so the caller must decode and resize first.
    reset_row: ResetRow<T>,
}

type EncodeRows<T> = fn(&[Row<T>]) -> (Box<[u8]>, Option<usize>);
type ResetRow<T> = fn(&EvictIndex<T>, usize, &T, usize) -> Option<Row<T>>;

/// A decompressed history payload with row offsets, kept for the oldest block
/// while bounded-history scrolling recycles its rows one at a time.
#[derive(Clone, Debug)]
pub struct EvictIndex<T> {
    raw: Vec<u8>,
    rows: Vec<usize>,
    styles: Vec<T>,
}

#[derive(Clone, Debug)]
struct EvictCache<T> {
    bytes: Arc<[u8]>,
    index: EvictIndex<T>,
}

impl<T> Copy for RowCodec<T> {}
impl<T> Clone for RowCodec<T> {
    fn clone(&self) -> Self {
        *self
    }
}

#[derive(Clone, Debug)]
struct Block<T> {
    bytes: Arc<[u8]>,
    start: usize,
    count: usize,
    columns: usize,
    resize_floor: Option<usize>,
    decoded: OnceLock<Vec<Row<T>>>,
    dirty: bool,
}

impl<T> Block<T> {
    fn new(rows: Vec<Row<T>>, codec: RowCodec<T>) -> Self {
        let (bytes, resize_floor) = (codec.encode)(&rows);
        Self {
            bytes: bytes.into(),
            start: 0,
            count: rows.len(),
            columns: rows.first().map_or(0, Row::len),
            resize_floor,
            decoded: OnceLock::new(),
            dirty: false,
        }
    }

    fn rows(&self, codec: RowCodec<T>) -> &[Row<T>] {
        self.decoded.get_or_init(|| self.decode_rows(codec))
    }

    fn decode_rows(&self, codec: RowCodec<T>) -> Vec<Row<T>> {
        let rows = (codec.decode)(&self.bytes);
        if self.start == 0
            && self.count == rows.len()
            && rows.iter().all(|row| row.len() == self.columns)
        {
            return rows;
        }
        rows.into_iter()
            .skip(self.start)
            .take(self.count)
            .map(|mut row| {
                if row.len() != self.columns {
                    (codec.resize_row)(&mut row, self.columns);
                }
                row
            })
            .collect()
    }

    fn row_mut(&mut self, index: usize, codec: RowCodec<T>) -> &mut Row<T> {
        self.rows(codec);
        self.dirty = true;
        &mut self.decoded.get_mut().expect("initialized row block")[index]
    }

    fn release_cache(&mut self, codec: RowCodec<T>) {
        if let Some(rows) = self.decoded.take() {
            if self.dirty {
                let (bytes, resize_floor) = (codec.encode)(&rows);
                self.bytes = bytes.into();
                self.start = 0;
                self.resize_floor = resize_floor;
                self.dirty = false;
            }
        }
    }

    fn into_rows(mut self, codec: RowCodec<T>) -> Vec<Row<T>> {
        self.decoded
            .take()
            .unwrap_or_else(|| self.decode_rows(codec))
    }
}

/// Recent editable rows followed by compressed history and an oldest row tail.
///
/// The tail lets bounded-history scrolling reuse one decoded oldest block. A
/// newly initialized blank row also lives there only until the next rotation.
#[derive(Clone, Debug)]
pub struct CompactRows<T> {
    recent: VecDeque<Row<T>>,
    blocks: VecDeque<Block<T>>,
    oldest: VecDeque<Row<T>>,
    codec: RowCodec<T>,
    visible: usize,
    block_rows: usize,
    len: usize,
    reflowing_recent: bool,
    needs_maintenance: bool,
    last_budget: usize,
    /// Index of the oldest block while its rows are recycled one by one.
    evict_cache: Option<Box<EvictCache<T>>>,
}

fn block_rows<T>(columns: usize) -> usize {
    let bytes = columns
        .saturating_mul(std::mem::size_of::<T>())
        .saturating_add(std::mem::size_of::<Row<T>>())
        .max(1);
    let count = (BLOCK_CELL_BYTES / bytes).clamp(1, MAX_BLOCK_ROWS);
    // Power-of-two blocks can split into smaller ranges without re-encoding.
    1usize << count.ilog2()
}

impl<T> CompactRows<T> {
    pub fn new(rows: Vec<Row<T>>, visible: usize, columns: usize, codec: RowCodec<T>) -> Self {
        assert!(rows.len() >= visible);
        let mut storage = Self {
            len: rows.len(),
            recent: rows.into(),
            blocks: VecDeque::new(),
            oldest: VecDeque::new(),
            codec,
            visible,
            block_rows: block_rows::<T>(columns),
            reflowing_recent: false,
            needs_maintenance: true,
            last_budget: 0,
            evict_cache: None,
        };
        storage.seal_recent();
        storage.recent.shrink_to_fit();
        storage
    }

    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline(always)]
    pub fn row(&self, index: usize) -> &Row<T> {
        // The visible screen and newest history are the parser's hot path.
        if index < self.recent.len() {
            return &self.recent[index];
        }
        self.cold_row(index)
    }

    /// Rows held in blocks. Only the oldest block can be partial: bounded
    /// history drops its rows one at a time without decoding the others.
    fn cold_rows(&self) -> usize {
        self.blocks.back().map_or(0, |last| {
            (self.blocks.len() - 1) * self.block_rows + last.count
        })
    }

    #[inline(never)]
    fn cold_row(&self, mut index: usize) -> &Row<T> {
        assert!(index < self.len);
        index -= self.recent.len();
        let cold_rows = self.cold_rows();
        if index < cold_rows {
            let block = &self.blocks[index / self.block_rows];
            return &block.rows(self.codec)[index % self.block_rows];
        }
        &self.oldest[index - cold_rows]
    }

    #[inline(always)]
    pub fn row_mut(&mut self, index: usize) -> &mut Row<T> {
        // Editing a visible row needs no history maintenance. Rotation can
        // briefly leave fewer recent rows than visible ones.
        if index < self.visible && index < self.recent.len() {
            return &mut self.recent[index];
        }
        self.cold_row_mut(index)
    }

    #[inline(never)]
    fn cold_row_mut(&mut self, mut index: usize) -> &mut Row<T> {
        assert!(index < self.len);
        if index >= self.visible {
            self.needs_maintenance = true;
        }
        if index < self.recent.len() {
            return &mut self.recent[index];
        }
        index -= self.recent.len();
        let cold_rows = self.cold_rows();
        if index < cold_rows {
            let block = &mut self.blocks[index / self.block_rows];
            return block.row_mut(index % self.block_rows, self.codec);
        }
        &mut self.oldest[index - cold_rows]
    }

    pub fn initialize(&mut self, count: usize, columns: usize)
    where
        T: Default,
    {
        self.needs_maintenance |= count != 0;
        self.oldest.extend((0..count).map(|_| Row::new(columns)));
        self.len += count;
    }

    /// Rotate left for positive counts, matching the dense storage's zero shift.
    pub fn rotate(&mut self, count: isize) {
        assert!(count.unsigned_abs() <= self.len);
        self.needs_maintenance |= count != 0;
        if count < 0 {
            for _ in 0..count.unsigned_abs() {
                let row = self.pop_oldest().expect("nonempty rotated storage");
                self.recent.push_front(row);
            }
        } else {
            for _ in 0..count as usize {
                self.ensure_recent();
                let row = self.recent.pop_front().expect("nonempty rotated storage");
                self.oldest.push_back(row);
            }
        }
        self.seal_recent();
    }

    pub fn swap(&mut self, a: usize, b: usize) {
        self.needs_maintenance |= a >= self.visible || b >= self.visible;
        if a == b {
            return;
        }
        if a < self.recent.len() && b < self.recent.len() {
            self.recent.swap(a, b);
        } else {
            // No layout-dependent pointer swaps: ownership moves through safe
            // replacements even for rows decoded from different cold blocks.
            let first = std::mem::replace(self.row_mut(a), Row::from_vec(Vec::new(), 0));
            let second = std::mem::replace(self.row_mut(b), first);
            *self.row_mut(a) = second;
        }
    }

    pub fn truncate(&mut self, len: usize) {
        assert!(len <= self.len);
        self.needs_maintenance |= self.len != len;
        while self.len > len {
            if self.oldest.is_empty() {
                if let Some(block) = self.blocks.back() {
                    if self.len - len >= block.count {
                        self.len -= self.blocks.pop_back().expect("last block").count;
                        continue;
                    }
                }
            }
            self.drop_oldest();
            self.len -= 1;
        }
    }

    pub fn set_visible(&mut self, visible: usize) {
        assert!(visible <= self.len);
        self.needs_maintenance |= self.visible != visible;
        self.visible = visible;
        while self.recent.len() < visible && !self.blocks.is_empty() {
            let block = self.blocks.pop_front().expect("first block");
            self.recent.extend(block.into_rows(self.codec));
            if self.blocks.is_empty() {
                self.evict_cache = None;
            }
        }
        while self.recent.len() < visible {
            self.recent
                .push_back(self.oldest.pop_front().expect("visible row"));
        }
        self.seal_recent();
    }

    /// Release all history read caches at a caller's exclusive borrow boundary.
    pub fn release_read_cache(&mut self) {
        for block in &mut self.blocks {
            block.release_cache(self.codec);
        }
    }

    /// Stored bytes, excluding the visible cells and temporary decode work.
    pub fn history_storage_bytes(&self) -> usize {
        let row_bytes = |row: &Row<T>| row.len().saturating_mul(std::mem::size_of::<T>());
        // Split ranges are adjacent and share one immutable allocation. Dirty
        // edits can separate siblings; counting those allocations again is
        // conservative and requires no allocation on the idle/cursor path.
        let mut previous = std::ptr::null();
        let mut payload = 0;
        for block in &self.blocks {
            let ptr = block.bytes.as_ptr();
            if ptr != previous {
                payload += block.bytes.len() + 2 * std::mem::size_of::<usize>();
            }
            previous = ptr;
        }
        let evict_cache = self.evict_cache.as_ref().map_or(0, |cache| {
            std::mem::size_of::<EvictCache<T>>()
                + cache.index.raw.capacity()
                + cache.index.rows.capacity() * std::mem::size_of::<usize>()
                + cache.index.styles.capacity() * std::mem::size_of::<T>()
        });
        payload
            + evict_cache
            + self.blocks.capacity() * std::mem::size_of::<Block<T>>()
            + self.recent.capacity() * std::mem::size_of::<Row<T>>()
            + self.oldest.capacity() * std::mem::size_of::<Row<T>>()
            + self
                .recent
                .iter()
                .skip(self.visible)
                .map(row_bytes)
                .sum::<usize>()
            + self.oldest.iter().map(row_bytes).sum::<usize>()
    }

    /// Discard only the oldest history when the retained representation is full.
    #[inline]
    pub fn bound_history_bytes(&mut self, budget: usize) {
        if !self.needs_maintenance && self.last_budget == budget {
            return;
        }
        self.release_read_cache();
        while self.len > self.visible && self.history_storage_bytes() > budget {
            if self.oldest.is_empty() && self.blocks.len() > 1 {
                self.len -= self.blocks.pop_back().expect("oldest block").count;
                self.evict_cache = None;
            } else {
                self.drop_oldest();
                self.len -= 1;
            }
        }
        // Do not retain a large block-index allocation after erasing history.
        if self.blocks.capacity() > self.blocks.len().saturating_mul(2).saturating_add(64) {
            self.blocks.shrink_to_fit();
        }
        self.needs_maintenance = false;
        self.last_budget = budget;
    }

    pub fn into_rows(self) -> Vec<Row<T>> {
        let mut rows = Vec::with_capacity(self.len);
        rows.extend(self.recent);
        for block in self.blocks {
            rows.extend(block.into_rows(self.codec));
        }
        rows.extend(self.oldest);
        rows
    }

    pub fn drain_rows(&mut self) -> Vec<Row<T>> {
        if self.reflowing_recent {
            let rows: Vec<_> = self.recent.drain(..).collect();
            self.len -= rows.len();
            return rows;
        }
        let mut rows = Vec::with_capacity(self.len);
        rows.extend(self.recent.drain(..));
        for block in self.blocks.drain(..) {
            rows.extend(block.into_rows(self.codec));
        }
        rows.extend(self.oldest.drain(..));
        self.evict_cache = None;
        self.len = 0;
        rows
    }

    pub fn replace_rows(&mut self, rows: Vec<Row<T>>, visible: usize, columns: usize) {
        self.needs_maintenance = true;
        if self.reflowing_recent {
            self.reflowing_recent = false;
            self.len += rows.len();
            self.recent = rows.into();
            self.visible = visible;
            self.seal_recent();
            return;
        }
        *self = Self::new(rows, visible, columns, self.codec);
    }

    /// Preserve cold hard lines that cannot participate in this reflow. Their
    /// storage range stays compressed; only requested read rows gain padding.
    pub fn prepare_reflow(&mut self, columns: usize) -> usize {
        self.release_read_cache();
        if !self.oldest.is_empty()
            || self.blocks.is_empty()
            || self
                .blocks
                .back()
                .is_some_and(|last| last.count != self.block_rows)
            || self
                .blocks
                .iter()
                .any(|block| block.resize_floor.is_none_or(|floor| floor > columns))
        {
            return self.len;
        }
        self.coalesce_shared_ranges(block_rows::<T>(columns));
        let next_rows = self.block_rows.min(block_rows::<T>(columns));
        if next_rows != self.block_rows {
            let mut blocks =
                VecDeque::with_capacity(self.blocks.len() * self.block_rows / next_rows);
            for block in self.blocks.drain(..) {
                for offset in (0..block.count).step_by(next_rows) {
                    blocks.push_back(Block {
                        bytes: block.bytes.clone(),
                        start: block.start + offset,
                        count: next_rows,
                        columns,
                        resize_floor: block.resize_floor,
                        decoded: OnceLock::new(),
                        dirty: false,
                    });
                }
            }
            self.blocks = blocks;
            self.block_rows = next_rows;
        } else {
            for block in &mut self.blocks {
                block.columns = columns;
            }
        }
        self.reflowing_recent = true;
        self.recent.len()
    }

    /// Undo a wide resize's index splitting without decoding or recompressing
    /// immutable payloads. Groups align from the oldest end; a small unmatched
    /// newest prefix becomes editable. Independently edited ranges remain split.
    fn coalesce_shared_ranges(&mut self, desired: usize) {
        let mut next = desired;
        while next > self.block_rows {
            let group = next / self.block_rows;
            let prefix = self.blocks.len() % group;
            if self.blocks.len() < group {
                next /= 2;
                continue;
            }
            let mergeable = (prefix..self.blocks.len()).step_by(group).all(|start| {
                let first = &self.blocks[start];
                (1..group).all(|offset| {
                    let block = &self.blocks[start + offset];
                    Arc::ptr_eq(&first.bytes, &block.bytes)
                        && block.start == first.start + offset * self.block_rows
                })
            });
            if !mergeable {
                next /= 2;
                continue;
            }
            for _ in 0..prefix {
                self.recent.extend(
                    self.blocks
                        .pop_front()
                        .expect("range prefix")
                        .into_rows(self.codec),
                );
            }
            let mut merged = VecDeque::with_capacity(self.blocks.len() / group);
            while let Some(mut first) = self.blocks.pop_front() {
                for _ in 1..group {
                    let block = self.blocks.pop_front().expect("complete range group");
                    first.count += block.count;
                    first.resize_floor = first
                        .resize_floor
                        .zip(block.resize_floor)
                        .map(|(a, b)| a.max(b));
                }
                merged.push_back(first);
            }
            self.blocks = merged;
            self.block_rows = next;
            return;
        }
    }

    fn pop_oldest(&mut self) -> Option<Row<T>> {
        if let Some(row) = self.oldest.pop_back() {
            return Some(row);
        }
        if let Some(block) = self.blocks.pop_back() {
            self.evict_cache = None;
            self.oldest = block.into_rows(self.codec).into();
            return self.oldest.pop_back();
        }
        self.recent.pop_back()
    }

    /// Discard the oldest row without decoding any other row.
    fn drop_oldest(&mut self) {
        if self.oldest.pop_back().is_some() {
            return;
        }
        if let Some(block) = self.blocks.back_mut() {
            if let Some(rows) = block.decoded.get_mut() {
                rows.pop();
            }
            block.count -= 1;
            if block.count == 0 {
                self.blocks.pop_back();
                self.evict_cache = None;
            }
            return;
        }
        self.recent.pop_back();
    }

    /// The oldest row after `Row::reset(template)`. A cold row is produced
    /// from its payload directly, so recycling history never decodes a block.
    fn pop_oldest_reset<D>(&mut self, template: &T) -> Row<T>
    where
        T: ResetDiscriminant<D> + GridCell + Default,
        D: PartialEq,
    {
        let fast = self.oldest.is_empty()
            && self
                .blocks
                .back()
                .is_some_and(|block| block.decoded.get().is_none());
        if fast {
            let block = self.blocks.back().expect("oldest block");
            let cached = self
                .evict_cache
                .as_ref()
                .is_some_and(|cache| Arc::ptr_eq(&cache.bytes, &block.bytes));
            if !cached {
                self.evict_cache = Some(Box::new(EvictCache {
                    bytes: block.bytes.clone(),
                    index: (self.codec.index)(&block.bytes),
                }));
            }
            let cache = self.evict_cache.as_ref().expect("oldest block index");
            let row = (self.codec.reset_row)(
                &cache.index,
                block.start + block.count - 1,
                template,
                block.columns,
            );
            if let Some(row) = row {
                self.drop_oldest();
                return row;
            }
        }
        let mut row = self.pop_oldest().expect("nonempty rotated storage");
        row.reset(template);
        row
    }

    /// `rotate(-count)` followed by `Row::reset(template)` of every rotated
    /// row, for callers that clear those rows before reading them.
    pub fn rotate_reset<D>(&mut self, count: usize, template: &T)
    where
        T: ResetDiscriminant<D> + GridCell + Default,
        D: PartialEq,
    {
        assert!(count <= self.len);
        self.needs_maintenance |= count != 0;
        for _ in 0..count {
            let row = self.pop_oldest_reset(template);
            self.recent.push_front(row);
        }
        self.seal_recent();
    }

    fn ensure_recent(&mut self) {
        if !self.recent.is_empty() {
            return;
        }
        if let Some(block) = self.blocks.pop_front() {
            self.recent.extend(block.into_rows(self.codec));
            if self.blocks.is_empty() {
                self.evict_cache = None;
            }
        } else {
            self.recent.append(&mut self.oldest);
        }
    }

    fn seal_recent(&mut self) {
        while self.recent.len() >= self.visible + self.block_rows {
            let rows = self.recent.split_off(self.recent.len() - self.block_rows);
            self.blocks.push_front(Block::new(rows.into(), self.codec));
        }
    }
}

/// Lossless cell/style encoding for process-local history. This is not a
/// persistent format and must not be used to decode external protocol data.
pub fn cell_codec() -> RowCodec<Cell> {
    RowCodec {
        encode: encode_cells,
        decode: decode_cells,
        resize_row: resize_cell_row,
        index: index_cells,
        reset_row: reset_cell_row,
    }
}

fn resize_cell_row(row: &mut Row<Cell>, columns: usize) {
    if columns > row.len() {
        row.grow(columns);
    } else {
        assert!(
            row.shrink(columns).is_none(),
            "cold resize must not discard content"
        );
    }
}

// Block layout. Integers are LEB128 varints unless stated otherwise.
//
//   styles_len, style_count, style*   (a style is a cell without its character;
//                                      `styles_len` covers count and styles)
//   row*: occupancy, columns, stored, text_len, UTF-8 text of the `stored`
//         characters, run_count, (style_id, cells)*
//
// `stored` excludes the row's suffix of cells equal to `Cell::default()`;
// decoding restores that suffix, so rows keep their exact length and cells.

/// Minimal FxHash-style hasher. Keys are process-local and never adversarial
/// in a way that matters here: collisions only cost a comparison.
#[derive(Default)]
struct FxHasher(u64);

impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut word = [0u8; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            self.write_u64(u64::from_le_bytes(word));
        }
    }

    #[inline]
    fn write_u8(&mut self, value: u8) {
        self.write_u64(u64::from(value));
    }

    #[inline]
    fn write_u32(&mut self, value: u32) {
        self.write_u64(u64::from(value));
    }

    #[inline]
    fn write_u64(&mut self, value: u64) {
        self.0 = (self.0.rotate_left(5) ^ value).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }

    #[inline]
    fn write_usize(&mut self, value: usize) {
        self.write_u64(value as u64);
    }

    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
}

type FxBuild = BuildHasherDefault<FxHasher>;

/// Hash key for a style with no extra storage: both colors and the flags.
#[inline]
fn color_key(value: Color) -> u32 {
    match value {
        Color::Named(value) => value as u32,
        Color::Spec(value) => {
            1 << 24 | u32::from(value.r) << 16 | u32::from(value.g) << 8 | u32::from(value.b)
        }
        Color::Indexed(value) => 2 << 24 | u32::from(value),
    }
}

#[inline]
fn plain_style_key(cell: &Cell) -> u128 {
    u128::from(color_key(cell.fg))
        | u128::from(color_key(cell.bg)) << 32
        | u128::from(cell.flags.bits()) << 64
}

#[derive(Clone, Eq, PartialEq)]
struct Style(Cell);

impl Hash for Style {
    fn hash<H: Hasher>(&self, state: &mut H) {
        plain_style_key(&self.0).hash(state);
        if let Some(extra) = &self.0.extra {
            extra.zerowidth.hash(state);
            extra.underline_color.map(color_key).hash(state);
            extra.hyperlink.hash(state);
        }
    }
}

#[inline]
fn put_varint(out: &mut Vec<u8>, mut value: usize) {
    while value >= 0x80 {
        out.push(value as u8 | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    put_varint(out, bytes.len());
    out.extend_from_slice(bytes);
}

fn put_color(out: &mut Vec<u8>, value: Color) {
    match value {
        Color::Named(value) => {
            out.push(0);
            put_varint(out, value as usize);
        }
        Color::Spec(value) => out.extend_from_slice(&[1, value.r, value.g, value.b]),
        Color::Indexed(value) => out.extend_from_slice(&[2, value]),
    }
}

fn put_style(out: &mut Vec<u8>, style: &Cell) {
    put_color(out, style.fg);
    put_color(out, style.bg);
    out.extend_from_slice(&style.flags.bits().to_le_bytes());
    let Some(extra) = &style.extra else {
        out.push(0);
        return;
    };
    out.push(1);
    put_varint(out, extra.zerowidth.len());
    for character in &extra.zerowidth {
        put_varint(out, *character as usize);
    }
    match extra.underline_color {
        Some(color) => {
            out.push(1);
            put_color(out, color);
        }
        None => out.push(0),
    }
    match &extra.hyperlink {
        Some(link) => {
            out.push(1);
            put_bytes(out, link.id().as_bytes());
            put_bytes(out, link.uri().as_bytes());
        }
        None => out.push(0),
    }
}

/// Style table for one block. Most blocks use a handful of styles, so plain
/// styles are found by a short linear scan; a larger table switches to a map.
#[derive(Default)]
struct StyleTable {
    styles: Vec<Cell>,
    plain: Vec<(u128, u32)>,
    plain_map: HashMap<u128, u32, FxBuild>,
    extra: HashMap<Style, u32, FxBuild>,
}

impl StyleTable {
    const LINEAR_PLAIN: usize = 16;

    fn push(&mut self, cell: &Cell) -> u32 {
        let mut style = cell.clone();
        style.c = ' ';
        self.styles.push(style);
        (self.styles.len() - 1) as u32
    }

    fn id(&mut self, cell: &Cell) -> u32 {
        if cell.extra.is_some() {
            let mut style = Style(cell.clone());
            style.0.c = ' ';
            if let Some(&id) = self.extra.get(&style) {
                return id;
            }
            let id = self.push(cell);
            self.extra.insert(style, id);
            return id;
        }
        let key = plain_style_key(cell);
        if self.plain.len() < Self::LINEAR_PLAIN {
            if let Some(&(_, id)) = self.plain.iter().find(|entry| entry.0 == key) {
                return id;
            }
            let id = self.push(cell);
            self.plain.push((key, id));
            if self.plain.len() == Self::LINEAR_PLAIN {
                self.plain_map.extend(self.plain.iter().copied());
            }
            return id;
        }
        if let Some(&id) = self.plain_map.get(&key) {
            return id;
        }
        let id = self.push(cell);
        self.plain_map.insert(key, id);
        id
    }
}

/// Equal to `Cell::default()`, without the branchy derived color comparisons.
#[inline]
fn is_default(cell: &Cell, default_key: u128) -> bool {
    cell.c == ' ' && cell.extra.is_none() && plain_style_key(cell) == default_key
}

fn encode_cells(rows: &[Row<Cell>]) -> (Box<[u8]>, Option<usize>) {
    let default_key = plain_style_key(&Cell::default());
    let mut table = StyleTable::default();
    let mut body = Vec::with_capacity(rows.iter().map(|row| row.len() + 8).sum());
    let mut runs: Vec<(u32, u32)> = Vec::new();
    let mut text = Vec::new();
    let mut floor = Some(1);
    for row in rows {
        let cells = &row[..];
        let stored = cells
            .iter()
            .rposition(|cell| !is_default(cell, default_key))
            .map_or(0, |i| i + 1);
        floor = floor.map(|floor: usize| floor.max(stored));
        runs.clear();
        text.clear();
        text.reserve(stored);
        let mut start = 0;
        while start < stored {
            let cell = &cells[start];
            // A soft wrap anywhere prevents cold reflow. Cells in one run
            // share flags, so checking each run's first cell suffices.
            if cell.flags.contains(Flags::WRAPLINE) {
                floor = None;
            }
            let id = table.id(cell);
            let mut end = start + 1;
            if cell.extra.is_none() {
                let key = plain_style_key(cell);
                while end < stored
                    && cells[end].extra.is_none()
                    && plain_style_key(&cells[end]) == key
                {
                    end += 1;
                }
            }
            let count = (end - start) as u32;
            match runs.last_mut() {
                Some(run) if run.0 == id => run.1 += count,
                _ => runs.push((id, count)),
            }
            start = end;
        }
        let mut ascii = true;
        text.extend(cells[..stored].iter().map(|cell| {
            ascii &= cell.c.is_ascii();
            cell.c as u8
        }));
        if !ascii {
            text.clear();
            for cell in &cells[..stored] {
                let mut buffer = [0; 4];
                text.extend_from_slice(cell.c.encode_utf8(&mut buffer).as_bytes());
            }
        }
        put_varint(&mut body, row.occ);
        put_varint(&mut body, cells.len());
        put_varint(&mut body, stored);
        put_bytes(&mut body, &text);
        put_varint(&mut body, runs.len());
        for &(id, count) in &runs {
            put_varint(&mut body, id as usize);
            put_varint(&mut body, count as usize);
        }
    }
    let mut styles = Vec::with_capacity(8 + table.styles.len() * 8);
    put_varint(&mut styles, table.styles.len());
    for style in &table.styles {
        put_style(&mut styles, style);
    }
    let mut raw = Vec::with_capacity(body.len() + styles.len() + 4);
    put_varint(&mut raw, styles.len());
    raw.extend_from_slice(&styles);
    raw.extend_from_slice(&body);
    (lz::compress(&raw).into_boxed_slice(), floor)
}

struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    #[inline]
    fn byte(&mut self) -> u8 {
        let byte = self.bytes[self.position];
        self.position += 1;
        byte
    }

    #[inline]
    fn varint(&mut self) -> usize {
        let mut value = 0usize;
        let mut shift = 0;
        loop {
            let byte = self.byte();
            value |= usize::from(byte & 0x7f) << shift;
            if byte < 0x80 {
                return value;
            }
            shift += 7;
        }
    }

    fn slice(&mut self, len: usize) -> &'a [u8] {
        let slice = &self.bytes[self.position..self.position + len];
        self.position += len;
        slice
    }

    fn string(&mut self) -> String {
        let len = self.varint();
        String::from_utf8(self.slice(len).to_vec()).expect("internally encoded UTF-8")
    }

    fn color(&mut self) -> Color {
        match self.byte() {
            0 => {
                let value = self.varint();
                Color::Named(
                    NAMED_COLORS
                        .iter()
                        .copied()
                        .find(|named| *named as usize == value)
                        .expect("internally encoded named color"),
                )
            }
            1 => {
                let [r, g, b] = self.slice(3) else {
                    unreachable!()
                };
                Color::Spec(Rgb {
                    r: *r,
                    g: *g,
                    b: *b,
                })
            }
            2 => Color::Indexed(self.byte()),
            tag => panic!("internally encoded color tag {tag}"),
        }
    }

    fn style(&mut self) -> Cell {
        let fg = self.color();
        let bg = self.color();
        let flags = Flags::from_bits_retain(u16::from_le_bytes([self.byte(), self.byte()]));
        let extra = (self.byte() == 1).then(|| {
            let zerowidth = (0..self.varint())
                .map(|_| char::from_u32(self.varint() as u32).expect("internally encoded scalar"))
                .collect();
            let underline_color = (self.byte() == 1).then(|| self.color());
            let hyperlink = (self.byte() == 1).then(|| {
                let id = self.string();
                Hyperlink::new(Some(id), self.string())
            });
            Arc::new(CellExtra {
                zerowidth,
                underline_color,
                hyperlink,
            })
        });
        Cell {
            c: ' ',
            fg,
            bg,
            flags,
            extra,
        }
    }
}

const NAMED_COLORS: [NamedColor; 29] = [
    NamedColor::Black,
    NamedColor::Red,
    NamedColor::Green,
    NamedColor::Yellow,
    NamedColor::Blue,
    NamedColor::Magenta,
    NamedColor::Cyan,
    NamedColor::White,
    NamedColor::BrightBlack,
    NamedColor::BrightRed,
    NamedColor::BrightGreen,
    NamedColor::BrightYellow,
    NamedColor::BrightBlue,
    NamedColor::BrightMagenta,
    NamedColor::BrightCyan,
    NamedColor::BrightWhite,
    NamedColor::Foreground,
    NamedColor::Background,
    NamedColor::Cursor,
    NamedColor::DimBlack,
    NamedColor::DimRed,
    NamedColor::DimGreen,
    NamedColor::DimYellow,
    NamedColor::DimBlue,
    NamedColor::DimMagenta,
    NamedColor::DimCyan,
    NamedColor::DimWhite,
    NamedColor::BrightForeground,
    NamedColor::DimForeground,
];

fn read_styles(reader: &mut Reader<'_>) -> Vec<Cell> {
    let _styles_len = reader.varint();
    (0..reader.varint()).map(|_| reader.style()).collect()
}

fn index_cells(bytes: &[u8]) -> EvictIndex<Cell> {
    let raw = lz::decompress(bytes);
    let mut reader = Reader {
        bytes: &raw,
        position: 0,
    };
    let styles = read_styles(&mut reader);
    let mut rows = Vec::new();
    while reader.position < raw.len() {
        rows.push(reader.position);
        let _occ = reader.varint();
        let _len = reader.varint();
        let _stored = reader.varint();
        let text_len = reader.varint();
        reader.position += text_len;
        for _ in 0..2 * reader.varint() {
            reader.varint();
        }
    }
    EvictIndex { raw, rows, styles }
}

/// `Row::reset(template)` of the encoded row, with `Cell`'s background as the
/// reset discriminant. Cells below the row's occupancy are replaced by the
/// reset, so only the cells above it are decoded.
fn reset_cell_row(
    index: &EvictIndex<Cell>,
    row: usize,
    template: &Cell,
    columns: usize,
) -> Option<Row<Cell>> {
    let mut reader = Reader {
        bytes: &index.raw,
        position: index.rows[row],
    };
    let occ = reader.varint();
    let len = reader.varint();
    let stored = reader.varint();
    if len != columns || len == 0 {
        return None;
    }
    let text_len = reader.varint();
    let text = reader.slice(text_len);
    let run_count = reader.varint();
    let runs_start = reader.position;
    let mut last_style = None;
    for _ in 0..run_count {
        last_style = Some(reader.varint());
        reader.varint();
    }
    let default = Cell::default();
    let last_bg = match last_style {
        Some(style) if stored == len => index.styles[style].bg,
        _ => default.bg,
    };
    let mut blank = default.clone();
    blank.reset(template);
    // A differing last cell makes the reset clear the whole row.
    let reset = if last_bg == template.bg { occ } else { len };
    let mut cells = Vec::with_capacity(len);
    cells.extend(std::iter::repeat_n(blank, reset));
    if reset < stored {
        // Decode the stored cells the reset keeps.
        let mut reader = Reader {
            bytes: &index.raw,
            position: runs_start,
        };
        let mut characters = std::str::from_utf8(text)
            .expect("internally encoded text")
            .chars();
        let mut column = 0;
        for _ in 0..run_count {
            let style = &index.styles[reader.varint()];
            for _ in 0..reader.varint() {
                let c = characters.next().expect("one scalar per encoded cell");
                if column >= reset {
                    let mut cell = style.clone();
                    cell.c = c;
                    cells.push(cell);
                }
                column += 1;
            }
        }
    }
    cells.extend(std::iter::repeat_n(default, len - cells.len()));
    Some(Row::from_vec(cells, 0))
}

fn decode_cells(bytes: &[u8]) -> Vec<Row<Cell>> {
    let raw = lz::decompress(bytes);
    let mut reader = Reader {
        bytes: &raw,
        position: 0,
    };
    let styles = read_styles(&mut reader);
    let mut rows = Vec::new();
    while reader.position < raw.len() {
        let occ = reader.varint();
        let len = reader.varint();
        let stored = reader.varint();
        assert!(occ <= len && stored <= len);
        let text_len = reader.varint();
        let text = reader.slice(text_len);
        let mut cells = Vec::with_capacity(len);
        if text.len() == stored {
            // ASCII: one byte per cell.
            let mut offset = 0;
            for _ in 0..reader.varint() {
                let style = &styles[reader.varint()];
                let count = reader.varint();
                let bytes = &text[offset..offset + count];
                offset += count;
                if style.extra.is_none() {
                    let (fg, bg, flags) = (style.fg, style.bg, style.flags);
                    cells.extend(bytes.iter().map(|&byte| Cell {
                        c: char::from(byte),
                        fg,
                        bg,
                        flags,
                        extra: None,
                    }));
                } else {
                    push_run(
                        &mut cells,
                        style,
                        count,
                        bytes.iter().map(|&byte| char::from(byte)),
                    );
                }
            }
        } else {
            let mut text = std::str::from_utf8(text)
                .expect("internally encoded text")
                .chars();
            for _ in 0..reader.varint() {
                let style = &styles[reader.varint()];
                let count = reader.varint();
                push_run(&mut cells, style, count, &mut text);
            }
            assert!(text.next().is_none());
        }
        assert_eq!(cells.len(), stored, "internally encoded runs");
        // Cloning one default is measurably cheaper than `resize_with`.
        cells.extend(std::iter::repeat_n(Cell::default(), len - stored));
        rows.push(Row::from_vec(cells, occ));
    }
    rows
}

fn push_run(cells: &mut Vec<Cell>, style: &Cell, count: usize, text: impl Iterator<Item = char>) {
    let before = cells.len();
    cells.extend(text.take(count).map(|c| {
        let mut cell = style.clone();
        cell.c = c;
        cell
    }));
    assert_eq!(cells.len() - before, count, "one scalar per encoded cell");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Term;
    use crate::event::VoidListener;
    use crate::grid::Dimensions;
    use crate::index::Column;
    use crate::index::Line;
    use crate::term::Config;
    use crate::vte::ansi::Processor;

    struct Size;
    impl Dimensions for Size {
        fn total_lines(&self) -> usize {
            24
        }
        fn screen_lines(&self) -> usize {
            24
        }
        fn columns(&self) -> usize {
            80
        }
    }

    fn parser_rows() -> Vec<Row<Cell>> {
        let mut term = Term::new(
            Config {
                scrolling_history: 10000,
                ..Config::default()
            },
            &Size,
            VoidListener,
        );
        let mut parser: Processor = Processor::new();
        for n in 0..300 {
            let text = format!(
                "\x1b]133;A\x07\x1b[38;2;{};{};{}m\x1b]8;id={n};https://example.invalid/{n}\x07{n:06} 界 e\u{301}\x1b]8;;\x07\x1b[0m\r\n",
                n % 256,
                n * 3 % 256,
                n * 7 % 256
            );
            parser.advance(&mut term, text.as_bytes());
        }
        let grid = term.grid();
        (0..grid.total_lines())
            .map(|i| grid[Line(23 - i as i32)].clone())
            .collect()
    }

    fn assert_rows(actual: &CompactRows<Cell>, expected: &[Row<Cell>]) {
        assert_eq!(actual.len(), expected.len());
        for (index, row) in expected.iter().enumerate() {
            // Row equality intentionally omits occupancy; compare it explicitly
            // because reset/reflow behavior depends on that private metadata.
            assert_eq!(actual.row(index), row, "row {index}");
            assert_eq!(actual.row(index).occ, row.occ, "occupancy {index}");
        }
    }

    /// The previous resize-floor definition, kept as the oracle.
    fn reference_resize_floor(rows: &[Row<Cell>]) -> Option<usize> {
        let default = Cell::default();
        let mut floor = 1;
        for row in rows {
            for column in 0..row.len() {
                let cell = &row[Column(column)];
                if cell.flags.contains(Flags::WRAPLINE) {
                    return None;
                }
                if cell != &default {
                    floor = floor.max(column + 1);
                }
            }
        }
        Some(floor)
    }

    fn assert_codec_round_trip(rows: &[Row<Cell>]) {
        let (bytes, floor) = encode_cells(rows);
        assert_eq!(floor, reference_resize_floor(rows));
        let decoded = decode_cells(&bytes);
        assert_eq!(decoded.len(), rows.len());
        for (index, (actual, expected)) in decoded.iter().zip(rows).enumerate() {
            assert_eq!(actual, expected, "row {index}");
            assert_eq!(actual.occ, expected.occ, "occupancy {index}");
            assert_eq!(actual.len(), expected.len(), "width {index}");
        }
    }

    #[test]
    fn every_cell_attribute_round_trips_through_the_codec() {
        use crate::vte::ansi::Rgb;
        let colors: Vec<Color> = NAMED_COLORS
            .iter()
            .map(|&named| Color::Named(named))
            .chain((0..=255).step_by(37).map(Color::Indexed))
            .chain([
                Color::Spec(Rgb { r: 0, g: 0, b: 0 }),
                Color::Spec(Rgb {
                    r: 255,
                    g: 128,
                    b: 1,
                }),
            ])
            .collect();
        let characters = ['a', ' ', '\0', '~', 'é', '界', '🦀', '\u{7f}', '\t'];
        let mut seed = 0x9e37_79b9u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for width in [1, 2, 7, 80, 333] {
            let mut rows = Vec::new();
            for row_index in 0..40 {
                let mut row = Row::<Cell>::new(width);
                let content = next() as usize % (width + 1);
                for column in 0..content {
                    let random = next();
                    let cell = &mut row[Column(column)];
                    cell.c = characters[random as usize % characters.len()];
                    if random % 3 == 0 {
                        cell.fg = colors[(random >> 8) as usize % colors.len()];
                        cell.bg = colors[(random >> 16) as usize % colors.len()];
                        // Includes WRAPLINE for some rows.
                        cell.flags = Flags::from_bits_retain((random >> 24) as u16);
                    }
                    match (random >> 40) % 11 {
                        0 => cell.push_zerowidth('\u{301}'),
                        1 => cell.set_underline_color(Some(
                            colors[(random >> 44) as usize % colors.len()],
                        )),
                        2 => cell.set_hyperlink(Some(Hyperlink::new(
                            Some("id"),
                            format!("https://x/{}", random % 5),
                        ))),
                        3 => cell.extra = Some(Arc::new(CellExtra::default())),
                        4 => {
                            cell.push_zerowidth('\u{308}');
                            cell.push_zerowidth('\u{20e3}');
                            cell.set_hyperlink(Some(Hyperlink::new(
                                None::<String>,
                                String::from("u"),
                            )));
                        }
                        _ => {}
                    }
                }
                // Occupancy is independent metadata, including beyond content.
                row.occ = if row_index % 4 == 0 {
                    width
                } else {
                    next() as usize % (width + 1)
                };
                rows.push(row);
            }
            assert_codec_round_trip(&rows);
            // Hard lines only, so the floor is a width, not `None`.
            for row in &mut rows {
                for cell in &mut row[..] {
                    cell.flags.remove(Flags::WRAPLINE);
                }
            }
            assert_codec_round_trip(&rows);
        }
        // A block with far more styles than the linear table holds.
        let mut row = Row::<Cell>::new(600);
        for (column, cell) in row[..].iter_mut().enumerate() {
            cell.c = 'x';
            cell.fg = Color::Spec(Rgb {
                r: column as u8,
                g: (column >> 8) as u8,
                b: 3,
            });
            if column % 5 == 0 {
                cell.push_zerowidth(char::from_u32(0x300 + column as u32 % 64).unwrap());
            }
        }
        assert_codec_round_trip(&[row.clone(), Row::new(600), row]);
        assert_codec_round_trip(&[]);
    }

    #[test]
    fn recycled_rows_equal_decoded_rows_after_reset() {
        use crate::vte::ansi::{NamedColor, Rgb};
        let backgrounds = [
            Color::Named(NamedColor::Background),
            Color::Named(NamedColor::Red),
            Color::Indexed(4),
            Color::Spec(Rgb { r: 1, g: 2, b: 3 }),
        ];
        let mut seed = 0x1234_5678_9abc_def1u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for width in [1, 3, 80] {
            let mut rows = Vec::new();
            for _ in 0..64 {
                let mut row = Row::<Cell>::new(width);
                let content = next() as usize % (width + 1);
                for column in 0..content {
                    let random = next();
                    let cell = &mut row[Column(column)];
                    cell.c = ['a', ' ', '界', 'é'][random as usize % 4];
                    cell.bg = backgrounds[(random >> 8) as usize % backgrounds.len()];
                    if random % 7 == 0 {
                        cell.push_zerowidth('\u{301}');
                    }
                }
                // Includes occupancy below, at and above the stored content.
                row.occ = next() as usize % (width + 1);
                rows.push(row);
            }
            let (bytes, _) = encode_cells(&rows);
            let index = index_cells(&bytes);
            assert_eq!(index.rows.len(), rows.len());
            for (position, row) in rows.iter().enumerate() {
                for background in backgrounds {
                    let template = Cell {
                        bg: background,
                        ..Cell::default()
                    };
                    let mut expected = row.clone();
                    expected.reset(&template);
                    let actual =
                        reset_cell_row(&index, position, &template, width).expect("matching width");
                    assert_eq!(actual, expected, "row {position} template {background:?}");
                    assert_eq!(actual.occ, expected.occ);
                    assert_eq!(actual.len(), expected.len());
                }
            }
            assert!(reset_cell_row(&index, 0, &Cell::default(), width + 1).is_none());
        }
    }

    /// Full bounded history recycles its oldest rows on every scroll. Dense
    /// storage is the oracle, including colored-background resets, scroll
    /// regions, scrollback reads and edits, erase and alternate screen.
    /// Geometry stays fixed: after resizing a full history, dense and compact
    /// storage already diverge on the base revision, a pre-existing difference
    /// outside this test.
    #[test]
    fn full_history_recycling_matches_dense_storage() {
        let config = Config {
            scrolling_history: 70,
            ..Config::default()
        };
        let size = crate::term::test::TermSize::new(12, 5);
        let mut dense = Term::new(config.clone(), &size, VoidListener);
        let mut compact = Term::new(config, &size, VoidListener);
        compact.grid_mut().enable_compact_history();
        let mut dense_parser: Processor = Processor::new();
        let mut compact_parser: Processor = Processor::new();
        let actions = [
            "\x1b[41m",
            "\x1b[48;5;20m",
            "\x1b[48;2;9;9;9m",
            "\x1b[0m",
            "\x1b[2;4r",
            "\x1b[1;5r",
            "\x1b[r",
            "\x1b[2S",
            "\x1b[7S",
            "\x1b[1T",
            "\x1bD",
            "\x1bM",
            "\x1b[2L",
            "\x1b[1M",
            "\x1b[2J",
            "\x1b[3J",
            "\x1b[K",
            "\x1b[44m\x1b[K\x1b[0m",
            "\x1b[45mcolored tail\x1b[K\r\n\x1b[0m",
            "\x1b[42m\x1b[2K\r\n\r\n",
            "\x1b[?1049h",
            "\x1b[?1049l",
            "\x1b[5;1H",
            "\x1b[H",
            "界界界",
            "e\u{301}",
            "\x1b]8;id=k;https://example.invalid\x07link\x1b]8;;\x07",
        ];
        let mut seed = 0x0bad_5eed_1234_5678u64;
        for step in 0..4000 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let text = match seed % 5 {
                0 | 1 => format!("{step} line {}\r\n", "x".repeat((seed >> 8) as usize % 20)),
                2 => format!("{}\n", step % 7),
                3 => actions[(seed >> 16) as usize % actions.len()].to_string(),
                _ => "\r\n".repeat(1 + (seed >> 20) as usize % 8),
            };
            dense_parser.advance(&mut dense, text.as_bytes());
            compact_parser.advance(&mut compact, text.as_bytes());
            if seed % 13 == 0 {
                // Edit a scrollback row, as selection and reflow can.
                let history = compact.grid().history_size() as i32;
                if history > 0 {
                    let line = Line(-(1 + (seed >> 40) as i32 % history));
                    compact.grid_mut()[line][Column(0)].c = '#';
                    dense.grid_mut()[line][Column(0)].c = '#';
                }
            }
            if seed % 3 == 0 || step % 50 == 0 {
                let (a, e) = (compact.grid(), dense.grid());
                assert_eq!(a.history_size(), e.history_size(), "history at {step}");
                assert_eq!(a.cursor, e.cursor, "cursor at {step}");
                for line in -(e.history_size() as i32)..e.screen_lines() as i32 {
                    assert_eq!(a[Line(line)], e[Line(line)], "line {line} at {step}");
                    assert_eq!(
                        a[Line(line)].occ,
                        e[Line(line)].occ,
                        "occupancy {line} at {step}"
                    );
                }
            }
            compact.grid_mut().release_history_read_cache();
        }
    }

    #[test]
    fn history_compression_round_trips_arbitrary_bytes() {
        let mut seed = 0x51_7c_c1_b7u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let mut inputs: Vec<Vec<u8>> = vec![
            Vec::new(),
            vec![7],
            vec![0; 3],
            vec![0; 70_000],
            b"abcabcabcabcabcabcabcabcabc".repeat(900),
        ];
        // Long matches that end on a mismatch rather than at the input end.
        for run in [3, 4, 15, 19, 270, 300, 301, 600, 5000] {
            let block: Vec<u8> = (0..run).map(|_| next() as u8).collect();
            let mut input = block.clone();
            input.push(b'X');
            input.extend_from_slice(&block);
            input.push(b'Y');
            input.extend_from_slice(&block[..run / 2]);
            input.extend_from_slice(b"tail bytes");
            inputs.push(input);
        }
        for length in [5, 8, 9, 13, 64, 255, 256, 270, 271, 4096, 70_000] {
            // Random, low-entropy and repeated structure at each length.
            inputs.push((0..length).map(|_| next() as u8).collect());
            inputs.push(
                (0..length)
                    .map(|_| b"ab \x1b"[next() as usize % 4])
                    .collect(),
            );
            let phrase: Vec<u8> = (0..1 + next() as usize % 300)
                .map(|_| next() as u8)
                .collect();
            inputs.push(phrase.iter().copied().cycle().take(length).collect());
        }
        for input in inputs {
            let packed = lz::compress(&input);
            assert_eq!(lz::decompress(&packed), input, "length {}", input.len());
        }
    }

    #[test]
    fn actual_parser_cells_and_occupancy_round_trip_losslessly() {
        let expected = parser_rows();
        let mut actual = CompactRows::new(expected.clone(), 24, 80, cell_codec());
        assert!(!actual.blocks.is_empty());
        assert_rows(&actual, &expected);
        assert!(
            actual
                .blocks
                .iter()
                .all(|block| block.decoded.get().is_some())
        );
        actual.release_read_cache();
        assert!(
            actual
                .blocks
                .iter()
                .all(|block| block.decoded.get().is_none())
        );
        assert_eq!(actual.into_rows(), expected);
    }

    #[test]
    fn random_storage_operations_match_an_uncompressed_sequence() {
        let mut expected = parser_rows();
        let mut actual = CompactRows::new(expected.clone(), 24, 80, cell_codec());
        let mut seed = 0x59fa_8261_u64;
        for step in 0..350 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let n = (seed >> 32) as usize;
            match n % 6 {
                0 => {
                    let count = 1 + n % 7;
                    actual.initialize(count, 80);
                    expected.extend((0..count).map(|_| Row::new(80)));
                }
                1 => {
                    let count = 1 + n % 25;
                    actual.rotate(-(count as isize));
                    expected.rotate_right(count);
                }
                2 => {
                    let count = 1 + n % 25;
                    actual.rotate(count as isize);
                    expected.rotate_left(count);
                }
                3 => {
                    let a = n % expected.len();
                    let b = (n / 7) % expected.len();
                    actual.swap(a, b);
                    expected.swap(a, b);
                }
                4 => {
                    let index = n % expected.len();
                    let column = Column(step % 80);
                    actual.row_mut(index)[column].c = '雪';
                    expected[index][column].c = '雪';
                }
                _ => {
                    let len = expected.len().saturating_sub(n % 5).max(30);
                    actual.truncate(len);
                    expected.truncate(len);
                }
            }
            actual.set_visible(24);
            assert_rows(&actual, &expected);
            actual.release_read_cache();
        }
        assert_eq!(actual.into_rows(), expected);
    }

    #[test]
    fn frozen_history_can_be_edited_then_recompressed() {
        let mut expected = parser_rows();
        let mut actual = CompactRows::new(expected.clone(), 24, 80, cell_codec());
        let index = actual.recent.len() + 10;
        actual.row_mut(index)[Column(12)].push_zerowidth('\u{308}');
        expected[index][Column(12)].push_zerowidth('\u{308}');
        actual.release_read_cache();
        assert!(
            actual
                .blocks
                .iter()
                .all(|block| block.decoded.get().is_none())
        );
        assert_rows(&actual, &expected);
    }

    #[test]
    fn stored_budget_evicts_oldest_rows_and_preserves_the_visible_grid() {
        let expected = parser_rows();
        let mut actual = CompactRows::new(expected.clone(), 24, 80, cell_codec());
        let original_bytes = actual.history_storage_bytes();
        // An unchanged grid must still obey a smaller subsequent allowance.
        actual.bound_history_bytes(original_bytes);
        let budget = original_bytes / 2;
        actual.bound_history_bytes(budget);
        assert!(actual.history_storage_bytes() <= budget);
        assert!(actual.len() < expected.len());
        assert!(actual.len() >= 24);
        assert_rows(&actual, &expected[..actual.len()]);
        actual.release_read_cache();
        assert!(actual.history_storage_bytes() <= budget);
    }

    #[test]
    fn high_entropy_styles_obey_storage_budget_without_touching_visible_cells() {
        use crate::vte::ansi::Rgb;
        let mut random = 17u32;
        let mut rows: Vec<Row<Cell>> = (0..1024).map(|_| Row::new(80)).collect();
        for row in &mut rows {
            for x in 0..80 {
                random ^= random << 13;
                random ^= random >> 17;
                random ^= random << 5;
                let cell = &mut row[Column(x)];
                cell.c = char::from_u32(33 + random % 90).unwrap();
                cell.fg = Color::Spec(Rgb {
                    r: random as u8,
                    g: (random >> 8) as u8,
                    b: (random >> 16) as u8,
                });
            }
        }
        let expected = rows.clone();
        let mut actual = CompactRows::new(rows, 24, 80, cell_codec());
        let original = actual.history_storage_bytes();
        assert!(original > 256 * 1024);
        actual.bound_history_bytes(256 * 1024);
        assert!(actual.history_storage_bytes() <= 256 * 1024);
        assert!(actual.len() < expected.len());
        assert!(actual.len() >= 24);
        assert_rows(&actual, &expected[..actual.len()]);
        actual.release_read_cache();
        assert!(actual.history_storage_bytes() <= 256 * 1024);
    }

    #[test]
    fn hard_line_resize_keeps_cold_payloads_compressed_and_splits_read_ranges() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static DECODED: AtomicUsize = AtomicUsize::new(0);
        fn decode(bytes: &[u8]) -> Vec<Row<Cell>> {
            DECODED.fetch_add(1, Ordering::Relaxed);
            decode_cells(bytes)
        }
        let original = parser_rows();
        let mut codec = cell_codec();
        codec.decode = decode;
        let mut storage = CompactRows::new(original.clone(), 24, 80, codec);
        let original_ptr = storage.blocks.back().unwrap().bytes.as_ptr();
        let count = storage.prepare_reflow(320);
        assert!(count < storage.len());
        let mut recent = storage.drain_rows();
        for row in &mut recent {
            resize_cell_row(row, 320);
        }
        storage.replace_rows(recent, 24, 320);
        assert_eq!(
            DECODED.load(Ordering::Relaxed),
            0,
            "resize decoded cold history"
        );
        assert_eq!(storage.block_rows, 16);
        assert_eq!(storage.blocks.back().unwrap().bytes.as_ptr(), original_ptr);
        for (index, row) in original.iter().cloned().enumerate() {
            let mut expected = row;
            resize_cell_row(&mut expected, 320);
            assert_eq!(storage.row(index), &expected);
        }
        storage.release_read_cache();
        assert!(DECODED.load(Ordering::Relaxed) > 0);
        for columns in [4096, 80] {
            storage.prepare_reflow(columns);
            let mut recent = storage.drain_rows();
            for row in &mut recent {
                resize_cell_row(row, columns);
            }
            storage.replace_rows(recent, 24, columns);
        }
        assert_eq!(storage.block_rows, block_rows::<Cell>(80));
        assert_eq!(storage.blocks.back().unwrap().bytes.as_ptr(), original_ptr);
        assert_rows(&storage, &original);
    }

    #[test]
    fn hard_line_parser_resize_matches_dense_across_cold_block_splits() {
        struct Geometry(usize, usize);
        impl Dimensions for Geometry {
            fn total_lines(&self) -> usize {
                self.1
            }
            fn screen_lines(&self) -> usize {
                self.1
            }
            fn columns(&self) -> usize {
                self.0
            }
        }
        let config = Config {
            scrolling_history: 1000,
            ..Config::default()
        };
        let mut dense = Term::new(config.clone(), &Size, VoidListener);
        let mut compact = Term::new(config, &Size, VoidListener);
        compact.grid_mut().enable_compact_history();
        let mut dense_parser: Processor = Processor::new();
        let mut compact_parser: Processor = Processor::new();
        for index in 0..500 {
            let line = format!("\x1b[31m{index:06} 界 e\u{301}\x1b[0m\r\n");
            dense_parser.advance(&mut dense, line.as_bytes());
            compact_parser.advance(&mut compact, line.as_bytes());
        }
        for (columns, lines) in [
            (320, 50),
            (120, 40),
            (139, 49),
            (4096, 24),
            (80, 24),
            (9, 24),
        ] {
            dense.resize(Geometry(columns, lines));
            compact.resize(Geometry(columns, lines));
            assert_eq!(compact.grid().cursor, dense.grid().cursor);
            assert_eq!(compact.grid().history_size(), dense.grid().history_size());
            for line in -(dense.grid().history_size() as i32)..lines as i32 {
                assert_eq!(
                    compact.grid()[Line(line)],
                    dense.grid()[Line(line)],
                    "row {line} at {columns}x{lines}"
                );
                compact.grid_mut().release_history_read_cache();
            }
        }
    }

    #[test]
    fn full_history_rotation_reuses_rows_without_losing_order() {
        let mut expected = parser_rows();
        let mut actual = CompactRows::new(expected.clone(), 24, 80, cell_codec());
        for n in 0..300 {
            actual.rotate(-1);
            expected.rotate_right(1);
            actual.row_mut(0)[Column(0)].c = char::from_u32(0x100 + n).unwrap();
            expected[0][Column(0)].c = char::from_u32(0x100 + n).unwrap();
        }
        assert_rows(&actual, &expected);
        actual.truncate(24);
        expected.truncate(24);
        assert_rows(&actual, &expected);
        assert!(actual.blocks.is_empty());
    }

    #[test]
    fn wide_rows_reduce_the_number_of_rows_per_block() {
        let rows = (0..40).map(|_| Row::new(1000)).collect();
        let actual = CompactRows::new(rows, 24, 1000, cell_codec());
        assert!(actual.block_rows < 6);
        assert!(actual.recent.len() < 30);
    }

    #[test]
    fn compact_parser_matches_dense_history_through_reflow_and_screen_changes() {
        struct Geometry(usize, usize);
        impl Dimensions for Geometry {
            fn total_lines(&self) -> usize {
                self.1
            }
            fn screen_lines(&self) -> usize {
                self.1
            }
            fn columns(&self) -> usize {
                self.0
            }
        }
        fn same(actual: &mut Term<VoidListener>, expected: &Term<VoidListener>, step: &str) {
            assert_eq!(actual.mode(), expected.mode(), "mode after {step}");
            let a = actual.grid();
            let e = expected.grid();
            assert_eq!(
                a.total_lines(),
                e.total_lines(),
                "history length after {step}"
            );
            assert_eq!(a.cursor, e.cursor, "cursor after {step}");
            assert_eq!(a.saved_cursor, e.saved_cursor, "saved cursor after {step}");
            for line in -(e.history_size() as i32)..e.screen_lines() as i32 {
                assert_eq!(a[Line(line)], e[Line(line)], "line {line} after {step}");
            }
            actual.grid_mut().release_history_read_cache();
        }
        let config = Config {
            scrolling_history: 1000,
            ..Config::default()
        };
        let mut dense = Term::new(config.clone(), &Size, VoidListener);
        let mut compact = Term::new(config, &Size, VoidListener);
        compact.grid_mut().enable_compact_history();
        let mut dense_parser: Processor = Processor::new();
        let mut compact_parser: Processor = Processor::new();
        let mut actions = Vec::new();
        for n in 0..400 {
            actions.push(format!(
                "{n:06} 界 e\u{301} {}\r\n",
                "wide wrapped words ".repeat(n % 9)
            ));
        }
        actions.extend(
            [
                "\x1b[4;18r\x1b[17;1H\x1b[3S\x1b[2T\x1b[r",
                "\x1b[3;7H\x1b7\x1b[4L\x1b[2M\x1b8",
                "\x1b[?1049h alternate 界\x1b[?1049l",
                "\x1b[?47h again\x1b[?47l",
                "\x1b[?1047h\x1b[2J\x1b[?1047l",
            ]
            .into_iter()
            .map(str::to_string),
        );
        for (index, action) in actions.iter().enumerate() {
            // Split every escape and UTF-8 sequence across parser feed boundaries.
            for chunk in action.as_bytes().chunks(3) {
                dense_parser.advance(&mut dense, chunk);
                compact_parser.advance(&mut compact, chunk);
            }
            if index % 31 == 0 || index >= 400 {
                same(&mut compact, &dense, &format!("output {index}"));
            }
        }
        for (columns, lines) in [(41, 24), (120, 60), (2, 4), (80, 24), (320, 50), (80, 24)] {
            dense.resize(Geometry(columns, lines));
            compact.resize(Geometry(columns, lines));
            same(&mut compact, &dense, &format!("resize {columns}x{lines}"));
        }
        for action in ["\x1b[3J", "new history\r\n".repeat(150).as_str(), "\x1bc"] {
            dense_parser.advance(&mut dense, action.as_bytes());
            compact_parser.advance(&mut compact, action.as_bytes());
            same(&mut compact, &dense, "erase or reset");
        }
    }
}
