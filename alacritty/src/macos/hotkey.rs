//! Native system-wide hotkey registration on the AppKit main thread.

use std::cell::Cell;
use std::ffi::c_void;
use std::mem;
use std::ptr;

use objc2::MainThreadMarker;
use serde::{Deserialize, Serialize, Serializer};
use winit::event_loop::EventLoopProxy;
use winit::keyboard::{KeyCode, ModifiersState};
use winit::platform::scancode::PhysicalKeyExtScancode;

use alacritty_config::SerdeReplace;

use crate::config::ModsWrapper;
use crate::event::{Event, EventType};

const KEYBOARD_EVENT: u32 = u32::from_be_bytes(*b"keyb");
const HOTKEY_SIGNATURE: u32 = u32::from_be_bytes(*b"Alac");
const HOTKEY_PRESSED: u32 = 5;
const HOTKEY_RELEASED: u32 = 6;
const EVENT_NOT_HANDLED: i32 = -9874;

#[derive(Deserialize, Serialize, Clone, Debug, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct HotkeyConfig {
    pub enabled: bool,
    pub key: KeyCode,
    #[serde(serialize_with = "serialize_modifiers")]
    pub mods: ModsWrapper,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            key: KeyCode::KeyL,
            mods: ModsWrapper(ModifiersState::SUPER | ModifiersState::SHIFT),
        }
    }
}

impl SerdeReplace for HotkeyConfig {
    fn replace(&mut self, value: toml::Value) -> Result<(), Box<dyn std::error::Error>> {
        let current = toml::Value::try_from(&*self)?;
        *self = Self::deserialize(crate::config::serde_utils::merge(current, value))?;
        Ok(())
    }
}

fn serialize_modifiers<S: Serializer>(
    mods: &ModsWrapper,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let names: Vec<_> = [
        (ModifiersState::SUPER, "Command"),
        (ModifiersState::SHIFT, "Shift"),
        (ModifiersState::CONTROL, "Control"),
        (ModifiersState::ALT, "Alt"),
    ]
    .into_iter()
    .filter_map(|(flag, name)| mods.0.contains(flag).then_some(name))
    .collect();
    let value = if names.is_empty() { "None".into() } else { names.join("|") };
    serializer.serialize_str(&value)
}

/// Carbon's modifier flags differ from AppKit's and winit's flags.
fn carbon_modifiers(mods: ModifiersState) -> u32 {
    let mut flags = 0;
    for (modifier, flag) in [
        (ModifiersState::SUPER, 1 << 8),
        (ModifiersState::SHIFT, 1 << 9),
        (ModifiersState::ALT, 1 << 11),
        (ModifiersState::CONTROL, 1 << 12),
    ] {
        if mods.contains(modifier) {
            flags |= flag;
        }
    }
    flags
}

struct CallbackState {
    proxy: EventLoopProxy<Event>,
    id: Cell<u32>,
    pressed: Cell<bool>,
}

struct Registration {
    handle: *mut c_void,
    config: HotkeyConfig,
}

impl Drop for Registration {
    fn drop(&mut self) {
        // SAFETY: This handle belongs to us and all registration changes run on the main thread.
        unsafe { UnregisterEventHotKey(self.handle) };
    }
}

/// Owns the native handler and its stable callback allocation; never moves off the main thread.
pub struct GlobalHotkey {
    handler: *mut c_void,
    state: Box<CallbackState>,
    registration: Option<Registration>,
    next_id: u32,
    _main_thread: MainThreadMarker,
}

impl GlobalHotkey {
    pub fn new(proxy: EventLoopProxy<Event>, mtm: MainThreadMarker) -> Result<Self, i32> {
        let mut state =
            Box::new(CallbackState { proxy, id: Cell::new(0), pressed: Cell::new(false) });
        let events = [
            EventTypeSpec { class: KEYBOARD_EVENT, kind: HOTKEY_PRESSED },
            EventTypeSpec { class: KEYBOARD_EVENT, kind: HOTKEY_RELEASED },
        ];
        let mut handler = ptr::null_mut();
        // SAFETY: The boxed state outlives the handler and stays at the same address. The handler
        // is removed before dropping the box. Carbon calls it only on the main event loop.
        let status = unsafe {
            InstallEventHandler(
                GetApplicationEventTarget(),
                hotkey_callback,
                events.len(),
                events.as_ptr(),
                ptr::from_mut(&mut *state).cast(),
                &mut handler,
            )
        };
        if status != 0 {
            return Err(status);
        }

        Ok(Self { handler, state, registration: None, next_id: 0, _main_thread: mtm })
    }

    /// Register replacements before releasing the old shortcut, so conflicts keep it working.
    pub fn update(&mut self, config: &HotkeyConfig) -> Result<(), String> {
        if !config.enabled {
            self.state.id.set(0);
            self.state.pressed.set(false);
            self.registration = None;
            return Ok(());
        }
        if self.registration.as_ref().is_some_and(|registered| registered.config == *config) {
            return Ok(());
        }

        let keycode = config.key.to_scancode().ok_or("Unsupported global hotkey key")?;
        if config.mods.0.is_empty() {
            return Err("Global hotkey requires at least one modifier".into());
        }
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let id = EventHotKeyId { signature: HOTKEY_SIGNATURE, id: self.next_id };
        let mut handle = ptr::null_mut();
        // SAFETY: All arguments match Carbon's ABI, the target is the application event loop,
        // and the main-thread marker prevents this owner from moving to another thread.
        let status = unsafe {
            RegisterEventHotKey(
                keycode,
                carbon_modifiers(config.mods.0),
                id,
                GetApplicationEventTarget(),
                1, // kEventHotKeyExclusive: do not toggle alongside another hotkey owner.
                &mut handle,
            )
        };
        if status != 0 {
            return Err(format!(
                "Could not register global hotkey {:?} (macOS error {status}); check for a \
                 conflicting shortcut in Hammerspoon or another application. Previous hotkey, \
                 if any, remains active",
                config.key,
            ));
        }

        self.registration = Some(Registration { handle, config: config.clone() });
        self.state.id.set(id.id);
        self.state.pressed.set(false);
        log::info!("Registered native global hotkey: {:?} + {:?}", config.mods.0, config.key);
        Ok(())
    }

    /// Ignore notifications queued before a configuration change disabled/replaced the hotkey.
    pub fn is_current(&self, id: u32) -> bool {
        id != 0 && self.state.id.get() == id
    }
}

impl Drop for GlobalHotkey {
    fn drop(&mut self) {
        self.registration = None;
        // SAFETY: Remove the handler before its boxed callback data is freed.
        unsafe { RemoveEventHandler(self.handler) };
    }
}

unsafe extern "C" fn hotkey_callback(
    _handler: *mut c_void,
    event: *mut c_void,
    user_data: *mut c_void,
) -> i32 {
    let mut id = EventHotKeyId { signature: 0, id: 0 };
    // SAFETY: Carbon supplies the event, and the output buffer matches EventHotKeyID's ABI.
    let status = unsafe {
        GetEventParameter(
            event,
            u32::from_be_bytes(*b"----"), // kEventParamDirectObject.
            u32::from_be_bytes(*b"hkid"), // typeEventHotKeyID.
            ptr::null_mut(),
            mem::size_of::<EventHotKeyId>(),
            ptr::null_mut(),
            ptr::from_mut(&mut id).cast(),
        )
    };
    if status != 0 || id.signature != HOTKEY_SIGNATURE {
        return EVENT_NOT_HANDLED;
    }

    // SAFETY: The allocation is owned by GlobalHotkey and outlives this registered handler.
    let state = unsafe { &*user_data.cast::<CallbackState>() };
    if id.id != state.id.get() {
        return EVENT_NOT_HANDLED;
    }
    // SAFETY: Carbon supplies a valid event for the duration of the callback.
    match unsafe { GetEventKind(event) } {
        HOTKEY_PRESSED if !state.pressed.replace(true) => {
            let _ = state.proxy.send_event(Event::new(EventType::GlobalHotkey(id.id), None));
        },
        HOTKEY_RELEASED => state.pressed.set(false),
        _ => (),
    }
    0
}

#[repr(C)]
struct EventTypeSpec {
    class: u32,
    kind: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct EventHotKeyId {
    signature: u32,
    id: u32,
}

// These declarations match the public CarbonEvents{,Core}.h SDK headers.
#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn GetApplicationEventTarget() -> *mut c_void;
    fn InstallEventHandler(
        target: *mut c_void,
        handler: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> i32,
        count: usize,
        events: *const EventTypeSpec,
        user_data: *mut c_void,
        handler_ref: *mut *mut c_void,
    ) -> i32;
    fn RemoveEventHandler(handler: *mut c_void) -> i32;
    fn RegisterEventHotKey(
        keycode: u32,
        modifiers: u32,
        id: EventHotKeyId,
        target: *mut c_void,
        options: u32,
        hotkey: *mut *mut c_void,
    ) -> i32;
    fn UnregisterEventHotKey(hotkey: *mut c_void) -> i32;
    fn GetEventKind(event: *mut c_void) -> u32;
    fn GetEventParameter(
        event: *mut c_void,
        name: u32,
        kind: u32,
        actual_kind: *mut u32,
        size: usize,
        actual_size: *mut usize,
        data: *mut c_void,
    ) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_key_and_modifier_codes() {
        assert_eq!(KeyCode::KeyL.to_scancode(), Some(37));
        assert_eq!(carbon_modifiers(ModifiersState::SUPER | ModifiersState::SHIFT), 0x300);
        assert_eq!(carbon_modifiers(ModifiersState::CONTROL | ModifiersState::ALT), 0x1800);
    }

    #[test]
    fn hotkey_config_serialization_roundtrip() {
        let config = HotkeyConfig { enabled: true, ..Default::default() };
        let value = toml::to_string(&config).unwrap();
        assert_eq!(toml::from_str::<HotkeyConfig>(&value).unwrap(), config);
    }
}
