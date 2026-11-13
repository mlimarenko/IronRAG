use std::{
    io::{self, SeekFrom, Write},
    pin::Pin,
    task::{Context as TaskContext, Poll},
    time::Duration,
};

use anyhow::{Context, bail};
use async_compression::{
    Level,
    tokio::{bufread::ZstdDecoder, write::ZstdEncoder},
};
use futures::{StreamExt, stream};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufReader, ReadBuf};
use tokio_util::{io::ReaderStream, sync::CancellationToken};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    services::knowledge::preparation::{
        self, Claim, GraphTopologyIdentity, PreparationRecord, PreparationStatus,
    },
};

use super::{load_compact_topology, render::render_ndjson_into};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ArtifactManifest {
    pub storage_key: String,
    pub compressed_bytes: u64,
    pub decoded_bytes: u64,
    pub sha256: String,
    pub node_count: u64,
    pub edge_count: u64,
    pub document_count: u64,
}

pub(crate) fn artifact_key(
    identity: &GraphTopologyIdentity,
    preparation_id: Uuid,
    attempt_id: Uuid,
) -> String {
    format!(
        "graph-topology/{}/{}/{preparation_id}/{attempt_id}.ndjson.zst",
        identity.workspace_id, identity.library_id
    )
}

struct CancelWriter<W> {
    inner: W,
    cancellation: CancellationToken,
    claim: Claim,
}

impl<W: Write> Write for CancelWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.cancellation.is_cancelled() || !self.claim.output_is_permitted() {
            return Err(io::Error::other("topology render cancelled or lease expired"));
        }
        self.inner.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.cancellation.is_cancelled() || !self.claim.output_is_permitted() {
            return Err(io::Error::other("topology render cancelled or lease expired"));
        }
        self.inner.flush()
    }
}

pub(crate) async fn build_artifact(
    state: &AppState,
    claim: &Claim,
    cancellation: CancellationToken,
) -> anyhow::Result<ArtifactManifest> {
    let identity = claim.record.identity;
    let topology = tokio::select! {
        () = cancellation.cancelled() => bail!("artifact build cancelled"),
        result = load_compact_topology(state, &identity) => result?,
    };
    if cancellation.is_cancelled() || !claim.output_is_permitted() {
        bail!("artifact build cancelled or lease expired");
    }
    let node_count = (topology.entities.len() + topology.documents.len()) as u64;
    let edge_count = (topology.edges.len() + topology.document_links.len()) as u64;
    let document_count = topology.documents.len() as u64;
    let preparation_id = claim.record.id;
    let render_cancellation = cancellation.clone();
    let render_claim = claim.clone();
    let renderer = tokio::task::spawn_blocking(move || -> anyhow::Result<std::fs::File> {
        let file = tempfile::tempfile().context("create topology render spool")?;
        let mut writer =
            CancelWriter { inner: file, cancellation: render_cancellation, claim: render_claim };
        render_ndjson_into(&mut writer, &topology, &identity, preparation_id)?;
        Ok(writer.inner)
    });
    let file = join_renderer(renderer, &cancellation).await?;
    if cancellation.is_cancelled() || !claim.output_is_permitted() {
        bail!("artifact build cancelled or lease expired");
    }
    let decoded_bytes = file.metadata()?.len();
    let (mut compressed, compressed_bytes, sha256) =
        Box::pin(compress_and_hash(file, claim, &cancellation)).await?;
    let storage_key = artifact_key(&claim.record.identity, preparation_id, claim.attempt_id);
    if !preparation::set_phase(state, claim, PreparationStatus::Publishing).await? {
        bail!("artifact attempt no longer owns its lease");
    }
    if cancellation.is_cancelled() || !claim.output_is_permitted() {
        bail!("artifact upload cancelled or lease expired");
    }
    upload_artifact(
        state,
        &identity,
        &storage_key,
        &mut compressed,
        compressed_bytes,
        &cancellation,
    )
    .await?;
    Ok(ArtifactManifest {
        storage_key,
        compressed_bytes,
        decoded_bytes,
        sha256,
        node_count,
        edge_count,
        document_count,
    })
}

async fn join_renderer(
    mut renderer: tokio::task::JoinHandle<anyhow::Result<std::fs::File>>,
    cancellation: &CancellationToken,
) -> anyhow::Result<std::fs::File> {
    tokio::select! {
        result = &mut renderer => Ok(result??),
        () = cancellation.cancelled() => {
            // The lease must not be released until the blocking renderer stops.
            if let Err(error) = renderer.await {
                tracing::warn!(%error, "cancelled topology renderer failed to join");
            }
            bail!("artifact build cancelled");
        }
    }
}

async fn upload_artifact(
    state: &AppState,
    identity: &GraphTopologyIdentity,
    storage_key: &str,
    compressed: &mut tokio::fs::File,
    compressed_bytes: u64,
    cancellation: &CancellationToken,
) -> anyhow::Result<()> {
    tokio::select! {
        () = cancellation.cancelled() => bail!("artifact build cancelled"),
        result = tokio::time::timeout(Duration::from_secs(300), state.content_storage.write_graph_topology_artifact(identity.workspace_id, identity.library_id, storage_key, compressed, compressed_bytes)) => { result.context("artifact upload timed out")??; }
    }
    Ok(())
}

async fn compress_and_hash(
    file: std::fs::File,
    claim: &Claim,
    cancellation: &CancellationToken,
) -> anyhow::Result<(tokio::fs::File, u64, String)> {
    let mut input = tokio::fs::File::from_std(file);
    input.seek(SeekFrom::Start(0)).await?;
    let compressed = tempfile::tempfile().context("create topology compressed spool")?;
    let compressed = tokio::fs::File::from_std(compressed);
    let mut encoder = ZstdEncoder::with_quality(compressed, Level::Precise(1));
    tokio::select! {
        () = cancellation.cancelled() => bail!("artifact build cancelled"),
        result = tokio::io::copy(&mut input, &mut encoder) => { result?; }
    }
    tokio::select! {
        () = cancellation.cancelled() => bail!("artifact build cancelled"),
        result = encoder.shutdown() => result?,
    }
    let mut compressed = encoder.into_inner();
    let compressed_bytes = compressed.metadata().await?.len();
    compressed.seek(SeekFrom::Start(0)).await?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 65536];
    loop {
        if !claim.output_is_permitted() {
            bail!("artifact build lease expired");
        }
        let n = tokio::select! {
            () = cancellation.cancelled() => bail!("artifact build cancelled"),
            result = compressed.read(&mut buffer) => result?,
        };
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    compressed.seek(SeekFrom::Start(0)).await?;
    Ok((compressed, compressed_bytes, hex::encode(hasher.finalize())))
}

struct HashReader<R> {
    inner: R,
    hash: Sha256,
    length: u64,
}

impl<R: AsyncRead + Unpin> AsyncRead for HashReader<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        match Pin::new(&mut self.inner).poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {
                let bytes = &buf.filled()[before..];
                self.hash.update(bytes);
                self.length += bytes.len() as u64;
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}

pub(crate) async fn result_body(
    state: AppState,
    record: PreparationRecord,
) -> anyhow::Result<axum::body::Body> {
    let Some(manifest) = record.artifact.clone() else {
        report_result_failure(&state, &record, "artifact_missing_receipt").await;
        bail!("missing artifact receipt");
    };
    let reader = match tokio::time::timeout(
        Duration::from_secs(30),
        state.content_storage.read_graph_topology_artifact(
            record.identity.workspace_id,
            record.identity.library_id,
            &manifest.storage_key,
        ),
    )
    .await
    {
        Ok(Ok(reader)) => reader,
        _ => {
            report_result_failure(&state, &record, "artifact_read_failed").await;
            bail!("artifact read failed or timed out");
        }
    };
    let (writer, reader_body) = tokio::io::duplex(65536);
    Ok(body_from_producer(reader_body, async move {
        let result = Box::pin(stream_checked(
            record.identity,
            record.id,
            &manifest,
            reader,
            writer,
            || async {
                #[cfg(feature = "test-support")]
                state
                    .runtime_graph_projection_cache
                    .pause_selected_graph_read(record.identity.library_id)
                    .await;
                Ok(preparation::identity_is_current(&state.persistence.postgres, &record.identity)
                    .await?)
            },
        ))
        .await;
        if let Err(error) = &result {
            // A disconnected client says nothing about the stored artifact.
            if !error
                .downcast_ref::<io::Error>()
                .is_some_and(|e| e.kind() == io::ErrorKind::BrokenPipe)
            {
                report_result_failure(&state, &record, "artifact_integrity_failed").await;
            }
        }
        result.map_err(io::Error::other)
    }))
}

fn body_from_producer<R, F>(reader: R, producer: F) -> axum::body::Body
where
    R: AsyncRead + Send + Unpin + 'static,
    F: std::future::Future<Output = io::Result<()>> + Send + 'static,
{
    let (sender, receiver) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        // A dropped receiver means the HTTP client stopped consuming the body.
        if sender.send(producer.await).is_err() {
            tracing::debug!("topology result consumer disconnected");
        }
    });
    let completion = stream::once(async move {
        match receiver.await {
            Ok(Ok(())) => Ok(axum::body::Bytes::new()),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(io::Error::other("topology result producer terminated")),
        }
    });
    axum::body::Body::from_stream(ReaderStream::new(reader).chain(completion))
}

async fn report_result_failure(state: &AppState, record: &PreparationRecord, code: &str) {
    if let Err(error) = preparation::fail_result(state, &record.identity, record.id, code).await {
        tracing::warn!(%error, "failed to record topology artifact failure");
    }
}

async fn stream_checked<R, W, F, Fut>(
    identity: GraphTopologyIdentity,
    preparation_id: Uuid,
    manifest: &ArtifactManifest,
    reader: R,
    mut writer: W,
    current: F,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<bool>>,
{
    let reader = HashReader { inner: reader, hash: Sha256::new(), length: 0 };
    let mut decoder = ZstdDecoder::new(BufReader::new(reader));
    decoder.multiple_members(true);
    let mut buffer = vec![0_u8; 65536];
    let mut tail = Vec::with_capacity(65536 + 8192);
    let mut meta = Vec::new();
    let mut meta_complete = false;
    let mut decoded = 0_u64;
    loop {
        let n = tokio::time::timeout(Duration::from_secs(30), decoder.read(&mut buffer))
            .await
            .context("artifact read timed out")??;
        if n == 0 {
            break;
        }
        decoded = decoded.checked_add(n as u64).context("decoded length overflow")?;
        if decoded > manifest.decoded_bytes {
            bail!("decoded artifact exceeds receipt");
        }
        if !meta_complete {
            for &byte in &buffer[..n] {
                if byte == b'\n' {
                    meta_complete = true;
                    break;
                }
                if meta.len() >= 16384 {
                    bail!("artifact meta too large");
                }
                meta.push(byte);
            }
            if meta_complete {
                let value: serde_json::Value = serde_json::from_slice(&meta)?;
                if value["s"] != "meta"
                    || value["v"] != 1
                    || value["library_id"] != identity.library_id.to_string()
                    || value["preparation_id"] != preparation_id.to_string()
                    || value["projection_version"] != identity.projection_version
                    || value["topology_generation"] != identity.topology_generation
                    || value["source_truth_version"] != identity.source_truth_version
                    || value["node_count"] != manifest.node_count
                    || value["edge_count"] != manifest.edge_count
                    || value["document_count"] != manifest.document_count
                {
                    bail!("artifact meta does not match receipt");
                }
            }
        }
        tail.extend_from_slice(&buffer[..n]);
        if meta_complete && tail.len() > 8192 {
            let release = tail.len() - 8192;
            writer.write_all(&tail[..release]).await?;
            tail.drain(..release);
        }
    }
    let reader = decoder.into_inner().into_inner();
    if !meta_complete
        || decoded != manifest.decoded_bytes
        || reader.length != manifest.compressed_bytes
        || hex::encode(reader.hash.finalize()) != manifest.sha256
    {
        bail!("artifact length or digest mismatch");
    }
    if tail.last() != Some(&b'\n') {
        bail!("artifact terminal newline missing");
    }
    let end_start =
        tail[..tail.len() - 1].iter().rposition(|&byte| byte == b'\n').map_or(0, |pos| pos + 1);
    let end: serde_json::Value = serde_json::from_slice(&tail[end_start..tail.len() - 1])?;
    if end["s"] != "end"
        || end["preparation_id"] != preparation_id.to_string()
        || end["node_count"] != manifest.node_count
        || end["edge_count"] != manifest.edge_count
        || end["document_count"] != manifest.document_count
    {
        bail!("artifact terminal receipt mismatch");
    }
    writer.write_all(&tail[..end_start]).await?;
    if !current().await? {
        bail!("artifact identity is no longer current");
    }
    writer.write_all(&tail[end_start..]).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn fixture(
        extra_bytes: usize,
        terminal: bool,
    ) -> (GraphTopologyIdentity, Uuid, ArtifactManifest, Vec<u8>) {
        let identity = GraphTopologyIdentity {
            workspace_id: Uuid::from_u128(1),
            library_id: Uuid::from_u128(2),
            projection_version: 1,
            topology_generation: 0,
            source_truth_version: 3,
        };
        let preparation_id = Uuid::from_u128(4);
        let meta = format!(
            "{{\"s\":\"meta\",\"v\":1,\"library_id\":\"{}\",\"preparation_id\":\"{}\",\"projection_version\":1,\"topology_generation\":0,\"source_truth_version\":3,\"generated_at\":\"2026-01-02T03:04:05Z\",\"node_count\":0,\"edge_count\":0,\"document_count\":0}}\n",
            identity.library_id, preparation_id
        );
        let end = format!(
            "{{\"s\":\"end\",\"preparation_id\":\"{preparation_id}\",\"node_count\":0,\"edge_count\":0,\"document_count\":0}}\n"
        );
        let id_map = b"{\"s\":\"id_map\",\"m\":{}}\n";
        let file = tokio::fs::File::from_std(tempfile::tempfile().unwrap());
        let mut encoder = ZstdEncoder::with_quality(file, Level::Precise(1));
        encoder.write_all(meta.as_bytes()).await.unwrap();
        encoder.write_all(id_map).await.unwrap();
        let line = b"{\"s\":\"nodes\",\"d\":[]}\n";
        let count = extra_bytes.div_ceil(line.len());
        let batch = line.repeat(1024);
        for _ in 0..(count / 1024) {
            encoder.write_all(&batch).await.unwrap();
        }
        for _ in 0..(count % 1024) {
            encoder.write_all(line).await.unwrap();
        }
        if terminal {
            encoder.write_all(end.as_bytes()).await.unwrap();
        }
        encoder.shutdown().await.unwrap();
        let mut file = encoder.into_inner();
        file.seek(SeekFrom::Start(0)).await.unwrap();
        let mut compressed = Vec::new();
        file.read_to_end(&mut compressed).await.unwrap();
        let manifest = ArtifactManifest {
            storage_key: String::new(),
            compressed_bytes: compressed.len() as u64,
            decoded_bytes: (meta.len()
                + id_map.len()
                + count * line.len()
                + if terminal { end.len() } else { 0 }) as u64,
            sha256: hex::encode(Sha256::digest(&compressed)),
            node_count: 0,
            edge_count: 0,
            document_count: 0,
        };
        (identity, preparation_id, manifest, compressed)
    }

    #[tokio::test]
    async fn verifies_integrity_end_and_currentness_before_terminal() {
        struct FailedRead;
        impl AsyncRead for FailedRead {
            fn poll_read(
                self: Pin<&mut Self>,
                _: &mut TaskContext<'_>,
                _: &mut ReadBuf<'_>,
            ) -> Poll<io::Result<()>> {
                Poll::Ready(Err(io::Error::other("injected read failure")))
            }
        }
        let (identity, id, manifest, compressed) = fixture(0, true).await;
        assert!(
            stream_checked(identity, id, &manifest, FailedRead, tokio::io::sink(), || async {
                Ok(true)
            })
            .await
            .is_err()
        );
        let output = tokio::fs::File::from_std(tempfile::tempfile().unwrap());
        assert!(
            stream_checked(identity, id, &manifest, &compressed[..], output, || async { Ok(true) })
                .await
                .is_ok()
        );
        let mut wrong = manifest.clone();
        wrong.sha256 = "00".to_string();
        let output = tokio::fs::File::from_std(tempfile::tempfile().unwrap());
        assert!(
            stream_checked(identity, id, &wrong, &compressed[..], output, || async { Ok(true) })
                .await
                .is_err()
        );
        let output = tokio::fs::File::from_std(tempfile::tempfile().unwrap());
        assert!(
            stream_checked(
                identity,
                id,
                &manifest,
                &compressed[..compressed.len() - 1],
                output,
                || async { Ok(true) }
            )
            .await
            .is_err()
        );
        let output = tokio::fs::File::from_std(tempfile::tempfile().unwrap());
        assert!(
            stream_checked(identity, id, &manifest, &compressed[..], output, || async {
                Ok(false)
            })
            .await
            .is_err()
        );
        let (writer, receiver) = tokio::io::duplex(1024);
        drop(receiver);
        assert!(
            stream_checked(identity, id, &manifest, &compressed[..], writer, || async { Ok(true) })
                .await
                .is_err()
        );
        let (identity, id, manifest, compressed) = fixture(0, false).await;
        let output = tokio::fs::File::from_std(tempfile::tempfile().unwrap());
        assert!(
            stream_checked(identity, id, &manifest, &compressed[..], output, || async { Ok(true) })
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn body_errors_for_forged_early_end_and_stale_identity() {
        let (identity, id, mut manifest, compressed) = fixture(0, true).await;
        let mut decoder = ZstdDecoder::new(BufReader::new(&compressed[..]));
        let mut forged = Vec::new();
        decoder.read_to_end(&mut forged).await.unwrap();
        forged.extend_from_slice(&vec![b'x'; 16384]);
        let mut encoder =
            ZstdEncoder::new(tokio::fs::File::from_std(tempfile::tempfile().unwrap()));
        encoder.write_all(&forged).await.unwrap();
        encoder.shutdown().await.unwrap();
        let mut file = encoder.into_inner();
        file.seek(SeekFrom::Start(0)).await.unwrap();
        let mut bad = Vec::new();
        file.read_to_end(&mut bad).await.unwrap();
        manifest.decoded_bytes = forged.len() as u64;
        manifest.compressed_bytes = bad.len() as u64;
        manifest.sha256 = "wrong".into();
        let (writer, reader) = tokio::io::duplex(65536);
        let body = body_from_producer(reader, async move {
            stream_checked(identity, id, &manifest, &bad[..], writer, || async { Ok(true) })
                .await
                .map_err(io::Error::other)
        });
        let mut stream = body.into_data_stream();
        let mut received = Vec::new();
        let mut failed = false;
        while let Some(item) = stream.next().await {
            match item {
                Ok(bytes) => received.extend_from_slice(&bytes),
                Err(_) => {
                    failed = true;
                    break;
                }
            }
        }
        assert!(String::from_utf8(received).unwrap().contains("\"s\":\"end\""));
        assert!(failed, "forged end must not be followed by clean EOF");

        let (identity, id, manifest, compressed) = fixture(0, true).await;
        let (writer, reader) = tokio::io::duplex(65536);
        let body = body_from_producer(reader, async move {
            stream_checked(identity, id, &manifest, &compressed[..], writer, || async { Ok(false) })
                .await
                .map_err(io::Error::other)
        });
        assert!(axum::body::to_bytes(body, usize::MAX).await.is_err());
    }

    #[tokio::test]
    async fn body_errors_if_producer_panics() {
        async fn fail() -> io::Result<()> {
            panic!("injected producer panic")
        }
        let (writer, reader) = tokio::io::duplex(64);
        let body = body_from_producer(reader, async move {
            drop(writer);
            fail().await
        });
        assert!(axum::body::to_bytes(body, usize::MAX).await.is_err());
    }

    #[tokio::test]
    async fn streams_more_than_128_mib_decoded() {
        let identity = GraphTopologyIdentity {
            workspace_id: Uuid::from_u128(1),
            library_id: Uuid::from_u128(2),
            projection_version: 1,
            topology_generation: 1,
            source_truth_version: 3,
        };
        let id = Uuid::from_u128(4);
        let count = 500_000_u32;
        let mut encoder = ZstdEncoder::with_quality(
            tokio::fs::File::from_std(tempfile::tempfile().unwrap()),
            Level::Precise(1),
        );
        let mut decoded = 0_u64;
        async fn frame(
            encoder: &mut ZstdEncoder<tokio::fs::File>,
            decoded: &mut u64,
            value: serde_json::Value,
        ) {
            let mut line = serde_json::to_vec(&value).unwrap();
            line.push(b'\n');
            *decoded += line.len() as u64;
            encoder.write_all(&line).await.unwrap();
        }
        frame(&mut encoder, &mut decoded, serde_json::json!({"s":"meta", "v":1, "library_id":identity.library_id, "preparation_id":id, "projection_version":1, "topology_generation":1, "source_truth_version":3, "generated_at":"2026-01-02T03:04:05Z", "node_count":count, "edge_count":0, "document_count":0})).await;
        for start in (1..=count).step_by(512) {
            let end = (start + 512).min(count + 1);
            let map: serde_json::Map<String, serde_json::Value> = (start..end)
                .map(|n| (Uuid::from_u128(n as u128).to_string(), serde_json::json!(n)))
                .collect();
            frame(&mut encoder, &mut decoded, serde_json::json!({"s":"id_map", "m":map})).await;
        }
        let label = "meaningful topology entity description ".repeat(8);
        for start in (1..=count).step_by(512) {
            let end = (start + 512).min(count + 1);
            let nodes: Vec<_> = (start..end).map(|n| serde_json::json!({"i":n, "l":label, "k":format!("entity-{n}"), "t":"entity"})).collect();
            frame(&mut encoder, &mut decoded, serde_json::json!({"s":"nodes", "d":nodes})).await;
        }
        frame(&mut encoder, &mut decoded, serde_json::json!({"s":"end", "preparation_id":id, "node_count":count, "edge_count":0, "document_count":0})).await;
        encoder.shutdown().await.unwrap();
        let mut file = encoder.into_inner();
        let compressed_bytes = file.metadata().await.unwrap().len();
        file.seek(SeekFrom::Start(0)).await.unwrap();
        let mut hash = Sha256::new();
        let mut buf = [0_u8; 65536];
        loop {
            let n = file.read(&mut buf).await.unwrap();
            if n == 0 {
                break;
            }
            hash.update(&buf[..n]);
        }
        file.seek(SeekFrom::Start(0)).await.unwrap();
        let manifest = ArtifactManifest {
            storage_key: String::new(),
            compressed_bytes,
            decoded_bytes: decoded,
            sha256: hex::encode(hash.finalize()),
            node_count: u64::from(count),
            edge_count: 0,
            document_count: 0,
        };
        assert!(decoded > 128 * 1024 * 1024);
        stream_checked(identity, id, &manifest, file, tokio::io::sink(), || async { Ok(true) })
            .await
            .unwrap();
    }
}
