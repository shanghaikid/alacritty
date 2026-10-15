//! Small C ABI for tmux. Handles are owned by one pane on its event-loop thread.
use crate::{Outcome, Store, Terminator};
use std::ffi::{c_char, c_int};

struct Handle {
    store: Store,
    label: String,
}

#[unsafe(no_mangle)]
pub extern "C" fn ps_new() -> *mut std::ffi::c_void {
    Box::into_raw(Box::new(Handle { store: Store::default(), label: String::new() })).cast()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ps_free(handle: *mut std::ffi::c_void) {
    if !handle.is_null() {
        drop(unsafe { Box::from_raw(handle.cast::<Handle>()) });
    }
}

/// 0 unchanged/rejected, 1 changed, 2 constant capability reply requested.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ps_apply(
    handle: *mut std::ffi::c_void,
    body: *const u8,
    len: usize,
    bel: c_int,
) -> c_int {
    let h = unsafe { &mut *handle.cast::<Handle>() };
    let bytes = unsafe { std::slice::from_raw_parts(body, len) };
    match h.store.apply(bytes, if bel != 0 { Terminator::Bel } else { Terminator::St }) {
        Outcome::Changed => 1,
        Outcome::Query(_) => 2,
        _ => 0,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ps_expire(handle: *mut std::ffi::c_void, reset: c_int) -> c_int {
    if handle.is_null() {
        return 0;
    }
    let h = unsafe { &mut *handle.cast::<Handle>() };
    c_int::from(if reset != 0 { h.store.reset() } else { h.store.expire_transient() })
}

/// Borrowed until the next call on this handle. Text contains no C0 or NUL.
/// tmux displays only state/kind/progress here, retaining its own pane identity.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ps_label(handle: *mut std::ffi::c_void) -> *const c_char {
    static EMPTY: &[u8] = b"\0";
    if handle.is_null() {
        return EMPTY.as_ptr().cast();
    }
    let h = unsafe { &mut *handle.cast::<Handle>() };
    h.label.clear();
    if let Some(record) = h.store.summary() {
        use std::fmt::Write;
        let name = record.kind.map_or(record.state.name(), crate::Kind::name);
        let _ = write!(h.label, "[{name}");
        if let Some(progress) = record.progress {
            let _ = write!(h.label, " {progress}%");
        }
        h.label.push(']');
    }
    h.label.push('\0');
    h.label.as_ptr().cast()
}
