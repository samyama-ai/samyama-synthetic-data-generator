//! Counter-based, seekable, deterministic PRNG streams and primary-key synthesizers.
//!
//! First-principles properties:
//! 1. **Stage & Field Stream Independence**: Each `(seed, entity_type, entity_index, field_name)`
//!    derives a mathematically isolated 64-bit counter state via SHA-256 / SplitMix64 mixing.
//!    Adding or modifying one field in an entity leaves every other field's values byte-identical.
//! 2. **O(1)-Memory Cross-Entity Recomputability**: Because any attribute of `(EntityType, i)`
//!    is a pure function of `(seed, EntityType, i, field_name)`, a child entity referencing
//!    parent index `p` can compute the parent's primary key OR any parent attribute (such as
//!    `Patient.dateOfBirth` for a temporal `@after` constraint) in `O(1)` time and `O(1)` RAM
//!    without storing parent rows in memory.
//! 3. **Thread-Count Invariance**: Partitioning `[0, N)` across `T` Rayon worker threads
//!    produces the exact same byte sequence for `T = 1` and `T = 64`.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt::Write;

/// Compute a deterministic 64-bit stream key from a string name using SHA-256 (big-endian first 8 bytes),
/// matching `src/sdg/core/rng.py`.
pub fn sha256_stream_key(name: &str) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(name.as_bytes());
    let digest = hasher.finalize();
    u64::from_be_bytes([
        digest[0], digest[1], digest[2], digest[3], digest[4], digest[5], digest[6], digest[7],
    ])
}

/// Fast 64-bit FNV-1a hash for compile-time or hot-loop identifiers.
#[inline]
pub const fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    let mut i = 0;
    while i < bytes.len() {
        h ^= bytes[i] as u64;
        h = h.wrapping_mul(0x100000001b3);
        i += 1;
    }
    h
}

/// Stafford variant 13 (SplitMix64) 64-bit avalanche mixer. Bijective on `u64`.
#[inline]
pub const fn splitmix64_mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
    z ^ (z >> 31)
}

/// Stateless, seekable counter-based PRNG.
#[derive(Clone, Debug)]
pub struct SeekableRng {
    state: u64,
    gamma: u64,
}

impl SeekableRng {
    /// Create a generator from raw 64-bit seed and stream key.
    #[inline]
    pub const fn new(seed: u64, stream_key: u64) -> Self {
        let s0 = splitmix64_mix(seed ^ stream_key.wrapping_mul(0x9e3779b97f4a7c15));
        // Ensure odd gamma for full 2^64 period Weyl sequence
        let gamma = splitmix64_mix(stream_key ^ 0xda942042e4dd58b5) | 1;
        Self { state: s0, gamma }
    }

    /// Create a deterministic stream for a named stage (e.g., `"topology"`, `"faults"`, `"alarms"`).
    pub fn for_stage(seed: u64, stage_name: &str) -> Self {
        Self::new(seed, sha256_stream_key(stage_name))
    }

    /// Create a deterministic PRNG for a specific `(entity_type_hash, entity_index, field_hash)`.
    #[inline]
    pub const fn for_field_hashes(
        seed: u64,
        entity_hash: u64,
        entity_index: u64,
        field_hash: u64,
    ) -> Self {
        let combined = splitmix64_mix(entity_hash ^ field_hash.rotate_left(29))
            ^ splitmix64_mix(entity_index.wrapping_mul(0x517cc1b727220a95));
        Self::new(seed, combined)
    }

    /// Convenience constructor for `(seed, entity_type, entity_index, field_name)`.
    #[inline]
    pub const fn for_field(
        seed: u64,
        entity_type: &str,
        entity_index: u64,
        field_name: &str,
    ) -> Self {
        Self::for_field_hashes(
            seed,
            fnv1a64(entity_type.as_bytes()),
            entity_index,
            fnv1a64(field_name.as_bytes()),
        )
    }

    /// Draw next 64-bit pseudorandom integer.
    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(self.gamma);
        splitmix64_mix(self.state)
    }

    /// Uniform float in `[0.0, 1.0)`.
    #[inline]
    pub fn next_f64(&mut self) -> f64 {
        const SCALE: f64 = 1.0 / ((1u64 << 53) as f64);
        ((self.next_u64() >> 11) as f64) * SCALE
    }

    /// Uniform integer in `[0, bound)`. Panics if `bound == 0`.
    #[inline]
    pub fn integers(&mut self, bound: u64) -> u64 {
        debug_assert!(bound > 0);
        if bound <= 1 {
            return 0;
        }
        // Lemire's nearly-divisionless unbiased range reduction
        let mut x = self.next_u64();
        let mut m = (x as u128).wrapping_mul(bound as u128);
        let mut l = m as u64;
        if l < bound {
            let t = bound.wrapping_neg() % bound;
            while l < t {
                x = self.next_u64();
                m = (x as u128).wrapping_mul(bound as u128);
                l = m as u64;
            }
        }
        (m >> 64) as u64
    }

    /// Uniform integer in `[min, max]` inclusive.
    #[inline]
    pub fn range_i64(&mut self, min: i64, max: i64) -> i64 {
        if max <= min {
            return min;
        }
        let span = (max as i128 - min as i128 + 1) as u64;
        min.wrapping_add(self.integers(span) as i64)
    }

    /// Uniform float in `[min, max)`.
    #[inline]
    pub fn uniform_f64(&mut self, min: f64, max: f64) -> f64 {
        if max <= min {
            return min;
        }
        min + (max - min) * self.next_f64()
    }

    /// Standard normal `N(0, 1)` via Box-Muller transform.
    #[inline]
    pub fn standard_normal(&mut self) -> f64 {
        let u1 = (1.0 - self.next_f64()).max(1e-15);
        let u2 = self.next_f64();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }

    /// Normal distribution `N(mean, std)`.
    #[inline]
    pub fn normal(&mut self, mean: f64, std: f64) -> f64 {
        mean + std * self.standard_normal()
    }

    /// Lognormal distribution parameterized by median and log-space sigma.
    #[inline]
    pub fn lognormal_median(&mut self, median: f64, sigma: f64) -> f64 {
        (median.max(1e-9).ln() + sigma * self.standard_normal()).exp()
    }

    /// Poisson distribution `Poisson(lambda)`.
    pub fn poisson(&mut self, lambda: f64) -> u64 {
        if lambda <= 0.0 {
            return 0;
        }
        if lambda < 30.0 {
            let l = (-lambda).exp();
            let mut k = 0u64;
            let mut p = 1.0;
            loop {
                k += 1;
                p *= self.next_f64().max(1e-15);
                if p <= l {
                    return k - 1;
                }
            }
        } else {
            // Normal approximation for large lambda
            (self.normal(lambda, lambda.sqrt()).round()).max(0.0) as u64
        }
    }

    /// Fast O(1) continuous-approximation Zipfian index sampler in `[0, n)` with exponent `s >= 0`.
    /// When `s <= 1e-6`, falls back to uniform sampling.
    #[inline]
    pub fn zipf_index(&mut self, n: u64, s: f64) -> u64 {
        if n <= 1 {
            return 0;
        }
        if s <= 1e-6 {
            return self.integers(n);
        }
        let u = self.next_f64();
        let n_f = n as f64;
        if (s - 1.0).abs() < 1e-6 {
            let idx = ((1.0 + n_f).powf(u) - 1.0).floor() as u64;
            idx.min(n - 1)
        } else {
            let one_minus_s = 1.0 - s;
            let hi = (n_f + 0.5).powf(one_minus_s);
            let lo = 0.5f64.powf(one_minus_s);
            let x = (u * (hi - lo) + lo).powf(1.0 / one_minus_s) - 0.5;
            (x.floor().max(0.0) as u64).min(n - 1)
        }
    }

    /// Choose a slice element uniformly.
    #[inline]
    pub fn choose<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        let idx = self.integers(items.len() as u64) as usize;
        &items[idx]
    }
}

/// Primary key generation strategies supporting O(1)-memory index bijection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PkKind {
    /// Deterministic RFC-4122 v4-formatted UUID derived bijectively from `(seed, entity_type, index)`.
    Uuid,
    /// Deterministic 26-character Crockford Base32 ULID derived from `(seed, entity_type, index)`.
    Ulid,
    /// Compact readable prefix format: `<prefix>-<index:08>` (e.g., `pat-00000042`).
    Prefixed(String),
}

impl PkKind {
    pub fn parse(spec: &str, default_prefix: &str) -> Self {
        let s = spec.trim();
        if s.eq_ignore_ascii_case("uuid") {
            Self::Uuid
        } else if s.eq_ignore_ascii_case("ulid") {
            Self::Ulid
        } else if let Some(pfx) = s.strip_prefix("prefix:") {
            Self::Prefixed(pfx.trim().to_string())
        } else {
            Self::Prefixed(default_prefix.to_lowercase())
        }
    }

    /// Format the deterministic primary key for `(seed, entity_hash, index)` directly into `out`.
    #[inline]
    pub fn write_pk(&self, seed: u64, entity_hash: u64, index: u64, out: &mut String) {
        match self {
            PkKind::Prefixed(pfx) => {
                let _ = write!(out, "{}-{:08}", pfx, index);
            }
            PkKind::Uuid => {
                // Bijective 128-bit hash from (seed, entity_hash, index)
                let hi = splitmix64_mix(seed ^ entity_hash ^ index.wrapping_mul(0x9e3779b97f4a7c15));
                let lo = splitmix64_mix(
                    seed.rotate_left(17)
                        ^ entity_hash.wrapping_mul(0xbf58476d1ce4e5b9)
                        ^ index,
                );
                // Set RFC-4122 version 4 and variant 1 bits
                let hi = (hi & 0xffffffffffff0fff) | 0x0000000000004000;
                let lo = (lo & 0x3fffffffffffffff) | 0x8000000000000000;
                let _ = write!(
                    out,
                    "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
                    (hi >> 32) as u32,
                    ((hi >> 16) & 0xffff) as u16,
                    (hi & 0xffff) as u16,
                    (lo >> 48) as u16,
                    lo & 0x0000ffffffffffff
                );
            }
            PkKind::Ulid => {
                const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
                // 48-bit monotonic timestamp base + 80-bit deterministic randomness
                let ts: u64 = 1_767_225_600_000u64 + (index / 1000);
                let r1 = splitmix64_mix(seed ^ entity_hash ^ index);
                let r2 = splitmix64_mix(r1 ^ index.rotate_left(31));
                let mut chars = [0u8; 26];
                // 10 timestamp chars
                let mut t = ts;
                for i in (0..10).rev() {
                    chars[i] = CROCKFORD[(t & 31) as usize];
                    t >>= 5;
                }
                // 16 randomness chars (80 bits from r1, r2)
                let mut v: u128 = ((r1 as u128) << 16) | ((r2 & 0xffff) as u128);
                for i in (10..26).rev() {
                    chars[i] = CROCKFORD[(v & 31) as usize];
                    v >>= 5;
                }
                out.push_str(std::str::from_utf8(&chars).unwrap_or("00000000000000000000000000"));
            }
        }
    }

    pub fn format_pk(&self, seed: u64, entity_hash: u64, index: u64) -> String {
        let mut s = String::with_capacity(36);
        self.write_pk(seed, entity_hash, index, &mut s);
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn test_seekable_rng_determinism_and_field_independence() {
        let mut r1 = SeekableRng::for_field(42, "Patient", 10, "email");
        let mut r2 = SeekableRng::for_field(42, "Patient", 10, "email");
        let mut r3 = SeekableRng::for_field(42, "Patient", 10, "age");
        let mut r4 = SeekableRng::for_field(42, "Patient", 11, "email");

        let v1 = r1.next_u64();
        let v2 = r2.next_u64();
        assert_eq!(v1, v2, "same (seed, entity, index, field) must produce identical values");
        assert_ne!(v1, r3.next_u64(), "different field must have independent stream");
        assert_ne!(v1, r4.next_u64(), "different entity index must have independent stream");
    }

    #[test]
    fn test_pk_bijective_uniqueness() {
        let kinds = [
            PkKind::Uuid,
            PkKind::Ulid,
            PkKind::Prefixed("pat".to_string()),
        ];
        let h = fnv1a64(b"Patient");
        for kind in &kinds {
            let mut seen = HashSet::new();
            for i in 0..10_000 {
                let pk = kind.format_pk(20261019, h, i);
                assert!(seen.insert(pk), "duplicate primary key at index {}", i);
            }
        }
    }
}
