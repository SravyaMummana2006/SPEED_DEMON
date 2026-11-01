use std::arch::x86_64::*;
use std::fs::{create_dir_all, metadata, File};
use std::io::{Read, Seek, SeekFrom::Start, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::Mutex;

const D: [&[u8]; 4] = [b"2026-10-30", b"2026-10-31", b"2026-11-01", b"2026-11-02"];

#[inline(always)]
fn push_val(b: &mut Vec<u8>, mut v: u32) {
    b.push(b' ');
    let s = b.len();
    while { b.push(b'0' + (v % 10) as u8); v /= 10; v > 0 } {}
    b[s..].reverse();
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
            let m = if k == 0 { 0 } else { w & ((1u64 << (8 * k)) - 1) };
            h = _mm_crc32_u64(h, m);
            return ((h << 1) | 1, p + k - s, p + k + 1);
        }
        h = _mm_crc32_u64(h, w);
        p += 8;
    }
}

#[inline(always)]
fn get_slot(tab: &[AtomicU64], ep: &Mutex<Vec<(usize, Vec<u8>)>>, eh: u64, b: &[u8]) -> usize {
    let mut i = ((eh >> 20) as usize) & 4095;
    loop {
        let slot = unsafe { tab.get_unchecked(i) };
        match slot.load(Relaxed) {
            v if v == eh => return i,
            0 => match slot.compare_exchange(0, eh, Relaxed, Relaxed) {
                Ok(_) => { ep.lock().unwrap().push((i, b.to_vec())); return i; }
                Err(v) if v == eh => return i,
                _ => i = (i + 1) & 4095,
            },
            _ => i = (i + 1) & 4095,
        }
    }
}

fn stream_chunk(
    path: &PathBuf, s0: usize, s1: usize, tab: &[AtomicU64], ep: &Mutex<Vec<(usize, Vec<u8>)>>,
) -> ([Vec<u32>; 96], [Vec<u16>; 96]) {
    let mut bk: ([Vec<u32>; 96], [Vec<u16>; 96]) = (std::array::from_fn(|_| Vec::new()), std::array::from_fn(|_| Vec::new()));
    let (mut last_eh, mut last_slot) = (0u64, 0usize);
    if s1 > s0 {
        let mut f = File::open(path).unwrap();
        f.seek(Start(s0 as u64)).unwrap();
        let (mut rdr, mut buf, mut filled) = (f.take((s1 - s0) as u64), vec![0u8; (64 * 1024) + 16], 0);
        loop {
            let lim = buf.len() - 16;
            if filled >= lim { buf.resize(buf.len() * 2 + 16, 0); continue; }
            let r = rdr.read(&mut buf[filled..lim]).unwrap();
            if r == 0 { break; }
            filled += r;
            if let Some(nl) = buf[..filled].iter().rposition(|&b| b == b'\n') {
                let (b, mut p) = (&buf, 0);
                while p <= nl {
                    let d = unsafe { (*b.as_ptr().add(p + 8) - b'0') * 10 + *b.as_ptr().add(p + 9) - b'0' } as usize;
                    let m = unsafe { *b.as_ptr().add(p + 6) - b'0' } as usize;
                    let h = ((d + 31 * m) - 30) * 24 + unsafe { (*b.as_ptr().add(p + 11) - b'0') * 10 + *b.as_ptr().add(p + 12) - b'0' } as usize;
                    p += 28;
                    p = if unsafe { *b.as_ptr().add(p + 3) } == b' ' { p + 4 }
                        else if unsafe { *b.as_ptr().add(p + 4) } == b' ' { p + 5 }
                        else if unsafe { *b.as_ptr().add(p + 7) } == b' ' { p + 8 }
                        else { while unsafe { *b.as_ptr().add(p) } != b' ' { p += 1; } p + 1 };
                    let (eh, ep_len, np) = unsafe { hash_ep(b, p) };
                    let slot = if eh == last_eh { last_slot } else {
                        let s = get_slot(tab, ep, eh, unsafe { std::slice::from_raw_parts(b.as_ptr().add(p), ep_len) });
                        last_eh = eh; last_slot = s; s
                    };
                    p = np + 4;
                    let mut lat = 0u32;
                    while unsafe { *b.as_ptr().add(p) } != b'\n' { lat = lat * 10 + unsafe { *b.as_ptr().add(p) - b'0' } as u32; p += 1; }
                    bk.1[h].push(((slot as u16) << 1) | (unsafe { *b.as_ptr().add(np) } == b'5') as u16);
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
        if a == "--data" { if let Some(v) = args.next() { data = Box::leak(v.into_boxed_str()); } }
        else if a == "--out" { if let Some(v) = args.next() { out = Box::leak(v.into_boxed_str()); } }
        else if !a.starts_with('-') {
            if pos == 0 { data = Box::leak(a.into_boxed_str()); }
            else { out = Box::leak(a.into_boxed_str()); }
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
                let to_read = blk.len().min(len - cur);
                let r = f.read(&mut blk[..to_read]).unwrap();
                if r == 0 { break; }
                if let Some(p) = blk[..r].iter().position(|&b| b == b'\n') { starts[i] = cur + p + 1; }
                cur += r;
            }
            if starts[i] == 0 { starts[i] = len; }
        }
    }
    for i in 1..=n { starts[i] = starts[i].max(starts[i - 1]).min(len); }

    let (tab, ep) = ((0..4096).map(|_| AtomicU64::new(0)).collect::<Vec<_>>(), Mutex::new(Vec::new()));
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
    let (mut rank, mut sorted) = ([0u16; 4096], Vec::with_capacity(ep_list.len()));
    for (r, &(slot, ref b)) in ep_list.iter().enumerate() { rank[slot] = r as u16; sorted.push(b); }

    let (slots, writer, next) = (
        (0..96).map(|_| Mutex::new(None)).collect::<Vec<_>>(),
        Mutex::new((0, { create_dir_all(out).unwrap(); File::create(PathBuf::from(out).join("summary.txt")).unwrap() })),
        AtomicUsize::new(0),
    );

    std::thread::scope(|s| {
        for _ in 0..n {
            s.spawn(|| loop {
                let h = next.fetch_add(1, Relaxed);
                if h >= 96 { break; }
                let total: usize = (0..bk.len()).map(|t| bk[t].0[h].len()).sum();
                if total == 0 {
                    *slots[h].lock().unwrap() = Some(Vec::new());
                    let mut w = writer.lock().unwrap();
                    while let Some(b) = (w.0 < 96).then(|| slots[w.0].lock().unwrap().take()).flatten() {
                        w.1.write_all(&b).unwrap();
                        w.0 += 1;
                    }
                    continue;
                }
                let mut keys = Vec::with_capacity(total);
                for t in 0..bk.len() {
                    let (l, tg) = (&bk[t].0[h], &bk[t].1[h]);
                    for (&lat, &tag) in l.iter().zip(tg) {
                        keys.push(((rank[(tag >> 1) as usize] as u64) << 33) | ((lat as u64) << 1) | (tag & 1) as u64);
                    }
                }
                keys.sort_unstable();
                let (mut buf, mut i) = (Vec::with_capacity(32 * 1024), 0);
                while i < keys.len() {
                    let (rk, mut j) = ((keys[i] >> 33) as usize, i + 1);
                    while j < keys.len() && (keys[j] >> 33) == rk as u64 { j += 1; }
                    let (cnt, errs) = ((j - i) as u32, keys[i..j].iter().map(|k| (k & 1) as u32).sum::<u32>());
                    let p = |pct: usize| ((keys[i + (pct * cnt as usize + 99) / 100 - 1] >> 1) & 0xFFFFFFFF) as u32;
                    buf.extend_from_slice(D[h / 24]);
                    buf.push(b'T');
                    buf.push(b'0' + (h % 24 / 10) as u8);
                    buf.push(b'0' + (h % 24 % 10) as u8);
                    buf.extend_from_slice(b":00:00Z ");
                    buf.extend_from_slice(sorted[rk]);
                    push_val(&mut buf, cnt);
                    push_val(&mut buf, p(50));
                    push_val(&mut buf, p(95));
                    push_val(&mut buf, p(99));
                    push_val(&mut buf, errs);
                    push_val(&mut buf, (errs as u64 * 1_000_000 / cnt as u64) as u32);
                    buf.push(b'\n');
                    i = j;
                }
                *slots[h].lock().unwrap() = Some(buf);
                let mut w = writer.lock().unwrap();
                while let Some(b) = (w.0 < 96).then(|| slots[w.0].lock().unwrap().take()).flatten() {
                    w.1.write_all(&b).unwrap();
                    w.0 += 1;
                }
            });
        }
    });
}
