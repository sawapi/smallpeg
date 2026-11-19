//! Progressive scan script search and scan emission.

use crate::component::Component;
use crate::huffman::{BitWriter, HuffTable, build_huff_table, emit_dht};
use crate::symbols::{
    SparseAc, ac_band_stats_multi, for_each_ac_band_symbol_sparse,
    for_each_ac_refine_symbol_sparse, for_each_dc_symbol, sparse_ac_par,
};

/// Each Huffman-coded scan carries the symbol statistics the scan search
/// already measured, so emission is a single pass.
enum ScanSpec {
    /// Interleaved DC scan, Ah = 0, point transform `al`.
    DcFirst { al: u8, freqs: Vec<[u32; 256]> },
    /// Interleaved DC refinement: one raw bit per block, no Huffman data.
    DcRefine { al: u8 },
    /// Single-component AC band, Ah = 0, point transform `al`.
    AcFirst {
        ci: usize,
        ss: usize,
        se: usize,
        al: u8,
        freq: [u32; 256],
    },
    /// Single-component AC refinement, Ah = al + 1.
    AcRefine {
        ci: usize,
        ss: usize,
        se: usize,
        al: u8,
        freq: [u32; 256],
    },
}

/// Progressive scan sequence with spectral selection AND successive
/// approximation (the common simple progression): coefficients are
/// sent coarse-first, then refined one bit plane at a time. The wins over
/// baseline come from EOB runs pricing empty blocks across the image, the
/// coarse first scans concentrating symbol statistics, and every scan
/// getting its own freshly optimized Huffman table (DHT redefinition
/// between scans is legal and effectively free).
pub(crate) fn encode_progressive_scans(
    comps: &[Component],
    order: &[(usize, usize)],
    mut out: Vec<u8>,
    threads: usize,
) -> Vec<u8> {
    use ScanSpec::*;
    let threads = crate::parallel::threads(threads);

    // Sparse (CSR) views of the AC coefficients: the scan search and the
    // AC scan emission only ever touch nonzero coefficients, which is what
    // makes the progressive stage cheap (byte-identical to the dense path).
    let sparse: Vec<SparseAc> = comps.iter().map(|c| sparse_ac_par(c, threads)).collect();

    // ---- Scan-script optimization: measure the exact coded size of every
    // candidate (statistics pass + optimal table), pick the cheapest. This
    // is what lets progressive win on smooth content too — no fixed script
    // fits every image.

    // Exact bit cost of one Huffman-coded scan given its symbol stats.
    let table_bits = |freq: &[u32; 256], payload: u64| -> u64 {
        let t = build_huff_table(freq);
        let sym_bits: u64 = freq
            .iter()
            .enumerate()
            .map(|(s, &f)| f as u64 * t.codes[s].1 as u64)
            .sum();
        // DHT (19 + values) and SOS (~12) bytes of header per scan.
        sym_bits + payload + (19 + t.values.len() as u64 + 12) * 8
    };

    // DC: choose the point transform by measured cost. A refinement level
    // costs exactly one raw bit per block plus a scan header.
    let n_dc = comps.iter().map(|c| c.table_class).max().unwrap_or(0) + 1;
    let dc_cost = |al: u8| -> (u64, Vec<[u32; 256]>) {
        let mut freqs = vec![[0u32; 256]; n_dc];
        let mut payload = 0u64;
        for_each_dc_symbol(comps, order, al, |tbl, sym, n, _| {
            freqs[tbl][sym as usize] += 1;
            payload += n as u64;
        });
        let mut bits: u64 = freqs.iter().map(|f| table_bits(f, 0)).sum();
        bits += payload;
        bits += al as u64 * (order.len() as u64 + 12 * 8); // refinement scans
        (bits, freqs)
    };
    // AC per component: band split {no, yes} x point transform {0, 1, 2}.
    // Refinement cost per level is independent of the chain's top, so it
    // is measured once per level.
    let ac_refine_cost = |ci: usize, al: u8| -> (u64, [u32; 256]) {
        let mut freq = [0u32; 256];
        let mut payload = 0u64;
        for_each_ac_refine_symbol_sparse(&sparse[ci], 1, 63, al, |sym, n, _| {
            if let Some(sym) = sym {
                freq[sym as usize] += 1;
            }
            payload += n as u64;
        });
        (table_bits(&freq, payload), freq)
    };

    // All the costs below are independent, so they run as parallel tasks
    // (luma first, as it is the largest). One walk over a component's
    // coefficients can price several bands, so bands are priced in groups.
    const SPLITS: [usize; 3] = [2, 5, 8];
    const BAND_GROUPS: usize = 3;
    let mut bands = vec![(1usize, 63usize)];
    for sp in SPLITS {
        bands.extend([(1, sp), (sp + 1, 63)]);
    }
    let groups: Vec<&[(usize, usize)]> = bands.chunks(bands.len().div_ceil(BAND_GROUPS)).collect();
    enum Task<'a> {
        Bands(usize, &'a [(usize, usize)]),
        Refine(usize, u8),
        Dc(u8),
    }
    #[allow(clippy::large_enum_variant)] // a few dozen short-lived values
    enum Cost {
        Bands(Vec<[([u32; 256], u64); 3]>),
        Refine((u64, [u32; 256])),
        Dc((u64, Vec<[u32; 256]>)),
    }
    let mut tasks = Vec::new();
    for ci in 0..comps.len() {
        tasks.extend(groups.iter().map(|g| Task::Bands(ci, g)));
        tasks.extend([Task::Refine(ci, 0), Task::Refine(ci, 1)]);
    }
    tasks.extend([Task::Dc(0), Task::Dc(1)]);
    let mut costs = crate::parallel::map(threads, &tasks, |task| match *task {
        Task::Bands(ci, bands) => Cost::Bands(ac_band_stats_multi(&sparse[ci], bands)),
        Task::Refine(ci, al) => Cost::Refine(ac_refine_cost(ci, al)),
        Task::Dc(al) => Cost::Dc(dc_cost(al)),
    })
    .into_iter();
    let mut band_stats = Vec::with_capacity(comps.len());
    let mut refine_costs = Vec::with_capacity(comps.len());
    for _ in 0..comps.len() {
        let mut stats = Vec::with_capacity(bands.len());
        for _ in 0..groups.len() {
            match costs.next() {
                Some(Cost::Bands(s)) => stats.extend(s),
                _ => unreachable!("tasks are listed per component"),
            }
        }
        let mut refine = || match costs.next() {
            Some(Cost::Refine(r)) => r,
            _ => unreachable!("tasks are listed per component"),
        };
        let (r0, r1) = (refine(), refine());
        band_stats.push(stats);
        refine_costs.push((r0, r1));
    }
    let mut dc = || match costs.next() {
        Some(Cost::Dc(d)) => d,
        _ => unreachable!("DC tasks are listed last"),
    };
    let (dc0, dc1) = (dc(), dc());
    let (dc_al, dc_freqs) = if dc1.0 < dc0.0 {
        (1u8, dc1.1)
    } else {
        (0, dc0.1)
    };

    let mut scans: Vec<ScanSpec> = vec![DcFirst {
        al: dc_al,
        freqs: dc_freqs,
    }];
    let mut refine_freqs: Vec<[[u32; 256]; 2]> = Vec::with_capacity(comps.len());
    let mut chain_al = vec![0u8; comps.len()];
    for ci in 0..comps.len() {
        let (r0, r1) = refine_costs[ci];
        let refine: [u64; 2] = [r0.0, r1.0];
        refine_freqs.push([r0.1, r1.1]);
        // Candidates: no split, or split the band at sp (low 1..=sp, high
        // sp+1..=63). Where the split lands matters a lot: smooth content
        // wants a tiny dense low band (1..=2) so the high band is almost
        // all EOB runs.
        let mut best = (u64::MAX, None::<usize>, 0u8);
        // One walk per band covers all three point transforms. Stats come
        // in the order of `bands`: (1, 63), then (1, sp) and (sp + 1, 63)
        // for each split.
        let mut stats = std::mem::take(&mut band_stats[ci]).into_iter();
        let full_s = stats.next().unwrap();
        let split_s: Vec<_> = SPLITS
            .iter()
            .map(|&sp| (sp, stats.next().unwrap(), stats.next().unwrap()))
            .collect();
        for al in 0u8..=2 {
            let tail: u64 = (0..al).map(|l| refine[l as usize]).sum();
            let (f, pl) = &full_s[al as usize];
            let full = table_bits(f, *pl) + tail;
            if full < best.0 {
                best = (full, None, al);
            }
            for (sp, lo_s, hi_s) in &split_s {
                let (lf, lp) = &lo_s[al as usize];
                let (hf, hp) = &hi_s[al as usize];
                let split = table_bits(lf, *lp) + table_bits(hf, *hp) + tail;
                if split < best.0 {
                    best = (split, Some(*sp), al);
                }
            }
        }
        let (_, split, al) = best;
        chain_al[ci] = al;
        match split {
            Some(sp) => {
                let (_, lo_s, hi_s) = split_s.iter().find(|s| s.0 == sp).unwrap();
                scans.push(AcFirst {
                    ci,
                    ss: 1,
                    se: sp,
                    al,
                    freq: lo_s[al as usize].0,
                });
                scans.push(AcFirst {
                    ci,
                    ss: sp + 1,
                    se: 63,
                    al,
                    freq: hi_s[al as usize].0,
                });
            }
            None => scans.push(AcFirst {
                ci,
                ss: 1,
                se: 63,
                al,
                freq: full_s[al as usize].0,
            }),
        }
    }
    // Refinement rounds, coarse planes first; DC refinement rides along.
    let max_al = chain_al.iter().copied().max().unwrap_or(0).max(dc_al);
    for level in (0..max_al).rev() {
        if level < dc_al {
            scans.push(DcRefine { al: level });
        }
        for ci in 0..comps.len() {
            if level < chain_al[ci] {
                scans.push(AcRefine {
                    ci,
                    ss: 1,
                    se: 63,
                    al: level,
                    freq: refine_freqs[ci][level as usize],
                });
            }
        }
    }

    let sos_interleaved = |out: &mut Vec<u8>, ah: u8, al: u8| {
        let sos_len = 6 + 2 * comps.len() as u16;
        out.extend_from_slice(&[0xFF, 0xDA]);
        out.extend_from_slice(&sos_len.to_be_bytes());
        out.push(comps.len() as u8);
        for (i, c) in comps.iter().enumerate() {
            out.push(i as u8 + 1);
            out.push((c.table_class as u8) << 4);
        }
        out.extend_from_slice(&[0x00, 0x00, (ah << 4) | al]);
    };

    // Every scan is a self-contained segment (DHT, SOS, byte-aligned entropy
    // data), so they can be written independently and concatenated.
    let segments = crate::parallel::map(threads, &scans, |spec| {
        let mut out = Vec::new();
        match *spec {
            DcFirst { al, ref freqs } => {
                let tables: Vec<HuffTable> = freqs.iter().map(build_huff_table).collect();
                for (dest, t) in tables.iter().enumerate() {
                    emit_dht(&mut out, 0, dest as u8, t);
                }
                sos_interleaved(&mut out, 0, al);
                let mut writer = BitWriter::new(out);
                for_each_dc_symbol(comps, order, al, |tbl, sym, n, bits| {
                    let (code, len) = tables[tbl].codes[sym as usize];
                    writer.put(code, len);
                    writer.put(bits, n);
                });
                writer.flush();
                out = writer.out;
            }
            DcRefine { al } => {
                sos_interleaved(&mut out, al + 1, al);
                let mut writer = BitWriter::new(out);
                for &(ci, bi) in order {
                    let bit = ((comps[ci].blocks[bi][0] as i32) >> al) & 1;
                    writer.put(bit as u16, 1);
                }
                writer.flush();
                out = writer.out;
            }
            AcFirst {
                ci,
                ss,
                se,
                al,
                ref freq,
            } => {
                let t = build_huff_table(freq);
                emit_dht(&mut out, 1, 0, &t);
                out.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x08, 1, ci as u8 + 1, 0x00]);
                out.extend_from_slice(&[ss as u8, se as u8, al]);
                let mut writer = BitWriter::new(out);
                for_each_ac_band_symbol_sparse(&sparse[ci], ss, se, al, |sym, n, bits| {
                    let (code, len) = t.codes[sym as usize];
                    writer.put(code, len);
                    writer.put(bits, n);
                });
                writer.flush();
                out = writer.out;
            }
            AcRefine {
                ci,
                ss,
                se,
                al,
                ref freq,
            } => {
                let t = build_huff_table(freq);
                emit_dht(&mut out, 1, 0, &t);
                out.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x08, 1, ci as u8 + 1, 0x00]);
                out.extend_from_slice(&[ss as u8, se as u8, ((al + 1) << 4) | al]);
                let mut writer = BitWriter::new(out);
                for_each_ac_refine_symbol_sparse(&sparse[ci], ss, se, al, |sym, n, bits| {
                    if let Some(sym) = sym {
                        let (code, len) = t.codes[sym as usize];
                        writer.put(code, len);
                    }
                    writer.put(bits, n);
                });
                writer.flush();
                out = writer.out;
            }
        }
        out
    });
    for segment in segments {
        out.extend_from_slice(&segment);
    }

    out.extend_from_slice(&[0xFF, 0xD9]); // EOI
    out
}
