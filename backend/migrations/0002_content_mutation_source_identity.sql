alter table content_mutation
    add column if not exists source_identity text;
