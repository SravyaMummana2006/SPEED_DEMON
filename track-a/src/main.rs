use std::arch::x86_64::*;
use std::fs::{create_dir_all, metadata, File};
use std::io::{Read, Seek, SeekFrom::Start, Write};
use std::path::PathBuf;
use std::sync::atomic::{
    AtomicPtr, AtomicU16, AtomicU64, AtomicUsize,
    Ordering::{Acquire, Relaxed, Release},
};
use std::sync::Mutex;

const D: [&[u8]; 4] = [b"2026-10-30", b"2026-10-31", b"2026-11-01", b"2026-11-02"];

#[repr(C)]
pub struct Slot {
    pub hash: AtomicU64,
    pub ptr: AtomicPtr<u8>,
    pub len: AtomicU16,
}

#[derive(Clone, Copy)]
struct EpCacheEntry {
    eh: u64,
    ptr: *const u8,
    len: u16,
    slot: u16,
}

#[inline(always)]
fn push_val(b: &mut Vec<u8>, mut v: u32) {
    b.push(b' ');
    let (mut buf, mut i) = ([0u8; 12], 12);
    while {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        v > 0
    } {}
    b.extend_from_slice(&buf[i..]);
}

#[inline(always)]
unsafe fn hash_ep(b: &[u8], mut p: usize) -> (u64, usize, usize) {
    let (s, mut h) = (p, 0x243F6A8885A308D3u64);
    loop {
        let w = (b.as_ptr().add(p) as *const u64).read_unaligned();
        let x = w ^ 0x2020202020202020;
        let t = x.wrapping_sub(0x0101010101010101) & !x & 0x8080808080808080;
        if t != 0 {
            let k = (t.trailing_zeros() / 8) as usize;
            let m = if k == 0 {
                0
            } else {
                w & ((1u64 << (8 * k)) - 1)
            };
            h = _mm_crc32_u64(h, m);
            return ((h << 1) | 1, p + k - s, p + k + 1);
        }
        h = _mm_crc32_u64(h, w);
        p += 8;
    }
}

#[inline(always)]
fn get_slot(tab: &[Slot], ep: &Mutex<Vec<(usize, Box<[u8]>)>>, eh: u64, b: &[u8]) -> usize {
    debug_assert!(eh != 0, "hash must be non-zero");
    let mut i = ((eh >> 20) as usize) & 4095;

    // 1. Fast path: contiguous cache-line lookup
    let mut probes = 0;
    while probes < 4096 {
        let slot = unsafe { tab.get_unchecked(i) };
        let v = slot.hash.load(Acquire);
        if v == eh {
            let len = slot.len.load(Relaxed) as usize;
            if len == b.len() {
                let p = slot.ptr.load(Relaxed);
                // SAFETY:
                // 1. Pointer Validity: `p` comes from a valid Box<[u8]>. Non-null and points to `len` bytes.
                // 2. Immutability: Endpoint bytes are never mutated after publication via `slot.hash.store(eh, Release)`.
                // 3. Ownership & Lifetime: Allocation is owned by `ep` vector, which outlives all worker threads.
                // 4. Thread Visibility: `slot.hash.load(Acquire)` synchronizes with `slot.hash.store(Release)`.
                let slice = unsafe { std::slice::from_raw_parts(p, len) };
                if slice == b {
                    return i;
                }
            }
            i = (i + 1) & 4095;
            probes += 1;
        } else if v == 0 {
            break;
        } else {
            i = (i + 1) & 4095;
            probes += 1;
        }
    }

    if probes >= 4096 {
        panic!("endpoint table full");
    }

    // 2. Slow path under lock
    let mut guard = ep.lock().unwrap();
    let mut i = ((eh >> 20) as usize) & 4095;
    let mut probes = 0;
    while probes < 4096 {
        let slot = unsafe { tab.get_unchecked(i) };
        let v = slot.hash.load(Relaxed);
        if v == eh {
            let len = slot.len.load(Relaxed) as usize;
            if len == b.len() {
                let p = slot.ptr.load(Relaxed);
                // SAFETY:
                // 1. Pointer Validity: `p` points to `len` bytes allocated via Box<[u8]> by a previous holder of `ep.lock()`.
                // 2. Immutability: Endpoint bytes are never mutated after creation.
                // 3. Ownership & Lifetime: Allocation is retained inside `ep`, which outlives all worker threads.
                // 4. Synchronization: Mutual exclusion on `ep` guarantees all prior slot initializations are visible.
                let slice = unsafe { std::slice::from_raw_parts(p, len) };
                if slice == b {
                    return i;
                }
            }
            i = (i + 1) & 4095;
            probes += 1;
        } else if v == 0 {
            let boxed: Box<[u8]> = b.to_vec().into_boxed_slice();
            let p = boxed.as_ptr();
            let len = boxed.len() as u16;

            slot.ptr.store(p as *mut u8, Relaxed);
            slot.len.store(len, Relaxed);
            guard.push((i, boxed));
            slot.hash.store(eh, Release);
            return i;
        } else {
            i = (i + 1) & 4095;
            probes += 1;
        }
    }

    panic!("endpoint table full");
}

const CACHE_SIZE: usize = 128;

fn stream_chunk(
    path: &PathBuf,
    s0: usize,
    s1: usize,
    tab: &[Slot],
    ep: &Mutex<Vec<(usize, Box<[u8]>)>>,
) -> ([Vec<u32>; 96], [Vec<u16>; 96]) {
    let mut bk: ([Vec<u32>; 96], [Vec<u16>; 96]) = (
        std::array::from_fn(|_| Vec::new()),
        std::array::from_fn(|_| Vec::new()),
    );
    let mut ep_cache = [EpCacheEntry {
        eh: 0,
        ptr: std::ptr::null(),
        len: 0,
        slot: 0,
    }; CACHE_SIZE];

    if s1 > s0 {
        let mut f = File::open(path).unwrap();
        f.seek(Start(s0 as u64)).unwrap();
        let (mut rdr, mut buf, mut filled) =
            (f.take((s1 - s0) as u64), vec![0u8; (128 * 1024) + 16], 0);
        loop {
            let lim = buf.len() - 16;
            if filled >= lim {
                buf.resize(buf.len() * 2 + 16, 0);
                continue;
            }
            let r = rdr.read(&mut buf[filled..lim]).unwrap();
            if r == 0 {
                break;
            }
            filled += r;
            if let Some(nl) = buf[..filled].iter().rposition(|&b| b == b'\n') {
                let (b, mut p) = (&buf, 0);
                while p <= nl {
                    // SWAR timestamp parse: single 64-bit load at p + 5
                    let w_ts = unsafe { (b.as_ptr().add(p + 5) as *const u64).read_unaligned() };
                    let m = ((w_ts >> 8) & 0xFF) as usize - 48;
                    let d = (((w_ts >> 24) & 0xFF) as usize - 48) * 10
                        + (((w_ts >> 32) & 0xFF) as usize - 48);
                    let hr = (((w_ts >> 48) & 0xFF) as usize - 48) * 10
                        + (((w_ts >> 56) & 0xFF) as usize - 48);
                    let h = ((d + 31 * m) - 30) * 24 + hr;
                    p += 28;

                    // Branchless SWAR method skip
                    let w_m = unsafe { (b.as_ptr().add(p) as *const u64).read_unaligned() };
                    let x_m = w_m ^ 0x2020202020202020;
                    let t_m = x_m.wrapping_sub(0x0101010101010101) & !x_m & 0x8080808080808080;
                    p += (t_m.trailing_zeros() / 8) as usize + 1;

                    let (eh, ep_len, np) = unsafe { hash_ep(b, p) };

                    // Deduplicated collision-safe thread-local direct-mapped cache
                    let c_idx = ((eh >> 4) as usize) & (CACHE_SIZE - 1);
                    let entry = unsafe { *ep_cache.get_unchecked(c_idx) };
                    let ep_slice = unsafe { std::slice::from_raw_parts(b.as_ptr().add(p), ep_len) };
                    let slot = if entry.eh == eh
                        && entry.len as usize == ep_len
                        && unsafe { std::slice::from_raw_parts(entry.ptr, ep_len) } == ep_slice
                    {
                        entry.slot as usize
                    } else {
                        let s = get_slot(tab, ep, eh, ep_slice);
                        let ptr = unsafe { tab.get_unchecked(s).ptr.load(Relaxed) };
                        ep_cache[c_idx] = EpCacheEntry {
                            eh,
                            ptr,
                            len: ep_len as u16,
                            slot: s as u16,
                        };
                        s
                    };

                    p = np + 4;
                    let mut lat = 0u32;
                    while unsafe { *b.as_ptr().add(p) } != b'\n' {
                        lat = lat * 10 + unsafe { *b.as_ptr().add(p) - b'0' } as u32;
                        p += 1;
                    }
                    bk.1[h].push(
                        ((slot as u16) << 1) | (unsafe { *b.as_ptr().add(np) } == b'5') as u16,
                    );
                    bk.0[h].push(lat);
                    p += 1;
                }
                let tail = filled - nl - 1;
                buf.copy_within(nl + 1..filled, 0);
                filled = tail;
            }
        }
    }
    bk
}

fn main() {
    let (mut data, mut out, mut pos, mut args) = ("/data", "/out", 0, std::env::args().skip(1));
    while let Some(a) = args.next() {
        if a == "--data" {
            if let Some(v) = args.next() {
                data = Box::leak(v.into_boxed_str());
            }
        } else if a == "--out" {
            if let Some(v) = args.next() {
                out = Box::leak(v.into_boxed_str());
            }
        } else if !a.starts_with('-') {
            if pos == 0 {
                data = Box::leak(a.into_boxed_str());
            } else {
                out = Box::leak(a.into_boxed_str());
            }
            pos += 1;
        }
    }
    let path = PathBuf::from(data).join("requests.log");
    let len = metadata(&path).map(|m| m.len() as usize).unwrap_or(0);
    let n = std::thread::available_parallelism().map_or(1, |p| p.get());

    let mut starts = vec![0; n + 1];
    starts[n] = len;
    if len > 0 && n > 1 {
        let (mut f, mut blk) = (File::open(&path).unwrap(), [0u8; 4096]);
        for i in 1..n {
            let mut cur = i * len / n;
            f.seek(Start(cur as u64)).unwrap();
            while cur < len && starts[i] == 0 {
                let to_read = 4096.min(len - cur);
                let r = f.read(&mut blk[..to_read]).unwrap();
                if r == 0 {
                    break;
                }
                if let Some(p) = blk[..r].iter().position(|&b| b == b'\n') {
                    starts[i] = cur + p + 1;
                }
                cur += r;
            }
            if starts[i] == 0 {
                starts[i] = len;
            }
        }
    }
    for i in 1..=n {
        starts[i] = starts[i].max(starts[i - 1]).min(len);
    }

    let tab: Vec<Slot> = (0..4096)
        .map(|_| Slot {
            hash: AtomicU64::new(0),
            ptr: AtomicPtr::new(std::ptr::null_mut()),
            len: AtomicU16::new(0),
        })
        .collect();
    let ep = Mutex::new(Vec::with_capacity(2048));
    let bk: Vec<([Vec<u32>; 96], [Vec<u16>; 96])> = std::thread::scope(|s| {
        let mut h = Vec::new();
        for t in 0..n {
            let (s0, s1, p, t_ref, e_ref) = (starts[t], starts[t + 1], &path, &tab, &ep);
            h.push(s.spawn(move || stream_chunk(p, s0, s1, t_ref, e_ref)));
        }
        h.into_iter().map(|h| h.join().unwrap()).collect()
    });

    let mut ep_list = ep.into_inner().unwrap();
    ep_list.sort_unstable_by(|a, b| a.1.cmp(&b.1));
    let num_ep = ep_list.len();
    let (mut rank, mut sorted) = ([0u16; 4096], Vec::with_capacity(num_ep));
    for (r, &(slot, ref b)) in ep_list.iter().enumerate() {
        rank[slot] = r as u16;
        sorted.push(&b[..]);
    }

    let (slots, writer, next) = (
        (0..96).map(|_| Mutex::new(None)).collect::<Vec<_>>(),
        Mutex::new((0, {
            create_dir_all(out).unwrap();
            File::create(PathBuf::from(out).join("summary.txt")).unwrap()
        })),
        AtomicUsize::new(0),
    );

    std::thread::scope(|s| {
        for _ in 0..n {
            s.spawn(|| {
                let mut buf = Vec::with_capacity(32 * 1024);
                let mut ep_buckets: Vec<Vec<u32>> =
                    (0..num_ep).map(|_| Vec::with_capacity(32)).collect();
                let mut ep_errs: Vec<u32> = vec![0; num_ep];

                loop {
                    let h = next.fetch_add(1, Relaxed);
                    if h >= 96 {
                        break;
                    }
                    buf.clear();

                    let total: usize = (0..bk.len()).map(|t| bk[t].0[h].len()).sum();
                    if total > 0 {
                        for b in ep_buckets.iter_mut() {
                            b.clear();
                        }
                        ep_errs.fill(0);

                        for t in 0..bk.len() {
                            let (lats, tags) = (&bk[t].0[h], &bk[t].1[h]);
                            for (&lat, &tag) in lats.iter().zip(tags) {
                                let rk = rank[(tag >> 1) as usize] as usize;
                                ep_buckets[rk].push(lat);
                                ep_errs[rk] += (tag & 1) as u32;
                            }
                        }

                        let mut prefix = [0u8; 21];
                        prefix[..10].copy_from_slice(D[h / 24]);
                        prefix[10] = b'T';
                        prefix[11] = b'0' + (h % 24 / 10) as u8;
                        prefix[12] = b'0' + (h % 24 % 10) as u8;
                        prefix[13..21].copy_from_slice(b":00:00Z ");

                        for rk in 0..num_ep {
                            let bucket = &mut ep_buckets[rk];
                            let cnt = bucket.len() as u32;
                            if cnt == 0 {
                                continue;
                            }

                            let errs = ep_errs[rk];
                            bucket.sort_unstable();
                            let p = |pct: usize| bucket[(pct * cnt as usize + 99) / 100 - 1];

                            buf.extend_from_slice(&prefix);
                            buf.extend_from_slice(sorted[rk]);
                            push_val(&mut buf, cnt);
                            push_val(&mut buf, p(50));
                            push_val(&mut buf, p(95));
                            push_val(&mut buf, p(99));
                            push_val(&mut buf, errs);
                            push_val(&mut buf, (errs as u64 * 1_000_000 / cnt as u64) as u32);
                            buf.push(b'\n');
                        }
                    }

                    *slots[h].lock().unwrap() = Some(buf.clone());
                    let mut w = writer.lock().unwrap();
                    while let Some(b) = (w.0 < 96)
                        .then(|| slots[w.0].lock().unwrap().take())
                        .flatten()
                    {
                        w.1.write_all(&b).unwrap();
                        w.0 += 1;
                    }
                }
            });
        }
    });
}
