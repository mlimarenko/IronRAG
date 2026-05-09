#[path = "support/compliance_scan.rs"]
mod compliance_scan;

use regex::Regex;

const ALLOWED_PREFIXES: &[&str] =
    &["apps/web/src/shared/api/generated/**", "docs/perf/**", "docs/constitution-audit-*.md"];
type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn source_docs_and_scripts_do_not_reference_prod_dataset_names() -> TestResult {
    let repo_root = compliance_scan::workspace_root();
    let scan_roots = [
        repo_root.join("apps/api/src"),
        repo_root.join("apps/web/src"),
        repo_root.join("scripts"),
        repo_root.join("docs"),
    ];
    let pattern = Regex::new(
        r"\bArtix\b|\b(?:SberSbp|AlfaSbp|Tinkoff|Sber|Alfa)\d*\b|pxm-ironrag\d+|graph\.piping\.space",
    )?;
    let mut findings = Vec::new();

    for root in scan_roots {
        findings.extend(compliance_scan::scan(&root, ALLOWED_PREFIXES, &pattern));
    }

    compliance_scan::print_findings(&findings);
    assert!(findings.is_empty(), "{} violations", findings.len());
    Ok(())
}
