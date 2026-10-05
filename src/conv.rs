//! Rate-1/2 convolutional code, constraint length 7, generators 133/171
//! (octal), with a soft-decision Viterbi decoder.
//!
//! Each block is terminated with six zero bits so the decoder starts and
//! ends in the all-zero state.

pub const TAIL: usize = 6;
const G0: u32 = 0o133;
const G1: u32 = 0o171;
const STATES: usize = 64;

#[inline]
fn outputs(reg: u32) -> (u8, u8) {
    (((reg & G0).count_ones() & 1) as u8, ((reg & G1).count_ones() & 1) as u8)
}

/// Encode `bits` (values 0/1) plus the tail: returns 2 * (len + 6) bits.
pub fn encode(bits: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 * (bits.len() + TAIL));
    let mut state = 0u32;
    for &b in bits.iter().chain([0u8; TAIL].iter()) {
        let reg = ((b as u32) << 6) | state;
        let (a, c) = outputs(reg);
        out.push(a);
        out.push(c);
        state = reg >> 1;
    }
    out
}

/// Maximum-likelihood sequence decoder. `llr[i] > 0` means coded bit i is
/// more likely 0; the magnitude is the confidence. Returns `nbits` bits.
pub fn viterbi(llr: &[f32], nbits: usize) -> Vec<u8> {
    let steps = nbits + TAIL;
    assert_eq!(llr.len(), 2 * steps, "LLR count must be 2 * (nbits + tail)");
    let mut out_tab = [(0u8, 0u8); 128];
    for (reg, e) in out_tab.iter_mut().enumerate() {
        *e = outputs(reg as u32);
    }
    const NEG: f32 = -1e30;
    let mut metric = [NEG; STATES];
    metric[0] = 0.0;
    let mut decisions = vec![0u64; steps];
    for t in 0..steps {
        let (l0, l1) = (llr[2 * t], llr[2 * t + 1]);
        // Branch metric for each of the four output pairs.
        let bm = [l0 + l1, l0 - l1, -l0 + l1, -l0 - l1];
        let mut next = [NEG; STATES];
        let mut dec = 0u64;
        let limit = if t < nbits { STATES } else { STATES / 2 }; // tail forces input 0
        for (ns, nm) in next.iter_mut().enumerate().take(limit) {
            let bit = (ns >> 5) as u32;
            let s0 = (ns & 31) << 1;
            let mut best = NEG;
            let mut which = 0u64;
            for lsb in 0..2usize {
                let s = s0 | lsb;
                let (a, c) = out_tab[((bit as usize) << 6) | s];
                let m = metric[s] + bm[(a as usize) << 1 | c as usize];
                if m > best {
                    best = m;
                    which = lsb as u64;
                }
            }
            *nm = best;
            dec |= which << ns;
        }
        decisions[t] = dec;
        metric = next;
    }
    let mut bits = vec![0u8; steps];
    let mut state = 0usize;
    for t in (0..steps).rev() {
        bits[t] = (state >> 5) as u8;
        let lsb = ((decisions[t] >> state) & 1) as usize;
        state = ((state & 31) << 1) | lsb;
    }
    bits.truncate(nbits);
    bits
}

/// Union bound on the bit error rate after soft Viterbi decoding of this
/// code with antipodal signalling in white Gaussian noise. Uses the
/// published information-weight spectrum (d = 10, 12, ..., 20).
pub fn union_bound_ber(ebn0_db: f64) -> f64 {
    const CD: [(f64, f64); 6] = [
        (10.0, 36.0),
        (12.0, 211.0),
        (14.0, 1404.0),
        (16.0, 11633.0),
        (18.0, 77433.0),
        (20.0, 502_690.0),
    ];
    let ebn0 = 10f64.powf(ebn0_db / 10.0);
    CD.iter()
        .map(|&(d, c)| c * crate::util::q_func((2.0 * d * 0.5 * ebn0).sqrt()))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::Rng;

    fn to_llr(coded: &[u8]) -> Vec<f32> {
        coded.iter().map(|&c| if c == 0 { 1.0 } else { -1.0 }).collect()
    }

    #[test]
    fn noiseless_round_trip() {
        let mut rng = Rng::new(61);
        for n in [1usize, 2, 7, 64, 890] {
            let bits: Vec<u8> = (0..n).map(|_| rng.bit()).collect();
            let coded = encode(&bits);
            assert_eq!(coded.len(), 2 * (n + TAIL));
            assert_eq!(viterbi(&to_llr(&coded), n), bits);
        }
    }

    #[test]
    fn free_distance_is_ten() {
        // The lightest non-zero codeword of the (133,171) code has weight 10.
        let mut min_w = usize::MAX;
        for m in 1u32..(1 << 12) {
            let bits: Vec<u8> = (0..12).map(|i| ((m >> i) & 1) as u8).collect();
            let w = encode(&bits).iter().filter(|&&b| b == 1).count();
            min_w = min_w.min(w);
        }
        assert_eq!(min_w, 10);
    }

    #[test]
    fn viterbi_is_maximum_likelihood_on_exhaustive_small_cases() {
        // For 8-bit messages compare against brute force over all 256 codewords.
        let mut rng = Rng::new(62);
        let n = 8;
        let book: Vec<Vec<u8>> = (0..256u32)
            .map(|m| encode(&(0..n).map(|i| ((m >> i) & 1) as u8).collect::<Vec<_>>()))
            .collect();
        for trial in 0..400 {
            let llr: Vec<f32> = (0..2 * (n + TAIL)).map(|_| rng.gauss() as f32).collect();
            let score = |cw: &[u8]| -> f32 { cw.iter().zip(&llr).map(|(&c, &l)| if c == 0 { l } else { -l }).sum() };
            let best = book.iter().map(|cw| score(cw)).fold(f32::MIN, f32::max);
            let got = viterbi(&llr, n);
            let got_score = score(&encode(&got));
            assert!((got_score - best).abs() < 1e-3, "trial {trial}: {got_score} vs ML {best}");
        }
    }

    #[test]
    fn corrects_any_four_hard_errors() {
        let mut rng = Rng::new(63);
        for _ in 0..300 {
            let bits: Vec<u8> = (0..120).map(|_| rng.bit()).collect();
            let mut coded = encode(&bits);
            let mut hit = std::collections::HashSet::new();
            while hit.len() < 4 {
                hit.insert(rng.below(coded.len()));
            }
            for &i in &hit {
                coded[i] ^= 1;
            }
            assert_eq!(viterbi(&to_llr(&coded), 120), bits);
        }
    }

    #[test]
    fn ber_in_gaussian_noise_respects_the_union_bound() {
        // Simulate BPSK in white noise and compare with the union bound,
        // which is an upper bound and is tight at these error rates.
        let mut rng = Rng::new(64);
        for (ebn0_db, blocks) in [(3.5, 4000)] {
            let sigma = (1.0 / (2.0 * 0.5 * 10f64.powf(ebn0_db / 10.0))).sqrt();
            let (mut errs, mut total) = (0usize, 0usize);
            for _ in 0..blocks {
                let bits: Vec<u8> = (0..1000).map(|_| rng.bit()).collect();
                let llr: Vec<f32> = encode(&bits)
                    .iter()
                    .map(|&c| ((1.0 - 2.0 * c as f64) + sigma * rng.gauss()) as f32)
                    .collect();
                let dec = viterbi(&llr, 1000);
                errs += dec.iter().zip(&bits).filter(|(a, b)| a != b).count();
                total += 1000;
            }
            let ber = errs as f64 / total as f64;
            let bound = union_bound_ber(ebn0_db);
            eprintln!("Eb/N0 {ebn0_db} dB: BER {ber:.2e}, union bound {bound:.2e}");
            assert!(
                ber < bound * 1.35 && ber > bound / 6.0,
                "Eb/N0 {ebn0_db}: BER {ber:.2e} vs bound {bound:.2e}"
            );
        }
    }
}
