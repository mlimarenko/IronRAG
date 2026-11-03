-- Executed only by the native guarded transition, never by hand.
ALTER TABLE public.catalog_library
    DROP CONSTRAINT catalog_library_recognition_policy_raster_engine_check;

UPDATE public.catalog_library
SET recognition_policy = jsonb_set(recognition_policy, '{rasterImageEngine}', '"xberg"'::jsonb, false)
WHERE recognition_policy ->> 'rasterImageEngine' = 'docling';

ALTER TABLE public.catalog_library
    ADD CONSTRAINT catalog_library_recognition_policy_raster_engine_check
    CHECK (((recognition_policy ? 'rasterImageEngine'::text)
        AND ((recognition_policy ->> 'rasterImageEngine'::text) = ANY (ARRAY['xberg'::text, 'vision'::text]))
        AND ((recognition_policy - 'rasterImageEngine'::text) = '{}'::jsonb)));
