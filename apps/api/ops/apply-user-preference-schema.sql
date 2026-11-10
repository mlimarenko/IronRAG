-- Additive live schema for the account-scoped telemetry preference (IRG-350).
--
-- A second migration file is forbidden, so an existing installation reaches
-- this revision only through the guarded operator command:
--
--   ironrag-maintenance migrate user-preference-schema --json
--   ironrag-maintenance migrate user-preference-schema --apply --json
--
-- The command refuses an unknown predecessor, partial or pre-applied DDL, and a
-- damaged target certificate, and stamps the baseline checksum only after the
-- table, primary key, consent check and principal foreign key are certified.
CREATE TABLE public.iam_user_preference (
    principal_id uuid NOT NULL,
    telemetry_consent text NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT iam_user_preference_telemetry_consent_check CHECK ((telemetry_consent = ANY (ARRAY['granted'::text, 'denied'::text])))
);

ALTER TABLE ONLY public.iam_user_preference
    ADD CONSTRAINT iam_user_preference_pkey PRIMARY KEY (principal_id);

ALTER TABLE ONLY public.iam_user_preference
    ADD CONSTRAINT iam_user_preference_principal_id_fkey FOREIGN KEY (principal_id) REFERENCES public.iam_principal(id) ON DELETE CASCADE;
