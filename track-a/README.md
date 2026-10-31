# Track A: Ingest
Fast, parallelized log parsing and metrics aggregation of requests.log for Speed Demon 2026.
Uses Rust standard library only (no external crates, zero unsafe, std::thread::scope).
Built with cargo release profile (opt-level = 3, LTO, single codegen unit, abort on panic, stripped).
Emits byte-identical summary.txt matching the Track A specification.
