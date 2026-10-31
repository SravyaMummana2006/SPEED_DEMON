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
