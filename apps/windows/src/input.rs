use desklink_protocol::{InputEvent, KeyCode, Modifiers, MouseButton};
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
    UnsupportedKey,
}

pub struct InputInjector {
    desktop: VirtualDesktop,
    pressed: PressedInputState,
}

impl InputInjector {
    pub fn new(desktop: VirtualDesktop) -> Self {
        Self {
            desktop,
            pressed: PressedInputState::default(),
        }
    }

    pub fn apply(&mut self, event: InputEvent) -> Result<(), InputInjectionError> {
        #[cfg(windows)]
        send_native(&event, self.desktop)?;
        match &event {
            InputEvent::MouseMove { x, y } => {
                let _ = self.desktop.map(NormalizedPoint::new(
                    *x as f32 / 1_000_000.0,
                    *y as f32 / 1_000_000.0,
                ));
            }
            InputEvent::MouseButton { button, pressed } => {
                if !matches!(
                    button,
                    MouseButton::Left | MouseButton::Right | MouseButton::Middle
                ) {
                    return Err(InputInjectionError::UnsupportedKey);
                }
                if *pressed {
                    self.pressed.press(&event);
                } else {
                    self.pressed.release(&event);
                }
            }
            InputEvent::Key {
                code,
                pressed,
                modifiers,
            } => {
                if !supported_key(code, *modifiers) {
                    return Err(InputInjectionError::UnsupportedKey);
                }
                if *pressed {
                    self.pressed.press(&event);
                } else {
                    self.pressed.release(&event);
                }
            }
        }
        Ok(())
    }

    pub fn release_all(&mut self) -> Vec<InputEvent> {
        self.pressed.release_all()
    }
}

#[cfg(windows)]
fn send_native(event: &InputEvent, desktop: VirtualDesktop) -> Result<(), InputInjectionError> {
    use std::mem::size_of;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
        KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN,
        MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE,
        MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEINPUT, SendInput,
        VIRTUAL_KEY, VK_BACK, VK_DOWN, VK_ESCAPE, VK_LEFT, VK_RETURN, VK_RIGHT, VK_TAB, VK_UP,
    };

    let input = match event {
        InputEvent::MouseMove { x, y } => {
            let (desktop_x, desktop_y) = desktop.map(NormalizedPoint::new(
                *x as f32 / 1_000_000.0,
                *y as f32 / 1_000_000.0,
            ));
            let dx = ((desktop_x - desktop.rect.left) as i64 * 65_535
                / i64::from(desktop.rect.width.max(1))) as i32;
            let dy = ((desktop_y - desktop.rect.top) as i64 * 65_535
                / i64::from(desktop.rect.height.max(1))) as i32;
            INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx,
                        dy,
                        mouseData: 0,
                        dwFlags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            }
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
            INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx: 0,
                        dy: 0,
                        mouseData: 0,
                        dwFlags: flags,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            }
        }
        InputEvent::Key { code, pressed, .. } => {
            let (virtual_key, scan, mut flags) = match code {
                KeyCode::Character(character) => {
                    (VIRTUAL_KEY(0), *character as u16, KEYEVENTF_UNICODE)
                }
                KeyCode::Enter => (VK_RETURN, 0, KEYBD_EVENT_FLAGS(0)),
                KeyCode::Escape => (VK_ESCAPE, 0, KEYBD_EVENT_FLAGS(0)),
                KeyCode::Backspace => (VK_BACK, 0, KEYBD_EVENT_FLAGS(0)),
                KeyCode::Tab => (VK_TAB, 0, KEYBD_EVENT_FLAGS(0)),
                KeyCode::ArrowUp => (VK_UP, 0, KEYBD_EVENT_FLAGS(0)),
                KeyCode::ArrowDown => (VK_DOWN, 0, KEYBD_EVENT_FLAGS(0)),
                KeyCode::ArrowLeft => (VK_LEFT, 0, KEYBD_EVENT_FLAGS(0)),
                KeyCode::ArrowRight => (VK_RIGHT, 0, KEYBD_EVENT_FLAGS(0)),
            };
            if !pressed {
                flags |= KEYEVENTF_KEYUP;
            }
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
    };
    let sent = unsafe { SendInput(std::slice::from_ref(&input), size_of::<INPUT>() as i32) };
    if sent == 1 {
        Ok(())
    } else {
        Err(InputInjectionError::Blocked)
    }
}

fn supported_key(code: &KeyCode, _modifiers: Modifiers) -> bool {
    matches!(
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

    #[test]
    fn release_all_clears_pressed_inputs() {
        let mut injector = InputInjector::new(VirtualDesktop {
            rect: DesktopRect::new(0, 0, 1920, 1080),
        });
        injector
            .apply(InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: true,
            })
            .unwrap();
        assert_eq!(injector.release_all().len(), 1);
        assert!(injector.release_all().is_empty());
    }
}
