//! What the session publishes to attached devices: a ring of recent events under a stream epoch,
//! and one bounded outbound queue per device. The session's loop calls into it and never awaits:
//! a device that cannot keep up is disconnected, not waited for.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

use tokio::sync::Notify;

use crate::protocol::{
    HOST_FRAME_CAP, HostMsg, Level, Reason, SNAPSHOT_PART_CAP, Sender, encode_frame, split_snapshot,
};

/// The bounds the ring and each device's queue keep.
#[derive(Clone, Copy, Debug)]
pub struct HubLimits {
    pub ring_events: usize,
    pub ring_bytes: usize,
    pub queue_events: usize,
    pub queue_bytes: usize,
}

impl HubLimits {
    /// The most snapshot bytes a device's queue accepts at once.
    fn snapshot_cap(&self) -> usize {
        self.queue_bytes.saturating_mul(4)
    }
}

impl Default for HubLimits {
    fn default() -> HubLimits {
        HubLimits {
            ring_events: 4096,
            ring_bytes: 16 * 1024 * 1024,
            queue_events: 256,
            queue_bytes: 8 * 1024 * 1024,
        }
    }
}

/// How a device's connection ends once its queue has drained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Close {
    /// Tell the device why, then close.
    Reject(Reason),
    /// A clean end the device may reconnect after.
    Bye,
    /// The device fell behind: it was told by a notice and reconnects with `Resume`.
    Overflow,
}

struct QueueState {
    frames: VecDeque<Vec<u8>>,
    bytes: usize,
    /// Snapshot frames at the front of `frames`: they are bounded by `snapshot_cap`, not by the
    /// queue's own bounds, or a snapshot over `queue_bytes` could never be delivered.
    snapshot_frames: usize,
    closing: Option<Close>,
}

/// One device's outbound queue. Pushing never waits.
pub struct DeviceQueue {
    state: Mutex<QueueState>,
    wake: Notify,
    limits: HubLimits,
}

/// What the writer takes from a queue.
pub enum Next {
    Frames(Vec<Vec<u8>>),
    Closed(Close),
    Empty,
}

impl DeviceQueue {
    fn new(limits: HubLimits) -> Arc<DeviceQueue> {
        Arc::new(DeviceQueue {
            state: Mutex::new(QueueState {
                frames: VecDeque::new(),
                bytes: 0,
                snapshot_frames: 0,
                closing: None,
            }),
            wake: Notify::new(),
            limits,
        })
    }

    fn state(&self) -> MutexGuard<'_, QueueState> {
        self.state.lock().expect("a device queue lock is poisoned")
    }

    /// Queues one frame; a queue past its bounds is cleared, left with a notice, and closed.
    pub(crate) fn push(&self, frame: Vec<u8>) {
        let mut state = self.state();
        if state.closing.is_some() {
            return;
        }
        if state.frames.len() - state.snapshot_frames >= self.limits.queue_events
            || state.bytes + frame.len() > self.limits.queue_bytes
        {
            state.frames.clear();
            let notice = encode_frame(&HostMsg::Notice {
                severity: Level::Warning,
                text: "this device fell behind the session and was disconnected; reconnecting"
                    .to_string(),
            });
            state.bytes = notice.len();
            state.snapshot_frames = 0;
            state.frames.push_back(notice);
            state.closing = Some(Close::Overflow);
        } else {
            state.bytes += frame.len();
            state.frames.push_back(frame);
        }
        drop(state);
        self.wake.notify_one();
    }

    /// Queues a snapshot's parts outside the overflow bounds. A snapshot over `snapshot_cap`
    /// bytes is refused with a notice and the device is closed with `TooLarge`.
    fn push_snapshot(&self, parts: &[Vec<u8>]) {
        let total: usize = parts.iter().map(Vec::len).sum();
        let mut state = self.state();
        if state.closing.is_some() {
            return;
        }
        if total > self.limits.snapshot_cap() {
            state.frames.clear();
            state.snapshot_frames = 0;
            let notice = encode_frame(&HostMsg::Notice {
                severity: Level::Warning,
                text: "the session is too large to attach to remotely".to_string(),
            });
            state.bytes = notice.len();
            state.frames.push_back(notice);
            state.closing = Some(Close::Reject(Reason::TooLarge));
        } else {
            // Snapshot parts precede anything already queued only if nothing is queued: the
            // device was awaiting the snapshot, so its queue holds no events.
            state.snapshot_frames += parts.len();
            state.frames.extend(parts.iter().cloned());
        }
        drop(state);
        self.wake.notify_one();
    }

    /// Ends the queue after what is already in it; the first reason wins.
    pub fn close(&self, how: Close) {
        let mut state = self.state();
        if state.closing.is_none() {
            state.closing = Some(how);
        }
        drop(state);
        self.wake.notify_one();
    }

    pub fn take(&self) -> Next {
        let mut state = self.state();
        if !state.frames.is_empty() {
            state.bytes = 0;
            state.snapshot_frames = 0;
            return Next::Frames(state.frames.drain(..).collect());
        }
        match state.closing {
            Some(how) => Next::Closed(how),
            None => Next::Empty,
        }
    }

    pub async fn wait(&self) {
        self.wake.notified().await;
    }
}

struct DeviceEntry {
    conn: u64,
    label: String,
    queue: Arc<DeviceQueue>,
    /// Joined by a `Snapshot` the session still has to provide; events skip it until then.
    awaiting_snapshot: bool,
}

#[derive(Default)]
struct HubState {
    seq: u64,
    ring: VecDeque<(u64, Arc<Vec<u8>>)>,
    ring_bytes: usize,
    devices: HashMap<Sender, DeviceEntry>,
    next_conn: u64,
    snapshot_wanted: bool,
    closed: bool,
}

/// Wakes the session's loop when a device needs a snapshot or the device list changed.
pub trait Waker: Send + Sync {
    fn wake(&self);
}

pub struct Hub {
    epoch: u64,
    limits: HubLimits,
    state: Mutex<HubState>,
    waker: Arc<dyn Waker>,
}

/// A device's place in the hub; dropping it ends the registration.
pub struct Registration {
    hub: Arc<Hub>,
    node: Sender,
    conn: u64,
    pub queue: Arc<DeviceQueue>,
}

impl Drop for Registration {
    fn drop(&mut self) {
        let mut state = self.hub.state();
        if state
            .devices
            .get(&self.node)
            .is_some_and(|d| d.conn == self.conn)
        {
            state.devices.remove(&self.node);
        }
    }
}

impl Hub {
    pub fn new(limits: HubLimits, waker: Arc<dyn Waker>) -> Arc<Hub> {
        let mut id = [0u8; 8];
        id.copy_from_slice(&uuid::Uuid::new_v4().as_bytes()[..8]);
        Arc::new(Hub {
            epoch: u64::from_be_bytes(id),
            limits,
            state: Mutex::new(HubState::default()),
            waker,
        })
    }

    fn state(&self) -> MutexGuard<'_, HubState> {
        self.state.lock().expect("the hub lock is poisoned")
    }

    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    pub fn seq(&self) -> u64 {
        self.state().seq
    }

    pub fn device_count(&self) -> usize {
        self.state().devices.len()
    }

    pub fn devices(&self) -> Vec<(Sender, String)> {
        let mut list: Vec<_> = self
            .state()
            .devices
            .iter()
            .map(|(node, d)| (node.clone(), d.label.clone()))
            .collect();
        list.sort();
        list
    }

    /// Assigns the next sequence number, keeps the event in the ring and queues it to every
    /// device that is live. Called from the session's loop only, so the sequence and the
    /// snapshots the loop provides stay in step.
    pub fn publish(&self, event: serde_json::Value) {
        let mut state = self.state();
        if state.closed {
            return;
        }
        state.seq += 1;
        let seq = state.seq;
        let mut frame = encode_frame(&HostMsg::Event {
            epoch: self.epoch,
            seq,
            event,
        });
        if frame.len() > HOST_FRAME_CAP {
            // The mirror truncates its own text fields; this is the backstop for anything else.
            frame = encode_frame(&HostMsg::Event {
                epoch: self.epoch,
                seq,
                event: serde_json::json!({ "kind": "oversized" }),
            });
        }
        let frame = Arc::new(frame);
        state.ring_bytes += frame.len();
        state.ring.push_back((seq, frame.clone()));
        while state.ring.len() > self.limits.ring_events
            || (state.ring_bytes > self.limits.ring_bytes && state.ring.len() > 1)
        {
            if let Some((_, old)) = state.ring.pop_front() {
                state.ring_bytes -= old.len();
            }
        }
        for device in state.devices.values().filter(|d| !d.awaiting_snapshot) {
            device.queue.push(frame.as_ref().clone());
        }
    }

    /// A notice for every attached device. Not replayed on resume.
    pub fn notice(&self, severity: Level, text: &str) {
        let frame = encode_frame(&HostMsg::Notice {
            severity,
            text: text.to_string(),
        });
        for device in self.state().devices.values() {
            device.queue.push(frame.clone());
        }
    }

    pub fn notice_to(&self, node: &Sender, severity: Level, text: &str) {
        let frame = encode_frame(&HostMsg::Notice {
            severity,
            text: text.to_string(),
        });
        if let Some(device) = self.state().devices.get(node) {
            device.queue.push(frame);
        }
    }

    /// Registers a device. With an epoch and a last sequence number the ring still covers, the
    /// gap is replayed; otherwise the device waits for a snapshot from the session.
    pub fn register(
        self: &Arc<Hub>,
        node: &Sender,
        label: &str,
        resume: Option<(u64, u64)>,
    ) -> Option<Registration> {
        let mut state = self.state();
        if state.closed {
            return None;
        }
        state.next_conn += 1;
        let conn = state.next_conn;
        let queue = DeviceQueue::new(self.limits);
        let replay = resume
            .filter(|(epoch, _)| *epoch == self.epoch)
            .and_then(|(_, last)| self.gap_after(&state, last));
        let mut awaiting_snapshot = true;
        if let Some(frames) = replay {
            let fits = frames.len() <= self.limits.queue_events
                && frames.iter().map(|f| f.len()).sum::<usize>() <= self.limits.queue_bytes;
            if fits {
                awaiting_snapshot = false;
                for frame in frames {
                    queue.push(frame.as_ref().clone());
                }
            }
        }
        if awaiting_snapshot {
            state.snapshot_wanted = true;
        }
        if let Some(old) = state.devices.insert(
            node.clone(),
            DeviceEntry {
                conn,
                label: label.to_string(),
                queue: queue.clone(),
                awaiting_snapshot,
            },
        ) {
            old.queue.close(Close::Reject(Reason::Superseded));
        }
        drop(state);
        if awaiting_snapshot {
            self.waker.wake();
        }
        Some(Registration {
            hub: self.clone(),
            node: node.clone(),
            conn,
            queue,
        })
    }

    /// The frames after `last`, when the ring still holds every one of them.
    fn gap_after(&self, state: &HubState, last: u64) -> Option<Vec<Arc<Vec<u8>>>> {
        if last > state.seq {
            return None;
        }
        if last == state.seq {
            return Some(Vec::new());
        }
        let first = state.ring.front()?.0;
        if first > last + 1 {
            return None;
        }
        Some(
            state
                .ring
                .iter()
                .filter(|(seq, _)| *seq > last)
                .map(|(_, frame)| frame.clone())
                .collect(),
        )
    }

    /// Whether a device is waiting for a snapshot.
    pub fn snapshot_wanted(&self) -> bool {
        self.state().snapshot_wanted
    }

    /// The session's state as of the latest published event, for every device waiting on one.
    pub fn provide_snapshot(&self, state_json: &str) {
        let mut state = self.state();
        state.snapshot_wanted = false;
        let seq = state.seq;
        let parts: Vec<Vec<u8>> = split_snapshot(self.epoch, seq, state_json, SNAPSHOT_PART_CAP)
            .iter()
            .map(encode_frame)
            .collect();
        for device in state.devices.values_mut().filter(|d| d.awaiting_snapshot) {
            device.awaiting_snapshot = false;
            device.queue.push_snapshot(&parts);
        }
    }

    /// Closes one device's connection after what it has queued; false when it is not attached.
    pub fn close_device(&self, node: &str, how: Close) -> bool {
        match self.state().devices.get(node) {
            Some(device) => {
                device.queue.close(how);
                true
            }
            None => false,
        }
    }

    /// Ends every device and refuses new ones.
    pub fn close_all(&self, how: Close) {
        let mut state = self.state();
        state.closed = true;
        for device in state.devices.values() {
            device.queue.close(how);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Counting(Mutex<u32>);
    impl Waker for Counting {
        fn wake(&self) {
            *self.0.lock().unwrap() += 1;
        }
    }

    fn hub(limits: HubLimits) -> (Arc<Hub>, Arc<Counting>) {
        let waker = Arc::new(Counting(Mutex::new(0)));
        (Hub::new(limits, waker.clone()), waker)
    }

    fn seqs(queue: &DeviceQueue) -> Vec<u64> {
        match queue.take() {
            Next::Frames(frames) => frames
                .iter()
                .filter_map(|f| match serde_json::from_slice::<HostMsg>(f) {
                    Ok(HostMsg::Event { seq, .. }) => Some(seq),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    fn node(name: &str) -> Sender {
        name.to_string()
    }

    #[test]
    fn a_resume_inside_the_ring_replays_exactly_the_gap() {
        let (hub, _) = hub(HubLimits::default());
        for i in 0..5 {
            hub.publish(serde_json::json!({ "n": i }));
        }
        let reg = hub
            .register(&node("a"), "l", Some((hub.epoch(), 2)))
            .unwrap();
        assert_eq!(seqs(&reg.queue), vec![3, 4, 5]);
        assert!(!hub.snapshot_wanted());
        hub.publish(serde_json::json!({ "n": 6 }));
        assert_eq!(seqs(&reg.queue), vec![6]);
    }

    #[test]
    fn a_gap_that_fell_out_of_the_ring_needs_a_snapshot() {
        let (hub, waker) = hub(HubLimits {
            ring_events: 3,
            ..HubLimits::default()
        });
        for i in 0..10 {
            hub.publish(serde_json::json!({ "n": i }));
        }
        let reg = hub
            .register(&node("a"), "l", Some((hub.epoch(), 2)))
            .unwrap();
        assert!(hub.snapshot_wanted());
        assert_eq!(*waker.0.lock().unwrap(), 1);
        assert!(seqs(&reg.queue).is_empty());
        hub.provide_snapshot("{\"state\":1}");
        match reg.queue.take() {
            Next::Frames(frames) => {
                let msg: HostMsg = serde_json::from_slice(&frames[0]).unwrap();
                assert!(matches!(
                    msg,
                    HostMsg::Snapshot {
                        seq: 10,
                        part: 0,
                        of: 1,
                        ..
                    }
                ));
            }
            _ => panic!("a snapshot"),
        }
        hub.publish(serde_json::json!({}));
        assert_eq!(seqs(&reg.queue), vec![11]);
    }

    #[test]
    fn a_stale_or_absent_epoch_gets_a_snapshot_not_a_replay() {
        let (hub, _) = hub(HubLimits::default());
        hub.publish(serde_json::json!({}));
        let stale = hub
            .register(&node("a"), "l", Some((hub.epoch() ^ 1, 1)))
            .unwrap();
        assert!(hub.snapshot_wanted());
        assert!(seqs(&stale.queue).is_empty());
        let fresh = hub.register(&node("b"), "l", None).unwrap();
        assert!(seqs(&fresh.queue).is_empty());
    }

    #[test]
    fn the_ring_is_evicted_by_bytes_too() {
        let (hub, _) = hub(HubLimits {
            ring_bytes: 600,
            ..HubLimits::default()
        });
        for _ in 0..20 {
            hub.publish(serde_json::json!({ "pad": "x".repeat(100) }));
        }
        let state = hub.state();
        assert!(
            state.ring.len() < 20 && state.ring_bytes <= 600 + 200,
            "{}",
            state.ring.len()
        );
        assert_eq!(state.ring.back().unwrap().0, 20);
    }

    #[test]
    fn a_queue_past_its_bounds_is_left_with_a_notice_and_closed_without_blocking() {
        let (hub, _) = hub(HubLimits {
            queue_events: 3,
            ..HubLimits::default()
        });
        let reg = hub
            .register(&node("a"), "l", Some((hub.epoch(), 0)))
            .unwrap();
        for i in 0..10 {
            hub.publish(serde_json::json!({ "n": i }));
        }
        let Next::Frames(frames) = reg.queue.take() else {
            panic!("frames");
        };
        assert_eq!(frames.len(), 1);
        assert!(matches!(
            serde_json::from_slice::<HostMsg>(&frames[0]).unwrap(),
            HostMsg::Notice {
                severity: Level::Warning,
                ..
            }
        ));
        assert!(matches!(reg.queue.take(), Next::Closed(Close::Overflow)));
        // The gap no longer fits a device queue, so the reconnect is given a snapshot instead.
        let _again = hub
            .register(&node("a"), "l", Some((hub.epoch(), 0)))
            .unwrap();
        assert!(hub.snapshot_wanted());
        // A gap that fits is replayed.
        let near = hub
            .register(&node("b"), "l", Some((hub.epoch(), 8)))
            .unwrap();
        assert_eq!(seqs(&near.queue), vec![9, 10]);
    }

    #[test]
    fn a_snapshot_larger_than_the_queue_bytes_is_still_delivered() {
        let (hub, _) = hub(HubLimits {
            queue_bytes: 100_000,
            ..HubLimits::default()
        });
        let reg = hub.register(&node("a"), "l", None).unwrap();
        hub.publish(serde_json::json!({}));
        let reg2 = hub
            .register(&node("b"), "l", Some((hub.epoch() ^ 1, 1)))
            .unwrap();
        assert!(hub.snapshot_wanted());
        hub.provide_snapshot(&format!("{{\"pad\":\"{}\"}}", "x".repeat(200_000)));
        let Next::Frames(frames) = reg2.queue.take() else {
            panic!("frames");
        };
        assert!(frames.iter().map(Vec::len).sum::<usize>() > 100_000);
        assert!(frames.iter().all(|f| matches!(
            serde_json::from_slice::<HostMsg>(f).unwrap(),
            HostMsg::Snapshot { .. }
        )));
        assert!(matches!(reg2.queue.take(), Next::Empty));
        drop(reg);
    }

    #[test]
    fn a_snapshot_is_cut_to_small_parts_that_reassemble() {
        let (hub, _) = hub(HubLimits::default());
        let _reg = hub.register(&node("a"), "l", None).unwrap();
        hub.publish(serde_json::json!({}));
        let reg2 = hub
            .register(&node("b"), "l", Some((hub.epoch() ^ 1, 1)))
            .unwrap();
        let state = format!("{{\"pad\":\"{}\"}}", "x".repeat(700_000));
        hub.provide_snapshot(&state);
        let Next::Frames(frames) = reg2.queue.take() else {
            panic!("frames");
        };
        assert!(frames.len() >= 3);
        let mut assembler = crate::protocol::SnapshotAssembler::default();
        let mut whole = None;
        for frame in &frames {
            assert!(frame.len() <= SNAPSHOT_PART_CAP);
            let HostMsg::Snapshot {
                epoch,
                seq,
                part,
                of,
                state,
            } = serde_json::from_slice(frame).unwrap()
            else {
                panic!("a snapshot part");
            };
            whole = assembler.push(epoch, seq, part, of, state);
        }
        assert_eq!(whole.expect("all parts arrived").2, state);
    }

    #[test]
    fn a_snapshot_past_the_hard_cap_is_refused_not_looped() {
        let (hub, _) = hub(HubLimits {
            queue_bytes: 10_000,
            ..HubLimits::default()
        });
        let reg = hub
            .register(&node("a"), "l", Some((hub.epoch() ^ 1, 1)))
            .unwrap();
        hub.provide_snapshot(&format!("{{\"pad\":\"{}\"}}", "x".repeat(100_000)));
        let Next::Frames(frames) = reg.queue.take() else {
            panic!("frames");
        };
        assert_eq!(frames.len(), 1);
        assert!(matches!(
            reg.queue.take(),
            Next::Closed(Close::Reject(Reason::TooLarge))
        ));
    }

    #[test]
    fn a_second_connection_from_a_node_supersedes_the_first() {
        let (hub, _) = hub(HubLimits::default());
        let first = hub.register(&node("a"), "l", None).unwrap();
        let second = hub.register(&node("a"), "l", None).unwrap();
        assert!(matches!(
            first.queue.take(),
            Next::Closed(Close::Reject(Reason::Superseded))
        ));
        assert_eq!(hub.device_count(), 1);
        drop(first);
        assert_eq!(
            hub.device_count(),
            1,
            "the old registration leaves the new one"
        );
        drop(second);
        assert_eq!(hub.device_count(), 0);
    }

    #[test]
    fn several_devices_each_get_the_same_events() {
        let (hub, _) = hub(HubLimits::default());
        let a = hub
            .register(&node("a"), "l", Some((hub.epoch(), 0)))
            .unwrap();
        let b = hub
            .register(&node("b"), "l", Some((hub.epoch(), 0)))
            .unwrap();
        hub.publish(serde_json::json!({}));
        assert_eq!(seqs(&a.queue), vec![1]);
        assert_eq!(seqs(&b.queue), vec![1]);
        assert!(hub.close_device("a", Close::Reject(Reason::Revoked)));
        assert!(!hub.close_device("zzz", Close::Bye));
    }
}
