#[cfg(feature = "heap-profile")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

use ironrag_backend::app;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    #[cfg(feature = "heap-profile")]
    let _profiler = dhat::Profiler::new_heap();

    app::run().await
}
