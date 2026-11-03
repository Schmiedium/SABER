//! Determinism hashes (verification §4.1, subsystems §10.1).
//!
//! `state_hash` is BLAKE3 over `sim_config_hash`, then for each authored body in `SimId` order its
//! id, DOF tag and the `to_bits()` of every integrated `f64` in packed order (position, velocity,
//! attitude `[w, x, y, z]`, body rates), then the `SimClock` tick and the position of every random
//! stream that has been drawn from (in stream-id order). Streams never drawn from are left out, so
//! a plugin that registers a stream but has nothing to do does not change the hash (R10). The same
//! function produces `final_state_hash` in the summaries, so tests and production agree.
//!
//! Every hash is the first 8 bytes of a BLAKE3 output, little-endian. `-0.0` and `+0.0` hash
//! differently and NaN payloads are hashed as they are, which is what a determinism hash wants.

use bevy_ecs::prelude::*;

use crate::body::{Dof, PointMass3, RigidBody6};
use crate::clock::SimClock;
use crate::dynamics::Dynamics;
use crate::rng::RngStreams;

const STATE_CONTEXT: &str = "SABER 2026-11-02 state hash v1";
const ROLLING_CONTEXT: &str = "SABER 2026-11-02 rolling state hash v1";
pub(crate) const CONFIG_CONTEXT: &str = "SABER 2026-11-02 sim config hash v1";

/// The run's identity hashes.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RunIdentity {
    /// Hash of the scene file (`scene_hash`, build step 1c); 0 for a World built in code.
    pub scene_hash: u64,
    /// Hash of the scene hash and the `SimConfig` (physics §10.5).
    pub sim_config_hash: u64,
}

pub(crate) fn finish(hasher: &blake3::Hasher) -> u64 {
    let mut out = [0u8; 8];
    out.copy_from_slice(&hasher.finalize().as_bytes()[..8]);
    u64::from_le_bytes(out)
}

/// The state hash of the World's current state. Allocation-free once the World has started.
pub fn state_hash(world: &World) -> u64 {
    let mut h = blake3::Hasher::new_derive_key(STATE_CONTEXT);
    h.update(
        &world
            .resource::<RunIdentity>()
            .sim_config_hash
            .to_le_bytes(),
    );
    for e in world.resource::<Dynamics>().bodies().entries() {
        h.update(&e.sim_id.0.to_le_bytes());
        match e.dof {
            Dof::Six => {
                let b = world.get::<RigidBody6>(e.entity).expect("body");
                let q = b.attitude;
                h.update(&[6]);
                for v in b.position.to_array() {
                    h.update(&v.to_bits().to_le_bytes());
                }
                for v in b.velocity.to_array() {
                    h.update(&v.to_bits().to_le_bytes());
                }
                for v in [q.w, q.x, q.y, q.z] {
                    h.update(&v.to_bits().to_le_bytes());
                }
                for v in b.body_rates.to_array() {
                    h.update(&v.to_bits().to_le_bytes());
                }
            }
            Dof::Three => {
                let b = world.get::<PointMass3>(e.entity).expect("body");
                h.update(&[3]);
                for v in b.position.to_array() {
                    h.update(&v.to_bits().to_le_bytes());
                }
                for v in b.velocity.to_array() {
                    h.update(&v.to_bits().to_le_bytes());
                }
            }
        }
    }
    h.update(&world.resource::<SimClock>().tick.to_le_bytes());
    for s in world.resource::<RngStreams>().drawn() {
        h.update(&s.id().to_le_bytes());
        h.update(&s.word_pos().to_le_bytes());
    }
    finish(&h)
}

/// Per-tick rolling hash `hash_k = H(hash_{k−1} ‖ state_hash_k)` (verification §4.1), for finding
/// the first tick at which two runs diverge. Present only when switched on with
/// `SimWorld::enable_rolling_hash`; it costs one state hash per tick.
#[derive(Resource, Clone, Debug, Default, PartialEq, Eq)]
pub struct RollingHash {
    /// The rolling hash after the latest tick.
    pub value: u64,
    /// `(tick, rolling hash)` after every tick of the run, starting with Tick 0, when `keep_history`
    /// was set (allocates as it grows; for tests and bisection).
    pub history: Vec<(u64, u64)>,
    /// Whether to keep `history`.
    pub keep_history: bool,
}

impl RollingHash {
    pub(crate) fn restart(&mut self, world_state: u64, tick: u64) {
        self.value = 0;
        self.history.clear();
        self.push(world_state, tick);
    }

    fn push(&mut self, state: u64, tick: u64) {
        let mut h = blake3::Hasher::new_derive_key(ROLLING_CONTEXT);
        h.update(&self.value.to_le_bytes());
        h.update(&state.to_le_bytes());
        self.value = finish(&h);
        if self.keep_history {
            self.history.push((tick, self.value));
        }
    }
}

/// `SimSet::Watchdog` (last core system): fold this tick's state into the rolling hash.
pub(crate) fn rolling_hash_system(world: &mut World) {
    let state = state_hash(world);
    let tick = world.resource::<SimClock>().tick;
    world.resource_mut::<RollingHash>().push(state, tick);
}
