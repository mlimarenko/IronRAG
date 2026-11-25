CREATE OR REPLACE VIEW public.content_current_revision AS
SELECT d.id AS document_id, d.workspace_id, d.library_id, r.id AS revision_id,
       h.visibility_intent_id,
       r.admission_identity->'replacement'->>'fileName' AS source_file_name,
       k.vector_state, k.vector_ready_at, k.graph_state, k.graph_ready_at
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
      AND r.admission_identity->'replacement'->>'checksum' = r.checksum
      AND r.admission_identity->'replacement'->'byteSize' = to_jsonb(r.byte_size)
      AND r.admission_identity->'replacement'->>'mimeType' = r.mime_type
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
