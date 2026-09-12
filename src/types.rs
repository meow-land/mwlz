//! Core types, constants, and error definitions for mwLZ.

/// Magic signature bytes for the mwLZ chunk header ("ML").
pub const MAGIC: [u8; 2] = [0x4D, 0x4C];

/// Fixed chunk header size in bytes.
pub const HEADER_SIZE: usize = 8;

/// Current mwLZ chunk format version (stored in byte 3 of the header).
pub const FORMAT_VERSION: u8 = 1;

/// Maximum number of nodes in the prefix tree dictionary (12-bit addressable: 0..4095).
pub const MAX_NODES: usize = 4096;

/// Number of pre-populated root literal nodes (0..255).
pub const ROOT_COUNT: usize = 256;

/// Maximum depth / phrase length of a dictionary node.
pub const MAX_DEPTH: u8 = 64;

/// Capacity of the compressor transition hash table.
pub const HASH_SIZE: usize = 4096;

/// Bitmask for the compressor transition hash table index.
pub const HASH_MASK: usize = HASH_SIZE - 1;

/// Chunk flag bit 0: Dictionary mode (0 = Reset, 1 = Freeze).
pub const FLAG_MODE_FREEZE: u8 = 0x01;

/// Chunk flag bit 1: Raw uncompressed fallback (memcpy mode).
pub const FLAG_RAW_FALLBACK: u8 = 0x02;

/// Sentinel parent_id for root nodes.
pub const ROOT_PARENT_ID: u16 = 0xFFFF;

/// A single node in the prefix tree dictionary. Exactly 4 bytes — this size
/// is load-bearing: it's what lets MAX_NODES nodes fit in 16 KB of L1.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Node {
    pub parent_id: u16,
    pub symbol: u8,
    pub depth: u8,
}

impl Node {
    /// Create a new node.
    #[inline(always)]
    pub const fn new(parent_id: u16, symbol: u8, depth: u8) -> Self {
        Self {
            parent_id,
            symbol,
            depth,
        }
    }

    /// Placeholder used to fill `nodes` arrays before `reset()`/`reset_with_seed()`
    /// populates real root values. Do not treat this as a valid root — `symbol: 0`
    /// here does not mean root byte 0x00.
    pub const ROOT: Self = Self {
        parent_id: ROOT_PARENT_ID,
        symbol: 0,
        depth: 1,
    };
}

/// Dictionary eviction policy when dictionary reaches 4096 nodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum DictMode {
    /// Mode Freeze (0x01): Freeze dictionary as read-only when full.
    #[default]
    Freeze,
    /// Mode Reset (0x00): Reset dictionary back to 256 root nodes when full.
    Reset,
}

impl DictMode {
    /// Extract the dictionary mode from chunk header flags.
    #[inline(always)]
    pub const fn from_flags(flags: u8) -> Self {
        if (flags & FLAG_MODE_FREEZE) != 0 {
            Self::Freeze
        } else {
            Self::Reset
        }
    }

    /// Convert the dictionary mode to header flag bits.
    #[inline(always)]
    pub const fn to_flag(self) -> u8 {
        match self {
            Self::Freeze => FLAG_MODE_FREEZE,
            Self::Reset => 0,
        }
    }
}

/// Errors returned during compression.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompressError {
    /// Destination buffer is too small to hold compressed data.
    OutputBufferTooSmall,
    /// Input size exceeds maximum addressable size (u32::MAX).
    InputTooLarge,
}

impl core::fmt::Display for CompressError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::OutputBufferTooSmall => write!(f, "destination buffer is too small"),
            Self::InputTooLarge => write!(f, "input data exceeds maximum block size"),
        }
    }
}

impl core::error::Error for CompressError {}

/// Errors returned during decompression.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecompressError {
    /// Chunk header magic signature mismatch.
    InvalidMagic,
    /// Unsupported format version.
    UnsupportedVersion,
    /// Header flags or fields are malformed.
    CorruptedHeader,
    /// Output buffer is smaller than uncompressed_size.
    OutputBufferTooSmall,
    /// Stream is truncated or payload bounds exceeded.
    CorruptedStream,
    /// Token contains a Node ID out of valid dictionary bounds.
    InvalidNodeId,
}

impl core::fmt::Display for DecompressError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidMagic => write!(f, "invalid mwLZ magic signature"),
            Self::UnsupportedVersion => write!(f, "unsupported mwLZ format version"),
            Self::CorruptedHeader => write!(f, "corrupted chunk header"),
            Self::OutputBufferTooSmall => {
                write!(f, "output buffer too small for decompressed data")
            }
            Self::CorruptedStream => write!(f, "compressed stream is corrupted or truncated"),
            Self::InvalidNodeId => write!(f, "node id out of dictionary bounds"),
        }
    }
}

impl core::error::Error for DecompressError {}
