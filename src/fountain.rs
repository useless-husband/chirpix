//! Rateless (fountain) code over packets, with unequal error protection.
//!
//! The source is cut into `K` packets of [`T`] bytes. The sender produces
//! an endless sequence of coded packets, each the XOR of some source
//! packets; which ones is a fixed function of the packet's sequence
//! number, so the receiver needs no side information beyond that number.
//!
//! Unequal protection uses *expanding windows* (Sejdinovic et al. 2009):
//! window i is the first `k_i` source packets, k_1 < k_2 < ... = K, and
//! each coded packet is drawn from one window. The first packets of the
//! stream are in every window, so they are recovered first.
//!
//! K is at most a few hundred here, so the decoder does full Gaussian
//! elimination over GF(2) (maximum-likelihood erasure decoding) instead
//! of the belief-propagation "peeling" that large-K LT codes need. A
//! source packet is known as soon as the received equations determine it.

use crate::util::Rng;

/// Payload bytes per packet.
pub const T: usize = 107;
pub const MAX_K: usize = 4000;

/// How coded packets are built from source packets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scheme {
    /// Expanding-window fountain: the scheme this project is about.
    Windowed = 0,
    /// One window covering everything: an ordinary fountain code.
    Flat = 1,
    /// No coding: source packets in order, repeated (a "data carousel").
    Carousel = 2,
}

impl Scheme {
    pub fn from_u8(v: u8) -> Option<Scheme> {
        match v {
            0 => Some(Scheme::Windowed),
            1 => Some(Scheme::Flat),
            2 => Some(Scheme::Carousel),
            _ => None,
        }
    }
}

/// Length of the repeating window schedule, and how many of each
/// `PERIOD` consecutive packets go to windows 1..6.
const PERIOD: usize = 50;
const SHARES: [usize; 6] = [17, 8, 7, 5, 5, 8];
/// Window sizes as fractions of K (the last window is K itself). With the
/// shares above and K = 0.34 x (packets heard in the design time), the
/// windows complete after about 1/15, 1/5, 2/5, 3/5, 4/5 and all of the
/// design time: 5, 15, 30, 45, 60 and 75 s for the default 75 s.
const FRACTIONS: [f64; 5] = [0.045, 0.10, 0.22, 0.36, 0.56];

#[derive(Clone)]
pub struct Plan {
    pub scheme: Scheme,
    pub k: usize,
    /// Window sizes in source packets, increasing; the last equals `k`.
    pub windows: Vec<usize>,
    pattern: Vec<u8>,
    /// For each slot of the pattern: how many earlier slots use the same window.
    same_before: Vec<usize>,
    /// Slots per period for each window.
    per_period: Vec<usize>,
}

/// A window that adds at most this many packets to the one before it is
/// sent plainly, round-robin, instead of as random combinations: with so
/// few unknowns, the one or two extra packets random combinations need
/// would be a large fraction of the window.
const PLAIN_UP_TO: usize = 8;

impl Plan {
    pub fn new(scheme: Scheme, k: usize) -> Plan {
        assert!((1..=MAX_K).contains(&k), "K out of range");
        let mut windows = Vec::new();
        if scheme == Scheme::Windowed {
            for f in FRACTIONS {
                let size = ((k as f64 * f).round() as usize).clamp(1, k);
                if windows.last().is_none_or(|&l| size > l) && size < k {
                    windows.push(size);
                }
            }
        }
        windows.push(k);
        // Spread each window's share evenly over the period (smooth
        // weighted round-robin), so any stretch of the transmission sees
        // the windows in close to their nominal proportions.
        let shares: Vec<usize> = if windows.len() == SHARES.len() {
            SHARES.to_vec()
        } else {
            // Degenerate small K: merge the shares of the missing small windows upward.
            let mut s = vec![0; windows.len()];
            let skip = SHARES.len() - windows.len();
            for (i, &v) in SHARES.iter().enumerate() {
                s[i.saturating_sub(skip)] += v;
            }
            s
        };
        let mut credit = vec![0i64; shares.len()];
        let mut pattern = Vec::with_capacity(PERIOD);
        for _ in 0..PERIOD {
            for (c, &s) in credit.iter_mut().zip(&shares) {
                *c += s as i64;
            }
            let best = (0..shares.len()).max_by_key(|&i| (credit[i], std::cmp::Reverse(i))).unwrap();
            credit[best] -= PERIOD as i64;
            pattern.push(best as u8);
        }
        let mut per_period = vec![0usize; windows.len()];
        let mut same_before = Vec::with_capacity(PERIOD);
        for &p in &pattern {
            same_before.push(per_period[p as usize]);
            per_period[p as usize] += 1;
        }
        Plan {
            scheme,
            k,
            windows,
            pattern,
            same_before,
            per_period,
        }
    }

    /// Which window packet `id` is drawn from.
    pub fn window_of(&self, id: u32) -> usize {
        self.pattern[id as usize % PERIOD] as usize
    }

    /// The set of source packets XORed into coded packet `id`, as a bitset.
    pub fn neighbours(&self, id: u32) -> Vec<u64> {
        let words = self.k.div_ceil(64);
        let mut v = vec![0u64; words];
        let set = |v: &mut Vec<u64>, i: usize| v[i / 64] |= 1 << (i % 64);
        match self.scheme {
            Scheme::Carousel => set(&mut v, id as usize % self.k),
            _ => {
                let wi = self.window_of(id);
                let size = self.windows[wi];
                let below = if wi == 0 { 0 } else { self.windows[wi - 1] };
                if size - below <= PLAIN_UP_TO && self.windows.len() > 1 {
                    // Small layer: its own packets, one at a time, in turn.
                    let slot = id as usize % PERIOD;
                    let ordinal = (id as usize / PERIOD) * self.per_period[wi] + self.same_before[slot];
                    set(&mut v, below + ordinal % (size - below));
                } else {
                    // Dense random combination: each packet of the window with probability 1/2.
                    let mut rng = Rng::new(0x00F0_17A1 ^ ((id as u64) << 20) ^ self.k as u64);
                    for (w, word) in v.iter_mut().enumerate() {
                        let lo = w * 64;
                        if lo >= size {
                            break;
                        }
                        let r = rng.next_u64();
                        *word = if size - lo >= 64 { r } else { r & ((1u64 << (size - lo)) - 1) };
                    }
                    if v.iter().all(|&w| w == 0) {
                        set(&mut v, id as usize % size);
                    }
                }
            }
        }
        v
    }
}

pub struct Encoder {
    pub plan: Plan,
    source: Vec<[u8; T]>,
}

impl Encoder {
    /// Split `data` into packets (zero-padded at the end).
    pub fn new(scheme: Scheme, data: &[u8]) -> Encoder {
        let k = data.len().div_ceil(T).max(1);
        let mut source = vec![[0u8; T]; k];
        for (i, chunk) in data.chunks(T).enumerate() {
            source[i][..chunk.len()].copy_from_slice(chunk);
        }
        Encoder {
            plan: Plan::new(scheme, k),
            source,
        }
    }

    pub fn packet(&self, id: u32) -> [u8; T] {
        let mut out = [0u8; T];
        for (w, &word) in self.plan.neighbours(id).iter().enumerate() {
            let mut bits = word;
            while bits != 0 {
                let i = w * 64 + bits.trailing_zeros() as usize;
                for (o, s) in out.iter_mut().zip(&self.source[i]) {
                    *o ^= s;
                }
                bits &= bits - 1;
            }
        }
        out
    }
}

struct Row {
    mask: Vec<u64>,
    data: [u8; T],
}

/// Incremental Gaussian elimination: the received equations are kept in
/// reduced row-echelon form, one row per pivot column.
pub struct Decoder {
    pub plan: Plan,
    rows: Vec<Option<Row>>,
    rank: usize,
    seen: std::collections::HashSet<u32>,
}

impl Decoder {
    pub fn new(plan: Plan) -> Decoder {
        let rows = (0..plan.k).map(|_| None).collect();
        Decoder {
            plan,
            rows,
            rank: 0,
            seen: Default::default(),
        }
    }

    pub fn rank(&self) -> usize {
        self.rank
    }

    /// Feed one received packet. Returns true if it added information.
    pub fn add(&mut self, id: u32, payload: &[u8; T]) -> bool {
        if self.rank == self.plan.k || !self.seen.insert(id) {
            return false;
        }
        let mut mask = self.plan.neighbours(id);
        let mut data = *payload;
        // Eliminate every pivot already known from the new equation.
        for w in 0..mask.len() {
            let mut bits = mask[w];
            while bits != 0 {
                let col = w * 64 + bits.trailing_zeros() as usize;
                bits &= bits - 1;
                if let Some(row) = &self.rows[col] {
                    for (m, r) in mask.iter_mut().zip(&row.mask) {
                        *m ^= r;
                    }
                    for (d, r) in data.iter_mut().zip(&row.data) {
                        *d ^= r;
                    }
                    // Columns below `col` in this word may have changed; later
                    // ones are re-read from the updated mask.
                    bits = mask[w] & !((2u64 << (col % 64)).wrapping_sub(1));
                }
            }
        }
        let Some(pivot) = mask
            .iter()
            .enumerate()
            .find(|(_, &m)| m != 0)
            .map(|(w, m)| w * 64 + m.trailing_zeros() as usize)
        else {
            return false;
        };
        // Clear the new pivot column from all existing rows.
        for row in self.rows.iter_mut().flatten() {
            if row.mask[pivot / 64] >> (pivot % 64) & 1 == 1 {
                for (m, r) in row.mask.iter_mut().zip(&mask) {
                    *m ^= r;
                }
                for (d, r) in row.data.iter_mut().zip(&data) {
                    *d ^= r;
                }
            }
        }
        self.rows[pivot] = Some(Row { mask, data });
        self.rank += 1;
        true
    }

    fn solved(&self, i: usize) -> bool {
        self.rows[i]
            .as_ref()
            .is_some_and(|r| r.mask.iter().map(|m| m.count_ones()).sum::<u32>() == 1)
    }

    /// Number of source packets recovered so far (anywhere in the stream).
    pub fn solved_count(&self) -> usize {
        (0..self.plan.k).filter(|&i| self.solved(i)).count()
    }

    /// Number of leading source packets recovered: the usable stream prefix.
    pub fn prefix_packets(&self) -> usize {
        (0..self.plan.k).take_while(|&i| self.solved(i)).count()
    }

    pub fn complete(&self) -> bool {
        self.prefix_packets() == self.plan.k
    }

    /// The recovered prefix of the source bytes.
    pub fn prefix_bytes(&self) -> Vec<u8> {
        let n = self.prefix_packets();
        let mut out = Vec::with_capacity(n * T);
        for i in 0..n {
            out.extend_from_slice(&self.rows[i].as_ref().unwrap().data);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(k: usize, seed: u64) -> Vec<u8> {
        let mut rng = Rng::new(seed);
        (0..k * T).map(|_| rng.next_u64() as u8).collect()
    }

    /// Feed packets `start..` with independent loss `p`; return how many
    /// packets were *sent* when each window, and the whole file, decoded.
    fn run(scheme: Scheme, k: usize, start: u32, loss: f64, seed: u64) -> (Vec<usize>, usize) {
        let data = source(k, seed);
        let enc = Encoder::new(scheme, &data);
        let mut dec = Decoder::new(enc.plan.clone());
        let mut rng = Rng::new(seed ^ 0xABCD);
        let mut when = vec![0usize; enc.plan.windows.len()];
        let mut sent = 0;
        loop {
            let id = start + sent as u32;
            sent += 1;
            if rng.f64() < loss {
                continue;
            }
            dec.add(id, &enc.packet(id));
            let prefix = dec.prefix_packets();
            for (i, &wsize) in enc.plan.windows.iter().enumerate() {
                if when[i] == 0 && prefix >= wsize {
                    when[i] = sent;
                }
            }
            if dec.complete() {
                assert_eq!(dec.prefix_bytes(), data, "decoded bytes differ (seed {seed})");
                return (when, sent);
            }
            assert!(sent < 40 * k + 400, "did not decode (seed {seed})");
        }
    }

    #[test]
    fn decodes_exactly_from_any_start_and_any_loss() {
        for (i, &k) in [1usize, 2, 5, 37, 64, 65, 164, 330].iter().enumerate() {
            for scheme in [Scheme::Windowed, Scheme::Flat, Scheme::Carousel] {
                for (j, loss) in [0.0, 0.3, 0.6].iter().enumerate() {
                    let seed = 1000 + (i * 10 + j) as u64;
                    run(scheme, k, (seed * 7919) as u32, *loss, seed);
                }
            }
        }
    }

    #[test]
    fn flat_fountain_needs_about_two_extra_packets() {
        // Random linear combinations over GF(2): the expected overhead is
        // about 1.6 packets regardless of K.
        let mut total = 0;
        let trials = 60;
        for t in 0..trials {
            let (_, sent) = run(Scheme::Flat, 150, t * 1000, 0.0, 2000 + t as u64);
            total += sent - 150;
        }
        let mean = total as f64 / trials as f64;
        assert!(mean < 3.0, "mean overhead {mean} packets");
    }

    #[test]
    fn windows_decode_in_order_and_on_schedule() {
        // K = 85 is what a 75 s QPSK transmission is sized for. Check the
        // design targets in packets sent (lossless channel, any start):
        // the packets a listener has after 5, 15, 30, 45, 60 and 75 s.
        let k = 85;
        let plan = Plan::new(Scheme::Windowed, k);
        assert_eq!(plan.windows, vec![4, 9, 19, 31, 48, 85]);
        let targets = [12usize, 48, 96, 150, 204, 252];
        let trials = 40;
        let mut late = [0usize; 6];
        let mut all: Vec<Vec<usize>> = vec![Vec::new(); 6];
        for t in 0..trials {
            let (when, _) = run(Scheme::Windowed, k, t * 977 + 13, 0.0, 3000 + t as u64);
            assert!(when.windows(2).all(|p| p[0] <= p[1]), "windows out of order: {when:?}");
            for i in 0..6 {
                late[i] += (when[i] > targets[i]) as usize;
                all[i].push(when[i]);
            }
        }
        for a in all.iter_mut() {
            a.sort();
            eprintln!("window decode slot: min {} median {} max {}", a[0], a[a.len() / 2], a[a.len() - 1]);
        }
        // Random combinations sometimes need a few packets more than the
        // minimum, so allow a fraction of late runs.
        eprintln!("late counts {late:?} of {trials}");
        for i in 0..6 {
            assert!(
                late[i] * 4 <= trials as usize,
                "window {} late in {}/{} runs",
                i + 1,
                late[i],
                trials
            );
        }
    }

    #[test]
    fn loss_delays_but_never_prevents_progress() {
        let k = 85;
        let (w0, s0) = run(Scheme::Windowed, k, 5, 0.0, 4000);
        let (w3, s3) = run(Scheme::Windowed, k, 5, 0.3, 4000);
        assert!(s3 > s0 && w3[1] >= w0[1]);
        // With 30% loss everything should arrive in roughly 1/0.7 the time.
        assert!((s3 as f64) < s0 as f64 / 0.7 * 1.25, "{s0} -> {s3}");
    }

    #[test]
    fn duplicate_and_redundant_packets_are_ignored() {
        let data = source(10, 5);
        let enc = Encoder::new(Scheme::Flat, &data);
        let mut dec = Decoder::new(enc.plan.clone());
        assert!(dec.add(3, &enc.packet(3)));
        assert!(!dec.add(3, &enc.packet(3)));
        for id in 0..40 {
            dec.add(id, &enc.packet(id));
        }
        assert!(dec.complete());
        assert_eq!(dec.rank(), 10);
        assert!(!dec.add(99, &enc.packet(99)));
    }
}
