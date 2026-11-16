//! Offline interaction-harness executable with Rust allocation accounting.
//! Uses the real CLI/renderer; normal octet binaries retain their own allocator.
use std::alloc::{GlobalAlloc, Layout, System};

struct CountingAllocator;
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn record(bytes: usize) {
    sexy_tui_rs::render_metrics::record(|work| {
        work.allocations += 1;
        work.allocated_bytes += bytes as u64;
    });
}

// As in sexy-tui-rs/examples/render_bench.rs, delegate every operation to System.
// Accounting touches only const-initialized thread-local Cells, never allocates,
// and tolerates TLS teardown. Foreign allocations are not Rust requests.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record(size);
        unsafe { System.realloc(pointer, layout, size) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }
}

fn main() -> std::process::ExitCode {
    let code = octet_sdk::build_runtime()
        .expect("build profiling runtime")
        .block_on(octet_sdk::run_cli());
    octet_sdk::restore_starting_terminal_attributes();
    code
}
