update ai_model_catalog
set metadata_json = jsonb_set(
        metadata_json,
        '{defaultRoles}',
        '["extract_graph","query_answer"]'::jsonb
    )
where provider_catalog_id = '00000000-0000-0000-0000-000000000101'
  and model_name = 'gpt-5.4-nano';
