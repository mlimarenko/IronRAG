use super::*;

#[test]
fn narrative_numbered_steps_are_not_mistaken_for_commands() {
    // A plain-word step is prose, not an invocation, however it is marked.
    assert!(!line_has_command_signal("1. Deploy the service"));
    assert!(!line_has_command_signal("2. Restart the worker"));

    // A marked step whose head carries executable name shape still counts.
    assert!(line_has_command_signal("1. apt-get install alpha"));
    assert!(line_has_command_signal("- ./install.sh --force"));
}

#[test]
fn dense_procedure_line_does_not_split_url_or_parent_relative_path() {
    assert_eq!(
        split_dense_procedure_line(
            "sample-transfer https://packages.example.invalidsample-update-token -o ../update-node.sh"
        ),
        vec![
            "sample-transfer https://packages.example.invalidsample-update-token -o ../update-node.sh"
        ]
    );
}

#[test]
fn dense_procedure_line_splits_local_script_after_file_mode_command() {
    assert_eq!(
        split_dense_procedure_line("sample-prepare +x sample-update-token sample-update-token"),
        vec!["sample-prepare +x sample-update-token", "sample-update-token"]
    );
}

#[test]
fn dense_procedure_line_preserves_ordered_command_arguments() {
    for line in
        ["1. alpha-admin prepare --target=/srv/alpha", "2. alpha-admin switch --version=2.0.0"]
    {
        assert_eq!(split_dense_procedure_line(line), vec![line]);
    }
}

#[test]
fn command_shape_does_not_depend_on_executable_spelling() {
    for executable in ["alpha-tool", "beta_tool", "./gamma-tool"] {
        assert!(line_has_command_signal(&format!("{executable} --mode=strict /work/update-token")));
    }
}

#[test]
fn command_signal_rejects_configuration_sections_and_hyphenated_sentences() {
    assert!(!line_has_command_signal("[Sample2]"));
    assert!(!line_has_command_signal("QR-code is not performed automatically."));
    assert!(line_has_command_signal("sample-runner --configure connector"));
    assert!(line_has_command_signal("SampleRunner --configure connector"));
    assert!(line_has_command_signal("工具 --configure connector"));
}
