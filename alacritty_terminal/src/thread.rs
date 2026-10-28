use std::thread::{Builder, JoinHandle};

/// PTY ingestion feeds user-visible work. On Apple Silicon, default-QoS
/// workers can otherwise be scheduled on efficiency cores with long wakeups.
#[cfg(target_os = "macos")]
pub(crate) fn prioritize_terminal_io() {
    // This affects only the calling worker, with no real-time priority or
    // affinity restriction. Failure leaves the OS's existing scheduling intact.
    let result = unsafe {
        libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INITIATED, 0)
    };
    if result != 0 {
        log::warn!("Unable to set terminal I/O QoS: {}", std::io::Error::from_raw_os_error(result));
    }
}

/// Like `thread::spawn`, but with a `name` argument.
pub fn spawn_named<F, T, S>(name: S, f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
    S: Into<String>,
{
    Builder::new().name(name.into()).spawn(f).expect("thread spawn works")
}
