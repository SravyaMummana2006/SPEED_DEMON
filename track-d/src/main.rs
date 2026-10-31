use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::env;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};

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
        data_dir = PathBuf::from(positionals[0].clone());
    }
    if positionals.len() >= 2 {
        out_dir = PathBuf::from(positionals[1].clone());
    }

    (data_dir, out_dir)
}

/// Compute number of threads: available_parallelism (capped by 1 MB rule).
fn compute_nt(body_len: usize) -> usize {
    let p = std::thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(1);
    if body_len < 1_000_000 {
        1
    } else {
        p
    }
}

/// Split `body` into `nt` line-aligned chunks.
/// starts[0] = 0, starts[nt] = body.len().
/// For i in 1..nt: first '\n' at or after p_i = i * len / nt,
/// start_i = pos_of_newline + 1 (or len if none found).
/// Starts are made non-decreasing.
fn line_aligned_starts(body: &[u8], nt: usize) -> Vec<usize> {
    let len = body.len();
    let mut starts = vec![0usize; nt + 1];
    starts[nt] = len;
    for i in 1..nt {
        let p = i * len / nt;
        let mut q = p;
        while q < len && body[q] != b'\n' {
            q += 1;
        }
        starts[i] = if q < len { q + 1 } else { len };
    }
    // Make non-decreasing
    for i in 1..=nt {
        if starts[i] < starts[i - 1] {
            starts[i] = starts[i - 1];
        }
    }
    starts
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (data_dir, out_dir) = parse_args();

    let graph_path = data_dir.join("graph.txt");
    let queries_path = data_dir.join("queries.txt");

    let graph_bytes = std::fs::read(&graph_path)?;
    let query_bytes = std::fs::read(&queries_path)?;

    // Parse header: first line is "n m"
    let mut pos = 0;
    let n = parse_u32(&graph_bytes, &mut pos) as usize;
    let m = parse_u32(&graph_bytes, &mut pos) as usize;

    // Advance pos to start of body (after the '\n' that ends the header line)
    while pos < graph_bytes.len() && graph_bytes[pos] != b'\n' {
        pos += 1;
    }
    if pos < graph_bytes.len() {
        pos += 1; // skip the '\n'
    }
    let first_edge_pos = pos;

    let body = &graph_bytes[first_edge_pos..];
    let nt = compute_nt(body.len());
    let starts = line_aligned_starts(body, nt);

    // ── Pass 1 (parallel): count out-degrees and in-degrees ───────────────────
    let counts_out: Vec<AtomicU32> = (0..n).map(|_| AtomicU32::new(0)).collect();
    let counts_in: Vec<AtomicU32> = (0..n).map(|_| AtomicU32::new(0)).collect();

    std::thread::scope(|s| {
        for ti in 0..nt {
            let chunk = &body[starts[ti]..starts[ti + 1]];
            let counts_out_ref = &counts_out;
            let counts_in_ref = &counts_in;
            s.spawn(move || {
                let mut p = 0usize;
                while p < chunk.len() {
                    // Skip whitespace / blank lines
                    while p < chunk.len()
                        && (chunk[p] == b' ' || chunk[p] == b'\n' || chunk[p] == b'\r')
                    {
                        p += 1;
                    }
                    if p >= chunk.len() {
                        break;
                    }
                    // Parse u
                    let mut u: u32 = 0;
                    while p < chunk.len() && chunk[p] >= b'0' && chunk[p] <= b'9' {
                        u = u * 10 + (chunk[p] - b'0') as u32;
                        p += 1;
                    }
                    // Skip space(s)
                    while p < chunk.len() && (chunk[p] == b' ' || chunk[p] == b'\r') {
                        p += 1;
                    }
                    // Parse v
                    let mut v: u32 = 0;
                    while p < chunk.len() && chunk[p] >= b'0' && chunk[p] <= b'9' {
                        v = v * 10 + (chunk[p] - b'0') as u32;
                        p += 1;
                    }
                    counts_out_ref[u as usize].fetch_add(1, Ordering::Relaxed);
                    counts_in_ref[v as usize].fetch_add(1, Ordering::Relaxed);

                    // Skip rest of line (skip w)
                    while p < chunk.len() && chunk[p] != b'\n' {
                        p += 1;
                    }
                    if p < chunk.len() {
                        p += 1; // skip '\n'
                    }
                }
            });
        }
    });

    // ── Prefix sums → out_offsets and in_offsets ──────────────────────────────
    let mut out_offsets = vec![0u32; n + 1];
    let mut in_offsets = vec![0u32; n + 1];
    for i in 0..n {
        out_offsets[i + 1] = out_offsets[i] + counts_out[i].load(Ordering::Relaxed);
        in_offsets[i + 1] = in_offsets[i] + counts_in[i].load(Ordering::Relaxed);
    }
    drop(counts_out);
    drop(counts_in);

    let cursor_out: Vec<AtomicU32> = out_offsets[..n]
        .iter()
        .map(|&v| AtomicU32::new(v))
        .collect();
    let cursor_in: Vec<AtomicU32> = in_offsets[..n].iter().map(|&v| AtomicU32::new(v)).collect();

    let fwd_edges: Vec<AtomicU64> = (0..m).map(|_| AtomicU64::new(0)).collect();
    let rev_edges: Vec<AtomicU64> = (0..m).map(|_| AtomicU64::new(0)).collect();

    // ── Pass 2 (parallel): fill forward and reverse CSR edges ─────────────────
    std::thread::scope(|s| {
        for ti in 0..nt {
            let chunk = &body[starts[ti]..starts[ti + 1]];
            let cursor_out_ref = &cursor_out;
            let cursor_in_ref = &cursor_in;
            let fwd_edges_ref = &fwd_edges;
            let rev_edges_ref = &rev_edges;
            s.spawn(move || {
                let mut p = 0usize;
                while p < chunk.len() {
                    // Skip whitespace / blank lines
                    while p < chunk.len()
                        && (chunk[p] == b' ' || chunk[p] == b'\n' || chunk[p] == b'\r')
                    {
                        p += 1;
                    }
                    if p >= chunk.len() {
                        break;
                    }
                    // Parse u
                    let mut u: u32 = 0;
                    while p < chunk.len() && chunk[p] >= b'0' && chunk[p] <= b'9' {
                        u = u * 10 + (chunk[p] - b'0') as u32;
                        p += 1;
                    }
                    // Skip space(s)
                    while p < chunk.len() && (chunk[p] == b' ' || chunk[p] == b'\r') {
                        p += 1;
                    }
                    // Parse v
                    let mut v: u32 = 0;
                    while p < chunk.len() && chunk[p] >= b'0' && chunk[p] <= b'9' {
                        v = v * 10 + (chunk[p] - b'0') as u32;
                        p += 1;
                    }
                    // Skip space(s)
                    while p < chunk.len() && (chunk[p] == b' ' || chunk[p] == b'\r') {
                        p += 1;
                    }
                    // Parse w
                    let mut w: u32 = 0;
                    while p < chunk.len() && chunk[p] >= b'0' && chunk[p] <= b'9' {
                        w = w * 10 + (chunk[p] - b'0') as u32;
                        p += 1;
                    }
                    // Skip rest of line
                    while p < chunk.len() && chunk[p] != b'\n' {
                        p += 1;
                    }
                    if p < chunk.len() {
                        p += 1; // skip '\n'
                    }

                    let fwd_dest =
                        cursor_out_ref[u as usize].fetch_add(1, Ordering::Relaxed) as usize;
                    let fwd_packed = ((v as u64) << 32) | (w as u64);
                    fwd_edges_ref[fwd_dest].store(fwd_packed, Ordering::Relaxed);

                    let rev_dest =
                        cursor_in_ref[v as usize].fetch_add(1, Ordering::Relaxed) as usize;
                    let rev_packed = ((u as u64) << 32) | (w as u64);
                    rev_edges_ref[rev_dest].store(rev_packed, Ordering::Relaxed);
                }
            });
        }
    });

    drop(cursor_out);
    drop(cursor_in);
    drop(graph_bytes);

    // ── Parse queries ─────────────────────────────────────────────────────────
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
            .max(1)
            .min(q_len);

        let next_query = AtomicUsize::new(0);

        let thread_results: Vec<Vec<(usize, Option<u64>)>> = std::thread::scope(|s| {
            let mut handles = Vec::with_capacity(n_threads);
            for _ in 0..n_threads {
                handles.push(s.spawn(|| {
                    let mut local_results = Vec::new();
                    let mut dist_f = vec![u64::MAX; n];
                    let mut dist_b = vec![u64::MAX; n];
                    let mut touched_f = Vec::new();
                    let mut touched_b = Vec::new();
                    let mut heap_f: BinaryHeap<Reverse<(u64, u32)>> = BinaryHeap::new();
                    let mut heap_b: BinaryHeap<Reverse<(u64, u32)>> = BinaryHeap::new();

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

                        dist_f[qs as usize] = 0;
                        touched_f.push(qs);
                        heap_f.push(Reverse((0, qs)));

                        dist_b[qt as usize] = 0;
                        touched_b.push(qt);
                        heap_b.push(Reverse((0, qt)));

                        let mut mu = u64::MAX;
                        let mut settled_f = 0u64;
                        let mut settled_b = 0u64;

                        loop {
                            // Pop stale entries from the top of both heaps
                            while let Some(&Reverse((d, u))) = heap_f.peek() {
                                if d > dist_f[u as usize] {
                                    heap_f.pop();
                                } else {
                                    break;
                                }
                            }
                            while let Some(&Reverse((d, u))) = heap_b.peek() {
                                if d > dist_b[u as usize] {
                                    heap_b.pop();
                                } else {
                                    break;
                                }
                            }

                            // If either heap is empty, stop
                            let (tf, tb) = match (heap_f.peek(), heap_b.peek()) {
                                (Some(&Reverse((tf, _))), Some(&Reverse((tb, _)))) => (tf, tb),
                                _ => break,
                            };

                            // If tf.saturating_add(tb) >= mu, stop
                            if tf.saturating_add(tb) >= mu {
                                break;
                            }

                            // Expand side that has settled FEWER nodes so far (ties: forward)
                            if settled_f <= settled_b {
                                let Reverse((d, u)) = heap_f.pop().unwrap();
                                settled_f += 1;

                                let start = out_offsets[u as usize] as usize;
                                let end = out_offsets[u as usize + 1] as usize;
                                for edge_idx in start..end {
                                    let x = fwd_edges[edge_idx].load(Ordering::Relaxed);
                                    let v = (x >> 32) as u32;
                                    let w = (x & 0xFFFF_FFFF) as u32;
                                    let nd = d.saturating_add(w as u64);
                                    if nd < dist_f[v as usize] {
                                        if dist_f[v as usize] == u64::MAX {
                                            touched_f.push(v);
                                        }
                                        dist_f[v as usize] = nd;
                                        heap_f.push(Reverse((nd, v)));

                                        let other = dist_b[v as usize];
                                        if other != u64::MAX {
                                            let cand = nd.saturating_add(other);
                                            if cand < mu {
                                                mu = cand;
                                            }
                                        }
                                    }
                                }
                            } else {
                                let Reverse((d, u)) = heap_b.pop().unwrap();
                                settled_b += 1;

                                let start = in_offsets[u as usize] as usize;
                                let end = in_offsets[u as usize + 1] as usize;
                                for edge_idx in start..end {
                                    let x = rev_edges[edge_idx].load(Ordering::Relaxed);
                                    let tail = (x >> 32) as u32;
                                    let w = (x & 0xFFFF_FFFF) as u32;
                                    let nd = d.saturating_add(w as u64);
                                    if nd < dist_b[tail as usize] {
                                        if dist_b[tail as usize] == u64::MAX {
                                            touched_b.push(tail);
                                        }
                                        dist_b[tail as usize] = nd;
                                        heap_b.push(Reverse((nd, tail)));

                                        let other = dist_f[tail as usize];
                                        if other != u64::MAX {
                                            let cand = nd.saturating_add(other);
                                            if cand < mu {
                                                mu = cand;
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        let ans = if mu == u64::MAX { None } else { Some(mu) };

                        heap_f.clear();
                        heap_b.clear();
                        for &u in &touched_f {
                            dist_f[u as usize] = u64::MAX;
                        }
                        touched_f.clear();
                        for &u in &touched_b {
                            dist_b[u as usize] = u64::MAX;
                        }
                        touched_b.clear();

                        local_results.push((idx, ans));
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

    // ── Write output ──────────────────────────────────────────────────────────
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
