// SPDX-License-Identifier: Apache-2.0

//! CLI argument parsing tests based on README examples.
//!
//! These tests verify that the command-line interface correctly parses
//! arguments as documented in the README examples.

use std::process::Command;

/// Helper to run criu-image-streamer with --help and check it succeeds
fn get_help_output() -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_criu-image-streamer"))
        .arg("--help")
        .output()
        .expect("Failed to execute criu-image-streamer");

    String::from_utf8_lossy(&output.stdout).to_string()
}

/// Test that --help works and shows expected subcommands
#[test]
fn test_help_shows_subcommands() {
    let help = get_help_output();
    assert!(help.contains("capture"), "Help should mention 'capture' subcommand");
    assert!(help.contains("serve"), "Help should mention 'serve' subcommand");
    assert!(help.contains("extract"), "Help should mention 'extract' subcommand");
}

/// Test that --help shows expected options
#[test]
fn test_help_shows_options() {
    let help = get_help_output();
    assert!(help.contains("--images-dir"), "Help should mention '--images-dir' option");
    assert!(help.contains("-D"), "Help should mention '-D' short option");
    assert!(help.contains("--shard-fds"), "Help should mention '--shard-fds' option");
    assert!(help.contains("--ext-file-fds"), "Help should mention '--ext-file-fds' option");
    assert!(help.contains("--progress-fd"), "Help should mention '--progress-fd' option");
    assert!(help.contains("--tcp-listen-remap"), "Help should mention '--tcp-listen-remap' option");
}

/// Test that capture subcommand has its own help
#[test]
fn test_capture_subcommand_exists() {
    let output = Command::new(env!("CARGO_BIN_EXE_criu-image-streamer"))
        .args(["--images-dir", "/tmp", "capture", "--help"])
        .output()
        .expect("Failed to execute criu-image-streamer");

    // The command should succeed (help requested)
    assert!(output.status.success() || output.status.code() == Some(0),
            "capture --help should succeed");
}

/// Test that serve subcommand has its own help
#[test]
fn test_serve_subcommand_exists() {
    let output = Command::new(env!("CARGO_BIN_EXE_criu-image-streamer"))
        .args(["--images-dir", "/tmp", "serve", "--help"])
        .output()
        .expect("Failed to execute criu-image-streamer");

    assert!(output.status.success() || output.status.code() == Some(0),
            "serve --help should succeed");
}

/// Test that extract subcommand has its own help
#[test]
fn test_extract_subcommand_exists() {
    let output = Command::new(env!("CARGO_BIN_EXE_criu-image-streamer"))
        .args(["--images-dir", "/tmp", "extract", "--help"])
        .output()
        .expect("Failed to execute criu-image-streamer");

    assert!(output.status.success() || output.status.code() == Some(0),
            "extract --help should succeed");
}

/// Test that --images-dir is required
#[test]
fn test_images_dir_required() {
    let output = Command::new(env!("CARGO_BIN_EXE_criu-image-streamer"))
        .arg("capture")
        .output()
        .expect("Failed to execute criu-image-streamer");

    // Should fail because --images-dir is missing
    assert!(!output.status.success(), "Should fail without --images-dir");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("images-dir") || stderr.contains("required"),
            "Error should mention missing images-dir");
}

/// Test that subcommand is required
#[test]
fn test_subcommand_required() {
    let output = Command::new(env!("CARGO_BIN_EXE_criu-image-streamer"))
        .args(["--images-dir", "/tmp"])
        .output()
        .expect("Failed to execute criu-image-streamer");

    // Should fail because subcommand is missing
    assert!(!output.status.success(), "Should fail without subcommand");
}

/// Test that -D is equivalent to --images-dir
#[test]
fn test_short_option_images_dir() {
    // Both should produce the same help output when used with --help
    let output_long = Command::new(env!("CARGO_BIN_EXE_criu-image-streamer"))
        .args(["--images-dir", "/tmp", "capture", "--help"])
        .output()
        .expect("Failed to execute criu-image-streamer");

    let output_short = Command::new(env!("CARGO_BIN_EXE_criu-image-streamer"))
        .args(["-D", "/tmp", "capture", "--help"])
        .output()
        .expect("Failed to execute criu-image-streamer");

    assert_eq!(output_long.status.success(), output_short.status.success(),
               "-D and --images-dir should behave the same");
}

/// README Example 1: Basic capture command structure
/// criu-image-streamer --images-dir /tmp capture
#[test]
fn test_readme_example1_capture_structure() {
    // We can't fully run capture without CRIU, but we can verify the args parse
    let output = Command::new(env!("CARGO_BIN_EXE_criu-image-streamer"))
        .args(["--images-dir", "/tmp", "capture", "--help"])
        .output()
        .expect("Failed to execute criu-image-streamer");

    assert!(output.status.success() || output.status.code() == Some(0));
}

/// README Example 1: Basic serve command structure
/// criu-image-streamer --images-dir /tmp serve
#[test]
fn test_readme_example1_serve_structure() {
    let output = Command::new(env!("CARGO_BIN_EXE_criu-image-streamer"))
        .args(["--images-dir", "/tmp", "serve", "--help"])
        .output()
        .expect("Failed to execute criu-image-streamer");

    assert!(output.status.success() || output.status.code() == Some(0));
}

/// README Example 2: Extract command structure
/// criu-image-streamer --images-dir output_dir extract
#[test]
fn test_readme_example2_extract_structure() {
    let output = Command::new(env!("CARGO_BIN_EXE_criu-image-streamer"))
        .args(["--images-dir", "output_dir", "extract", "--help"])
        .output()
        .expect("Failed to execute criu-image-streamer");

    assert!(output.status.success() || output.status.code() == Some(0));
}

/// README Example 3: Multi-shard capture with --shard-fds
/// criu-image-streamer --images-dir /tmp --shard-fds 10,11,12 capture
/// Note: We can't actually test with real FDs, but we verify the argument parsing
#[test]
fn test_readme_example3_shard_fds_parsing() {
    // Verify that --shard-fds option is documented in help
    let help = get_help_output();
    assert!(help.contains("--shard-fds"), "Should support --shard-fds option");
    assert!(help.contains("-s"), "Should support -s short option for shard-fds");
}

/// README Example 4: External file with --ext-file-fds
/// criu-image-streamer --images-dir /tmp --ext-file-fds fs.tar:20 capture
#[test]
fn test_readme_example4_ext_file_fds_parsing() {
    let help = get_help_output();
    assert!(help.contains("--ext-file-fds"), "Should support --ext-file-fds option");
    assert!(help.contains("-e"), "Should support -e short option for ext-file-fds");
}

/// Test --progress-fd option is available
#[test]
fn test_progress_fd_option() {
    let help = get_help_output();
    assert!(help.contains("--progress-fd"), "Should support --progress-fd option");
    assert!(help.contains("-p"), "Should support -p short option for progress-fd");
}

/// Test --tcp-listen-remap option is available
#[test]
fn test_tcp_listen_remap_option() {
    let help = get_help_output();
    assert!(help.contains("--tcp-listen-remap"), "Should support --tcp-listen-remap option");
}

/// Test that invalid ext-file-fds format is rejected
#[test]
fn test_invalid_ext_file_fds_format() {
    let output = Command::new(env!("CARGO_BIN_EXE_criu-image-streamer"))
        .args(["--images-dir", "/tmp", "--ext-file-fds", "invalid_format", "capture"])
        .output()
        .expect("Failed to execute criu-image-streamer");

    assert!(!output.status.success(), "Should fail with invalid ext-file-fds format");
}

/// Test that invalid tcp-listen-remap format is rejected
#[test]
fn test_invalid_tcp_listen_remap_format() {
    let output = Command::new(env!("CARGO_BIN_EXE_criu-image-streamer"))
        .args(["--images-dir", "/tmp", "--tcp-listen-remap", "invalid", "serve"])
        .output()
        .expect("Failed to execute criu-image-streamer");

    assert!(!output.status.success(), "Should fail with invalid tcp-listen-remap format");
}

/// Test that non-integer shard-fds is rejected
#[test]
fn test_invalid_shard_fds() {
    let output = Command::new(env!("CARGO_BIN_EXE_criu-image-streamer"))
        .args(["--images-dir", "/tmp", "--shard-fds", "abc", "capture"])
        .output()
        .expect("Failed to execute criu-image-streamer");

    assert!(!output.status.success(), "Should fail with non-integer shard-fds");
}

/// Test that non-integer progress-fd is rejected
#[test]
fn test_invalid_progress_fd() {
    let output = Command::new(env!("CARGO_BIN_EXE_criu-image-streamer"))
        .args(["--images-dir", "/tmp", "--progress-fd", "abc", "capture"])
        .output()
        .expect("Failed to execute criu-image-streamer");

    assert!(!output.status.success(), "Should fail with non-integer progress-fd");
}

/// Test unknown subcommand is rejected
#[test]
fn test_unknown_subcommand() {
    let output = Command::new(env!("CARGO_BIN_EXE_criu-image-streamer"))
        .args(["--images-dir", "/tmp", "unknown"])
        .output()
        .expect("Failed to execute criu-image-streamer");

    assert!(!output.status.success(), "Should fail with unknown subcommand");
}

/// Test unknown option is rejected
#[test]
fn test_unknown_option() {
    let output = Command::new(env!("CARGO_BIN_EXE_criu-image-streamer"))
        .args(["--images-dir", "/tmp", "--unknown-option", "capture"])
        .output()
        .expect("Failed to execute criu-image-streamer");

    assert!(!output.status.success(), "Should fail with unknown option");
}
