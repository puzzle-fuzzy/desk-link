use desklink_protocol::{InputEvent, KeyCode, Modifiers, MouseButton};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NormalizedPoint {
    pub x: f32,
    pub y: f32,
}
impl NormalizedPoint {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DesktopRect {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}
impl DesktopRect {
    pub fn new(left: i32, top: i32, width: u32, height: u32) -> Self {
        Self {
            left,
            top,
            width,
            height,
        }
    }
}

pub fn map_to_desktop(point: NormalizedPoint, desktop: DesktopRect) -> (i32, i32) {
    let x = point.x.clamp(0.0, 1.0);
    let y = point.y.clamp(0.0, 1.0);
    let width = desktop.width.saturating_sub(1);
    let height = desktop.height.saturating_sub(1);
    (
        desktop.left + (x * width as f32).floor() as i32,
        desktop.top + (y * height as f32).floor() as i32,
    )
}

pub fn next_input_sequence(sequence: &mut u64) -> u64 {
    *sequence = sequence.wrapping_add(1);
    if *sequence == 0 {
        *sequence = 1;
    }
    *sequence
}

#[derive(Debug, Default)]
pub struct InputSequencer {
    next: u64,
}
impl InputSequencer {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn next_sequence(&mut self) -> u64 {
        next_input_sequence(&mut self.next)
    }
}

#[derive(Debug, Default)]
pub struct PressedInputState {
    buttons: Vec<MouseButton>,
    keys: Vec<(KeyCode, Modifiers)>,
}
impl PressedInputState {
    pub fn press(&mut self, event: &InputEvent) {
        match event {
            InputEvent::MouseButton {
                button,
                pressed: true,
            } if !self.buttons.contains(button) => self.buttons.push(*button),
            InputEvent::Key {
                code: KeyCode::Character(_),
                ..
            } => {}
            InputEvent::Key {
                code,
                pressed: true,
                modifiers,
            } if !self.keys.iter().any(|(k, _)| k == code) => self.keys.push((*code, *modifiers)),
            _ => {}
        }
    }
    pub fn release(&mut self, event: &InputEvent) {
        match event {
            InputEvent::MouseButton {
                button,
                pressed: false,
            } => {
                self.buttons.retain(|held| held != button);
            }
            InputEvent::Key {
                code: KeyCode::Character(_),
                ..
            } => {}
            InputEvent::Key {
                code,
                pressed: false,
                ..
            } => {
                self.keys.retain(|(held, _)| held != code);
            }
            _ => {}
        }
    }
    pub fn release_all(&mut self) -> Vec<InputEvent> {
        let events = self.release_events();
        self.keys.clear();
        self.buttons.clear();
        events
    }
    pub fn release_events(&self) -> Vec<InputEvent> {
        let mut events = Vec::new();
        for (code, modifiers) in self.keys.iter().rev() {
            events.push(InputEvent::Key {
                code: *code,
                pressed: false,
                modifiers: *modifiers,
            });
        }
        for button in self.buttons.iter().rev() {
            events.push(InputEvent::MouseButton {
                button: *button,
                pressed: false,
            });
        }
        events
    }
}
