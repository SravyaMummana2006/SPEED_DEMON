# Performance Journal — Track A (Ingest)

## Baseline (v1: Sequential Rust, Standard Library Only)
- **Architecture**: Single-threaded, zero external crates, no `unsafe`. Entire file read into memory via `std::fs::read`. Line parsing over raw byte slices without per-line string allocations. Endpoints interned into global dictionary (`HashMap<Vec<u8>, u32, BuildHasherDefault<MyHasher>>`). Packed 64-bit group keys (`(hour_key << 32) | endpoint_id`). Groups store count, 5xx error count, and a `Vec<u32>` of latencies. Latencies sorted with `sort_unstable()`, nearest-rank percentiles computed via `(p * n + 99) / 100`. Output sorted by chronological hour, then unsigned byte order of endpoints.
- **Dataset**: `full-seed1` (3,100,000 records, 238,836,234 bytes)
- **CPU**: 13th Gen Intel(R) Core(TM) i5-13420H
- **Measured Wall Times (5 runs)**:
  - Run 1: 0.8322 s (Peak Working Set: 260.16 MB)
  - Run 2: 0.7951 s (Peak Working Set: 260.12 MB)
  - Run 3: 0.8400 s (Peak Working Set: 260.13 MB)
  - Run 4: 0.7971 s (Peak Working Set: 260.13 MB)
  - Run 5: 0.7925 s (Peak Working Set: 260.16 MB)
- **Summary**:
  - **Min Wall Time**: 0.7925 s (792.52 ms)
  - **Median Wall Time**: 0.7971 s (797.06 ms)
  - **Peak Working Set**: 260.13 MB median (260.16 MB max)

## Multithreaded Parsing (v2: std::thread::scope)
- **Architecture**: Multi-threaded chunked line parsing via `std::thread::scope`. Zero external crates, no `unsafe`. File split across $n = \text{available\_parallelism()}$ threads at aligned newline boundaries. Each thread processes disjoint chunk into thread-local endpoint table and group table. Main thread sequentially merges local endpoint IDs and group aggregates, performs parallel group sort and per-group latency sort/percentile calculation, and emits `summary.txt`.
- **Dataset**: `full-seed1` (3,100,000 records, 238,836,234 bytes)
- **CPU**: 13th Gen Intel(R) Core(TM) i5-13420H (12 logical cores)
- **Measured Wall Times (5 runs)**:
  - Run 1: 0.4376 s (Peak Working Set: 269.04 MB)
  - Run 2: 0.4462 s (Peak Working Set: 268.71 MB)
  - Run 3: 0.5327 s (Peak Working Set: 268.16 MB)
  - Run 4: 0.5624 s (Peak Working Set: 270.21 MB)
  - Run 5: 0.4711 s (Peak Working Set: 268.97 MB)
- **Summary**:
  - **Min Wall Time**: 0.4376 s (437.60 ms) — **1.81x faster** vs 0.7925 s baseline
  - **Median Wall Time**: 0.4711 s (471.10 ms) — **1.69x faster** vs 0.7971 s baseline
  - **Peak Working Set**: 268.97 MB median (270.21 MB max) — within 1 GB limit
