use std::collections::HashMap;
use std::env;
use std::fs::File;
use std::hash::{BuildHasherDefault, Hasher};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const BOM: &[u8] = b"\xef\xbb\xbf";
const UNMATCHED: &str = "_UNMATCHED";
const MAX_REGION: usize = 1_000_000;
const BUF_SIZE: usize = 1024 * 1024;

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

type NameDictionary = HashMap<Vec<u8>, u32, BuildHasherDefault<MyHasher>>;

#[derive(Default)]
struct CountryStats {
    count: u64,
    id_sum: u64,
    amount_sum: i64,
    amount_min: Option<i64>,
    amount_max: Option<i64>,
    name_bits: Vec<u64>,
    name_bytes: u64,
    note_bytes: u64,
}

impl CountryStats {
    #[inline]
    fn add(&mut self, id: u64, name_id: u32, name_len: u64, note_len: u64, cents: i64) {
        self.count += 1;
        self.id_sum += id;
        self.amount_sum += cents;
        self.amount_min = Some(match self.amount_min {
            Some(min) => min.min(cents),
            None => cents,
        });
        self.amount_max = Some(match self.amount_max {
            Some(max) => max.max(cents),
            None => cents,
        });
        self.name_bytes += name_len;
        self.note_bytes += note_len;

        let word_idx = (name_id / 64) as usize;
        let bit_mask = 1u64 << (name_id % 64);
        if word_idx >= self.name_bits.len() {
            self.name_bits.resize(word_idx + 1, 0);
        }
        self.name_bits[word_idx] |= bit_mask;
    }

    fn distinct_names(&self) -> u64 {
        self.name_bits.iter().map(|w| w.count_ones() as u64).sum()
    }
}

fn format_cents(cents: i64) -> String {
    let sign = if cents < 0 { "-" } else { "" };
    let abs_cents = cents.unsigned_abs();
    format!("{}{}.{:02}", sign, abs_cents / 100, abs_cents % 100)
}

struct RegionTable {
    region_to_country: Vec<u16>,
    country_names: Vec<String>,
    unmatched_cid: u16,
}

fn load_regions(path: &Path) -> Result<RegionTable, Box<dyn std::error::Error>> {
    let file = File::open(path)?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines();

    // Check header
    if let Some(header_line) = lines.next() {
        let header = header_line?;
        if header.trim() != "region_id,country" {
            return Err("invalid regions.csv header".into());
        }
    }

    let mut country_names = Vec::new();
    let mut country_to_id = HashMap::new();

    let unmatched_cid = 0u16;
    country_names.push(UNMATCHED.to_string());
    country_to_id.insert(UNMATCHED.to_string(), unmatched_cid);

    let mut region_to_country = vec![unmatched_cid; MAX_REGION];

    for line in lines {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some((id_str, country_str)) = trimmed.split_once(',') {
            let id: u32 = id_str.parse()?;
            let cid = *country_to_id
                .entry(country_str.to_string())
                .or_insert_with(|| {
                    let new_id = country_names.len() as u16;
                    country_names.push(country_str.to_string());
                    new_id
                });
            if (id as usize) < MAX_REGION {
                region_to_country[id as usize] = cid;
            }
        }
    }

    Ok(RegionTable {
        region_to_country,
        country_names,
        unmatched_cid,
    })
}

struct RecordParser<'a> {
    country_stats: Vec<CountryStats>,
    local_names: Vec<Vec<u8>>,
    names_dict: NameDictionary,
    scratch_name: Vec<u8>,
    regions: &'a RegionTable,
}

impl<'a> RecordParser<'a> {
    fn new(regions: &'a RegionTable) -> Self {
        let country_stats: Vec<CountryStats> = (0..regions.country_names.len())
            .map(|_| CountryStats::default())
            .collect();
        let local_names: Vec<Vec<u8>> = Vec::new();
        let names_dict: NameDictionary = HashMap::default();
        let scratch_name: Vec<u8> = Vec::with_capacity(64);

        Self {
            country_stats,
            local_names,
            names_dict,
            scratch_name,
            regions,
        }
    }

    fn parse_block(&mut self, data: &[u8]) {
        let mut pos = 0;
        let len = data.len();

        while pos < len {
            // Skip BOM at start of record if present
            if data[pos..].starts_with(BOM) {
                pos += 3;
            }

            // 1. id (ASCII digits until comma)
            let mut id: u64 = 0;
            while pos < len && data[pos] != b',' {
                if data[pos] >= b'0' && data[pos] <= b'9' {
                    id = id * 10 + (data[pos] - b'0') as u64;
                }
                pos += 1;
            }
            if pos < len && data[pos] == b',' {
                pos += 1;
            }

            // 2. name
            let (name_slice, name_len): (&[u8], u64) = if pos < len && data[pos] == b'"' {
                pos += 1; // skip opening quote
                let start = pos;
                let mut has_quotes = false;
                while pos < len {
                    if data[pos] == b'"' {
                        if pos + 1 < len && data[pos + 1] == b'"' {
                            has_quotes = true;
                            pos += 2;
                        } else {
                            break; // closing quote
                        }
                    } else {
                        pos += 1;
                    }
                }
                let raw_slice = &data[start..pos];
                if pos < len && data[pos] == b'"' {
                    pos += 1; // skip closing quote
                }
                if has_quotes {
                    self.scratch_name.clear();
                    let mut i = 0;
                    while i < raw_slice.len() {
                        if raw_slice[i] == b'"'
                            && i + 1 < raw_slice.len()
                            && raw_slice[i + 1] == b'"'
                        {
                            self.scratch_name.push(b'"');
                            i += 2;
                        } else {
                            self.scratch_name.push(raw_slice[i]);
                            i += 1;
                        }
                    }
                    (&self.scratch_name[..], self.scratch_name.len() as u64)
                } else {
                    (raw_slice, raw_slice.len() as u64)
                }
            } else {
                // unquoted empty or value until comma
                let start = pos;
                while pos < len && data[pos] != b',' {
                    pos += 1;
                }
                let raw_slice = &data[start..pos];
                (raw_slice, raw_slice.len() as u64)
            };
            if pos < len && data[pos] == b',' {
                pos += 1;
            }

            let name_id = if let Some(&nid) = self.names_dict.get(name_slice) {
                nid
            } else {
                let nid = self.local_names.len() as u32;
                let vec_name = name_slice.to_vec();
                self.names_dict.insert(vec_name.clone(), nid);
                self.local_names.push(vec_name);
                nid
            };

            // 3. note
            let mut note_len: u64 = 0;
            if pos < len && data[pos] == b'"' {
                pos += 1; // skip opening quote
                while pos < len {
                    if data[pos] == b'"' {
                        if pos + 1 < len && data[pos + 1] == b'"' {
                            note_len += 1;
                            pos += 2;
                        } else {
                            pos += 1; // closing quote
                            break;
                        }
                    } else {
                        note_len += 1;
                        pos += 1;
                    }
                }
            } else {
                // unquoted empty or value until comma
                while pos < len && data[pos] != b',' {
                    note_len += 1;
                    pos += 1;
                }
            }
            if pos < len && data[pos] == b',' {
                pos += 1;
            }

            // 4. region_id (ASCII digits until comma)
            let mut region_id: u32 = 0;
            while pos < len && data[pos] != b',' {
                if data[pos] >= b'0' && data[pos] <= b'9' {
                    region_id = region_id * 10 + (data[pos] - b'0') as u32;
                }
                pos += 1;
            }
            if pos < len && data[pos] == b',' {
                pos += 1;
            }

            // 5. amount (fixed-point cents)
            let negative = if pos < len && data[pos] == b'-' {
                pos += 1;
                true
            } else {
                false
            };
            let mut whole: i64 = 0;
            while pos < len && data[pos] >= b'0' && data[pos] <= b'9' {
                whole = whole * 10 + (data[pos] - b'0') as i64;
                pos += 1;
            }
            if pos < len && data[pos] == b'.' {
                pos += 1;
            }
            let mut frac: i64 = 0;
            let mut frac_digits = 0;
            while pos < len && data[pos] >= b'0' && data[pos] <= b'9' && frac_digits < 2 {
                frac = frac * 10 + (data[pos] - b'0') as i64;
                pos += 1;
                frac_digits += 1;
            }
            let mut cents = whole * 100 + frac;
            if negative {
                cents = -cents;
            }

            // Skip to end of record (CRLF or LF or EOF)
            if pos < len && data[pos] == b'\r' {
                pos += 1;
            }
            if pos < len && data[pos] == b'\n' {
                pos += 1;
            }

            // Direct table lookup for country index
            let cid = if (region_id as usize) < MAX_REGION {
                self.regions.region_to_country[region_id as usize]
            } else {
                self.regions.unmatched_cid
            };
            self.country_stats[cid as usize].add(id, name_id, name_len, note_len, cents);
        }
    }

    fn finish(self) -> (Vec<CountryStats>, Vec<Vec<u8>>) {
        (self.country_stats, self.local_names)
    }
}

fn stream_chunk(
    path: &Path,
    start: u64,
    len: u64,
    buf_size: usize,
    regions: &RegionTable,
) -> Result<(Vec<CountryStats>, Vec<Vec<u8>>), std::io::Error> {
    let mut parser = RecordParser::new(regions);
    if len == 0 {
        return Ok(parser.finish());
    }

    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(start))?;
    let mut reader = file.take(len);

    let initial_cap = buf_size.max(64);
    let mut buf = vec![0u8; initial_cap];
    let mut filled = 0;

    loop {
        if filled == buf.len() {
            buf.resize(buf.len() * 2, 0);
        }
        let k = reader.read(&mut buf[filled..])?;
        if k == 0 {
            break;
        }
        filled += k;

        // Find the cut
        let q = buf[..filled].iter().filter(|&&b| b == b'"').count();
        let mut par = (q & 1) as u8;
        let mut cut = None;
        for idx in (0..filled).rev() {
            let b = buf[idx];
            let par_before = if b == b'"' { par ^ 1 } else { par };
            if b == b'\n' && par_before == 0 {
                cut = Some(idx + 1);
                break;
            }
            par = par_before;
        }

        if let Some(cut) = cut {
            parser.parse_block(&buf[..cut]);
            buf.copy_within(cut..filled, 0);
            filled -= cut;
        }
    }

    if filled > 0 {
        parser.parse_block(&buf[..filled]);
    }

    Ok(parser.finish())
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

    if positionals.len() >= 1 {
        data_dir = PathBuf::from(positionals[0]);
    }
    if positionals.len() >= 2 {
        out_dir = PathBuf::from(positionals[1]);
    }

    (data_dir, out_dir)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (data_dir, out_dir) = parse_args();

    let regions_path = data_dir.join("regions.csv");
    let records_path = data_dir.join("records.csv");

    let regions = load_regions(&regions_path)?;
    let len = std::fs::metadata(&records_path)?.len();

    let body_start = if len == 0 {
        0
    } else {
        let mut file = File::open(&records_path)?;
        let mut header_buf = [0u8; 4096];
        let to_read = (len as usize).min(header_buf.len());
        file.read_exact(&mut header_buf[..to_read])?;
        let header_slice = &header_buf[..to_read];

        let mut pos = 0;
        if header_slice.starts_with(BOM) {
            pos += 3;
        }
        match header_slice[pos..].iter().position(|&b| b == b'\n') {
            Some(idx) => (pos + idx + 1) as u64,
            None => len,
        }
    };

    let body_len = len.saturating_sub(body_start);

    let p_avail = std::thread::available_parallelism()
        .map(|m| m.get())
        .unwrap_or(1);
    let n = if body_len < 1_000_000 { 1 } else { p_avail };

    let mut p = Vec::with_capacity(n + 1);
    for i in 0..=n {
        p.push(body_start + (i as u64) * body_len / (n as u64));
    }

    // PHASE 1 (parallel): count b'"' in each segment [p_i, p_{i+1})
    let quote_counts: Vec<usize> = if n <= 1 {
        vec![0]
    } else {
        std::thread::scope(|s| {
            let mut handles = Vec::with_capacity(n);
            for i in 0..n {
                let start = p[i];
                let count_len = p[i + 1] - p[i];
                let path = &records_path;
                handles.push(s.spawn(move || -> Result<usize, std::io::Error> {
                    let mut file = File::open(path)?;
                    file.seek(SeekFrom::Start(start))?;
                    let mut reader = file.take(count_len);
                    let mut buf = vec![0u8; BUF_SIZE];
                    let mut total_quotes = 0;
                    loop {
                        let k = reader.read(&mut buf)?;
                        if k == 0 {
                            break;
                        }
                        total_quotes += buf[..k].iter().filter(|&&b| b == b'"').count();
                    }
                    Ok(total_quotes)
                }));
            }
            handles
                .into_iter()
                .map(|h| h.join().unwrap().unwrap())
                .collect()
        })
    };

    // Prefix-XOR parities: parity_at[i] = quotes in body[0..p_i] mod 2
    let mut parity_at = vec![0u8; n + 1];
    for i in 0..n {
        parity_at[i + 1] = parity_at[i] ^ ((quote_counts[i] & 1) as u8);
    }

    // PHASE 2 (main thread, serial): find record boundaries for chunk starts
    let mut starts = vec![0u64; n + 1];
    starts[0] = body_start;
    starts[n] = len;

    if n > 1 {
        let mut file = File::open(&records_path)?;
        for i in 1..n {
            let p_i = p[i];
            if p_i >= len {
                starts[i] = len;
                continue;
            }
            let mut parity = parity_at[i];
            file.seek(SeekFrom::Start(p_i))?;
            let mut scan_buf = [0u8; 4096];
            let mut found = false;
            let mut abs_pos = p_i;
            loop {
                let k = file.read(&mut scan_buf)?;
                if k == 0 {
                    break;
                }
                for j in 0..k {
                    let b = scan_buf[j];
                    if b == b'"' {
                        parity ^= 1;
                    } else if b == b'\n' && parity == 0 {
                        starts[i] = abs_pos + (j as u64) + 1;
                        found = true;
                        break;
                    }
                }
                if found {
                    break;
                }
                abs_pos += k as u64;
            }
            if !found {
                starts[i] = len;
            }
        }
    }

    // Clamp starts non-decreasing
    for i in 1..=n {
        starts[i] = starts[i].max(starts[i - 1]).min(len);
    }

    // PHASE 3 (parallel parsing): parse chunks
    let thread_results: Vec<(Vec<CountryStats>, Vec<Vec<u8>>)> = if n <= 1 {
        vec![stream_chunk(
            &records_path,
            starts[0],
            starts[1] - starts[0],
            BUF_SIZE,
            &regions,
        )?]
    } else {
        std::thread::scope(|s| {
            let mut handles = Vec::with_capacity(n);
            for i in 0..n {
                let start = starts[i];
                let chunk_len = starts[i + 1] - starts[i];
                let path = &records_path;
                let r = &regions;
                handles.push(s.spawn(move || stream_chunk(path, start, chunk_len, BUF_SIZE, r)));
            }
            handles
                .into_iter()
                .map(|h| h.join().unwrap().unwrap())
                .collect()
        })
    };

    // PHASE 4 (merge on main thread)
    let num_countries = regions.country_names.len();
    let mut global_stats: Vec<CountryStats> = (0..num_countries)
        .map(|_| CountryStats::default())
        .collect();
    let mut global_dict: NameDictionary = HashMap::default();

    for (thread_stats, local_names) in thread_results {
        let mut local_to_global: Vec<u32> = Vec::with_capacity(local_names.len());
        for name in local_names {
            let gid = if let Some(&gid) = global_dict.get(&name[..]) {
                gid
            } else {
                let gid = global_dict.len() as u32;
                global_dict.insert(name, gid);
                gid
            };
            local_to_global.push(gid);
        }

        for (cid, t_stats) in thread_stats.into_iter().enumerate() {
            if t_stats.count == 0 {
                continue;
            }
            let g_stats = &mut global_stats[cid];
            g_stats.count += t_stats.count;
            g_stats.id_sum += t_stats.id_sum;
            g_stats.amount_sum += t_stats.amount_sum;
            if let Some(t_min) = t_stats.amount_min {
                g_stats.amount_min = Some(match g_stats.amount_min {
                    Some(cur_min) => cur_min.min(t_min),
                    None => t_min,
                });
            }
            if let Some(t_max) = t_stats.amount_max {
                g_stats.amount_max = Some(match g_stats.amount_max {
                    Some(cur_max) => cur_max.max(t_max),
                    None => t_max,
                });
            }
            g_stats.name_bytes += t_stats.name_bytes;
            g_stats.note_bytes += t_stats.note_bytes;

            for (w_idx, &word) in t_stats.name_bits.iter().enumerate() {
                let mut w = word;
                while w != 0 {
                    let bit = w.trailing_zeros();
                    let local_id = (w_idx * 64 + bit as usize) as u32;
                    let global_id = local_to_global[local_id as usize];

                    let gw_idx = (global_id / 64) as usize;
                    let gbit_mask = 1u64 << (global_id % 64);
                    if gw_idx >= g_stats.name_bits.len() {
                        g_stats.name_bits.resize(gw_idx + 1, 0);
                    }
                    g_stats.name_bits[gw_idx] |= gbit_mask;

                    w &= w - 1;
                }
            }
        }
    }

    let mut active: Vec<(&String, &CountryStats)> = regions
        .country_names
        .iter()
        .zip(global_stats.iter())
        .filter(|(_, stats)| stats.count > 0)
        .collect();
    active.sort_by_key(|(name, _)| *name);

    std::fs::create_dir_all(&out_dir)?;
    let out_file_path = out_dir.join("aggregates.txt");
    let mut out_file = File::create(&out_file_path)?;

    for (key, stats) in active {
        let line = format!(
            "{} {} {} {} {} {} {} {} {}\n",
            key,
            stats.count,
            stats.id_sum,
            format_cents(stats.amount_sum),
            format_cents(stats.amount_min.unwrap_or(0)),
            format_cents(stats.amount_max.unwrap_or(0)),
            stats.distinct_names(),
            stats.name_bytes,
            stats.note_bytes
        );
        out_file.write_all(line.as_bytes())?;
    }

    Ok(())
}
