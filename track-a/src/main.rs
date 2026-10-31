use std::collections::HashMap;
use std::env;
use std::fs::File;
use std::hash::{BuildHasherDefault, Hasher};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

#[derive(Default)]
struct MyHasher(u64);

impl Hasher for MyHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }

    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut hash = self.0;
        let mut i = 0;
        while i + 8 <= bytes.len() {
            let word = u64::from_ne_bytes(bytes[i..i + 8].try_into().unwrap());
            hash = (hash.rotate_left(5) ^ word).wrapping_mul(0x517cc1b727220a95);
            i += 8;
        }
        if i + 4 <= bytes.len() {
            let word = u32::from_ne_bytes(bytes[i..i + 4].try_into().unwrap()) as u64;
            hash = (hash.rotate_left(5) ^ word).wrapping_mul(0x517cc1b727220a95);
            i += 4;
        }
        while i < bytes.len() {
            hash = (hash.rotate_left(5) ^ (bytes[i] as u64)).wrapping_mul(0x517cc1b727220a95);
            i += 1;
        }
        self.0 = hash;
    }
}

type EndpointDictionary = HashMap<Vec<u8>, u32, BuildHasherDefault<MyHasher>>;
type GroupMap = HashMap<u64, usize, BuildHasherDefault<MyHasher>>;

struct Group {
    hour: u32,
    endpoint_id: u32,
    count: u32,
    errors: u32,
    latencies: Vec<u32>,
}

#[inline(always)]
fn parse_2digits(b: &[u8]) -> u32 {
    ((b[0] - b'0') as u32) * 10 + ((b[1] - b'0') as u32)
}

#[inline(always)]
fn parse_4digits(b: &[u8]) -> u32 {
    ((b[0] - b'0') as u32) * 1000
        + ((b[1] - b'0') as u32) * 100
        + ((b[2] - b'0') as u32) * 10
        + ((b[3] - b'0') as u32)
}

struct ChunkParser {
    endpoints: Vec<Vec<u8>>,
    endpoint_dict: EndpointDictionary,
    groups: Vec<Group>,
    group_map: GroupMap,
}

impl ChunkParser {
    fn new() -> Self {
        Self {
            endpoints: Vec::new(),
            endpoint_dict: HashMap::default(),
            groups: Vec::new(),
            group_map: HashMap::default(),
        }
    }

    fn parse_block(&mut self, data: &[u8]) {
        let len = data.len();
        let mut pos = 0;

        while pos < len {
            let line_start = pos;
            while pos < len && data[pos] != b'\n' {
                pos += 1;
            }
            let line = &data[line_start..pos];
            if pos < len && data[pos] == b'\n' {
                pos += 1;
            }
            if line.is_empty() {
                continue;
            }

            // 1. ts (27 bytes: YYYY-MM-DDTHH:MM:SS.ffffffZ)
            let year = parse_4digits(&line[0..4]);
            let month = parse_2digits(&line[5..7]);
            let day = parse_2digits(&line[8..10]);
            let hour = parse_2digits(&line[11..13]);
            let hour_key = year * 1_000_000 + month * 10_000 + day * 100 + hour;

            // 2. method (after 1st space at index 27, ends at 2nd space)
            let mut p2 = 28;
            while p2 < line.len() && line[p2] != b' ' {
                p2 += 1;
            }

            // 3. endpoint (between 2nd space and 3rd space)
            let endpoint_start = p2 + 1;
            let mut p3 = endpoint_start;
            while p3 < line.len() && line[p3] != b' ' {
                p3 += 1;
            }
            let endpoint_bytes = &line[endpoint_start..p3];

            let endpoint_id = if let Some(&id) = self.endpoint_dict.get(endpoint_bytes) {
                id
            } else {
                let id = self.endpoints.len() as u32;
                let vec_endpoint = endpoint_bytes.to_vec();
                self.endpoint_dict.insert(vec_endpoint.clone(), id);
                self.endpoints.push(vec_endpoint);
                id
            };

            // 4. status (between 3rd space and 4th space)
            let status_start = p3 + 1;
            let mut p4 = status_start;
            while p4 < line.len() && line[p4] != b' ' {
                p4 += 1;
            }
            let status_bytes = &line[status_start..p4];
            let status = ((status_bytes[0] - b'0') as u32) * 100
                + ((status_bytes[1] - b'0') as u32) * 10
                + ((status_bytes[2] - b'0') as u32);

            // 5. latency (from 4th space + 1 to line end)
            let latency_bytes = &line[p4 + 1..];
            let mut latency: u32 = 0;
            for &b in latency_bytes {
                if b >= b'0' && b <= b'9' {
                    latency = latency * 10 + (b - b'0') as u32;
                }
            }

            // Group lookup
            let group_key = ((hour_key as u64) << 32) | (endpoint_id as u64);
            let group_idx = if let Some(&idx) = self.group_map.get(&group_key) {
                idx
            } else {
                let idx = self.groups.len();
                self.group_map.insert(group_key, idx);
                self.groups.push(Group {
                    hour: hour_key,
                    endpoint_id,
                    count: 0,
                    errors: 0,
                    latencies: Vec::new(),
                });
                idx
            };

            let g = &mut self.groups[group_idx];
            g.count += 1;
            if (500..=599).contains(&status) {
                g.errors += 1;
            }
            g.latencies.push(latency);
        }
    }

    fn finish(self) -> (Vec<Vec<u8>>, Vec<Group>) {
        (self.endpoints, self.groups)
    }
}

fn stream_chunk(
    path: &Path,
    start: u64,
    chunk_len: u64,
    buf_size: usize,
) -> (Vec<Vec<u8>>, Vec<Group>) {
    if chunk_len == 0 {
        return (Vec::new(), Vec::new());
    }

    let mut file = File::open(path).expect("failed to open input file");
    file.seek(SeekFrom::Start(start)).expect("failed to seek");
    let mut reader = file.take(chunk_len);

    let mut parser = ChunkParser::new();
    let mut buf = vec![0u8; buf_size.max(1)];
    let mut filled = 0;

    loop {
        if filled == buf.len() {
            buf.resize(buf.len() * 2, 0);
        }
        let bytes_read = reader.read(&mut buf[filled..]).expect("failed to read");
        if bytes_read == 0 {
            break;
        }
        filled += bytes_read;

        if let Some(last_nl) = buf[..filled].iter().rposition(|&b| b == b'\n') {
            parser.parse_block(&buf[..=last_nl]);
            let tail_len = filled - (last_nl + 1);
            buf.copy_within(last_nl + 1..filled, 0);
            filled = tail_len;
        }
    }

    if filled > 0 {
        parser.parse_block(&buf[..filled]);
    }

    parser.finish()
}

#[inline(always)]
fn write_u32(buf: &mut Vec<u8>, mut val: u32) {
    if val == 0 {
        buf.push(b'0');
        return;
    }
    let mut tmp = [0u8; 10];
    let mut i = 0;
    while val > 0 {
        tmp[i] = b'0' + (val % 10) as u8;
        val /= 10;
        i += 1;
    }
    while i > 0 {
        i -= 1;
        buf.push(tmp[i]);
    }
}

#[inline(always)]
fn write_date(buf: &mut Vec<u8>, hour_key: u32) {
    let y = hour_key / 1_000_000;
    let m = (hour_key / 10_000) % 100;
    let d = (hour_key / 100) % 100;
    let h = hour_key % 100;

    buf.push(b'0' + (y / 1000) as u8);
    buf.push(b'0' + ((y / 100) % 10) as u8);
    buf.push(b'0' + ((y / 10) % 10) as u8);
    buf.push(b'0' + (y % 10) as u8);
    buf.push(b'-');
    buf.push(b'0' + (m / 10) as u8);
    buf.push(b'0' + (m % 10) as u8);
    buf.push(b'-');
    buf.push(b'0' + (d / 10) as u8);
    buf.push(b'0' + (d % 10) as u8);
    buf.push(b'T');
    buf.push(b'0' + (h / 10) as u8);
    buf.push(b'0' + (h % 10) as u8);
    buf.extend_from_slice(b":00:00Z");
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

struct HourEntry {
    rank: u32,
    count: u32,
    errors: u32,
    latencies: Vec<u32>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (data_dir, out_dir) = parse_args();
    let input_path = data_dir.join("requests.log");

    let len = std::fs::metadata(&input_path)?.len();
    if len == 0 {
        std::fs::create_dir_all(&out_dir)?;
        let out_file_path = out_dir.join("summary.txt");
        File::create(&out_file_path)?;
        return Ok(());
    }

    let buf_size = 1024 * 1024;

    let p = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let n = if len < 1_000_000 { 1 } else { p };

    let mut starts = vec![0u64; n + 1];
    starts[0] = 0;
    starts[n] = len;

    if n > 1 {
        let mut boundary_file = File::open(&input_path)?;
        let mut block = [0u8; 4096];
        for i in 1..n {
            let p_i = (i as u64) * len / (n as u64);
            boundary_file.seek(SeekFrom::Start(p_i))?;
            let mut cur_pos = p_i;
            let mut found_boundary = None;
            while cur_pos < len {
                let bytes_to_read = std::cmp::min(block.len() as u64, len - cur_pos) as usize;
                let bytes_read = boundary_file.read(&mut block[..bytes_to_read])?;
                if bytes_read == 0 {
                    break;
                }
                if let Some(pos) = block[..bytes_read].iter().position(|&b| b == b'\n') {
                    found_boundary = Some(cur_pos + pos as u64 + 1);
                    break;
                }
                cur_pos += bytes_read as u64;
            }
            starts[i] = found_boundary.unwrap_or(len);
        }
    }

    for i in 1..=n {
        starts[i] = starts[i].max(starts[i - 1]).min(len);
    }

    let thread_results: Vec<(Vec<Vec<u8>>, Vec<Group>)> = if n <= 1 {
        vec![stream_chunk(
            &input_path,
            starts[0],
            starts[1] - starts[0],
            buf_size,
        )]
    } else {
        std::thread::scope(|s| {
            let mut handles = Vec::with_capacity(n);
            for i in 0..n {
                let start_i = starts[i];
                let chunk_len = starts[i + 1] - starts[i];
                let path = &input_path;
                handles.push(s.spawn(move || stream_chunk(path, start_i, chunk_len, buf_size)));
            }
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        })
    };

    // ── Step 1: Global endpoints & rank ─────────────────────────────────────
    let mut global_endpoints: Vec<Vec<u8>> = Vec::new();
    let mut global_endpoint_dict: EndpointDictionary = HashMap::default();

    let mut all_local_to_global: Vec<Vec<u32>> = Vec::with_capacity(thread_results.len());
    for (local_endpoints, _) in &thread_results {
        let mut local_to_global: Vec<u32> = Vec::with_capacity(local_endpoints.len());
        for ep in local_endpoints {
            let gid = if let Some(&gid) = global_endpoint_dict.get(&ep[..]) {
                gid
            } else {
                let gid = global_endpoints.len() as u32;
                global_endpoint_dict.insert(ep.clone(), gid);
                global_endpoints.push(ep.clone());
                gid
            };
            local_to_global.push(gid);
        }
        all_local_to_global.push(local_to_global);
    }

    let mut sorted_gids: Vec<u32> = (0..global_endpoints.len() as u32).collect();
    sorted_gids
        .sort_unstable_by(|&a, &b| global_endpoints[a as usize].cmp(&global_endpoints[b as usize]));

    let mut rank = vec![0u32; global_endpoints.len()];
    for (r, &gid) in sorted_gids.iter().enumerate() {
        rank[gid as usize] = r as u32;
    }

    let sorted_endpoints: Vec<Vec<u8>> = sorted_gids
        .iter()
        .map(|&gid| global_endpoints[gid as usize].clone())
        .collect();
    drop(global_endpoints);
    drop(global_endpoint_dict);

    // ── Step 2: Bucket per hour & move groups ───────────────────────────────
    let mut hour_keys: Vec<u32> = Vec::new();
    for (_, groups) in &thread_results {
        for g in groups {
            hour_keys.push(g.hour);
        }
    }
    hour_keys.sort_unstable();
    hour_keys.dedup();

    let num_hours = hour_keys.len();

    let mut raw_buckets: Vec<Vec<HourEntry>> = (0..num_hours).map(|_| Vec::new()).collect();

    for (t_idx, (_local_endpoints, local_groups)) in thread_results.into_iter().enumerate() {
        let local_to_global = &all_local_to_global[t_idx];
        for g in local_groups {
            let gid = local_to_global[g.endpoint_id as usize];
            let r = rank[gid as usize];
            let h_idx = hour_keys.binary_search(&g.hour).unwrap();
            raw_buckets[h_idx].push(HourEntry {
                rank: r,
                count: g.count,
                errors: g.errors,
                latencies: g.latencies,
            });
        }
    }

    let hour_buckets: Vec<Mutex<Vec<HourEntry>>> =
        raw_buckets.into_iter().map(Mutex::new).collect();

    // ── Step 3: Parallel hour bucket processing ─────────────────────────────
    let n_workers = n.min(num_hours).max(1);
    let next_hour = AtomicUsize::new(0);

    let thread_outputs: Vec<Vec<(usize, Vec<u8>)>> = std::thread::scope(|s| {
        let mut handles = Vec::with_capacity(n_workers);
        for _ in 0..n_workers {
            handles.push(s.spawn(|| {
                let mut local_results = Vec::new();
                loop {
                    let h_idx = next_hour.fetch_add(1, Ordering::Relaxed);
                    if h_idx >= num_hours {
                        break;
                    }
                    let hour_key = hour_keys[h_idx];
                    let mut bucket = std::mem::take(&mut *hour_buckets[h_idx].lock().unwrap());

                    // Sort bucket by rank ascending
                    bucket.sort_unstable_by_key(|e| e.rank);

                    let mut hour_buf = Vec::with_capacity(bucket.len() * 70);

                    // Merge entries with equal rank
                    let mut i = 0;
                    while i < bucket.len() {
                        let mut j = i + 1;
                        while j < bucket.len() && bucket[j].rank == bucket[i].rank {
                            j += 1;
                        }

                        let rank_val = bucket[i].rank;
                        let mut count = bucket[i].count;
                        let mut errors = bucket[i].errors;
                        let mut latencies = std::mem::take(&mut bucket[i].latencies);

                        for k in (i + 1)..j {
                            count += bucket[k].count;
                            errors += bucket[k].errors;
                            latencies.append(&mut bucket[k].latencies);
                        }

                        // Sort latencies
                        latencies.sort_unstable();
                        let lat_len = latencies.len();
                        let rank_fn = |p: usize| (p * lat_len + 99) / 100;
                        let p50 = latencies[rank_fn(50) - 1];
                        let p95 = latencies[rank_fn(95) - 1];
                        let p99 = latencies[rank_fn(99) - 1];
                        let error_ppm = (errors as u64 * 1_000_000 / count as u64) as u32;

                        let endpoint_bytes = &sorted_endpoints[rank_val as usize];

                        write_date(&mut hour_buf, hour_key);
                        hour_buf.push(b' ');
                        hour_buf.extend_from_slice(endpoint_bytes);
                        hour_buf.push(b' ');
                        write_u32(&mut hour_buf, count);
                        hour_buf.push(b' ');
                        write_u32(&mut hour_buf, p50);
                        hour_buf.push(b' ');
                        write_u32(&mut hour_buf, p95);
                        hour_buf.push(b' ');
                        write_u32(&mut hour_buf, p99);
                        hour_buf.push(b' ');
                        write_u32(&mut hour_buf, errors);
                        hour_buf.push(b' ');
                        write_u32(&mut hour_buf, error_ppm);
                        hour_buf.push(b'\n');

                        i = j;
                    }

                    local_results.push((h_idx, hour_buf));
                }
                local_results
            }));
        }
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });

    let mut per_hour_buffers: Vec<Vec<u8>> = (0..num_hours).map(|_| Vec::new()).collect();
    for thread_res in thread_outputs {
        for (h_idx, buf) in thread_res {
            per_hour_buffers[h_idx] = buf;
        }
    }

    // ── Step 4: Write per-hour buffers in hour order ─────────────────────────
    std::fs::create_dir_all(&out_dir)?;
    let out_file_path = out_dir.join("summary.txt");
    let mut out_file = File::create(&out_file_path)?;
    for buf in per_hour_buffers {
        out_file.write_all(&buf)?;
    }

    Ok(())
}
