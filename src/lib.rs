//! # mwLZ (Micro-Window LZ)
//!
//! Deterministic dictionary compressor based on prefix trees and quantum byte-packs.
//! Entire compressor/decompressor state fits in L1 data cache (24 KB / 16 KB).
//! `#![no_std]`, zero heap allocations in the core engine.

#![no_std]

#[cfg(feature = "alloc")]
extern crate alloc;

#[cfg(feature = "std")]
extern crate std;

mod compress;
mod decompress;
pub mod types;

pub use compress::CompressorState;
pub use decompress::DecompressorState;
pub use types::{
    CompressError, DecompressError, DictMode, Node, FLAG_MODE_FREEZE, FLAG_RAW_FALLBACK,
    FORMAT_VERSION, HEADER_SIZE, MAGIC, MAX_DEPTH, MAX_NODES, ROOT_COUNT,
};

/// Calculate the maximum buffer size required to safely compress an input of length `src_len`.
#[inline]
pub const fn max_compressed_size(src_len: usize) -> usize {
    HEADER_SIZE + src_len + (src_len / 8 + 1) * 3 + 32
}

/// Compress `src` into `dst` with temporary stack allocation of workspace state.
///
/// ```rust
/// use mwlz::{compress, decompress, max_compressed_size, DictMode};
///
/// let data = b"sample uncompressed payload";
/// let mut comp = vec![0u8; max_compressed_size(data.len())];
/// let mut decomp = vec![0u8; data.len()];
///
/// let c_len = compress(data, &mut comp, DictMode::Freeze).unwrap();
/// let d_len = decompress(&comp[..c_len], &mut decomp).unwrap();
/// assert_eq!(&decomp[..d_len], data);
/// ```
#[inline]
pub fn compress(
    src: &[u8],
    dst: &mut [u8],
    mode: DictMode,
) -> Result<usize, CompressError> {
    let mut state = CompressorState::new();
    compress::compress_into(src, dst, &mut state, mode)
}

/// Compress `src` into `dst`, reusing `state` across calls to avoid
/// re-initializing the dictionary from cold each time.
#[inline]
pub fn compress_with_state(
    src: &[u8],
    dst: &mut [u8],
    state: &mut CompressorState,
    mode: DictMode,
) -> Result<usize, CompressError> {
    compress::compress_into(src, dst, state, mode)
}

/// Decompress `src` into `dst` with temporary stack allocation of workspace state.
#[inline]
pub fn decompress(src: &[u8], dst: &mut [u8]) -> Result<usize, DecompressError> {
    decompress::decompress_into(src, dst)
}

/// Decompress `src` into `dst`, reusing `state` across calls to avoid
/// re-initializing the dictionary from cold each time.
#[inline]
pub fn decompress_with_state(
    src: &[u8],
    dst: &mut [u8],
    state: &mut DecompressorState,
) -> Result<usize, DecompressError> {
    decompress::decompress_into_with_state(src, dst, state)
}

/// Extract the uncompressed size from an mwLZ chunk header (8 bytes).
#[inline]
pub fn uncompressed_size(src: &[u8]) -> Result<usize, DecompressError> {
    if src.len() < HEADER_SIZE {
        return Err(DecompressError::CorruptedHeader);
    }
    if src[0] != MAGIC[0] || src[1] != MAGIC[1] {
        return Err(DecompressError::InvalidMagic);
    }
    if src[3] != FORMAT_VERSION {
        return Err(DecompressError::UnsupportedVersion);
    }
    let size = u32::from_le_bytes([src[4], src[5], src[6], src[7]]) as usize;
    Ok(size)
}

/// Extract the dictionary mode from an mwLZ chunk header.
#[inline]
pub fn dict_mode(src: &[u8]) -> Result<DictMode, DecompressError> {
    if src.len() < HEADER_SIZE {
        return Err(DecompressError::CorruptedHeader);
    }
    if src[0] != MAGIC[0] || src[1] != MAGIC[1] {
        return Err(DecompressError::InvalidMagic);
    }
    if src[3] != FORMAT_VERSION {
        return Err(DecompressError::UnsupportedVersion);
    }
    Ok(DictMode::from_flags(src[2]))
}

/// Check whether the chunk is in Raw Fallback mode (uncompressed memcpy).
#[inline]
pub fn is_raw_fallback(src: &[u8]) -> Result<bool, DecompressError> {
    if src.len() < HEADER_SIZE {
        return Err(DecompressError::CorruptedHeader);
    }
    if src[0] != MAGIC[0] || src[1] != MAGIC[1] {
        return Err(DecompressError::InvalidMagic);
    }
    if src[3] != FORMAT_VERSION {
        return Err(DecompressError::UnsupportedVersion);
    }
    Ok((src[2] & FLAG_RAW_FALLBACK) != 0)
}

/// Compress `src` into a newly allocated `Vec<u8>` (available with `feature = "alloc"`).
#[cfg(feature = "alloc")]
pub fn compress_to_vec(src: &[u8], mode: DictMode) -> Result<alloc::vec::Vec<u8>, CompressError> {
    let mut state = CompressorState::new();
    let max_len = max_compressed_size(src.len());
    let mut dst = alloc::vec![0u8; max_len];
    let written = compress_with_state(src, &mut dst, &mut state, mode)?;
    dst.truncate(written);
    Ok(dst)
}

/// Decompress `src` into a newly allocated `Vec<u8>` (available with `feature = "alloc"`).
#[cfg(feature = "alloc")]
pub fn decompress_to_vec(src: &[u8]) -> Result<alloc::vec::Vec<u8>, DecompressError> {
    let size = uncompressed_size(src)?;
    let mut dst = alloc::vec![0u8; size];
    let written = decompress(src, &mut dst)?;
    dst.truncate(written);
    Ok(dst)
}
