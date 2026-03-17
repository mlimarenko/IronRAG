use std::{env, path::Path, process::Command};

#[test]
#[ignore = "requires RUSTRAG_OPENAI_API_KEY and the local runtime stack"]
fn live_openai_graph_runtime_smoke() {
    if env::var("RUSTRAG_OPENAI_API_KEY").unwrap_or_default().trim().is_empty() {
        eprintln!("RUSTRAG_OPENAI_API_KEY is not set; skipping ignored live OpenAI test");
        return;
    }

    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("backend repo root");
    let output_dir = repo_root.join("docs/checkpoints/runtime-smoke").join("openai-live-test");
    let status = Command::new("bash")
        .arg(repo_root.join("scripts/smoke/runtime-openai.sh"))
        .arg(&output_dir)
        .current_dir(repo_root)
        .status()
        .expect("run runtime-openai.sh");

    assert!(status.success(), "runtime-openai.sh exited with {status}");
}
