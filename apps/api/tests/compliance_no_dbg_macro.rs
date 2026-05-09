#[path = "support/compliance_scan.rs"]
mod compliance_scan;

use regex::Regex;

const EXCLUSIONS: &[&str] = &[];
type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn api_source_and_tests_do_not_use_dbg_macro() -> TestResult {
    let api_root = compliance_scan::api_manifest_dir();
    let scan_roots = [api_root.join("src"), api_root.join("tests")];
    let pattern = Regex::new(r"\bdbg!\s*\(")?;
    let mut findings = Vec::new();

    for root in scan_roots {
        findings.extend(compliance_scan::scan(&root, EXCLUSIONS, &pattern));
    }

    compliance_scan::print_findings(&findings);
    assert!(findings.is_empty(), "{} violations", findings.len());
    Ok(())
}
