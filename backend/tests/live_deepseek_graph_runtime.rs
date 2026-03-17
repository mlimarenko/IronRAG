use std::{env, path::Path, process::Command};

#[test]
#[ignore = "requires RUSTRAG_DEEPSEEK_API_KEY and the local runtime stack"]
fn live_deepseek_graph_runtime_smoke() {
    if env::var("RUSTRAG_DEEPSEEK_API_KEY").unwrap_or_default().trim().is_empty() {
        eprintln!("RUSTRAG_DEEPSEEK_API_KEY is not set; skipping ignored live DeepSeek test");
        return;
    }

    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("backend repo root");
    let output_dir = repo_root.join("docs/checkpoints/runtime-smoke").join("deepseek-live-test");
    let status = Command::new("bash")
        .arg(repo_root.join("scripts/smoke/runtime-deepseek.sh"))
        .arg(&output_dir)
        .current_dir(repo_root)
        .status()
        .expect("run runtime-deepseek.sh");

    assert!(status.success(), "runtime-deepseek.sh exited with {status}");
}
