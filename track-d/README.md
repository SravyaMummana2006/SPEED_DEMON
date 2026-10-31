# Track D: Traverse
Fast, parallelized shortest path computation on large directed graphs for Speed Demon 2026.
Uses Rust standard library only (no external crates, zero unsafe, CSR graph + parallel Dijkstra).
Built with cargo release profile (opt-level = 3, LTO, single codegen unit, abort on panic, stripped).
Emits byte-identical distances.txt matching the Track D specification.
