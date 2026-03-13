use rustrag_backend::app;

fn main() -> anyhow::Result<()> {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?
        .block_on(app::run())
}
