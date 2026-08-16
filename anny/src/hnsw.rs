use crate::traits::{Distance, Metric, Scalar};
use std::cell::RefCell;
use std::cmp::Ordering;
use std::mem::MaybeUninit;

// child of this module so it can reach Hnsw's private fields
#[path = "serial.rs"]
mod serial;

const fn const_ln(x: f64) -> f64 {
    let (mut m, mut k) = (x, 0i32);
    while m >= 2.0 {
        m /= 2.0;
        k += 1;
    }
    while m < 1.0 {
        m *= 2.0;
        k -= 1;
    }
    let s = (m - 1.0) / (m + 1.0);
    let s2 = s * s;
    let (mut term, mut sum, mut n, mut i) = (s, 0.0, 1.0, 0);
    while i < 16 {
        sum += term / n;
        term *= s2;
        n += 2.0;
        i += 1;
    }
    2.0 * sum + (k as f64) * std::f64::consts::LN_2
}

const NONE: u32 = u32::MAX;

struct NodeMeta {
    level: u8,
    upper: u32, // arena offset, or NONE if level == 0
    alive: bool,
}

// ---- non-generic, thread-local visited buffer (the expensive N-sized one) ----
thread_local! {
    static VISITED: RefCell<(Vec<u32>, u32)> = const { RefCell::new((Vec::new(), 0)) };
}

// ordered candidate; ordered externally by Distance::cmp_total so floats work.
#[derive(Clone, Copy)]
struct Item<D: Distance>(D, u32);

// ------------------------------------------------------------------------
// Frontier: a fixed-capacity, stack-resident search frontier.
//
// `items[..len]` is kept sorted ascending by distance and acts as BOTH the
// candidate set and the ef-best result set (the two-heap formulation collapses
// to one bounded list: anything that would fall outside the best-N is exactly
// what the old `cand.top > res.worst` break would have skipped, so dropping it
// is equivalent). `done[i]` marks an expanded slot; `front` is a lower-bound
// hint for the nearest unexpanded slot.
//
// Storage is MaybeUninit because D has no Default; every slot in [..len] is
// always initialized, and D: Copy means there is nothing to drop on reset.
// ------------------------------------------------------------------------
struct Frontier<D: Distance, const N: usize> {
    items: [MaybeUninit<Item<D>>; N],
    done: [bool; N],
    len: usize,
    front: usize,
}

impl<D: Distance, const N: usize> Frontier<D, N> {
    fn new() -> Self {
        Self {
            items: [MaybeUninit::uninit(); N],
            done: [false; N],
            len: 0,
            front: 0,
        }
    }

    #[inline]
    fn reset(&mut self) {
        self.len = 0;
        self.front = 0;
    }

    // sorted-ascending view of the live candidates
    #[inline]
    fn items(&self) -> &[Item<D>] {
        // SAFETY: [..len] are all initialized; Item<D>: Copy.
        unsafe { std::slice::from_raw_parts(self.items.as_ptr() as *const Item<D>, self.len) }
    }

    #[inline]
    fn dist_at(&self, i: usize) -> D {
        // SAFETY: caller guarantees i < len.
        unsafe { self.items[i].assume_init() }.0
    }

    // insert (d,id), keeping the list sorted and capped at N; rejects anything
    // not better than the current worst once full.
    #[inline]
    fn push(&mut self, d: D, id: u32) {
        if N == 0 {
            return;
        }
        let full = self.len == N;
        if full && d.cmp_total(&self.dist_at(N - 1)) != Ordering::Less {
            return;
        }
        // insertion index p in [0..=len]
        let mut p = self.len;
        while p > 0 && d.cmp_total(&self.dist_at(p - 1)) == Ordering::Less {
            p -= 1;
        }
        // shift [p..hi) up by one; when full this drops the old worst at N-1.
        let hi = if full { N - 1 } else { self.len };
        if hi > p {
            self.items.copy_within(p..hi, p + 1);
            self.done.copy_within(p..hi, p + 1);
        }
        self.items[p] = MaybeUninit::new(Item(d, id));
        self.done[p] = false;
        self.len = hi + 1;
        // a newly inserted (undone) item at p must be reachable by next()
        if p < self.front {
            self.front = p;
        }
    }

    // nearest unexpanded candidate; marks it expanded and advances the hint.
    #[inline]
    fn next(&mut self) -> Option<(D, u32)> {
        while self.front < self.len {
            if !self.done[self.front] {
                let i = self.front;
                self.done[i] = true;
                let it = unsafe { self.items[i].assume_init() };
                self.front += 1;
                while self.front < self.len && self.done[self.front] {
                    self.front += 1;
                }
                return Some((it.0, it.1));
            }
            self.front += 1;
        }
        None
    }
}

pub struct Hnsw<
    Dtype,
    Distance: Metric<Dtype>,
    const DIM: usize,
    const M_0: usize,
    const K: usize,
    const EF_SEARCH: usize,
    const EF_BUILD: usize,
    const MAX_LEVEL: usize,
> {
    _metric: Distance,
    vectors: Vec<[Dtype; DIM]>,
    l0: Vec<[u32; M_0]>,
    meta: Vec<NodeMeta>,
    upper: Vec<u32>,
    free_nodes: Vec<u32>,
    free_upper: [Vec<u32>; MAX_LEVEL],
    entry_point: Option<(u8, u32)>,
    rng_state: u64,

    visited: Vec<u32>,
    vstamp: u32,
}

impl<
    Dtype,
    Distance,
    const DIM: usize,
    const M_0: usize,
    const K: usize,
    const EF_SEARCH: usize,
    const EF_BUILD: usize,
    const MAX_LEVEL: usize,
> Hnsw<Dtype, Distance, DIM, M_0, K, EF_SEARCH, EF_BUILD, MAX_LEVEL>
where
    Dtype: Scalar,
    Distance: Metric<Dtype>,
{
    const M: usize = M_0 / 2;
    const M_L: f64 = 1.0 / const_ln(Self::M as f64);

    pub fn new(metric: Distance, seed: u64) -> Self {
        // EF_SEARCH must be able to hold K results; cheap compile-time-ish guard.
        debug_assert!(EF_SEARCH >= K, "EF_SEARCH must be >= K");
        debug_assert!(EF_BUILD >= 1, "EF_BUILD must be >= 1");
        Self {
            _metric: metric,
            vectors: Vec::new(),
            l0: Vec::new(),
            meta: Vec::new(),
            upper: Vec::new(),
            free_nodes: Vec::new(),
            free_upper: std::array::from_fn(|_| Vec::new()),
            entry_point: None,
            rng_state: seed | 1,
            visited: Vec::new(),
            vstamp: 0,
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.meta.iter().filter(|m| m.alive).count()
    }
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entry_point.is_none()
    }

    #[inline]
    fn cap_at(layer: usize) -> usize {
        if layer == 0 { M_0 } else { Self::M }
    }

    #[inline]
    fn dq(&self, q: &[Dtype], b: usize) -> Distance::Out {
        Distance::distance(q, &self.vectors[b][..])
    }
    #[inline]
    fn dd(&self, a: usize, b: usize) -> Distance::Out {
        Distance::distance(&self.vectors[a][..], &self.vectors[b][..])
    }

    #[inline]
    fn present(&self, id: usize, layer: usize) -> bool {
        self.meta[id].alive && layer <= self.meta[id].level as usize
    }

    #[inline]
    fn nbr(&self, id: usize, layer: usize) -> &[u32] {
        if layer == 0 {
            &self.l0[id][..]
        } else if layer > self.meta[id].level as usize {
            &[]
        } else {
            let off = self.meta[id].upper as usize + (layer - 1) * Self::M;
            &self.upper[off..off + Self::M]
        }
    }

    #[inline]
    fn nbr_mut<'a>(
        l0: &'a mut [[u32; M_0]],
        upper: &'a mut [u32],
        meta: &[NodeMeta],
        id: usize,
        layer: usize,
    ) -> &'a mut [u32] {
        if layer == 0 {
            &mut l0[id][..]
        } else {
            let off = meta[id].upper as usize + (layer - 1) * Self::M;
            &mut upper[off..off + Self::M]
        }
    }

    #[inline]
    fn random_level(&mut self) -> usize {
        let mut x = self.rng_state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng_state = x;
        let u = ((x >> 11) as f64) / ((1u64 << 53) as f64);
        let u = if u <= 0.0 { f64::MIN_POSITIVE } else { u };
        let lvl = (-u.ln() * Self::M_L).floor() as i64;
        (lvl.max(0) as usize).min(MAX_LEVEL - 1)
    }

    #[inline]
    fn alloc_upper(&mut self, level: usize) -> u32 {
        if level == 0 {
            return NONE;
        }
        let width = level * Self::M;
        if let Some(off) = self.free_upper[level].pop() {
            for s in &mut self.upper[off as usize..off as usize + width] {
                *s = NONE;
            }
            off
        } else {
            let off = self.upper.len() as u32;
            self.upper.resize(self.upper.len() + width, NONE);
            off
        }
    }

    // ---------- search a single layer; result lands sorted in `fr` ----------
    #[inline]
    fn search_layer<const N: usize>(
        &self,
        q: &[Dtype],
        entries: &[u32],
        layer: usize,
        vis: &mut [u32],
        stamp: u32,
        fr: &mut Frontier<Distance::Out, N>,
    ) {
        fr.reset();
        for &e in entries {
            let e = e as usize;
            if vis[e] == stamp {
                continue;
            }
            vis[e] = stamp;
            let d = self.dq(q, e);
            fr.push(d, e as u32);
        }
        while let Some((_cd, c)) = fr.next() {
            let c = c as usize;
            let nbrs = self.nbr(c, layer);
            for k in 0..nbrs.len() {
                let n = nbrs[k];
                if n == NONE {
                    continue;
                }
                let n = n as usize;
                if !self.present(n, layer) || vis[n] == stamp {
                    continue;
                }
                vis[n] = stamp;
                let d = self.dq(q, n);
                // push self-rejects anything not better than the full worst,
                // which is exactly the old `worse` gate.
                fr.push(d, n as u32);
            }
        }
    }

    // greedy 1-NN walk for the upper-layer descent (no visited needed)
    #[inline]
    fn greedy(&self, q: &[Dtype], mut cur: usize, layer: usize) -> usize {
        let mut cur_d = self.dq(q, cur);
        loop {
            let mut best = cur;
            let mut best_d = cur_d;
            let nbrs = self.nbr(cur, layer);
            for &n in nbrs {
                if n == NONE {
                    continue;
                }
                let n = n as usize;
                if !self.present(n, layer) {
                    continue;
                }
                let d = self.dq(q, n);
                if d.cmp_total(&best_d) == Ordering::Less {
                    best = n;
                    best_d = d;
                }
            }
            if best == cur {
                break;
            }
            cur = best;
            cur_d = best_d;
        }
        cur
    }

    // Algorithm 4: pick up to `cap` diverse neighbors from a sorted-ascending
    // candidate list `sel`. `exclude` is dropped if seen. Returns ids in `out`.
    #[inline]
    fn select_heuristic(
        &self,
        sel: &[Item<Distance::Out>],
        cap: usize,
        exclude: u32,
        out: &mut Vec<u32>,
    ) {
        out.clear();
        for &Item(dq, e) in sel {
            if e == exclude {
                continue;
            }
            if out.len() >= cap {
                break;
            }
            let mut keep = true;
            for &r in out.iter() {
                if self.dd(e as usize, r as usize).cmp_total(&dq) == Ordering::Less {
                    keep = false;
                    break;
                }
            }
            if keep {
                out.push(e);
            }
        }
        // keepPrunedConnections: backfill to cap with the nearest leftovers
        if out.len() < cap {
            for &Item(_, e) in sel {
                if out.len() >= cap {
                    break;
                }
                if e != exclude && !out.contains(&e) {
                    out.push(e);
                }
            }
        }
    }

    // ----------------------------- public: search -----------------------------
    // Returns up to K nearest (id, distance), ascending. K is now THE k.
    pub fn search(&self, q: &[Dtype]) -> Vec<(Distance::Out, u32)> {
        let (e_lvl, e_id) = match self.entry_point {
            Some(ep) => ep,
            None => return Vec::new(),
        };
        let mut cur = e_id as usize;
        for layer in (1..=e_lvl as usize).rev() {
            cur = self.greedy(q, cur, layer);
        }
        let mut out = VISITED.with(|v| {
            let (buf, stamp) = &mut *v.borrow_mut();
            Self::bump(buf, stamp, self.meta.len());
            let mut fr = Frontier::<Distance::Out, EF_SEARCH>::new();
            self.search_layer(q, &[cur as u32], 0, buf, *stamp, &mut fr);
            // fr.items() is already ascending.
            fr.items()
                .iter()
                .map(|&Item(d, i)| (d, i))
                .collect::<Vec<_>>()
        });
        out.truncate(K);
        out
    }

    #[inline]
    fn bump(buf: &mut Vec<u32>, stamp: &mut u32, n: usize) {
        if buf.len() < n {
            buf.resize(n, 0);
        }
        *stamp = stamp.wrapping_add(1);
        if *stamp == 0 {
            for x in buf.iter_mut() {
                *x = 0;
            }
            *stamp = 1;
        }
    }

    // ----------------------------- public: insert -----------------------------
    pub fn insert(&mut self, v: [Dtype; DIM]) -> u32 {
        let level = self.random_level();
        let upper = self.alloc_upper(level);
        let id = if let Some(slot) = self.free_nodes.pop() {
            let s = slot as usize;
            self.vectors[s] = v;
            self.l0[s] = [NONE; M_0];
            self.meta[s] = NodeMeta {
                level: level as u8,
                upper,
                alive: true,
            };
            slot
        } else {
            let s = self.meta.len() as u32;
            self.vectors.push(v);
            self.l0.push([NONE; M_0]);
            self.meta.push(NodeMeta {
                level: level as u8,
                upper,
                alive: true,
            });
            s
        };

        let (e_lvl, e_id) = match self.entry_point {
            Some(ep) => ep,
            None => {
                self.entry_point = Some((level as u8, id));
                return id;
            }
        };

        let q = self.vectors[id as usize]; // copy out so we can mutate self freely
        let mut cur = e_id as usize;
        for layer in ((level + 1)..=e_lvl as usize).rev() {
            cur = self.greedy(&q[..], cur, layer);
        }

        let top = level.min(e_lvl as usize);
        let mut entries: Vec<u32> = vec![cur as u32];

        // Detach the owned visited buffer so we can hold &self (graph reads) and
        // &mut the buffer simultaneously. The frontier is a stack local now, so
        // it needs no detaching. Single-writer => no reader contention.
        let n_nodes = self.meta.len();
        let mut vbuf = std::mem::take(&mut self.visited);
        let mut fr = Frontier::<Distance::Out, EF_BUILD>::new();

        for layer in (0..=top).rev() {
            Self::bump(&mut vbuf, &mut self.vstamp, n_nodes);
            let stamp = self.vstamp;
            self.search_layer(&q[..], &entries, layer, &mut vbuf, stamp, &mut fr);

            // candidates are already sorted ascending in fr.items()
            let cap = Self::cap_at(layer);
            let mut chosen: Vec<u32> = Vec::with_capacity(cap);
            self.select_heuristic(fr.items(), cap, id, &mut chosen);

            // next layer descends from the full candidate set
            entries.clear();
            entries.extend(fr.items().iter().map(|it| it.1));

            // write the new node's links
            {
                let slots = Self::nbr_mut(
                    &mut self.l0,
                    &mut self.upper,
                    &self.meta,
                    id as usize,
                    layer,
                );
                for s in slots.iter_mut() {
                    *s = NONE;
                }
                for (i, &c) in chosen.iter().enumerate() {
                    slots[i] = c;
                }
            }
            // symmetric back-links, with pruning on the neighbor side
            for &n in &chosen {
                self.connect(n as usize, id, layer);
            }
        }

        self.visited = vbuf; // give the buffer back

        if level as u8 > e_lvl {
            self.entry_point = Some((level as u8, id));
        }
        id
    }

    // add edge n -> newid at layer, pruning n's list (symmetric removals on drop)
    #[inline]
    fn connect(&mut self, n: usize, newid: u32, layer: usize) {
        if !self.present(n, layer) {
            return;
        }
        let cap = Self::cap_at(layer);
        let mut cur: [u32; M_0] = [NONE; M_0];
        let mut len = 0usize;
        for &x in self.nbr(n, layer) {
            if x != NONE && self.present(x as usize, layer) {
                cur[len] = x;
                len += 1;
            }
        }
        if cur[..len].contains(&newid) {
            return;
        }
        if len < cap {
            let slots = Self::nbr_mut(&mut self.l0, &mut self.upper, &self.meta, n, layer);
            for s in slots.iter_mut() {
                if *s == NONE {
                    *s = newid;
                    break;
                }
            }
            return;
        }
        // full: run heuristic over (current ∪ newid) ranked by distance to n
        let mut sel: Vec<Item<Distance::Out>> = (0..len)
            .map(|i| Item(self.dd(n, cur[i] as usize), cur[i]))
            .collect();
        sel.push(Item(self.dd(n, newid as usize), newid));
        sel.sort_by(|a, b| a.0.cmp_total(&b.0));
        let mut chosen: Vec<u32> = Vec::with_capacity(cap);
        self.select_heuristic(&sel, cap, n as u32, &mut chosen);

        for i in 0..len {
            let z = cur[i];
            if z != newid && !chosen.contains(&z) {
                self.remove_link(z as usize, n as u32, layer);
            }
        }
        let slots = Self::nbr_mut(&mut self.l0, &mut self.upper, &self.meta, n, layer);
        for s in slots.iter_mut() {
            *s = NONE;
        }
        for (i, &c) in chosen.iter().enumerate() {
            slots[i] = c;
        }
    }

    #[inline]
    fn remove_link(&mut self, a: usize, target: u32, layer: usize) {
        if !self.present(a, layer) {
            return;
        }
        let slots = Self::nbr_mut(&mut self.l0, &mut self.upper, &self.meta, a, layer);
        for s in slots.iter_mut() {
            if *s == target {
                *s = NONE;
            }
        }
    }

    // ----------------------------- public: remove -----------------------------
    pub fn remove(&mut self, id: u32) {
        let idu = id as usize;
        if idu >= self.meta.len() || !self.meta[idu].alive {
            return;
        }
        let level = self.meta[idu].level as usize;

        for layer in 0..=level {
            let mut s: [u32; M_0] = [NONE; M_0];
            let mut sl = 0usize;
            for &x in self.nbr(idu, layer) {
                if x != NONE && self.present(x as usize, layer) {
                    s[sl] = x;
                    sl += 1;
                }
            }
            for i in 0..sl {
                self.remove_link(s[i] as usize, id, layer);
            }
            for i in 0..sl {
                let n = s[i] as usize;
                let cap = Self::cap_at(layer);
                let mut pool: Vec<u32> = Vec::with_capacity(2 * M_0);
                let push = |v: u32, pool: &mut Vec<u32>| {
                    if v != NONE && v as usize != n && !pool.contains(&v) {
                        pool.push(v);
                    }
                };
                for &x in self.nbr(n, layer) {
                    if x != NONE && self.present(x as usize, layer) {
                        push(x, &mut pool);
                    }
                }
                for j in 0..sl {
                    if j != i {
                        push(s[j], &mut pool);
                    }
                }
                let mut sel: Vec<Item<Distance::Out>> = pool
                    .iter()
                    .map(|&t| Item(self.dd(n, t as usize), t))
                    .collect();
                sel.sort_by(|a, b| a.0.cmp_total(&b.0));
                let mut chosen: Vec<u32> = Vec::with_capacity(cap);
                self.select_heuristic(&sel, cap, n as u32, &mut chosen);

                let slots = Self::nbr_mut(&mut self.l0, &mut self.upper, &self.meta, n, layer);
                for s in slots.iter_mut() {
                    *s = NONE;
                }
                for (k, &c) in chosen.iter().enumerate() {
                    slots[k] = c;
                }
                for &c in &chosen {
                    self.ensure_link(c as usize, n as u32, layer);
                }
            }
        }

        self.free_nodes.push(id);
        if level > 0 {
            self.free_upper[level].push(self.meta[idu].upper);
        }
        self.meta[idu].alive = false;
        self.meta[idu].upper = NONE;
        self.l0[idu] = [NONE; M_0];

        if matches!(self.entry_point, Some((_, e)) if e == id) {
            self.entry_point = self.highest_alive();
        }
    }

    #[inline]
    fn ensure_link(&mut self, a: usize, target: u32, layer: usize) {
        if !self.present(a, layer) {
            return;
        }
        let slots = Self::nbr_mut(&mut self.l0, &mut self.upper, &self.meta, a, layer);
        let mut free = None;
        for (i, s) in slots.iter().enumerate() {
            if *s == target {
                return;
            }
            if *s == NONE && free.is_none() {
                free = Some(i);
            }
        }
        if let Some(i) = free {
            slots[i] = target;
        }
    }

    #[inline]
    fn highest_alive(&self) -> Option<(u8, u32)> {
        let mut best: Option<(u8, u32)> = None;
        for (i, m) in self.meta.iter().enumerate() {
            if m.alive && best.is_none_or(|(bl, _)| m.level > bl) {
                best = Some((m.level, i as u32));
            }
        }
        best
    }
}
// ============================ tests ============================
#[cfg(test)]
mod tests {
    use crate::metric::{L1, L2};

    use super::*;
    use std::collections::HashSet;

    struct Rng(u64);
    impl Rng {
        fn new(seed: u64) -> Self {
            Self(seed | 1)
        }
        fn next_u64(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn f32(&mut self) -> f32 {
            ((self.next_u64() >> 11) as f32) / ((1u64 << 53) as f32)
        }
        fn vec<const D: usize>(&mut self) -> [f32; D] {
            std::array::from_fn(|_| self.f32())
        }
        fn clustered<const D: usize>(&mut self, centers: &[[f32; D]]) -> [f32; D] {
            let c = &centers[(self.next_u64() as usize) % centers.len()];
            std::array::from_fn(|i| c[i] + (self.f32() - 0.5) * 0.05)
        }
    }

    fn l2(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum()
    }
    fn l1i(a: &[i32], b: &[i32]) -> i32 {
        a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum()
    }

    fn brute<const D: usize>(data: &[(u32, [f32; D])], q: &[f32; D], k: usize) -> Vec<u32> {
        let mut v: Vec<(f32, u32)> = data.iter().map(|(id, p)| (l2(p, q), *id)).collect();
        v.sort_by(|a, b| a.0.total_cmp(&b.0));
        v.into_iter().take(k).map(|(_, i)| i).collect()
    }

    // recall over the static K results
    fn recall_at<
        const D: usize,
        Me,
        const M0: usize,
        const K: usize,
        const ES: usize,
        const EB: usize,
        const ML: usize,
    >(
        ix: &Hnsw<f32, Me, D, M0, K, ES, EB, ML>,
        live: &[(u32, [f32; D])],
        queries: usize,
        seed: u64,
    ) -> f64
    where
        Me: Metric<f32, Out = f32>,
    {
        let mut rng = Rng::new(seed);
        let (mut hit, mut total) = (0usize, 0usize);
        for _ in 0..queries {
            let q: [f32; D] = rng.vec();
            let truth = brute(live, &q, K);
            let got: HashSet<u32> = ix.search(&q).into_iter().map(|(_, i)| i).collect();
            for t in truth {
                total += 1;
                if got.contains(&t) {
                    hit += 1;
                }
            }
        }
        hit as f64 / total as f64
    }

    fn recall_dist<
        const D: usize,
        Me,
        const M0: usize,
        const K: usize,
        const ES: usize,
        const EB: usize,
        const ML: usize,
    >(
        ix: &Hnsw<f32, Me, D, M0, K, ES, EB, ML>,
        live: &[(u32, [f32; D])],
        queries: &[[f32; D]],
    ) -> f64
    where
        Me: Metric<f32, Out = f32>,
    {
        let (mut hit, mut total) = (0usize, 0usize);
        for q in queries {
            let mut td: Vec<f32> = live.iter().map(|(_, p)| l2(p, q)).collect();
            td.sort_by(|a, b| a.total_cmp(b));
            let kth = td[K.min(td.len()) - 1];
            let eps = 1e-4 * (1.0 + kth);
            for (d, _) in ix.search(q) {
                total += 1;
                if d <= kth + eps {
                    hit += 1;
                }
            }
        }
        hit as f64 / total as f64
    }

    fn check_invariants<
        Dtype,
        Me,
        const D: usize,
        const M0: usize,
        const K: usize,
        const ES: usize,
        const EB: usize,
        const ML: usize,
    >(
        ix: &Hnsw<Dtype, Me, D, M0, K, ES, EB, ML>,
    ) where
        Dtype: Scalar,
        Me: Metric<Dtype>,
    {
        let n = ix.meta.len();
        let mut max_alive_level: Option<u8> = None;
        for id in 0..n {
            let m = &ix.meta[id];
            if !m.alive {
                continue;
            }
            max_alive_level = Some(max_alive_level.map_or(m.level, |x| x.max(m.level)));
            if m.level > 0 {
                let off = m.upper as usize;
                let width = m.level as usize * (M0 / 2);
                assert!(
                    off != NONE as usize && off + width <= ix.upper.len(),
                    "node {id} arena oob: off={off} width={width} len={}",
                    ix.upper.len()
                );
            }
            for layer in 0..=m.level as usize {
                let nbrs = ix.nbr(id, layer);
                let mut seen = HashSet::new();
                for &x in nbrs {
                    if x == NONE {
                        continue;
                    }
                    assert!((x as usize) < n, "node {id} layer {layer} -> bad id {x}");
                    assert!(x as usize != id, "node {id} layer {layer} self-loop");
                    assert!(
                        seen.insert(x),
                        "node {id} layer {layer} duplicate neighbor {x}"
                    );
                }
            }
        }
        match (ix.entry_point, max_alive_level) {
            (Some((el, eid)), Some(ml)) => {
                assert!(ix.meta[eid as usize].alive, "entry points to dead node");
                assert_eq!(el, ix.meta[eid as usize].level, "entry level mismatch");
                assert_eq!(el, ml, "entry is not the highest-level alive node");
            }
            (None, None) => {}
            (ep, ml) => panic!("entry/alive disagreement: {:?} vs max_level {:?}", ep, ml),
        }
    }

    fn dangling<
        Dtype,
        Me,
        const D: usize,
        const M0: usize,
        const K: usize,
        const ES: usize,
        const EB: usize,
        const ML: usize,
    >(
        ix: &Hnsw<Dtype, Me, D, M0, K, ES, EB, ML>,
    ) -> (usize, usize)
    where
        Dtype: Scalar,
        Me: Metric<Dtype>,
    {
        let (mut bad, mut total) = (0usize, 0usize);
        for id in 0..ix.meta.len() {
            if !ix.meta[id].alive {
                continue;
            }
            for layer in 0..=ix.meta[id].level as usize {
                for &x in ix.nbr(id, layer) {
                    if x == NONE {
                        continue;
                    }
                    total += 1;
                    if !ix.present(x as usize, layer) {
                        bad += 1;
                    }
                }
            }
        }
        (bad, total)
    }

    // type aliases keep the (now longer) generic list readable in tests
    type Ix8 = Hnsw<f32, L2, 8, 16, 10, 20, 40, 12>;
    type Ix16 = Hnsw<f32, L2, 16, 16, 10, 20, 40, 12>;

    #[test]
    fn empty_index() {
        let ix: Ix8 = Hnsw::new(L2, 1);
        assert!(ix.is_empty());
        assert_eq!(ix.len(), 0);
        assert!(ix.search(&[0.0; 8]).is_empty());
        check_invariants(&ix);
    }

    #[test]
    fn single_insert_self_query() {
        let mut ix: Ix8 = Hnsw::new(L2, 1);
        let v = [0.3; 8];
        let id = ix.insert(v);
        assert_eq!(ix.len(), 1);
        assert!(!ix.is_empty());
        let r = ix.search(&v);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].1, id);
        assert_eq!(r[0].0, 0.0);
        check_invariants(&ix);
    }

    #[test]
    fn results_are_sorted_and_bounded() {
        let mut rng = Rng::new(5);
        let mut ix: Ix8 = Hnsw::new(L2, 5);
        for _ in 0..50 {
            ix.insert(rng.vec());
        }
        let q: [f32; 8] = rng.vec();
        let r = ix.search(&q);
        assert_eq!(r.len(), 10); // K
        for w in r.windows(2) {
            assert!(w[0].0 <= w[1].0, "results not ascending by distance");
        }
        // fewer than K nodes -> clamps to N, never panics
        let mut small: Ix8 = Hnsw::new(L2, 1);
        for _ in 0..3 {
            small.insert(rng.vec());
        }
        assert_eq!(small.search(&q).len(), 3);
    }

    #[test]
    fn every_inserted_point_finds_itself() {
        let mut rng = Rng::new(11);
        let mut ix: Ix8 = Hnsw::new(L2, 11);
        let mut pts = Vec::new();
        for _ in 0..500 {
            let v: [f32; 8] = rng.vec();
            pts.push((ix.insert(v), v));
        }
        for (id, v) in &pts {
            let top = ix.search(v);
            assert!(!top.is_empty());
            assert_eq!(top[0].0, 0.0, "self distance must be zero");
            assert_eq!(top[0].1, *id);
        }
        check_invariants(&ix);
    }

    #[test]
    fn recall_uniform_2k() {
        let mut rng = Rng::new(0xABCDEF);
        let mut ix: Ix16 = Hnsw::new(L2, 7);
        let mut live = Vec::new();
        for _ in 0..2000 {
            let v: [f32; 16] = rng.vec();
            live.push((ix.insert(v), v));
        }
        check_invariants(&ix);
        let recall = recall_at(&ix, &live, 200, 1);
        println!("uniform recall@10 = {recall:.3}");
        assert!(recall > 0.90, "uniform recall too low: {recall}");
    }

    #[test]
    fn recall_clustered() {
        let mut rng = Rng::new(0x99);
        let centers: Vec<[f32; 16]> = (0..20).map(|_| rng.vec()).collect();
        let mut ix: Ix16 = Hnsw::new(L2, 3);
        let mut live = Vec::new();
        for _ in 0..2000 {
            let v: [f32; 16] = rng.clustered(&centers);
            live.push((ix.insert(v), v));
        }
        check_invariants(&ix);
        let mut qrng = Rng::new(2);
        let queries: Vec<[f32; 16]> = (0..200).map(|_| qrng.clustered(&centers)).collect();
        let recall = recall_dist(&ix, &live, &queries);
        println!("clustered distance-recall@10 = {recall:.3}");
        assert!(recall > 0.90, "clustered recall too low: {recall}");
    }

    #[test]
    fn deleted_never_returned_and_len_tracks() {
        const N: usize = 1500;
        let mut rng = Rng::new(0x1234);
        let mut ix: Ix8 = Hnsw::new(L2, 42);
        let mut all: Vec<(u32, [f32; 8])> = Vec::new();
        for _ in 0..N {
            let v: [f32; 8] = rng.vec();
            all.push((ix.insert(v), v));
        }
        assert_eq!(ix.len(), N);

        let mut dead: HashSet<u32> = HashSet::new();
        let mut live: Vec<(u32, [f32; 8])> = Vec::new();
        for (i, (id, v)) in all.iter().enumerate() {
            if i % 5 == 0 {
                ix.remove(*id);
                dead.insert(*id);
            } else {
                live.push((*id, *v));
            }
        }
        assert_eq!(ix.len(), live.len());
        check_invariants(&ix);

        let mut rng2 = Rng::new(77);
        for _ in 0..300 {
            let q: [f32; 8] = rng2.vec();
            for (_, id) in ix.search(&q) {
                assert!(!dead.contains(&id), "deleted id {id} returned by search");
            }
        }
        let recall = recall_at(&ix, &live, 200, 9);
        println!("post-delete recall@10 = {recall:.3}");
        assert!(recall > 0.88, "post-delete recall too low: {recall}");
    }

    #[test]
    fn remove_all_then_recover() {
        let mut rng = Rng::new(8);
        let mut ix: Ix8 = Hnsw::new(L2, 8);
        let ids: Vec<u32> = (0..200).map(|_| ix.insert(rng.vec())).collect();
        for id in &ids {
            ix.remove(*id);
        }
        assert!(ix.is_empty());
        assert_eq!(ix.len(), 0);
        assert!(ix.search(&[0.0; 8]).is_empty());
        check_invariants(&ix);
        let v = [0.5; 8];
        let nid = ix.insert(v);
        assert_eq!(ix.len(), 1);
        assert_eq!(ix.search(&v)[0].1, nid);
        check_invariants(&ix);
    }

    #[test]
    fn idempotent_and_bogus_remove() {
        let mut ix: Ix8 = Hnsw::new(L2, 1);
        let id = ix.insert([1.0; 8]);
        ix.remove(id);
        ix.remove(id);
        ix.remove(99999);
        assert_eq!(ix.len(), 0);
        check_invariants(&ix);
    }

    #[test]
    fn duplicate_vectors() {
        let mut ix: Ix8 = Hnsw::new(L2, 4);
        let mut rng = Rng::new(4);
        let v = [0.7; 8];
        for _ in 0..50 {
            ix.insert(v);
        }
        for _ in 0..50 {
            ix.insert(rng.vec());
        }
        check_invariants(&ix);
        let r = ix.search(&v);
        assert_eq!(r.len(), 10);
        assert!(
            r.iter().all(|(d, _)| *d == 0.0),
            "duplicates must dominate the result at their own location"
        );
    }

    #[test]
    fn churn_invariants_hold() {
        let mut rng = Rng::new(99);
        let mut ix: Hnsw<f32, L2, 12, 32, 10, 20, 40, 8> = Hnsw::new(L2, 7);
        let mut live: Vec<(u32, [f32; 12])> = Vec::new();
        for _ in 0..3000 {
            let v: [f32; 12] = rng.vec();
            live.push((ix.insert(v), v));
        }
        for round in 0..5 {
            let mut keep = Vec::new();
            for (i, (id, v)) in live.iter().enumerate() {
                if i % 5 == 0 {
                    ix.remove(*id);
                } else {
                    keep.push((*id, *v));
                }
            }
            live = keep;
            for _ in 0..600 {
                let v: [f32; 12] = rng.vec();
                live.push((ix.insert(v), v));
            }
            check_invariants(&ix);
            assert_eq!(ix.len(), live.len(), "len mismatch after round {round}");
        }
        let (bad, total) = dangling(&ix);
        println!(
            "after churn: {bad}/{total} inert stale links ({:.2}%)",
            100.0 * bad as f64 / total as f64
        );
        let recall = recall_at(&ix, &live, 150, 5);
        println!("churn recall@10 = {recall:.3} (live={})", live.len());
        assert!(recall > 0.85, "churn recall too low: {recall}");
    }

    #[test]
    fn generic_over_int_scalar_and_distance() {
        let mut ix: Hnsw<i32, L1, 6, 16, 5, 10, 20, 10> = Hnsw::new(L1, 13);
        let mut rng = Rng::new(21);
        let mut live: Vec<(u32, [i32; 6])> = Vec::new();
        for _ in 0..800 {
            let v: [i32; 6] = std::array::from_fn(|_| (rng.next_u64() % 100) as i32);
            live.push((ix.insert(v), v));
        }
        check_invariants(&ix);
        let mut rng2 = Rng::new(64);
        let (mut hit, mut total) = (0usize, 0usize);
        for _ in 0..150 {
            let q: [i32; 6] = std::array::from_fn(|_| (rng2.next_u64() % 100) as i32);
            let mut truth: Vec<(i32, u32)> = live.iter().map(|(id, p)| (l1i(p, &q), *id)).collect();
            truth.sort_by_key(|x| x.0);
            let truth: HashSet<u32> = truth.into_iter().take(5).map(|(_, i)| i).collect();
            let got: HashSet<u32> = ix.search(&q).into_iter().map(|(_, i)| i).collect();
            for t in truth {
                total += 1;
                if got.contains(&t) {
                    hit += 1;
                }
            }
        }
        let recall = hit as f64 / total as f64;
        println!("int/L1 recall@5 = {recall:.3}");
        assert!(recall > 0.80, "int recall too low: {recall}");
    }

    #[test]
    fn entry_point_survives_repeated_top_removal() {
        let mut rng = Rng::new(31);
        let mut ix: Ix8 = Hnsw::new(L2, 31);
        let mut live: Vec<(u32, [f32; 8])> = Vec::new();
        for _ in 0..600 {
            let v: [f32; 8] = rng.vec();
            live.push((ix.insert(v), v));
        }
        for _ in 0..30 {
            let entry = ix.entry_point.map(|(_, e)| e);
            if let Some(e) = entry {
                ix.remove(e);
                live.retain(|(id, _)| *id != e);
                check_invariants(&ix);
            }
        }
        assert_eq!(ix.len(), live.len());
        let recall = recall_at(&ix, &live, 150, 6);
        println!("after entry churn recall@10 = {recall:.3}");
        assert!(recall > 0.85, "recall too low after entry churn: {recall}");
    }

    #[test]
    fn determinism_same_seed() {
        let build = |seed: u64| {
            let mut rng = Rng::new(123);
            let mut ix: Ix8 = Hnsw::new(L2, seed);
            for _ in 0..300 {
                ix.insert(rng.vec());
            }
            let mut qrng = Rng::new(456);
            let mut out = Vec::new();
            for _ in 0..20 {
                let q: [f32; 8] = qrng.vec();
                out.push(ix.search(&q));
            }
            out
        };
        let a = build(42);
        let b = build(42);
        assert_eq!(a.len(), b.len());
        for (ra, rb) in a.iter().zip(&b) {
            let ia: Vec<u32> = ra.iter().map(|(_, i)| *i).collect();
            let ib: Vec<u32> = rb.iter().map(|(_, i)| *i).collect();
            assert_eq!(ia, ib, "same seed produced different results");
        }
    }
}
