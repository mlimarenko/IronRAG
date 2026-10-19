use super::*;

#[test]
fn table_summary_with_table_row_target_is_not_aggregation_lookup() {
    let row_lookup_ir = retrieve_table_column_inventory_ir("accounts table");
    let summary_ir = QueryIR {
        act: QueryAct::RetrieveValue,
        target_types: vec![crate::domains::query_ir::QueryTargetKind::TableSummary],
        ..describe_query_ir()
    };

    assert!(!question_asks_table_aggregation("", Some(&row_lookup_ir)));
    assert!(question_asks_table_aggregation("", Some(&summary_ir)));
}
