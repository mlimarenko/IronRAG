-- Isolated schema-owner fixture: publication is seeded, not exercised through
-- lifecycle writers. Immutable snapshots, not document labels, own filenames.
DO $$
DECLARE
    workspace uuid := uuidv7();
    library uuid := uuidv7();
    document uuid;
    revision uuid;
    intent_mutation uuid;
    intent_item uuid;
    payload_mutation uuid;
    payload_item uuid;
    scenario record;
    replacement jsonb := jsonb_build_object(
        'checksum', 'sha256:' || repeat('a', 64), 'byteSize', 3,
        'mimeType', 'text/plain', 'fileName', 'replacement.txt',
        'title', null, 'languageCode', null, 'sourceUri', null, 'documentHint', null);
    intent_identity jsonb;
    payload_identity jsonb;
    admission_identity jsonb;
    visible integer;
    published_filename text;
BEGIN
    IF to_regtype('public.admission_visibility_policy') IS NOT NULL
        OR EXISTS (SELECT 1 FROM information_schema.columns WHERE table_schema = 'public'
            AND table_name = 'content_mutation' AND column_name = 'visibility_policy') THEN
        RAISE EXCEPTION 'canonical intent schema must not retain the retired policy enum or column';
    END IF;
    INSERT INTO catalog_workspace(id, slug, display_name)
        VALUES (workspace, 'filename-schema', 'Filename schema');
    INSERT INTO catalog_library(id, workspace_id, slug, display_name)
        VALUES (library, workspace, 'filename-schema', 'Filename schema');
    FOR scenario IN
        SELECT * FROM (VALUES
            ('immutable filename differs from old document label', '"replacement.txt"'::jsonb, true),
            ('missing filename', null::jsonb, false),
            ('null filename', 'null'::jsonb, false),
            ('empty filename', '""'::jsonb, false),
            ('blank filename', '"   "'::jsonb, false),
            ('whitespace filename', to_jsonb(E'\t\r\n'::text), false),
            ('numeric filename', '17'::jsonb, false),
            ('boolean filename', 'true'::jsonb, false),
            ('object filename', '{}'::jsonb, false),
            ('array filename', '[]'::jsonb, false),
            ('mismatched filename', '"different.txt"'::jsonb, false),
            ('mismatched checksum', '"replacement.txt"'::jsonb, false),
            ('mismatched size', '"replacement.txt"'::jsonb, false),
            ('string size', '"replacement.txt"'::jsonb, false),
            ('mismatched MIME', '"replacement.txt"'::jsonb, false),
            ('mismatched title', '"replacement.txt"'::jsonb, false),
            ('mismatched language', '"replacement.txt"'::jsonb, false),
            ('mismatched URI', '"replacement.txt"'::jsonb, false),
            ('mismatched hint', '"replacement.txt"'::jsonb, false),
            ('mismatched source identity', '"replacement.txt"'::jsonb, false),
            ('mismatched materialization token', '"replacement.txt"'::jsonb, false),
            ('revision at floor', '"replacement.txt"'::jsonb, false),
            ('unbound item', '"replacement.txt"'::jsonb, false),
            ('pruned publication history', '"replacement.txt"'::jsonb, true),
            ('text not readable', '"replacement.txt"'::jsonb, false),
            ('missing text timestamp', '"replacement.txt"'::jsonb, false),
            ('vector not ready', '"replacement.txt"'::jsonb, false),
            ('missing vector timestamp', '"replacement.txt"'::jsonb, false),
            ('graph not ready', '"replacement.txt"'::jsonb, false)
        ) AS cases(name, filename, expected)
    LOOP
        document := uuidv7(); revision := uuidv7();
        intent_mutation := uuidv7(); intent_item := uuidv7();
        payload_mutation := uuidv7(); payload_item := uuidv7();
        intent_identity := jsonb_build_object('replacement', replacement,
            'sourceIdentity', 'source-A', 'materializationToken', 'opaque-A');
        IF scenario.filename IS NULL THEN
            intent_identity := intent_identity #- '{replacement,fileName}';
        ELSIF scenario.name <> 'mismatched filename' THEN
            intent_identity := jsonb_set(intent_identity, '{replacement,fileName}', scenario.filename);
        END IF;
        payload_identity := intent_identity;
        IF scenario.name = 'mismatched filename' THEN
            payload_identity := jsonb_set(payload_identity, '{replacement,fileName}', scenario.filename);
        ELSIF scenario.name IN ('mismatched checksum', 'mismatched size', 'string size',
                'mismatched MIME', 'mismatched title', 'mismatched language',
                'mismatched URI', 'mismatched hint') THEN
            -- Equal intent/payload snapshots must still match actual revision metadata.
            payload_identity := jsonb_set(payload_identity, ARRAY['replacement', CASE scenario.name
                WHEN 'mismatched checksum' THEN 'checksum'
                WHEN 'mismatched size' THEN 'byteSize' WHEN 'string size' THEN 'byteSize'
                WHEN 'mismatched MIME' THEN 'mimeType' WHEN 'mismatched title' THEN 'title'
                WHEN 'mismatched language' THEN 'languageCode' WHEN 'mismatched URI' THEN 'sourceUri'
                ELSE 'documentHint' END], CASE WHEN scenario.name = 'mismatched size'
                    THEN '4'::jsonb ELSE '"wrong"'::jsonb END);
            intent_identity := payload_identity;
        ELSIF scenario.name = 'mismatched source identity' THEN
            payload_identity := jsonb_set(payload_identity, '{sourceIdentity}', '"source-B"');
        ELSIF scenario.name = 'mismatched materialization token' THEN
            payload_identity := jsonb_set(payload_identity, '{materializationToken}', '"opaque-B"');
        END IF;
        INSERT INTO content_document(id, workspace_id, library_id, external_key, source_file_name, revision_sequence)
            VALUES (document, workspace, library, scenario.name, 'old-document-label.txt', 8);
        admission_identity := payload_identity || jsonb_build_object('origin', 'mutation',
            'visibilityIntentId', CASE WHEN scenario.name = 'unbound item' THEN null ELSE intent_item END);
        BEGIN
            INSERT INTO content_revision(id, document_id, workspace_id, library_id, revision_number,
                    content_source_kind, checksum, mime_type, byte_size, storage_key, admission_identity)
                VALUES (revision, document, workspace, library, 8, 'upload', 'sha256:' || repeat('a', 64),
                    'text/plain', 3, 'generated-storage-key-is-not-a-filename', admission_identity);
        EXCEPTION WHEN check_violation THEN
            IF scenario.expected THEN RAISE; END IF;
            CONTINUE;
        END;
        INSERT INTO content_mutation_reference(id, workspace_id, library_id, request_surface)
            VALUES (intent_mutation, workspace, library, 'rest'), (payload_mutation, workspace, library, 'rest');
        INSERT INTO content_mutation(id, workspace_id, library_id, operation_kind,
                mutation_state, request_fingerprint, request_identity)
            VALUES (intent_mutation, workspace, library, 'visibility', 'applied', 'fixture-intent', intent_identity),
                (payload_mutation, workspace, library, 'replace', 'applied', 'fixture-payload', payload_identity);
        INSERT INTO content_mutation_item(id, mutation_id, document_id, item_state)
            VALUES (intent_item, intent_mutation, document, 'applied');
        INSERT INTO content_visibility_intent(mutation_item_id, document_id, revision_floor)
            VALUES (intent_item, document, CASE WHEN scenario.name = 'revision at floor' THEN 8 ELSE 7 END);
        INSERT INTO content_mutation_item(id, mutation_id, document_id, result_revision_id, item_state, visibility_intent_id)
            VALUES (payload_item, payload_mutation, document, revision,
                'applied',
                CASE WHEN scenario.name = 'unbound item' THEN null ELSE intent_item END);
        INSERT INTO content_document_head(document_id, active_revision_id, readable_revision_id,
                latest_mutation_id, visibility_intent_id)
            VALUES (document, revision, revision, payload_mutation, intent_item);
        IF scenario.name = 'pruned publication history' THEN
            DELETE FROM content_mutation_reference WHERE id = payload_mutation;
        END IF;
        INSERT INTO knowledge_document(document_id, workspace_id, library_id, external_key,
                document_state, created_at, updated_at)
            VALUES (document, workspace, library, scenario.name, 'active', now(), now());
        INSERT INTO knowledge_revision(revision_id, workspace_id, library_id, document_id,
                revision_number, revision_state, revision_kind, mime_type, checksum, byte_size,
                text_state, vector_state, graph_state, text_readable_at, vector_ready_at, created_at)
            VALUES (revision, workspace, library, document, 8, 'ready', 'upload', 'text/plain',
                'sha256:' || repeat('a', 64), 3,
                CASE WHEN scenario.name = 'text not readable' THEN 'accepted' ELSE 'text_readable' END,
                CASE WHEN scenario.name = 'vector not ready' THEN 'accepted' ELSE 'ready' END,
                CASE WHEN scenario.name = 'graph not ready' THEN 'accepted' ELSE 'graph_degraded' END,
                CASE WHEN scenario.name = 'missing text timestamp' THEN null ELSE now() END,
                CASE WHEN scenario.name = 'missing vector timestamp' THEN null ELSE now() END, now());
        SELECT count(*) INTO visible FROM content_current_revision WHERE document_id = document;
        IF visible <> scenario.expected::integer THEN
            RAISE EXCEPTION 'filename schema regression: %, expected %, got %', scenario.name, scenario.expected::integer, visible;
        END IF;
        SELECT source_file_name INTO published_filename FROM content_current_revision WHERE document_id = document;
        IF scenario.expected AND published_filename IS DISTINCT FROM 'replacement.txt' THEN
            RAISE EXCEPTION 'published filename must come from immutable payload, got %', published_filename;
        END IF;
        UPDATE content_document SET source_file_name = 'renamed-after-bound-READY.txt' WHERE id = document;
        SELECT count(*) INTO visible FROM content_current_revision WHERE document_id = document;
        IF visible <> scenario.expected::integer THEN
            RAISE EXCEPTION 'filename schema regression after rename: %, expected %, got %', scenario.name, scenario.expected::integer, visible;
        END IF;
        SELECT source_file_name INTO published_filename FROM content_current_revision WHERE document_id = document;
        IF scenario.expected AND published_filename IS DISTINCT FROM 'replacement.txt' THEN
            RAISE EXCEPTION 'label rename changed immutable published filename to %', published_filename;
        END IF;
        IF (SELECT r.admission_identity FROM content_revision r WHERE r.id = revision) IS DISTINCT FROM admission_identity
            OR NOT EXISTS (SELECT 1 FROM content_document_head WHERE document_id = document
                AND readable_revision_id = revision AND visibility_intent_id = intent_item) THEN
            RAISE EXCEPTION 'visibility read changed immutable identity or published head';
        END IF;
        IF scenario.name IN ('immutable filename differs from old document label',
                'missing filename', 'null filename', 'empty filename', 'blank filename',
                'whitespace filename', 'numeric filename', 'boolean filename',
                'object filename', 'array filename') THEN
            -- Removing an intent does not change the admitted source filename.
            UPDATE content_document_head SET visibility_intent_id = null WHERE document_id = document;
            SELECT count(*), max(source_file_name) INTO visible, published_filename
                FROM content_current_revision WHERE document_id = document;
            IF visible <> 1 OR published_filename IS DISTINCT FROM 'replacement.txt' THEN
                RAISE EXCEPTION 'ordinary filename projection failed closed incorrectly: %, visible %, filename %',
                    scenario.name, visible, published_filename;
            END IF;
        END IF;
    END LOOP;
END;
$$;

-- Import publication has no operational owner. Mutable hint/storage metadata
-- may change without replacing its immutable admission or admitted filename.
DO $$
DECLARE
    workspace uuid := uuidv7(); library uuid := uuidv7();
    document uuid := uuidv7(); revision uuid := uuidv7(); unadmitted uuid := uuidv7();
    identity jsonb := jsonb_build_object('origin', 'certified_import',
        'replacement', jsonb_build_object('checksum', 'sha256:' || repeat('b',64),
            'byteSize', 3, 'mimeType', 'text/plain', 'fileName', 'admitted-import.txt',
            'title', null, 'languageCode', null, 'sourceUri', null, 'documentHint', null),
        'provenance', jsonb_build_object('recoverySnapshotId',repeat('a',64),
            'identityCertificateSha256',repeat('b',64), 'sourceBaselineSha384',repeat('c',96),
            'admittedBy','schema-test-operator'));
    filename text;
BEGIN
    INSERT INTO catalog_workspace(id,slug,display_name) VALUES (workspace,'import-schema','Import schema');
    INSERT INTO catalog_library(id,workspace_id,slug,display_name) VALUES (library,workspace,'import-schema','Import schema');
    INSERT INTO content_document(id,workspace_id,library_id,external_key)
        VALUES (document,workspace,library,'imported');
    INSERT INTO knowledge_document(document_id,workspace_id,library_id,external_key,
        document_state,created_at,updated_at)
        VALUES (document,workspace,library,'imported','active',now(),now());
    INSERT INTO content_revision(id,document_id,workspace_id,library_id,revision_number,
        content_source_kind,checksum,mime_type,byte_size,admission_identity)
        VALUES (revision,document,workspace,library,1,'upload','sha256:' || repeat('b',64),
            'text/plain',3,identity);
    INSERT INTO knowledge_revision(revision_id,document_id,workspace_id,library_id,revision_number,
        revision_state,revision_kind,checksum,mime_type,byte_size,text_state,vector_state,
        graph_state,text_readable_at,created_at)
        VALUES (revision,document,workspace,library,1,'ready','upload','sha256:' || repeat('b',64),
            'text/plain',3,'text_readable','accepted','accepted',now(),now());
    INSERT INTO content_document_head(document_id,active_revision_id,readable_revision_id)
        VALUES (document,revision,revision);
    UPDATE content_revision SET document_hint='updated hint',storage_key='relocated/key' WHERE id=revision;
    SELECT source_file_name INTO filename FROM content_current_revision WHERE document_id=document;
    IF filename IS DISTINCT FROM 'admitted-import.txt' THEN
        RAISE EXCEPTION 'import admission did not preserve readable source after metadata update';
    END IF;
    BEGIN
        UPDATE content_revision SET admission_identity=jsonb_set(identity,'{replacement,fileName}', '"forged.txt"')
            WHERE id=revision;
        RAISE EXCEPTION 'immutable import admission accepted a rewrite';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    BEGIN
        UPDATE content_revision SET checksum='sha256:' || repeat('d',64) WHERE id=revision;
        RAISE EXCEPTION 'admitted revision accepted different source bytes';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    IF EXISTS (SELECT 1 FROM content_document_head WHERE document_id=document
        AND latest_successful_attempt_id IS NOT NULL) THEN
        RAISE EXCEPTION 'import publication fabricated an attempt';
    END IF;
    INSERT INTO content_revision(id,document_id,workspace_id,library_id,revision_number,
        content_source_kind,checksum,mime_type,byte_size)
        VALUES (unadmitted,document,workspace,library,2,'upload','sha256:' || repeat('e',64),
            'text/plain',3);
    BEGIN
        UPDATE content_document_head SET readable_revision_id=unadmitted WHERE document_id=document;
        SET CONSTRAINTS content_document_head_revision_admission IMMEDIATE;
        RAISE EXCEPTION 'unadmitted revision became readable';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    IF (SELECT readable_revision_id FROM content_document_head WHERE document_id=document)
        IS DISTINCT FROM revision THEN
        RAISE EXCEPTION 'rejected admission did not roll back its head write';
    END IF;
END;
$$;
