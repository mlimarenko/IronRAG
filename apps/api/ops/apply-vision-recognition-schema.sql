ALTER TABLE public.catalog_library DROP CONSTRAINT catalog_library_recognition_policy_raster_engine_check;

DO $vision$
DECLARE changed bigint;
BEGIN
    UPDATE public.catalog_library
       SET recognition_policy = '{"rasterImageEngine":"vision"}'::jsonb
     WHERE recognition_policy = '{"rasterImageEngine":"xberg"}'::jsonb;
    GET DIAGNOSTICS changed = ROW_COUNT;
    IF changed <> (SELECT count(*) FROM pg_temp.vision_policy_before WHERE needs_conversion) THEN
        RAISE EXCEPTION 'Vision policy changed-row count mismatch';
    END IF;
END
$vision$;

ALTER TABLE public.catalog_library ADD CONSTRAINT catalog_library_recognition_policy_raster_engine_check
    CHECK ((recognition_policy = '{"rasterImageEngine": "vision"}'::jsonb));
