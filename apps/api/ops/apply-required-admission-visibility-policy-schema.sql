-- Additive live schema: the typed REQUIRED-admission visibility policy.
--
-- Fresh installs receive the `public.admission_visibility_policy` enum and the
-- `content_mutation.visibility_policy` column from
-- apps/api/migrations/0001_init.sql. Existing databases receive them only
-- through `ironrag-maintenance migrate required-admission-visibility-policy-schema
-- --apply`, which holds a transaction-scoped advisory lock, applies this DDL,
-- validates the object groups its own target revision declares — the full current
-- set at this revision — and only then compare-and-sets the baseline checksum.
--
-- Both statements are idempotent: a database that already carries the type or the
-- column succeeds unchanged, so a re-run after a failed stamp is safe. The column
-- carries the single canonical legacy default `'readable'`, so every pre-existing
-- row stays valid without a backfill and the previous binary keeps working with
-- the new object present.
DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_type
        WHERE typname = 'admission_visibility_policy'
          AND typnamespace = 'public'::regnamespace
    ) THEN
        CREATE TYPE public.admission_visibility_policy AS ENUM (
            'readable',
            'suppressed_until_ready'
        );
    END IF;
END
$$;

ALTER TABLE public.content_mutation
    ADD COLUMN IF NOT EXISTS visibility_policy public.admission_visibility_policy
    DEFAULT 'readable'::public.admission_visibility_policy NOT NULL;

COMMENT ON COLUMN public.content_mutation.visibility_policy IS 'Durable, typed admission visibility decision. `readable` is the single legacy default: the accepted replacement is published without suppressing the current readable revision. `suppressed_until_ready` means this REQUIRED replacement cleared both head projections in its admission transaction and only a later accepted mutation that publishes READY restores readability. Every null-head repair predicate reads this column.';
