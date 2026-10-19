use super::*;

/// Buffers Postgres rows per-table and flushes them as a single
/// `jsonb_populate_recordset` statement. Each table keeps its own
/// pending vec so different `PostgreSQL` sections cannot be accidentally inserted
/// through the most recent table's column list.
pub(super) struct PgBatcher {
    pub(super) pending: BTreeMap<String, Vec<serde_json::Value>>,
    /// Rows Postgres actually wrote, per table. Counting what was handed to the
    /// batcher instead would report rows a conflict clause dropped as restored.
    pub(super) written: BTreeMap<String, u64>,
}

impl PgBatcher {
    pub(super) fn new() -> Self {
        Self { pending: BTreeMap::new(), written: BTreeMap::new() }
    }

    /// Rows written per table so far, consumed once the restore finishes.
    pub(super) fn take_written_counts(&mut self) -> BTreeMap<String, u64> {
        std::mem::take(&mut self.written)
    }

    pub(super) fn push(&mut self, table: &str, row: serde_json::Value) {
        self.pending.entry(table.to_string()).or_default().push(row);
    }

    pub(super) async fn on_new_section(
        &mut self,
        table: &str,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> anyhow::Result<()> {
        let tables: Vec<String> = self
            .pending
            .keys()
            .filter(|pending_table| pending_table.as_str() != table)
            .cloned()
            .collect();
        for pending_table in tables {
            self.flush_table(tx, &pending_table).await?;
        }
        Ok(())
    }

    pub(super) async fn maybe_flush(
        &mut self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> anyhow::Result<()> {
        let tables: Vec<String> = self
            .pending
            .iter()
            .filter(|(_table, rows)| rows.len() >= IMPORT_BATCH_ROWS)
            .map(|(table, _rows)| table.clone())
            .collect();
        for table in tables {
            while self.pending.get(&table).is_some_and(|rows| rows.len() >= IMPORT_BATCH_ROWS) {
                self.flush_partial(tx, &table, IMPORT_BATCH_ROWS).await?;
            }
        }
        Ok(())
    }

    pub(super) async fn flush(
        &mut self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ) -> anyhow::Result<()> {
        let tables: Vec<String> = self.pending.keys().cloned().collect();
        for table in tables {
            self.flush_table(tx, &table).await?;
        }
        Ok(())
    }

    pub(super) async fn flush_table(
        &mut self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        table: &str,
    ) -> anyhow::Result<()> {
        while self.pending.get(table).is_some_and(|rows| !rows.is_empty()) {
            let take = self.pending.get(table).map_or(0, |rows| rows.len().min(IMPORT_BATCH_ROWS));
            self.flush_partial(tx, table, take).await?;
        }
        self.pending.remove(table);
        Ok(())
    }

    pub(super) async fn flush_partial(
        &mut self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        table: &str,
        take: usize,
    ) -> anyhow::Result<()> {
        let rows = self
            .pending
            .get_mut(table)
            .ok_or_else(|| anyhow!("flush_partial called for empty table `{table}`"))?;
        let tail = rows.split_off(take.min(rows.len()));
        let head = std::mem::replace(rows, tail);
        if rows.is_empty() {
            self.pending.remove(table);
        }
        let written = insert_pg_rows_bulk(tx, table, head).await?;
        *self.written.entry(table.to_string()).or_default() += written;
        Ok(())
    }
}
