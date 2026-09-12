# mwLZ

A deterministic dictionary block compressor based on prefix trees and quantum byte-packs, engineered for chunk compression while staying entirely inside the CPU's L1 cache.

It is not a general-purpose Zstandard competitor. It trades away ratio at the high end in exchange for something most compressors don't give you: **zero cache misses on the hot path.**

```toml
[dependencies]
mwlz = "0.1.1"
```

---

## Why this exists

Every mainstream LZ-family compressor — LZ4, Snappy, Deflate, Zstd — encodes a match as `(offset, length)`, pointing backward into a sliding history window that can be tens of kilobytes to several megabytes wide. That window has to live somewhere, and for anything but tiny inputs, "somewhere" means L2 or L3 cache, or worse, main memory. Every match lookup is a potential cache miss.

mwLZ removes the window entirely. Instead of remembering *where* a match was, both the compressor and the decompressor build the **same tree, in the same order, at the same time**. A match isn't "the string I saw 340 bytes ago" — it's "node #217 in the tree we're both maintaining." No offset is ever written to the stream, because none is needed: the decompressor already has the same node.

That single decision is what makes everything else possible. The entire dictionary — 4096 nodes, 4 bytes each — is 16 KB. Add the compressor's transition table and you're at 24 KB. That fits inside a single CPU core's L1 data cache (typically 32–48 KB) with room to spare. The decompressor needs even less: 16 KB, just the node table. There is nothing here that has to leave L1 during normal operation.

This is a fundamentally different trade-off than Zstd or LZ4 make, not a smaller version of the same idea. Zstd searches a large window to find the best possible match, and that search is where its ratio advantage comes from. mwLZ has no window to search — it only ever asks "have I seen this exact continuation before, right now, in this tree?" That's a cheaper question with a less thorough answer. You get a compressor that never touches L2, at the cost of not finding matches that a windowed compressor would.

---

## How it works

### The tree

Both sides maintain an identical prefix trie, capped at 4096 nodes. Each node is exactly 4 bytes:

```rust
#[repr(C)]
pub struct Node {
    pub parent_id: u16, // 0xFFFF for the 256 roots
    pub symbol: u8,      // the byte this edge represents
    pub depth: u8,       // exact string length at this node (1..64)
}
```

The 256 roots are the raw byte values 0–255, always present. Every other node is "some root's string, plus more bytes." A `Node ID` is a 12-bit reference into this table — that's the entire vocabulary the wire format needs to express a match.

### `depth` replaces pointer chasing

Most trie-based decoders reconstruct a matched string by walking up through parent pointers, byte by byte. mwLZ stores `depth` — the exact length of the string at each node — precisely so the decoder never has to do that walk:

- **depth = 1** (a root): write the raw byte directly.
- **depth = 2**: the parent is always a root, and a root's symbol equals its own ID. So the whole match is `(symbol << 8) | parent_id` — one 16-bit store, zero memory reads of the parent node.
- **depth > 2**: advance the output pointer by `depth` immediately, then unpack backward directly into the destination buffer. No recursion, no scratch buffer.

### Implicit run extension

Real data — CSV columns, padding, repeated JSON keys — often continues matching past a single trie node in a simple cyclic pattern: `src[i] == src[i - depth]`. Rather than emit a second token for this, mwLZ folds the extension into the *same* token, for free.

The lookup table naturally has 4 spare bits (explained below), so run length up to 7 bytes rides along at no extra cost, with an overflow byte for longer runs. This is where the 2+ GB/s decompression numbers on repetitive data come from — it's not a special mode, it's the same code path taking a shortcut.

### Two dictionary modes

Once the tree fills up (4096 nodes), you choose what happens next:

- **`DictMode::Freeze`** (default) — the tree stops growing and becomes read-only. No more node insertions, no more hash table writes — just fast lookups against an already-hot L1 structure. 1.5–2× faster compression on homogeneous data and structured blocks.
- **`DictMode::Reset`** — the tree resets to the 256 roots and starts over. Use this when a block mixes formats internally (e.g. a JSON header followed by a binary blob), where a frozen dictionary tuned to the first format would just waste bytes on the second.

### Raw fallback

High-entropy input (already-compressed data, ciphertext, random noise) doesn't compress under any scheme. mwLZ detects this and flips a flag to bypass encoding entirely — the block is stored as a raw copy. You never pay a size penalty for feeding it something incompressible.

---

## Wire format

### Header — 8 bytes

```text
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|         Magic ("ML")          |     Flags     |  Version (1)  |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
|                     Uncompressed Size (LE)                    |
+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+-+
```

| Field | Type | Notes |
|---|---|---|
| Magic | `[u8; 2]` | `[0x4D, 0x4C]` (ASCII `"ML"`) |
| Flags | `u8` | bit 0: `1` = Freeze mode, `0` = Reset mode · bit 1: `1` = raw fallback · bits 2–7: reserved |
| Version | `u8` | `1` |
| Uncompressed Size | `u32` LE | original block size in bytes |

### Payload — Quantum Packs

Data is grouped into packs of 8 tokens, each pack prefixed by one control byte:

```text
[ Control Byte: 1B ] [ Token 0 ] [ Token 1 ] ... [ Token 7 ]
```

Each bit in the control byte (LSB first) says what the corresponding token is:

- **`0` — Literal:** 1 raw byte.
- **`1` — Dictionary hit:** a `u16`, little-endian, packed as follows.

A 12-bit `Node ID` only needs 12 of the 16 bits in a `u16`. Rather than leave the other 4 bits as padding, the format spends them on run-length, for free:

```text
 15  14  13  12  11  10   9   8   7   6   5   4   3   2   1   0
+---+---+---+---+---+---+---+---+---+---+---+---+---+---+---+---+
| O |  Run Ext  |                    Node ID                    |
+---+---+---+---+---+---+---+---+---+---+---+---+---+---+---+---+
```

| Bits | Field | Meaning |
|---|---|---|
| 0–11 | Node ID | 0–4095 |
| 12–14 | Run Ext | implicit run extension, 0–7 bytes, at no extra cost |
| 15 | Overflow | if set, one more byte (`extra_len: u8`) follows, extending the run up to 263 bytes total (`7 + 1 + extra_len`) |

If overflow isn't set, the token is exactly 2 bytes and still carries up to 7 bytes of run extension. Overflow costs exactly one extra byte and unlocks the long runs that drive the multi-GB/s decompression numbers on repetitive data.

---

## Usage

### Basic

```rust
use mwlz::{compress, decompress, max_compressed_size, DictMode};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let input = b"Deterministic dictionary block compression with mwLZ";

    let mut comp_buf = vec![0u8; max_compressed_size(input.len())];
    let comp_size = compress(input, &mut comp_buf, DictMode::Freeze)?;

    let mut decomp_buf = vec![0u8; input.len()];
    let decomp_size = decompress(&comp_buf[..comp_size], &mut decomp_buf)?;

    assert_eq!(&decomp_buf[..decomp_size], input);
    Ok(())
}
```

### Reusing state across many chunks

`CompressorState` and `DecompressorState` hold the tree and tables. Allocate one per worker thread and reuse it — this keeps the structure hot in L1 across chunks instead of rebuilding it from cold every call.

```rust
use mwlz::{compress_with_state, decompress_with_state, CompressorState, DecompressorState, DictMode};

fn worker_loop(chunks: &[Vec<u8>]) {
    let mut comp_state = CompressorState::new();
    let mut decomp_state = DecompressorState::new();
    let mut comp_buf = vec![0u8; 1024 * 1024 + 1024];
    let mut decomp_buf = vec![0u8; 1024 * 1024];

    for chunk in chunks {
        let comp_size = compress_with_state(chunk, &mut comp_buf, &mut comp_state, DictMode::Freeze)
            .expect("compression failed");

        let decomp_size = decompress_with_state(&comp_buf[..comp_size], &mut decomp_buf, &mut decomp_state)
            .expect("decompression failed");

        assert_eq!(&decomp_buf[..decomp_size], &chunk[..]);
    }
}
```

### `Vec`-based convenience API (`alloc` or `std`)

```rust
use mwlz::{compress_to_vec, decompress_to_vec, DictMode};

let data = b"payload data to compress";
let compressed = compress_to_vec(data, DictMode::Freeze).unwrap();
let decompressed = decompress_to_vec(&compressed).unwrap();
assert_eq!(decompressed, data);
```

### Reading a block's header without decompressing it

```rust
use mwlz::{dict_mode, is_raw_fallback, uncompressed_size};

fn inspect_block(compressed: &[u8]) {
    println!("unpacks to: {} bytes", uncompressed_size(compressed).unwrap());
    println!("dictionary mode: {:?}", dict_mode(compressed).unwrap());
    println!("raw fallback: {}", is_raw_fallback(compressed).unwrap());
}
```

### `no_std`

```toml
[dependencies]
mwlz = { version = "0.1.1", default-features = false }
```

Without the `alloc` feature, the `compress`/`decompress` and `*_with_state` functions are available; the `_to_vec` convenience wrappers are not.

---

## API reference

### Constants

```rust
pub const FORMAT_VERSION: u8 = 1;
pub const MAGIC: [u8; 2] = [0x4D, 0x4C];
pub const HEADER_SIZE: usize = 8;
pub const MAX_NODES: usize = 4096;
pub const ROOT_COUNT: usize = 256;
pub const MAX_DEPTH: u8 = 64;
pub const FLAG_MODE_FREEZE: u8 = 0x01;
pub const FLAG_RAW_FALLBACK: u8 = 0x02;
```

### Types

```rust
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Node {
    pub parent_id: u16,
    pub symbol: u8,
    pub depth: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DictMode {
    #[default]
    Freeze,
    Reset,
}

impl DictMode {
    pub const fn from_flags(flags: u8) -> Self;
    pub const fn to_flag(self) -> u8;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressError {
    OutputBufferTooSmall,
    InputTooLarge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecompressError {
    InvalidMagic,
    UnsupportedVersion,
    CorruptedHeader,
    OutputBufferTooSmall,
    CorruptedStream,
    InvalidNodeId,
}

pub struct CompressorState { /* ~24 KB: node table + transition table */ }
impl CompressorState {
    pub fn new() -> Self;
    pub fn reset(&mut self);
    pub fn reset_with_seed(&mut self, seed: u32);
}

pub struct DecompressorState { /* 16 KB: node table only */ }
impl DecompressorState {
    pub fn new() -> Self;
    pub fn reset(&mut self);
}
```

### Functions

| Function | Description |
|---|---|
| `compress(src, dst, mode) -> Result<usize, CompressError>` | Compress with a fresh, stack-allocated state. |
| `compress_with_state(src, dst, state, mode) -> Result<usize, CompressError>` | Compress reusing an existing `CompressorState`. |
| `decompress(src, dst) -> Result<usize, DecompressError>` | Decompress with a fresh, stack-allocated state. |
| `decompress_with_state(src, dst, state) -> Result<usize, DecompressError>` | Decompress reusing an existing `DecompressorState`. |
| `max_compressed_size(input_len) -> usize` | Worst-case output buffer size (`HEADER_SIZE + input_len + (input_len / 8 + 1) * 3 + 32`). |
| `uncompressed_size(src) -> Result<usize, DecompressError>` | Read the original size from the header. |
| `dict_mode(src) -> Result<DictMode, DecompressError>` | Read the dictionary mode from the header. |
| `is_raw_fallback(src) -> Result<bool, DecompressError>` | Check whether the block is a raw (uncompressed) fallback. |
| `compress_to_vec(src, mode) -> Result<Vec<u8>, CompressError>` *(alloc)* | Allocating convenience wrapper for `compress`. |
| `decompress_to_vec(src) -> Result<Vec<u8>, DecompressError>` *(alloc)* | Allocating convenience wrapper for `decompress`. |

### Cargo features

| Feature | Effect |
|---|---|
| `default = ["std"]` | Standard library enabled (implies `alloc`). |
| `alloc` | Enables `compress_to_vec` / `decompress_to_vec`. |
| `default-features = false` | Pure `no_std`, no allocations anywhere. |

---

## Benchmarks

`cargo run --release --example bench`, chunk sizes 64 KB – 1 MB.

| Dataset | Mode | Original | Compressed | Ratio | Compress | Decompress |
|---|---|---|---|---|---|---|
| JSON API Logs | Freeze | 64.0 KB | 31.0 KB | 2.06x | 281.0 MB/s | 584.2 MB/s |
| JSON API Logs | Reset | 64.0 KB | 33.4 KB | 1.91x | 153.1 MB/s | 282.5 MB/s |
| Rust Source Code | Freeze | 64.0 KB | 27.9 KB | 2.29x | 439.6 MB/s | 919.0 MB/s |
| .mw Archive Index | Freeze | 256.0 KB | 131.4 KB | 1.95x | 267.4 MB/s | 520.9 MB/s |
| Sensor Telemetry CSV | Freeze | 1.00 MB | 410.1 KB | 2.50x | 257.7 MB/s | 561.3 MB/s |
| Sparse Bitmask | Freeze | 1.00 MB | 10.9 KB | 94.24x | 1214.1 MB/s | 2665.7 MB/s |
| Incompressible (raw fallback) | Freeze | 256.0 KB | 256.0 KB | 1.00x | 137.6 MB/s | 52.8 GB/s |

**Testbed:** Intel Core Ultra 5 125H (14C/18T), 16 GB LPDDR5X-6000, Windows 11, Rust 1.98 nightly, `opt-level = 3` + LTO.

For context: this sits in the same ratio range as fast Zstd levels on structured text, but the ceiling is lower — mwLZ isn't trying to compete with Zstd at high compression levels, and won't. What it offers instead is a cache footprint low enough that these numbers hold steady under real multi-threaded load, where a windowed compressor's effective throughput degrades as cores compete for shared L2/L3.

---

## Why "square wheels"?

Every trade mwLZ makes is a trade against ratio. A real sliding window finds real matches that a 4096-node tree, capped and possibly frozen, simply will not find. Zstd, at a comparable speed setting, will usually compress better. That's not a bug I'm hiding — it's the whole design.

The bet is that for a lot of workloads, *consistent* is worth more than *optimal*. A windowed compressor's throughput depends on what else is fighting for L2/L3 on the machine at that moment. mwLZ's throughput doesn't — its entire state lives in a place nothing else is allowed to evict it from. It's a squarer wheel: less smooth in the best case, but it doesn't care what's happening in the rest of the room.

If you need the best possible ratio, use Zstd. If you need predictable, cache-bounded throughput on small-to-mid blocks and can live with somewhat less compression, that's what this was built for.

---

## License

Licensed under the Apache License, Version 2.0 ([LICENSE](./LICENSE) or <http://www.apache.org/licenses/LICENSE-2.0>).