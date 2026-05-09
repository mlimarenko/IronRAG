#[path = "support/compliance_scan.rs"]
mod compliance_scan;

use regex::Regex;

const EXCLUSIONS: &[&str] = &[];
type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn http_handlers_do_not_panic_unwrap_or_expect() -> TestResult {
    let api_root = compliance_scan::api_manifest_dir();
    let handler_roots =
        [api_root.join("src/interfaces/http.rs"), api_root.join("src/interfaces/http")];
    let pattern = Regex::new(r"\bpanic!\s*\(|\.unwrap\s*\(\s*\)|\.expect\s*\(")?;
    let mut findings = Vec::new();

    for root in handler_roots {
        findings.extend(compliance_scan::scan_outside_cfg_test_blocks(&root, EXCLUSIONS, &pattern));
    }

    compliance_scan::print_findings(&findings);
    assert!(findings.is_empty(), "{} violations", findings.len());
    Ok(())
}
