//! Flat binary serialization for [`Hnsw`]. The graph is pure POD arrays
//! (node id = index, `u32::MAX` sentinels, flat arenas), so persistence is a
//! versioned header plus raw slice dumps; restoring is bit-faithful — the
//! free lists and rng state come back exactly, so searches AND subsequent
//! inserts on a restored graph match the original.
use super::*;
use std::io::{self, Read, Write};

const MAGIC: [u8; 8] = *b"ANNYHNSW";
const VERSION: u32 = 1;

fn bad(msg: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

fn read_u32<R: Read>(r: &mut R) -> io::Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn read_u64<R: Read>(r: &mut R) -> io::Result<u64> {
    let mut b = [0u8; 8];
    r.read_exact(&mut b)?;
    Ok(u64::from_le_bytes(b))
}

// counts are node ids / arena offsets, so they must fit in u32
fn read_len<R: Read>(r: &mut R) -> io::Result<usize> {
    let n = read_u64(r)?;
    if n > u32::MAX as u64 {
        return Err(bad("count exceeds u32 range"));
    }
    Ok(n as usize)
}

fn write_pod<T: Copy, W: Write>(w: &mut W, s: &[T]) -> io::Result<()> {
    // SAFETY: T here is only ever a Scalar primitive, u32, or a fixed-size
    // array of those — plain-old-data with no padding — so every byte of the
    // slice is initialized.
    let bytes = unsafe { std::slice::from_raw_parts(s.as_ptr() as *const u8, size_of_val(s)) };
    w.write_all(bytes)
}

fn read_pod<T: Copy, R: Read>(r: &mut R, len: usize) -> io::Result<Vec<T>> {
    let nbytes = len
        .checked_mul(size_of::<T>())
        .ok_or(bad("length overflow"))?;
    let mut bytes = vec![0u8; nbytes];
    r.read_exact(&mut bytes)?;
    let mut out: Vec<T> = Vec::with_capacity(len);
    // SAFETY: same POD argument as write_pod (any bit pattern is a valid T);
    // the source holds exactly len * size_of::<T>() bytes and the fresh
    // allocation holds len Ts, so the copy is in-bounds and set_len covers
    // only initialized elements.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out.as_mut_ptr() as *mut u8, nbytes);
        out.set_len(len);
    }
    Ok(out)
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
    /// Serialize the full graph state (little-endian, versioned).
    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_all(&MAGIC)?;
        w.write_all(&VERSION.to_le_bytes())?;
        for c in [DIM, M_0, MAX_LEVEL, size_of::<Dtype>()] {
            w.write_all(&(c as u64).to_le_bytes())?;
        }
        let n = self.meta.len();
        debug_assert!(self.vectors.len() == n && self.l0.len() == n);
        w.write_all(&(n as u64).to_le_bytes())?;
        w.write_all(&(self.upper.len() as u64).to_le_bytes())?;
        w.write_all(&(self.free_nodes.len() as u64).to_le_bytes())?;
        for fl in &self.free_upper {
            w.write_all(&(fl.len() as u64).to_le_bytes())?;
        }
        let (present, elvl, eid) = match self.entry_point {
            Some((l, i)) => (1u8, l, i),
            None => (0, 0, 0),
        };
        w.write_all(&[present, elvl])?;
        w.write_all(&eid.to_le_bytes())?;
        w.write_all(&self.rng_state.to_le_bytes())?;

        // meta as parallel arrays: NodeMeta has padding, never dump it raw
        let levels: Vec<u8> = self.meta.iter().map(|m| m.level).collect();
        let uppers: Vec<u32> = self.meta.iter().map(|m| m.upper).collect();
        let alive: Vec<u8> = self.meta.iter().map(|m| m.alive as u8).collect();
        w.write_all(&levels)?;
        write_pod(w, &uppers)?;
        w.write_all(&alive)?;

        write_pod(w, &self.vectors)?;
        write_pod(w, &self.l0)?;
        write_pod(w, &self.upper)?;
        write_pod(w, &self.free_nodes)?;
        for fl in &self.free_upper {
            write_pod(w, fl)?;
        }
        Ok(())
    }

    /// Deserialize a graph written by [`write_to`](Self::write_to). Any
    /// header or shape mismatch yields [`io::ErrorKind::InvalidData`].
    /// `_seed` is accepted for call-site symmetry with [`new`](Self::new);
    /// the persisted `rng_state` wins so restored builds stay deterministic.
    pub fn read_from<R: Read>(r: &mut R, metric: Distance, _seed: u64) -> io::Result<Self> {
        let mut magic = [0u8; 8];
        r.read_exact(&mut magic)?;
        if magic != MAGIC {
            return Err(bad("bad magic"));
        }
        if read_u32(r)? != VERSION {
            return Err(bad("unsupported version"));
        }
        for want in [DIM, M_0, MAX_LEVEL, size_of::<Dtype>()] {
            if read_u64(r)? != want as u64 {
                return Err(bad("const generic mismatch"));
            }
        }
        let n = read_len(r)?;
        let upper_len = read_len(r)?;
        let free_nodes_len = read_len(r)?;
        if free_nodes_len > n {
            return Err(bad("free_nodes longer than node count"));
        }
        let mut free_upper_lens = [0usize; MAX_LEVEL];
        for l in &mut free_upper_lens {
            *l = read_len(r)?;
            if *l > upper_len {
                return Err(bad("free_upper longer than arena"));
            }
        }
        let mut eb = [0u8; 2];
        r.read_exact(&mut eb)?;
        let eid = read_u32(r)?;
        let entry_point = match eb[0] {
            0 if eb[1] == 0 && eid == 0 => None,
            1 => Some((eb[1], eid)),
            _ => return Err(bad("bad entry point")),
        };
        let rng_state = read_u64(r)?;

        let levels: Vec<u8> = read_pod(r, n)?;
        let uppers: Vec<u32> = read_pod(r, n)?;
        let alive: Vec<u8> = read_pod(r, n)?;
        let vectors: Vec<[Dtype; DIM]> = read_pod(r, n)?;
        let l0: Vec<[u32; M_0]> = read_pod(r, n)?;
        let upper: Vec<u32> = read_pod(r, upper_len)?;
        let free_nodes: Vec<u32> = read_pod(r, free_nodes_len)?;
        let mut free_upper: Vec<Vec<u32>> = Vec::with_capacity(MAX_LEVEL);
        for (level, &len) in free_upper_lens.iter().enumerate() {
            let fl: Vec<u32> = read_pod(r, len)?;
            // a freed block at level L must span L*M in-bounds slots
            let width = level * Self::M;
            if fl
                .iter()
                .any(|&off| off as usize + width > upper_len || (level == 0 && !fl.is_empty()))
            {
                return Err(bad("free_upper offset out of bounds"));
            }
            free_upper.push(fl);
        }
        let free_upper: [Vec<u32>; MAX_LEVEL] =
            free_upper.try_into().unwrap_or_else(|_| unreachable!());

        // shape checks: everything an accessor would index unchecked
        let m = Self::M;
        for i in 0..n {
            let (lvl, ab) = (levels[i] as usize, alive[i]);
            if lvl >= MAX_LEVEL || ab > 1 {
                return Err(bad("bad node meta"));
            }
            if ab == 1 && lvl > 0 && uppers[i] as usize + lvl * m > upper_len {
                return Err(bad("node arena slice out of bounds"));
            }
        }
        if free_nodes.iter().any(|&id| id as usize >= n) {
            return Err(bad("free node id out of bounds"));
        }
        match entry_point {
            Some((lvl, id)) => {
                if id as usize >= n || alive[id as usize] != 1 || levels[id as usize] != lvl {
                    return Err(bad("entry point inconsistent"));
                }
            }
            None if alive.contains(&1) => return Err(bad("missing entry point")),
            None => {}
        }

        let meta = (0..n)
            .map(|i| NodeMeta {
                level: levels[i],
                upper: uppers[i],
                alive: alive[i] == 1,
            })
            .collect();

        Ok(Self {
            _metric: metric,
            vectors,
            l0,
            meta,
            upper,
            free_nodes,
            free_upper,
            entry_point,
            rng_state,
            // scratch matches new(): bump() handles the fresh zeroed state
            visited: Vec::new(),
            vstamp: 0,
        })
    }
}

// ============================ tests ============================
#[cfg(test)]
mod tests {
    use super::*;
    use crate::metric::L2;

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
    }

    type Ix = Hnsw<f32, L2, 8, 16, 10, 20, 40, 12>;

    fn churned_index() -> Ix {
        let mut rng = Rng::new(0xC0FFEE);
        let mut ix: Ix = Hnsw::new(L2, 42);
        let ids: Vec<u32> = (0..400).map(|_| ix.insert(rng.vec())).collect();
        for (i, id) in ids.iter().enumerate() {
            if i % 5 == 0 {
                ix.remove(*id);
            }
        }
        ix
    }

    #[test]
    fn roundtrip_is_bit_faithful() {
        let mut ix = churned_index();
        let mut blob = Vec::new();
        ix.write_to(&mut blob).unwrap();
        let mut back: Ix = Hnsw::read_from(&mut blob.as_slice(), L2, 42).unwrap();

        let mut qrng = Rng::new(0xBEEF);
        for _ in 0..25 {
            let q: [f32; 8] = qrng.vec();
            assert_eq!(ix.search(&q), back.search(&q), "restored search diverged");
        }
        // re-serialization is byte-identical
        let mut blob2 = Vec::new();
        back.write_to(&mut blob2).unwrap();
        assert_eq!(blob, blob2, "restored graph re-serializes differently");
        // further inserts land identically: free-list order + rng preserved
        let mut irng = Rng::new(0x5EED);
        for _ in 0..100 {
            let v: [f32; 8] = irng.vec();
            assert_eq!(ix.insert(v), back.insert(v), "insert ids diverged");
        }
        for _ in 0..25 {
            let q: [f32; 8] = qrng.vec();
            assert_eq!(
                ix.search(&q),
                back.search(&q),
                "post-insert search diverged"
            );
        }
    }

    #[test]
    fn empty_index_roundtrips() {
        let ix: Ix = Hnsw::new(L2, 7);
        let mut blob = Vec::new();
        ix.write_to(&mut blob).unwrap();
        let back: Ix = Hnsw::read_from(&mut blob.as_slice(), L2, 7).unwrap();
        assert!(back.is_empty());
        assert!(back.search(&[0.0; 8]).is_empty());
    }

    fn err_kind<T>(r: io::Result<T>) -> io::ErrorKind {
        match r {
            Ok(_) => panic!("expected an error"),
            Err(e) => e.kind(),
        }
    }

    #[test]
    fn header_mismatch_rejected() {
        let ix = churned_index();
        let mut blob = Vec::new();
        ix.write_to(&mut blob).unwrap();

        // wrong const generics at the reader
        type IxWide = Hnsw<f32, L2, 16, 16, 10, 20, 40, 12>;
        let k = err_kind(IxWide::read_from(&mut blob.as_slice(), L2, 42));
        assert_eq!(k, io::ErrorKind::InvalidData);

        // corrupt magic
        let mut b = blob.clone();
        b[0] ^= 0xFF;
        let k = err_kind(Ix::read_from(&mut b.as_slice(), L2, 42));
        assert_eq!(k, io::ErrorKind::InvalidData);

        // corrupt a const byte (DIM lives at offset 12, after magic+version)
        let mut b = blob.clone();
        b[12] ^= 0x01;
        let k = err_kind(Ix::read_from(&mut b.as_slice(), L2, 42));
        assert_eq!(k, io::ErrorKind::InvalidData);

        // truncation errors out rather than yielding garbage
        err_kind(Ix::read_from(&mut &blob[..blob.len() / 2], L2, 42));
    }
}
