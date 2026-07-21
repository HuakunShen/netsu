//! The only monio-dependent file: global OS input capture and injection, plus
//! event normalization and emergency-chord detection. Ported from the source
//! demo (proven against monio 0.1.1).

use anyhow::{Context, bail};
use monio::{
    Button, DisplayInfo, Event, EventType, Key, ScrollDirection,
    channel::{ChannelHookHandle, listen_async_channel},
};
use serde_json::Value;
use tokio::sync::mpsc;

use super::input::{
    ButtonState, InputInjector, KeyState, NormalizedInputEvent, PointerButton, PortableKey,
};

pub struct MonioCapture {
    pub handle: ChannelHookHandle,
    pub events: mpsc::Receiver<Event>,
    pub display: DisplayInfo,
}

impl MonioCapture {
    pub fn start(capacity: usize) -> anyhow::Result<Self> {
        let display = monio::primary_display().context("query Monio primary display")?;
        let (handle, events) =
            listen_async_channel(capacity).context("start Monio global input hook")?;
        Ok(Self { handle, events, display })
    }
}

pub struct MonioInjector {
    display: DisplayInfo,
}

impl MonioInjector {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self { display: monio::primary_display().context("query Monio primary display")? })
    }
}

impl InputInjector for MonioInjector {
    fn inject(&self, event: &NormalizedInputEvent) -> anyhow::Result<()> {
        match event {
            NormalizedInputEvent::Key { key, state } => {
                let key = portable_to_monio_key(key)?;
                match state {
                    KeyState::Down => monio::key_press(key),
                    KeyState::Up => monio::key_release(key),
                }
                .context("simulate keyboard input with Monio")?;
            }
            NormalizedInputEvent::PointerMove { x, y } => {
                let bounds = self.display.bounds;
                let x = bounds.x + f64::from(*x) * bounds.width.max(1.0);
                let y = bounds.y + f64::from(*y) * bounds.height.max(1.0);
                monio::mouse_move(x, y).context("simulate pointer movement with Monio")?;
            }
            NormalizedInputEvent::PointerButton { button, state } => {
                let button = pointer_to_monio_button(*button);
                match state {
                    ButtonState::Down => monio::mouse_press(button),
                    ButtonState::Up => monio::mouse_release(button),
                }
                .context("simulate pointer button with Monio")?;
            }
            NormalizedInputEvent::Wheel { delta_x, delta_y } => {
                let (direction, delta) = if delta_x.abs() > delta_y.abs() {
                    if *delta_x < 0.0 {
                        (ScrollDirection::Left, delta_x.abs())
                    } else {
                        (ScrollDirection::Right, delta_x.abs())
                    }
                } else if *delta_y < 0.0 {
                    (ScrollDirection::Up, delta_y.abs())
                } else {
                    (ScrollDirection::Down, delta_y.abs())
                };
                if delta > 0.0 {
                    let (x, y) =
                        monio::mouse_position().context("query pointer position with Monio")?;
                    monio::simulate(&Event::mouse_wheel(x, y, direction, f64::from(delta)))
                        .context("simulate pointer wheel with Monio")?;
                }
            }
            // The controlled session expands this into individual releases first.
            NormalizedInputEvent::ReleaseAll => {}
        }
        Ok(())
    }
}

pub fn normalize_event(
    event: &Event,
    display: &DisplayInfo,
) -> anyhow::Result<Option<NormalizedInputEvent>> {
    let normalized = match event.event_type {
        EventType::KeyPressed | EventType::KeyReleased => {
            let keyboard = event
                .keyboard
                .as_ref()
                .context("Monio keyboard event has no keyboard data")?;
            let Some(key) = monio_to_portable_key(keyboard.key)? else {
                return Ok(None);
            };
            NormalizedInputEvent::Key {
                key,
                state: if event.event_type == EventType::KeyPressed {
                    KeyState::Down
                } else {
                    KeyState::Up
                },
            }
        }
        EventType::MouseMoved | EventType::MouseDragged => {
            let mouse =
                event.mouse.as_ref().context("Monio pointer event has no pointer data")?;
            let bounds = display.bounds;
            NormalizedInputEvent::pointer_move(
                ((mouse.x - bounds.x) / bounds.width.max(1.0)) as f32,
                ((mouse.y - bounds.y) / bounds.height.max(1.0)) as f32,
            )
        }
        EventType::MousePressed | EventType::MouseReleased => {
            let mouse = event
                .mouse
                .as_ref()
                .context("Monio pointer button event has no pointer data")?;
            let button = mouse.button.context("Monio pointer button event has no button")?;
            NormalizedInputEvent::PointerButton {
                button: monio_to_pointer_button(button),
                state: if event.event_type == EventType::MousePressed {
                    ButtonState::Down
                } else {
                    ButtonState::Up
                },
            }
        }
        EventType::MouseWheel => {
            let wheel = event.wheel.as_ref().context("Monio wheel event has no wheel data")?;
            let delta = wheel.delta as f32;
            let (delta_x, delta_y) = match wheel.direction {
                ScrollDirection::Up => (0.0, -delta),
                ScrollDirection::Down => (0.0, delta),
                ScrollDirection::Left => (-delta, 0.0),
                ScrollDirection::Right => (delta, 0.0),
            };
            NormalizedInputEvent::Wheel { delta_x, delta_y }
        }
        EventType::HookEnabled
        | EventType::HookDisabled
        | EventType::KeyTyped
        | EventType::MouseClicked => return Ok(None),
    };
    Ok(Some(normalized))
}

pub fn is_emergency_key(event: &NormalizedInputEvent) -> bool {
    matches!(event, NormalizedInputEvent::Key { key, state: KeyState::Down } if key.as_str() == "Escape")
}

pub fn is_control_key(event: &NormalizedInputEvent) -> Option<bool> {
    match event {
        NormalizedInputEvent::Key { key, state }
            if matches!(key.as_str(), "ControlLeft" | "ControlRight") =>
        {
            Some(*state == KeyState::Down)
        }
        _ => None,
    }
}

pub fn is_alt_key(event: &NormalizedInputEvent) -> Option<bool> {
    match event {
        NormalizedInputEvent::Key { key, state }
            if matches!(key.as_str(), "AltLeft" | "AltRight") =>
        {
            Some(*state == KeyState::Down)
        }
        _ => None,
    }
}

fn monio_to_portable_key(key: Key) -> anyhow::Result<Option<PortableKey>> {
    match serde_json::to_value(key).context("serialize Monio key name")? {
        Value::String(name) => PortableKey::new(name).map(Some),
        Value::Object(_) => Ok(None),
        other => bail!("unexpected serialized Monio key {other}"),
    }
}

fn portable_to_monio_key(key: &PortableKey) -> anyhow::Result<Key> {
    serde_json::from_value(Value::String(key.as_str().to_owned()))
        .with_context(|| format!("unsupported portable key {}", key.as_str()))
}

fn monio_to_pointer_button(button: Button) -> PointerButton {
    match button {
        Button::Left => PointerButton::Left,
        Button::Right => PointerButton::Right,
        Button::Middle => PointerButton::Middle,
        Button::Button4 => PointerButton::Back,
        Button::Button5 => PointerButton::Forward,
        Button::Unknown(number) => PointerButton::Other(number),
    }
}

fn pointer_to_monio_button(button: PointerButton) -> Button {
    match button {
        PointerButton::Left => Button::Left,
        PointerButton::Right => Button::Right,
        PointerButton::Middle => Button::Middle,
        PointerButton::Back => Button::Button4,
        PointerButton::Forward => Button::Button5,
        PointerButton::Other(number) => Button::Unknown(number),
    }
}
