//! Implementation of the mwLZ compressor.

use crate::types::*;

/// Reusable workspace state for the mwLZ compressor (~24.5 KB), sized to stay
/// resident in L1D. See crate-level docs for why this matters.
pub struct CompressorState {
    /// Prefix tree nodes table (16 KB).
    pub nodes: [Node; MAX_NODES],
    /// Direct-mapped transition hash table: (parent, symbol) -> node_id (8 KB).
    pub hash_table: [u16; HASH_SIZE],
    /// Current number of nodes in the dictionary (256..4096).
    pub node_count: u16,
    /// Previously emitted node ID for LZW phrase linking.
    pub prev_node: u16,
    /// Flag indicating whether the dictionary is frozen in read-only mode.
    pub is_frozen: bool,
    /// Active mwhash seed for direct-mapped transition table.
    pub seed: u32,
}

impl CompressorState {
    /// Sentinel value representing an empty hash table slot.
    pub const EMPTY_HASH: u16 = 0xFFFF;
    /// Default seed constant (Golden Ratio / mwhash family).
    pub const SEED_DEFAULT: u32 = 0x9E37_79B9;

    /// Create a compressor state with placeholder root nodes. Call `reset()` or
    /// `reset_with_seed()` before use — `new()` alone does not populate valid
    /// root nodes (see `Node::ROOT`).
    pub const fn new() -> Self {
        Self {
            nodes: [Node::ROOT; MAX_NODES],
            hash_table: [Self::EMPTY_HASH; HASH_SIZE],
            node_count: ROOT_COUNT as u16,
            prev_node: ROOT_PARENT_ID,
            is_frozen: false,
            seed: Self::SEED_DEFAULT,
        }
    }

    /// Reset state for compressing a new chunk with a specific seed.
    pub fn reset_with_seed(&mut self, seed: u32) {
        self.node_count = ROOT_COUNT as u16;
        self.prev_node = ROOT_PARENT_ID;
        self.is_frozen = false;
        self.seed = seed;
        self.hash_table.fill(Self::EMPTY_HASH);

        let mut i = 0;
        while i < ROOT_COUNT {
            self.nodes[i] = Node::new(ROOT_PARENT_ID, i as u8, 1);
            i += 1;
        }
    }

    /// Reset state using default seed.
    pub fn reset(&mut self) {
        self.reset_with_seed(Self::SEED_DEFAULT);
    }

    /// Compute direct-mapped hash table index for transition (parent, symbol).
    /// Multiply-xor-shift mix, no branches.
    #[inline(always)]
    pub fn hash(&self, parent_id: u16, symbol: u8) -> usize {
        let key = ((parent_id as u32) << 8) | (symbol as u32);
        let h = (key ^ self.seed).wrapping_mul(Self::SEED_DEFAULT);
        (h ^ (h >> 16)) as usize & HASH_MASK
    }

    /// Try inserting a new transition into the dictionary.
    ///
    /// Returns `true` if a dictionary reset occurred.
    #[inline(always)]
    pub fn try_insert(&mut self, parent_id: u16, symbol: u8, mode: DictMode) -> bool {
        if self.is_frozen || self.node_count >= MAX_NODES as u16 {
            return false;
        }

        let parent = self.nodes[parent_id as usize];
        if parent.depth >= MAX_DEPTH {
            return false;
        }

        let new_id = self.node_count;
        self.nodes[new_id as usize] = Node::new(parent_id, symbol, parent.depth + 1);

        let h = self.hash(parent_id, symbol);
        self.hash_table[h] = new_id;

        self.node_count += 1;

        if self.node_count == MAX_NODES as u16 {
            match mode {
                DictMode::Reset => {
                    self.node_count = ROOT_COUNT as u16;
                    self.hash_table.fill(Self::EMPTY_HASH);
                    self.prev_node = ROOT_PARENT_ID;
                    return true;
                }
                DictMode::Freeze => {
                    self.is_frozen = true;
                }
            }
        }
        false
    }
}

impl Default for CompressorState {
    fn default() -> Self {
        let mut state = Self::new();
        state.reset();
        state
    }
}

/// Writes quantum packs directly into `dst`, byte by byte — no intermediate
/// buffer. Caller owns flushing via `finish()`.
struct PackWriter<'a> {
    dst: &'a mut [u8],
    dst_pos: usize,
    ctrl_pos: usize,
    control_byte: u8,
    count: u8,
}

impl<'a> PackWriter<'a> {
    #[inline(always)]
    fn new(dst: &'a mut [u8], start_pos: usize) -> Result<Self, CompressError> {
        if start_pos >= dst.len() {
            return Err(CompressError::OutputBufferTooSmall);
        }
        Ok(Self {
            dst,
            dst_pos: start_pos + 1, // reserve start_pos for the control byte
            ctrl_pos: start_pos,
            control_byte: 0,
            count: 0,
        })
    }

    #[inline(always)]
    fn push_literal(&mut self, byte: u8) -> Result<(), CompressError> {
        if self.dst_pos >= self.dst.len() {
            return Err(CompressError::OutputBufferTooSmall);
        }
        self.dst[self.dst_pos] = byte;
        self.dst_pos += 1;
        self.count += 1;

        if self.count == 8 {
            self.flush_pack()?;
        }
        Ok(())
    }

    #[inline(always)]
    fn push_hit(&mut self, node_id: u16, run_ext: usize) -> Result<(), CompressError> {
        self.control_byte |= 1 << self.count;

        let (run_bits, overflow, extra_byte) = if run_ext <= 7 {
            (run_ext as u16, false, None)
        } else {
            let extra = core::cmp::min(run_ext - 8, 255) as u8;
            (7u16, true, Some(extra))
        };

        let token: u16 = (node_id & 0x0FFF)
            | (run_bits << 12)
            | (if overflow { 0x8000 } else { 0 });

        let needed = if extra_byte.is_some() { 3 } else { 2 };
        if self.dst_pos + needed > self.dst.len() {
            return Err(CompressError::OutputBufferTooSmall);
        }

        unsafe {
            core::ptr::write_unaligned(
                self.dst.as_mut_ptr().add(self.dst_pos) as *mut u16,
                token.to_le(),
            );
        }
        self.dst_pos += 2;

        if let Some(extra) = extra_byte {
            self.dst[self.dst_pos] = extra;
            self.dst_pos += 1;
        }

        self.count += 1;

        if self.count == 8 {
            self.flush_pack()?;
        }
        Ok(())
    }

    #[inline(always)]
    fn flush_pack(&mut self) -> Result<(), CompressError> {
        self.dst[self.ctrl_pos] = self.control_byte;
        self.control_byte = 0;
        self.count = 0;
        self.ctrl_pos = self.dst_pos;
        if self.dst_pos < self.dst.len() {
            self.dst_pos += 1; // reserve byte for next control byte
        }
        Ok(())
    }

    #[inline(always)]
    fn finish(self) -> usize {
        if self.count > 0 {
            self.dst[self.ctrl_pos] = self.control_byte;
            self.dst_pos
        } else {
            self.ctrl_pos
        }
    }
}

/// Compress `src` into `dst` using `state`'s dictionary. `state` is reset
/// internally with a seed derived from `src`'s first bytes — any prior
/// dictionary contents in `state` are discarded.
pub fn compress_into(
    src: &[u8],
    dst: &mut [u8],
    state: &mut CompressorState,
    mode: DictMode,
) -> Result<usize, CompressError> {
    if src.len() > u32::MAX as usize {
        return Err(CompressError::InputTooLarge);
    }

    if dst.len() < HEADER_SIZE {
        return Err(CompressError::OutputBufferTooSmall);
    }

    // Fast path: empty input
    if src.is_empty() {
        dst[0..2].copy_from_slice(&MAGIC);
        dst[2] = match mode {
            DictMode::Reset => 0x00,
            DictMode::Freeze => FLAG_MODE_FREEZE,
        };
        dst[3] = FORMAT_VERSION;
        dst[4..8].copy_from_slice(&0u32.to_le_bytes());
        return Ok(HEADER_SIZE);
    }

    // Check if raw fallback is immediately needed (e.g. small incompressible payload)
    let min_needed_for_raw = HEADER_SIZE + src.len();

    // Derive dynamic seed from input header bytes using mwhash
    let dynamic_seed = if src.is_empty() {
        CompressorState::SEED_DEFAULT
    } else {
        let sample_len = core::cmp::min(src.len(), 64);
        ::mwhash::mwhash(&src[..sample_len])
    };

    state.reset_with_seed(dynamic_seed);

    let mut writer = PackWriter::new(dst, HEADER_SIZE)?;
    let mut src_pos = 0;

    while src_pos < src.len() {
        let first_byte = src[src_pos];
        let mut curr_node = first_byte as u16;
        let mut match_len = 1;

        while src_pos + match_len < src.len() {
            let next_sym = src[src_pos + match_len];
            let h = state.hash(curr_node, next_sym);
            let cand = state.hash_table[h];

            if cand != CompressorState::EMPTY_HASH && cand < state.node_count {
                let node = state.nodes[cand as usize];
                if node.parent_id == curr_node && node.symbol == next_sym {
                    curr_node = cand;
                    match_len += 1;
                    continue;
                }
            }
            break;
        }

        // Cyclic run extension: src[match_target + k] == src[match_base + k].
        // Encoded in the token's run bits (see wire format docs) instead of a
        // separate token.
        let mut ext_len = 0;
        let max_ext = core::cmp::min(263, src.len() - (src_pos + match_len));
        let match_target = src_pos + match_len;
        let match_base = src_pos;

        if max_ext > 0 && src[match_target] == src[match_base] {
            while ext_len + 8 <= max_ext {
                let a = unsafe {
                    core::ptr::read_unaligned(src.as_ptr().add(match_target + ext_len) as *const u64)
                };
                let b = unsafe {
                    core::ptr::read_unaligned(src.as_ptr().add(match_base + ext_len) as *const u64)
                };
                if a == b {
                    ext_len += 8;
                } else {
                    let diff = a ^ b;
                    #[cfg(target_endian = "little")]
                    let matching_bytes = (diff.trailing_zeros() / 8) as usize;
                    #[cfg(target_endian = "big")]
                    let matching_bytes = (diff.leading_zeros() / 8) as usize;
                    ext_len += matching_bytes;
                    break;
                }
            }
            while ext_len < max_ext && src[match_target + ext_len] == src[match_base + ext_len] {
                ext_len += 1;
            }
        }

        let token_node: u16;
        if match_len == 1 && ext_len == 0 {
            writer.push_literal(first_byte)?;
            token_node = first_byte as u16;
            src_pos += 1;
        } else {
            writer.push_hit(curr_node, ext_len)?;
            token_node = curr_node;
            src_pos += match_len + ext_len;
        }

        // Insert (prev_node + first_byte_of_this_token) as a new dictionary entry —
        // LZMW-style: we learn phrases from what we just emitted, one step behind.
        if !state.is_frozen {
            if state.prev_node != ROOT_PARENT_ID {
                let was_reset = state.try_insert(state.prev_node, first_byte, mode);
                if was_reset {
                    state.prev_node = ROOT_PARENT_ID;
                } else {
                    state.prev_node = token_node;
                }
            } else {
                state.prev_node = token_node;
            }
        }

        // Early abort on expansion
        if writer.dst_pos > min_needed_for_raw {
            if dst.len() < min_needed_for_raw {
                return Err(CompressError::OutputBufferTooSmall);
            }
            dst[0..2].copy_from_slice(&MAGIC);
            dst[2] = FLAG_RAW_FALLBACK | if mode == DictMode::Freeze { FLAG_MODE_FREEZE } else { 0 };
            dst[3] = FORMAT_VERSION;
            dst[4..8].copy_from_slice(&(src.len() as u32).to_le_bytes());
            dst[HEADER_SIZE..HEADER_SIZE + src.len()].copy_from_slice(src);
            return Ok(min_needed_for_raw);
        }
    }

    let final_len = writer.finish();

    if final_len >= min_needed_for_raw {
        if dst.len() < min_needed_for_raw {
            return Err(CompressError::OutputBufferTooSmall);
        }
        dst[0..2].copy_from_slice(&MAGIC);
        dst[2] = FLAG_RAW_FALLBACK | if mode == DictMode::Freeze { FLAG_MODE_FREEZE } else { 0 };
        dst[3] = FORMAT_VERSION;
        dst[4..8].copy_from_slice(&(src.len() as u32).to_le_bytes());
        dst[HEADER_SIZE..HEADER_SIZE + src.len()].copy_from_slice(src);
        return Ok(min_needed_for_raw);
    }

    dst[0..2].copy_from_slice(&MAGIC);
    dst[2] = match mode {
        DictMode::Reset => 0x00,
        DictMode::Freeze => FLAG_MODE_FREEZE,
    };
    dst[3] = FORMAT_VERSION;
    dst[4..8].copy_from_slice(&(src.len() as u32).to_le_bytes());

    Ok(final_len)
}
