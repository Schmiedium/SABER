//! `Core.Recorder`: the black-box flight recorder (`Architecture.md` §6.1, subsystems §5.1).
//!
//! A fixed ring of `K` [`TickFrame`]s, each with room for every authored body and every fragmenting
//! body's fragment aggregates (IB-16; individual fragments are never recorded), allocated at Tick 0
//! and overwritten in place, so recording never allocates. The Tick-0 state is recorded at start,
//! then one frame per tick in `SimSet::Record`. A frame is committed (the cursor moves) only after
//! it is fully written, so every frame in the ring is complete.

use bevy_ecs::prelude::*;
use sim_schema::{BodySnapshot, TickFrame};

use crate::body::{Dof, PointMass3, RigidBody6};
use crate::clock::SimClock;
use crate::dynamics::Dynamics;
use crate::fragments::FragmentPool;
use crate::telemetry::TickEnergy;

/// Ring buffer of the last `K` tick frames.
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct FlightRecorder {
    frames: Vec<TickFrame>,
    cursor: usize,
    total_recorded: u64,
}

impl FlightRecorder {
    /// A recorder with `capacity` frames of room for `bodies` bodies each.
    pub fn new(capacity: usize, bodies: usize) -> Self {
        let mut r = Self::default();
        r.ensure(capacity, bodies, 0);
        r
    }

    /// Empty the ring and make sure it has `capacity` frames with room for `bodies` bodies and
    /// `parents` fragment clouds each (Tick 0). Allocates only when the existing buffers are too
    /// small.
    pub fn ensure(&mut self, capacity: usize, bodies: usize, parents: usize) {
        let capacity = capacity.max(1);
        self.frames.truncate(capacity);
        self.frames.resize_with(capacity, TickFrame::default);
        for f in &mut self.frames {
            f.bodies.clear();
            f.bodies.reserve(bodies);
            f.fragments.clear();
            f.fragments.reserve(parents);
        }
        self.cursor = 0;
        self.total_recorded = 0;
    }

    /// Number of frame slots `K`.
    pub fn capacity(&self) -> usize {
        self.frames.len()
    }

    /// Frames currently held (at most `K`).
    pub fn len(&self) -> usize {
        self.total_recorded.min(self.frames.len() as u64) as usize
    }

    /// Whether nothing has been recorded since Tick 0.
    pub fn is_empty(&self) -> bool {
        self.total_recorded == 0
    }

    /// Frames recorded in this run, including overwritten ones.
    pub fn total_recorded(&self) -> u64 {
        self.total_recorded
    }

    /// The slot for the next frame, emptied (capacity kept). Call [`FlightRecorder::commit`]
    /// once it is written.
    pub fn begin_frame(&mut self) -> &mut TickFrame {
        let frame = &mut self.frames[self.cursor];
        frame.bodies.clear();
        frame.fragments.clear();
        frame
    }

    /// Make the frame written since [`FlightRecorder::begin_frame`] part of the record.
    pub fn commit(&mut self) {
        self.cursor = (self.cursor + 1) % self.frames.len();
        self.total_recorded += 1;
    }

    /// The most recent committed frame.
    pub fn latest(&self) -> Option<&TickFrame> {
        (self.total_recorded > 0).then(|| &self.frames[self.latest_index()])
    }

    fn latest_index(&self) -> usize {
        (self.cursor + self.frames.len() - 1) % self.frames.len()
    }

    /// Set `flag` on body `sim_id` in the most recent frame.
    pub fn flag_latest(&mut self, sim_id: u32, flag: u8) {
        if self.total_recorded == 0 {
            return;
        }
        let i = self.latest_index();
        if let Some(b) = self.frames[i]
            .bodies
            .iter_mut()
            .find(|b| b.sim_id == sim_id)
        {
            b.flags |= flag;
        }
    }

    /// The held frames, oldest first.
    pub fn chronological(&self) -> impl Iterator<Item = &TickFrame> {
        let count = self.len();
        let start = if self.total_recorded >= self.frames.len() as u64 {
            self.cursor
        } else {
            0
        };
        (0..count).map(move |i| &self.frames[(start + i) % self.frames.len()])
    }

    /// A copy of the held frames, oldest first, for a crash dump.
    pub fn drain_chronological(&self) -> Vec<TickFrame> {
        self.chronological().cloned().collect()
    }
}

/// Write the current state as the next frame.
pub(crate) fn record_frame(world: &mut World) {
    crate::with_resource(world, |world, recorder: &mut FlightRecorder| {
        let dynamics = world.resource::<Dynamics>();
        let clock = world.resource::<SimClock>();
        let energy = world.resource::<TickEnergy>();
        let frame = recorder.begin_frame();
        frame.tick = clock.tick;
        frame.t_s = clock.time();
        frame.total_energy = if energy.tick == clock.tick {
            energy.total
        } else {
            f64::NAN
        };
        for (i, e) in dynamics.bodies().entries().iter().enumerate() {
            let mut snap = BodySnapshot {
                sim_id: e.sim_id.0,
                quat_wxyz: [1.0, 0.0, 0.0, 0.0],
                force_mag: dynamics.tick_wrench(i).force.length(),
                ..BodySnapshot::default()
            };
            match e.dof {
                Dof::Six => {
                    let b = world.get::<RigidBody6>(e.entity).expect("body");
                    snap.pos = b.position.to_array();
                    snap.lin_vel = b.velocity.to_array();
                    let q = b.attitude;
                    snap.quat_wxyz = [q.w, q.x, q.y, q.z];
                    snap.ang_vel = b.body_rates.to_array();
                }
                Dof::Three => {
                    let b = world.get::<PointMass3>(e.entity).expect("body");
                    snap.pos = b.position.to_array();
                    snap.lin_vel = b.velocity.to_array();
                }
            }
            frame.bodies.push(snap);
        }
        world
            .resource::<FragmentPool>()
            .snapshot_into(&mut frame.fragments);
        recorder.commit();
    });
}
