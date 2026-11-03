use super::*;

async fn seed_lexical_document(
    fixture: &KnowledgeSearchFixture,
    workspace_id: Uuid,
    library_id: Uuid,
    title: &str,
    file_name: &str,
    chunks: &[&str],
) -> Result<Vec<Uuid>> {
    let document_id = seed_metadata_search_document(
        &fixture.postgres,
        workspace_id,
        library_id,
        file_name,
        title,
        "active",
    )
    .await?;
    let revision_id: Uuid = sqlx::query_scalar(
        "select readable_revision_id from content_document_head where document_id=$1",
    )
    .bind(document_id)
    .fetch_one(&fixture.postgres)
    .await?;
    sqlx::query(
        "update knowledge_document set title=$2,active_revision_id=$3,
        readable_revision_id=$3,latest_revision_no=1 where document_id=$1",
    )
    .bind(document_id)
    .bind(title)
    .bind(revision_id)
    .execute(&fixture.postgres)
    .await?;
    let mut ids = Vec::new();
    for (index, content) in chunks.iter().enumerate() {
        let chunk_id = Uuid::now_v7();
        sqlx::query("insert into knowledge_chunk(chunk_id,workspace_id,library_id,document_id,
            revision_id,chunk_index,chunk_kind,content_text,normalized_text,chunk_state,quality_score)
            values($1,$2,$3,$4,$5,$6,'paragraph',$7,$7,'ready',1.0)")
            .bind(chunk_id).bind(workspace_id).bind(library_id).bind(document_id)
            .bind(revision_id).bind(i32::try_from(index)?).bind(content)
            .execute(&fixture.postgres).await?;
        ids.push(chunk_id);
    }
    let source = chunks.join("\n");
    seed_prepared_structured_revision(
        &fixture.document_store,
        PreparedStructuredRevisionSeed {
            workspace_id,
            library_id,
            document_id,
            revision_id,
            source_text: &source,
            chunk_count: i32::try_from(chunks.len())?,
        },
    )
    .await?;
    Ok(ids)
}

async fn search_ids(
    fixture: &KnowledgeSearchFixture,
    library_id: Uuid,
    query: &str,
    config: Option<&str>,
) -> Result<BTreeSet<Uuid>> {
    let hits = fixture
        .search_store
        .search_chunks(ChunkLexicalQuery {
            library_id,
            query,
            query_origin: LexicalQueryOrigin::UserQuery,
            limit: 128,
            temporal_start: None,
            temporal_end: None,
            text_search_config: config,
        })
        .await?;
    let ids: BTreeSet<_> = hits.iter().map(|hit| hit.chunk_id).collect();
    anyhow::ensure!(ids.len() == hits.len(), "duplicate chunk IDs for {query}");
    Ok(ids)
}

#[tokio::test]
#[ignore = "requires isolated postgres with database create/drop access"]
async fn preserves_native_query_intent_before_candidate_limits() -> Result<()> {
    let fixture = KnowledgeSearchFixture::create().await?;
    let result = async {
        let (workspace_id, library_id) =
            create_empty_vector_test_library(&fixture, "lexical-intent").await?;
        let mut chunks = Vec::new();
        for (title, file, body) in [
            ("Relay Standard", "standard.txt", "relay cumulative standard"),
            ("Relay Extended", "extended.txt", "relay immediate extended"),
            ("Relay cumulative", "title.txt", "relay stable"),
            ("Relay Archive", "cumulative notes", "relay stable"),
            ("Beacon Notes", "beacon.txt", "beacon cumulative"),
            ("Relay Combined", "combined.txt", "relay cumulative immediate"),
            ("Relay Reversed", "reversed.txt", "relay immediate cumulative"),
            ("Relay Loose", "loose.txt", "relay bridge immediate"),
            ("Relay Variant", "variant.txt", "relayed immediateness"),
            ("Field Manual", "manual.txt", "unrelated prose"),
            ("Unrelated", "Field Manual", "unrelated prose"),
            ("Field", "Manual", "unrelated prose"),
            ("Field Manuals", "english.txt", "unrelated prose"),
            ("Identifier", "rx-42", "unrelated prose"),
            ("Manual", "boundary.txt", "Field"),
            ("Relay Host", "cumulative.txt", "relay stable"),
            ("Field Notes", "notes.txt", "field manuals"),
            ("Field Notes", "guide.txt", "field guide"),
        ] {
            chunks.push(
                seed_lexical_document(&fixture, workspace_id, library_id, title, file, &[body])
                    .await?[0],
            );
        }
        let stack = seed_lexical_document(
            &fixture,
            workspace_id,
            library_id,
            "Relay Stack",
            "stack.txt",
            &["forbidden cumulative", "forbidden cumulative", "stable"],
        )
        .await?;

        // This is text/soft-title overlap: the title lacks 'cumulative', so
        // the identity lane cannot hide duplicate IDs from that lane pair.
        anyhow::ensure!(
            search_ids(&fixture, library_id, "relay cumulative", None).await?.contains(&chunks[0])
        );

        let excluded = search_ids(&fixture, library_id, "relay -cumulative", None).await?;
        anyhow::ensure!(excluded.contains(&chunks[1]), "negative query lost its positive twin");
        for index in [0, 2, 3, 5, 6] {
            anyhow::ensure!(
                !excluded.contains(&chunks[index]),
                "excluded body/title/file leaked at {index}"
            );
        }
        // PostgreSQL treats a host-like filename as one lexeme, not a stem plus
        // extension. Native negation must not become a substring blacklist.
        anyhow::ensure!(excluded.contains(&chunks[15]));
        anyhow::ensure!(excluded.contains(&stack[2]), "eligible third title chunk was crowded out");
        anyhow::ensure!(!excluded.contains(&stack[0]) && !excluded.contains(&stack[1]));
        let opposite = search_ids(&fixture, library_id, "relay -immediate", None).await?;
        anyhow::ensure!(opposite.contains(&chunks[0]) && !opposite.contains(&chunks[1]));
        anyhow::ensure!(search_ids(&fixture, library_id, "relay -relay", None).await?.is_empty());

        // Native OR grouping must not become one global excluded-word list.
        let left_or = search_ids(&fixture, library_id, "relay OR beacon -cumulative", None).await?;
        anyhow::ensure!(left_or.contains(&chunks[0]) && !left_or.contains(&chunks[4]));
        let right_or =
            search_ids(&fixture, library_id, "relay -cumulative OR beacon", None).await?;
        anyhow::ensure!(right_or.contains(&chunks[4]) && !right_or.contains(&chunks[0]));
        let both = search_ids(&fixture, library_id, "relay OR beacon", None).await?;
        anyhow::ensure!(both.contains(&chunks[0]) && both.contains(&chunks[4]));

        let negative_phrase =
            search_ids(&fixture, library_id, "relay -\"cumulative immediate\"", None).await?;
        for index in [0, 1, 6] {
            anyhow::ensure!(negative_phrase.contains(&chunks[index]));
        }
        anyhow::ensure!(!negative_phrase.contains(&chunks[5]));
        let phrase = search_ids(&fixture, library_id, "\"relay immediate\"", None).await?;
        anyhow::ensure!(phrase.contains(&chunks[1]) && phrase.contains(&chunks[6]));
        anyhow::ensure!(!phrase.contains(&chunks[7]) && !phrase.contains(&chunks[8]));

        // The searchable record already includes metadata; syntax must not turn
        // a quoted title ask into a body-only query. Positions are flattened.
        for query in ["Field Manual", "\"Field Manual\""] {
            let titles = search_ids(&fixture, library_id, query, None).await?;
            for index in [9, 10, 11, 14] {
                anyhow::ensure!(
                    titles.contains(&chunks[index]),
                    "lost title/filename/boundary {index} for {query}"
                );
            }
        }
        anyhow::ensure!(
            !search_ids(&fixture, library_id, "\"Field Manual\"", None)
                .await?
                .contains(&chunks[12])
        );
        anyhow::ensure!(
            search_ids(&fixture, library_id, "\"Field Manual\"", Some("english"))
                .await?
                .contains(&chunks[12])
        );
        anyhow::ensure!(
            search_ids(&fixture, library_id, "\"field manuals\"", Some("english"))
                .await?
                .contains(&chunks[16]),
            "English body phrase was rejected by mixed analyzers"
        );
        let english_negative =
            search_ids(&fixture, library_id, "field -manuals", Some("english")).await?;
        anyhow::ensure!(!english_negative.contains(&chunks[16]), "English body negation leaked");
        anyhow::ensure!(
            english_negative.contains(&chunks[17]),
            "English negative query lost positive control"
        );
        for query in ["rx-42", "\"rx-42\""] {
            anyhow::ensure!(
                search_ids(&fixture, library_id, query, None).await?.contains(&chunks[13]),
                "literal hyphen changed meaning"
            );
        }

        // More forbidden title documents than the discovery cap must not hide
        // the only eligible title-only document inserted after them.
        for index in 0..51 {
            seed_lexical_document(
                &fixture,
                workspace_id,
                library_id,
                "Caprelay cumulative",
                &format!("excluded-{index}.txt"),
                &["forbidden"],
            )
            .await?;
        }
        let eligible = seed_lexical_document(
            &fixture,
            workspace_id,
            library_id,
            "Caprelay Manual",
            "eligible.txt",
            &["clear"],
        )
        .await?[0];
        let capped = search_ids(&fixture, library_id, "caprelay -cumulative", None).await?;
        anyhow::ensure!(
            capped == BTreeSet::from([eligible]),
            "eligible document lost before the title cap"
        );
        Ok(())
    }
    .await;
    fixture.cleanup().await?;
    result
}
