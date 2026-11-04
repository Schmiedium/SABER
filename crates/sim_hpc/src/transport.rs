//! How ranks talk (IB-05): the handful of operations the campaign runner needs, behind one trait so
//! the same rank code runs over MPI on a cluster (`sim_hpc::mpi`, feature `mpi`), as one process
//! with `--local` ([`Single`]), and as several ranks in one test process ([`Threads`]).
//!
//! Only each rank's main thread calls a transport (MPI `Funneled`). Messages are byte vectors with a
//! [`Tag`]; the payloads are [`crate::protocol`]'s bincode encodings.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Barrier, Mutex};

/// Message tags (IB-05).
#[repr(i32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tag {
    /// Rank N → 0: `ChunkRequest`.
    RequestWork = 1,
    /// Rank 0 → N: `ChunkAssign`.
    AssignWork = 2,
    /// Rank 0 → N: no more work.
    Terminate = 3,
    /// Rank 0 → N: stop now (`AbortMsg`).
    Abort = 4,
    /// Rank N → 0: one rank's part of a gather.
    Gather = 5,
    /// Rank N → 0: `NodeReport`, the rank's last message.
    Report = 6,
}

impl Tag {
    /// The tag of a wire value.
    pub fn from_i32(v: i32) -> Option<Tag> {
        [
            Tag::RequestWork,
            Tag::AssignWork,
            Tag::Terminate,
            Tag::Abort,
            Tag::Gather,
            Tag::Report,
        ]
        .into_iter()
        .find(|t| *t as i32 == v)
    }
}

/// A received message: sender, tag, payload.
pub type Envelope = (u32, Tag, Vec<u8>);

/// What the campaign runner needs from a communicator.
pub trait Transport {
    /// This rank.
    fn rank(&self) -> u32;
    /// Number of ranks.
    fn size(&self) -> u32;
    /// Broadcast from rank 0: rank 0 passes its bytes, every other rank gets them back.
    fn broadcast(&self, bytes: Vec<u8>) -> Vec<u8>;
    /// Send a tagged message.
    fn send(&self, to: u32, tag: Tag, bytes: Vec<u8>);
    /// The next message with any tag, if one has arrived. Messages from one sender arrive in the
    /// order they were sent.
    fn try_recv(&self) -> Option<Envelope>;
    /// The next message with this tag, if one has arrived; messages with other tags stay queued.
    fn try_recv_tag(&self, tag: Tag) -> Option<Envelope>;
    /// Wait for the next message with this tag; messages with other tags stay queued.
    fn recv_tag(&self, tag: Tag) -> Envelope;
    /// Gather at rank 0, in rank order (rank 0 gets every rank's bytes, the others `None`).
    fn gather(&self, bytes: Vec<u8>) -> Option<Vec<Vec<u8>>> {
        if self.rank() != 0 {
            self.send(0, Tag::Gather, bytes);
            return None;
        }
        let mut out = vec![Vec::new(); self.size() as usize];
        out[0] = bytes;
        for _ in 1..self.size() {
            let (from, _, b) = self.recv_tag(Tag::Gather);
            out[from as usize] = b;
        }
        Some(out)
    }
}

/// One rank, no communication: `sim_hpc --local`.
#[derive(Debug, Default)]
pub struct Single;

impl Transport for Single {
    fn rank(&self) -> u32 {
        0
    }
    fn size(&self) -> u32 {
        1
    }
    fn broadcast(&self, bytes: Vec<u8>) -> Vec<u8> {
        bytes
    }
    fn send(&self, to: u32, tag: Tag, _: Vec<u8>) {
        panic!("a single rank has no rank {to} to send {tag:?} to");
    }
    fn try_recv(&self) -> Option<Envelope> {
        None
    }
    fn try_recv_tag(&self, _: Tag) -> Option<Envelope> {
        None
    }
    fn recv_tag(&self, tag: Tag) -> Envelope {
        panic!("a single rank never receives {tag:?}");
    }
}

/// Ranks as threads of one process, over channels: the MPI protocol without MPI, for tests.
pub struct Threads {
    rank: u32,
    peers: Vec<Sender<Envelope>>,
    rx: Mutex<Receiver<Envelope>>,
    held: Mutex<VecDeque<Envelope>>,
    bcast_tx: Vec<Sender<Vec<u8>>>,
    bcast_rx: Mutex<Receiver<Vec<u8>>>,
    barrier: Arc<Barrier>,
}

impl Threads {
    /// `n` connected ranks; hand one to each thread.
    pub fn connect(n: u32) -> Vec<Threads> {
        assert!(n >= 1, "at least one rank");
        let (txs, rxs): (Vec<_>, Vec<_>) = (0..n).map(|_| channel::<Envelope>()).unzip();
        let (btxs, brxs): (Vec<_>, Vec<_>) = (0..n).map(|_| channel::<Vec<u8>>()).unzip();
        let barrier = Arc::new(Barrier::new(n as usize));
        rxs.into_iter()
            .zip(brxs)
            .enumerate()
            .map(|(rank, (rx, brx))| Threads {
                rank: rank as u32,
                peers: txs.clone(),
                rx: Mutex::new(rx),
                held: Mutex::new(VecDeque::new()),
                bcast_tx: btxs.clone(),
                bcast_rx: Mutex::new(brx),
                barrier: barrier.clone(),
            })
            .collect()
    }
}

impl Transport for Threads {
    fn rank(&self) -> u32 {
        self.rank
    }
    fn size(&self) -> u32 {
        self.peers.len() as u32
    }
    fn broadcast(&self, bytes: Vec<u8>) -> Vec<u8> {
        let out = if self.rank == 0 {
            for tx in &self.bcast_tx[1..] {
                let _ = tx.send(bytes.clone());
            }
            bytes
        } else {
            self.bcast_rx
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .recv()
                .expect("rank 0 broadcasts")
        };
        self.barrier.wait();
        out
    }
    fn send(&self, to: u32, tag: Tag, bytes: Vec<u8>) {
        // A rank that already finished has dropped its receiver; like MPI's eager sends, the
        // message is simply never read.
        let _ = self.peers[to as usize].send((self.rank, tag, bytes));
    }
    fn try_recv(&self) -> Option<Envelope> {
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(m) = held.pop_front() {
            return Some(m);
        }
        self.rx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .try_recv()
            .ok()
    }
    fn try_recv_tag(&self, tag: Tag) -> Option<Envelope> {
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        let rx = self.rx.lock().unwrap_or_else(|e| e.into_inner());
        held.extend(rx.try_iter());
        let i = held.iter().position(|m| m.1 == tag)?;
        held.remove(i)
    }
    fn recv_tag(&self, tag: Tag) -> Envelope {
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = held.iter().position(|m| m.1 == tag) {
            return held.remove(i).expect("found");
        }
        let rx = self.rx.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            let m = rx.recv().expect("a peer holds a sender");
            if m.1 == tag {
                return m;
            }
            held.push_back(m);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threads_broadcast_send_and_gather() {
        let ranks = Threads::connect(3);
        std::thread::scope(|s| {
            for t in &ranks {
                s.spawn(move || {
                    let b = t.broadcast(if t.rank() == 0 { vec![7, 8] } else { vec![] });
                    assert_eq!(b, vec![7, 8]);
                    if t.rank() != 0 {
                        t.send(0, Tag::RequestWork, vec![t.rank() as u8]);
                    } else {
                        let mut from: Vec<u32> =
                            (1..3).map(|_| t.recv_tag(Tag::RequestWork).0).collect();
                        from.sort();
                        assert_eq!(from, vec![1, 2]);
                    }
                    let g = t.gather(vec![t.rank() as u8 * 10]);
                    if t.rank() == 0 {
                        assert_eq!(g, Some(vec![vec![0], vec![10], vec![20]]));
                    } else {
                        assert_eq!(g, None);
                    }
                });
            }
        });
    }

    #[test]
    fn recv_tag_keeps_other_messages_queued() {
        let ranks = Threads::connect(2);
        ranks[1].send(0, Tag::RequestWork, vec![1]);
        ranks[1].send(0, Tag::Gather, vec![2]);
        ranks[1].send(0, Tag::Report, vec![3]);
        assert_eq!(ranks[0].recv_tag(Tag::Gather), (1, Tag::Gather, vec![2]));
        assert_eq!(
            ranks[0].try_recv_tag(Tag::Report),
            Some((1, Tag::Report, vec![3]))
        );
        assert_eq!(ranks[0].try_recv_tag(Tag::Report), None);
        assert_eq!(ranks[0].try_recv(), Some((1, Tag::RequestWork, vec![1])));
        assert_eq!(ranks[0].try_recv(), None);
        assert_eq!(Tag::from_i32(4), Some(Tag::Abort));
        assert_eq!(Tag::from_i32(9), None);
    }
}
