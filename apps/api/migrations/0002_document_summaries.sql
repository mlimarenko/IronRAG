-- Phase 3: Hierarchical Summarization
-- Adds document-level and library-level AI-generated summaries.

alter table content_document_head
    add column document_summary text;

alter table catalog_library
    add column ai_summary text;
