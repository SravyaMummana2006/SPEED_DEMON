use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::env;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

#[inline(always)]
fn parse_u32(bytes: &[u8], pos: &mut usize) -> u32 {
    while *pos < bytes.len()
        && (bytes[*pos] == b' ' || bytes[*pos] == b'\n' || bytes[*pos] == b'\r')
    {
        *pos += 1;
    }
    let mut val: u64 = 0;
    while *pos < bytes.len() && bytes[*pos] >= b'0' && bytes[*pos] <= b'9' {
        val = val * 10 + (bytes[*pos] - b'0') as u64;
        *pos += 1;
    }
    val as u32
}

fn parse_args() -> (PathBuf, PathBuf) {
    let args: Vec<String> = env::args().collect();
    let mut data_dir = PathBuf::from("/data");
    let mut out_dir = PathBuf::from("/out");

    let mut i = 1;
    let mut positionals = Vec::new();
    while i < args.len() {
        if args[i] == "--data" && i + 1 < args.len() {
            data_dir = PathBuf::from(&args[i + 1]);
            i += 2;
        } else if args[i] == "--out" && i + 1 < args.len() {
            out_dir = PathBuf::from(&args[i + 1]);
            i += 2;
        } else {
            positionals.push(&args[i]);
            i += 1;
        }
    }

    if !positionals.is_empty() {
        data_dir = PathBuf::from(positionals[0]);
    }
    if positionals.len() >= 2 {
        out_dir = PathBuf::from(positionals[1]);
    }

    (data_dir, out_dir)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (data_dir, out_dir) = parse_args();

    let graph_path = data_dir.join("graph.txt");
    let queries_path = data_dir.join("queries.txt");

    let graph_bytes = std::fs::read(&graph_path)?;
    let query_bytes = std::fs::read(&queries_path)?;

    // Parse header
    let mut pos = 0;
    let n = parse_u32(&graph_bytes, &mut pos) as usize;
    let m = parse_u32(&graph_bytes, &mut pos) as usize;

    let first_edge_pos = pos;

    // Pass 1: count out-degrees
    let mut counts = vec![0u32; n];
    let mut p1 = first_edge_pos;
    while p1 < graph_bytes.len() {
        while p1 < graph_bytes.len()
            && (graph_bytes[p1] == b' ' || graph_bytes[p1] == b'\n' || graph_bytes[p1] == b'\r')
        {
            p1 += 1;
        }
        if p1 >= graph_bytes.len() {
            break;
        }
        let mut u: u32 = 0;
        while p1 < graph_bytes.len() && graph_bytes[p1] >= b'0' && graph_bytes[p1] <= b'9' {
            u = u * 10 + (graph_bytes[p1] - b'0') as u32;
            p1 += 1;
        }
        counts[u as usize] += 1;
        while p1 < graph_bytes.len() && graph_bytes[p1] != b'\n' {
            p1 += 1;
        }
        if p1 < graph_bytes.len() && graph_bytes[p1] == b'\n' {
            p1 += 1;
        }
    }

    // Prefix sums for CSR offsets
    let mut offsets = vec![0u32; n + 1];
    for i in 0..n {
        offsets[i + 1] = offsets[i] + counts[i];
    }
    let mut cursor = offsets.clone();

    // Pass 2: fill CSR edges array
    let mut edges = vec![(0u32, 0u32); m];
    let mut p2 = first_edge_pos;
    for _ in 0..m {
        let u = parse_u32(&graph_bytes, &mut p2);
        let v = parse_u32(&graph_bytes, &mut p2);
        let w = parse_u32(&graph_bytes, &mut p2);
        let dest = cursor[u as usize] as usize;
        cursor[u as usize] += 1;
        edges[dest] = (v, w);
    }
    drop(cursor);
    drop(counts);
    drop(graph_bytes);

    // Parse queries
    let mut queries = Vec::new();
    let mut pq = 0;
    while pq < query_bytes.len() {
        while pq < query_bytes.len()
            && (query_bytes[pq] == b' ' || query_bytes[pq] == b'\n' || query_bytes[pq] == b'\r')
        {
            pq += 1;
        }
        if pq >= query_bytes.len() {
            break;
        }
        let s = parse_u32(&query_bytes, &mut pq);
        let t = parse_u32(&query_bytes, &mut pq);
        queries.push((s, t));
    }
    drop(query_bytes);

    let q_len = queries.len();
    let mut answers: Vec<Option<u64>> = vec![None; q_len];

    if q_len > 0 {
        let n_threads = std::thread::available_parallelism()
            .map(|p| p.get())
            .unwrap_or(1)
            .min(q_len)
            .max(1);

        let next_query = AtomicUsize::new(0);

        let thread_results: Vec<Vec<(usize, Option<u64>)>> = std::thread::scope(|s| {
            let mut handles = Vec::with_capacity(n_threads);
            for _ in 0..n_threads {
                handles.push(s.spawn(|| {
                    let mut local_results = Vec::new();
                    let mut dist = vec![u64::MAX; n];
                    let mut touched = Vec::new();
                    let mut heap: BinaryHeap<Reverse<(u64, u32)>> = BinaryHeap::new();

                    loop {
                        let idx = next_query.fetch_add(1, Ordering::Relaxed);
                        if idx >= q_len {
                            break;
                        }
                        let (qs, qt) = queries[idx];
                        if qs == qt {
                            local_results.push((idx, Some(0)));
                            continue;
                        }

                        dist[qs as usize] = 0;
                        touched.push(qs);
                        heap.push(Reverse((0, qs)));

                        let mut found = None;

                        while let Some(Reverse((d, u))) = heap.pop() {
                            if d > dist[u as usize] {
                                continue;
                            }
                            if u == qt {
                                found = Some(d);
                                break;
                            }
                            let start = offsets[u as usize] as usize;
                            let end = offsets[u as usize + 1] as usize;
                            for edge_idx in start..end {
                                let (v, w) = edges[edge_idx];
                                let nd = d + w as u64;
                                if nd < dist[v as usize] {
                                    if dist[v as usize] == u64::MAX {
                                        touched.push(v);
                                    }
                                    dist[v as usize] = nd;
                                    heap.push(Reverse((nd, v)));
                                }
                            }
                        }

                        heap.clear();
                        for &u in &touched {
                            dist[u as usize] = u64::MAX;
                        }
                        touched.clear();

                        local_results.push((idx, found));
                    }
                    local_results
                }));
            }
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });

        for thread_res in thread_results {
            for (idx, ans) in thread_res {
                answers[idx] = ans;
            }
        }
    }

    // Write output
    std::fs::create_dir_all(&out_dir)?;
    let out_file_path = out_dir.join("distances.txt");
    let mut out_file = BufWriter::new(File::create(&out_file_path)?);

    for ans in answers {
        if let Some(d) = ans {
            writeln!(out_file, "{}", d)?;
        } else {
            writeln!(out_file, "-1")?;
        }
    }

    out_file.flush()?;
    Ok(())
}
