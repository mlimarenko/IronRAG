use super::{
    XbergExtractionError as Error, XbergSource,
    output::{response_failure, validate_response},
};
use crate::{
    integrations::extraction_protocol::{
        Extraction, Limits, MAX_OUTPUT, MAX_TEXT, MAX_UNITS, Operation, Request, Response,
    },
    shared::admission::{PermitWaitError, acquire_permit_within},
};
use sha2::{Digest, Sha256};
use std::{
    process::{ExitStatus, Stdio},
    sync::{Arc, LazyLock},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::{Semaphore, oneshot},
};

static NATIVE_SLOT: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(1)));
const STDERR_LIMIT: usize = 4096;
// Admission follows bounded source, output and raster buffers, not model weights.
const RASTER_WORKING_BYTES: u64 = crate::integrations::extraction_protocol::MAX_PIXELS * 8;
const MEMORY_MARGIN_MIB: u64 = 64;

struct Settings {
    binary: String,
    timeout: u64,
    acquire: u64,
}

fn positive_setting(name: &str, default: u64, max: u64) -> Result<u64, Error> {
    match std::env::var(name) {
        Ok(raw) => raw
            .parse::<u64>()
            .ok()
            .filter(|n| *n > 0 && *n <= max)
            .ok_or_else(|| Error::Configuration(format!("{name} must be between 1 and {max}"))),
        Err(std::env::VarError::NotPresent) => Ok(default),
        Err(_) => Err(Error::Configuration(format!("{name} must be Unicode"))),
    }
}

impl Settings {
    fn load() -> Result<Self, Error> {
        // Startup rejects retired extraction keys before settings are loaded.
        let binary = match std::env::var("IRONRAG_XBERG_EXTRACT_BIN") {
            Ok(value) if !value.trim().is_empty() => value,
            Err(std::env::VarError::NotPresent) => "ironrag-extract".into(),
            _ => {
                return Err(Error::Configuration(
                    "IRONRAG_XBERG_EXTRACT_BIN must be a nonempty Unicode path".into(),
                ));
            }
        };
        Ok(Self {
            binary,
            timeout: positive_setting("IRONRAG_XBERG_TIMEOUT_SECS", 300, 300)?,
            acquire: positive_setting("IRONRAG_XBERG_ACQUIRE_TIMEOUT_SECS", 600, 3600)?,
        })
    }
}

pub(super) fn page_batch_size() -> u32 {
    // Invalid values are rejected by invoke before a child can start.
    positive_setting("IRONRAG_XBERG_PAGE_BATCH_SIZE", 10, 64).unwrap_or(10) as u32
}

struct Admission {
    settings: Settings,
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
}

pub(super) async fn invoke(
    source: XbergSource<'_>,
    bytes: Vec<u8>,
    operation: Operation,
    range: Option<(u32, u32)>,
) -> Result<(Extraction, i64), Error> {
    if bytes.len() > crate::integrations::extraction_protocol::MAX_INPUT {
        return Err(Error::Contract("input byte budget exceeded".into()));
    }
    let settings = Settings::load()?;
    positive_setting("IRONRAG_XBERG_PAGE_BATCH_SIZE", 10, 64)?;
    let permit = acquire_permit_within(
        Arc::clone(&NATIVE_SLOT),
        Duration::from_secs(settings.acquire),
        "xberg_native",
    )
    .await
    .map_err(|error| match error {
        PermitWaitError::AcquireTimeout { timeout_ms, .. } => Error::AcquireTimeout { timeout_ms },
        PermitWaitError::LimiterClosed { .. } => Error::LimiterClosed,
    })?;
    ensure_native_memory(bytes.len(), &operation)?;
    invoke_admitted(source, bytes, operation, range, Admission { settings, permit: Some(permit) })
        .await
}

async fn invoke_admitted(
    source: XbergSource<'_>,
    bytes: Vec<u8>,
    operation: Operation,
    range: Option<(u32, u32)>,
    admission: Admission,
) -> Result<(Extraction, i64), Error> {
    let Admission { settings, permit } = admission;
    // /var/tmp is disk-backed in the runtime image; never stage original input
    // in /dev/shm or another memory-backed extraction workspace.
    let scratch = tempfile::Builder::new()
        .prefix("ironrag-xberg-")
        .tempdir_in("/var/tmp")
        .map_err(Error::TempDir)?;
    let extension = source.source_format;
    if !extension.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        || extension.len() > 32
    {
        return Err(Error::Contract("invalid source format".into()));
    }
    let path = scratch.path().join(format!("source.{extension}"));
    std::fs::write(&path, &bytes).map_err(Error::WriteInput)?;
    let digest = hex::encode(Sha256::digest(&bytes));
    let input_bytes =
        u64::try_from(bytes.len()).map_err(|_| Error::Contract("input length overflow".into()))?;
    let request = Request {
        path,
        mime: resolved_mime(source).to_owned(),
        source_sha256: digest,
        limits: Limits {
            input_bytes,
            output_bytes: MAX_OUTPUT,
            text_bytes: MAX_TEXT,
            units: MAX_UNITS,
        },
        operation,
        start: range.map(|r| r.0),
        end: range.map(|r| r.1),
    };
    request.validate().map_err(|error| Error::Contract(error.to_string()))?;
    let request_bytes = serde_json::to_vec(&request).map_err(Error::InvalidJson)?;
    let (mut sender, receiver) = oneshot::channel();
    // The supervisor, not the caller future, owns the scratch and permit. A
    // dropped/cancelled caller closes the receiver; cleanup still awaits reap.
    tokio::spawn(async move {
        let mut command = Command::new(&settings.binary);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        let result = match command.spawn() {
            Err(error) => Err(Error::Spawn(error)),
            Ok(mut child) => {
                let pid = child.id();
                let io = async {
                    let mut stdin = child
                        .stdin
                        .take()
                        .ok_or_else(|| Error::Supervisor("missing stdin".into()))?;
                    let stdout = child
                        .stdout
                        .take()
                        .ok_or_else(|| Error::Supervisor("missing stdout".into()))?;
                    let stderr = child
                        .stderr
                        .take()
                        .ok_or_else(|| Error::Supervisor("missing stderr".into()))?;
                    let write = async {
                        stdin.write_all(&request_bytes).await.map_err(Error::Spawn)?;
                        stdin.shutdown().await.map_err(Error::Spawn)?;
                        drop(stdin);
                        Ok::<_, Error>(())
                    };
                    let (_, stdout, stderr) = tokio::try_join!(
                        write,
                        read_bounded(stdout, MAX_OUTPUT),
                        read_bounded(stderr, STDERR_LIMIT)
                    )?;
                    let status = child.wait().await.map_err(Error::Spawn)?;
                    if !status.success() {
                        if !process_status_is_memory_kill(&status) && !stdout.is_empty() {
                            let response: Response =
                                serde_json::from_slice(&stdout).map_err(Error::InvalidJson)?;
                            return Err(response_failure(response));
                        }
                        return Err(Error::ProcessFailed {
                            status,
                            stderr: String::from_utf8_lossy(&stderr).into_owned(),
                        });
                    }
                    let response: Response =
                        serde_json::from_slice(&stdout).map_err(Error::InvalidJson)?;
                    let elapsed = i64::try_from(response.total_milliseconds).unwrap_or(i64::MAX);
                    let output = validate_response(response, &request)?;
                    Ok((output, elapsed))
                };
                let result = tokio::select! {
                    biased;
                    () = sender.closed() => Err(Error::Supervisor("caller cancelled".into())),
                    result = tokio::time::timeout(Duration::from_secs(settings.timeout), io) => result.unwrap_or(Err(Error::Timeout(settings.timeout))),
                };
                // Kill the entire native process group, including descendants
                // that inherited pipes. Check reap before releasing resources.
                #[cfg(unix)]
                let group_stopped = if let Some(pid) = pid {
                    match Command::new("/bin/kill")
                        .args(["-KILL", "--", &format!("-{pid}")])
                        .env("LC_ALL", "C")
                        .output()
                        .await
                    {
                        Ok(output) => {
                            output.status.success()
                                || String::from_utf8_lossy(&output.stderr)
                                    .contains("No such process")
                        }
                        Err(_) => false,
                    }
                } else {
                    true
                };
                #[cfg(not(unix))]
                let group_stopped = false;
                if !group_stopped {
                    // A direct-child kill cannot establish descendant exit.
                    // Still request it, but retain resources on this failure.
                    if let Err(error) = child.start_kill() {
                        tracing::error!(%error, "Xberg direct-child termination failed");
                    }
                }
                let reap = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;
                if !group_stopped || !matches!(reap, Ok(Ok(_))) {
                    // Fail closed: the OS has not confirmed exit. Retain both
                    // resources rather than deleting input under a live child.
                    std::mem::forget(scratch);
                    std::mem::forget(permit);
                    let _ = sender.send(Err(Error::Supervisor("process-group termination or checked reap failed/exceeded 10s; workspace and slot retained".into())));
                    return;
                }
                result
            }
        };
        let result = match scratch.close() {
            Ok(()) => result,
            Err(error) => Err(Error::TempDir(error)),
        };
        drop(permit);
        let _ = sender.send(result);
    });
    receiver.await.map_err(|error| Error::Supervisor(error.to_string()))?
}

fn resolved_mime<'a>(source: XbergSource<'a>) -> &'a str {
    match source.source_format {
        "pdf" => "application/pdf",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "tif" | "tiff" => "image/tiff",
        "bmp" => "image/bmp",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => source.mime_type.unwrap_or("application/octet-stream"),
    }
}

async fn read_bounded(mut reader: impl AsyncRead + Unpin, limit: usize) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    (&mut reader).take(limit as u64 + 1).read_to_end(&mut bytes).await.map_err(Error::Spawn)?;
    if bytes.len() > limit {
        return Err(Error::Contract("child output budget exceeded".into()));
    }
    Ok(bytes)
}

fn read_u64(path: &str) -> Option<u64> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn ensure_native_memory(input_bytes: usize, operation: &Operation) -> Result<(), Error> {
    let host = std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|text| {
            text.lines().find_map(|line| {
                line.strip_prefix("MemAvailable:")?.split_whitespace().next()?.parse::<u64>().ok()
            })
        })
        .map(|kb| kb / 1024);
    let cgroup = read_u64("/sys/fs/cgroup/memory.max")
        .zip(read_u64("/sys/fs/cgroup/memory.current"))
        .or_else(|| {
            read_u64("/sys/fs/cgroup/memory/memory.limit_in_bytes")
                .zip(read_u64("/sys/fs/cgroup/memory/memory.usage_in_bytes"))
        });
    let (limit, used) = cgroup
        .map(|(limit, used)| (limit / 1048576, used / 1048576))
        .unwrap_or((host.unwrap_or_default(), 0));
    let available = limit.saturating_sub(used).min(host.unwrap_or_default());
    let working_bytes = input_bytes as u64 * 2
        + if *operation == Operation::Extract {
            RASTER_WORKING_BYTES + MAX_OUTPUT as u64 * 2
        } else {
            0
        };
    memory_headroom(limit, used, available, working_bytes.div_ceil(1048576))
}

pub(super) fn memory_headroom(
    limit: u64,
    used: u64,
    available: u64,
    required_mib: u64,
) -> Result<(), Error> {
    if available < required_mib + MEMORY_MARGIN_MIB {
        return Err(Error::InsufficientMemory {
            memory_limit_mib: limit,
            current_rss_mib: used,
            available_mib: available,
            required_mib,
            safety_margin_mib: MEMORY_MARGIN_MIB,
        });
    }
    Ok(())
}

pub(super) fn process_status_is_memory_kill(status: &ExitStatus) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status.signal() == Some(9)
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        false
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn bounded_reader_refuses_a_complete_oversized_stream() {
        let bytes = std::io::Cursor::new(vec![b'x'; 33]);
        assert!(matches!(read_bounded(bytes, 32).await, Err(Error::Contract(_))));
    }

    #[tokio::test]
    async fn cancellation_and_timeout_reap_native_child_before_scratch_and_permit_release() {
        for cancel in [true, false] {
            let test_dir = tempfile::tempdir().unwrap();
            let script = test_dir.path().join("native-fixture");
            let request_file = test_dir.path().join("request.json");
            let pid_file = test_dir.path().join("pid");
            std::fs::write(
                &script,
                format!(
                    "#!/bin/sh\ncat > '{}'\nprintf '%s' \"$$\" > '{}'\nexec /bin/sleep 30\n",
                    request_file.display(),
                    pid_file.display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
            let semaphore = Arc::new(Semaphore::new(1));
            let permit = Arc::clone(&semaphore).acquire_owned().await.unwrap();
            let task = tokio::spawn(async move {
                invoke_admitted(
                    XbergSource {
                        file_name: None,
                        mime_type: Some("application/pdf"),
                        source_format: "pdf",
                    },
                    b"source bytes".to_vec(),
                    Operation::Extract,
                    None,
                    Admission {
                        settings: Settings {
                            binary: script.to_string_lossy().into_owned(),
                            timeout: 1,
                            acquire: 1,
                        },
                        permit: Some(permit),
                    },
                )
                .await
            });
            tokio::time::timeout(Duration::from_secs(5), async {
                while !pid_file.exists() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("native child must start");
            let request: Request =
                serde_json::from_slice(&std::fs::read(&request_file).unwrap()).unwrap();
            let pid = std::fs::read_to_string(&pid_file).unwrap();
            assert!(request.path.exists());
            if cancel {
                task.abort();
                assert!(task.await.unwrap_err().is_cancelled());
            } else {
                assert!(matches!(task.await.unwrap(), Err(Error::Timeout(1))));
            }
            let _released = tokio::time::timeout(Duration::from_secs(5), semaphore.acquire())
                .await
                .expect("supervisor must reap and release slot")
                .unwrap();
            assert!(!request.path.exists(), "scratch cleanup precedes slot release");
            assert!(
                !std::path::Path::new(&format!("/proc/{pid}")).exists(),
                "checked reap precedes cleanup"
            );
        }
    }

    #[test]
    fn native_admission_uses_bounded_working_set_not_model_reservation() {
        assert!(memory_headroom(1024, 256, 768, 256).is_ok());
        assert!(memory_headroom(256, 200, 56, 32).is_err());
    }
}
