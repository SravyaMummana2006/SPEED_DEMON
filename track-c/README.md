# Track C: Join
Fast, parallelized join and aggregation of records.csv and regions.csv for Speed Demon 2026.
Uses Rust standard library only (no external crates, zero unsafe).
Built with cargo release profile (opt-level = 3, LTO, single codegen unit, abort on panic, stripped).
Emits byte-identical aggregates.txt matching the Track C specification.
