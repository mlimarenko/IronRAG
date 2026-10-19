-- Canonical visibility intent transition. The maintenance owner validates the
-- exact predecessor, historical policy closure and coherent published lineage
-- before running this body. The retired policy has no current runtime authority.
LOCK TABLE public.content_mutation, public.content_mutation_item,
    public.content_document_head, public.content_web_ingest_run, public.ingest_job
    IN SHARE ROW EXCLUSIVE MODE;
ALTER TABLE public.content_mutation DROP COLUMN visibility_policy;
DROP TYPE public.admission_visibility_policy;
ALTER TYPE public.content_mutation_operation_kind ADD VALUE 'visibility';
ALTER TABLE public.content_document
    ADD COLUMN revision_sequence integer DEFAULT 0 NOT NULL,
    ADD CONSTRAINT content_document_revision_sequence_check CHECK (revision_sequence >= 0);
UPDATE public.content_document d SET revision_sequence = COALESCE(
    (SELECT max(r.revision_number) FROM public.content_revision r WHERE r.document_id = d.id), 0
);
ALTER TABLE public.content_mutation ADD COLUMN request_identity jsonb;
CREATE TABLE public.content_mutation_reference (
    id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL,
    library_id uuid NOT NULL,
    request_surface public.surface_kind NOT NULL,
    idempotency_scope text,
    idempotency_key text,
    reservation_principal_id uuid,
    CONSTRAINT content_mutation_reference_scope_key UNIQUE (id, workspace_id, library_id),
    CONSTRAINT content_mutation_reference_library_fkey FOREIGN KEY (library_id, workspace_id)
        REFERENCES public.catalog_library (id, workspace_id) ON DELETE CASCADE,
    CONSTRAINT content_mutation_reference_reservable_check CHECK
        (idempotency_key IS NULL OR idempotency_scope IS NOT NULL OR reservation_principal_id IS NOT NULL)
);
INSERT INTO public.content_mutation_reference
    SELECT id, workspace_id, library_id, request_surface, idempotency_scope,
           idempotency_key, requested_by_principal_id FROM public.content_mutation;
ALTER TABLE public.content_document_head DROP CONSTRAINT content_document_head_latest_mutation_id_fkey,
    ADD CONSTRAINT content_document_head_latest_mutation_id_fkey FOREIGN KEY (latest_mutation_id)
        REFERENCES public.content_mutation_reference (id) ON DELETE SET NULL;
ALTER TABLE public.content_mutation_item DROP CONSTRAINT content_mutation_item_mutation_id_fkey,
    ADD CONSTRAINT content_mutation_item_mutation_id_fkey FOREIGN KEY (mutation_id)
        REFERENCES public.content_mutation_reference (id) ON DELETE CASCADE;
ALTER TABLE public.content_web_ingest_run DROP CONSTRAINT content_web_ingest_run_mutation_id_fkey,
    ADD CONSTRAINT content_web_ingest_run_mutation_id_fkey FOREIGN KEY (mutation_id, workspace_id, library_id)
        REFERENCES public.content_mutation_reference (id, workspace_id, library_id) ON DELETE CASCADE;
ALTER TABLE public.ingest_job DROP CONSTRAINT ingest_job_mutation_id_fkey,
    ADD CONSTRAINT ingest_job_mutation_id_fkey FOREIGN KEY (mutation_id, workspace_id, library_id)
        REFERENCES public.content_mutation_reference (id, workspace_id, library_id) ON DELETE SET NULL (mutation_id);
CREATE TABLE public.content_mutation_predecessor_audit (
    id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL,
    library_id uuid NOT NULL,
    operation_kind public.content_mutation_operation_kind NOT NULL,
    requested_by_principal_id uuid,
    source_identity text,
    mutation_state public.content_mutation_state NOT NULL,
    requested_at timestamptz NOT NULL,
    completed_at timestamptz,
    failure_code text,
    conflict_code text,
    request_fingerprint text,
    request_identity jsonb,
    predecessor_baseline_sha384 bytea NOT NULL,
    origin_workspace_id uuid NOT NULL,
    origin_library_id uuid NOT NULL,
    CONSTRAINT content_mutation_predecessor_audit_reference_fkey FOREIGN KEY (id, workspace_id, library_id)
        REFERENCES public.content_mutation_reference (id, workspace_id, library_id) ON DELETE CASCADE,
    CONSTRAINT content_mutation_predecessor_audit_identity_check CHECK (request_identity IS NULL),
    CONSTRAINT content_mutation_predecessor_audit_certificate_check CHECK
        (predecessor_baseline_sha384 = decode('bc9ee0ed6d3b14c32ced3759377f9323d7944e2e18cebb09f8e841dfaa9a20b123bd3532654b8d4edba8efaf2e4768bb', 'hex')),
    CONSTRAINT content_mutation_predecessor_audit_terminal_check CHECK
        (mutation_state IN ('applied', 'failed', 'conflicted', 'canceled'))
);
INSERT INTO public.content_mutation_predecessor_audit
    SELECT id, workspace_id, library_id, operation_kind, requested_by_principal_id,
           source_identity, mutation_state, requested_at, completed_at, failure_code,
           conflict_code, request_fingerprint, NULL::jsonb,
           decode('bc9ee0ed6d3b14c32ced3759377f9323d7944e2e18cebb09f8e841dfaa9a20b123bd3532654b8d4edba8efaf2e4768bb', 'hex'),
           workspace_id, library_id
    FROM public.content_mutation WHERE request_identity IS NULL;
DELETE FROM public.content_mutation WHERE request_identity IS NULL;
ALTER TABLE public.content_mutation
    DROP CONSTRAINT content_mutation_library_id_workspace_id_fkey,
    DROP COLUMN request_surface, DROP COLUMN idempotency_scope, DROP COLUMN idempotency_key,
    ALTER COLUMN request_identity SET NOT NULL,
    ADD CONSTRAINT content_mutation_reference_fkey FOREIGN KEY (id, workspace_id, library_id)
        REFERENCES public.content_mutation_reference (id, workspace_id, library_id) ON DELETE CASCADE,
    ADD CONSTRAINT content_mutation_identity_object_check CHECK (jsonb_typeof(request_identity) = 'object');
CREATE UNIQUE INDEX idx_content_mutation_idempotency ON public.content_mutation_reference
    (reservation_principal_id, request_surface, idempotency_key) WHERE idempotency_key IS NOT NULL;
CREATE UNIQUE INDEX idx_content_mutation_scoped_idempotency ON public.content_mutation_reference
    (idempotency_scope, request_surface, idempotency_key)
    WHERE idempotency_scope IS NOT NULL AND idempotency_key IS NOT NULL;
ALTER TABLE public.content_mutation_item
    ADD COLUMN visibility_intent_id uuid,
    ADD CONSTRAINT content_mutation_item_id_document_key UNIQUE (id, document_id),
    ADD CONSTRAINT content_mutation_item_published_lineage_key UNIQUE (id, document_id, result_revision_id);
CREATE INDEX content_mutation_item_document_result_idx
    ON public.content_mutation_item (document_id, result_revision_id);
CREATE TABLE public.content_visibility_intent (
    mutation_item_id uuid PRIMARY KEY,
    document_id uuid NOT NULL,
    revision_floor integer NOT NULL CHECK (revision_floor >= 0),
    CONSTRAINT content_visibility_intent_document_key UNIQUE (mutation_item_id, document_id),
    CONSTRAINT content_visibility_intent_item_fkey FOREIGN KEY (mutation_item_id, document_id)
        REFERENCES public.content_mutation_item (id, document_id) ON DELETE CASCADE,
    CONSTRAINT content_visibility_intent_document_fkey FOREIGN KEY (document_id)
        REFERENCES public.content_document (id) ON DELETE CASCADE
);
ALTER TABLE public.content_mutation_item ADD CONSTRAINT content_mutation_item_visibility_intent_fkey
    FOREIGN KEY (visibility_intent_id, document_id)
    REFERENCES public.content_visibility_intent (mutation_item_id, document_id)
    ON DELETE NO ACTION DEFERRABLE INITIALLY DEFERRED;
ALTER TABLE public.content_document_head ADD COLUMN visibility_intent_id uuid;
ALTER TABLE public.content_revision ADD COLUMN admission_identity jsonb;
-- Maintenance validates the exact readable set and certificate before staging.
-- No attempts, mutation payloads or source filenames are reconstructed here.
UPDATE public.content_revision revision SET admission_identity = imported.admission_identity
FROM pg_temp.content_revision_import_admission imported WHERE imported.revision_id = revision.id;
ALTER TABLE public.content_document_head
    ADD CONSTRAINT content_document_head_visibility_intent_fkey
        FOREIGN KEY (visibility_intent_id, document_id)
        REFERENCES public.content_visibility_intent (mutation_item_id, document_id)
        ON DELETE NO ACTION DEFERRABLE INITIALLY DEFERRED;

CREATE FUNCTION public.preserve_content_request_identity() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        IF NEW.request_identity IS NULL OR NEW.request_fingerprint IS NULL
           OR jsonb_typeof(NEW.request_identity) <> 'object' THEN
            RAISE EXCEPTION 'new content mutation requires semantic identity and fingerprint' USING ERRCODE = '23514';
        END IF;
    ELSIF (NEW.id, NEW.workspace_id, NEW.library_id) IS DISTINCT FROM (OLD.id, OLD.workspace_id, OLD.library_id)
       OR NEW.request_identity IS DISTINCT FROM OLD.request_identity
       OR NEW.request_fingerprint IS DISTINCT FROM OLD.request_fingerprint THEN
        RAISE EXCEPTION 'content request identity is immutable' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER content_mutation_request_identity_immutable
    BEFORE INSERT OR UPDATE ON public.content_mutation
    FOR EACH ROW EXECUTE FUNCTION public.preserve_content_request_identity();

CREATE FUNCTION public.preserve_content_mutation_reference() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        PERFORM 1 FROM public.content_mutation_reference WHERE id=OLD.id FOR UPDATE;
        IF FOUND THEN
            RAISE EXCEPTION 'historical audit deletion requires registry parent deletion' USING ERRCODE = '23514';
        END IF;
        RETURN OLD;
    END IF;
    IF TG_TABLE_NAME = 'content_mutation_predecessor_audit' OR NEW IS DISTINCT FROM OLD THEN
        RAISE EXCEPTION 'mutation reference and historical audit are immutable' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER content_mutation_reference_immutable BEFORE UPDATE ON public.content_mutation_reference
    FOR EACH ROW EXECUTE FUNCTION public.preserve_content_mutation_reference();
CREATE TRIGGER content_mutation_predecessor_audit_immutable BEFORE UPDATE OR DELETE ON public.content_mutation_predecessor_audit
    FOR EACH ROW EXECUTE FUNCTION public.preserve_content_mutation_reference();
CREATE FUNCTION public.verify_content_mutation_representation() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE mutation uuid; representations integer;
BEGIN
    mutation := COALESCE(NEW.id, OLD.id);
    PERFORM 1 FROM public.content_mutation_reference WHERE id = mutation FOR UPDATE;
    IF NOT FOUND THEN RETURN NULL; END IF;
    SELECT (SELECT count(*) FROM public.content_mutation WHERE id = mutation)
         + (SELECT count(*) FROM public.content_mutation_predecessor_audit WHERE id = mutation)
        INTO representations;
    IF representations <> 1 THEN
        RAISE EXCEPTION 'mutation reference requires exactly one representation' USING ERRCODE = '23514';
    END IF;
    IF EXISTS (SELECT 1 FROM public.content_mutation_predecessor_audit audit
               JOIN public.content_mutation_item item ON item.mutation_id = audit.id
               WHERE audit.id = mutation AND (item.item_state NOT IN ('applied', 'failed', 'conflicted', 'skipped')
                 OR item.visibility_intent_id IS NOT NULL)) THEN
        RAISE EXCEPTION 'historical mutation items must be terminal and unbound' USING ERRCODE = '23514';
    END IF;
    RETURN NULL;
END;
$$;
CREATE CONSTRAINT TRIGGER content_mutation_reference_representation AFTER INSERT ON public.content_mutation_reference
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION public.verify_content_mutation_representation();
CREATE CONSTRAINT TRIGGER content_mutation_representation AFTER INSERT OR UPDATE OR DELETE ON public.content_mutation
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION public.verify_content_mutation_representation();
CREATE CONSTRAINT TRIGGER content_mutation_predecessor_audit_representation AFTER INSERT OR UPDATE OR DELETE ON public.content_mutation_predecessor_audit
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION public.verify_content_mutation_representation();
CREATE FUNCTION public.preserve_content_lineage_owner() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_TABLE_NAME = 'content_document' THEN
        IF (NEW.id, NEW.workspace_id, NEW.library_id) IS DISTINCT FROM (OLD.id, OLD.workspace_id, OLD.library_id) THEN
            RAISE EXCEPTION 'document scope is immutable' USING ERRCODE = '23514';
        END IF;
    ELSIF (NEW.id, NEW.mutation_id, NEW.document_id) IS DISTINCT FROM (OLD.id, OLD.mutation_id, OLD.document_id) THEN
        RAISE EXCEPTION 'mutation item ownership is immutable' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER content_mutation_item_owner_immutable BEFORE UPDATE ON public.content_mutation_item
    FOR EACH ROW EXECUTE FUNCTION public.preserve_content_lineage_owner();
CREATE TRIGGER content_document_scope_immutable BEFORE UPDATE ON public.content_document
    FOR EACH ROW EXECUTE FUNCTION public.preserve_content_lineage_owner();
CREATE FUNCTION public.verify_content_mutation_lineage() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    affected jsonb := to_jsonb(NEW);
    mutation uuid := (affected->>'mutation_id')::uuid;
    affected_document uuid := (affected->>'document_id')::uuid;
    item_id uuid;
    job_id uuid;
BEGIN
    IF TG_TABLE_NAME = 'content_mutation_item' THEN item_id := NEW.id;
    ELSIF TG_TABLE_NAME = 'ingest_job' THEN item_id := NEW.mutation_item_id; job_id := NEW.id;
    ELSE mutation := NEW.latest_mutation_id;
    END IF;
    IF EXISTS (SELECT 1 FROM public.content_mutation_item item
        JOIN public.content_mutation_reference owner ON owner.id = item.mutation_id
        JOIN public.content_document document ON document.id = item.document_id
        WHERE item.id = item_id AND (owner.workspace_id, owner.library_id) <> (document.workspace_id, document.library_id))
       OR EXISTS (SELECT 1 FROM public.content_mutation_item item
        LEFT JOIN public.content_revision result ON result.id=item.result_revision_id
        LEFT JOIN public.content_revision base ON base.id=item.base_revision_id
        WHERE item.id=item_id AND ((result.id IS NOT NULL AND result.document_id<>item.document_id)
            OR (base.id IS NOT NULL AND base.document_id<>item.document_id)))
       OR EXISTS (SELECT 1 FROM public.content_document_head head
        JOIN public.content_mutation_reference owner ON owner.id = head.latest_mutation_id
        JOIN public.content_document document ON document.id = head.document_id
        WHERE head.document_id = affected_document AND (owner.workspace_id, owner.library_id) <> (document.workspace_id, document.library_id))
       OR EXISTS (SELECT 1 FROM public.ingest_job job
        JOIN public.content_mutation_item item ON item.id = job.mutation_item_id
        WHERE (job.id = job_id OR item.id = item_id)
          AND (job.mutation_id IS DISTINCT FROM item.mutation_id
           OR job.knowledge_document_id IS DISTINCT FROM item.document_id))
       OR EXISTS (SELECT 1 FROM public.content_mutation_predecessor_audit audit
        JOIN public.content_mutation_item item ON item.mutation_id = audit.id
        WHERE audit.id = mutation AND (item.item_state NOT IN ('applied', 'failed', 'conflicted', 'skipped') OR item.visibility_intent_id IS NOT NULL)) THEN
        RAISE EXCEPTION 'mutation lineage scope or historical item state is invalid' USING ERRCODE = '23514';
    END IF;
    RETURN NULL;
END;
$$;
CREATE CONSTRAINT TRIGGER content_mutation_item_lineage AFTER INSERT OR UPDATE ON public.content_mutation_item
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION public.verify_content_mutation_lineage();
CREATE CONSTRAINT TRIGGER content_document_head_mutation_lineage AFTER INSERT OR UPDATE ON public.content_document_head
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION public.verify_content_mutation_lineage();
CREATE CONSTRAINT TRIGGER ingest_job_mutation_lineage AFTER INSERT OR UPDATE ON public.ingest_job
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION public.verify_content_mutation_lineage();

CREATE FUNCTION public.preserve_revision_admission_identity() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE identity jsonb := NEW.admission_identity; replacement jsonb; provenance jsonb;
BEGIN
    IF TG_OP = 'UPDATE' AND (NEW.admission_identity IS DISTINCT FROM OLD.admission_identity
        OR (NEW.id, NEW.document_id, NEW.workspace_id, NEW.library_id) IS DISTINCT FROM
           (OLD.id, OLD.document_id, OLD.workspace_id, OLD.library_id)) THEN
        RAISE EXCEPTION 'revision admission and ownership are immutable' USING ERRCODE = '23514';
    END IF;
    IF identity IS NULL THEN RETURN NEW; END IF;
    replacement := identity->'replacement';
    IF NOT COALESCE(jsonb_typeof(identity) = 'object'
        AND jsonb_typeof(replacement) = 'object'
        AND replacement ?& ARRAY['checksum','byteSize','mimeType','fileName','title','languageCode','sourceUri','documentHint']
        AND replacement - ARRAY['checksum','byteSize','mimeType','fileName','title','languageCode','sourceUri','documentHint'] = '{}'::jsonb
        AND replacement->>'checksum' = NEW.checksum
        AND replacement->'byteSize' = to_jsonb(NEW.byte_size)
        AND replacement->>'mimeType' = NEW.mime_type
        AND jsonb_typeof(replacement->'checksum') = 'string'
        AND replacement->>'checksum' ~ '^sha256:[0-9a-f]{64}$'
        AND NEW.byte_size >= 0
        AND jsonb_typeof(replacement->'mimeType') = 'string'
        AND NEW.mime_type <> '' AND NEW.mime_type = lower(btrim(NEW.mime_type))
        AND jsonb_typeof(replacement->'fileName') = 'string'
        AND replacement->>'fileName' ~ '[^[:space:]]'
        AND replacement->>'fileName' = btrim(replacement->>'fileName')
        AND replacement->>'fileName' !~ '[/\\[:cntrl:]]'
        AND replacement->>'fileName' NOT IN ('.', '..')
        AND replacement->'title' = COALESCE(to_jsonb(NEW.title), 'null'::jsonb)
        AND replacement->'languageCode' = COALESCE(to_jsonb(NEW.language_code), 'null'::jsonb)
        AND replacement->'sourceUri' = COALESCE(to_jsonb(NEW.source_uri), 'null'::jsonb)
        AND jsonb_typeof(replacement->'documentHint') IN ('string','null'), false) THEN
        RAISE EXCEPTION 'revision admission descriptor is invalid' USING ERRCODE = '23514';
    END IF;
    IF identity->>'origin' = 'mutation' THEN
        IF NOT COALESCE(identity ?& ARRAY['origin','replacement','sourceIdentity','materializationToken','visibilityIntentId']
            AND identity - ARRAY['origin','replacement','sourceIdentity','materializationToken','visibilityIntentId'] = '{}'::jsonb
            AND jsonb_typeof(identity->'sourceIdentity') IN ('string','null')
            AND jsonb_typeof(identity->'materializationToken') IN ('string','null')
            AND (identity->'visibilityIntentId' = 'null'::jsonb OR (
                jsonb_typeof(identity->'visibilityIntentId') = 'string'
                AND identity->>'visibilityIntentId' ~ '^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$'
                AND jsonb_typeof(identity->'sourceIdentity') = 'string'
                AND identity->>'sourceIdentity' ~ '[^[:space:]]')), false) THEN
            RAISE EXCEPTION 'revision mutation admission is invalid' USING ERRCODE = '23514';
        END IF;
    ELSIF identity->>'origin' = 'certified_import' THEN
        provenance := identity->'provenance';
        IF NOT COALESCE(identity - ARRAY['origin','replacement','provenance'] = '{}'::jsonb
            AND jsonb_typeof(provenance) = 'object'
            AND provenance ?& ARRAY['recoverySnapshotId','identityCertificateSha256','sourceBaselineSha384','admittedBy']
            AND provenance - ARRAY['recoverySnapshotId','identityCertificateSha256','sourceBaselineSha384','admittedBy'] = '{}'::jsonb
            AND jsonb_typeof(provenance->'recoverySnapshotId') = 'string'
            AND provenance->>'recoverySnapshotId' ~ '^[0-9a-f]{64}$'
            AND jsonb_typeof(provenance->'identityCertificateSha256') = 'string'
            AND provenance->>'identityCertificateSha256' ~ '^[0-9a-f]{64}$'
            AND jsonb_typeof(provenance->'sourceBaselineSha384') = 'string'
            AND provenance->>'sourceBaselineSha384' ~ '^[0-9a-f]{96}$'
            AND jsonb_typeof(provenance->'admittedBy') = 'string'
            AND provenance->>'admittedBy' ~ '[^[:space:]]'
            AND provenance->>'admittedBy' !~ '[[:cntrl:]]', false) THEN
            RAISE EXCEPTION 'revision import provenance is invalid' USING ERRCODE = '23514';
        END IF;
    ELSE
        RAISE EXCEPTION 'unknown revision admission origin' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER content_revision_admission_immutable BEFORE INSERT OR UPDATE ON public.content_revision
    FOR EACH ROW EXECUTE FUNCTION public.preserve_revision_admission_identity();
CREATE FUNCTION public.verify_head_revision_admission() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM public.content_document_head head
        JOIN public.content_document document ON document.id = head.document_id
        LEFT JOIN public.content_revision revision ON revision.id = head.readable_revision_id
        WHERE head.document_id = NEW.document_id AND head.readable_revision_id IS NOT NULL
          AND (revision.id IS NULL OR revision.admission_identity IS NULL
            OR (revision.document_id, revision.workspace_id, revision.library_id) IS DISTINCT FROM
               (document.id, document.workspace_id, document.library_id))) THEN
        RAISE EXCEPTION 'readable head requires an admitted revision in its scope' USING ERRCODE = '23514';
    END IF;
    RETURN NULL;
END;
$$;
CREATE CONSTRAINT TRIGGER content_document_head_revision_admission AFTER INSERT OR UPDATE ON public.content_document_head
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION public.verify_head_revision_admission();


-- Validate staged admissions and enqueue the new constraint for existing heads.
UPDATE public.content_revision SET admission_identity = admission_identity
    WHERE admission_identity IS NOT NULL;
UPDATE public.content_document_head SET readable_revision_id = readable_revision_id
    WHERE readable_revision_id IS NOT NULL;

CREATE VIEW public.content_current_revision AS
SELECT d.id AS document_id, d.workspace_id, d.library_id, r.id AS revision_id,
       h.visibility_intent_id,
       r.admission_identity->'replacement'->>'fileName' AS source_file_name
FROM public.content_document d
JOIN public.content_document_head h ON h.document_id = d.id
JOIN public.content_revision r ON r.id = h.readable_revision_id AND r.document_id = d.id
    AND r.workspace_id = d.workspace_id AND r.library_id = d.library_id
JOIN public.knowledge_revision k ON k.revision_id = r.id AND k.document_id = d.id
    AND k.workspace_id = d.workspace_id AND k.library_id = d.library_id
LEFT JOIN public.content_visibility_intent intent
    ON intent.mutation_item_id = h.visibility_intent_id AND intent.document_id = d.id
LEFT JOIN public.content_mutation_item intent_item ON intent_item.id = intent.mutation_item_id
LEFT JOIN public.content_mutation intent_mutation ON intent_mutation.id = intent_item.mutation_id
WHERE d.document_state = 'active' AND d.deleted_at IS NULL
  AND jsonb_typeof(r.admission_identity) = 'object'
  AND k.text_state = 'text_readable' AND k.text_readable_at IS NOT NULL
  AND (h.visibility_intent_id IS NULL OR (
      intent.mutation_item_id IS NOT NULL
      AND r.admission_identity->>'origin' = 'mutation'
      AND r.admission_identity->>'visibilityIntentId' = intent.mutation_item_id::text
      AND r.revision_number > intent.revision_floor
      AND intent_mutation.workspace_id = d.workspace_id AND intent_mutation.library_id = d.library_id
      AND k.vector_state = 'ready' AND k.vector_ready_at IS NOT NULL
      AND k.graph_state IN ('ready', 'graph_degraded')
      AND r.admission_identity->'replacement'->>'checksum' = r.checksum
      AND r.admission_identity->'replacement'->'byteSize' = to_jsonb(r.byte_size)
      AND r.admission_identity->'replacement'->>'mimeType' = r.mime_type
       -- The immutable admitted replacement owns the filename. Document labels
       -- and generated storage paths are mutable/non-semantic and cannot bind it.
       AND jsonb_typeof(r.admission_identity->'replacement'->'fileName') = 'string'
       AND r.admission_identity->'replacement'->>'fileName' ~ '[^[:space:]]'
      AND r.admission_identity->'replacement'->>'title' IS NOT DISTINCT FROM r.title
      AND r.admission_identity->'replacement'->>'languageCode' IS NOT DISTINCT FROM r.language_code
      AND r.admission_identity->'replacement'->>'sourceUri' IS NOT DISTINCT FROM r.source_uri
      AND r.admission_identity->'replacement'->>'documentHint' IS NOT DISTINCT FROM r.document_hint
      AND r.admission_identity->'replacement' = intent_mutation.request_identity->'replacement'
      AND r.admission_identity->'sourceIdentity' = intent_mutation.request_identity->'sourceIdentity'
      AND r.admission_identity->'materializationToken' = intent_mutation.request_identity->'materializationToken'
  ));
