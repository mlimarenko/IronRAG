//! Isolated model-free byte-input extraction process.
mod general;
mod input;
mod runner;

#[cfg(test)]
mod general_tests;

use clap::Parser;
use ironrag_backend::integrations::extraction_protocol::{MAX_OUTPUT, Request, Response};
use std::io::{Read, Write};

/// The child accepts only its bounded stdin protocol, never model arguments.
#[derive(Parser)]
struct Arguments {}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> std::process::ExitCode {
    Arguments::parse();
    let started = std::time::Instant::now();
    let request = read_request();
    let output_limit = request.as_ref().map_or(MAX_OUTPUT, |request| request.limits.output_bytes);
    let result = match request {
        Ok(request) => runner::extract_native(request).await,
        Err(error) => Err(error),
    };
    let failed = result.is_err();
    let response = Response::new(result, started.elapsed(), None);
    match serde_json::to_vec(&response) {
        Ok(bytes) if bytes.len() <= output_limit => {
            if std::io::stdout().lock().write_all(&bytes).is_err() {
                return std::process::ExitCode::FAILURE;
            }
        }
        _ => {
            let refusal = Response::new(
                Err(anyhow::anyhow!("output_limit")),
                started.elapsed(),
                Some("output_limit".into()),
            );
            if let Ok(bytes) = serde_json::to_vec(&refusal) {
                let _ = std::io::stdout().lock().write_all(&bytes);
            }
            return std::process::ExitCode::FAILURE;
        }
    }
    if failed { std::process::ExitCode::FAILURE } else { std::process::ExitCode::SUCCESS }
}

fn read_request() -> anyhow::Result<Request> {
    let mut bytes = Vec::new();
    std::io::stdin().take(65537).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= 65536, "request_limit");
    let request: Request = serde_json::from_slice(&bytes)?;
    request.validate()?;
    Ok(request)
}

#[cfg(test)]
mod protocol_tests {
    use super::*;
    use ironrag_backend::integrations::extraction_protocol::Operation;

    #[test]
    fn protocol_rejects_retired_recognition_fields() -> anyhow::Result<()> {
        let request = Request {
            path: "/data/document.pdf".into(),
            operation: Operation::Extract,
            mime: "application/pdf".into(),
            source_sha256: "0".repeat(64),
            limits: Default::default(),
            start: Some(1),
            end: Some(2),
        };
        let decoded: Request = serde_json::from_slice(&serde_json::to_vec(&request)?)?;
        decoded.validate()?;
        for name in ["ocr", "languages", "unknown"] {
            let mut unknown = serde_json::to_value(&request)?;
            unknown[name] = serde_json::json!(true);
            assert!(serde_json::from_value::<Request>(unknown).is_err());
        }
        assert!(
            Arguments::try_parse_from(["ironrag-extract", "--models-root", "/unused"]).is_err()
        );
        assert!(Arguments::try_parse_from(["ironrag-extract"]).is_ok());
        Ok(())
    }
}
