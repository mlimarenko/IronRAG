-- One exact execution profile identifies one vector space per library and
-- vector kind. Legacy opaque keys intentionally remain outside this contract.
create unique index if not exists knowledge_vector_manifest_exact_profile_dimension_key
    on knowledge_vector_relation_manifest (library_id, vector_kind, embedding_model_key)
    where embedding_model_key like 'embedding-profile:v1:%'
       or embedding_model_key like 'embedding-rebuild:v1:%';
