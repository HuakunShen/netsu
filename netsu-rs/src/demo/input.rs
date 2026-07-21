//! Transport-free input core: normalized events, a coalescing queue, a replay
//! gate, and pressed-key tracking for release-all. Ported verbatim from the
//! source demo — self-contained (serde + anyhow only).

use std::{
    collections::{BTreeSet, VecDeque},
    time::{Duration, Instant},
};

use anyhow::ensure;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PortableKey(String);

impl PortableKey {
    pub fn new(value: impl Into<String>) -> anyhow::Result<Self> {
        let value = value.into();
        ensure!(
            !value.is_empty()
                && value.len() <= 32
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
            "portable key must be 1-32 ASCII alphanumeric/underscore characters"
        );
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum KeyState {
    Down,
    Up,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PointerButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
    Other(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ButtonState {
    Down,
    Up,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum NormalizedInputEvent {
    Key {
        key: PortableKey,
        state: KeyState,
    },
    PointerMove {
        x: f32,
        y: f32,
    },
    PointerButton {
        button: PointerButton,
        state: ButtonState,
    },
    Wheel {
        delta_x: f32,
        delta_y: f32,
    },
    ReleaseAll,
}

#[derive(Debug, Clone)]
pub struct CapturedInputEvent {
    pub event: NormalizedInputEvent,
    pub captured_at: Instant,
}

impl CapturedInputEvent {
    pub fn new(event: NormalizedInputEvent) -> Self {
        Self {
            event,
            captured_at: Instant::now(),
        }
    }
}

impl NormalizedInputEvent {
    pub fn pointer_move(x: f32, y: f32) -> Self {
        Self::PointerMove {
            x: x.clamp(0.0, 1.0),
            y: y.clamp(0.0, 1.0),
        }
    }

    fn is_pointer_move(&self) -> bool {
        matches!(self, Self::PointerMove { .. })
    }

    /// `q` is reserved by the interactive demo as a local stop key.
    pub fn is_local_quit(&self) -> bool {
        matches!(self, Self::Key { key, state: KeyState::Down } if key.as_str() == "KeyQ")
    }
}

/// Bounded event queue that coalesces consecutive pointer moves (never dropping
/// key/button transitions).
pub struct InputQueue {
    capacity: usize,
    events: VecDeque<CapturedInputEvent>,
}

impl InputQueue {
    pub fn new(capacity: usize) -> anyhow::Result<Self> {
        ensure!(capacity > 0, "input queue capacity must be positive");
        Ok(Self {
            capacity,
            events: VecDeque::with_capacity(capacity),
        })
    }

    pub fn push(&mut self, event: NormalizedInputEvent) -> anyhow::Result<()> {
        self.push_captured(CapturedInputEvent::new(event))
    }

    pub fn push_captured(&mut self, captured: CapturedInputEvent) -> anyhow::Result<()> {
        if captured.event.is_pointer_move()
            && let Some(last_move) = self
                .events
                .back_mut()
                .filter(|queued| queued.event.is_pointer_move())
        {
            *last_move = captured;
            return Ok(());
        }
        if self.events.len() == self.capacity {
            if let Some(index) = self
                .events
                .iter()
                .position(|queued| queued.event.is_pointer_move())
            {
                self.events.remove(index);
            } else if captured.event.is_pointer_move() {
                return Ok(());
            } else {
                anyhow::bail!("input queue is full of non-coalescible transitions");
            }
        }
        self.events.push_back(captured);
        Ok(())
    }

    pub fn pop(&mut self) -> Option<NormalizedInputEvent> {
        self.pop_captured().map(|captured| captured.event)
    }

    pub fn pop_captured(&mut self) -> Option<CapturedInputEvent> {
        self.events.pop_front()
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

/// Rejects stale or replayed input on the controlled side.
pub struct InputGate {
    maximum_age: Duration,
    last_sequence: Option<u64>,
}

impl InputGate {
    pub fn new(maximum_age: Duration) -> Self {
        Self {
            maximum_age,
            last_sequence: None,
        }
    }

    pub fn accept(&mut self, sequence: u64, controller_queue_age: Duration) -> bool {
        if controller_queue_age > self.maximum_age
            || self.last_sequence.is_some_and(|last| sequence <= last)
        {
            return false;
        }
        self.last_sequence = Some(sequence);
        true
    }
}

pub trait InputInjector: Send + Sync + 'static {
    fn inject(&self, event: &NormalizedInputEvent) -> anyhow::Result<()>;
}

/// Tracks currently-held keys/buttons so all can be released on stop/disconnect.
pub struct PressedState {
    keys: BTreeSet<PortableKey>,
    buttons: BTreeSet<PointerButton>,
}

impl PressedState {
    pub fn new() -> Self {
        Self {
            keys: BTreeSet::new(),
            buttons: BTreeSet::new(),
        }
    }

    pub fn observe(&mut self, event: &NormalizedInputEvent) {
        match event {
            NormalizedInputEvent::Key { key, state } => match state {
                KeyState::Down => {
                    self.keys.insert(key.clone());
                }
                KeyState::Up => {
                    self.keys.remove(key);
                }
            },
            NormalizedInputEvent::PointerButton { button, state } => match state {
                ButtonState::Down => {
                    self.buttons.insert(*button);
                }
                ButtonState::Up => {
                    self.buttons.remove(button);
                }
            },
            NormalizedInputEvent::ReleaseAll => {
                self.keys.clear();
                self.buttons.clear();
            }
            _ => {}
        }
    }

    pub fn release_events(&self) -> Vec<NormalizedInputEvent> {
        let mut events = self
            .keys
            .iter()
            .cloned()
            .map(|key| NormalizedInputEvent::Key {
                key,
                state: KeyState::Up,
            })
            .collect::<Vec<_>>();
        events.extend(self.buttons.iter().copied().map(|button| {
            NormalizedInputEvent::PointerButton {
                button,
                state: ButtonState::Up,
            }
        }));
        events
    }
}

impl Default for PressedState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coalesces_pointer_moves_but_keeps_transitions() {
        let mut q = InputQueue::new(4).unwrap();
        q.push(NormalizedInputEvent::pointer_move(0.1, 0.1))
            .unwrap();
        q.push(NormalizedInputEvent::pointer_move(0.2, 0.2))
            .unwrap();
        assert_eq!(q.len(), 1); // coalesced
        q.push(NormalizedInputEvent::Key {
            key: PortableKey::new("KeyA").unwrap(),
            state: KeyState::Down,
        })
        .unwrap();
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn gate_rejects_stale_and_replayed() {
        let mut gate = InputGate::new(Duration::from_millis(100));
        assert!(gate.accept(1, Duration::from_millis(10)));
        assert!(!gate.accept(1, Duration::from_millis(10))); // replay
        assert!(!gate.accept(2, Duration::from_millis(500))); // too old
        assert!(gate.accept(2, Duration::from_millis(10)));
    }

    #[test]
    fn pressed_state_releases_everything() {
        let mut p = PressedState::new();
        p.observe(&NormalizedInputEvent::Key {
            key: PortableKey::new("KeyA").unwrap(),
            state: KeyState::Down,
        });
        p.observe(&NormalizedInputEvent::PointerButton {
            button: PointerButton::Left,
            state: ButtonState::Down,
        });
        let releases = p.release_events();
        assert_eq!(releases.len(), 2);
    }

    #[test]
    fn q_is_a_local_quit() {
        assert!(
            NormalizedInputEvent::Key {
                key: PortableKey::new("KeyQ").unwrap(),
                state: KeyState::Down,
            }
            .is_local_quit()
        );
    }
}
