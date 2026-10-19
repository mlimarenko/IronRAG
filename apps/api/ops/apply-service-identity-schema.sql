-- Additive live schema for owner-scoped service identities.
--
-- Fresh installs receive these exact objects from
-- apps/api/migrations/0001_init.sql. Existing databases receive this
-- file only through `ironrag-maintenance migrate service-identity-schema
-- --apply`, which holds a transaction-scoped advisory lock and updates the
-- baseline checksum only after this DDL succeeds.

CREATE TABLE public.iam_service_identity (
    principal_id uuid NOT NULL,
    owner_auth_provider_kind text NOT NULL,
    owner_external_subject text NOT NULL,
    created_by_principal_id uuid,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    deleted_at timestamp with time zone,
    CONSTRAINT iam_service_identity_oidc_owner_check CHECK (((owner_auth_provider_kind ~~ 'oidc:%'::text) AND (btrim(owner_external_subject) <> ''::text)))
);

CREATE TABLE public.iam_service_identity_operation (
    id uuid DEFAULT uuidv7() NOT NULL,
    service_identity_principal_id uuid NOT NULL,
    owner_auth_provider_kind text NOT NULL,
    owner_external_subject text NOT NULL,
    actor_principal_id uuid,
    operation_kind text NOT NULL,
    idempotency_key text NOT NULL,
    request_fingerprint text NOT NULL,
    credential_secret_version integer,
    encrypted_credential text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    superseded_at timestamp with time zone,
    CONSTRAINT iam_service_identity_operation_credential_check CHECK (((operation_kind = 'revoke'::text) AND (credential_secret_version IS NULL) AND (encrypted_credential IS NULL) AND (superseded_at IS NULL)) OR ((operation_kind = ANY (ARRAY['create'::text, 'rotate'::text])) AND (credential_secret_version IS NOT NULL) AND (((encrypted_credential IS NOT NULL) AND (superseded_at IS NULL)) OR ((encrypted_credential IS NULL) AND (superseded_at IS NOT NULL))))),
    CONSTRAINT iam_service_identity_operation_fingerprint_check CHECK ((request_fingerprint ~ '^[0-9a-f]{64}$'::text)),
    CONSTRAINT iam_service_identity_operation_idempotency_key_check CHECK (((char_length(idempotency_key) >= 1) AND (char_length(idempotency_key) <= 128))),
    CONSTRAINT iam_service_identity_operation_kind_check CHECK ((operation_kind = ANY (ARRAY['create'::text, 'rotate'::text, 'revoke'::text])))
);

ALTER TABLE ONLY public.iam_service_identity
    ADD CONSTRAINT iam_service_identity_pkey PRIMARY KEY (principal_id);
ALTER TABLE ONLY public.iam_service_identity
    ADD CONSTRAINT iam_service_identity_principal_id_owner_key UNIQUE (principal_id, owner_auth_provider_kind, owner_external_subject);
ALTER TABLE ONLY public.iam_service_identity_operation
    ADD CONSTRAINT iam_service_identity_operation_owner_idempotency_key_key UNIQUE (owner_auth_provider_kind, owner_external_subject, idempotency_key);
ALTER TABLE ONLY public.iam_service_identity_operation
    ADD CONSTRAINT iam_service_identity_operation_pkey PRIMARY KEY (id);

CREATE UNIQUE INDEX idx_iam_service_identity_active_credential ON public.iam_service_identity_operation USING btree (service_identity_principal_id) WHERE (encrypted_credential IS NOT NULL);
CREATE INDEX idx_iam_service_identity_owner ON public.iam_service_identity USING btree (owner_auth_provider_kind, owner_external_subject, created_at, principal_id);

ALTER TABLE ONLY public.iam_service_identity
    ADD CONSTRAINT iam_service_identity_created_by_principal_id_fkey FOREIGN KEY (created_by_principal_id) REFERENCES public.iam_principal(id) ON DELETE SET NULL;
ALTER TABLE ONLY public.iam_service_identity
    ADD CONSTRAINT iam_service_identity_principal_id_fkey FOREIGN KEY (principal_id) REFERENCES public.iam_api_token(principal_id) ON DELETE CASCADE;
ALTER TABLE ONLY public.iam_service_identity_operation
    ADD CONSTRAINT iam_service_identity_operation_identity_fkey FOREIGN KEY (service_identity_principal_id, owner_auth_provider_kind, owner_external_subject) REFERENCES public.iam_service_identity(principal_id, owner_auth_provider_kind, owner_external_subject) ON DELETE CASCADE;
ALTER TABLE ONLY public.iam_service_identity_operation
    ADD CONSTRAINT iam_service_identity_operation_actor_principal_id_fkey FOREIGN KEY (actor_principal_id) REFERENCES public.iam_principal(id) ON DELETE SET NULL;
ALTER TABLE ONLY public.iam_service_identity_operation
    ADD CONSTRAINT iam_service_identity_operation_secret_fkey FOREIGN KEY (service_identity_principal_id, credential_secret_version) REFERENCES public.iam_api_token_secret(token_principal_id, secret_version) ON DELETE CASCADE;
