//! Brace expansion holds its budget while it expands (spec 1.4: 1,000 patterns, 4 MiB). This
//! file is its own test binary, so the counting allocator sees only this test.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the layout comes from the caller, as `GlobalAlloc::alloc` requires.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            let live = LIVE.fetch_add(layout.size(), Ordering::SeqCst) + layout.size();
            PEAK.fetch_max(live, Ordering::SeqCst);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` was allocated by `alloc` above with this layout.
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::SeqCst);
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// The most memory one expansion may hold: the two pattern lists of one step, each within the
/// 4 MiB budget, plus a working copy of the pattern. A 90 KB pattern stays far below it.
const MEMORY_BOUND: usize = 16 * 1024 * 1024;

#[test]
fn a_huge_brace_group_is_over_budget_without_allocating_its_expansion() {
    // The reviewer's rule file: one group of 20,000 alternatives and a 50,000-byte tail. Expanded
    // in full it is 20,000 patterns of 50,001 bytes, about 1 GB.
    let pattern = format!("{{{}}}{}", vec!["a"; 20_000].join(","), "x".repeat(50_000));

    let start = LIVE.load(Ordering::SeqCst);
    PEAK.store(start, Ordering::SeqCst);
    let expanded = cm_core::glob::expand_braces(&pattern);
    let peak = PEAK.load(Ordering::SeqCst) - start;

    assert!(expanded.is_none(), "the expansion is over budget");
    assert!(
        peak < MEMORY_BOUND,
        "expansion held {peak} bytes at its peak, the bound is {MEMORY_BOUND}"
    );
}
