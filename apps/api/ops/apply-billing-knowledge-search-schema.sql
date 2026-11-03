ALTER TYPE public.billing_owning_execution_kind ADD VALUE 'knowledge_search';

-- Billing attribution needs a real request owner, not a fabricated query execution.
CREATE TABLE public.billing_knowledge_search (
    id uuid PRIMARY KEY,
    workspace_id uuid NOT NULL,
    library_id uuid NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);

ALTER TABLE ONLY public.billing_knowledge_search
    ADD CONSTRAINT billing_knowledge_search_library_scope_fkey FOREIGN KEY (library_id, workspace_id) REFERENCES public.catalog_library(id, workspace_id) ON DELETE CASCADE;

CREATE INDEX idx_billing_knowledge_search_library_scope ON public.billing_knowledge_search USING btree (library_id, workspace_id);
