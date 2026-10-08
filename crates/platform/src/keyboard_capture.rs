//! Foreground-only system-key capture. Ordinary typing remains on GPUI's IME path.
use opsssh_term_core::{Key, KeyEventKind, Modifiers};

#[derive(Clone, Copy, Debug)]
pub struct CapturedKey {
    pub target: u64,
    pub generation: u64,
    pub key: Key,
    pub modifiers: Modifiers,
    pub kind: KeyEventKind,
    pub release_capture: bool,
}

/// The only native combinations intercepted; secure and Windows-key chords pass through.
#[cfg(any(target_os = "windows", test))]
fn captured_key(key: u32, alt: bool, control: bool, _shift: bool) -> Option<(Key, bool)> {
    match key {
        0x7b if alt && control => Some((Key::Function(12), true)),
        0x09 if alt => Some((Key::Tab, false)),
        0x1b if alt || control => Some((Key::Escape, false)),
        0x73 if alt => Some((Key::Function(4), false)),
        _ => None,
    }
}

#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
mod native {
    use super::*;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use std::{
        collections::HashMap,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, AtomicU64, Ordering},
        },
        thread,
        time::Duration,
    };
    use windows_sys::Win32::{
        Foundation::*,
        System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
        UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
    };

    struct State {
        hwnd: usize,
        enabled: AtomicBool,
        failed: AtomicBool,
        stopping: AtomicBool,
        target: AtomicU64,
        generation: AtomicU64,
        events: async_channel::Sender<CapturedKey>,
        pressed: Mutex<HashMap<u32, CapturedKey>>,
    }
    static CURRENT: Mutex<Option<Arc<State>>> = Mutex::new(None);

    impl State {
        fn route(&self, vk: u32, up: bool, foreground: usize, modifiers: Modifiers) -> bool {
            let Ok(mut pressed) = self.pressed.try_lock() else {
                self.failed.store(true, Ordering::Release);
                self.enabled.store(false, Ordering::Release);
                return false;
            };
            if up && let Some(mut event) = pressed.remove(&vk) {
                event.kind = KeyEventKind::Release;
                if !event.release_capture
                    && self.enabled.load(Ordering::Acquire)
                    && foreground == self.hwnd
                    && self.target.load(Ordering::Acquire) == event.target
                    && self.generation.load(Ordering::Acquire) == event.generation
                    && self.events.try_send(event).is_err()
                {
                    self.failed.store(true, Ordering::Release);
                    self.enabled.store(false, Ordering::Release);
                }
                return true;
            }
            if up
                || !self.enabled.load(Ordering::Acquire)
                || foreground != self.hwnd
                || modifiers.super_key
            {
                return false;
            }
            let Some((key, release_capture)) =
                captured_key(vk, modifiers.alt, modifiers.control, modifiers.shift)
            else {
                return false;
            };
            let event = if let Some(previous) = pressed.get(&vk) {
                CapturedKey {
                    kind: KeyEventKind::Repeat,
                    ..*previous
                }
            } else {
                CapturedKey {
                    target: self.target.load(Ordering::Acquire),
                    generation: self.generation.load(Ordering::Acquire),
                    key,
                    modifiers,
                    kind: KeyEventKind::Press,
                    release_capture,
                }
            };
            if self.events.try_send(event).is_err() {
                self.failed.store(true, Ordering::Release);
                self.enabled.store(false, Ordering::Release);
                return false;
            }
            pressed.insert(vk, event);
            if release_capture {
                self.enabled.store(false, Ordering::Release);
            }
            true
        }
    }

    // SAFETY: Windows supplies a valid KBDLLHOOKSTRUCT for HC_ACTION. Its memory is
    // borrowed only for this callback. The callback never waits or performs UI/I/O.
    unsafe extern "system" fn callback(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code != HC_ACTION as i32 {
            return unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) };
        }
        let Ok(current) = CURRENT.try_lock() else {
            return unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) };
        };
        let Some(state) = current.as_ref() else {
            return unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) };
        };
        let data = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
        let up = matches!(wparam as u32, WM_KEYUP | WM_SYSKEYUP);
        let modifiers = Modifiers {
            alt: data.flags & LLKHF_ALTDOWN != 0,
            control: unsafe { GetAsyncKeyState(VK_CONTROL as i32) } < 0,
            shift: unsafe { GetAsyncKeyState(VK_SHIFT as i32) } < 0,
            super_key: unsafe { GetAsyncKeyState(VK_LWIN as i32) } < 0
                || unsafe { GetAsyncKeyState(VK_RWIN as i32) } < 0,
        };
        if state.route(
            data.vkCode,
            up,
            unsafe { GetForegroundWindow() } as usize,
            modifiers,
        ) {
            1
        } else {
            unsafe { CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam) }
        }
    }

    pub struct KeyboardCapture {
        state: Arc<State>,
        thread_id: u32,
        thread: Option<thread::JoinHandle<()>>,
        receive: async_channel::Receiver<CapturedKey>,
    }
    impl std::fmt::Debug for KeyboardCapture {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("KeyboardCapture")
                .field("enabled", &self.is_enabled())
                .finish()
        }
    }
    impl KeyboardCapture {
        pub fn new(window: &gpui::Window) -> Result<Self, String> {
            let handle = HasWindowHandle::window_handle(window).map_err(|e| e.to_string())?;
            let RawWindowHandle::Win32(handle) = handle.as_raw() else {
                return Err("Not a Windows window".into());
            };
            let (events, receive) = async_channel::bounded(128);
            let state = Arc::new(State {
                hwnd: handle.hwnd.get() as usize,
                enabled: AtomicBool::new(false),
                failed: AtomicBool::new(false),
                stopping: AtomicBool::new(false),
                target: AtomicU64::new(0),
                generation: AtomicU64::new(0),
                events,
                pressed: Mutex::new(HashMap::with_capacity(8)),
            });
            {
                let mut current = CURRENT.lock().map_err(|e| e.to_string())?;
                if current.is_some() {
                    return Err("Keyboard capture is already installed".into());
                }
                *current = Some(state.clone());
            }
            let (ready, installed) = std::sync::mpsc::sync_channel(0);
            let thread_state = state.clone();
            let thread = thread::Builder::new()
                .name("opsssh-keyboard".into())
                .spawn(move || {
                    // SAFETY: module handle is borrowed for this process; the hook and message
                    // queue live on this thread, and are destroyed before its state is released.
                    unsafe {
                        let mut message: MSG = std::mem::zeroed();
                        PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_NOREMOVE);
                        let hook = SetWindowsHookExW(
                            WH_KEYBOARD_LL,
                            Some(callback),
                            GetModuleHandleW(std::ptr::null()),
                            0,
                        );
                        if hook.is_null() {
                            let _ = ready.send(Err(std::io::Error::last_os_error().to_string()));
                        } else if thread_state.stopping.load(Ordering::Acquire) {
                            UnhookWindowsHookEx(hook);
                        } else {
                            if ready.send(Ok(GetCurrentThreadId())).is_ok() {
                                while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
                                    TranslateMessage(&message);
                                    DispatchMessageW(&message);
                                }
                            }
                            UnhookWindowsHookEx(hook);
                        }
                    }
                    if !thread_state.stopping.load(Ordering::Acquire) {
                        thread_state.failed.store(true, Ordering::Release);
                        thread_state.enabled.store(false, Ordering::Release);
                    }
                    thread_state.events.close();
                    if let Ok(mut current) = CURRENT.lock() {
                        *current = None;
                    }
                })
                .map_err(|e| {
                    if let Ok(mut current) = CURRENT.lock() {
                        *current = None;
                    }
                    e.to_string()
                })?;
            let thread_id = match installed.recv_timeout(Duration::from_secs(2)) {
                Ok(Ok(id)) => id,
                Ok(Err(error)) => {
                    let _ = thread.join();
                    return Err(error);
                }
                Err(error) => {
                    state.stopping.store(true, Ordering::Release);
                    // If installation just raced the timeout, its reply carries the thread ID.
                    if let Ok(Ok(id)) = installed.try_recv() {
                        unsafe {
                            PostThreadMessageW(id, WM_QUIT, 0, 0);
                        }
                    }
                    return Err(error.to_string());
                }
            };
            Ok(Self {
                state,
                thread_id,
                thread: Some(thread),
                receive,
            })
        }
        pub fn events(&self) -> async_channel::Receiver<CapturedKey> {
            self.receive.clone()
        }
        pub fn set_target(&self, target: Option<(u64, u64)>) {
            if self.state.failed.load(Ordering::Acquire) {
                self.state.enabled.store(false, Ordering::Release);
                return;
            }
            if let Some((target, generation)) = target {
                self.state.target.store(target, Ordering::Release);
                self.state.generation.store(generation, Ordering::Release);
                self.state.enabled.store(true, Ordering::Release);
            } else {
                self.state.enabled.store(false, Ordering::Release);
            }
        }
        pub fn has_failed(&self) -> bool {
            self.state.failed.load(Ordering::Acquire)
        }
        pub fn is_enabled(&self) -> bool {
            self.state.enabled.load(Ordering::Acquire)
        }
    }
    impl Drop for KeyboardCapture {
        fn drop(&mut self) {
            self.state.stopping.store(true, Ordering::Release);
            self.state.enabled.store(false, Ordering::Release);
            self.state.events.close();
            // SAFETY: thread_id belongs to the installing thread with a created message queue.
            unsafe {
                PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0);
            }
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }
    #[cfg(test)]
    mod route_tests {
        use super::*;
        fn state(capacity: usize) -> (State, async_channel::Receiver<CapturedKey>) {
            let (events, receive) = async_channel::bounded(capacity);
            (
                State {
                    hwnd: 12,
                    enabled: AtomicBool::new(true),
                    failed: AtomicBool::new(false),
                    stopping: AtomicBool::new(false),
                    target: AtomicU64::new(7),
                    generation: AtomicU64::new(3),
                    events,
                    pressed: Mutex::new(HashMap::with_capacity(8)),
                },
                receive,
            )
        }
        #[test]
        fn foreground_guard_repeat_release_and_exit_chord() {
            let (state, receive) = state(8);
            let alt = Modifiers {
                alt: true,
                ..Default::default()
            };
            assert!(!state.route(9, false, 13, alt));
            assert!(receive.is_empty());
            assert!(state.route(9, false, 12, alt));
            assert!(state.route(9, false, 12, alt));
            assert!(state.route(9, true, 12, Modifiers::default()));
            let events = (0..3)
                .map(|_| receive.try_recv().unwrap())
                .collect::<Vec<_>>();
            assert_eq!(
                events.iter().map(|e| e.kind).collect::<Vec<_>>(),
                vec![
                    KeyEventKind::Press,
                    KeyEventKind::Repeat,
                    KeyEventKind::Release
                ]
            );
            assert!(
                events
                    .iter()
                    .all(|e| e.target == 7 && e.generation == 3 && e.modifiers.alt)
            );
            assert!(state.route(
                0x7b,
                false,
                12,
                Modifiers {
                    alt: true,
                    control: true,
                    ..Default::default()
                }
            ));
            assert!(receive.try_recv().unwrap().release_capture);
            assert!(!state.enabled.load(Ordering::Acquire));
            assert!(!state.route(9, false, 12, alt));
        }
        #[test]
        fn delivery_failure_is_fail_open_and_stale_releases_are_not_forwarded() {
            let (state, receive) = state(1);
            let alt = Modifiers {
                alt: true,
                ..Default::default()
            };
            assert!(state.route(9, false, 12, alt));
            assert!(!state.route(0x73, false, 12, alt));
            assert!(state.failed.load(Ordering::Acquire));
            assert!(!state.enabled.load(Ordering::Acquire));
            receive.try_recv().unwrap();
            assert!(state.route(9, true, 12, alt));
            assert!(receive.is_empty());
            let (state, receive) = self::state(8);
            assert!(state.route(9, false, 12, alt));
            receive.try_recv().unwrap();
            state.generation.store(4, Ordering::Release);
            assert!(state.route(9, true, 12, alt));
            assert!(receive.is_empty());
            assert!(!state.route(
                9,
                false,
                12,
                Modifiers {
                    alt: true,
                    super_key: true,
                    ..Default::default()
                }
            ));
        }
    }
}
#[cfg(target_os = "windows")]
pub use native::KeyboardCapture;

#[cfg(not(target_os = "windows"))]
#[derive(Debug)]
pub struct KeyboardCapture;
#[cfg(not(target_os = "windows"))]
impl KeyboardCapture {
    pub fn new(_: &gpui::Window) -> Result<Self, String> {
        Err("System keyboard capture is available on Windows".into())
    }
    pub fn events(&self) -> async_channel::Receiver<CapturedKey> {
        async_channel::bounded(1).1
    }
    pub fn set_target(&self, _: Option<(u64, u64)>) {}
    pub fn has_failed(&self) -> bool {
        false
    }
    pub fn is_enabled(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_system_chords_and_release_are_captured() {
        assert_eq!(captured_key(9, true, false, false), Some((Key::Tab, false)));
        assert_eq!(captured_key(9, true, false, true), Some((Key::Tab, false)));
        assert_eq!(
            captured_key(0x73, true, false, false),
            Some((Key::Function(4), false))
        );
        assert_eq!(
            captured_key(0x7b, true, true, false),
            Some((Key::Function(12), true))
        );
        assert_eq!(captured_key(9, false, false, false), None);
        assert_eq!(captured_key(0x2e, true, true, false), None);
        assert_eq!(captured_key(0x43, false, true, false), None);
    }
}
