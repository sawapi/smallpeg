//! Assembling a complete JPEG from quantized components: trellis
//! passes, then the baseline and progressive candidates.

use crate::component::Component;
use crate::huffman::{BitWriter, HuffTable, build_huff_table, emit_dht};
use crate::progressive::encode_progressive_scans;
use crate::quant::ZIGZAG;
use crate::symbols::{category, for_each_symbol, symbol_stats};
use crate::trellis::{estimate_bits, trellis_ac};

#[allow(clippy::too_many_arguments)]
pub(crate) fn encode_document(
    mut comps: Vec<Component>,
    order: &[(usize, usize)],
    qts: &[[u16; 64]],
    width: u16,
    height: u16,
    app_markers: &[u8],
    lambda: f32,
    chroma_lambda_scale: f32,
    freq_gamma: f32,
    progressive: bool,
    lambda_gate: bool,
    deadzone: (f32, f32),
    aq_lite: (f32, f32, f32, f32),
    jfif: bool,
    threads: usize,
) -> Vec<u8> {
    let blocks = comps.iter().map(|c| c.blocks.len()).sum();
    let threads = crate::parallel::threads(threads, blocks);
    // Trellis distortion weights per quant table (zigzag order); gamma 0
    // leaves every weight at exactly 1.0, i.e. plain step units.
    let freq_wts: Vec<[f32; 64]> = qts
        .iter()
        .map(|qt| {
            let mut wv = [1f32; 64];
            if freq_gamma != 0.0 {
                let gm = (qt.iter().skip(1).map(|&q| (q as f32).ln()).sum::<f32>() / 63.0).exp();
                for k in 1..64 {
                    wv[k] = (qt[ZIGZAG[k]] as f32 / gm).powf(freq_gamma);
                }
            }
            wv
        })
        .collect();
    // Pass 1: symbol statistics from plain rounding.
    let n_tables = comps.iter().map(|c| c.table_class).max().unwrap_or(0) * 2 + 2;
    let (freqs, _) = symbol_stats(&comps, order, n_tables, threads);

    // Trellis pass: re-decide every AC coefficient against rate estimates
    // derived from pass 1, then redo the statistics for the final tables.
    let ac_bits: Vec<[f32; 256]> = (0..n_tables / 2)
        .map(|tc| estimate_bits(&freqs[tc * 2 + 1]))
        .collect();
    // Visual-masking multipliers per luma block: activity = pixel-domain
    // AC energy; noise hides in texture, so busy blocks get a larger
    // lambda (one-sided — flat blocks keep the base).
    let (dz_alpha, dz_cap) = deadzone;
    let mults: Vec<Vec<f32>> = comps
        .iter()
        .map(|c| {
            if dz_alpha <= 0.0 || c.table_class != 0 {
                return Vec::new();
            }
            let qt = &qts[c.qt_id as usize];
            let mut logs = vec![0f32; c.raw.len()];
            let mut sum = 0f64;
            for (bi, raw) in c.raw.iter().enumerate() {
                let mut a = 0f32;
                for k in 1..64 {
                    let e = raw[k] * qt[ZIGZAG[k]] as f32;
                    a += e * e;
                }
                logs[bi] = (a + 1.0).ln();
                sum += logs[bi] as f64;
            }
            let mbar = (sum / logs.len() as f64) as f32;
            logs.iter()
                .map(|&m| (dz_alpha * (m - mbar)).exp().clamp(1.0, dz_cap))
                .collect()
        })
        .collect();
    // AQ-lite risk map: flat blocks bordering strong-edge blocks (and the
    // edge blocks themselves) get a reduced lambda.
    let (aq_flat, aq_strong, aq_gain, aq_boost) = aq_lite;
    let aq_mults: Vec<Vec<f32>> = comps
        .iter()
        .map(|c| {
            if aq_gain >= 1.0 || c.table_class != 0 {
                return Vec::new();
            }
            let qt = &qts[c.qt_id as usize];
            let n = c.blocks.len();
            let bh = n / c.bw;
            let mut var = vec![0f32; n];
            for (bi, raw) in c.raw.iter().enumerate() {
                let mut a = 0f32;
                for k in 1..64 {
                    let e = raw[k] * qt[ZIGZAG[k]] as f32;
                    a += e * e;
                }
                var[bi] = a / 64.0;
            }
            let mut m = vec![aq_boost; n];
            for by in 0..bh {
                for bx in 0..c.bw {
                    let bi = by * c.bw + bx;
                    let me_flat = var[bi] < aq_flat;
                    let me_strong = var[bi] > aq_strong;
                    if !me_flat && !me_strong {
                        continue;
                    }
                    let mut hit = false;
                    for dy in -1i32..=1 {
                        for dx in -1i32..=1 {
                            if dy == 0 && dx == 0 {
                                continue;
                            }
                            let (ny, nx) = (by as i32 + dy, bx as i32 + dx);
                            if ny < 0 || nx < 0 || ny >= bh as i32 || nx >= c.bw as i32 {
                                continue;
                            }
                            let nv = var[(ny as usize) * c.bw + nx as usize];
                            if (me_flat && nv > aq_strong) || (me_strong && nv < aq_flat) {
                                hit = true;
                            }
                        }
                    }
                    if hit {
                        m[bi] = aq_gain;
                    }
                }
            }
            m
        })
        .collect();

    let run_trellis = |comps: &mut Vec<Component>, mult: f32| {
        for (ci, c) in comps.iter_mut().enumerate() {
            let bits = &ac_bits[c.table_class];
            let wt = &freq_wts[c.qt_id as usize];
            let lam = lambda
                * mult
                * if c.table_class == 1 {
                    chroma_lambda_scale
                } else {
                    1.0
                };
            let m = &mults[ci];
            // Blocks are independent, so chunks of them on separate threads
            // produce byte-identical output in any interleaving.
            let chunk = c.blocks.len().div_ceil(threads).max(1);
            let (raw, aqm) = (&c.raw, &aq_mults[ci]);
            let chunks = c
                .blocks
                .chunks_mut(chunk)
                .zip(raw.chunks(chunk))
                .enumerate();
            crate::parallel::for_each(chunks, |(t, (bch, rch))| {
                let base = t * chunk;
                for (i, (blk, rw)) in bch.iter_mut().zip(rch).enumerate() {
                    let mut lb = if m.is_empty() { lam } else { lam * m[base + i] };
                    if !aqm.is_empty() {
                        lb *= aqm[base + i];
                    }
                    trellis_ac(rw, blk, bits, lb, wt);
                }
            });
        }
    };

    // Per-image lambda escalation for gradient-like content. Under the
    // fine-grained tables the default lambda is measurably too
    // conservative on smooth gradients (a 2x lambda sits on a strictly
    // better equal-PSNR curve there), while on noise and photos it only
    // trades quality away. The two are separated by where the AC energy
    // lives: gradients concentrate it in the first couple of zigzag
    // positions, noise spreads it across the spectrum.
    let (mut nz, mut e_low, mut e_all) = (0usize, 0f64, 0f64);
    let mut nblocks = 0usize;
    for c in &comps {
        nblocks += c.blocks.len();
        for (blk, raw) in c.blocks.iter().zip(&c.raw) {
            nz += blk[1..].iter().filter(|&&v| v != 0).count();
            for k in 1..64 {
                let e = (raw[k] * raw[k]) as f64;
                e_all += e;
                if k <= 2 {
                    e_low += e;
                }
            }
        }
    }
    let gradient_like =
        lambda_gate && nz as f64 / nblocks as f64 <= 6.0 && e_all > 0.0 && e_low / e_all >= 0.75;
    run_trellis(&mut comps, 1.0);
    if gradient_like {
        // Try 2x; accept if the true pixel-domain AC error stays within
        // ~0.3 dB (the equal-PSNR win on gradient content was validated
        // curve-level with a lambda sweep). Keep a copy of the
        // 1x coefficients so rejection is a memcpy, not a third trellis.
        let dist = |comps: &Vec<Component>| -> f64 {
            let mut d = 0f64;
            for c in comps.iter() {
                let qt = &qts[c.qt_id as usize];
                for (blk, raw) in c.blocks.iter().zip(&c.raw) {
                    for k in 1..64 {
                        let e = (raw[k] - blk[k] as f32) as f64 * qt[ZIGZAG[k]] as f64;
                        d += e * e;
                    }
                }
            }
            d
        };
        let d_base = dist(&comps);
        let saved: Vec<Vec<[i16; 64]>> = comps.iter().map(|c| c.blocks.clone()).collect();
        run_trellis(&mut comps, 2.0);
        let d2 = dist(&comps);
        // 0.3 dB allowance: 10^(0.03) ~= 1.0715.
        if d2 > d_base.max(f64::MIN_POSITIVE) * 1.0715 {
            for (c, blocks) in comps.iter_mut().zip(saved) {
                c.blocks = blocks;
            }
        }
    }
    // DC trellis: the DC coefficients form per-component DPCM chains, so
    // the rate of block i depends on block i-1's choice — a Viterbi pass
    // over candidates {round-1, round, round+1} per block. Rate uses the
    // pass-1 DC statistics; distortion stays in quantization-step units,
    // consistent with the AC trellis.
    {
        let dc_bits: Vec<[f32; 256]> = (0..n_tables / 2)
            .map(|tc| estimate_bits(&freqs[tc * 2]))
            .collect();
        // Chains are per component and independent: solve them in parallel,
        // then write the chosen DC values back.
        let components: Vec<usize> = (0..comps.len()).collect();
        let chosen = crate::parallel::map(threads, &components, |&ci| {
            let lam = lambda
                * if comps[ci].table_class == 1 {
                    chroma_lambda_scale
                } else {
                    1.0
                };
            let bits = &dc_bits[comps[ci].table_class];
            let chain: Vec<usize> = order
                .iter()
                .filter(|&&(c, _)| c == ci)
                .map(|&(_, bi)| bi)
                .collect();
            let rate = |prev: i32, cur: i32| -> f32 {
                let sct = category(cur - prev);
                bits[sct as usize] + sct as f32
            };
            let t0: Vec<f32> = chain.iter().map(|&bi| comps[ci].raw[bi][0]).collect();
            let r0: Vec<i32> = t0.iter().map(|t| t.round() as i32).collect();
            // MCU padding blocks are invisible; their distortion is free.
            let vis: Vec<bool> = chain
                .iter()
                .map(|&bi| bi % comps[ci].bw < comps[ci].aw && bi / comps[ci].bw < comps[ci].ah)
                .collect();
            let cand = |j: usize, k: usize| -> i32 { (r0[j] + k as i32 - 1).clamp(-1023, 1023) };
            let mut back: Vec<[u8; 3]> = vec![[0; 3]; chain.len()];
            let mut cost = [0f32; 3];
            for k in 0..3 {
                let v = cand(0, k);
                let d = t0[0] - v as f32;
                cost[k] = if vis[0] { d * d } else { 0.0 } + lam * rate(0, v);
            }
            for j in 1..chain.len() {
                let mut next = [f32::INFINITY; 3];
                for k in 0..3 {
                    let v = cand(j, k);
                    let d = t0[j] - v as f32;
                    let d2 = if vis[j] { d * d } else { 0.0 };
                    for pk in 0..3 {
                        let c = cost[pk] + d2 + lam * rate(cand(j - 1, pk), v);
                        if c < next[k] {
                            next[k] = c;
                            back[j][k] = pk as u8;
                        }
                    }
                }
                cost = next;
            }
            let mut k = (0..3).min_by(|&a, &b| cost[a].total_cmp(&cost[b])).unwrap();
            let mut dc = vec![0i16; chain.len()];
            for j in (0..chain.len()).rev() {
                dc[j] = cand(j, k) as i16;
                if j > 0 {
                    k = back[j][k] as usize;
                }
            }
            (chain, dc)
        });
        for (ci, (chain, dc)) in chosen.into_iter().enumerate() {
            for (bi, v) in chain.into_iter().zip(dc) {
                comps[ci].blocks[bi][0] = v;
            }
        }
    }

    // The trellis outcome is final; the pre-rounding coefficients are no
    // longer needed, so free them before the entropy stages run.
    for c in comps.iter_mut() {
        c.raw = Vec::new();
    }

    // Headers common to both modes, up to (not including) the SOF marker.
    let mut head = Vec::new();
    head.extend_from_slice(&[0xFF, 0xD8]); // SOI
    // APP0 / JFIF (defined for 1 and 3 components only; four-component
    // files carry an Adobe APP14 in `app_markers` instead, or nothing).
    if jfif {
        head.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x10]);
        head.extend_from_slice(b"JFIF\0");
        head.extend_from_slice(&[0x01, 0x01, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00]);
    }
    // Optional APP1 (EXIF) / APP2 (ICC) segments, pre-rendered.
    head.extend_from_slice(app_markers);
    // DQT
    for (id, qt) in qts.iter().enumerate() {
        head.extend_from_slice(&[0xFF, 0xDB, 0x00, 0x43, id as u8]);
        for i in 0..64 {
            head.push(qt[ZIGZAG[i]] as u8);
        }
    }
    // SOF0 (baseline) / SOF2 (progressive); frame parameters are identical.
    let emit_sof = |out: &mut Vec<u8>, marker: u8| {
        let sof_len = 8 + 3 * comps.len() as u16;
        out.extend_from_slice(&[0xFF, marker]);
        out.extend_from_slice(&sof_len.to_be_bytes());
        out.push(8);
        out.extend_from_slice(&height.to_be_bytes());
        out.extend_from_slice(&width.to_be_bytes());
        out.push(comps.len() as u8);
        for (i, c) in comps.iter().enumerate() {
            out.push(i as u8 + 1);
            out.push((c.h << 4) | c.v);
            out.push(c.qt_id);
        }
    };

    // Baseline candidate: one interleaved scan.
    // Baseline candidate: header and tables first, plus its exact entropy
    // bit count, so the scan data is only written when it can still win.
    let mut base_head = head.clone();
    emit_sof(&mut base_head, 0xC0);
    let (freqs, payload) = symbol_stats(&comps, order, n_tables, threads);
    let tables: Vec<HuffTable> = freqs.iter().map(build_huff_table).collect();
    for (id, t) in tables.iter().enumerate() {
        emit_dht(&mut base_head, (id & 1) as u8, (id / 2) as u8, t);
    }
    let sos_len = 6 + 2 * comps.len() as u16;
    base_head.extend_from_slice(&[0xFF, 0xDA]);
    base_head.extend_from_slice(&sos_len.to_be_bytes());
    base_head.push(comps.len() as u8);
    for (i, c) in comps.iter().enumerate() {
        base_head.push(i as u8 + 1);
        let dc = c.table_class as u8;
        let ac = c.table_class as u8;
        base_head.push((dc << 4) | ac);
    }
    base_head.extend_from_slice(&[0x00, 0x3F, 0x00]);
    let code_bits: u64 = freqs
        .iter()
        .zip(&tables)
        .map(|(f, t)| {
            f.iter()
                .zip(&t.codes)
                .map(|(&n, &(_, len))| n as u64 * len as u64)
                .sum::<u64>()
        })
        .sum();
    // Byte stuffing only adds bytes, so this never exceeds the real length.
    let baseline_min_len = base_head.len() + (code_bits + payload).div_ceil(8) as usize + 2;
    let render_baseline = |out: Vec<u8>| -> Vec<u8> {
        let mut writer = BitWriter::new(out);
        writer
            .out
            .reserve((code_bits + payload).div_ceil(8) as usize * 101 / 100 + 64);
        for_each_symbol(&comps, order, |tbl, sym, n, bits| {
            let (code, len) = tables[tbl].codes[sym as usize];
            writer.put(code, len);
            writer.put(bits, n);
        });
        writer.flush();
        let mut out = writer.out;
        out.extend_from_slice(&[0xFF, 0xD9]); // EOI
        out
    };
    if !progressive {
        return render_baseline(base_head);
    }

    // Progressive candidate: succeeds on photographic content, can lose on
    // very smooth gradients where refinement passes cost more than EOB
    // runs save. The DCT/trellis work is shared, so rendering both entropy
    // streams and keeping the smaller one is cheap scan optimization.
    let mut prog_head = head;
    emit_sof(&mut prog_head, 0xC2);
    let prog = encode_progressive_scans(&comps, order, prog_head, threads);
    if prog.len() <= baseline_min_len {
        return prog;
    }
    let baseline = render_baseline(base_head);
    if prog.len() <= baseline.len() {
        prog
    } else {
        baseline
    }
}
