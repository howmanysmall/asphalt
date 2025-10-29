use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use tempfile::tempdir;

#[test]
fn test_convert_command_success() {
    let temp_dir = tempdir().unwrap();
    let input_path = temp_dir.path().join("old_config.toml");
    let output_path = temp_dir.path().join("new_config.toml");

    // Create a minimal old-format config (flat pack options)
    let old_config = r#"
[creator]
type = "user"
id = 123

[codegen]
typescript = true

[inputs.assets]
path = "assets/**/*"
output_path = "src/shared"

[inputs.assets.pack]
enabled = true
max_size = [1024, 1024]
power_of_two = false
padding = 4
extrude = 2
allow_trim = true
algorithm = "max_rects"
page_limit = 5
sort = "max_side"
dedupe = true
"#;

    fs::write(&input_path, old_config).unwrap();

    // Run the convert command
    let mut cmd = Command::from_std(std::process::Command::new(env!("CARGO_BIN_EXE_asphalt")));
    cmd.arg("convert")
        .arg("--input")
        .arg(&input_path)
        .arg("--output")
        .arg(&output_path);

    cmd.assert().success();

    // Check that output file was created
    assert!(output_path.exists());

    // Optionally, check that the output contains expected new format
    let output_content = fs::read_to_string(&output_path).unwrap();
    assert!(output_content.contains("type = \"static\""));
    assert!(output_content.contains("max_size = ["));
    assert!(output_content.contains("1024"));
}

#[test]
fn test_convert_command_dry_run() {
    let temp_dir = tempdir().unwrap();
    let input_path = temp_dir.path().join("old_config.toml");

    // Create a minimal old-format config
    let old_config = r#"
[creator]
type = "user"
id = 123

[inputs.assets]
path = "assets/**/*"
output_path = "src/shared"

[inputs.assets.pack]
enabled = true
padding = 2
"#;

    fs::write(&input_path, old_config).unwrap();

    // Run the convert command with --dry-run
    let mut cmd = Command::from_std(std::process::Command::new(env!("CARGO_BIN_EXE_asphalt")));
    cmd.arg("convert")
        .arg("--input")
        .arg(&input_path)
        .arg("--dry-run");

    cmd.assert()
        .success()
        .stdout(predicate::str::contains("=== Dry run: would write to"))
        .stdout(predicate::str::contains(
            "Flat pack options converted to PackMode::Static",
        ));
}
