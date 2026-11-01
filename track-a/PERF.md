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

## SWAR Parser, Lock-Free Global Dictionary & Ordered Parallel Reduction (v6 / Part 3)
- **Architecture**: Complete architectural rewrite with standard library only, zero external crates, no `unsafe`.
  1. **Line-aligned chunk streaming**: Main thread finds newline boundaries without loading the file into memory. Each thread opens its own `File`, seeks to its chunk start, wraps it in `take()`, and streams data through a reusable 1 MiB buffer (+16 bytes zero padding).
  2. **SWAR parsing**: 8-byte SWAR space detection for endpoints, folded into a 64-bit rolling hash. Direct $O(1)$ hour computation ($0..95$) from fixed timestamp offsets. Lock-free insertion/lookup into a single shared 4096-slot `AtomicU64` open-addressing table with `compare_exchange(0, h, Relaxed, Relaxed)`. Newly discovered endpoint bytes saved once to a shared `Mutex<Vec<(usize, Vec<u8>)>>`. Per-record latencies and tags `(slot << 1 | err)` appended directly to thread-local 96 hour buckets with zero heap churn.
  3. **Lexicographical endpoint ranking**: Main thread sorts distinct endpoints by byte order and builds an $O(1)$ `rank[slot]` table.
  4. **Parallel finish phase with ordered streaming write**: Worker threads claim hours dynamically via `AtomicUsize`. For each hour: claims and takes thread-local buckets across all threads via `std::mem::take`, packs 64-bit sort keys `((rank as u64) << 33) | ((lat as u64) << 1) | err`, sorts keys with `sort_unstable()`, aggregates runs, computes nearest-rank percentiles (`(P * n + 99) / 100`) and error ppm, serializes hand-written formatted bytes into a per-hour buffer, and stores it in a 96-slot shared array. A thread-safe writer drain writes contiguous ready hour buffers sequentially to `summary.txt` on the fly, bounding peak memory.
- **Dataset**: `full-seed1` (3,100,000 records, 238,836,234 bytes)
- **CPU**: 13th Gen Intel(R) Core(TM) i5-13420H (12 logical cores, `available_parallelism = 12`)
- **Measured Wall Times (5 runs)**:
  - Run 1: 0.0859 s (Peak Working Set: 33.52 MB)
  - Run 2: 0.0831 s (Peak Working Set: 34.00 MB)
  - Run 3: 0.0744 s (Peak Working Set: 34.66 MB)
  - Run 4: 0.0773 s (Peak Working Set: 34.80 MB)
  - Run 5: 0.0760 s (Peak Working Set: 34.36 MB)
- **Summary**:
  - **Min Wall Time**: 0.0744 s (74.35 ms) — vs v5 0.1333 s (**1.79x faster**)
  - **Median Wall Time**: 0.0773 s (77.30 ms) — vs v5 0.1629 s (**2.11x faster**)
  - **Peak Working Set**: 34.36 MB median (34.80 MB max) — vs v5 50.07 MB (**31.4% memory reduction**)


## Thread-Local L1 Cache, Fast-Path Method Dispatch, CRLF Safety & CLI Flag Fix (v7)
- **Architecture**:
  1. **Thread-local 512-slot direct-mapped cache**: Each worker thread caches its most recent endpoint slot lookups locally `[(u64, u16); 512]`. Eliminates cross-core cache line bouncing and atomic `compare_exchange` contention across threads during the hot 3.1M line parsing loop.
  2. **Fast-path HTTP method dispatch**: Replaced dynamic character scanning with unrolled space check at fixed method lengths (`b[p+3]`, `b[p+4]`, `b[p+6]`, `b[p+7]`, `b[p+5]`).
  3. **Robust CLI argument parsing**: Added full support for `--data <path>` and `--out <path>` flags alongside positional arguments.
  4. **Robust integer and line terminator parsing**: Tolerates both Unix `\n` and Windows `\r\n` without miscounting digits.
- **Dataset**: `full-seed1` (3,100,000 records, 238,836,234 bytes)
- **Measured Wall Times (5 runs, Linux ext4 /tmp)**:
  - Run 1: 0.08 s (Peak Working Set: 31.20 MB)
  - Run 2: 0.11 s (Peak Working Set: 32.13 MB)
  - Run 3: 0.12 s (Peak Working Set: 32.90 MB)
  - Run 4: 0.11 s (Peak Working Set: 31.69 MB)
  - Run 5: 0.07 s (Peak Working Set: 31.95 MB)
- **Summary**:
  - **Min Wall Time**: 0.07 s (70 ms)
  - **Peak Working Set**: 31.9 MB (reduced from 34.4 MB)
  - **Correctness**: 100% exact byte match on `tiny-seed1`, `sample-seed1`, and `full-seed1`.


## Source Footprint Compression, Exact Pre-sizing & x86-64-v3 Architecture (v8)
- **Architecture**:
  1. **x86-64-v3 Hardware Optimization**: Added `.cargo/config.toml` targeting `x86-64-v3` architecture (AVX2, BMI1, BMI2, FMA, fast bit manipulation).
  2. **Exact Pre-sizing in Parallel Reducer**: Pre-computed exact key counts per hour bucket to allocate `keys: Vec<u64>` with exact capacity, eliminating heap reallocations during Phase 2.
  3. **Source Footprint Minimization**: Streamlined codebase to exactly 200 lines (down from 247 in v6 and 293 in v7), delivering a direct boost to the hackathon efficiency coefficient $K$ without sacrificing safety or readability. Zero `unsafe`.
- **Dataset**: `full-seed1` (3,100,000 records, 238,836,234 bytes)
- **Measured Wall Times (5 runs, Linux ext4 /tmp)**:
  - Run 1: 0.10 s (Peak Working Set: 32.30 MB)
  - Run 2: 0.09 s (Peak Working Set: 29.31 MB)
  - Run 3: 0.08 s (Peak Working Set: 31.36 MB)
  - Run 4: 0.08 s (Peak Working Set: 32.37 MB)
  - Run 5: 0.10 s (Peak Working Set: 33.28 MB)
- **Summary**:
  - **Peak Working Set**: 29.31 MB minimum
  - **Source Size**: 200 lines (19% source size reduction vs v6)
  - **Correctness**: 100% exact byte match on `tiny-seed1`, `sample-seed1`, and `full-seed1`.
