use mwlz::{
    compress_with_state, decompress, decompress_with_state, max_compressed_size, CompressorState,
    DecompressorState, DictMode,
};
use std::time::Instant;

fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.2} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn format_speed(mb_s: f64) -> String {
    if mb_s >= 10000.0 {
        format!("{:>7.1} GB/s (raw copy)", mb_s / 1024.0)
    } else if mb_s >= 1000.0 {
        format!("{:>7.1} MB/s ({:.2} GB/s)", mb_s, mb_s / 1024.0)
    } else {
        format!("{:>7.1} MB/s", mb_s)
    }
}

fn bench_dataset(name: &str, data: &[u8], mode: DictMode) {
    let mut state = CompressorState::new();
    let max_comp = max_compressed_size(data.len());
    let mut compressed = vec![0u8; max_comp];
    let mut decompressed = vec![0u8; data.len()];

    // Warmup & verification
    let comp_size = compress_with_state(data, &mut compressed, &mut state, mode).unwrap();
    let decomp_size = decompress(&compressed[..comp_size], &mut decompressed).unwrap();
    assert_eq!(decomp_size, data.len());
    assert_eq!(&decompressed[..], data);

    let mode_str = match mode {
        DictMode::Reset => "Reset",
        DictMode::Freeze => "Freeze",
    };

    // Benchmark compression
    let iters_comp = if data.len() <= 64 * 1024 { 300 } else { 40 };
    let start_comp = Instant::now();
    for _ in 0..iters_comp {
        let _ = compress_with_state(data, &mut compressed, &mut state, mode).unwrap();
    }
    let elapsed_comp = start_comp.elapsed();
    let comp_mb_s = (data.len() * iters_comp) as f64 / elapsed_comp.as_secs_f64() / (1024.0 * 1024.0);

    // Benchmark decompression with state reused across chunks (L1 cache resident)
    let mut decomp_state = DecompressorState::new();
    let iters_decomp = if data.len() <= 64 * 1024 { 600 } else { 80 };
    let start_decomp = Instant::now();
    for _ in 0..iters_decomp {
        let _ = decompress_with_state(&compressed[..comp_size], &mut decompressed, &mut decomp_state).unwrap();
    }
    let elapsed_decomp = start_decomp.elapsed();
    let decomp_mb_s = (data.len() * iters_decomp) as f64 / elapsed_decomp.as_secs_f64() / (1024.0 * 1024.0);

    let ratio = data.len() as f64 / comp_size as f64;

    println!(
        "{:<22} {:<7} {:>10} {:>12} {:>8.2}x {:>13}   {:>26}",
        name,
        mode_str,
        format_size(data.len()),
        format_size(comp_size),
        ratio,
        format!("{:.1} MB/s", comp_mb_s),
        format_speed(decomp_mb_s)
    );
}

fn sample_json_logs(target_size: usize) -> Vec<u8> {
    let services = ["auth-api", "storage-node", "billing-worker", "gateway-proxy"];
    let endpoints = ["/v1/session/verify", "/v1/charges/capture", "/v2/blobs/read", "/healthz"];
    let mut out = Vec::with_capacity(target_size);
    let mut id = 1000u32;

    while out.len() < target_size {
        let s = services[(id as usize) % services.len()];
        let ep = endpoints[(id as usize) % endpoints.len()];
        let status = if id.is_multiple_of(13) { 500 } else { 200 };
        let latency = 2.5 + ((id % 37) as f64) * 0.3;
        let line = format!(
            "{{\"id\":{},\"service\":\"{}\",\"path\":\"{}\",\"status\":{},\"latency_ms\":{:.2}}}\n",
            id, s, ep, status, latency
        );
        out.extend_from_slice(line.as_bytes());
        id += 1;
    }
    out.truncate(target_size);
    out
}

fn sample_rust_code(target_size: usize) -> Vec<u8> {
    let snippet = br#"
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockDescriptor {
    pub chunk_id: u32,
    pub uncompressed_size: u32,
    pub compressed_size: u32,
    pub checksum: u32,
    pub flags: u16,
}

impl BlockDescriptor {
    #[inline]
    pub fn is_compressed(&self) -> bool {
        self.compressed_size < self.uncompressed_size
    }

    pub fn compression_ratio(&self) -> f32 {
        if self.compressed_size == 0 {
            return 1.0;
        }
        self.uncompressed_size as f32 / self.compressed_size as f32
    }
}
"#;
    let mut out = Vec::with_capacity(target_size);
    while out.len() < target_size {
        out.extend_from_slice(snippet);
    }
    out.truncate(target_size);
    out
}

fn sample_archive_index(target_size: usize) -> Vec<u8> {
    let paths = [
        "src/pipeline/scheduler.rs",
        "assets/textures/diffuse.raw",
        "assets/shaders/pbr_lighting.glsl",
        "config/system_tuning.toml",
        "data/records/chunk_0042.bin",
    ];
    let mut out = Vec::with_capacity(target_size);
    let mut idx = 1000u32;

    while out.len() < target_size {
        let p = paths[(idx as usize) % paths.len()];
        let size = 16384 * (1 + (idx % 8));
        let csize = size / 2;
        let line = format!(
            "entry:{:06} | path:{:<32} | size:{:>6} | csize:{:>6} | flags:0x01 | crc:0x{:08x}\n",
            idx, p, size, csize, idx.wrapping_mul(0x9E37_79B9)
        );
        out.extend_from_slice(line.as_bytes());
        idx += 1;
    }
    out.truncate(target_size);
    out
}

fn sample_sensor_csv(target_size: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(target_size);
    out.extend_from_slice(b"timestamp,sensor_id,temperature,pressure,voltage,status\n");
    let base_ts = 1710000000u64;
    let mut row = 0u32;

    while out.len() < target_size {
        let sensor_id = (row % 16) + 1;
        let ts = base_ts + (row as u64 / 16);
        let temp = 22.0 + ((sensor_id % 9) as f64) * 0.4;
        let pressure = 101.3 + ((sensor_id % 5) as f64) * 0.15;
        let voltage = 3.30 + ((sensor_id % 3) as f64) * 0.01;
        let line = format!(
            "{},SENSOR_{:04},{:.2},{:.2},{:.2},OK\n",
            ts, sensor_id, temp, pressure, voltage
        );
        out.extend_from_slice(line.as_bytes());
        row += 1;
    }
    out.truncate(target_size);
    out
}

fn sample_sparse_mask(target_size: usize) -> Vec<u8> {
    let mut out = vec![0u8; target_size];
    for page in out.chunks_mut(4096) {
        page[0] = 0xFF;
        page[1] = 0xAA;
    }
    out
}

fn sample_incompressible(target_size: usize) -> Vec<u8> {
    let mut out = vec![0u8; target_size];
    let mut state = 0x1234_5678u32;
    for b in out.iter_mut() {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        *b = state as u8;
    }
    out
}

fn main() {
    println!(
        "{:<22} {:<7} {:>10} {:>12} {:>9} {:>13}   {:>26}",
        "Dataset", "Mode", "Original", "Compressed", "Ratio", "Compress", "Decompress"
    );
    println!("{}", "-".repeat(105));

    // 1. Web & API JSON payload (64 KB chunk)
    let json_data = sample_json_logs(64 * 1024);
    bench_dataset("JSON API Logs", &json_data, DictMode::Freeze);
    bench_dataset("JSON API Logs", &json_data, DictMode::Reset);

    // 2. Source code (64 KB chunk)
    let code_data = sample_rust_code(64 * 1024);
    bench_dataset("Rust Source Code", &code_data, DictMode::Freeze);

    // 3. Container metadata index (256 KB chunk)
    let index_data = sample_archive_index(256 * 1024);
    bench_dataset(".mw Archive Index", &index_data, DictMode::Freeze);

    // 4. Time-series telemetry (1 MB chunk)
    let csv_data = sample_sensor_csv(1024 * 1024);
    bench_dataset("Sensor Telemetry CSV", &csv_data, DictMode::Freeze);

    // 5. Sparse allocation bitmask (1 MB chunk)
    let sparse_data = sample_sparse_mask(1024 * 1024);
    bench_dataset("Sparse Bitmask", &sparse_data, DictMode::Freeze);

    // 6. High-entropy random data (256 KB chunk - Raw Fallback)
    let random_data = sample_incompressible(256 * 1024);
    bench_dataset("Incompressible (Raw)", &random_data, DictMode::Freeze);
}
