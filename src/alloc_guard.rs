//! Counts the allocations a thread makes, so a test can show that code meant for
//! the audio thread does not allocate once it is running. Test builds only: the
//! counting allocator stands in for the system one in the whole test binary, and
//! it counts per thread, so tests running in parallel do not disturb each other.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    // A `const` cell with no destructor: reading it never allocates, and it can be
    // read at any point in a thread's life, so the allocator may use it freely.
    static COUNT: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

fn count() {
    COUNT.with(|c| c.set(c.get() + 1));
}

// SAFETY: every method forwards to `System` with the arguments it was given.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count();
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        count();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Run `f` and say how many times this thread asked for memory while it ran
/// (an allocation, a zeroed one, or growing a block).
pub fn allocations_in<R>(f: impl FnOnce() -> R) -> (R, usize) {
    let before = COUNT.with(Cell::get);
    let result = f();
    (result, COUNT.with(Cell::get) - before)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_counts_what_the_closure_allocates_and_nothing_else() {
        let (_, none) = allocations_in(|| 1 + 1);
        assert_eq!(none, 0);

        let (v, some) = allocations_in(|| vec![0u8; 1024]);
        assert_eq!(v.len(), 1024);
        assert_eq!(some, 1);

        let (_, grown) = allocations_in(|| {
            let mut v: Vec<u8> = Vec::new();
            v.push(1); // the first block
            v.extend([0u8; 100]); // grown
            v
        });
        assert!(grown >= 2, "{grown}");
    }

    #[test]
    fn other_threads_allocations_are_not_counted() {
        // Starting and joining a thread allocates a handful of times here, but the
        // thousand boxes made over there are not ours.
        let (_, mine) = allocations_in(|| {
            std::thread::spawn(|| (0..1000).map(Box::new).count()).join().unwrap()
        });
        assert!(mine < 100, "{mine}");
    }
}
