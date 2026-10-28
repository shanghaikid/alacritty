//! Bounded PTY read-ahead for macOS, where master reads are usually just 1 KiB.
//!
//! The parser can consume a previous batch while this thread drains the kernel
//! queue. A readiness socket integrates the batches with the existing event
//! loop; writes, resize, child signals and parser timeouts stay on that loop.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, ErrorKind, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};

const BUFFER_SIZE: usize = 64 * 1024;
const BUFFER_COUNT: usize = 4;
const GATHER_BUDGET: Duration = Duration::from_millis(3);

struct Batch {
    bytes: Box<[u8]>,
    offset: usize,
    len: usize,
}

struct State {
    free: Vec<Box<[u8]>>,
    ready: VecDeque<Batch>,
    stop: bool,
    drain: bool,
    done: bool,
    waiting_for_refill: bool,
    error: Option<io::Error>,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    stop: AtomicBool,
}

pub struct PtyReader {
    /// Readable exactly while a batch or a terminal result is pending.
    pub(super) notification: UnixStream,
    // Avoid reporting socket HUP when the worker has published its last batch;
    // the event loop intentionally ignores I/O events marked as interrupted.
    _notification_sender: UnixStream,
    control: UnixStream,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl PtyReader {
    pub(super) fn new(file: File) -> io::Result<Self> {
        Self::spawn(file)
    }

    fn spawn<R: Read + AsRawFd + Send + 'static>(file: R) -> io::Result<Self> {
        let (notification, sender) = UnixStream::pair()?;
        let (control, receiver) = UnixStream::pair()?;
        for socket in [&notification, &sender, &control, &receiver] {
            socket.set_nonblocking(true)?;
        }
        let notification_sender = sender.try_clone()?;
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                free: (0..BUFFER_COUNT).map(|_| vec![0; BUFFER_SIZE].into_boxed_slice()).collect(),
                ready: VecDeque::with_capacity(BUFFER_COUNT),
                stop: false,
                drain: false,
                done: false,
                waiting_for_refill: false,
                error: None,
            }),
            changed: Condvar::new(),
            stop: AtomicBool::new(false),
        });
        let worker_shared = shared.clone();
        let thread = thread::Builder::new().name("PTY gather".into()).spawn(move || {
            crate::thread::prioritize_terminal_io();
            gather(file, receiver, sender, &worker_shared);
        })?;
        Ok(Self {
            notification,
            _notification_sender: notification_sender,
            control,
            shared,
            thread: Some(thread),
        })
    }

    /// After child exit, read through the next empty kernel queue. Reads then
    /// wait for the gather thread instead of mistaking an in-flight batch for
    /// the end of output. An open slave inherited by a descendant need not EOF.
    pub(super) fn begin_drain(&mut self) {
        self.shared.state.lock().drain = true;
        let _ = self.control.write(&[1]);
    }
}

impl Read for PtyReader {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        let mut state = self.shared.state.lock();
        while state.ready.is_empty() && state.drain && !state.done {
            self.shared.changed.wait(&mut state);
        }
        let result = if let Some(batch) = state.ready.front_mut() {
            let len = output.len().min(batch.len - batch.offset);
            output[..len].copy_from_slice(&batch.bytes[batch.offset..batch.offset + len]);
            batch.offset += len;
            if batch.offset == batch.len {
                let batch = state.ready.pop_front().unwrap();
                state.free.push(batch.bytes);
                self.shared.changed.notify_one();
                if state.ready.is_empty() && state.waiting_for_refill {
                    // The parser has caught up. Publish the gatherer's pending
                    // bytes now rather than waiting for its refill timer.
                    let _ = self.control.write(&[1]);
                }
            }
            Ok(len)
        } else if let Some(error) = state.error.take() {
            Err(error)
        } else if state.done {
            Ok(0)
        } else {
            Err(ErrorKind::WouldBlock.into())
        };
        let terminal_result_pending =
            matches!(&result, Ok(n) if *n != 0) && (state.done || state.error.is_some());
        if state.ready.is_empty() && !terminal_result_pending {
            // Keep the state locked through clearing readiness so a concurrent
            // publication cannot lose its notification. Consuming the last
            // batch does not consume a separately pending EOF/error result.
            let mut signal = [0; 64];
            while self.notification.read(&mut signal).is_ok_and(|n| n != 0) {}
        }
        result
    }
}

impl Drop for PtyReader {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.state.lock().stop = true;
        self.shared.changed.notify_all();
        let _ = self.control.write(&[1]);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn wait_readable(fd: i32, control: &mut UnixStream, timeout: i32) -> io::Result<bool> {
    let mut fds = [
        libc::pollfd { fd, events: libc::POLLIN, revents: 0 },
        libc::pollfd { fd: control.as_raw_fd(), events: libc::POLLIN, revents: 0 },
    ];
    // Both descriptors are owned by this thread and outlive poll.
    let result = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, timeout) };
    if result < 0 {
        let error = io::Error::last_os_error();
        return if error.kind() == ErrorKind::Interrupted { Ok(true) } else { Err(error) };
    }
    if fds[1].revents != 0 {
        let mut signal = [0; 64];
        while control.read(&mut signal).is_ok_and(|n| n != 0) {}
    }
    Ok(result != 0)
}

fn gather<R: Read + AsRawFd>(
    mut file: R,
    mut control: UnixStream,
    mut sender: UnixStream,
    shared: &Shared,
) {
    let mut failure = None;
    let mut done = false;
    while !done {
        let mut bytes = {
            let mut state = shared.state.lock();
            while state.free.is_empty() && !state.stop {
                shared.changed.wait(&mut state);
            }
            if state.stop {
                break;
            }
            state.free.pop().unwrap()
        };
        let mut len = 0;
        let mut spins = 0;
        let mut started = None;
        while len < bytes.len() {
            if shared.stop.load(Ordering::Relaxed) {
                done = true;
                break;
            }
            match file.read(&mut bytes[len..]) {
                Ok(0) => {
                    done = true;
                    break;
                },
                Ok(count) => {
                    len += count;
                    spins = 0;
                    started.get_or_insert_with(Instant::now);
                },
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    if shared.state.lock().drain {
                        done = true;
                        break;
                    }
                    // Bridge writer refill gaps only while parsing queued work.
                    // Draining that work wakes control and publishes this batch.
                    if len != 0 {
                        if len < 1024 || started.is_some_and(|t| t.elapsed() >= GATHER_BUDGET) {
                            break;
                        }
                        if spins < 16 {
                            spins += 1;
                            std::hint::spin_loop();
                            continue;
                        }
                        let mut state = shared.state.lock();
                        if state.ready.is_empty() {
                            break;
                        }
                        state.waiting_for_refill = true;
                    }
                    let readable = wait_readable(
                        file.as_raw_fd(),
                        &mut control,
                        if len == 0 { -1 } else { 1 },
                    );
                    let caught_up = {
                        let mut state = shared.state.lock();
                        state.waiting_for_refill = false;
                        len != 0 && state.ready.is_empty()
                    };
                    match readable {
                        Ok(_) if caught_up => break,
                        Ok(true) => (),
                        Ok(false) => break,
                        Err(error) => {
                            failure = Some(error);
                            done = true;
                            break;
                        },
                    }
                },
                Err(error) => {
                    failure = Some(error);
                    done = true;
                    break;
                },
            }
        }
        let mut state = shared.state.lock();
        let was_empty = state.ready.is_empty();
        if len != 0 {
            state.ready.push_back(Batch { bytes, offset: 0, len });
            if was_empty {
                let _ = sender.write(&[1]);
            }
            shared.changed.notify_all();
        } else {
            state.free.push(bytes);
        }
    }
    let mut state = shared.state.lock();
    state.done = true;
    state.error = failure;
    let _ = sender.write(&[1]);
    shared.changed.notify_all();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn final_notification_survives_last_batch_until_result_is_read() {
        struct EndResult(UnixStream, bool);
        impl AsRawFd for EndResult {
            fn as_raw_fd(&self) -> i32 {
                self.0.as_raw_fd()
            }
        }
        impl Read for EndResult {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                match self.0.read(buffer) {
                    Ok(0) if self.1 => Err(io::Error::other("test terminal error")),
                    result => result,
                }
            }
        }
        for error in [false, true] {
            let (input, mut writer) = UnixStream::pair().unwrap();
            input.set_nonblocking(true).unwrap();
            let mut reader = PtyReader::spawn(EndResult(input, error)).unwrap();
            writer.write_all(b"last batch").unwrap();
            drop(writer);
            {
                let mut state = reader.shared.state.lock();
                let deadline = Instant::now() + Duration::from_secs(2);
                while !state.done {
                    assert!(Instant::now() < deadline, "worker did not publish its final result");
                    reader.shared.changed.wait_for(&mut state, Duration::from_millis(10));
                }
            }
            let mut data = [0; 10];
            assert_eq!(reader.read(&mut data).unwrap(), data.len());
            assert_eq!(&data, b"last batch");
            let mut descriptor = libc::pollfd {
                fd: reader.notification.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // EOF/errors are separate results and must still wake the poller
            // when the parser yields immediately after consuming the last batch.
            assert_eq!(unsafe { libc::poll(&mut descriptor, 1, 0) }, 1);
            let result = reader.read(&mut data);
            if error {
                assert_eq!(result.unwrap_err().kind(), ErrorKind::Other);
            } else {
                assert_eq!(result.unwrap(), 0);
            }
            assert_eq!(unsafe { libc::poll(&mut descriptor, 1, 0) }, 0);
        }
    }

    #[test]
    fn readiness_survives_partial_reads_and_repeated_publication() {
        let (input, mut writer) = UnixStream::pair().unwrap();
        input.set_nonblocking(true).unwrap();
        let mut reader = PtyReader::spawn(input).unwrap();
        let mut descriptor =
            libc::pollfd { fd: reader.notification.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        for value in 0..100u8 {
            writer.write_all(&[value; 3]).unwrap();
            // The descriptor is owned by reader and remains open throughout.
            assert_eq!(unsafe { libc::poll(&mut descriptor, 1, 1000) }, 1);
            let mut first = [0];
            assert_eq!(reader.read(&mut first).unwrap(), 1);
            assert_eq!(first, [value]);
            assert_eq!(unsafe { libc::poll(&mut descriptor, 1, 0) }, 1);
            let mut rest = [0; 2];
            assert_eq!(reader.read(&mut rest).unwrap(), 2);
            assert_eq!(rest, [value; 2]);
            assert_eq!(unsafe { libc::poll(&mut descriptor, 1, 0) }, 0);
        }
    }

    #[test]
    fn pipeline_preserves_bytes_and_partial_reads() {
        let (input, mut writer) = UnixStream::pair().unwrap();
        input.set_nonblocking(true).unwrap();
        let mut reader = PtyReader::spawn(input).unwrap();
        let expected: Vec<u8> = (0..BUFFER_SIZE * 19).map(|n| (n % 251) as u8).collect();
        let sent = expected.clone();
        let writer = thread::spawn(move || writer.write_all(&sent).unwrap());
        let mut actual = Vec::new();
        let mut buf = [0; 997];
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(Instant::now() < deadline, "pipeline stalled");
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => actual.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(1))
                },
                Err(e) => panic!("{e}"),
            }
        }
        writer.join().unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn drain_does_not_require_slave_eof() {
        let (input, mut writer) = UnixStream::pair().unwrap();
        input.set_nonblocking(true).unwrap();
        let mut reader = PtyReader::spawn(input).unwrap();
        writer.write_all(b"last output before child exit").unwrap();
        reader.begin_drain();
        let mut actual = Vec::new();
        reader.read_to_end(&mut actual).unwrap();
        assert_eq!(actual, b"last output before child exit");
        drop(writer);
    }

    #[test]
    fn drop_unblocks_idle_and_full_pipeline() {
        for full in [false, true] {
            let (input, mut writer) = UnixStream::pair().unwrap();
            input.set_nonblocking(true).unwrap();
            let reader = PtyReader::spawn(input).unwrap();
            let producer = thread::spawn(move || {
                if full {
                    let _ = writer.write_all(&vec![42; BUFFER_SIZE * (BUFFER_COUNT + 3)]);
                }
                thread::sleep(Duration::from_millis(100));
            });
            thread::sleep(Duration::from_millis(20));
            let start = Instant::now();
            drop(reader);
            assert!(start.elapsed() < Duration::from_secs(1));
            producer.join().unwrap();
        }
    }
}
