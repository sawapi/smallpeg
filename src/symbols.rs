//! Symbol streams of baseline and progressive scans, dense and sparse,
//! and the statistics the progressive scan search is built on.

use crate::component::Component;

/// Bit category of a coefficient value (0..=11 for baseline).
#[inline]
pub(crate) fn category(v: i32) -> u8 {
    (32 - v.unsigned_abs().leading_zeros()) as u8
}

/// Amplitude bits: negatives are stored as value-1 in `size` low bits.
#[inline]
fn amplitude(v: i32, size: u8) -> u16 {
    if v < 0 {
        ((v - 1) & ((1i32 << size) - 1)) as u16
    } else {
        v as u16
    }
}

/// Bit k set when coefficient k is nonzero. Built in groups of eight so
/// the compiler can turn each group into one vector compare.
#[inline]
fn nonzero_mask(blk: &[i16; 64]) -> u64 {
    let mut nz = 0u64;
    for (g, chunk) in blk.as_chunks::<8>().0.iter().enumerate() {
        let mut m = 0u8;
        for (j, &v) in chunk.iter().enumerate() {
            m |= ((v != 0) as u8) << j;
        }
        nz |= (m as u64) << (8 * g);
    }
    nz
}

/// Walk every entropy symbol of the scan. `f(table_id, symbol, nbits, bits)`
/// where table_id = table_class * 2 + (0 for DC, 1 for AC).
pub(crate) fn for_each_symbol(
    comps: &[Component],
    order: &[(usize, usize)],
    f: impl FnMut(usize, u8, u8, u16),
) {
    for_each_symbol_from(comps, order, vec![0i32; comps.len()], f);
}

/// Symbol counts per table and the total amplitude bits of
/// `for_each_symbol`, counted over chunks of `order` in parallel. Each chunk
/// starts from the DC predictors left by the blocks before it, so the sums
/// are the same however `order` is split.
pub(crate) fn symbol_stats(
    comps: &[Component],
    order: &[(usize, usize)],
    n_tables: usize,
    threads: usize,
) -> (Vec<[u32; 256]>, u64) {
    let ranges = threads.clamp(1, order.len().max(1));
    if ranges == 1 {
        // One pass, with no walk ahead for the predictors.
        let mut freqs = vec![[0u32; 256]; n_tables];
        let mut payload = 0u64;
        for_each_symbol(comps, order, |tbl, sym, n, _| {
            freqs[tbl][sym as usize] += 1;
            payload += n as u64;
        });
        return (freqs, payload);
    }
    let len = order.len().div_ceil(ranges).max(1);
    let mut starts = Vec::with_capacity(ranges);
    let mut prev_dc = vec![0i32; comps.len()];
    for (i, chunk) in order.chunks(len).enumerate() {
        starts.push((i * len, chunk.len(), prev_dc.clone()));
        for &(ci, bi) in chunk {
            prev_dc[ci] = comps[ci].blocks[bi][0] as i32;
        }
    }
    let parts = crate::parallel::map(threads, &starts, |(start, count, prev)| {
        let mut freqs = vec![[0u32; 256]; n_tables];
        let mut payload = 0u64;
        for_each_symbol_from(
            comps,
            &order[*start..][..*count],
            prev.clone(),
            |tbl, sym, n, _| {
                freqs[tbl][sym as usize] += 1;
                payload += n as u64;
            },
        );
        (freqs, payload)
    });
    let mut freqs = vec![[0u32; 256]; n_tables];
    let mut payload = 0u64;
    for (f, p) in parts {
        for (dst, src) in freqs.iter_mut().zip(&f) {
            for (d, s) in dst.iter_mut().zip(src) {
                *d += s;
            }
        }
        payload += p;
    }
    (freqs, payload)
}

/// `for_each_symbol` over part of the interleave order, starting from the
/// given per-component DC predictors.
fn for_each_symbol_from(
    comps: &[Component],
    order: &[(usize, usize)],
    mut prev_dc: Vec<i32>,
    mut f: impl FnMut(usize, u8, u8, u16),
) {
    for &(ci, bi) in order {
        let c = &comps[ci];
        let blk = &c.blocks[bi];
        let dc_tbl = c.table_class * 2;
        let ac_tbl = c.table_class * 2 + 1;

        let dc = blk[0] as i32;
        let diff = dc - prev_dc[ci];
        prev_dc[ci] = dc;
        let s = category(diff);
        f(dc_tbl, s, s, amplitude(diff, s));

        // Visit nonzero AC positions only: a bitmask built branch-free,
        // then walked with trailing_zeros (most blocks are sparse).
        let mut nz = nonzero_mask(blk) & !1;
        let mut next = 1u32; // first position not yet coded
        while nz != 0 {
            let k = nz.trailing_zeros();
            nz &= nz - 1;
            let mut run = (k - next) as u8;
            while run > 15 {
                f(ac_tbl, 0xF0, 0, 0); // ZRL
                run -= 16;
            }
            let v = blk[k as usize] as i32;
            let s = category(v);
            f(ac_tbl, (run << 4) | s, s, amplitude(v, s));
            next = k + 1;
        }
        if next < 64 {
            f(ac_tbl, 0x00, 0, 0); // EOB
        }
    }
}

/// DC symbols of a progressive first scan with successive-approximation
/// point transform `al` (arithmetic shift, per T.81). `f(table_class,
/// symbol, nbits, bits)`.
pub(crate) fn for_each_dc_symbol(
    comps: &[Component],
    order: &[(usize, usize)],
    al: u8,
    mut f: impl FnMut(usize, u8, u8, u16),
) {
    let mut prev_dc = vec![0i32; comps.len()];
    for &(ci, bi) in order {
        let c = &comps[ci];
        let dc = (c.blocks[bi][0] as i32) >> al;
        let diff = dc - prev_dc[ci];
        prev_dc[ci] = dc;
        let s = category(diff);
        f(c.table_class, s, s, amplitude(diff, s));
    }
}

/// AC symbols of one spectral band, first scan (Ah = 0) with point
/// transform `al`: coefficients enter as sign * (|v| >> al). Blocks whose
/// band is entirely zero accumulate into EOB runs: one EOBn symbol prices
/// a whole run of empty blocks, which is where progressive beats baseline
/// on smooth content.
#[allow(dead_code)] // reference implementation; the sparse walker must match it
fn for_each_ac_band_symbol(
    c: &Component,
    ss: usize,
    se: usize,
    al: u8,
    mut f: impl FnMut(u8, u8, u16),
) {
    let mut eobrun: u32 = 0;
    macro_rules! flush_eobrun {
        () => {
            if eobrun > 0 {
                let r = 31 - eobrun.leading_zeros();
                f((r << 4) as u8, r as u8, (eobrun & ((1 << r) - 1)) as u16);
                #[allow(unused_assignments)]
                {
                    eobrun = 0;
                }
            }
        };
    }
    for by in 0..c.ah {
        for bx in 0..c.aw {
            let blk = &c.blocks[by * c.bw + bx];
            let mut run = 0u32;
            let mut first = true;
            for k in ss..=se {
                let raw = blk[k] as i32;
                let mag = (raw.unsigned_abs() >> al) as i32;
                if mag == 0 {
                    run += 1;
                    continue;
                }
                let v = if raw < 0 { -mag } else { mag };
                if first {
                    flush_eobrun!();
                    first = false;
                }
                while run > 15 {
                    f(0xF0, 0, 0); // ZRL
                    run -= 16;
                }
                let s = category(v);
                f(((run as u8) << 4) | s, s, amplitude(v, s));
                run = 0;
            }
            if run > 0 {
                eobrun += 1;
                if eobrun == 32767 {
                    flush_eobrun!();
                }
            }
        }
    }
    flush_eobrun!();
}

/// AC refinement scan (Ah = al + 1, Al = al), T.81 G.1.2.3: coefficients
/// already significant at Ah get one correction bit; coefficients whose
/// bit `al` is their first nonzero bit are "newly significant" and coded
/// as (run, 1) symbols. Correction bits ride the entropy stream appended
/// to whichever symbol's span crosses their position — that ordering is
/// exactly what the decoder replays, so ZRLs must be emitted the moment a
/// zero-history run hits 16, not deferred.
///
/// `f(Some(sym), nbits, bits)` is a Huffman symbol with its immediate
/// payload; `f(None, 1, bit)` is an appended raw correction bit.
#[allow(dead_code)] // reference implementation; the sparse walker must match it
fn for_each_ac_refine_symbol(
    c: &Component,
    ss: usize,
    se: usize,
    al: u8,
    mut f: impl FnMut(Option<u8>, u8, u16),
) {
    let hi = al + 1;
    let mut eobrun: u32 = 0;
    let mut eob_bits: Vec<u16> = Vec::new();
    macro_rules! flush_eobrun {
        () => {
            if eobrun > 0 {
                let r = 31 - eobrun.leading_zeros();
                f(
                    Some((r << 4) as u8),
                    r as u8,
                    (eobrun & ((1 << r) - 1)) as u16,
                );
                for &b in &eob_bits {
                    f(None, 1, b);
                }
                eob_bits.clear();
                #[allow(unused_assignments)]
                {
                    eobrun = 0;
                }
            }
        };
    }
    for by in 0..c.ah {
        for bx in 0..c.aw {
            let blk = &c.blocks[by * c.bw + bx];
            // Last position that becomes significant in this scan; only
            // spans before it may emit ZRL (a trailing ZRL would desync
            // the decoder's EOB handling).
            let mut last_sig: Option<usize> = None;
            for k in ss..=se {
                let a = (blk[k] as i32).unsigned_abs();
                if (a >> hi) == 0 && (a >> al) != 0 {
                    last_sig = Some(k);
                }
            }
            let tail_from = match last_sig {
                None => ss,
                Some(ls) => {
                    flush_eobrun!();
                    let mut run = 0u32;
                    let mut corr: Vec<u16> = Vec::new();
                    for k in ss..=ls {
                        let raw = blk[k] as i32;
                        let a = raw.unsigned_abs();
                        if (a >> hi) != 0 {
                            corr.push(((a >> al) & 1) as u16);
                        } else if (a >> al) == 0 {
                            run += 1;
                            if run == 16 {
                                f(Some(0xF0), 0, 0);
                                for &b in &corr {
                                    f(None, 1, b);
                                }
                                corr.clear();
                                run = 0;
                            }
                        } else {
                            f(Some(((run as u8) << 4) | 1), 1, if raw > 0 { 1 } else { 0 });
                            for &b in &corr {
                                f(None, 1, b);
                            }
                            corr.clear();
                            run = 0;
                        }
                    }
                    if ls == se {
                        continue; // fully coded, no EOB contribution
                    }
                    ls + 1
                }
            };
            // Remaining positions join the EOB run; their correction bits
            // are buffered and follow the EOBn symbol.
            for k in tail_from..=se {
                let a = (blk[k] as i32).unsigned_abs();
                if (a >> hi) != 0 {
                    eob_bits.push(((a >> al) & 1) as u16);
                }
            }
            eobrun += 1;
            if eobrun == 32767 {
                flush_eobrun!();
            }
        }
    }
    flush_eobrun!();
}

/// CSR sparse view of a component's AC coefficients in scan (by,bx) order:
/// offsets[i]..offsets[i+1] indexes (zigzag position, value) pairs.
pub(crate) struct SparseAc {
    offsets: Vec<u32>,
    entries: Vec<(u8, i16)>,
}

/// Same result as `sparse_ac`, built over bands of block rows in parallel.
pub(crate) fn sparse_ac_par(c: &Component, threads: usize) -> SparseAc {
    let bands = threads.clamp(1, c.ah.max(1));
    if bands == 1 {
        return sparse_ac(c);
    }
    let rows = c.ah.div_ceil(bands);
    let starts: Vec<usize> = (0..c.ah).step_by(rows).collect();
    let parts = crate::parallel::map(threads, &starts, |&first| {
        let mut ends = Vec::with_capacity(rows * c.aw);
        let mut entries = Vec::new();
        for by in first..(first + rows).min(c.ah) {
            for bx in 0..c.aw {
                let blk = &c.blocks[by * c.bw + bx];
                for (k, &v) in blk.iter().enumerate().skip(1) {
                    if v != 0 {
                        entries.push((k as u8, v));
                    }
                }
                ends.push(entries.len() as u32);
            }
        }
        (ends, entries)
    });
    let total = parts.iter().map(|p| p.1.len()).sum();
    let mut offsets = Vec::with_capacity(c.aw * c.ah + 1);
    let mut entries = Vec::with_capacity(total);
    offsets.push(0u32);
    for (ends, part) in parts {
        let base = entries.len() as u32;
        offsets.extend(ends.iter().map(|&e| base + e));
        entries.extend_from_slice(&part);
    }
    SparseAc { offsets, entries }
}

pub(crate) fn sparse_ac(c: &Component) -> SparseAc {
    let n = c.aw * c.ah;
    let mut offsets = Vec::with_capacity(n + 1);
    let mut entries = Vec::new();
    offsets.push(0u32);
    for by in 0..c.ah {
        for bx in 0..c.aw {
            let blk = &c.blocks[by * c.bw + bx];
            for (k, &v) in blk.iter().enumerate().skip(1) {
                if v != 0 {
                    entries.push((k as u8, v));
                }
            }
            offsets.push(entries.len() as u32);
        }
    }
    SparseAc { offsets, entries }
}

/// Sparse equivalent of `for_each_ac_band_symbol`: same symbol stream,
/// but touches only nonzero coefficients.
pub(crate) fn for_each_ac_band_symbol_sparse(
    s: &SparseAc,
    ss: usize,
    se: usize,
    al: u8,
    mut f: impl FnMut(u8, u8, u16),
) {
    let mut eobrun: u32 = 0;
    macro_rules! flush_eobrun {
        () => {
            if eobrun > 0 {
                let r = 31 - eobrun.leading_zeros();
                f((r << 4) as u8, r as u8, (eobrun & ((1 << r) - 1)) as u16);
                #[allow(unused_assignments)]
                {
                    eobrun = 0;
                }
            }
        };
    }
    for w in s.offsets.windows(2) {
        let ents = &s.entries[w[0] as usize..w[1] as usize];
        let mut last = ss - 1; // position of last emitted nonzero
        let mut first = true;
        for &(kp, v) in ents {
            let k = kp as usize;
            if k < ss || k > se {
                continue;
            }
            let mag = ((v as i32).unsigned_abs() >> al) as i32;
            if mag == 0 {
                continue;
            }
            let vv = if v < 0 { -mag } else { mag };
            if first {
                flush_eobrun!();
                first = false;
            }
            let mut run = (k - last - 1) as u32;
            while run > 15 {
                f(0xF0, 0, 0); // ZRL
                run -= 16;
            }
            let sz = category(vv);
            f(((run as u8) << 4) | sz, sz, amplitude(vv, sz));
            last = k;
        }
        if last < se {
            eobrun += 1;
            if eobrun == 32767 {
                flush_eobrun!();
            }
        }
    }
    flush_eobrun!();
}

/// Sparse equivalent of `for_each_ac_refine_symbol`.
pub(crate) fn for_each_ac_refine_symbol_sparse(
    s: &SparseAc,
    ss: usize,
    se: usize,
    al: u8,
    mut f: impl FnMut(Option<u8>, u8, u16),
) {
    let hi = al + 1;
    let mut eobrun: u32 = 0;
    let mut eob_bits: Vec<u16> = Vec::new();
    macro_rules! flush_eobrun {
        () => {
            if eobrun > 0 {
                let r = 31 - eobrun.leading_zeros();
                f(
                    Some((r << 4) as u8),
                    r as u8,
                    (eobrun & ((1 << r) - 1)) as u16,
                );
                for &b in &eob_bits {
                    f(None, 1, b);
                }
                eob_bits.clear();
                #[allow(unused_assignments)]
                {
                    eobrun = 0;
                }
            }
        };
    }
    // Scratch buffers reused across blocks (cleared per block).
    let mut ents: Vec<(usize, i32)> = Vec::with_capacity(64);
    let mut corr: Vec<u16> = Vec::with_capacity(64);
    for w in s.offsets.windows(2) {
        ents.clear();
        ents.extend(
            s.entries[w[0] as usize..w[1] as usize]
                .iter()
                .filter(|&&(k, _)| (k as usize) >= ss && (k as usize) <= se)
                .map(|&(k, v)| (k as usize, v as i32)),
        );
        let mut last_sig: Option<usize> = None;
        for &(k, v) in &ents {
            let a = v.unsigned_abs();
            if (a >> hi) == 0 && (a >> al) != 0 {
                last_sig = Some(k);
            }
        }
        let tail_from = match last_sig {
            None => ss,
            Some(ls) => {
                flush_eobrun!();
                let mut run = 0u32;
                corr.clear();
                let mut pos = ss; // next position to account for
                for &(k, v) in ents.iter().filter(|&&(k, _)| k <= ls) {
                    let a = v.unsigned_abs();
                    // true zeros in the gap [pos, k)
                    let mut gap = (k - pos) as u32;
                    while gap > 0 {
                        let take = gap.min(16 - run);
                        run += take;
                        gap -= take;
                        if run == 16 {
                            f(Some(0xF0), 0, 0);
                            for &b in &corr {
                                f(None, 1, b);
                            }
                            corr.clear();
                            run = 0;
                        }
                    }
                    pos = k + 1;
                    if (a >> hi) != 0 {
                        corr.push(((a >> al) & 1) as u16);
                    } else if (a >> al) == 0 {
                        run += 1;
                        if run == 16 {
                            f(Some(0xF0), 0, 0);
                            for &b in &corr {
                                f(None, 1, b);
                            }
                            corr.clear();
                            run = 0;
                        }
                    } else {
                        f(Some(((run as u8) << 4) | 1), 1, if v > 0 { 1 } else { 0 });
                        for &b in &corr {
                            f(None, 1, b);
                        }
                        corr.clear();
                        run = 0;
                    }
                }
                if ls == se {
                    continue;
                }
                ls + 1
            }
        };
        for &(_, v) in ents.iter().filter(|&&(k, _)| k >= tail_from) {
            let a = v.unsigned_abs();
            if (a >> hi) != 0 {
                eob_bits.push(((a >> al) & 1) as u16);
            }
        }
        eobrun += 1;
        if eobrun == 32767 {
            flush_eobrun!();
        }
    }
    flush_eobrun!();
}

/// Cost statistics of one band for al = 0, 1, 2 in a single sparse pass:
/// returns per-al (symbol freq table, amplitude-bit payload). Exactly the
/// same counts as three separate `for_each_ac_band_symbol_sparse` passes.
#[cfg(test)]
fn ac_band_stats_all_al(s: &SparseAc, ss: usize, se: usize) -> [([u32; 256], u64); 3] {
    ac_band_stats_multi(s, &[(ss, se)]).pop().unwrap()
}

/// `ac_band_stats_all_al` for several bands in one walk over the blocks.
pub(crate) fn ac_band_stats_multi(
    s: &SparseAc,
    bands: &[(usize, usize)],
) -> Vec<[([u32; 256], u64); 3]> {
    struct St {
        freq: [u32; 256],
        payload: u64,
        eobrun: u32,
    }
    #[inline]
    fn flush(st: &mut St) {
        if st.eobrun > 0 {
            let r = 31 - st.eobrun.leading_zeros();
            st.freq[(r << 4) as usize] += 1;
            st.payload += r as u64;
            st.eobrun = 0;
        }
    }
    let nb = bands.len();
    let mut st: Vec<[St; 3]> = (0..nb)
        .map(|_| {
            std::array::from_fn(|_| St {
                freq: [0; 256],
                payload: 0,
                eobrun: 0,
            })
        })
        .collect();
    let mut last = vec![[0usize; 3]; nb];
    let mut first = vec![[true; 3]; nb];
    for w in s.offsets.windows(2) {
        let ents = &s.entries[w[0] as usize..w[1] as usize];
        for (b, &(ss, _)) in bands.iter().enumerate() {
            last[b] = [ss - 1; 3];
            first[b] = [true; 3];
        }
        for &(kp, v) in ents {
            let k = kp as usize;
            let a = (v as i32).unsigned_abs();
            for (b, &(ss, se)) in bands.iter().enumerate() {
                if k < ss || k > se {
                    continue;
                }
                for al in 0..3usize {
                    let mag = (a >> al) as i32;
                    if mag == 0 {
                        break; // larger shifts are zero too
                    }
                    let stx = &mut st[b][al];
                    if first[b][al] {
                        flush(stx);
                        first[b][al] = false;
                    }
                    let mut run = (k - last[b][al] - 1) as u32;
                    while run > 15 {
                        stx.freq[0xF0] += 1;
                        run -= 16;
                    }
                    let sz = category(mag);
                    stx.freq[((run as usize) << 4) | sz as usize] += 1;
                    stx.payload += sz as u64;
                    last[b][al] = k;
                }
            }
        }
        for (b, &(_, se)) in bands.iter().enumerate() {
            for al in 0..3usize {
                if last[b][al] < se {
                    let stx = &mut st[b][al];
                    stx.eobrun += 1;
                    if stx.eobrun == 32767 {
                        flush(stx);
                    }
                }
            }
        }
    }
    st.into_iter()
        .map(|mut sts| {
            for stx in sts.iter_mut() {
                flush(stx);
            }
            sts.map(|x| (x.freq, x.payload))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sparse walkers must emit exactly the dense walkers' symbol
    /// streams. Adversarial coefficient soup: empty blocks in long runs
    /// (EOBRUN cap territory), 16+ zero gaps (ZRL boundaries), small
    /// magnitudes that vanish under the point transform, history-significant
    /// vs newly-significant mixes, and a padded block grid (bw > aw).
    #[test]
    fn sparse_walkers_match_dense() {
        let (aw, ah, bw) = (210usize, 160usize, 213usize);
        let mut seed = 0x5eed_cafe_u64;
        let mut rnd = move || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as u32
        };
        let mut blocks = vec![[0i16; 64]; bw * ah];
        for by in 0..ah {
            for bx in 0..aw {
                let blk = &mut blocks[by * bw + bx];
                match rnd() % 10 {
                    0..=4 => {} // empty (long EOB runs)
                    5 | 6 => {
                        // sparse small values, wide gaps
                        for _ in 0..(1 + rnd() % 3) {
                            let k = 1 + (rnd() % 63) as usize;
                            blk[k] = [1, -1, 2, -2, 3, -3][(rnd() % 6) as usize];
                        }
                    }
                    7 | 8 => {
                        for _ in 0..(4 + rnd() % 12) {
                            let k = 1 + (rnd() % 63) as usize;
                            blk[k] = (rnd() % 4000) as i16 - 2000;
                        }
                    }
                    _ => {
                        // tail-heavy: nonzero at se and beyond ZRL gaps
                        blk[63] = 5;
                        blk[1 + (rnd() % 20) as usize] = -7;
                    }
                }
                // padding columns stay whatever; they must be ignored
            }
        }
        let c = Component {
            blocks,
            raw: Vec::new(),
            table_class: 0,
            bw,
            aw,
            ah,
            h: 1,
            v: 1,
            qt_id: 0,
            e_low: 0.0,
            e_all: 0.0,
            energy: false,
        };
        let s = sparse_ac(&c);
        for &(ss, se) in &[(1usize, 63usize), (1, 5), (6, 63), (1, 2), (3, 63)] {
            for al in 0u8..=2 {
                let mut dense = Vec::new();
                for_each_ac_band_symbol(&c, ss, se, al, |a, b, cc| dense.push((a, b, cc)));
                let mut sp = Vec::new();
                for_each_ac_band_symbol_sparse(&s, ss, se, al, |a, b, cc| sp.push((a, b, cc)));
                assert_eq!(dense, sp, "band ss={ss} se={se} al={al}");
            }
            for al in 0u8..=1 {
                let mut dense = Vec::new();
                for_each_ac_refine_symbol(&c, ss, se, al, |a, b, cc| dense.push((a, b, cc)));
                let mut sp = Vec::new();
                for_each_ac_refine_symbol_sparse(&s, ss, se, al, |a, b, cc| sp.push((a, b, cc)));
                assert_eq!(dense, sp, "refine ss={ss} se={se} al={al}");
            }
        }
        // fused stats must equal three separate passes
        for &(ss, se) in &[(1usize, 63usize), (1, 8), (9, 63)] {
            let all = ac_band_stats_all_al(&s, ss, se);
            for al in 0u8..=2 {
                let mut freq = [0u32; 256];
                let mut payload = 0u64;
                for_each_ac_band_symbol_sparse(&s, ss, se, al, |sym, n, _| {
                    freq[sym as usize] += 1;
                    payload += n as u64;
                });
                assert_eq!(all[al as usize].0, freq, "fused freq al={al}");
                assert_eq!(all[al as usize].1, payload, "fused payload al={al}");
            }
        }
    }
}
