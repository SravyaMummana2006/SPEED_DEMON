# Performance Journal — Track C (Join)

## Baseline (v1: Naive Sequential Rust)
- **Architecture**: Single-threaded, raw bytes parser, `HashMap<u32, String>` region lookup, `HashMap<String, CountryStats>` group accumulator, `std::collections::HashSet<Vec<u8>>` for distinct names.
- **Full Scale (3,320,000 records, 201.9 MB)**:
  - Run 1: 5.5230 s
  - Run 2: 5.3360 s
  - Run 3: 5.5560 s
  - Run 4: 5.4508 s
  - Run 5: 6.9058 s
  - **Min**: 5.3360 s (5,336.04 ms)
  - **Median**: 5.5230 s (5,523.00 ms)
  - **Peak Memory**: 405.69 MB

---

## Optimization 1: Direct Table Region Lookup & Country Indexing (v2)

### Changes
1. **Flat Direct-Indexed Region Table**:
   - Replaced `HashMap<u32, String>` with a flat `Vec<u16>` of size 1,000,000 entries.
   - Any `region_id < 1,000,000` is looked up in $O(1)$ directly by index.
   - Any `region_id >= 1,000,000` is mapped directly to `unmatched_cid` without branching or hashing.
2. **Dense Country Indexing (`cid`)**:
   - Mapped each distinct country string to a dense `u16` index, reserving `0` for `_UNMATCHED`.
   - Replaced per-record `HashMap<String, CountryStats>` with `Vec<CountryStats>` directly indexed by `cid`.
   - Completely eliminated per-record `String` allocations, clones, and hash table lookups.

### Benchmark Results on Full Dataset (3.32M rows)
- **Run 1**: 2.0518 s
- **Run 2**: 2.1031 s
- **Run 3**: 2.1155 s
- **Run 4**: 2.2848 s
- **Run 5**: 2.2922 s
- **Min Wall Time**: **2.0518 s** (**2.60x faster**, 61.5% reduction vs 5.34 s baseline)
- **Median Wall Time**: **2.1155 s** (**2.61x faster**, 61.7% reduction vs 5.52 s baseline)
- **Peak Working Set**: **395.74 MB** (reduced by ~10 MB)

### Verification
- `tiny-seed1`: **PASS** (`e4e43bc39920e9d446c3fcd700fdd35b3959e9495c6aaa738491df38a1aed733`)
- `sample-seed1`: **PASS** (`9035fed36823232dd41ae2d1b9d6d21aa7c73f49d8e95647e953a89d5ebe9a66`)
- `edge1`: **PASS** (`6966d443af80c6e0816b1cc45a5b3c3f687d3040d96f656f817d0e6f7f28df16`)
- `edge2`: **PASS** (`b5adb964dccd14430bd011657f2cd31b1f3dba1dc66785aed06282449aa757c1`)
- `edge3`: **PASS** (`9d0a3e299cfa206653ea5c22373679c0e4241be11d2a463ac623ba628ec35451`)
- `full-seed1`: **PASS** (`d8088b1cd42ba1338b1345b107b55db7b96ef1f5066f3ff64fd26a8912a0f536`)

---

## Optimization 2: Global Name Interning & Per-Country Bitsets (v3)

### Changes
1. **Single Global Name Dictionary**:
   - Replaced per-country `HashSet<Vec<u8>>` with a single global dictionary `HashMap<Vec<u8>, u32, BuildHasherDefault<MyHasher>>`.
   - Implemented zero-allocation lookups using input `&[u8]` byte slices (allocating a `Vec<u8>` only on the first insertion of a unique name).
   - Names containing escaped `""` quotes are unescaped into a single reusable scratch buffer cleared per record.
   - Implemented `MyHasher` (custom FxHash rotate-xor-multiply hasher in standard library).
2. **Per-Country Bitsets**:
   - Replaced hash sets with dynamic bitsets `Vec<u64>` per country.
   - Setting a name presence is a single word index and bitwise OR (`name_bits[word_idx] |= mask`).
   - Distinct names count is computed via population count (`count_ones()`).
   - Drastically reduced allocations and heap fragmentation.

### Benchmark Results on Full Dataset (3.32M rows)
- **Run 1**: 1.0306 s
- **Run 2**: 1.1650 s
- **Run 3**: 1.4608 s
- **Run 4**: 1.2767 s
- **Run 5**: 1.3149 s
- **Min Wall Time**: **1.0306 s** (**1.99x faster** vs v2 2.05 s; **5.18x faster** vs v1 baseline 5.34 s)
- **Median Wall Time**: **1.2767 s** (**1.66x faster** vs v2 2.12 s; **4.33x faster** vs v1 baseline 5.52 s)
- **Peak Working Set**: **210.32 MB** (reduced from 395.74 MB — saved 185.42 MB, only ~8.4 MB above raw file size)

### Verification
- `tiny-seed1`: **PASS** (`e4e43bc39920e9d446c3fcd700fdd35b3959e9495c6aaa738491df38a1aed733`)
- `sample-seed1`: **PASS** (`9035fed36823232dd41ae2d1b9d6d21aa7c73f49d8e95647e953a89d5ebe9a66`)
- `edge1`: **PASS** (`6966d443af80c6e0816b1cc45a5b3c3f687d3040d96f656f817d0e6f7f28df16`)
- `edge2`: **PASS** (`b5adb964dccd14430bd011657f2cd31b1f3dba1dc66785aed06282449aa757c1`)
- `edge3`: **PASS** (`9d0a3e299cfa206653ea5c22373679c0e4241be11d2a463ac623ba628ec35451`)
- `full-seed1`: **PASS** (`d8088b1cd42ba1338b1345b107b55db7b96ef1f5066f3ff64fd26a8912a0f536`)

---

## Optimization 3: Multithreaded Two-Pass Record Chunking & Parallel Parsing (v4)

### Changes
1. **Multi-Threaded Architecture via `std::thread::scope` (No external crates, zero `unsafe`)**:
   - Skips BOM and CSV header line once on the main thread; sets `body` to all remaining bytes.
   - Defaults to $n = \text{std::thread::available\_parallelism()}$ (falls back to 1 if `body.len() < 1 MB`).
2. **Phase 1 (Parallel Quote Counting & Parity Prefix)**:
   - Divides `body` into nominal segments $[p_i, p_{i+1})$ where $p_i = i \times \text{body.len()} / n$.
   - Worker threads count quote bytes (`b'"'`) in parallel.
   - Computes prefix-XOR quote parities: $\text{parity\_at}(p_i) = \sum_{j < i} \text{quotes}[j] \pmod 2$.
3. **Phase 2 (Parallel Record Boundary Alignment)**:
   - Chunk 0 starts at 0.
   - For $i > 0$, worker threads scan forward from $p_i$ tracking quote parity; the first newline (`b'\n'`) encountered when parity is EVEN is the record boundary. Chunk $i$ starts at byte $k + 1$.
   - Clamps starts monotonically non-decreasing.
4. **Phase 3 (Parallel Parsing)**:
   - Each thread parses its chunk $\text{body}[\text{starts}[i]..\text{starts}[i+1]]$ independently into thread-local `CountryStats` and thread-local names vector.
5. **Phase 4 (Main Thread Merge)**:
   - Iterates thread-local names, interning them into one global `NameDictionary` and building a local-to-global ID map.
   - Merges bitsets via word iteration with bit manipulation (`trailing_zeros` and `BLSR`), mapping local IDs to global IDs.
   - Sums counts, sums, and computes min/max.

### Benchmark Results on Full Dataset (3.32M rows, 201.9 MB)
- **CPU**: 13th Gen Intel(R) Core(TM) i5-13420H (12 logical processors, `available_parallelism` = 12)
- **Run 1**: 0.5218 s (Peak Working Set: 379.29 MB)
- **Run 2**: 0.5273 s (Peak Working Set: 379.81 MB)
- **Run 3**: 0.4933 s (Peak Working Set: 380.12 MB)
- **Run 4**: 0.4814 s (Peak Working Set: 378.59 MB)
- **Run 5**: 0.4801 s (Peak Working Set: 378.91 MB)
- **Min Wall Time**: **0.4801 s** (480.14 ms) — **2.15x faster** vs v3 (1.0306 s); **11.11x faster** vs v1 baseline (5.3360 s)
- **Median Wall Time**: **0.4933 s** (493.33 ms) — **2.59x faster** vs v3 (1.2767 s); **11.20x faster** vs v1 baseline (5.5230 s)
- **Peak Working Set**: **379.29 MB** median (380.12 MB max)

### Verification
- Matrix testing across 7 thread counts ($n \in \{1, 2, 3, 5, 7, 12, 16\}$) across all 6 datasets (tiny, edge1, edge2, edge3, sample, full): **42 / 42 PASS**
- `tiny-seed1`: **PASS** (`e4e43bc39920e9d446c3fcd700fdd35b3959e9495c6aaa738491df38a1aed733`)
- `sample-seed1`: **PASS** (`9035fed36823232dd41ae2d1b9d6d21aa7c73f49d8e95647e953a89d5ebe9a66`)
- `edge1`: **PASS** (`6966d443af80c6e0816b1cc45a5b3c3f687d3040d96f656f817d0e6f7f28df16`)
- `edge2`: **PASS** (`b5adb964dccd14430bd011657f2cd31b1f3dba1dc66785aed06282449aa757c1`)
- `edge3`: **PASS** (`9d0a3e299cfa206653ea5c22373679c0e4241be11d2a463ac623ba628ec35451`)
- `full-seed1`: **PASS** (`d8088b1cd42ba1338b1345b107b55db7b96ef1f5066f3ff64fd26a8912a0f536`)

---

## Optimization 4: Streaming File Input with Reusable Thread Buffers (v5)

### Changes
1. **Zero Whole-File In-Memory Loading**:
   - Replaced `std::fs::read(&records_path)` with per-thread streaming directly from disk via `File::seek` and `Read::take`.
   - Main thread reads only the first 4096 bytes to skip optional UTF-8 BOM and locate the CSV header newline (`body_start`), obtaining file length via metadata.
2. **Phase 1 (Parallel Quote Counting)**:
   - Worker threads open independent `File` handles, seek to nominal cuts $p_i$, wrap in `take(p_{i+1} - p_i)`, and count quote bytes using a reusable 1 MiB buffer.
   - Main thread computes prefix-XOR quote parities in serial.
3. **Phase 2 (Serial Record Boundary Alignment)**:
   - Main thread scans forward from each $p_i$ in 4096-byte blocks tracking quote parity; the first newline encountered with parity 0 marks the chunk start.
4. **Phase 3 (Parallel Streaming Parsing with `RecordParser`)**:
   - Refactored parsing state into `RecordParser` struct with reusable `scratch_name` and thread-local dictionary.
   - Each thread streams its assigned byte range in a reusable 1 MiB buffer (`BUF_SIZE = 1048576`).
   - Scans backwards from filled buffer length to identify the latest record boundary (`\n` at even quote parity), parses complete records via `parse_block`, and shifts incomplete trailing bytes with `copy_within`.
   - Parses any remaining unclosed record at EOF after streaming loop.
5. **Phase 4 (Merge) & Output**:
   - Kept verbatim.

### Benchmark Results on Full Dataset (3.32M rows, 201.9 MB)
- **CPU**: 13th Gen Intel(R) Core(TM) i5-13420H (12 logical processors, `available_parallelism` = 12)
- **Run 1**: 0.7843 s (Peak Working Set: 196.27 MB)
- **Run 2**: 0.7058 s (Peak Working Set: 195.10 MB)
- **Run 3**: 0.6888 s (Peak Working Set: 197.04 MB)
- **Run 4**: 0.8060 s (Peak Working Set: 197.31 MB)
- **Run 5**: 0.7168 s (Peak Working Set: 195.71 MB)
- **Min Wall Time**: **0.6888 s** (688.81 ms) — **7.75x faster** vs v1 baseline (5.3360 s)
- **Median Wall Time**: **0.7168 s** (716.85 ms) — **7.70x faster** vs v1 baseline (5.5230 s)
- **Peak Working Set**: **196.27 MB** median (**197.31 MB** max) — **reduced by 183.02 MB (48.2%)** vs v4 (379.29 MB)

### Verification
- Matrix testing across 7 thread counts ($n \in \{1, 2, 3, 5, 7, 12, 16\}$) and 5 buffer sizes ($buf \in \{64, 100, 333, 4096, 1048576\}$) across 5 datasets (tiny, edge1, edge2, edge3, sample): **175 / 175 PASS**
- Full dataset verification across 4 configurations ($n \in \{\text{default}, 12\}$, $buf \in \{65536, 1048576\}$): **4 / 4 PASS**
- Total matrix tests: **179 / 179 PASS**
- `tiny-seed1`: **PASS** (`e4e43bc39920e9d446c3fcd700fdd35b3959e9495c6aaa738491df38a1aed733`)
- `sample-seed1`: **PASS** (`9035fed36823232dd41ae2d1b9d6d21aa7c73f49d8e95647e953a89d5ebe9a66`)
- `edge1`: **PASS** (`6966d443af80c6e0816b1cc45a5b3c3f687d3040d96f656f817d0e6f7f28df16`)
- `edge2`: **PASS** (`b5adb964dccd14430bd011657f2cd31b1f3dba1dc66785aed06282449aa757c1`)
- `edge3`: **PASS** (`9d0a3e299cfa206653ea5c22373679c0e4241be11d2a463ac623ba628ec35451`)
- `full-seed1`: **PASS** (`d8088b1cd42ba1338b1345b107b55db7b96ef1f5066f3ff64fd26a8912a0f536`)
