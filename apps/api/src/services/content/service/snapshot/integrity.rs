use super::*;

/// Exact digest metadata for an archive payload entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SnapshotEntryDigest {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

/// The complete archive inventory declared by the terminal summary.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SnapshotInventory {
    pub postgres_row_counts: BTreeMap<String, u64>,
    pub postgres_parts: Vec<SnapshotEntryDigest>,
    pub blobs: Vec<SnapshotEntryDigest>,
}

/// Counts from the prevalidation blob-byte check (IRG-32).
///
/// `verified` counts blobs whose bytes were proven against the digest that a
/// `content_revision.checksum` names; `unverifiable` counts blobs with no such
/// proof. The check proves the archive is internally consistent, never that it
/// is authentic: a coherent substitution of both the blob and its checksum
/// still passes and is simply counted as verified.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct SnapshotBlobVerification {
    pub verified: u64,
    pub unverifiable: u64,
}

/// Rejects an inventory that cannot exactly account for the manifest's data.
pub(super) fn validate_strict_snapshot_inventory(
    manifest: &SnapshotManifest,
    inventory: &SnapshotInventory,
) -> anyhow::Result<()> {
    let declared_tables: HashSet<&str> =
        manifest.postgres_tables.iter().map(String::as_str).collect();
    let counted_tables: HashSet<&str> =
        inventory.postgres_row_counts.keys().map(String::as_str).collect();
    anyhow::ensure!(
        declared_tables == counted_tables,
        "snapshot summary row counts must exactly match manifest postgres_tables"
    );
    for digest in inventory.postgres_parts.iter().chain(&inventory.blobs) {
        anyhow::ensure!(
            digest.size > 0,
            "snapshot inventory entry `{}` has zero size",
            digest.path
        );
        anyhow::ensure!(
            digest.sha256.len() == 64 && digest.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "snapshot inventory entry `{}` has invalid sha256",
            digest.path
        );
    }
    let part_tables: HashSet<&str> = inventory
        .postgres_parts
        .iter()
        .filter_map(|digest| digest.path.strip_prefix("postgres/"))
        .filter_map(|path| path.split_once('/').map(|(table, _)| table))
        .collect();
    anyhow::ensure!(
        part_tables.is_subset(&declared_tables),
        "snapshot inventory contains a postgres part for an undeclared table"
    );
    anyhow::ensure!(
        (!manifest.has_blobs && inventory.blobs.is_empty())
            || (manifest.has_blobs
                && inventory.blobs.iter().all(|entry| entry.path.starts_with("blobs/"))),
        "snapshot inventory blob declaration is inconsistent"
    );
    Ok(())
}

/// Ensures the final archive entry is its exact declared inventory.
pub(super) fn validate_terminal_snapshot_summary(
    expected: &SnapshotInventory,
    observed: &SnapshotInventory,
    is_terminal: bool,
) -> anyhow::Result<()> {
    anyhow::ensure!(is_terminal, "snapshot summary.json must be the terminal archive entry");
    anyhow::ensure!(
        expected == observed,
        "snapshot summary does not match observed archive inventory"
    );
    Ok(())
}

pub(super) fn snapshot_entry_digest(path: String, bytes: &[u8]) -> SnapshotEntryDigest {
    SnapshotEntryDigest {
        path,
        size: bytes.len() as u64,
        sha256: hex::encode(Sha256::digest(bytes)),
    }
}

/// What the `content_revision` rows folded for one encoded blob path say about
/// its bytes.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct BlobPathExpectation {
    /// Distinct digest segments (the prefix before the first `-` of the storage
    /// key's last `/`-separated component) across every row that encodes here.
    segments: BTreeSet<String>,
    /// Distinct checksums that parse as exactly `sha256:` + 64 lowercase hex.
    comparable: BTreeSet<String>,
}

impl BlobPathExpectation {
    /// The digest the byte contract proves for this blob, when it is proven:
    /// every row names the same storage-key segment, that segment is itself 64
    /// lowercase hex, and every comparable checksum names it too. A second row
    /// with a differing comparable checksum, or a row deriving a different
    /// segment, leaves the contract unproven rather than guessing.
    pub(super) fn proven_digest(&self) -> Option<&str> {
        if self.segments.len() != 1 {
            return None;
        }
        let segment = self.segments.iter().next()?;
        if !is_lowercase_sha256_hex(segment) {
            return None;
        }
        if self.comparable.is_empty() || !self.comparable.iter().all(|value| value == segment) {
            return None;
        }
        Some(segment)
    }
}

/// `true` for exactly 64 lowercase ASCII hex digits. That is the shape
/// `build_revision_storage_key` embeds in a storage key and
/// `content_revision.checksum` carries after its `sha256:` prefix.
pub(super) fn is_lowercase_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Parses `<64 lowercase hex>` out of `sha256:<hex>`, and nothing else. The
/// only such value required by the system is this repository's own checksum
/// form; `sha256:source` and fixture digests deliberately do not qualify.
pub(super) fn comparable_sha256_digest(checksum: &str) -> Option<&str> {
    let digest = checksum.strip_prefix("sha256:")?;
    is_lowercase_sha256_hex(digest).then_some(digest)
}

/// The digest segment of a revision storage key: the prefix before the first
/// `-` of its last `/`-separated component, matching
/// `content/{ws}/{lib}/{digest}-{safe_file_name}`. `None` when the key has no
/// such component.
pub(super) fn storage_key_segment(storage_key: &str) -> Option<&str> {
    let file_name = storage_key.rsplit('/').next()?;
    file_name.split_once('-').map(|(segment, _)| segment)
}

/// Folds `(storage_key, checksum)` pairs from `content_revision` rows into
/// `expectations`, the per-encoded-path expectation the byte check later reads.
/// The caller streams NDJSON and folds each line as it arrives, so no
/// `(storage_key, checksum)` buffer is ever retained and the whole
/// `content_revision` table cannot grow memory use. The blob path is never
/// decoded backwards — `encode_blob_path` is lossy — so the fold goes from
/// candidate storage keys to their encoded form, exactly as the exporter does.
pub(super) fn fold_content_revision_blob_expectations<'a>(
    expectations: &mut BTreeMap<String, BlobPathExpectation>,
    rows: impl IntoIterator<Item = (&'a str, &'a str)>,
) {
    for (storage_key, checksum) in rows {
        let expectation = expectations.entry(encode_blob_path(storage_key)).or_default();
        if let Some(segment) = storage_key_segment(storage_key) {
            expectation.segments.insert(segment.to_string());
        }
        if let Some(digest) = comparable_sha256_digest(checksum) {
            expectation.comparable.insert(digest.to_string());
        }
    }
}

/// Verifies every observed blob against the `content_revision` byte contract.
///
/// A contract-proven blob whose bytes disagree aborts the import before the
/// target is cleared. A blob whose contract is not proven is counted as
/// unverifiable and never aborts. This proves the archive is internally
/// consistent, not authentic: substituting both the blob bytes and the row's
/// checksum together still passes.
pub(super) fn verify_snapshot_blobs(
    observed: &SnapshotInventory,
    expectations: &BTreeMap<String, BlobPathExpectation>,
) -> anyhow::Result<SnapshotBlobVerification> {
    let mut verification = SnapshotBlobVerification::default();
    for blob in &observed.blobs {
        let encoded = blob.path.strip_prefix("blobs/").unwrap_or(&blob.path);
        match expectations.get(encoded).and_then(BlobPathExpectation::proven_digest) {
            Some(expected) if blob.sha256 == expected => verification.verified += 1,
            Some(expected) => {
                return Err(anyhow!(
                    "snapshot blob `{}` bytes disagree with the digest its content_revision.checksum names: expected {expected}, observed {}. The archive is internally inconsistent; this is not a failed authenticity check, because a coherent substitution of both the blob and its checksum would pass.",
                    blob.path,
                    blob.sha256
                ));
            }
            None => verification.unverifiable += 1,
        }
    }
    Ok(verification)
}

/// Soft cap for a single NDJSON part inside the tar stream. Small enough
/// that no individual table part holds the entire table in memory, large
/// enough that tar header overhead stays negligible.
pub(super) const CHUNK_BYTES_SOFT_CAP: usize = 64 * 1024 * 1024;

/// Hard cap on a single NDJSON row during import. Rows are read with
/// `read_until` against a bounded buffer; anything beyond this size
/// aborts the import. The biggest legitimate row in the current schema
/// is a `content_revision` with an embedded markdown blob; even very
/// verbose ones stay well under 16 MiB.
pub(super) const MAX_IMPORT_LINE_BYTES: usize = 32 * 1024 * 1024;
/// Bounded compressed archive size accepted before prevalidation.
pub(crate) const MAX_SNAPSHOT_ARCHIVE_BYTES: u64 = 16 * 1024 * 1024 * 1024;
/// Bounded decompressed tar payload accepted during prevalidation. This is
/// enforced independently of the compressed-body cap because zstd streams can
/// expand by orders of magnitude.
pub(super) const MAX_SNAPSHOT_DECOMPRESSED_BYTES: u64 = 16 * 1024 * 1024 * 1024;
/// JSON control entries are deliberately small. Payload entries must be NDJSON
/// or blobs and are always validated as streams.
pub(super) const MAX_SNAPSHOT_METADATA_BYTES: u64 = 8 * 1024 * 1024;

pub(super) struct SpoolSnapshotArchive {
    pub(super) path: tempfile::TempPath,
}

pub(super) async fn spool_snapshot_archive<R>(body: R) -> anyhow::Result<SpoolSnapshotArchive>
where
    R: AsyncRead + Unpin + Send,
{
    let file = tempfile::NamedTempFile::new().context("create snapshot spool file")?;
    let path = file.into_temp_path();
    let mut output = tokio::fs::File::create(&path).await.context("open snapshot spool file")?;
    let copied = tokio::io::copy(&mut body.take(MAX_SNAPSHOT_ARCHIVE_BYTES + 1), &mut output)
        .await
        .context("spool snapshot archive")?;
    anyhow::ensure!(copied <= MAX_SNAPSHOT_ARCHIVE_BYTES, "snapshot archive exceeds maximum size");
    tokio::io::AsyncWriteExt::shutdown(&mut output)
        .await
        .context("finalize snapshot spool file")?;
    Ok(SpoolSnapshotArchive { path })
}

pub(super) async fn prevalidate_snapshot_archive(
    spool: &SpoolSnapshotArchive,
) -> anyhow::Result<(SnapshotManifest, SnapshotBlobVerification)> {
    Box::pin(prevalidate_snapshot_archive_with_limits(
        spool,
        MAX_SNAPSHOT_DECOMPRESSED_BYTES,
        MAX_SNAPSHOT_DECOMPRESSED_BYTES,
    ))
    .await
}

pub(super) async fn prevalidate_snapshot_archive_with_limits(
    spool: &SpoolSnapshotArchive,
    max_entry_bytes: u64,
    max_total_bytes: u64,
) -> anyhow::Result<(SnapshotManifest, SnapshotBlobVerification)> {
    let file =
        tokio::fs::File::open(&spool.path).await.context("open snapshot for prevalidation")?;
    let decoder = ZstdDecoder::new(BufReader::new(file));
    let archive = Archive::new(decoder);
    let mut entries = archive.entries().context("open snapshot archive for prevalidation")?;
    let (manifest, _, manifest_size) = read_library_snapshot_manifest(&mut entries).await?;
    anyhow::ensure!(
        manifest_size <= max_total_bytes,
        "snapshot archive exceeds maximum decompressed size"
    );
    let mut observed = SnapshotInventory {
        postgres_row_counts: manifest
            .postgres_tables
            .iter()
            .map(|table| (table.clone(), 0))
            .collect(),
        ..SnapshotInventory::default()
    };
    let mut terminal_summary: Option<SnapshotSummary> = None;
    let mut decompressed_bytes = manifest_size;
    // Folded line by line as the NDJSON parts stream past; never buffered.
    let mut blob_expectations: BTreeMap<String, BlobPathExpectation> = BTreeMap::new();
    let mut content_closure = SnapshotContentClosure::default();
    while let Some(entry) = entries.next().await {
        let mut entry = entry.context("read snapshot prevalidation entry")?;
        let path = entry
            .path()
            .context("read snapshot prevalidation path")?
            .to_string_lossy()
            .into_owned();
        validate_archive_path(&path)?;
        let declared_size = register_prevalidation_entry(
            &entry,
            &path,
            &mut decompressed_bytes,
            max_entry_bytes,
            max_total_bytes,
        )?;
        if path == "summary.json" {
            anyhow::ensure!(terminal_summary.is_none(), "snapshot has duplicate summary.json");
            terminal_summary =
                Some(read_bounded_json_entry(&mut entry, &path, declared_size).await?);
            continue;
        }
        anyhow::ensure!(terminal_summary.is_none(), "snapshot summary.json must be terminal");
        if let Some(rest) = path.strip_prefix("postgres/") {
            let (table, _) = split_section_path(rest)?;
            let (rows, digest) = Box::pin(validate_ndjson_entry(
                &mut entry,
                &path,
                table,
                declared_size,
                &mut blob_expectations,
                &mut content_closure,
            ))
            .await
            .with_context(|| format!("validate snapshot rows in `{path}`"))?;
            *observed.postgres_row_counts.entry(table.to_string()).or_default() += rows;
            observed.postgres_parts.push(digest);
        } else if path.starts_with("blobs/") {
            observed.blobs.push(
                Box::pin(stream_snapshot_entry_digest(&mut entry, &path, declared_size)).await?,
            );
        } else if path == "EXPORT_FAILED.json" {
            let _: serde_json::Value =
                read_bounded_json_entry(&mut entry, &path, declared_size).await?;
            bail!("snapshot archive is marked as failed export");
        } else {
            bail!("unknown snapshot entry `{path}`");
        }
    }
    let summary = terminal_summary
        .ok_or_else(|| anyhow!("snapshot archive missing terminal summary.json"))?;
    validate_strict_snapshot_inventory(&manifest, &summary.inventory)?;
    validate_terminal_snapshot_summary(&summary.inventory, &observed, true)?;
    let verification = verify_snapshot_blobs(&observed, &blob_expectations)?;
    content_closure.validate(&manifest)?;
    Ok((manifest, verification))
}

pub(super) fn register_prevalidation_entry<R>(
    entry: &async_tar::Entry<R>,
    path: &str,
    total: &mut u64,
    max_entry_bytes: u64,
    max_total_bytes: u64,
) -> anyhow::Result<u64>
where
    R: AsyncRead + Unpin,
{
    let declared_size = entry.header().size().with_context(|| format!("read size of `{path}`"))?;
    anyhow::ensure!(
        declared_size <= max_entry_bytes,
        "snapshot entry `{path}` exceeds maximum decompressed entry size"
    );
    *total = total
        .checked_add(declared_size)
        .ok_or_else(|| anyhow!("snapshot decompressed size overflow"))?;
    anyhow::ensure!(
        *total <= max_total_bytes,
        "snapshot archive exceeds maximum decompressed size"
    );
    Ok(declared_size)
}

pub(super) async fn read_bounded_json_entry<T, R>(
    entry: &mut async_tar::Entry<R>,
    path: &str,
    declared_size: u64,
) -> anyhow::Result<T>
where
    T: serde::de::DeserializeOwned,
    R: AsyncRead + Unpin,
{
    anyhow::ensure!(
        declared_size <= MAX_SNAPSHOT_METADATA_BYTES,
        "snapshot metadata entry `{path}` exceeds maximum size"
    );
    let mut bytes = Vec::with_capacity(declared_size as usize);
    entry.read_to_end(&mut bytes).await.with_context(|| format!("read `{path}`"))?;
    anyhow::ensure!(bytes.len() as u64 == declared_size, "snapshot entry `{path}` is truncated");
    serde_json::from_slice(&bytes).with_context(|| format!("parse `{path}`"))
}

pub(super) async fn stream_snapshot_entry_digest<R>(
    entry: &mut async_tar::Entry<R>,
    path: &str,
    declared_size: u64,
) -> anyhow::Result<SnapshotEntryDigest>
where
    R: AsyncRead + Unpin,
{
    let mut hasher = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = entry.read(&mut buffer).await.with_context(|| format!("read `{path}`"))?;
        if read == 0 {
            break;
        }
        size = size.checked_add(read as u64).ok_or_else(|| anyhow!("snapshot entry too large"))?;
        anyhow::ensure!(size <= declared_size, "snapshot entry `{path}` exceeds its declared size");
        hasher.update(&buffer[..read]);
    }
    anyhow::ensure!(size == declared_size, "snapshot entry `{path}` is truncated");
    Ok(SnapshotEntryDigest { path: path.to_string(), size, sha256: hex::encode(hasher.finalize()) })
}

pub(super) async fn validate_ndjson_entry<R>(
    entry: &mut async_tar::Entry<R>,
    path: &str,
    table: &str,
    declared_size: u64,
    blob_expectations: &mut BTreeMap<String, BlobPathExpectation>,
    content_closure: &mut SnapshotContentClosure,
) -> anyhow::Result<(u64, SnapshotEntryDigest)>
where
    R: AsyncRead + Unpin,
{
    let mut hasher = Sha256::new();
    let mut size = 0_u64;
    let mut rows = 0_u64;
    let mut line = Vec::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = entry.read(&mut buffer).await.with_context(|| format!("read `{path}`"))?;
        if read == 0 {
            break;
        }
        size = size.checked_add(read as u64).ok_or_else(|| anyhow!("snapshot entry too large"))?;
        anyhow::ensure!(size <= declared_size, "snapshot entry `{path}` exceeds its declared size");
        hasher.update(&buffer[..read]);
        for fragment in buffer[..read].split_inclusive(|byte| *byte == b'\n') {
            anyhow::ensure!(
                line.len()
                    .checked_add(fragment.len())
                    .is_some_and(|length| length <= MAX_IMPORT_LINE_BYTES),
                "ndjson line exceeds {MAX_IMPORT_LINE_BYTES} bytes"
            );
            line.extend_from_slice(fragment);
            if fragment.last() == Some(&b'\n') {
                record_ndjson_line(&line, table, &mut rows, blob_expectations, content_closure)?;
                line.clear();
            }
        }
    }
    if !line.is_empty() {
        record_ndjson_line(&line, table, &mut rows, blob_expectations, content_closure)?;
    }
    anyhow::ensure!(size == declared_size, "snapshot entry `{path}` is truncated");
    Ok((
        rows,
        SnapshotEntryDigest {
            path: path.to_string(),
            size,
            sha256: hex::encode(hasher.finalize()),
        },
    ))
}

/// Counts one NDJSON row and, for `content_revision`, folds its
/// `storage_key`/`checksum` pair into `blob_expectations` so prevalidation can
/// later verify blob bytes. The line is parsed once for both purposes and the
/// pair is never retained beyond this call.
fn record_ndjson_line(
    line: &[u8],
    table: &str,
    rows: &mut u64,
    blob_expectations: &mut BTreeMap<String, BlobPathExpectation>,
    content_closure: &mut SnapshotContentClosure,
) -> anyhow::Result<()> {
    let Some(value) = parse_ndjson_line(line)? else {
        return Ok(());
    };
    *rows = rows.checked_add(1).ok_or_else(|| anyhow!("ndjson row count overflow"))?;
    content_closure.observe(table, &value)?;
    if table == "content_revision"
        && let (Some(storage_key), Some(checksum)) = (
            value.get("storage_key").and_then(serde_json::Value::as_str),
            value.get("checksum").and_then(serde_json::Value::as_str),
        )
    {
        fold_content_revision_blob_expectations(
            blob_expectations,
            std::iter::once((storage_key, checksum)),
        );
    }
    Ok(())
}

/// Parses a non-blank NDJSON line, or `None` for a blank one.
pub(super) fn parse_ndjson_line(line: &[u8]) -> anyhow::Result<Option<serde_json::Value>> {
    let line = trim_trailing_newline(line);
    if line.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    serde_json::from_slice::<serde_json::Value>(line).context("parse ndjson row").map(Some)
}
