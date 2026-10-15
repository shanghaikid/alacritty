use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use alacritty_status::{MAX_RECORDS, Store, Terminator};
use base64::Engine;

struct Counter;
static COUNTING: AtomicBool = AtomicBool::new(false);
static ALLOCS: AtomicU64 = AtomicU64::new(0);
unsafe impl GlobalAlloc for Counter {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(pointer, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: Counter = Counter;

fn run(name: &str, packets: &[Vec<u8>]) {
    let mut store = Store::default();
    for i in 0..10_000 {
        black_box(store.apply(&packets[i % packets.len()], Terminator::St));
    }
    let iterations = 1_000_000;
    ALLOCS.store(0, Ordering::Relaxed);
    COUNTING.store(true, Ordering::Relaxed);
    let start = Instant::now();
    for i in 0..iterations {
        black_box(store.apply(black_box(&packets[i % packets.len()]), Terminator::St));
    }
    let elapsed = start.elapsed();
    COUNTING.store(false, Ordering::Relaxed);
    let allocations = ALLOCS.load(Ordering::Relaxed);
    println!(
        "{name:20} {:8.1} ns/report  {:8.2} M reports/s  allocations={allocations}",
        elapsed.as_nanos() as f64 / iterations as f64,
        iterations as f64 / elapsed.as_secs_f64() / 1e6
    );
    assert_eq!(allocations, 0, "warmed {name} allocated");
}

fn main() {
    println!(
        "Store inline size: {} bytes; record cap: {MAX_RECORDS}",
        std::mem::size_of::<Store>()
    );
    run("duplicate", &[b"state=working:app=build:progress=40".to_vec()]);
    run(
        "progress",
        &[
            b"state=working:app=build:progress=40".to_vec(),
            b"state=working:app=build:progress=41".to_vec(),
        ],
    );
    run(
        "64 record writers",
        &(0..MAX_RECORDS)
            .map(|i| format!("state=working:id=child{i}:progress=40").into_bytes())
            .collect::<Vec<_>>(),
    );
    let engine = base64::engine::general_purpose::STANDARD;
    let text = engine.encode("x".repeat(2048));
    run(
        "max text updates",
        &[
            format!("state=working:progress=40:msg={text}").into_bytes(),
            format!("state=working:progress=41:msg={text}").into_bytes(),
        ],
    );
    run("invalid base64", &[b"state=working:msg=____".to_vec()]);
    run("overlimit", &[vec![b'a'; 4096]]);
    run("query", &[b"?".to_vec()]);
}
