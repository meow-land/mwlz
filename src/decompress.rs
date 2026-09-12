//! Implementation of the mwLZ zero-cost decompressor.

use crate::types::*;

/// Reusable workspace state for the mwLZ decompressor (16 KB).
pub struct DecompressorState {
    pub nodes: [Node; MAX_NODES],
}

impl DecompressorState {
    /// Create a new initialized decompressor state.
    pub fn new() -> Self {
        let mut state = Self {
            nodes: [Node::ROOT; MAX_NODES],
        };
        state.reset();
        state
    }

    /// Reset roots to initial state.
    #[inline(always)]
    pub fn reset(&mut self) {
        let mut i = 0;
        while i < ROOT_COUNT {
            self.nodes[i] = Node::new(ROOT_PARENT_ID, i as u8, 1);
            i += 1;
        }
    }
}

impl Default for DecompressorState {
    fn default() -> Self {
        Self::new()
    }
}

/// Decompress `src` into `dst` using `state`'s dictionary. `state` is reset
/// internally before decoding — see `compress_into` for the matching note.
pub fn decompress_into_with_state(
    src: &[u8],
    dst: &mut [u8],
    state: &mut DecompressorState,
) -> Result<usize, DecompressError> {
    if src.len() < HEADER_SIZE {
        return Err(DecompressError::CorruptedHeader);
    }

    // Check magic signature ("ML")
    if src[0] != MAGIC[0] || src[1] != MAGIC[1] {
        return Err(DecompressError::InvalidMagic);
    }

    if src[3] != FORMAT_VERSION {
        return Err(DecompressError::UnsupportedVersion);
    }

    let flags = src[2];
    let mode = DictMode::from_flags(flags);

    let uncompressed_size = u32::from_le_bytes([src[4], src[5], src[6], src[7]]) as usize;

    if dst.len() < uncompressed_size {
        return Err(DecompressError::OutputBufferTooSmall);
    }

    if uncompressed_size == 0 {
        return Ok(0);
    }

    // Check Raw Fallback mode
    if (flags & FLAG_RAW_FALLBACK) != 0 {
        let needed = HEADER_SIZE + uncompressed_size;
        if src.len() < needed {
            return Err(DecompressError::CorruptedStream);
        }
        dst[..uncompressed_size].copy_from_slice(&src[HEADER_SIZE..needed]);
        return Ok(uncompressed_size);
    }

    state.reset();
    let nodes = &mut state.nodes;

    let mut node_count: u16 = ROOT_COUNT as u16;
    let mut prev_node: u16 = ROOT_PARENT_ID;
    let mut is_frozen = false;

    let mut src_pos = HEADER_SIZE;
    let mut dst_pos = 0;

    while dst_pos < uncompressed_size {
        if src_pos >= src.len() {
            return Err(DecompressError::CorruptedStream);
        }

        let control_byte = src[src_pos];
        src_pos += 1;

        for bit_idx in 0..8 {
            if dst_pos >= uncompressed_size {
                break;
            }

            let is_hit = ((control_byte >> bit_idx) & 1) != 0;

            let token_node: u16;
            let first_char: u8;

            if !is_hit {
                if src_pos >= src.len() {
                    return Err(DecompressError::CorruptedStream);
                }
                let byte = src[src_pos];
                src_pos += 1;

                dst[dst_pos] = byte;
                dst_pos += 1;

                token_node = byte as u16;
                first_char = byte;
            } else {
                if src_pos + 2 > src.len() {
                    return Err(DecompressError::CorruptedStream);
                }
                let token = u16::from_le_bytes([src[src_pos], src[src_pos + 1]]);
                src_pos += 2;

                let node_id = token & 0x0FFF;
                let run_bits = ((token >> 12) & 0x07) as usize;
                let overflow = (token & 0x8000) != 0;

                let run_ext = if overflow {
                    if src_pos >= src.len() {
                        return Err(DecompressError::CorruptedStream);
                    }
                    let extra = src[src_pos] as usize;
                    src_pos += 1;
                    8 + extra
                } else {
                    run_bits
                };

                if node_id > node_count {
                    return Err(DecompressError::InvalidNodeId);
                }

                let base_len: usize;

                if node_id == node_count {
                    // Token references the node we're about to create (KwKwK): the encoder
                    // emitted a phrase that repeats immediately, so the decoder must
                    // synthesize prev_phrase + prev_phrase's own first byte before this
                    // node formally exists.
                    if prev_node == ROOT_PARENT_ID {
                        return Err(DecompressError::CorruptedStream);
                    }
                    let prev_depth = nodes[prev_node as usize].depth as usize;
                    base_len = prev_depth + 1;

                    let needed_end = match dst_pos.checked_add(base_len).and_then(|v| v.checked_add(run_ext)) {
                        Some(val) => val,
                        None => return Err(DecompressError::OutputBufferTooSmall),
                    };
                    if needed_end > dst.len() {
                        return Err(DecompressError::OutputBufferTooSmall);
                    }

                    unsafe {
                        let dst_ptr = dst.as_mut_ptr();
                        let nodes_ptr = nodes.as_ptr();
                        if prev_depth == 1 {
                            let sym = (*nodes_ptr.add(prev_node as usize)).symbol;
                            *dst_ptr.add(dst_pos) = sym;
                            first_char = sym;
                        } else if prev_depth == 2 {
                            let n = *nodes_ptr.add(prev_node as usize);
                            let p_sym = n.parent_id as u8;
                            *dst_ptr.add(dst_pos) = p_sym;
                            *dst_ptr.add(dst_pos + 1) = n.symbol;
                            first_char = p_sym;
                        } else {
                            let mut curr = prev_node;
                            let mut p = dst_pos + prev_depth;
                            while curr >= ROOT_COUNT as u16 {
                                p -= 1;
                                let n = *nodes_ptr.add(curr as usize);
                                *dst_ptr.add(p) = n.symbol;
                                curr = n.parent_id;
                            }
                            let root_sym = curr as u8;
                            *dst_ptr.add(p - 1) = root_sym;
                            first_char = root_sym;
                        }
                        *dst_ptr.add(dst_pos + prev_depth) = first_char;
                    }
                    dst_pos += base_len;
                    token_node = node_id;
                } else {
                    let node = nodes[node_id as usize];
                    base_len = node.depth as usize;

                    let needed_end = match dst_pos.checked_add(base_len).and_then(|v| v.checked_add(run_ext)) {
                        Some(val) => val,
                        None => return Err(DecompressError::OutputBufferTooSmall),
                    };
                    if needed_end > dst.len() {
                        return Err(DecompressError::OutputBufferTooSmall);
                    }

                    unsafe {
                        let dst_ptr = dst.as_mut_ptr();
                        let nodes_ptr = nodes.as_ptr();
                        if base_len == 1 {
                            *dst_ptr.add(dst_pos) = node.symbol;
                            first_char = node.symbol;
                        } else if base_len == 2 {
                            let p_sym = node.parent_id as u8;
                            *dst_ptr.add(dst_pos) = p_sym;
                            *dst_ptr.add(dst_pos + 1) = node.symbol;
                            first_char = p_sym;
                        } else {
                            let mut curr = node_id;
                            let mut p = dst_pos + base_len;
                            while curr >= ROOT_COUNT as u16 {
                                p -= 1;
                                let n = *nodes_ptr.add(curr as usize);
                                *dst_ptr.add(p) = n.symbol;
                                curr = n.parent_id;
                            }
                            let root_sym = curr as u8;
                            *dst_ptr.add(p - 1) = root_sym;
                            first_char = root_sym;
                        }
                    }

                    dst_pos += base_len;
                    token_node = node_id;
                }

                if run_ext > 0 {
                    let start = dst_pos;
                    let mut s = start - base_len;
                    let mut d = start;
                    let end = start + run_ext;
                    if base_len >= 8 {
                        while d + 8 <= end {
                            unsafe {
                                let chunk = core::ptr::read_unaligned(dst.as_ptr().add(s) as *const u64);
                                core::ptr::write_unaligned(dst.as_mut_ptr().add(d) as *mut u64, chunk);
                            }
                            d += 8;
                            s += 8;
                        }
                    }
                    while d < end {
                        dst[d] = dst[s];
                        d += 1;
                        s += 1;
                    }
                    dst_pos = end;
                }
            }

            // Mirror of the encoder's insertion in compress.rs: learn (prev_node,
            // first_char) as a new node so both sides' dictionaries stay in sync.
            let mut was_reset = false;
            if prev_node != ROOT_PARENT_ID && !is_frozen && node_count < MAX_NODES as u16 {
                let parent = nodes[prev_node as usize];
                if parent.depth < MAX_DEPTH {
                    nodes[node_count as usize] =
                        Node::new(prev_node, first_char, parent.depth + 1);
                    node_count += 1;

                    if node_count == MAX_NODES as u16 {
                        match mode {
                            DictMode::Freeze => {
                                is_frozen = true;
                            }
                            DictMode::Reset => {
                                node_count = ROOT_COUNT as u16;
                                was_reset = true;
                            }
                        }
                    }
                }
            }
            if was_reset {
                prev_node = ROOT_PARENT_ID;
            } else {
                prev_node = token_node;
            }
        }
    }

    if dst_pos != uncompressed_size {
        return Err(DecompressError::CorruptedStream);
    }

    Ok(dst_pos)
}

/// Decompress `src` into `dst` using a temporary stack-allocated workspace state.
#[inline]
pub fn decompress_into(src: &[u8], dst: &mut [u8]) -> Result<usize, DecompressError> {
    let mut state = DecompressorState::new();
    decompress_into_with_state(src, dst, &mut state)
}

