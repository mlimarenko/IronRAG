ALTER TYPE public.billing_owning_execution_kind ADD VALUE 'mcp_document_read';

-- Attribution outlives revision-byte retention; only the tenant scope cascades.
CREATE TABLE public.billing_document_read (
    id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL,
    library_id uuid NOT NULL,
    document_id uuid NOT NULL,
    revision_id uuid NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);

ALTER TABLE ONLY public.billing_document_read
    ADD CONSTRAINT billing_document_read_library_scope_fkey FOREIGN KEY (library_id, workspace_id) REFERENCES public.catalog_library(id, workspace_id) ON DELETE CASCADE;

CREATE INDEX idx_billing_document_read_library_scope ON public.billing_document_read USING btree (library_id, workspace_id);
