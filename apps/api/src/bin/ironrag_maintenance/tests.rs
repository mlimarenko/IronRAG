use super::audit::audit_commands::webhook_outbox_audit_cursor;
use super::lease::lease_commands::scope_for_clear_failure;
use super::*;

#[test]
fn user_preference_migration_requires_explicit_apply() {
    for apply in [false, true] {
        let mut args = vec!["ironrag-maintenance", "migrate", "user-preference-schema", "--json"];
        if apply {
            args.push("--apply");
        }
        let cli = Cli::try_parse_from(args).expect("parse account telemetry preference transition");
        assert!(matches!(cli.command,
            Command::Migrate(MigrateCommand::UserPreferenceSchema { apply: actual, json: true })
            if actual == apply));
    }
}

#[test]
fn billing_knowledge_search_migration_requires_explicit_apply() {
    for apply in [false, true] {
        let mut args =
            vec!["ironrag-maintenance", "migrate", "billing-knowledge-search-schema", "--json"];
        if apply {
            args.push("--apply");
        }
        let cli = Cli::try_parse_from(args).expect("parse knowledge-search billing transition");
        assert!(matches!(cli.command,
            Command::Migrate(MigrateCommand::BillingKnowledgeSearchSchema { apply: actual, json: true })
            if actual == apply));
    }
}

#[test]
fn billing_document_read_migration_requires_explicit_apply() {
    for apply in [false, true] {
        let mut args =
            vec!["ironrag-maintenance", "migrate", "billing-document-read-schema", "--json"];
        if apply {
            args.push("--apply");
        }
        let cli = Cli::try_parse_from(args).expect("parse native-read billing transition");
        assert!(matches!(cli.command,
            Command::Migrate(MigrateCommand::BillingDocumentReadSchema { apply: actual, json: true })
            if actual == apply));
    }
}

#[test]
fn vision_schema_migration_defaults_to_dry_run_and_requires_explicit_apply() {
    for apply in [false, true] {
        let mut args = vec!["ironrag-maintenance", "migrate", "vision-recognition-schema"];
        if apply {
            args.push("--apply");
        }
        let cli = Cli::try_parse_from(args).expect("parse Vision transition");
        assert!(matches!(cli.command,
            Command::Migrate(MigrateCommand::VisionRecognitionSchema { apply: actual, json: false })
            if actual == apply));
    }
}

#[test]
fn xberg_schema_migration_defaults_to_dry_run_and_requires_explicit_apply() {
    for apply in [false, true] {
        let mut args = vec!["ironrag-maintenance", "migrate", "xberg-recognition-schema"];
        if apply {
            args.push("--apply");
        }
        let cli = Cli::try_parse_from(args).expect("parse Xberg transition");
        assert!(matches!(cli.command,
            Command::Migrate(MigrateCommand::XbergRecognitionSchema { apply: actual, json: false })
            if actual == apply));
    }
}

#[test]
fn accepts_complete_webhook_outbox_audit_cursor() {
    let created_at = chrono::DateTime::UNIX_EPOCH;
    let id = Uuid::nil();

    let cursor = webhook_outbox_audit_cursor(Some(created_at), Some(id));

    assert_eq!(cursor.ok(), Some(Some(WebhookLifecycleOutboxAuditCursor { created_at, id })));
}

#[test]
fn rejects_partial_webhook_outbox_audit_cursor() {
    let created_at = chrono::DateTime::UNIX_EPOCH;

    let error = webhook_outbox_audit_cursor(Some(created_at), None);

    assert!(error.is_err_and(|error| {
        error.to_string() == "--before-created-at and --before-id must be supplied together"
    }));
}

#[test]
fn parses_historical_projection_window_subcommand() {
    let library_id = Uuid::from_u128(7);
    let cli = Cli::try_parse_from([
        "ironrag-maintenance",
        "gc",
        "historical-projection-window",
        "--library",
        &library_id.to_string(),
        "--dry-run",
        "--json",
    ])
    .expect("gc historical-projection-window must parse");

    match cli.command {
        Command::Gc(GcCommand::HistoricalProjectionWindow { library, dry_run, json }) => {
            assert_eq!(library, library_id);
            assert!(dry_run);
            assert!(json);
        }
        _ => panic!("expected gc historical-projection-window"),
    }
}

#[test]
fn uses_instance_scope_when_clearing_a_failure_without_a_library() {
    assert_eq!(scope_for_clear_failure(None), Scope::Instance);
}

#[test]
fn uses_library_scope_when_clearing_a_failure_for_a_library() {
    let library_id = Uuid::nil();

    assert_eq!(scope_for_clear_failure(Some(library_id)), Scope::Library(library_id));
}
