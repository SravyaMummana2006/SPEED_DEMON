use std::collections::HashMap;
use std::env;
use std::fs::File;
use std::hash::{BuildHasherDefault, Hasher};
use std::io::{BufWriter, Write};
use std::path::PathBuf;

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

fn parse_chunk(data: &[u8]) -> (Vec<Vec<u8>>, Vec<Group>) {
    let len = data.len();
    let mut pos = 0;

    let mut endpoints: Vec<Vec<u8>> = Vec::new();
    let mut endpoint_dict: EndpointDictionary = HashMap::default();

    let mut groups: Vec<Group> = Vec::new();
    let mut group_map: GroupMap = HashMap::default();

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

        let endpoint_id = if let Some(&id) = endpoint_dict.get(endpoint_bytes) {
            id
        } else {
            let id = endpoints.len() as u32;
            let vec_endpoint = endpoint_bytes.to_vec();
            endpoint_dict.insert(vec_endpoint.clone(), id);
            endpoints.push(vec_endpoint);
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
        let group_idx = if let Some(&idx) = group_map.get(&group_key) {
            idx
        } else {
            let idx = groups.len();
            group_map.insert(group_key, idx);
            groups.push(Group {
                hour: hour_key,
                endpoint_id,
                count: 0,
                errors: 0,
                latencies: Vec::new(),
            });
            idx
        };

        let g = &mut groups[group_idx];
        g.count += 1;
        if (500..=599).contains(&status) {
            g.errors += 1;
        }
        g.latencies.push(latency);
    }

    (endpoints, groups)
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
    let input_path = data_dir.join("requests.log");

    let data = std::fs::read(&input_path)?;
    let len = data.len();

    // Thread count n: available_parallelism, fallback 1. If file < 1 MB, n = 1.
    let p = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let n = if len < 1_000_000 { 1 } else { p };

    // Nominal cuts and line boundary alignment
    let mut starts = vec![0; n + 1];
    starts[0] = 0;
    starts[n] = len;

    for i in 1..n {
        let p_i = i * len / n;
        let mut idx = len;
        for pos in p_i..len {
            if data[pos] == b'\n' {
                idx = pos + 1;
                break;
            }
        }
        starts[i] = idx;
    }

    for i in 1..=n {
        starts[i] = starts[i].max(starts[i - 1]).min(len);
    }

    // Parallel parsing with std::thread::scope
    let thread_results: Vec<(Vec<Vec<u8>>, Vec<Group>)> = if n <= 1 {
        vec![parse_chunk(data.as_slice())]
    } else {
        std::thread::scope(|s| {
            let mut handles = Vec::with_capacity(n);
            for i in 0..n {
                let chunk = &data[starts[i]..starts[i + 1]];
                handles.push(s.spawn(move || parse_chunk(chunk)));
            }
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        })
    };

    // Merge on main thread
    let mut global_endpoints: Vec<Vec<u8>> = Vec::new();
    let mut global_endpoint_dict: EndpointDictionary = HashMap::default();

    let mut global_groups: Vec<Group> = Vec::new();
    let mut global_group_map: GroupMap = HashMap::default();

    for (local_endpoints, local_groups) in thread_results {
        let mut local_to_global: Vec<u32> = Vec::with_capacity(local_endpoints.len());
        for ep in local_endpoints {
            let gid = if let Some(&gid) = global_endpoint_dict.get(&ep[..]) {
                gid
            } else {
                let gid = global_endpoints.len() as u32;
                global_endpoint_dict.insert(ep.clone(), gid);
                global_endpoints.push(ep);
                gid
            };
            local_to_global.push(gid);
        }

        for lg in local_groups {
            let global_endpoint_id = local_to_global[lg.endpoint_id as usize];
            let group_key = ((lg.hour as u64) << 32) | (global_endpoint_id as u64);

            let group_idx = if let Some(&idx) = global_group_map.get(&group_key) {
                idx
            } else {
                let idx = global_groups.len();
                global_group_map.insert(group_key, idx);
                global_groups.push(Group {
                    hour: lg.hour,
                    endpoint_id: global_endpoint_id,
                    count: 0,
                    errors: 0,
                    latencies: Vec::new(),
                });
                idx
            };

            let gg = &mut global_groups[group_idx];
            gg.count += lg.count;
            gg.errors += lg.errors;
            gg.latencies.extend(lg.latencies);
        }
    }

    // Sort groups by hour ascending, then endpoint bytes ascending
    global_groups.sort_unstable_by(|a, b| match a.hour.cmp(&b.hour) {
        std::cmp::Ordering::Equal => {
            global_endpoints[a.endpoint_id as usize].cmp(&global_endpoints[b.endpoint_id as usize])
        }
        other => other,
    });

    std::fs::create_dir_all(&out_dir)?;
    let out_file_path = out_dir.join("summary.txt");
    let mut out_file = BufWriter::new(File::create(&out_file_path)?);

    for mut g in global_groups {
        g.latencies.sort_unstable();
        let n = g.latencies.len();
        let rank = |p: usize| (p * n + 99) / 100;
        let p50 = g.latencies[rank(50) - 1];
        let p95 = g.latencies[rank(95) - 1];
        let p99 = g.latencies[rank(99) - 1];
        let error_ppm = (g.errors as u64 * 1_000_000 / g.count as u64) as u32;

        let y = g.hour / 1_000_000;
        let m = (g.hour / 10_000) % 100;
        let d = (g.hour / 100) % 100;
        let h = g.hour % 100;

        let endpoint_str = std::str::from_utf8(&global_endpoints[g.endpoint_id as usize]).unwrap();
        writeln!(
            out_file,
            "{:04}-{:02}-{:02}T{:02}:00:00Z {} {} {} {} {} {} {}",
            y, m, d, h, endpoint_str, g.count, p50, p95, p99, g.errors, error_ppm
        )?;
    }

    out_file.flush()?;
    Ok(())
}
