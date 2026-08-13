//! The installer has to declare the same version the workspace does, and that
//! check has to survive either line ending.
//!
//! Issue #50: `installer/setup.iss` is stored with LF, and a Windows checkout
//! with the default `core.autocrlf=true` -- this machine and the GitHub Actions
//! Windows runners both -- hands the working tree CRLF. The packaging scripts
//! locate the declaration with a `(?m)^...$`-anchored regex, and in .NET's
//! multiline mode `$` matches immediately before the `\n`, so a `\r` between
//! the closing quote and the anchor makes the pattern find nothing. The gate
//! then answers according to how the tree was checked out rather than to what
//! the file declares.
//!
//! These checks read the declarations directly, tolerant of both endings, so a
//! real mismatch fails in `cargo test --workspace` instead of on whichever
//! machine runs the installer build. They also assert the scripts' own anchors
//! still allow the carriage return, which is the defect itself.

use std::path::{Path, PathBuf};

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(relative: &str) -> String {
    let path = repository_root().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {relative}: {error}"))
}

/// Lines without their line ending, whichever ending the checkout produced.
fn lines(text: &str) -> Vec<&str> {
    text.split('\n')
        .map(|line| line.trim_end_matches('\r'))
        .collect()
}

/// The single value of a `#define <name> "<value>"` directive.
fn defined(text: &str, name: &str) -> String {
    let prefix = format!("#define {name} \"");
    let mut found: Vec<&str> = Vec::new();
    for line in lines(text) {
        if let Some(rest) = line.strip_prefix(&prefix) {
            let value = rest
                .strip_suffix('"')
                .unwrap_or_else(|| panic!("#define {name} is not a closed string: {line:?}"));
            found.push(value);
        }
    }
    assert_eq!(
        found.len(),
        1,
        "expected exactly one #define {name} in setup.iss, found {}",
        found.len()
    );
    found[0].to_owned()
}

/// The `[workspace.package]` version, which every crate inherits.
fn workspace_version() -> String {
    let cargo = read("Cargo.toml");
    let mut in_workspace_package = false;
    for line in lines(&cargo) {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_workspace_package = trimmed == "[workspace.package]";
            continue;
        }
        if !in_workspace_package {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("version") {
            let value = rest
                .trim_start()
                .strip_prefix('=')
                .expect("workspace version is not an assignment")
                .trim()
                .trim_matches('"');
            return value.to_owned();
        }
    }
    panic!("Cargo.toml has no [workspace.package] version");
}

#[test]
fn the_installer_declares_the_workspace_version() {
    let version = workspace_version();
    let parts: Vec<&str> = version.split('.').collect();
    assert_eq!(
        parts.len(),
        3,
        "workspace version is not canonical: {version}"
    );
    for part in &parts {
        assert!(
            !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()),
            "workspace version is not canonical: {version}"
        );
    }

    let setup = read("installer/setup.iss");
    assert_eq!(
        defined(&setup, "AppProductVersion"),
        version,
        "installer AppProductVersion does not match the workspace version"
    );
}

#[test]
fn the_default_payload_directory_carries_the_same_version() {
    let version = workspace_version();
    let setup = read("installer/setup.iss");
    // The release pipeline overrides AppVersionedDir with the build id; the
    // default is what a local build installs under, so it has to move with the
    // product version rather than pointing at the previous release's payload.
    assert_eq!(
        defined(&setup, "AppVersionedDir"),
        format!("{{app}}\\versions\\{version}-dev"),
        "the default versioned payload directory does not match the workspace version"
    );
}

/// The regression guard for Issue #50 itself. Both gates are fail-closed, so a
/// missing `\r?` costs a refused build rather than a mislabelled installer --
/// but it refuses on a correct tree, which is worse than useless.
#[test]
fn every_packaging_version_gate_allows_a_carriage_return() {
    let gates: &[(&str, &str)] = &[
        ("scripts/build-installer.ps1", "^#define AppProductVersion "),
        (
            ".github/workflows/release.yml",
            "^#define AppProductVersion ",
        ),
        (".github/workflows/release.yml", "^version = "),
    ];
    for (file, anchor) in gates {
        let text = read(file);
        let matching: Vec<&str> = lines(&text)
            .into_iter()
            .filter(|line| line.contains(anchor))
            .collect();
        assert!(
            !matching.is_empty(),
            "{file} no longer anchors a version gate on {anchor:?}; \
             update this guard along with the gate"
        );
        for line in matching {
            assert!(
                line.contains("\\r?$"),
                "{file} anchors a version gate without allowing the carriage \
                 return a core.autocrlf=true checkout produces: {line:?}"
            );
        }
    }
}
