#[path = "support/compliance_scan.rs"]
mod compliance_scan;

use regex::Regex;

const ALLOWED_PREFIXES: &[&str] = &[
    "apps/api/src/services/*/prompts/**",
    "apps/api/src/services/*/i18n/**",
    "apps/api/src/services/*/locales/**",
    "apps/api/tests/**",
];
type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn services_and_interfaces_do_not_embed_cyrillic_copy() -> TestResult {
    let api_root = compliance_scan::api_manifest_dir();
    let scan_roots = [api_root.join("src/services"), api_root.join("src/interfaces")];
    let pattern = Regex::new(r"\p{Cyrillic}+[^\p{Cyrillic}\n]+\p{Cyrillic}+")?;
    let mut findings = Vec::new();

    for root in scan_roots {
        findings.extend(compliance_scan::scan_rust_string_literals(
            &root,
            ALLOWED_PREFIXES,
            &pattern,
        ));
    }

    compliance_scan::print_findings(&findings);
    assert!(findings.is_empty(), "{} violations", findings.len());
    Ok(())
}
