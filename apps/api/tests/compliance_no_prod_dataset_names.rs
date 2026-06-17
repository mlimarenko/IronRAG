#[path = "support/compliance_scan.rs"]
mod compliance_scan;

use regex::Regex;

const ALLOWED_PREFIXES: &[&str] =
    &["apps/web/src/shared/api/generated/**", "docs/perf/**", "docs/constitution-audit-*.md"];
type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn source_docs_and_scripts_do_not_reference_configured_forbidden_terms() -> TestResult {
    let Ok(pattern_text) = std::env::var("IRONRAG_COMPLIANCE_FORBIDDEN_PATTERN") else {
        return Ok(());
    };
    let pattern_text = pattern_text.trim();
    if pattern_text.is_empty() {
        return Ok(());
    }
    let repo_root = compliance_scan::workspace_root();
    let scan_roots = [
        repo_root.join("apps/api/src"),
        repo_root.join("apps/web/src"),
        repo_root.join("scripts"),
        repo_root.join("docs"),
    ];
    let pattern = Regex::new(pattern_text)?;
    let mut findings = Vec::new();

    for root in scan_roots {
        findings.extend(compliance_scan::scan(&root, ALLOWED_PREFIXES, &pattern));
    }

    compliance_scan::print_findings(&findings);
    assert!(findings.is_empty(), "{} violations", findings.len());
    Ok(())
}
