use mwlz::{
    compress, compress_with_state, decompress, decompress_with_state, max_compressed_size,
    CompressorState, DecompressError, DecompressorState, DictMode,
};

fn roundtrip_test(input: &[u8], mode: DictMode) {
    let mut state = CompressorState::new();
    let max_comp = max_compressed_size(input.len());
    let mut compressed = vec![0u8; max_comp];

    let comp_size =
        compress_with_state(input, &mut compressed, &mut state, mode).expect("compression failed");
    assert!(comp_size <= max_comp);

    let mut decompressed = vec![0u8; input.len()];
    let decomp_size =
        decompress(&compressed[..comp_size], &mut decompressed).expect("decompression failed");

    assert_eq!(decomp_size, input.len());
    assert_eq!(decompressed, input);
}

#[test]
fn test_empty_buffer() {
    roundtrip_test(b"", DictMode::Reset);
    roundtrip_test(b"", DictMode::Freeze);
}

#[test]
fn test_single_byte() {
    roundtrip_test(b"x", DictMode::Reset);
    roundtrip_test(b"x", DictMode::Freeze);
}

#[test]
fn test_small_literals() {
    roundtrip_test(b"abcdefghijklmnopqrstuvwxyz0123456789", DictMode::Reset);
    roundtrip_test(b"abcdefghijklmnopqrstuvwxyz0123456789", DictMode::Freeze);
}

#[test]
fn test_rle_zeros() {
    let zeros_100 = vec![0u8; 100];
    roundtrip_test(&zeros_100, DictMode::Reset);

    let zeros_1000 = vec![0u8; 1000];
    roundtrip_test(&zeros_1000, DictMode::Reset);

    let zeros_64k = vec![0u8; 64 * 1024];
    roundtrip_test(&zeros_64k, DictMode::Reset);
    roundtrip_test(&zeros_64k, DictMode::Freeze);
}

#[test]
fn test_repeating_patterns() {
    let mut pattern = Vec::new();
    for _ in 0..1000 {
        pattern.extend_from_slice(b"CONTAINER_HEADER_RECORD_FIELD_ID_");
    }
    roundtrip_test(&pattern, DictMode::Reset);
    roundtrip_test(&pattern, DictMode::Freeze);
}

#[test]
fn test_json_metadata() {
    let json_template = br#"{"event_id": 1048576, "schema": "mw.container.v1", "tags": ["fast", "lz", "l1_cache", "no_std"], "valid": true, "timestamp": 1726000000}"#;
    let mut data = Vec::new();
    for i in 0..500 {
        data.extend_from_slice(json_template);
        data.extend_from_slice(format!(", \"seq\": {i}\n").as_bytes());
    }
    roundtrip_test(&data, DictMode::Reset);
    roundtrip_test(&data, DictMode::Freeze);
}

#[test]
fn test_block_64kb() {
    let mut data = vec![0u8; 64 * 1024];
    for (i, b) in data.iter_mut().enumerate() {
        *b = ((i % 17) * 13 + (i % 251)) as u8;
    }
    roundtrip_test(&data, DictMode::Reset);
    roundtrip_test(&data, DictMode::Freeze);
}

#[test]
fn test_block_1mb() {
    // 1 MB block typical of .mw container
    let mut data = vec![0u8; 1024 * 1024];
    for (i, chunk) in data.chunks_mut(64).enumerate() {
        let prefix = (i % 256) as u8;
        for (j, b) in chunk.iter_mut().enumerate() {
            *b = prefix ^ (j as u8);
        }
    }
    roundtrip_test(&data, DictMode::Reset);
    roundtrip_test(&data, DictMode::Freeze);
}

#[test]
fn test_incompressible_random_noise_fallback() {
    // Pseudo-random linear congruential generator
    let mut noise = vec![0u8; 16 * 1024];
    let mut seed = 123456789u32;
    for b in noise.iter_mut() {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        *b = (seed >> 24) as u8;
    }
    roundtrip_test(&noise, DictMode::Reset);
    roundtrip_test(&noise, DictMode::Freeze);
}

#[test]
fn test_decompression_error_handling() {
    let mut dst = [0u8; 128];

    // Truncated header
    assert_eq!(
        decompress(&[0x4D], &mut dst),
        Err(DecompressError::CorruptedHeader)
    );

    // Invalid magic
    assert_eq!(
        decompress(&[0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00], &mut dst),
        Err(DecompressError::InvalidMagic)
    );

    // Output buffer too small
    let valid_header = [0x4D, 0x4C, 0x00, 0x01, 0xFF, 0x00, 0x00, 0x00]; // 255 bytes uncompressed, version 1
    let mut small_dst = [0u8; 10];
    assert_eq!(
        decompress(&valid_header, &mut small_dst),
        Err(DecompressError::OutputBufferTooSmall)
    );
}

#[test]
fn test_compression_ratio_check() {
    let text = b"The quick brown fox jumps over the lazy dog. ".repeat(200);

    let max_comp = max_compressed_size(text.len());
    let mut compressed = vec![0u8; max_comp];

    let comp_size =
        compress(&text, &mut compressed, DictMode::Reset).expect("compress failed");

    let ratio = text.len() as f64 / comp_size as f64;
    assert!(
        ratio >= 1.5,
        "Expected good compression ratio for repetitive text, got {ratio:.2}x"
    );
}

#[test]
fn test_run_extension_boundary_conditions() {
    // 1 base byte + 7 repeat bytes = 8 bytes total (boundary: run_ext = 7)
    let len_8 = vec![b'A'; 8];
    roundtrip_test(&len_8, DictMode::Reset);

    // 1 base byte + 8 repeat bytes = 9 bytes total (boundary: run_ext = 8, overflow flag = 1, extra = 0)
    let len_9 = vec![b'A'; 9];
    roundtrip_test(&len_9, DictMode::Reset);

    // 1 base byte + 263 repeat bytes = 264 bytes total (max single-token run_ext = 263, extra = 255)
    let len_264 = vec![b'B'; 264];
    roundtrip_test(&len_264, DictMode::Reset);

    // 1 base byte + 300 repeat bytes (spans multiple tokens with run extensions)
    let len_301 = vec![b'C'; 301];
    roundtrip_test(&len_301, DictMode::Reset);
    roundtrip_test(&len_301, DictMode::Freeze);
}

#[test]
fn test_kwkwk_anomaly() {
    // Classic LZW KwKwK pattern: string + first character of string immediately recurring
    let mut pattern = Vec::new();
    for _ in 0..50 {
        pattern.extend_from_slice(b"ABABABA");
        pattern.extend_from_slice(b"BCBCBCB");
        pattern.extend_from_slice(b"CDCDCDC");
    }
    roundtrip_test(&pattern, DictMode::Reset);
    roundtrip_test(&pattern, DictMode::Freeze);
}

#[test]
fn test_dictionary_saturation_and_reset_cycles() {
    // Generate enough unique 2-byte and 3-byte combinations to fill all 4096 nodes
    // and force multiple Reset / Freeze transitions
    let mut data = Vec::new();
    for i in 0..10_000 {
        data.push((i & 0xFF) as u8);
        data.push(((i >> 8) & 0xFF) as u8);
        data.push((i % 7) as u8);
    }
    roundtrip_test(&data, DictMode::Reset);
    roundtrip_test(&data, DictMode::Freeze);
}

#[test]
fn test_persistent_decompressor_state_reuse() {
    use mwlz::{decompress_with_state, DecompressorState};

    let mut state = CompressorState::new();
    let mut decomp_state = DecompressorState::new();

    let chunks = [
        b"first_chunk_data_entry_12345".as_slice(),
        b"second_chunk_different_stream_data_abcde".as_slice(),
        b"third_chunk_more_and_more_records_xyz".as_slice(),
    ];

    for chunk in chunks {
        let max_comp = max_compressed_size(chunk.len());
        let mut comp = vec![0u8; max_comp];
        let mut decomp = vec![0u8; chunk.len()];

        let c_len = compress_with_state(chunk, &mut comp, &mut state, DictMode::Reset).unwrap();
        let d_len = decompress_with_state(&comp[..c_len], &mut decomp, &mut decomp_state).unwrap();

        assert_eq!(d_len, chunk.len());
        assert_eq!(&decomp, chunk);
    }
}

#[test]
fn test_corrupted_stream_safety() {
    let sample = b"TEST_STREAM_DATA_FOR_CORRUPTION_TESTING_1234567890";
    let max_comp = max_compressed_size(sample.len());
    let mut comp = vec![0u8; max_comp];
    let c_len = compress(sample, &mut comp, DictMode::Reset).unwrap();

    let mut out = vec![0u8; sample.len()];

    // Truncate at every single byte position from header to end
    for cut in 0..c_len {
        let res = decompress(&comp[..cut], &mut out);
        assert!(res.is_err(), "Expected error for truncated stream at {cut}/{c_len}");
    }

    // Corrupt random bytes in payload
    for i in 8..c_len {
        let mut corrupted = comp[..c_len].to_vec();
        corrupted[i] ^= 0xFF;
        // Should either safely return an error or decode to some output without panic
        let _ = decompress(&corrupted, &mut out);
    }
}

#[test]
fn test_header_introspection() {
    use mwlz::{dict_mode, is_raw_fallback, uncompressed_size};

    // Compressible sample (repeating phrases)
    let sample = b"METADATA_HEADER_TEST_KEY_VALUE_RECORD_METADATA_HEADER_TEST_KEY_VALUE_RECORD_MORE_DATA_HERE".repeat(10);
    let mut comp = vec![0u8; max_compressed_size(sample.len())];

    let c_len = compress(&sample, &mut comp, DictMode::Freeze).unwrap();
    assert_eq!(uncompressed_size(&comp[..c_len]).unwrap(), sample.len());
    assert_eq!(dict_mode(&comp[..c_len]).unwrap(), DictMode::Freeze);
    assert!(!is_raw_fallback(&comp[..c_len]).unwrap());

    let c_len_reset = compress(&sample, &mut comp, DictMode::Reset).unwrap();
    assert_eq!(dict_mode(&comp[..c_len_reset]).unwrap(), DictMode::Reset);

    // Incompressible random/short sample (raw fallback)
    let raw_sample = [0x19, 0x82, 0x33, 0x94, 0xA5, 0x11, 0x77, 0x48, 0xC9, 0xD2];
    let c_raw_len = compress(&raw_sample, &mut comp, DictMode::Freeze).unwrap();
    assert_eq!(uncompressed_size(&comp[..c_raw_len]).unwrap(), raw_sample.len());
    assert_eq!(dict_mode(&comp[..c_raw_len]).unwrap(), DictMode::Freeze);
    assert!(is_raw_fallback(&comp[..c_raw_len]).unwrap());

    // Corrupted magic
    let mut bad_magic = comp[..c_len].to_vec();
    bad_magic[0] = 0x00;
    assert_eq!(uncompressed_size(&bad_magic), Err(DecompressError::InvalidMagic));
}

#[test]
fn test_unsupported_version_error() {
    use mwlz::uncompressed_size;

    let sample = b"TEST_SAMPLE_FOR_VERSION_CHECKING_123456789";
    let mut comp = vec![0u8; max_compressed_size(sample.len())];
    let c_len = compress(sample, &mut comp, DictMode::Freeze).unwrap();

    let mut bad_version = comp[..c_len].to_vec();
    bad_version[3] = 99; // Non-existent version

    let mut out = vec![0u8; sample.len()];
    assert_eq!(decompress(&bad_version, &mut out), Err(DecompressError::UnsupportedVersion));
    assert_eq!(uncompressed_size(&bad_version), Err(DecompressError::UnsupportedVersion));
}

#[test]
fn test_default_state_impls() {
    let mut comp_state: CompressorState = Default::default();
    let mut decomp_state: DecompressorState = Default::default();

    let data = b"DEFAULT_IMPL_TEST_PAYLOAD_STRING";
    let mut comp = vec![0u8; max_compressed_size(data.len())];
    let mut decomp = vec![0u8; data.len()];

    let c_len = compress_with_state(data, &mut comp, &mut comp_state, DictMode::Freeze).unwrap();
    let d_len = decompress_with_state(&comp[..c_len], &mut decomp, &mut decomp_state).unwrap();

    assert_eq!(d_len, data.len());
    assert_eq!(&decomp, data);
}

#[test]
fn test_std_vector_apis() {
    #[cfg(feature = "alloc")]
    {
        use mwlz::{compress_to_vec, decompress_to_vec};

        let sample = b"hello_world_std_vector_allocation_test_string_12345";
        let comp = compress_to_vec(sample, DictMode::Freeze).unwrap();
        assert!(comp.len() > 8);

        let decomp = decompress_to_vec(&comp).unwrap();
        assert_eq!(&decomp, sample);
    }
}

#[test]
fn test_power_of_two_and_near_boundaries() {
    let powers: [usize; 13] = [2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096, 8192];
    for p in powers {
        for delta in [-1isize, 0, 1] {
            let len = (p as isize + delta) as usize;
            if len == 0 {
                continue;
            }
            let mut pattern = vec![0u8; len];
            for (i, b) in pattern.iter_mut().enumerate() {
                *b = (i % 251) as u8;
            }
            roundtrip_test(&pattern, DictMode::Reset);
            roundtrip_test(&pattern, DictMode::Freeze);
        }
    }
}

#[test]
fn test_alternating_periodic_patterns() {
    let periods: [usize; 7] = [1, 2, 3, 7, 8, 9, 16];
    for period in periods {
        let mut data = vec![0u8; 1024];
        for (i, b) in data.iter_mut().enumerate() {
            *b = (i % period) as u8;
        }
        roundtrip_test(&data, DictMode::Reset);
        roundtrip_test(&data, DictMode::Freeze);
    }
}

#[test]
fn test_max_depth_phrase_limit() {
    // Generate a repeating pattern long enough to saturate MAX_DEPTH (64)
    let pattern = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let data = pattern.repeat(30); // 36 * 30 = 1080 bytes
    roundtrip_test(&data, DictMode::Reset);
    roundtrip_test(&data, DictMode::Freeze);
}

#[test]
fn test_unaligned_slice_buffers() {
    let sample = b"TEST_UNALIGNED_BUFFER_POINTER_OFFSETS_1234567890".repeat(5);
    let max_comp = max_compressed_size(sample.len()) + 16;
    let mut raw_comp = vec![0u8; max_comp];
    let mut raw_decomp = vec![0u8; sample.len() + 16];

    for offset in 1..8 {
        let comp_slice = &mut raw_comp[offset..];
        let c_len = compress(&sample, comp_slice, DictMode::Freeze).unwrap();

        let decomp_slice = &mut raw_decomp[offset..offset + sample.len()];
        let d_len = decompress(&comp_slice[..c_len], decomp_slice).unwrap();

        assert_eq!(d_len, sample.len());
        assert_eq!(decomp_slice, sample.as_slice());
    }
}

#[test]
fn test_fuzz_pseudo_random_entropy_matrix() {
    // Deterministic XorShift64 PRNG
    let mut state: u64 = 0x8542_5987_1148_5421;
    let mut next_u64 = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };

    let sizes: [usize; 6] = [7, 33, 257, 1025, 4099, 16385];
    for size in sizes {
        // High entropy (random noise)
        let mut noise = vec![0u8; size];
        for b in noise.iter_mut() {
            *b = next_u64() as u8;
        }
        roundtrip_test(&noise, DictMode::Reset);
        roundtrip_test(&noise, DictMode::Freeze);

        // Medium entropy (masked nibbles)
        let mut medium = vec![0u8; size];
        for b in medium.iter_mut() {
            *b = (next_u64() & 0x0F) as u8;
        }
        roundtrip_test(&medium, DictMode::Reset);
        roundtrip_test(&medium, DictMode::Freeze);
    }
}

#[test]
fn test_exact_and_undersized_output_buffers() {
    let sample = b"EXACT_BUFFER_SIZE_VERIFICATION_PAYLOAD_STRING".repeat(4);
    let max_comp = max_compressed_size(sample.len());
    let mut comp = vec![0u8; max_comp];
    let c_len = compress(&sample, &mut comp, DictMode::Freeze).unwrap();

    // Exact destination buffer size
    let mut exact_dst = vec![0u8; sample.len()];
    let d_len = decompress(&comp[..c_len], &mut exact_dst).unwrap();
    assert_eq!(d_len, sample.len());
    assert_eq!(&exact_dst, &sample);

    // 1 byte too small -> must return OutputBufferTooSmall
    let mut small_dst = vec![0u8; sample.len() - 1];
    assert_eq!(
        decompress(&comp[..c_len], &mut small_dst),
        Err(DecompressError::OutputBufferTooSmall)
    );
}

#[test]
fn test_large_chunk_20mb() {
    // 20 MB buffer
    let size = 20 * 1024 * 1024;
    let pattern = b"0123456789abcdefghijklmnopqrstuvwxyz_CHUNK_20MB_DATA_BLOCK!";
    let mut large_buf = vec![0u8; size];
    for (i, b) in large_buf.iter_mut().enumerate() {
        *b = pattern[i % pattern.len()];
    }

    let max_comp = max_compressed_size(size);
    let mut comp = vec![0u8; max_comp];
    let mut decomp = vec![0u8; size];

    let c_len = compress(&large_buf, &mut comp, DictMode::Freeze).unwrap();
    assert!(c_len < size);

    let d_len = decompress(&comp[..c_len], &mut decomp).unwrap();
    assert_eq!(d_len, size);
    assert_eq!(decomp, large_buf);
}


