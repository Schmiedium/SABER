//! `SimRng`: the run's root random source and its named substreams (subsystems §5.1).
//!
//! The root is seeded from `(param_set_id, replicate_seed)`. Every consumer draws from its own
//! named [`RngStream`]: an independent ChaCha8 stream whose key is the root seed and whose stream
//! number is derived from the name. Adding a new noise source therefore never shifts any other
//! source's draws (test R9, `verification-and-test-strategy.md` §4.2).
//!
//! The samplers are implemented here rather than taken from `rand_distr`, so a dependency upgrade
//! can never change a draw. They use only IEEE arithmetic, `sqrt` and `ln` from the pinned build.

use bevy_ecs::prelude::*;
use rand_chacha::ChaCha8Rng;
use rand_core::{Rng, SeedableRng};

/// BLAKE3 key-derivation context for the root seed. Changing it changes every draw.
const ROOT_CONTEXT: &str = "SABER 2026-11-02 SimRng root seed v1";
/// BLAKE3 key-derivation context for stream numbers. Changing it changes every draw.
const STREAM_CONTEXT: &str = "SABER 2026-11-02 SimRng stream id v1";

/// The root random source of one run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SimRng {
    seed: [u8; 32],
}

impl SimRng {
    /// The root for one run, from its manifest identity.
    pub fn new(param_set_id: u32, replicate_seed: u64) -> Self {
        let mut material = [0u8; 12];
        material[..4].copy_from_slice(&param_set_id.to_le_bytes());
        material[4..].copy_from_slice(&replicate_seed.to_le_bytes());
        Self {
            seed: blake3::derive_key(ROOT_CONTEXT, &material),
        }
    }

    /// The root seed (hashed into the run's identity and crash dumps).
    pub fn seed(&self) -> [u8; 32] {
        self.seed
    }

    /// The named substream, e.g. `rng.stream("wind")`. The same name always gives the same
    /// sequence for the same root, whatever other streams exist or were drawn from.
    pub fn stream(&self, name: &str) -> RngStream {
        let id = stream_id(name);
        let mut rng = ChaCha8Rng::from_seed(self.seed);
        rng.set_stream(id);
        RngStream { rng, id }
    }
}

/// The 64-bit ChaCha stream number for a stream name.
pub fn stream_id(name: &str) -> u64 {
    let key = blake3::derive_key(STREAM_CONTEXT, name.as_bytes());
    let mut id = [0u8; 8];
    id.copy_from_slice(&key[..8]);
    u64::from_le_bytes(id)
}

/// One named, independent random stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RngStream {
    rng: ChaCha8Rng,
    id: u64,
}

impl RngStream {
    /// The stream number derived from its name.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// How many 32-bit words this stream has produced; part of the state hash
    /// (`verification-and-test-strategy.md` §4.1).
    pub fn word_pos(&self) -> u128 {
        self.rng.get_word_pos()
    }

    /// Uniform 64-bit integer.
    pub fn next_u64(&mut self) -> u64 {
        self.rng.next_u64()
    }

    /// Uniform in `[0, 1)` with 53 random bits.
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    /// Uniform in `(0, 1)` with 53 random bits, never exactly 0 or 1 (for inverse-CDF sampling).
    pub fn next_open01(&mut self) -> f64 {
        ((self.next_u64() >> 11) as f64 + 0.5) * (1.0 / (1u64 << 53) as f64)
    }

    /// Uniform in `[lo, hi)`.
    pub fn uniform(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.next_f64()
    }

    /// Standard normal by Marsaglia's polar method. The second variate of each accepted pair is
    /// discarded, so a stream's position depends only on how many normals were drawn.
    pub fn standard_normal(&mut self) -> f64 {
        loop {
            let u = 2.0 * self.next_f64() - 1.0;
            let v = 2.0 * self.next_f64() - 1.0;
            let s = u * u + v * v;
            if s > 0.0 && s < 1.0 {
                return u * (-2.0 * s.ln() / s).sqrt();
            }
        }
    }

    /// Normal with the given mean and standard deviation.
    pub fn normal(&mut self, mean: f64, std: f64) -> f64 {
        mean + std * self.standard_normal()
    }
}

/// A registered stream of the World's [`RngStreams`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StreamHandle(usize);

/// The World's random streams. Models register their named streams at Tick 0 (allocation allowed)
/// and draw through the handle afterwards. Registrations survive World reuse; at each run's Tick 0
/// every stream is re-derived from the new root.
#[derive(Resource, Clone, Debug)]
pub struct RngStreams {
    root: SimRng,
    names: Vec<String>,
    streams: Vec<RngStream>,
    by_id: Vec<usize>,
}

/// An empty placeholder with an all-zero root (not a run's root: the kernel installs one at every
/// Tick 0). It exists so a system can take the streams out of the World with
/// `sim_core::with_resource`, which, unlike `World::resource_scope`, never allocates.
impl Default for RngStreams {
    fn default() -> Self {
        Self::new(SimRng { seed: [0; 32] })
    }
}

impl RngStreams {
    /// Streams for a run with this root.
    pub fn new(root: SimRng) -> Self {
        Self {
            root,
            names: Vec::new(),
            streams: Vec::new(),
            by_id: Vec::new(),
        }
    }

    /// The run's root.
    pub fn root(&self) -> &SimRng {
        &self.root
    }

    /// The handle of the stream with this name, registering it on first use.
    ///
    /// # Panics
    /// If two different names map to the same 64-bit stream id (a BLAKE3 collision).
    pub fn register(&mut self, name: &str) -> StreamHandle {
        if let Some(i) = self.names.iter().position(|n| n == name) {
            return StreamHandle(i);
        }
        let stream = self.root.stream(name);
        let pos = self
            .by_id
            .binary_search_by_key(&stream.id(), |&i| self.streams[i].id())
            .expect_err("two random stream names share a stream id");
        let handle = self.streams.len();
        self.by_id.insert(pos, handle);
        self.names.push(name.to_owned());
        self.streams.push(stream);
        StreamHandle(handle)
    }

    /// The stream behind a handle.
    pub fn get(&self, handle: StreamHandle) -> &RngStream {
        &self.streams[handle.0]
    }

    /// The stream behind a handle, for drawing.
    pub fn get_mut(&mut self, handle: StreamHandle) -> &mut RngStream {
        &mut self.streams[handle.0]
    }

    /// Start a new run: new root, every registered stream back at its start.
    pub(crate) fn reseed(&mut self, root: SimRng) {
        for (stream, name) in self.streams.iter_mut().zip(&self.names) {
            *stream = root.stream(name);
        }
        self.root = root;
    }

    /// The streams drawn from in this run, in stream-id order (hashed into the state hash).
    pub fn drawn(&self) -> impl Iterator<Item = &RngStream> {
        self.by_id
            .iter()
            .map(|&i| &self.streams[i])
            .filter(|s| s.word_pos() > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_identity_same_draws() {
        let a: Vec<u64> = {
            let mut s = SimRng::new(7, 42).stream("wind");
            (0..16).map(|_| s.next_u64()).collect()
        };
        let b: Vec<u64> = {
            let mut s = SimRng::new(7, 42).stream("wind");
            (0..16).map(|_| s.next_u64()).collect()
        };
        assert_eq!(a, b);
    }

    #[test]
    fn identity_and_name_both_change_the_stream() {
        let first = |p, r, name| SimRng::new(p, r).stream(name).next_u64();
        let base = first(7, 42, "wind");
        assert_ne!(base, first(8, 42, "wind"));
        assert_ne!(base, first(7, 43, "wind"));
        assert_ne!(base, first(7, 42, "gnss"));
    }

    #[test]
    fn unit_interval_bounds() {
        let mut s = SimRng::new(0, 0).stream("u");
        for _ in 0..10_000 {
            let x = s.next_f64();
            assert!((0.0..1.0).contains(&x));
        }
    }

    #[test]
    fn word_position_counts_draws() {
        let mut s = SimRng::new(1, 2).stream("x");
        assert_eq!(s.word_pos(), 0);
        s.next_u64();
        assert_eq!(s.word_pos(), 2);
    }
}
