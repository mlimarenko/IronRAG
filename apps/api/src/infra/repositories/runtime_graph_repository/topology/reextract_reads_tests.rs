use super::parse_graph_contribution_count;

#[test]
fn graph_contribution_count_parser_rejects_missing_or_malformed_markers() {
    assert_eq!(parse_graph_contribution_count(Some("0")), Some(0));
    assert_eq!(parse_graph_contribution_count(Some("42")), Some(42));
    assert_eq!(parse_graph_contribution_count(None), None);
    assert_eq!(parse_graph_contribution_count(Some("-1")), None);
    assert_eq!(parse_graph_contribution_count(Some("1.5")), None);
    assert_eq!(parse_graph_contribution_count(Some("unknown")), None);
    let oversized = format!("{}0", usize::MAX);
    assert_eq!(parse_graph_contribution_count(Some(&oversized)), None);
}
