use super::{CheckError, Options, parse_options, project_root, run_checks};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    path: PathBuf,
    root: PathBuf,
    project: PathBuf,
    cargo: PathBuf,
    records: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let project = PathBuf::from(command_path(
            &project_root().expect("the check script identifies its project"),
        ));
        let root = PathBuf::from(
            std::env::var_os("RECUVORA_TEST_TEMP")
                .expect("RECUVORA_TEST_TEMP must name an external directory"),
        );
        assert!(root.is_absolute());
        let resolved_root = root.canonicalize().expect("the test root already exists");
        assert!(!resolved_root.starts_with(project.canonicalize().unwrap()));
        let root = PathBuf::from(command_path(&resolved_root));
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = root.join(format!(
            "windows-check-{}-{stamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        let records = path.join("records");
        fs::create_dir(&records).unwrap();
        let cargo = path.join("Cargo 替身.exe");
        let fixture = Self {
            path,
            root,
            project,
            cargo,
            records,
        };
        let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| OsString::from("rustc"));
        let status = Command::new(rustc)
            .arg("--edition=2024")
            .args(["-D", "warnings"])
            .arg(fixture.project.join("tests/windows_check_cargo.rs"))
            .arg("-o")
            .arg(&fixture.cargo)
            .env("RECUVORA_CHECK_STUB_RECORD", &fixture.records)
            .current_dir(&fixture.project)
            .status()
            .expect("start rustc for the Cargo substitute");
        assert!(status.success(), "compile the Cargo substitute: {status}");
        fixture
    }

    fn options(&self, build_dir: &Path, temp_root: &Path) -> Options {
        Options {
            build_dir: build_dir.to_path_buf(),
            temp_root: temp_root.to_path_buf(),
            cargo_path: self.cargo.clone().into_os_string(),
        }
    }

    fn calls(&self) -> Vec<Vec<String>> {
        let mut calls = Vec::new();
        for index in 1.. {
            let record = self.records.join(format!("cargo-call-{index}.txt"));
            if !record.exists() {
                break;
            }
            calls.push(
                fs::read_to_string(record)
                    .unwrap()
                    .lines()
                    .map(str::to_owned)
                    .collect(),
            );
        }
        calls
    }

    fn reset_calls(&self) {
        for index in 1..=self.calls().len() {
            fs::remove_file(self.records.join(format!("cargo-call-{index}.txt"))).unwrap();
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        assert_eq!(self.path.parent(), Some(self.root.as_path()));
        if let Err(error) = fs::remove_dir_all(&self.path) {
            if std::thread::panicking() {
                eprintln!("cannot clean this test's directory: {error}");
            } else {
                panic!("cannot clean this test's directory: {error}");
            }
        }
    }
}

fn environment() -> BTreeMap<OsString, OsString> {
    std::env::vars_os().collect()
}

fn command_arguments() -> Vec<Vec<OsString>> {
    [
        vec!["fmt", "--all", "--", "--check"],
        vec![
            "clippy",
            "--all-targets",
            "--locked",
            "--",
            "-D",
            "warnings",
        ],
        vec!["test", "--all-targets", "--locked"],
        vec!["test", "--doc", "--locked"],
    ]
    .into_iter()
    .map(|arguments| arguments.into_iter().map(OsString::from).collect())
    .collect()
}

#[test]
fn runs_all_checks_with_isolated_child_environment_and_unicode_paths() {
    let fixture = Fixture::new();
    let build = fixture.path.join("编译输出 with spaces");
    let temp = fixture.path.join("测试数据 with spaces");
    let before = environment();
    let inherited_path = std::env::var_os("PATH");
    let inherited_rustc = std::env::var_os("RUSTC");
    run_checks(fixture.options(&build, &temp), &fixture.project).unwrap();
    assert_eq!(
        environment(),
        before,
        "child overrides leave caller environment intact"
    );
    let calls = fixture.calls();
    assert_eq!(calls.len(), 4);
    for (call, expected) in calls.iter().zip(command_arguments()) {
        assert_eq!(call.len(), 7);
        assert_eq!(call[0], format!("{expected:?}"));
        assert_eq!(
            Path::new(&call[1]).canonicalize().unwrap(),
            fixture.project.canonicalize().unwrap()
        );
        assert_eq!(
            Path::new(&call[2]).canonicalize().unwrap(),
            build.canonicalize().unwrap()
        );
        assert_eq!(
            Path::new(&call[3]).canonicalize().unwrap(),
            temp.canonicalize().unwrap()
        );
        assert!(
            !call[2].starts_with(r"\\?\"),
            "Cargo receives a regular build path"
        );
        assert!(
            !call[3].starts_with(r"\\?\"),
            "tests receive a regular temporary path"
        );
        assert_eq!(call[4], "4");
        assert_eq!(call[5], format!("{inherited_path:?}"));
        assert_eq!(call[6], format!("{inherited_rustc:?}"));
    }
}

#[test]
fn validates_cli_arguments_and_reports_an_unavailable_cargo() {
    let parse = |arguments: &[&str]| {
        parse_options(arguments.iter().map(|argument| OsString::from(*argument)))
    };
    assert!(parse(&["--help"]).unwrap().is_none());
    let valid = parse(&["--build-dir", "build", "--temp-root", "temp"])
        .unwrap()
        .unwrap();
    assert_eq!(valid.build_dir, PathBuf::from("build"));
    assert_eq!(valid.temp_root, PathBuf::from("temp"));
    assert_eq!(valid.cargo_path, OsString::from("cargo"));
    for invalid in [
        vec![],
        vec!["--unknown"],
        vec!["--build-dir"],
        vec!["--build-dir", "build"],
        vec!["--build-dir", "build", "--temp-root"],
        vec!["--build-dir", "", "--temp-root", "temp"],
        vec![
            "--build-dir",
            "build",
            "--temp-root",
            "temp",
            "--cargo-path",
        ],
        vec![
            "--build-dir",
            "build",
            "--build-dir",
            "again",
            "--temp-root",
            "temp",
        ],
        vec!["--help", "--build-dir", "build"],
    ] {
        let error = parse(&invalid).unwrap_err();
        assert_ne!(error.exit_code(), 0, "invalid CLI arguments: {invalid:?}");
    }

    let fixture = Fixture::new();
    let build = fixture.path.join("build without Cargo");
    let temp = fixture.path.join("temp without Cargo");
    let mut options = fixture.options(&build, &temp);
    options.cargo_path = fixture.path.join("missing cargo.exe").into_os_string();
    let before = environment();
    let error = run_checks(options, &fixture.project).unwrap_err();
    assert_ne!(error.exit_code(), 0);
    assert!(error.to_string().starts_with("Cannot start Cargo"));
    assert!(fixture.calls().is_empty());
    assert_eq!(environment(), before);
}

#[test]
fn stops_at_every_failed_check_and_preserves_its_exit_code() {
    let fixture = Fixture::new();
    let before = environment();
    for failed in 1..=4 {
        fixture.reset_calls();
        fs::write(fixture.records.join("fail-at.txt"), failed.to_string()).unwrap();
        let build = fixture.path.join(format!("build-{failed}"));
        let temp = fixture.path.join(format!("temp-{failed}"));
        let error = run_checks(fixture.options(&build, &temp), &fixture.project).unwrap_err();
        assert_eq!(error.exit_code(), 30 + failed);
        let calls = fixture.calls();
        assert_eq!(calls.len(), failed as usize);
        for (call, expected) in calls.iter().zip(command_arguments()) {
            assert_eq!(call[0], format!("{expected:?}"));
        }
        assert_eq!(environment(), before);
    }
}

fn assert_rejected_before_creation(fixture: &Fixture, build: &Path, temp: &Path) {
    let build_existed = build.exists();
    let temp_existed = temp.exists();
    let result: Result<(), CheckError> = run_checks(fixture.options(build, temp), &fixture.project);
    assert!(result.is_err(), "reject build={build:?}, temp={temp:?}");
    assert_ne!(result.unwrap_err().exit_code(), 0);
    assert!(
        fixture.calls().is_empty(),
        "invalid paths must not start Cargo"
    );
    assert_eq!(
        build.exists(),
        build_existed,
        "do not create the build directory"
    );
    assert_eq!(
        temp.exists(),
        temp_existed,
        "do not create the test directory"
    );
}

fn command_path(path: &Path) -> OsString {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}").into()
    } else {
        text.strip_prefix(r"\\?\").unwrap_or(&text).into()
    }
}

#[test]
fn rejects_source_overlapping_and_junction_paths_before_any_output_is_created() {
    let fixture = Fixture::new();
    let external = fixture.path.join("untouched external directory");
    let source_child = fixture.project.join(format!(
        "check-script-rejected-output-{}",
        std::process::id()
    ));
    assert!(!source_child.exists());
    assert_rejected_before_creation(&fixture, &source_child, &external);
    assert_rejected_before_creation(&fixture, &external, &source_child);
    assert_rejected_before_creation(&fixture, &external, &external);
    assert_rejected_before_creation(&fixture, &external, &external.join("nested"));
    assert_rejected_before_creation(&fixture, &external.join("nested"), &external);
    let source_with_parent = fixture.project.join("scripts").join("..").join("output");
    assert_rejected_before_creation(&fixture, &source_with_parent, &external);

    let destination = fixture.path.join("actual directory");
    fs::create_dir(&destination).unwrap();
    let junction = fixture.path.join("directory junction");
    let status = Command::new("cmd")
        .args(["/D", "/C", "mklink", "/J"])
        .arg(command_path(&junction))
        .arg(command_path(&destination))
        .status()
        .expect("create a junction for path-boundary verification");
    assert!(status.success(), "junction creation failed: {status}");
    assert_rejected_before_creation(&fixture, &junction.join("build"), &external);
    assert_rejected_before_creation(&fixture, &external, &junction.join("temp"));
    fs::remove_dir(&junction).unwrap();
    assert!(
        destination.exists(),
        "removing the junction retains its destination"
    );
}
