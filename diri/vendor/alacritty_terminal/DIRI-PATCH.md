# Diri terminal extensions

Pinned alacritty_terminal 0.26.0 (Apache-2.0). The terminal metadata extension adds a
PROMPT_START cell flag and Handler::mark_prompt implementation, paired with the
OSC 133 A dispatch in vendored VTE. Markers follow the existing grid erase,
scroll, and reflow lifecycle and are ignored on the alternate screen.

This keeps prompt navigation in the authoritative parser, including synchronized
updates, instead of adding a second escape-sequence parser or guessing boundaries
from terminal text. No new runtime dependency. The source participates in the
Remote Helper Build ID. Revisit this small patch when updating the parser.

## Deferred alternate-screen allocation

The pristine alternate grid is represented by `None` until the first screen
switch. It is created at the current dimensions, then uses the original cursor,
clear, swap, resize, and reset operations. Once created, its allocation is retained
for reuse. The active primary grid and history remain eager and authoritative.

This removes one unused 80×24 grid (46,848 requested heap bytes) from terminals
that have never entered a full-screen program. The tradeoff is allocating that
grid at first entry. There is no wire or checkpoint format change. Parser source
already participates in the Remote Helper Build ID; live Holders keep their binary.

The resource gate and actual-parser screen-switch/resize/reset regressions live
in `diri-terminal-state`; run `cargo bench -p diri-terminal-state --bench
terminal_parity -- --empty-gate` from the workspace for the allocation gate.

## Spare history-row allocation

Storage still grows in batches and reuses cleared rows. Its batch and shrink
cache limit now scales with row byte size: at most 1,000 rows and about 64 KiB
of newly allocated rows, with a minimum of one row. A request for more live rows
is always satisfied. The reserve includes cell and row-descriptor sizes; existing
Vec capacity and rows retained through reflow are not a total-memory guarantee.

Previously, the first history row eagerly allocated 1,000 rows, even at wide
terminal dimensions. The change preserves ring indexing, row identity, history
limits, resize/reflow and serialized formats. It trades smaller growth batches
for lower retained heap. The short-history resource gate covers both sides of
the first-scroll boundary; upstream storage tests cover indexing and rotation.

The opt-in enhanced keyboard parser also keeps direct CSI = mode changes in
the active stack entry. Queries, push/pop, and alternate-screen transitions
therefore observe the same flags. Its bounded overflow evicts keyboard entries
without touching window titles. Diri does not enable negotiation by default.

## Compact history codec

Cold history blocks (the `compact-history` feature used by the Engine and the
Remote Helper) are encoded as a hand-written binary layout and compressed with
a small LZ77 block codec in `grid/compact/lz.rs`. They were previously
`serde_json` values compressed with DEFLATE (`flate2`), which dominated the
cost of streaming output: every line that scrolls into history is encoded once.

Per block: a style table (cells without their character), then per row the
occupancy, width, stored-cell count, the UTF-8 text of the stored cells and
style runs. The suffix of cells equal to `Cell::default()` is implied by the
width. The resize floor is computed during the same pass. The codec is
lossless, process-local and only ever decodes its own output; corruption still
panics, as before. `flate2` and `serde_json` are no longer dependencies of the
feature; no new dependency is added.

Tradeoff: LZ without entropy coding stores more bytes than DEFLATE. On 10,000
rows of 160 columns this measured 54.5 vs 53.0 bytes per row for `git log -p`
output and 41.8 vs 35.4 for a synthetic colored build log. The 4 MiB history
budget is unchanged and measured the same way, so output that is limited by
that budget (not by the 10,000-row limit) can retain fewer rows than before.

## Recycled history rows

When history is full, scrolling moves the oldest row to the bottom of the
screen and resets it. Compact storage used to decode the whole oldest block to
recycle one row. `Grid::scroll_up` now calls `Storage::rotate_reset` when every
rotated row lies inside the region it resets afterwards. Compact storage then
builds each recycled row in its reset state directly from the encoded row:
`Row::reset` only keeps cells above the row's occupancy, and only when the
last cell's background matches the template. Other rows of the block stay
encoded; the oldest block may be partial. Dropping single rows for the byte
budget or truncation likewise no longer decodes. The grid's own reset of those
rows is then a no-op. Dense storage keeps plain rotation.

A decoded block used to count at full cell size toward the history budget
until its rows were recycled. The partially consumed block now counts its
compressed bytes plus one decompressed index.

## Recycling from recipe bits and spare rows

Encoding a block also records, per row, whether `Row::reset` of that row
yields only template cells for every template: the row has the block width,
is not empty, and every cell at or beyond its occupancy is `Cell::default()`
(then a differing template background resets every cell, and an equal one
means the default background, whose kept cells already are reset cells).
Nearly every output line qualifies. Recycling such an oldest row drops it
without decompressing or indexing its block; rows that do not qualify, or
blocks at another width after a resize, take the indexed path above. The 64
recipe bits fit in `Block` without changing its counted size (`start` is now
a `u32`), so the history budget retains exactly the same rows.

The rows a seal has just encoded are kept as spare allocations for the rows
that recycling hands back, so a scroll no longer allocates and frees a row.
A spare row whose reset would keep non-template cells is marked fully
occupied first; the others reset only the cells they wrote. Spares are
released at every `bound_history_bytes` (each `HeadlessScreen` settle),
before stored bytes are measured, so they neither count toward nor outlive
the history budget, and an idle terminal retains none.

`bound_history_bytes` runs after every read that scrolled. It no longer visits
every block: the payload term of `history_storage_bytes` is kept current by
the block pushes and pops at the ends that streaming output uses and is
recomputed from scratch after any other block change (debug builds assert it
equals the recomputation), and read caches are only released when a history
row was read or edited since the last release.

## Line content sources

`Term::damage_content_sources` reports, for full damage, where each screen
line's cells were at the last `reset_damage`: scrolling (`scroll_up_relative`
and `scroll_down_relative`, which cover linefeed, reverse index, SU/SD and
IL/DL) moves the sources with the lines and marks uncovered lines changed,
instead of forgetting them. Any line damage, and every other full damage,
still marks lines changed or the sources unknown. `input` leaves its cells to
cursor damage, so a scroll marks the cursor line first, and full damage marks
the previous and current cursor lines like the partial damage query does.
Clearing a wide character's leading spacer on the line above the cursor was
not damaged at all; it now marks that line changed (renderer damage is
unchanged). `diri-terminal-state` uses the sources to keep moved rows'
fingerprints; rendering is unaffected.

## Printable ASCII input

`Term::input_ascii` (the vendored VTE's `Handler::input_ascii`) writes a run of
printable ASCII a row segment at a time. It is exactly `input` per character:
the segment stops at a wide character or spacer, and wrapping, insert mode and
a pending wrap fall back to `input`. Charset mapping, prompt marks, template
colors, flags and hyperlinks, occupancy and the final cursor/wrap state match.
Visible-row indexing also has an inlined fast path.

Tests: `ascii_runs_match_per_character_input` (randomized against `input`),
`recycled_rows_equal_decoded_rows_after_reset` (also checks the recipe bits
and spare-row resets), `streaming_full_history_recycles_without_decoding`,
`recycle_bits_keep_the_counted_block_size`,
`random_storage_operations_match_an_uncompressed_sequence` (now with
recycling and byte bounds), `scrolling_moves_content_sources`,
`full_history_recycling_matches_dense_storage`,
`every_cell_attribute_round_trips_through_the_codec` and
`history_compression_round_trips_arbitrary_bytes`. There is no CI job for this
excluded crate; run them from this directory with the vendored VTE:

```sh
cargo --config 'patch.crates-io.vte.path="../vte"' test --features compact-history
```

(Remove the generated `Cargo.lock` afterwards.) While testing this patch, dense
and compact storage were found to diverge after resizing a full history; that
difference exists on the base revision too and is not changed here.
