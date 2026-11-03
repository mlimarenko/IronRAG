-- Frozen synthetic data only, after the ordinary fresh-database migration.
INSERT INTO catalog_workspace(id,slug,display_name)
VALUES ('28500000-0000-4000-8000-000000000001','irg285','IRG-285 synthetic replay');
INSERT INTO catalog_library(id,workspace_id,slug,display_name)
VALUES ('28500000-0000-4000-8000-000000000002','28500000-0000-4000-8000-000000000001','irg285','IRG-285 synthetic replay');

CREATE TEMP TABLE replay_input AS
SELECT * FROM jsonb_to_recordset($1::jsonb) AS doc(
    key text, sha256 text, text text, document_id uuid, revision_id uuid, chunk_id uuid, title text
);
INSERT INTO content_document(id,workspace_id,library_id,external_key,revision_sequence)
SELECT document_id,'28500000-0000-4000-8000-000000000001','28500000-0000-4000-8000-000000000002',key,1 FROM replay_input;
INSERT INTO knowledge_document(document_id,workspace_id,library_id,external_key,file_name,title,
    document_state,active_revision_id,readable_revision_id,latest_revision_no,created_at,updated_at)
SELECT document_id,'28500000-0000-4000-8000-000000000001','28500000-0000-4000-8000-000000000002',key,key,title,
    'active',revision_id,revision_id,1,now(),now() FROM replay_input;
INSERT INTO knowledge_revision(revision_id,workspace_id,library_id,document_id,revision_number,
    revision_state,revision_kind,mime_type,checksum,title,byte_size,normalized_text,text_state,
    vector_state,graph_state,text_readable_at,created_at)
SELECT revision_id,'28500000-0000-4000-8000-000000000001','28500000-0000-4000-8000-000000000002',document_id,1,
    'ready','upload','text/plain','sha256:'||sha256,title,octet_length(text),text,'text_readable',
    'accepted','accepted',now(),now() FROM replay_input;
INSERT INTO content_revision(id,document_id,workspace_id,library_id,revision_number,content_source_kind,
    checksum,mime_type,byte_size,title,admission_identity)
SELECT revision_id,document_id,'28500000-0000-4000-8000-000000000001','28500000-0000-4000-8000-000000000002',1,'upload',
    'sha256:'||sha256,'text/plain',octet_length(text),title,
    jsonb_build_object('origin','mutation','sourceIdentity',null,'materializationToken',null,'visibilityIntentId',null,
        'replacement',jsonb_build_object('checksum','sha256:'||sha256,'byteSize',octet_length(text),
            'mimeType','text/plain','fileName',key,'title',title,'languageCode',null,'sourceUri',null,'documentHint',null))
FROM replay_input;
INSERT INTO content_document_head(document_id,active_revision_id,readable_revision_id)
SELECT document_id,revision_id,revision_id FROM replay_input;
INSERT INTO knowledge_structured_revision(revision_id,workspace_id,library_id,document_id,
    preparation_state,normalization_profile,source_format,block_count,chunk_count,typed_fact_count,
    outline_json,prepared_at,updated_at)
SELECT revision_id,'28500000-0000-4000-8000-000000000001','28500000-0000-4000-8000-000000000002',document_id,
    'prepared','synthetic-whole-file','text',0,1,0,'[]',now(),now() FROM replay_input;
INSERT INTO knowledge_chunk(chunk_id,workspace_id,library_id,document_id,revision_id,
    chunk_index,chunk_kind,content_text,normalized_text,chunk_state,quality_score)
SELECT chunk_id,'28500000-0000-4000-8000-000000000001','28500000-0000-4000-8000-000000000002',document_id,revision_id,
    0,'paragraph',text,text,'ready',1.0 FROM replay_input;

-- Both diagnostic engines see the exact generated-FTS text, not title boosts.
CREATE TABLE lexical_raw AS
SELECT row_number() OVER (ORDER BY c.chunk_id) AS id,c.chunk_id,d.key AS document_key,
    ironrag_unaccent(c.content_text||' '||c.normalized_text||'   ') AS body,c.search_tsv
FROM knowledge_chunk c JOIN replay_input d ON d.chunk_id=c.chunk_id;
ALTER TABLE lexical_raw ADD PRIMARY KEY(id);
