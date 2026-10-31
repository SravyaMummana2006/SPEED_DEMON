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

## Streaming Multithreaded Parsing (v3: Reusable 1 MiB Streaming Buffer per Thread)
- **Architecture**: Stop reading the entire file into memory. Main thread determines file size via `std::fs::metadata` and finds line boundaries serially by seeking to nominal cuts `p_i = i * len / n` and reading 4096-byte blocks to find the first `\n`. Each thread opens its own `File`, seeks to its chunk start, wraps it in `Read::take(chunk_len)`, and streams data through a reusable 1 MiB buffer (`Vec<u8>`). Whole lines up to the last `\n` in the buffer are parsed via `ChunkParser` struct maintaining endpoint dictionary and group tables across blocks; unprocessed tails are shifted to index 0 via `copy_within`. If buffer is full with no newline, it grows dynamically; at EOF, any remaining tail is parsed as a final line. Zero-length `requests.log` immediately produces an empty 0-byte `summary.txt`. Standard library only, no external crates, no `unsafe`.
- **Dataset**: `full-seed1` (3,100,000 records, 238,836,234 bytes)
- **CPU**: 13th Gen Intel(R) Core(TM) i5-13420H (12 logical cores, `available_parallelism = 12`)
- **Measured Wall Times (5 runs)**:
  - Run 1: 0.2949 s (Peak Working Set: 45.73 MB)
  - Run 2: 0.3235 s (Peak Working Set: 44.16 MB)
  - Run 3: 0.3148 s (Peak Working Set: 47.07 MB)
  - Run 4: 0.3248 s (Peak Working Set: 44.77 MB)
  - Run 5: 0.3444 s (Peak Working Set: 44.55 MB)
- **Summary**:
  - **Min Wall Time**: 0.2949 s (294.87 ms) — vs v2 0.4376 s (−32.6%)
  - **Median Wall Time**: 0.3235 s (323.48 ms) — vs v2 0.4711 s (−31.3%)
  - **Peak Working Set**: 44.77 MB median (47.07 MB max) — vs v2 268.97 MB (−83.4% memory reduction)

## Hand-Written Output Formatting (v4 / Part 1: Byte Buffer Formatting)
- **Architecture**: Replaced `writeln!` output formatting loop with hand-written byte serialization directly into a pre-allocated `Vec<u8>`. Custom `write_date` writes `YYYY-MM-DDTHH:00:00Z` from the hour key digits without string formatting overhead. Endpoint bytes are appended directly from raw slices without UTF-8 validation or allocations. Custom inline `write_u32` writes decimal representations directly. Output buffer written in a single `write_all` call. Standard library only, no external crates, no `unsafe`.
- **Dataset**: `full-seed1` (3,100,000 records, 238,836,234 bytes)
- **CPU**: 13th Gen Intel(R) Core(TM) i5-13420H (12 logical cores, `available_parallelism = 12`)
- **Measured Wall Times (5 runs)**:
  - Run 1: 0.2501 s (Peak Working Set: 46.92 MB)
  - Run 2: 0.2446 s (Peak Working Set: 46.33 MB)
  - Run 3: 0.2611 s (Peak Working Set: 46.60 MB)
  - Run 4: 0.2732 s (Peak Working Set: 46.02 MB)
  - Run 5: 0.2721 s (Peak Working Set: 44.43 MB)
- **Summary**:
  - **Min Wall Time**: 0.2446 s (244.61 ms) — vs v3 0.2949 s (−17.1%)
  - **Median Wall Time**: 0.2611 s (261.07 ms) — vs v3 0.3235 s (−19.3%)
  - **Peak Working Set**: 46.33 MB median (46.92 MB max)

## Per-Hour Bucketing & Parallel Merging / Percentiles (v5 / Part 2)
- **Architecture**: Multi-threaded post-processing across hour buckets via `std::thread::scope`. After streaming parse threads join:
  1. Main thread collects global endpoints and pre-computes lexicographical ranks `rank[gid]` once.
  2. Main thread collects distinct hour keys, sorts them ascending, creates per-hour buckets `Vec<HourEntry>`, and moves each thread's groups into their hour bucket (`latencies` vector moved with zero copying).
  3. Worker threads dynamically claim hours via `AtomicUsize`. For each hour: sorts bucket entries by endpoint rank, merges adjacent equal-rank entries (summing counts and appending latencies), sorts latencies, computes nearest-rank percentiles and error ppm, and appends hand-formatted bytes directly to that hour's own buffer.
  4. Main thread sequentially writes the pre-sorted per-hour byte buffers in exact chronological order.
  Zero global mutex contention, standard library only, no external crates, no `unsafe`.
- **Dataset**: `full-seed1` (3,100,000 records, 238,836,234 bytes)
- **CPU**: 13th Gen Intel(R) Core(TM) i5-13420H (12 logical cores, `available_parallelism = 12`)
- **Measured Wall Times (5 runs)**:
  - Run 1: 0.2260 s (Peak Working Set: 50.36 MB)
  - Run 2: 0.2289 s (Peak Working Set: 46.58 MB)
  - Run 3: 0.1333 s (Peak Working Set: 51.36 MB)
  - Run 4: 0.1629 s (Peak Working Set: 49.85 MB)
  - Run 5: 0.1483 s (Peak Working Set: 50.07 MB)
- **Summary**:
  - **Min Wall Time**: 0.1333 s (133.28 ms) — vs v3 0.2949 s (−54.8%)
  - **Median Wall Time**: 0.1629 s (162.90 ms) — vs v3 0.3235 s (−49.6%)
  - **Peak Working Set**: 50.07 MB median (51.36 MB max) — well within 1 GB limit
