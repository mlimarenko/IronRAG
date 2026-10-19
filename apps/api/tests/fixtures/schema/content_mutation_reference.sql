-- Canonical modern and historical representations: no trigger disabling or
-- invented predecessor semantic identity. Exception blocks roll back rejected writes.
DO $$
DECLARE
    workspace uuid := uuidv7();
    library uuid := uuidv7();
    modern uuid := uuidv7();
    historical uuid := uuidv7();
    orphan uuid := uuidv7();
    origin_workspace uuid := uuidv7();
    origin_library uuid := uuidv7();
    certificate bytea := decode('bc9ee0ed6d3b14c32ced3759377f9323d7944e2e18cebb09f8e841dfaa9a20b123bd3532654b8d4edba8efaf2e4768bb', 'hex');
BEGIN
    INSERT INTO catalog_workspace(id,slug,display_name) VALUES(workspace,'reference-schema','Reference schema');
    INSERT INTO catalog_library(id,workspace_id,slug,display_name) VALUES(library,workspace,'reference-schema','Reference schema');
    INSERT INTO content_mutation_reference(id,workspace_id,library_id,request_surface,idempotency_scope,idempotency_key)
        VALUES(modern,workspace,library,'rest','modern-scope','modern-key'),
              (historical,workspace,library,'rest','v1:system:'||workspace::text,'retained-real-key');
    INSERT INTO content_mutation(id,workspace_id,library_id,operation_kind,mutation_state,request_identity,request_fingerprint)
        VALUES(modern,workspace,library,'replace','applied','{}','original-modern-fingerprint');
    INSERT INTO content_mutation_predecessor_audit(id,workspace_id,library_id,operation_kind,mutation_state,
        requested_at,request_fingerprint,predecessor_baseline_sha384,origin_workspace_id,origin_library_id)
        VALUES(historical,workspace,library,'replace','applied',now(),null,certificate,origin_workspace,origin_library);
    SET CONSTRAINTS ALL IMMEDIATE;
    SET CONSTRAINTS ALL DEFERRED;
    BEGIN
        INSERT INTO content_mutation_reference(id,workspace_id,library_id,request_surface) VALUES(orphan,workspace,library,'rest');
        SET CONSTRAINTS ALL IMMEDIATE;
        RAISE EXCEPTION 'orphan reference accepted';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    BEGIN
        INSERT INTO content_mutation_predecessor_audit(id,workspace_id,library_id,operation_kind,mutation_state,
            requested_at,predecessor_baseline_sha384,origin_workspace_id,origin_library_id)
            VALUES(modern,workspace,library,'replace','applied',now(),certificate,workspace,library);
        SET CONSTRAINTS ALL IMMEDIATE;
        RAISE EXCEPTION 'duplicate representation accepted';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    BEGIN
        DELETE FROM content_mutation WHERE id=modern;
        SET CONSTRAINTS ALL IMMEDIATE;
        RAISE EXCEPTION 'direct modern deletion accepted';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    BEGIN
        DELETE FROM content_mutation_predecessor_audit WHERE id=historical;
        SET CONSTRAINTS ALL IMMEDIATE;
        RAISE EXCEPTION 'direct audit deletion accepted';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    BEGIN
        UPDATE content_mutation_predecessor_audit SET request_fingerprint='invented' WHERE id=historical;
        RAISE EXCEPTION 'audit update accepted';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    BEGIN
        UPDATE content_mutation_predecessor_audit SET origin_workspace_id=workspace WHERE id=historical;
        RAISE EXCEPTION 'provenance rewrite accepted';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    BEGIN
        INSERT INTO content_mutation_predecessor_audit(id,workspace_id,library_id,operation_kind,mutation_state,
            requested_at,request_identity,predecessor_baseline_sha384,origin_workspace_id,origin_library_id)
            VALUES(modern,workspace,library,'replace','applied',now(),'{}',certificate,origin_workspace,origin_library);
        RAISE EXCEPTION 'historical semantic identity fabrication accepted';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    BEGIN
        UPDATE content_mutation SET request_identity='{"invented":true}' WHERE id=modern;
        RAISE EXCEPTION 'modern identity update accepted';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    BEGIN
        UPDATE content_mutation_reference SET idempotency_key='changed' WHERE id=historical;
        RAISE EXCEPTION 'historical reservation update accepted';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    BEGIN
        INSERT INTO content_mutation(id,workspace_id,library_id,operation_kind,mutation_state,request_fingerprint)
            VALUES(uuidv7(),workspace,library,'replace','applied','missing-identity');
        RAISE EXCEPTION 'modern null identity accepted';
    EXCEPTION WHEN check_violation THEN NULL;
    END;
    BEGIN
        INSERT INTO content_mutation_reference(id,workspace_id,library_id,request_surface,idempotency_scope,idempotency_key)
            VALUES(uuidv7(),workspace,library,'rest','v1:system:'||workspace::text,'retained-real-key');
        RAISE EXCEPTION 'historical key reservation lost';
    EXCEPTION WHEN unique_violation THEN NULL;
    END;
    DELETE FROM content_mutation_reference WHERE id=modern;
    SET CONSTRAINTS ALL IMMEDIATE;
    IF EXISTS(SELECT 1 FROM content_mutation WHERE id=modern)
        OR NOT EXISTS(SELECT 1 FROM content_mutation_predecessor_audit WHERE id=historical AND request_fingerprint IS NULL
            AND request_identity IS NULL AND origin_workspace_id=origin_workspace AND origin_library_id=origin_library
            AND workspace_id<>origin_workspace_id AND library_id<>origin_library_id) THEN
        RAISE EXCEPTION 'canonical parent deletion or historical retention failed';
    END IF;
    SET CONSTRAINTS ALL DEFERRED;
    INSERT INTO content_mutation_reference(id,workspace_id,library_id,request_surface)
        VALUES(orphan,workspace,library,'rest');
    INSERT INTO content_mutation_predecessor_audit(id,workspace_id,library_id,operation_kind,mutation_state,
        requested_at,request_identity,predecessor_baseline_sha384,origin_workspace_id,origin_library_id)
        VALUES(orphan,workspace,library,'replace','applied',now(),null,certificate,origin_workspace,origin_library);
    DELETE FROM content_mutation_reference WHERE id=orphan;
    SET CONSTRAINTS ALL IMMEDIATE;
    IF EXISTS(SELECT 1 FROM content_mutation_predecessor_audit WHERE id=orphan) THEN
        RAISE EXCEPTION 'audit deletion through its registry parent failed';
    END IF;
END;
$$;
