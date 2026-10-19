use super::*;

pub(super) async fn append_json_entry<T, W>(
    builder: &mut Builder<W>,
    path: &str,
    value: &T,
) -> anyhow::Result<()>
where
    T: Serialize,
    W: AsyncWrite + Unpin + Send + Sync,
{
    let bytes = serde_json::to_vec_pretty(value).context("serialize json entry")?;
    append_raw_entry(builder, path, &bytes).await.map(|_| ())
}

pub(super) async fn append_raw_entry<W>(
    builder: &mut Builder<W>,
    path: &str,
    bytes: &[u8],
) -> anyhow::Result<SnapshotEntryDigest>
where
    W: AsyncWrite + Unpin + Send + Sync,
{
    let mut header = Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_entry_type(EntryType::Regular);
    header.set_cksum();
    // Use `append_data` instead of `append(&header, data)` so that
    // async-tar emits a GNU LongName extension header for paths that
    // exceed the 100-byte ustar limit. Blob storage keys routinely
    // reach ~160 chars (workspace + library + document + hash + ext).
    builder
        .append_data(&mut header, path, bytes)
        .await
        .with_context(|| format!("append tar entry `{path}`"))?;
    Ok(snapshot_entry_digest(path.to_string(), bytes))
}

/// Which export flavour is finalizing the archive. It owns that flavour's
/// vocabulary: the failure sentinel's id key, the tracing field name, the log
/// message prefix and the finalize error contexts. Keeping the vocabulary in
/// one place lets both flavours share one finalizer without changing any
/// observable log or sentinel field.
#[derive(Clone, Copy)]
pub(super) enum ArchiveOwner {
    Library,
    Workspace,
}

impl ArchiveOwner {
    const fn id_key(self) -> &'static str {
        match self {
            Self::Library => "library_id",
            Self::Workspace => "workspace_id",
        }
    }

    const fn log_prefix(self) -> &'static str {
        match self {
            Self::Library => "",
            Self::Workspace => "workspace ",
        }
    }

    const fn finalize_contexts(self) -> (&'static str, &'static str) {
        match self {
            Self::Library => ("finalize tar builder", "finalize zstd stream"),
            Self::Workspace => ("finalize workspace tar builder", "finalize workspace tar stream"),
        }
    }
}

/// The owning id for an archive being finalized.
pub(super) struct ArchiveFinalizeScope {
    pub(super) owner: ArchiveOwner,
    pub(super) id: Uuid,
}

/// Emits one finalize warning with the flavour's own structured field name
/// (`library_id` or `workspace_id`) so existing log queries keep matching.
fn warn_archive_finalize(scope: &ArchiveFinalizeScope, finalize_error: &anyhow::Error) {
    match scope.owner {
        ArchiveOwner::Library => warn_library_archive_finalize(scope, finalize_error),
        ArchiveOwner::Workspace => warn_workspace_archive_finalize(scope, finalize_error),
    }
}

fn warn_library_archive_finalize(scope: &ArchiveFinalizeScope, _finalize_error: &anyhow::Error) {
    tracing::warn!(
            library_id = %scope.id,
            "{}snapshot export finalize also failed after primary export error",
            scope.owner.log_prefix(),
    );
}

fn warn_workspace_archive_finalize(scope: &ArchiveFinalizeScope, _finalize_error: &anyhow::Error) {
    tracing::warn!(
            workspace_id = %scope.id,
            "{}snapshot export finalize also failed after primary export error",
            scope.owner.log_prefix(),
    );
}

/// Appends the `EXPORT_FAILED.json` sentinel on the failure path, then always
/// finalizes the tar writer. Dropping an `async_tar::Builder` without
/// `into_inner().await` panics and leaves the client a silently truncated
/// archive, so both export flavours share exactly this one finalizer.
///
/// `inner_result` is the body outcome; a finalize error only takes over when
/// the body itself succeeded.
pub(super) async fn finalize_archive_with_failure_sentinel<W>(
    mut builder: Builder<W>,
    scope: ArchiveFinalizeScope,
    inner_result: anyhow::Result<()>,
) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin + Send + Sync,
{
    if let Err(error) = &inner_result {
        append_archive_failure_sentinel(&mut builder, &scope, error).await;
    }
    let (builder_context, stream_context) = scope.owner.finalize_contexts();
    let finalize_result = finalize_archive_writer(builder, builder_context, stream_context).await;
    resolve_archive_finalize_outcome(&scope, inner_result, finalize_result)
}

/// Best-effort sentinel write: a failed append is logged, never allowed to
/// mask the body error that the caller propagates.
async fn append_archive_failure_sentinel<W>(
    builder: &mut Builder<W>,
    scope: &ArchiveFinalizeScope,
    error: &anyhow::Error,
) where
    W: AsyncWrite + Unpin + Send + Sync,
{
    let failure = serde_json::json!({
        "status": "export_failed",
        scope.owner.id_key(): scope.id.to_string(),
        "error": format!("{error:#}"),
    });
    let Err(append_err) = append_json_entry(builder, "EXPORT_FAILED.json", &failure).await else {
        return;
    };
    match scope.owner {
        ArchiveOwner::Library => warn_library_archive_sentinel(scope, &append_err),
        ArchiveOwner::Workspace => warn_workspace_archive_sentinel(scope, &append_err),
    }
}

fn warn_library_archive_sentinel(scope: &ArchiveFinalizeScope, _append_err: &anyhow::Error) {
    tracing::warn!(
            library_id = %scope.id,
            "{}snapshot export failed to append EXPORT_FAILED.json sentinel",
            scope.owner.log_prefix(),
    );
}

fn warn_workspace_archive_sentinel(scope: &ArchiveFinalizeScope, _append_err: &anyhow::Error) {
    tracing::warn!(
            workspace_id = %scope.id,
            "{}snapshot export failed to append EXPORT_FAILED.json sentinel",
            scope.owner.log_prefix(),
    );
}

/// Flushes the tar trailer and shuts the underlying writer down. The contexts
/// come from the flavour so a propagated finalize error reads exactly as it did
/// before the two finalizers were unified.
async fn finalize_archive_writer<W>(
    builder: Builder<W>,
    builder_context: &'static str,
    stream_context: &'static str,
) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin + Send + Sync,
{
    let mut writer = builder.into_inner().await.context(builder_context)?;
    tokio::io::AsyncWriteExt::shutdown(&mut writer).await.context(stream_context)?;
    Ok(())
}

/// The body outcome wins over a finalize error; a finalize error alone is
/// reported. Logs the double-failure case so a hidden finalize error is visible.
fn resolve_archive_finalize_outcome(
    scope: &ArchiveFinalizeScope,
    inner_result: anyhow::Result<()>,
    finalize_result: anyhow::Result<()>,
) -> anyhow::Result<()> {
    match (inner_result, finalize_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(primary), Ok(())) => Err(primary),
        (Ok(()), Err(finalize_err)) => Err(finalize_err),
        (Err(primary), Err(finalize_err)) => {
            warn_archive_finalize(scope, &finalize_err);
            Err(primary)
        }
    }
}

pub(super) fn validate_archive_path(path: &str) -> anyhow::Result<()> {
    if path.is_empty() {
        bail!("tar entry with empty path");
    }
    if path.starts_with('/') {
        bail!("tar entry `{path}` is absolute");
    }
    for component in path.split('/') {
        if component == ".." {
            bail!("tar entry `{path}` contains parent traversal");
        }
    }
    Ok(())
}

pub(super) fn split_section_path(rest: &str) -> anyhow::Result<(&str, &str)> {
    // Layout: <section>/<file>.ndjson
    let (section, file) =
        rest.split_once('/').ok_or_else(|| anyhow!("path `{rest}` is not `<section>/<file>`"))?;
    if !file.starts_with("part-") || !file.ends_with(".ndjson") || file.contains('/') {
        bail!("section file `{file}` is not a valid snapshot part");
    }
    Ok((section, file))
}

pub(super) async fn read_ndjson_entry_and<R, F>(
    entry: &mut async_tar::Entry<R>,
    consume: &mut F,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    F: FnMut(serde_json::Value) -> anyhow::Result<()>,
{
    let mut reader = BufReader::new(entry);
    let mut line: Vec<u8> = Vec::new();
    let mut line_no: usize = 0;
    loop {
        line.clear();
        let read = bounded_read_until(&mut reader, b'\n', &mut line, MAX_IMPORT_LINE_BYTES)
            .await
            .with_context(|| format!("read ndjson line {line_no}"))?;
        if read == 0 {
            break;
        }
        line_no += 1;
        let trimmed = trim_trailing_newline(&line);
        if trimmed.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let value: serde_json::Value = serde_json::from_slice(trimmed)
            .with_context(|| format!("parse ndjson line {line_no}"))?;
        consume(value)?;
    }
    Ok(())
}

pub(super) async fn bounded_read_until<R>(
    reader: &mut BufReader<R>,
    delim: u8,
    buf: &mut Vec<u8>,
    max: usize,
) -> anyhow::Result<usize>
where
    R: AsyncRead + Unpin,
{
    let mut total: usize = 0;
    loop {
        let available = reader.fill_buf().await.context("ndjson fill_buf")?;
        if available.is_empty() {
            return Ok(total);
        }
        if let Some(pos) = available.iter().position(|b| *b == delim) {
            let slice = &available[..=pos];
            if total + slice.len() > max {
                bail!("ndjson line exceeds {max} bytes");
            }
            buf.extend_from_slice(slice);
            total += slice.len();
            let len = slice.len();
            reader.consume(len);
            return Ok(total);
        }
        let len = available.len();
        if total + len > max {
            bail!("ndjson line exceeds {max} bytes");
        }
        buf.extend_from_slice(available);
        total += len;
        reader.consume(len);
    }
}

pub(super) fn trim_trailing_newline(line: &[u8]) -> &[u8] {
    let mut end = line.len();
    while end > 0 && (line[end - 1] == b'\n' || line[end - 1] == b'\r') {
        end -= 1;
    }
    &line[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn archive_failure_sentinel_preserves_library_and_workspace_identity_keys() {
        for (owner, id_key) in
            [(ArchiveOwner::Library, "library_id"), (ArchiveOwner::Workspace, "workspace_id")]
        {
            let id = Uuid::now_v7();
            let mut bytes = Vec::new();
            let error = finalize_archive_with_failure_sentinel(
                Builder::new(&mut bytes),
                ArchiveFinalizeScope { owner, id },
                Err(anyhow!("body failure")),
            )
            .await
            .expect_err("the body failure must propagate after finalization");
            assert_eq!(error.to_string(), "body failure");

            let archive = Archive::new(bytes.as_slice());
            let mut entries = archive.entries().expect("finalized tar must be readable");
            let mut sentinel = None;
            while let Some(entry) = entries.next().await {
                let mut entry = entry.expect("tar entry must be valid");
                if entry.path().expect("tar path").as_ref()
                    == std::path::Path::new("EXPORT_FAILED.json")
                {
                    let mut payload = Vec::new();
                    tokio::io::AsyncReadExt::read_to_end(&mut entry, &mut payload)
                        .await
                        .expect("sentinel payload must be complete");
                    assert!(sentinel.is_none(), "exactly one failure sentinel is permitted");
                    sentinel = Some(
                        serde_json::from_slice::<serde_json::Value>(&payload)
                            .expect("failure sentinel must remain JSON"),
                    );
                }
            }
            assert_eq!(
                sentinel,
                Some(serde_json::json!({
                    "status": "export_failed", id_key: id.to_string(), "error": "body failure",
                }))
            );
        }
    }

    #[test]
    fn archive_finalize_preserves_primary_error_precedence_for_both_owners() {
        for owner in [ArchiveOwner::Library, ArchiveOwner::Workspace] {
            let scope = ArchiveFinalizeScope { owner, id: Uuid::nil() };
            assert!(resolve_archive_finalize_outcome(&scope, Ok(()), Ok(())).is_ok());
            for (body_failed, finalize_failed, expected) in [
                (true, false, "body failure"),
                (false, true, "finalize failure"),
                (true, true, "body failure"),
            ] {
                let body = if body_failed { Err(anyhow!("body failure")) } else { Ok(()) };
                let finalize =
                    if finalize_failed { Err(anyhow!("finalize failure")) } else { Ok(()) };
                assert_eq!(
                    resolve_archive_finalize_outcome(&scope, body, finalize)
                        .expect_err("a failing archive stage must not report success")
                        .to_string(),
                    expected,
                );
            }
        }
    }
}
