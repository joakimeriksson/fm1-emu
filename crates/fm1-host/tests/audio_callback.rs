use fm1_host::audio::{Playback, RATE};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! { static ALLOCATIONS: Cell<Option<usize>> = const { Cell::new(None) }; }
struct CountingAllocator;
// SAFETY: all allocations and deallocations delegate to the system allocator unchanged.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.with(|count| { if let Some(n) = count.get() { count.set(Some(n + 1)); } });
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) { unsafe { System.dealloc(ptr, layout) } }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        ALLOCATIONS.with(|count| { if let Some(n) = count.get() { count.set(Some(n + 1)); } });
        unsafe { System.realloc(ptr, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

#[test]
fn callback_does_not_allocate_during_playback_starvation_or_recovery() {
    let (mut tx, rx) = rtrb::RingBuffer::new(1024);
    let mut playback = Playback::new(rx, 48_000.0);
    for _ in 0..512 { tx.push([0.25, -0.25]).unwrap(); }
    ALLOCATIONS.with(|count| count.set(Some(0)));
    for _ in 0..1024 { std::hint::black_box(playback.frame(128)); }
    for _ in 0..512 { tx.push([0.25, -0.25]).unwrap(); }
    for _ in 0..128 { std::hint::black_box(playback.frame(128)); }
    let allocations = ALLOCATIONS.with(|count| count.replace(None).unwrap());
    assert_eq!(allocations, 0);
    assert_eq!(playback.underruns, 1);
}

#[test]
fn ring_wraparound_does_not_split_stereo_or_reorder_samples() {
    let (mut tx, rx) = rtrb::RingBuffer::new(64);
    let mut playback = Playback::new(rx, RATE);
    for i in 0..32 { tx.push([i as f32, -(i as f32)]).unwrap(); }
    for i in 0..10_000 {
        let frame = playback.frame(16);
        assert_eq!(frame, [i as f32, -(i as f32)]);
        tx.push([(i+32) as f32, -((i+32) as f32)]).unwrap();
    }
    assert_eq!(playback.underruns, 0);
}
