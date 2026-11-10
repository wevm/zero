//! xxHash32 over UTF-8 bytes, and the wider `h64`/`h128` built from it
//! (`packages/shared/src/hash.ts`).

pub const PRIME1: u32 = 2_654_435_761;
pub const PRIME2: u32 = 2_246_822_519;
pub const PRIME3: u32 = 3_266_489_917;
pub const PRIME4: u32 = 668_265_263;
pub const PRIME5: u32 = 374_761_393;

#[inline]
pub fn round(acc: u32, word: u32) -> u32 {
    acc.wrapping_add(word.wrapping_mul(PRIME2))
        .rotate_left(13)
        .wrapping_mul(PRIME1)
}

#[inline]
pub fn avalanche(mut acc: u32) -> u32 {
    acc ^= acc >> 15;
    acc = acc.wrapping_mul(PRIME2);
    acc ^= acc >> 13;
    acc = acc.wrapping_mul(PRIME3);
    acc ^= acc >> 16;
    acc
}

/// Reference xxHash32 of `bytes` with `seed`.
pub fn hash(bytes: &[u8], seed: u32) -> u32 {
    let word = |b: &[u8]| u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    let mut rest = bytes;
    let mut acc = if bytes.len() >= 16 {
        let mut v = [
            seed.wrapping_add(PRIME1).wrapping_add(PRIME2),
            seed.wrapping_add(PRIME2),
            seed,
            seed.wrapping_sub(PRIME1),
        ];
        while rest.len() >= 16 {
            for (lane, chunk) in v.iter_mut().zip(rest[..16].as_chunks::<4>().0) {
                *lane = round(*lane, word(chunk));
            }
            rest = &rest[16..];
        }
        v[0].rotate_left(1)
            .wrapping_add(v[1].rotate_left(7))
            .wrapping_add(v[2].rotate_left(12))
            .wrapping_add(v[3].rotate_left(18))
    } else {
        seed.wrapping_add(PRIME5)
    };
    acc = acc.wrapping_add(bytes.len() as u32);
    while rest.len() >= 4 {
        acc = acc
            .wrapping_add(word(rest).wrapping_mul(PRIME3))
            .rotate_left(17)
            .wrapping_mul(PRIME4);
        rest = &rest[4..];
    }
    for &b in rest {
        acc = acc
            .wrapping_add(u32::from(b).wrapping_mul(PRIME5))
            .rotate_left(11)
            .wrapping_mul(PRIME1);
    }
    avalanche(acc)
}

/// `h32(s)`: xxHash32 of the UTF-8 bytes of `s`, seed 0.
pub fn h32(s: &str) -> u32 {
    hash(s.as_bytes(), 0)
}

/// `h64(s)`: the seed-0 digest in the high word, seed-1 in the low word.
pub fn h64(s: &str) -> u64 {
    u64::from(hash(s.as_bytes(), 0)) << 32 | u64::from(hash(s.as_bytes(), 1))
}

/// `h128(s)`: the digests for seeds 0 to 3, highest-order word first.
pub fn h128(s: &str) -> u128 {
    (0..4).fold(0, |r, seed| r << 32 | u128::from(hash(s.as_bytes(), seed)))
}
