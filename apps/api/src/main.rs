#[cfg(feature = "heap-profile")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

// Production allocator: jemalloc with a background decay thread so freed pages
// are returned to the OS instead of being retained in glibc per-thread arenas
// (the cause of the idle worker sitting at multiple GiB of RSS and recurring
// MEMCG OOM kills). Decay timing and the background thread are enabled at
// runtime via the `_RJEM_MALLOC_CONF` env var baked into the runtime image
// (apps/api/Dockerfile), so no `unsafe` configuration symbol is required here
// (the `malloc_conf` symbol would need `#[export_name]`, which the crate's
// forbid(unsafe_code) lint rejects). Disabled under `heap-profile`, where dhat
// must own the global allocator, and on msvc, where jemalloc is unsupported.
#[cfg(all(not(feature = "heap-profile"), not(target_env = "msvc")))]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

use ironrag_backend::app;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    #[cfg(feature = "heap-profile")]
    let _profiler = dhat::Profiler::new_heap();

    app::run().await
}
