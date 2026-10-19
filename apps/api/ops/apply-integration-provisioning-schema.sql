ALTER TABLE public.iam_service_identity
    ADD COLUMN provisioning_authority text;

ALTER TABLE public.iam_service_identity
    ADD CONSTRAINT iam_service_identity_provisioning_authority_check
    CHECK (provisioning_authority IS NULL OR provisioning_authority = 'integration');

CREATE INDEX idx_iam_service_identity_provisioning_authority
    ON public.iam_service_identity USING btree (provisioning_authority)
    WHERE provisioning_authority IS NOT NULL;

ALTER TABLE public.catalog_workspace
    ADD COLUMN provisioned_by_principal_id uuid;

ALTER TABLE public.catalog_workspace
    ADD CONSTRAINT catalog_workspace_provisioned_by_principal_id_fkey
    FOREIGN KEY (provisioned_by_principal_id) REFERENCES public.iam_principal(id);

ALTER TABLE public.catalog_library
    ADD COLUMN provisioned_by_principal_id uuid;

ALTER TABLE public.catalog_library
    ADD CONSTRAINT catalog_library_provisioned_by_principal_id_fkey
    FOREIGN KEY (provisioned_by_principal_id) REFERENCES public.iam_principal(id);

CREATE INDEX idx_catalog_workspace_provisioning_owner
    ON public.catalog_workspace USING btree (provisioned_by_principal_id)
    WHERE provisioned_by_principal_id IS NOT NULL;

CREATE INDEX idx_catalog_library_provisioning_owner
    ON public.catalog_library USING btree (provisioned_by_principal_id, workspace_id)
    WHERE provisioned_by_principal_id IS NOT NULL;

CREATE FUNCTION public.prevent_catalog_provisioning_owner_change() RETURNS trigger
    LANGUAGE plpgsql
    SET search_path TO 'pg_catalog', 'public'
    AS $$
begin
    if new.provisioned_by_principal_id is distinct from old.provisioned_by_principal_id then
        raise exception 'catalog provisioning ownership is immutable';
    end if;
    return new;
end
$$;

CREATE TRIGGER trg_catalog_workspace_provisioning_owner_immutable
    BEFORE UPDATE OF provisioned_by_principal_id ON public.catalog_workspace
    FOR EACH ROW EXECUTE FUNCTION public.prevent_catalog_provisioning_owner_change();

CREATE TRIGGER trg_catalog_library_provisioning_owner_immutable
    BEFORE UPDATE OF provisioned_by_principal_id ON public.catalog_library
    FOR EACH ROW EXECUTE FUNCTION public.prevent_catalog_provisioning_owner_change();
