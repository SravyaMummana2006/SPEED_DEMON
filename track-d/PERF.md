# Performance Journal — Track D (Traverse)

## Baseline (v1: Sequential Rust, Standard Library Only)
- **Architecture**: Single-threaded, zero external crates, no `unsafe`. CSR graph representation (offsets `n+1` u32, edges `m` (u32, u32) pairs) populated via counting sort by node `u`. Single-threaded sequential Dijkstra per query with `std::collections::BinaryHeap<Reverse<(u64, u32)>>`, `Vec<u64>` distance table reset via touched list.
- **Dataset**: `sample-seed1` (174,051 nodes, 500,000 edges, 200 queries)
- **CPU**: 13th Gen Intel(R) Core(TM) i5-13420H
- **Measured Wall Time**: 0.2456 s (245.59 ms)

## Multithreaded Across Queries (v2: std::thread::scope)
- **Architecture**: Multi-threaded query processing via `std::thread::scope` across $n = \text{available\_parallelism()}$ worker threads (12 threads). Work distribution via shared `AtomicUsize` dynamic work stealing. Each thread owns independent `dist` array, `touched` tracking list, and `BinaryHeap`. Results returned as `(index, answer)` pairs and placed in input order.
- **Dataset**: `full-seed1` (1,740,512 nodes, 5,000,000 edges, 2,000 queries)
- **CPU**: 13th Gen Intel(R) Core(TM) i5-13420H (12 logical cores, `available_parallelism = 12`)
- **Measured Wall Times (5 runs)**:
  - Run 1: 2.7790 s (Peak Working Set: 223.87 MB)
  - Run 2: 2.6477 s (Peak Working Set: 223.77 MB)
  - Run 3: 2.6895 s (Peak Working Set: 223.71 MB)
  - Run 4: 2.5024 s (Peak Working Set: 223.75 MB)
  - Run 5: 2.5380 s (Peak Working Set: 223.78 MB)
- **Summary**:
  - **Min Wall Time**: 2.5024 s (2,502.37 ms)
  - **Median Wall Time**: 2.6477 s (2,647.74 ms)
  - **Peak Working Set**: 223.77 MB median (223.87 MB max) — well within 1 GB limit

## Parallel CSR Build (v3: AtomicU32/AtomicU64 parallel passes)
- **Architecture**: Two-pass parallel CSR construction via `std::thread::scope`. Body bytes (after header line) split into `nt` line-aligned chunks (nominal cut `p_i = i * body_len / nt`, start after next `\n`). **Pass 1** (parallel): each thread parses `u` per line, increments shared `Vec<AtomicU32>` counts with `Relaxed` fetch_add. Main thread builds prefix-sum `offsets` and cursor (`Vec<AtomicU32>` seeded from `offsets[..n]`). **Pass 2** (parallel): each thread parses `u v w`, atomically claims a slot via `cursor[u].fetch_add(1, Relaxed)`, stores packed edge `(v as u64) << 32 | w as u64` into `Vec<AtomicU64>` with `Relaxed` store. Search reads packed u64 edges with `load(Relaxed)`. `nt` = `available_parallelism()` (fallback 1); reduced to 1 if body < 1 MB. No unsafe, no crates, std only.
- **Dataset**: `full-seed1` (1,740,512 nodes, 5,000,000 edges, 2,000 queries)
- **CPU**: 13th Gen Intel(R) Core(TM) i5-13420H (12 logical cores, `available_parallelism = 12`)
- **Measured Wall Times (5 runs)**:
  - Run 1: 2.7454 s (Peak Working Set: 219.71 MB)
  - Run 2: 2.5154 s (Peak Working Set: 223.86 MB)
  - Run 3: 2.7066 s (Peak Working Set: 223.63 MB)
  - Run 4: 2.3336 s (Peak Working Set: 223.87 MB)
  - Run 5: 2.3284 s (Peak Working Set: 223.79 MB)
- **Summary**:
  - **Min Wall Time**: 2.3284 s (2,328.36 ms) — vs v2 2.5024 s (−7.0%)
  - **Median Wall Time**: 2.5154 s (2,515.44 ms) — vs v2 2.6477 s (−5.0%)
  - **Peak Working Set**: 223.79 MB median (223.87 MB max) — within 1 GB limit

## Parallel Bidirectional CSR & Balanced Bidirectional Dijkstra (v4)
- **Architecture**:
  1. **Dual Parallel CSR Construction**: In Pass 1, worker threads parse $u$ and $v$ per line, atomically incrementing `counts_out[u]` and `counts_in[v]`. The main thread computes prefix sums for both `out_offsets` and `in_offsets` ($n+1$ entries each). In Pass 2, worker threads parse $u, v, w$, claiming slots via `cursor_out[u].fetch_add(1)` and `cursor_in[v].fetch_add(1)` to store packed edges `((v as u64) << 32) | w as u64` in `fwd_edges` and `((u as u64) << 32) | w as u64` in `rev_edges`. File bytes and counts are dropped immediately after Pass 2.
  2. **Balanced Bidirectional Dijkstra Search**: Each worker thread maintains `dist_f` and `dist_b` initialized to `u64::MAX`, per-thread touched lists, and two binary heaps. Per query, the side that has settled fewer nodes so far is expanded (ties go forward). Loop discards stale entries, terminates when either heap is empty or $top_f + top_b \ge \mu$, and updates best distance $\mu = \min(\mu, nd + other\_dist)$ on every successful relaxation when the opposite search has reached the node. Touched nodes are reset to `u64::MAX` after each query.
  3. **Zero Unsafe, Zero External Crates, Standard Library Only**.
- **Dataset**: `full-seed1` (1,740,512 nodes, 5,000,000 edges, 2,000 queries)
- **CPU**: 13th Gen Intel(R) Core(TM) i5-13420H (12 logical cores, `available_parallelism = 12`)
- **Measured Wall Times (5 runs)**:
  - Run 1: 0.9415 s (Peak Working Set: 419.30 MB)
  - Run 2: 0.8401 s (Peak Working Set: 418.32 MB)
  - Run 3: 0.7792 s (Peak Working Set: 418.85 MB)
  - Run 4: 0.8019 s (Peak Working Set: 418.70 MB)
  - Run 5: 0.7749 s (Peak Working Set: 418.71 MB)
- **Summary**:
  - **Min Wall Time**: 0.7749 s (774.95 ms) — **3.01x faster** vs v3 2.33 s (−66.7%)
  - **Median Wall Time**: 0.8019 s (801.89 ms) — **3.14x faster** vs v3 2.52 s (−68.2%)
  - **Peak Working Set**: 418.71 MB median (419.30 MB max) — well within 1 GB limit
- **Verification**:
  - Matrix testing across 7 thread counts ($n \in \{1, 2, 3, 5, 7, 12, 16\}$) across 6 datasets (`tiny`, `edge1`, `edge1_min`, `edge2`, `edge3`, `sample`): **42 / 42 PASS**
  - Full scale dataset verification ($n \in \{12, \text{default}\}$): **2 / 2 PASS** (`a23dbdf0a3e49cc8901310a9ee1dc6a9a569a85297b6da75c2a7002cb2ed277e`)
  - Stress testing across 60 tiny datasets (seeds 2 through 61) with `SD_THREADS=4`: **60 / 60 PASS**
  - Total verified tests: **104 / 104 PASS**
