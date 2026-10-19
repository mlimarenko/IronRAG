use super::*;

impl PgContextStore {
    pub(super) async fn replace_bundle_chunk_edges_impl(
        &self,
        bundle_id: Uuid,
        library_id: Uuid,
        edges: &[KnowledgeBundleChunkEdgeRow],
    ) -> anyhow::Result<Vec<KnowledgeBundleChunkEdgeRow>> {
        let mut tx = self.pool.begin().await?;
        delete_edges_in_tx(&mut tx, "knowledge_bundle_chunk", bundle_id).await?;
        if edges.is_empty() {
            tx.commit().await?;
            return Ok(Vec::new());
        }

        let chunk_ids: Vec<Uuid> = edges.iter().map(|edge| edge.chunk_id).collect();
        let ranks: Vec<i32> = edges.iter().map(|edge| edge.rank).collect();
        let scores: Vec<f64> = edges.iter().map(|edge| edge.score).collect();
        let reasons: Vec<Option<&str>> =
            edges.iter().map(|edge| edge.inclusion_reason.as_deref()).collect();
        let created_ats: Vec<DateTime<Utc>> = edges.iter().map(|edge| edge.created_at).collect();
        let rows = sqlx::query_as::<_, PgChunkEdgeRow>(
            "with input as (
                select t.chunk_id, t.rank, t.score, t.inclusion_reason, t.created_at, t.ord
                from unnest(
                    $3::uuid[], $4::int[], $5::double precision[], $6::text[], $7::timestamptz[]
                ) with ordinality as t(chunk_id, rank, score, inclusion_reason, created_at, ord)
             ),
             dedup as (
                select distinct on (chunk_id)
                    chunk_id, rank, score, inclusion_reason, created_at, ord
                from input
                order by chunk_id, ord desc
             ),
             upserted as (
                insert into knowledge_bundle_chunk (
                    bundle_id, chunk_id, library_id, rank, score, inclusion_reason, created_at
                )
                select $1::uuid, d.chunk_id, $2::uuid, d.rank, d.score, d.inclusion_reason, d.created_at
                from dedup d
                join knowledge_chunk target
                  on target.chunk_id = d.chunk_id
                 and target.library_id = $2
                on conflict (bundle_id, chunk_id) do update
                set library_id = excluded.library_id,
                    rank = excluded.rank,
                    score = excluded.score,
                    inclusion_reason = excluded.inclusion_reason,
                    created_at = excluded.created_at
                returning bundle_id, chunk_id, rank, score, inclusion_reason, created_at
             )
             select u.bundle_id, u.chunk_id, u.rank, u.score, u.inclusion_reason, u.created_at
             from upserted u
             join dedup d on d.chunk_id = u.chunk_id
             order by d.ord",
        )
        .bind(bundle_id)
        .bind(library_id)
        .bind(&chunk_ids)
        .bind(&ranks)
        .bind(&scores)
        .bind(&reasons)
        .bind(&created_ats)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(map_rows(rows))
    }

    pub(super) async fn replace_bundle_entity_edges_impl(
        &self,
        bundle_id: Uuid,
        library_id: Uuid,
        edges: &[KnowledgeBundleEntityEdgeRow],
    ) -> anyhow::Result<Vec<KnowledgeBundleEntityEdgeRow>> {
        let mut tx = self.pool.begin().await?;
        delete_edges_in_tx(&mut tx, "knowledge_bundle_entity", bundle_id).await?;
        if edges.is_empty() {
            tx.commit().await?;
            return Ok(Vec::new());
        }

        let entity_ids: Vec<Uuid> = edges.iter().map(|edge| edge.entity_id).collect();
        let ranks: Vec<i32> = edges.iter().map(|edge| edge.rank).collect();
        let scores: Vec<f64> = edges.iter().map(|edge| edge.score).collect();
        let reasons: Vec<Option<&str>> =
            edges.iter().map(|edge| edge.inclusion_reason.as_deref()).collect();
        let created_ats: Vec<DateTime<Utc>> = edges.iter().map(|edge| edge.created_at).collect();
        let rows = sqlx::query_as::<_, PgEntityEdgeRow>(
            "with input as (
                select t.entity_id, t.rank, t.score, t.inclusion_reason, t.created_at, t.ord
                from unnest(
                    $3::uuid[], $4::int[], $5::double precision[], $6::text[], $7::timestamptz[]
                ) with ordinality as t(entity_id, rank, score, inclusion_reason, created_at, ord)
             ),
             dedup as (
                select distinct on (entity_id)
                    entity_id, rank, score, inclusion_reason, created_at, ord
                from input
                order by entity_id, ord desc
             ),
             upserted as (
                insert into knowledge_bundle_entity (
                    bundle_id, entity_id, library_id, rank, score, inclusion_reason, created_at
                )
                select $1::uuid, d.entity_id, $2::uuid, d.rank, d.score, d.inclusion_reason, d.created_at
                from dedup d
                join runtime_graph_node target
                  on target.id = d.entity_id
                 and target.library_id = $2
                on conflict (bundle_id, entity_id) do update
                set library_id = excluded.library_id,
                    rank = excluded.rank,
                    score = excluded.score,
                    inclusion_reason = excluded.inclusion_reason,
                    created_at = excluded.created_at
                returning bundle_id, entity_id, rank, score, inclusion_reason, created_at
             )
             select u.bundle_id, u.entity_id, u.rank, u.score, u.inclusion_reason, u.created_at
             from upserted u
             join dedup d on d.entity_id = u.entity_id
             order by d.ord",
        )
        .bind(bundle_id)
        .bind(library_id)
        .bind(&entity_ids)
        .bind(&ranks)
        .bind(&scores)
        .bind(&reasons)
        .bind(&created_ats)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(map_rows(rows))
    }

    pub(super) async fn replace_bundle_relation_edges_impl(
        &self,
        bundle_id: Uuid,
        library_id: Uuid,
        edges: &[KnowledgeBundleRelationEdgeRow],
    ) -> anyhow::Result<Vec<KnowledgeBundleRelationEdgeRow>> {
        let mut tx = self.pool.begin().await?;
        delete_edges_in_tx(&mut tx, "knowledge_bundle_relation", bundle_id).await?;
        if edges.is_empty() {
            tx.commit().await?;
            return Ok(Vec::new());
        }

        let relation_ids: Vec<Uuid> = edges.iter().map(|edge| edge.relation_id).collect();
        let ranks: Vec<i32> = edges.iter().map(|edge| edge.rank).collect();
        let scores: Vec<f64> = edges.iter().map(|edge| edge.score).collect();
        let reasons: Vec<Option<&str>> =
            edges.iter().map(|edge| edge.inclusion_reason.as_deref()).collect();
        let created_ats: Vec<DateTime<Utc>> = edges.iter().map(|edge| edge.created_at).collect();
        let rows = sqlx::query_as::<_, PgRelationEdgeRow>(
            "with input as (
                select t.relation_id, t.rank, t.score, t.inclusion_reason, t.created_at, t.ord
                from unnest(
                    $3::uuid[], $4::int[], $5::double precision[], $6::text[], $7::timestamptz[]
                ) with ordinality as t(relation_id, rank, score, inclusion_reason, created_at, ord)
             ),
             dedup as (
                select distinct on (relation_id)
                    relation_id, rank, score, inclusion_reason, created_at, ord
                from input
                order by relation_id, ord desc
             ),
             upserted as (
                insert into knowledge_bundle_relation (
                    bundle_id, relation_id, library_id, rank, score, inclusion_reason, created_at
                )
                select $1::uuid, d.relation_id, $2::uuid, d.rank, d.score, d.inclusion_reason, d.created_at
                from dedup d
                join runtime_graph_edge target
                  on target.id = d.relation_id
                 and target.library_id = $2
                on conflict (bundle_id, relation_id) do update
                set library_id = excluded.library_id,
                    rank = excluded.rank,
                    score = excluded.score,
                    inclusion_reason = excluded.inclusion_reason,
                    created_at = excluded.created_at
                returning bundle_id, relation_id, rank, score, inclusion_reason, created_at
             )
             select u.bundle_id, u.relation_id, u.rank, u.score, u.inclusion_reason, u.created_at
             from upserted u
             join dedup d on d.relation_id = u.relation_id
             order by d.ord",
        )
        .bind(bundle_id)
        .bind(library_id)
        .bind(&relation_ids)
        .bind(&ranks)
        .bind(&scores)
        .bind(&reasons)
        .bind(&created_ats)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(map_rows(rows))
    }

    pub(super) async fn replace_bundle_evidence_edges_impl(
        &self,
        bundle_id: Uuid,
        library_id: Uuid,
        edges: &[KnowledgeBundleEvidenceEdgeRow],
    ) -> anyhow::Result<Vec<KnowledgeBundleEvidenceEdgeRow>> {
        let mut tx = self.pool.begin().await?;
        delete_edges_in_tx(&mut tx, "knowledge_bundle_evidence", bundle_id).await?;
        if edges.is_empty() {
            tx.commit().await?;
            return Ok(Vec::new());
        }

        let evidence_ids: Vec<Uuid> = edges.iter().map(|edge| edge.evidence_id).collect();
        let ranks: Vec<i32> = edges.iter().map(|edge| edge.rank).collect();
        let scores: Vec<f64> = edges.iter().map(|edge| edge.score).collect();
        let reasons: Vec<Option<&str>> =
            edges.iter().map(|edge| edge.inclusion_reason.as_deref()).collect();
        let created_ats: Vec<DateTime<Utc>> = edges.iter().map(|edge| edge.created_at).collect();
        let rows = sqlx::query_as::<_, PgEvidenceEdgeRow>(
            "with input as (
                select t.evidence_id, t.rank, t.score, t.inclusion_reason, t.created_at, t.ord
                from unnest(
                    $3::uuid[], $4::int[], $5::double precision[], $6::text[], $7::timestamptz[]
                ) with ordinality as t(evidence_id, rank, score, inclusion_reason, created_at, ord)
             ),
             dedup as (
                select distinct on (evidence_id)
                    evidence_id, rank, score, inclusion_reason, created_at, ord
                from input
                order by evidence_id, ord desc
             ),
             upserted as (
                insert into knowledge_bundle_evidence (
                    bundle_id, evidence_id, library_id, rank, score, inclusion_reason, created_at
                )
                select $1::uuid, d.evidence_id, $2::uuid, d.rank, d.score, d.inclusion_reason, d.created_at
                from dedup d
                join runtime_graph_evidence target
                  on target.id = d.evidence_id
                 and target.library_id = $2
                on conflict (bundle_id, evidence_id) do update
                set library_id = excluded.library_id,
                    rank = excluded.rank,
                    score = excluded.score,
                    inclusion_reason = excluded.inclusion_reason,
                    created_at = excluded.created_at
                returning bundle_id, evidence_id, rank, score, inclusion_reason, created_at
             )
             select u.bundle_id, u.evidence_id, u.rank, u.score, u.inclusion_reason, u.created_at
             from upserted u
             join dedup d on d.evidence_id = u.evidence_id
             order by d.ord",
        )
        .bind(bundle_id)
        .bind(library_id)
        .bind(&evidence_ids)
        .bind(&ranks)
        .bind(&scores)
        .bind(&reasons)
        .bind(&created_ats)
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(map_rows(rows))
    }

    pub(super) async fn list_bundle_chunk_edges_impl(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeBundleChunkEdgeRow>> {
        let rows = sqlx::query_as::<_, PgChunkEdgeRow>(
            "select bundle_id, chunk_id, rank, score, inclusion_reason, created_at
             from knowledge_bundle_chunk
             where bundle_id = $1
             order by rank asc, created_at asc, chunk_id asc",
        )
        .bind(bundle_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(map_rows(rows))
    }

    pub(super) async fn list_bundle_entity_edges_impl(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeBundleEntityEdgeRow>> {
        let rows = sqlx::query_as::<_, PgEntityEdgeRow>(
            "select bundle_id, entity_id, rank, score, inclusion_reason, created_at
             from knowledge_bundle_entity
             where bundle_id = $1
             order by rank asc, created_at asc, entity_id asc",
        )
        .bind(bundle_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(map_rows(rows))
    }

    pub(super) async fn list_bundle_relation_edges_impl(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeBundleRelationEdgeRow>> {
        let rows = sqlx::query_as::<_, PgRelationEdgeRow>(
            "select bundle_id, relation_id, rank, score, inclusion_reason, created_at
             from knowledge_bundle_relation
             where bundle_id = $1
             order by rank asc, created_at asc, relation_id asc",
        )
        .bind(bundle_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(map_rows(rows))
    }

    pub(super) async fn list_bundle_evidence_edges_impl(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeBundleEvidenceEdgeRow>> {
        let rows = sqlx::query_as::<_, PgEvidenceEdgeRow>(
            "select bundle_id, evidence_id, rank, score, inclusion_reason, created_at
             from knowledge_bundle_evidence
             where bundle_id = $1
             order by rank asc, created_at asc, evidence_id asc",
        )
        .bind(bundle_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(map_rows(rows))
    }

    pub(super) async fn delete_bundle_chunk_edges_impl(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<u64> {
        let result = sqlx::query("delete from knowledge_bundle_chunk where bundle_id = $1")
            .bind(bundle_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    pub(super) async fn delete_bundle_entity_edges_impl(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<u64> {
        let result = sqlx::query("delete from knowledge_bundle_entity where bundle_id = $1")
            .bind(bundle_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    pub(super) async fn delete_bundle_relation_edges_impl(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<u64> {
        let result = sqlx::query("delete from knowledge_bundle_relation where bundle_id = $1")
            .bind(bundle_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    pub(super) async fn delete_bundle_evidence_edges_impl(
        &self,
        bundle_id: Uuid,
    ) -> anyhow::Result<u64> {
        let result = sqlx::query("delete from knowledge_bundle_evidence where bundle_id = $1")
            .bind(bundle_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }
}
