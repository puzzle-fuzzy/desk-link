use desklink_protocol::{
    InputEvent, KeyCode, MAX_POINTER_COORDINATE, MAX_WHEEL_DELTA, Modifiers, MouseButton,
};
use desklink_session::{DesktopRect, NormalizedPoint, PressedInputState, map_to_desktop};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VirtualDesktop {
    pub rect: DesktopRect,
}

impl VirtualDesktop {
    pub fn map(&self, point: NormalizedPoint) -> (i32, i32) {
        map_to_desktop(point, self.rect)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputInjectionError {
    Blocked,
    InvalidInput,
    UnsupportedKey,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ModifierKey {
    Control,
    Alt,
    Shift,
    Meta,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum KeyAction {
    Modifier(ModifierKey, bool),
    Main(bool),
}

fn modifier_keys(modifiers: Modifiers) -> Vec<ModifierKey> {
    [
        (Modifiers::CONTROL, ModifierKey::Control),
        (Modifiers::ALT, ModifierKey::Alt),
        (Modifiers::SHIFT, ModifierKey::Shift),
        (Modifiers::META, ModifierKey::Meta),
    ]
    .into_iter()
    .filter_map(|(flag, key)| modifiers.contains(flag).then_some(key))
    .collect()
}

fn key_action_plan(pressed: bool, modifiers: Modifiers) -> Vec<KeyAction> {
    let mut keys = modifier_keys(modifiers);
    if pressed {
        let mut actions = keys
            .drain(..)
            .map(|key| KeyAction::Modifier(key, true))
            .collect::<Vec<_>>();
        actions.push(KeyAction::Main(true));
        actions
    } else {
        let mut actions = vec![KeyAction::Main(false)];
        actions.extend(
            keys.into_iter()
                .rev()
                .map(|key| KeyAction::Modifier(key, false)),
        );
        actions
    }
}

fn absolute_axis(position: i32, origin: i32, extent: u32) -> i32 {
    let span = extent.saturating_sub(1);
    if span == 0 {
        return 0;
    }
    let offset = (i64::from(position) - i64::from(origin)).clamp(0, i64::from(span));
    (offset * 65_535 / i64::from(span)) as i32
}

pub trait InputBackend {
    fn send(
        &mut self,
        event: &InputEvent,
        desktop: VirtualDesktop,
    ) -> Result<(), InputInjectionError>;
}

#[derive(Default)]
pub struct NativeInputBackend;

impl InputBackend for NativeInputBackend {
    fn send(
        &mut self,
        event: &InputEvent,
        desktop: VirtualDesktop,
    ) -> Result<(), InputInjectionError> {
        #[cfg(windows)]
        return native::send(event, desktop);

        #[cfg(not(windows))]
        {
            let _ = (event, desktop);
            Ok(())
        }
    }
}

pub struct InputInjector<B: InputBackend = NativeInputBackend> {
    desktop: VirtualDesktop,
    pressed: PressedInputState,
    backend: B,
}

impl InputInjector<NativeInputBackend> {
    pub fn new(desktop: VirtualDesktop) -> Self {
        Self::with_backend(desktop, NativeInputBackend)
    }
}

impl<B: InputBackend> InputInjector<B> {
    pub fn with_backend(desktop: VirtualDesktop, backend: B) -> Self {
        Self {
            desktop,
            pressed: PressedInputState::default(),
            backend,
        }
    }

    pub fn apply(&mut self, event: InputEvent) -> Result<(), InputInjectionError> {
        match &event {
            InputEvent::MouseMove { x, y } => {
                if !(0..=MAX_POINTER_COORDINATE).contains(x)
                    || !(0..=MAX_POINTER_COORDINATE).contains(y)
                {
                    return Err(InputInjectionError::InvalidInput);
                }
                let _ = self.desktop.map(NormalizedPoint::new(
                    *x as f32 / 1_000_000.0,
                    *y as f32 / 1_000_000.0,
                ));
            }
            InputEvent::MouseButton { button, .. } => {
                if !matches!(
                    button,
                    MouseButton::Left | MouseButton::Right | MouseButton::Middle
                ) {
                    return Err(InputInjectionError::UnsupportedKey);
                }
            }
            InputEvent::Key {
                code, modifiers, ..
            } => {
                if !supported_key(code, *modifiers) {
                    return Err(InputInjectionError::UnsupportedKey);
                }
            }
            InputEvent::MouseWheel { delta_x, delta_y } => {
                if (*delta_x == 0 && *delta_y == 0)
                    || !(-MAX_WHEEL_DELTA..=MAX_WHEEL_DELTA).contains(delta_x)
                    || !(-MAX_WHEEL_DELTA..=MAX_WHEEL_DELTA).contains(delta_y)
                {
                    return Err(InputInjectionError::InvalidInput);
                }
            }
        }
        self.backend.send(&event, self.desktop)?;
        match &event {
            InputEvent::MouseButton { pressed: true, .. }
            | InputEvent::Key { pressed: true, .. } => self.pressed.press(&event),
            InputEvent::MouseButton { pressed: false, .. }
            | InputEvent::Key { pressed: false, .. } => self.pressed.release(&event),
            InputEvent::MouseMove { .. } | InputEvent::MouseWheel { .. } => {}
        }
        Ok(())
    }

    pub fn release_all(&mut self) -> Result<Vec<InputEvent>, InputInjectionError> {
        let events = self.pressed.release_events();
        let mut released = Vec::with_capacity(events.len());
        for event in events {
            self.backend.send(&event, self.desktop)?;
            self.pressed.release(&event);
            released.push(event);
        }
        Ok(released)
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }
}

impl<B: InputBackend> Drop for InputInjector<B> {
    fn drop(&mut self) {
        let _ = self.release_all();
    }
}

#[cfg(windows)]
mod native {
    use super::*;
    use std::mem::size_of;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
        KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MOUSE_EVENT_FLAGS,
        MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
        MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
        MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEINPUT, SendInput,
        VIRTUAL_KEY, VK_BACK, VK_CONTROL, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_LWIN, VK_MENU, VK_RETURN,
        VK_RIGHT, VK_SHIFT, VK_TAB, VK_UP, VkKeyScanW,
    };

    pub(super) fn send(
        event: &InputEvent,
        desktop: VirtualDesktop,
    ) -> Result<(), InputInjectionError> {
        let (inputs, cleanup) = match event {
            InputEvent::MouseMove { x, y } => {
                let (desktop_x, desktop_y) = desktop.map(NormalizedPoint::new(
                    *x as f32 / 1_000_000.0,
                    *y as f32 / 1_000_000.0,
                ));
                (
                    vec![mouse_input(
                        absolute_axis(desktop_x, desktop.rect.left, desktop.rect.width),
                        absolute_axis(desktop_y, desktop.rect.top, desktop.rect.height),
                        0,
                        MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                    )],
                    None,
                )
            }
            InputEvent::MouseButton { button, pressed } => {
                let flags = match (button, pressed) {
                    (MouseButton::Left, true) => MOUSEEVENTF_LEFTDOWN,
                    (MouseButton::Left, false) => MOUSEEVENTF_LEFTUP,
                    (MouseButton::Right, true) => MOUSEEVENTF_RIGHTDOWN,
                    (MouseButton::Right, false) => MOUSEEVENTF_RIGHTUP,
                    (MouseButton::Middle, true) => MOUSEEVENTF_MIDDLEDOWN,
                    (MouseButton::Middle, false) => MOUSEEVENTF_MIDDLEUP,
                };
                (vec![mouse_input(0, 0, 0, flags)], None)
            }
            InputEvent::MouseWheel { delta_x, delta_y } => {
                let mut inputs = Vec::with_capacity(2);
                if *delta_y != 0 {
                    inputs.push(mouse_input(0, 0, *delta_y as u32, MOUSEEVENTF_WHEEL));
                }
                if *delta_x != 0 {
                    inputs.push(mouse_input(0, 0, *delta_x as u32, MOUSEEVENTF_HWHEEL));
                }
                (inputs, None)
            }
            InputEvent::Key {
                code,
                pressed,
                modifiers,
            } => {
                let inputs = key_inputs(*code, *pressed, *modifiers)?;
                let cleanup = key_inputs(*code, false, *modifiers)?;
                (inputs, Some(cleanup))
            }
        };
        let sent = unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) } as usize;
        if sent == inputs.len() {
            return Ok(());
        }
        if let Some(cleanup) = cleanup {
            let _ = unsafe { SendInput(&cleanup, size_of::<INPUT>() as i32) };
        }
        Err(InputInjectionError::Blocked)
    }

    fn mouse_input(dx: i32, dy: i32, data: u32, flags: MOUSE_EVENT_FLAGS) -> INPUT {
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    mouseData: data,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }

    #[derive(Clone, Copy)]
    struct KeySpec {
        virtual_key: VIRTUAL_KEY,
        scan: u16,
        flags: KEYBD_EVENT_FLAGS,
    }

    fn key_inputs(
        code: KeyCode,
        pressed: bool,
        mut modifiers: Modifiers,
    ) -> Result<Vec<INPUT>, InputInjectionError> {
        let specs = key_specs(code, &mut modifiers)?;
        let mut inputs = Vec::with_capacity(modifier_keys(modifiers).len() + specs.len());
        for action in key_action_plan(pressed, modifiers) {
            match action {
                KeyAction::Modifier(modifier, state) => {
                    let (virtual_key, mut flags) = match modifier {
                        ModifierKey::Control => (VK_CONTROL, KEYBD_EVENT_FLAGS(0)),
                        ModifierKey::Alt => (VK_MENU, KEYBD_EVENT_FLAGS(0)),
                        ModifierKey::Shift => (VK_SHIFT, KEYBD_EVENT_FLAGS(0)),
                        ModifierKey::Meta => (VK_LWIN, KEYEVENTF_EXTENDEDKEY),
                    };
                    if !state {
                        flags |= KEYEVENTF_KEYUP;
                    }
                    inputs.push(keyboard_input(virtual_key, 0, flags));
                }
                KeyAction::Main(state) => {
                    for spec in &specs {
                        let mut flags = spec.flags;
                        if !state {
                            flags |= KEYEVENTF_KEYUP;
                        }
                        inputs.push(keyboard_input(spec.virtual_key, spec.scan, flags));
                    }
                }
            }
        }
        Ok(inputs)
    }

    fn key_specs(
        code: KeyCode,
        modifiers: &mut Modifiers,
    ) -> Result<Vec<KeySpec>, InputInjectionError> {
        let spec = |virtual_key, flags| KeySpec {
            virtual_key,
            scan: 0,
            flags,
        };
        let specs = match code {
            KeyCode::Character(character) if modifiers.is_empty() => {
                let mut encoded = [0; 2];
                character
                    .encode_utf16(&mut encoded)
                    .iter()
                    .map(|unit| KeySpec {
                        virtual_key: VIRTUAL_KEY(0),
                        scan: *unit,
                        flags: KEYEVENTF_UNICODE,
                    })
                    .collect()
            }
            KeyCode::Character(character) => {
                let code_unit = u16::try_from(u32::from(character))
                    .map_err(|_| InputInjectionError::UnsupportedKey)?;
                let mapping = unsafe { VkKeyScanW(code_unit) };
                if mapping == -1 {
                    return Err(InputInjectionError::UnsupportedKey);
                }
                let mapping = mapping as u16;
                let layout_modifiers = mapping >> 8;
                if layout_modifiers & 1 != 0 {
                    *modifiers |= Modifiers::SHIFT;
                }
                if layout_modifiers & 2 != 0 {
                    *modifiers |= Modifiers::CONTROL;
                }
                if layout_modifiers & 4 != 0 {
                    *modifiers |= Modifiers::ALT;
                }
                vec![spec(VIRTUAL_KEY(mapping & 0xff), KEYBD_EVENT_FLAGS(0))]
            }
            KeyCode::Enter => vec![spec(VK_RETURN, KEYBD_EVENT_FLAGS(0))],
            KeyCode::Escape => vec![spec(VK_ESCAPE, KEYBD_EVENT_FLAGS(0))],
            KeyCode::Backspace => vec![spec(VK_BACK, KEYBD_EVENT_FLAGS(0))],
            KeyCode::Tab => vec![spec(VK_TAB, KEYBD_EVENT_FLAGS(0))],
            KeyCode::ArrowUp => vec![spec(VK_UP, KEYEVENTF_EXTENDEDKEY)],
            KeyCode::ArrowDown => vec![spec(VK_DOWN, KEYEVENTF_EXTENDEDKEY)],
            KeyCode::ArrowLeft => vec![spec(VK_LEFT, KEYEVENTF_EXTENDEDKEY)],
            KeyCode::ArrowRight => vec![spec(VK_RIGHT, KEYEVENTF_EXTENDEDKEY)],
        };
        Ok(specs)
    }

    fn keyboard_input(virtual_key: VIRTUAL_KEY, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: virtual_key,
                    wScan: scan,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        }
    }
}

fn supported_key(code: &KeyCode, modifiers: Modifiers) -> bool {
    modifiers.is_valid()
        && matches!(
            code,
            KeyCode::Character(_)
                | KeyCode::Enter
                | KeyCode::Escape
                | KeyCode::Backspace
                | KeyCode::Tab
                | KeyCode::ArrowUp
                | KeyCode::ArrowDown
                | KeyCode::ArrowLeft
                | KeyCode::ArrowRight
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct RecordingBackend {
        events: Vec<InputEvent>,
        fail_after: Option<usize>,
    }

    impl InputBackend for RecordingBackend {
        fn send(
            &mut self,
            event: &InputEvent,
            _desktop: VirtualDesktop,
        ) -> Result<(), InputInjectionError> {
            if self.fail_after == Some(self.events.len()) {
                return Err(InputInjectionError::Blocked);
            }
            self.events.push(event.clone());
            Ok(())
        }
    }

    #[test]
    fn release_all_sends_native_key_and_button_up_before_clearing() {
        let desktop = VirtualDesktop {
            rect: DesktopRect::new(0, 0, 1920, 1080),
        };
        let mut injector = InputInjector::with_backend(desktop, RecordingBackend::default());
        injector
            .apply(InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: true,
            })
            .unwrap();
        injector
            .apply(InputEvent::Key {
                code: KeyCode::Enter,
                pressed: true,
                modifiers: Modifiers::CONTROL | Modifiers::SHIFT,
            })
            .unwrap();

        let released = injector.release_all().unwrap();
        assert_eq!(released.len(), 2);
        assert_eq!(injector.backend().events.len(), 4);
        assert!(matches!(
            injector.backend().events[2],
            InputEvent::Key {
                pressed: false,
                modifiers,
                ..
            } if modifiers == Modifiers::CONTROL | Modifiers::SHIFT
        ));
        assert_eq!(
            injector.backend().events[3],
            InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: false,
            }
        );
        assert!(injector.release_all().unwrap().is_empty());
    }

    #[test]
    fn wheel_events_are_injected_without_becoming_pressed() {
        let desktop = VirtualDesktop {
            rect: DesktopRect::new(0, 0, 1920, 1080),
        };
        let mut injector = InputInjector::with_backend(desktop, RecordingBackend::default());
        let wheel = InputEvent::MouseWheel {
            delta_x: -120,
            delta_y: 240,
        };

        injector.apply(wheel.clone()).unwrap();

        assert_eq!(injector.backend().events, vec![wheel]);
        assert!(injector.release_all().unwrap().is_empty());
    }

    #[test]
    fn modifier_action_plan_wraps_key_down_and_reverses_key_up() {
        let modifiers = Modifiers::CONTROL | Modifiers::SHIFT | Modifiers::ALT;
        assert_eq!(
            key_action_plan(true, modifiers),
            vec![
                KeyAction::Modifier(ModifierKey::Control, true),
                KeyAction::Modifier(ModifierKey::Alt, true),
                KeyAction::Modifier(ModifierKey::Shift, true),
                KeyAction::Main(true),
            ]
        );
        assert_eq!(
            key_action_plan(false, modifiers),
            vec![
                KeyAction::Main(false),
                KeyAction::Modifier(ModifierKey::Shift, false),
                KeyAction::Modifier(ModifierKey::Alt, false),
                KeyAction::Modifier(ModifierKey::Control, false),
            ]
        );
    }

    #[test]
    fn absolute_axis_maps_both_desktop_endpoints_exactly() {
        assert_eq!(absolute_axis(100, 100, 1920), 0);
        assert_eq!(absolute_axis(2019, 100, 1920), 65_535);
        assert_eq!(absolute_axis(100, 100, 1), 0);
    }

    #[test]
    fn release_all_retains_unsent_inputs_when_native_injection_is_blocked() {
        let desktop = VirtualDesktop {
            rect: DesktopRect::new(0, 0, 1920, 1080),
        };
        let mut injector = InputInjector::with_backend(desktop, RecordingBackend::default());
        injector
            .apply(InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: true,
            })
            .unwrap();
        injector
            .apply(InputEvent::Key {
                code: KeyCode::Enter,
                pressed: true,
                modifiers: Modifiers(0),
            })
            .unwrap();
        injector.backend_mut().fail_after = Some(3);

        assert_eq!(injector.release_all(), Err(InputInjectionError::Blocked));
        assert!(matches!(
            injector.backend().events[2],
            InputEvent::Key { pressed: false, .. }
        ));
        injector.backend_mut().fail_after = None;
        assert_eq!(injector.release_all().unwrap().len(), 1);
        assert_eq!(
            injector.backend().events[3],
            InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: false,
            }
        );
    }
}
