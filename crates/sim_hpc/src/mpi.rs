//! The MPI transport (IB-05, feature `mpi`): [`Transport`] over an MPI communicator with rsmpi.
//! `MPI_THREAD_FUNNELED`: only each rank's main thread calls MPI; worker threads never do.
//!
//! Messages are byte vectors received with a matched probe, so a message is taken exactly once and
//! its length need not be known. Broadcasts send the length first, then the bytes in pieces of at
//! most 1 GiB (MPI counts are `int`).

use ::mpi::Threading;
use ::mpi::environment::Universe;
use ::mpi::point_to_point::{MatchedReceiveVec, Message, Status};
use ::mpi::topology::SimpleCommunicator;
use ::mpi::traits::*;

use crate::transport::{Envelope, Tag, Transport};

/// Largest piece of a broadcast.
const PIECE: usize = 1 << 30;

/// An initialized MPI job: dropping it finalizes MPI.
pub struct Mpi {
    world: SimpleCommunicator,
    _universe: Universe,
}

impl Mpi {
    /// Initialize MPI with `MPI_THREAD_FUNNELED`. `None` if MPI was already initialized or the
    /// library offers less than funneled threading.
    pub fn init() -> Option<Mpi> {
        let (universe, threading) = ::mpi::initialize_with_threading(Threading::Funneled)?;
        if threading == Threading::Single {
            return None;
        }
        Some(Mpi {
            world: universe.world(),
            _universe: universe,
        })
    }

    fn envelope(&self, msg: Message, status: Status) -> Envelope {
        let tag = Tag::from_i32(status.tag())
            .unwrap_or_else(|| panic!("an MPI message with unknown tag {}", status.tag()));
        let from = status.source_rank() as u32;
        let (bytes, _) = (msg, status).matched_receive_vec::<u8>();
        (from, tag, bytes)
    }
}

impl Transport for Mpi {
    fn rank(&self) -> u32 {
        self.world.rank() as u32
    }

    fn size(&self) -> u32 {
        self.world.size() as u32
    }

    fn broadcast(&self, bytes: Vec<u8>) -> Vec<u8> {
        let root = self.world.process_at_rank(0);
        let mut len = bytes.len() as u64;
        root.broadcast_into(&mut len);
        let mut buf = if self.rank() == 0 {
            bytes
        } else {
            vec![0u8; len as usize]
        };
        for piece in buf.chunks_mut(PIECE) {
            root.broadcast_into(piece);
        }
        buf
    }

    fn send(&self, to: u32, tag: Tag, bytes: Vec<u8>) {
        self.world
            .process_at_rank(to as i32)
            .send_with_tag(&bytes[..], tag as i32);
    }

    fn try_recv(&self) -> Option<Envelope> {
        let (msg, status) = self.world.any_process().immediate_matched_probe()?;
        Some(self.envelope(msg, status))
    }

    fn try_recv_tag(&self, tag: Tag) -> Option<Envelope> {
        let (msg, status) = self
            .world
            .any_process()
            .immediate_matched_probe_with_tag(tag as i32)?;
        Some(self.envelope(msg, status))
    }

    fn recv_tag(&self, tag: Tag) -> Envelope {
        let (msg, status) = self.world.any_process().matched_probe_with_tag(tag as i32);
        self.envelope(msg, status)
    }
}
