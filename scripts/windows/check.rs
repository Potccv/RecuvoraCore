#![forbid(unsafe_code)]

use std::{
    env,
    ffi::{OsStr, OsString},
    fmt, fs,
    io::ErrorKind,
    os::windows::fs::MetadataExt,
    path::{Component, Path, PathBuf, Prefix},
    process::{self, Command},
};

const HELP: &str = "Core development checks (Windows)
Usage: core-check.exe --build-dir <directory> --temp-root <directory> [--cargo-path <program>]

  --build-dir   Build output outside the Core source directory
  --temp-root   Test data outside the Core source directory
  --cargo-path  Existing Cargo program (default: cargo)
  --help        Show this help

Build and test directories must be separate and have no linked ancestors.";

#[derive(Debug)]
struct Options {
    build_dir: PathBuf,
    temp_root: PathBuf,
    cargo_path: OsString,
}

#[derive(Debug)]
struct CheckError {
    message: String,
    code: i32,
}

impl CheckError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: 1,
        }
    }

    fn exit_code(&self) -> i32 {
        self.code
    }
}

impl fmt::Display for CheckError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

fn parse_options(args: impl Iterator<Item = OsString>) -> Result<Option<Options>, CheckError> {
    let mut args = args.peekable();
    let mut build_dir = None;
    let mut temp_root = None;
    let mut cargo_path = None;
    while let Some(flag) = args.next() {
        if flag == "--help" && build_dir.is_none() && temp_root.is_none() && cargo_path.is_none() {
            if args.peek().is_some() {
                return Err(CheckError::new("--help must be used by itself."));
            }
            return Ok(None);
        }
        let destination = match flag.to_str() {
            Some("--build-dir") => &mut build_dir,
            Some("--temp-root") => &mut temp_root,
            Some("--cargo-path") => &mut cargo_path,
            _ => return Err(CheckError::new(format!("Unknown argument: {flag:?}"))),
        };
        if destination.is_some() {
            return Err(CheckError::new(format!("Duplicate argument: {flag:?}")));
        }
        let value = args
            .next()
            .filter(|value| !value.is_empty() && !value.to_string_lossy().starts_with("--"))
            .ok_or_else(|| CheckError::new(format!("A value is required for {flag:?}.")))?;
        *destination = Some(value);
    }
    Ok(Some(Options {
        build_dir: build_dir
            .map(PathBuf::from)
            .ok_or_else(|| CheckError::new("--build-dir is required."))?,
        temp_root: temp_root
            .map(PathBuf::from)
            .ok_or_else(|| CheckError::new("--temp-root is required."))?,
        cargo_path: cargo_path.unwrap_or_else(|| OsString::from("cargo")),
    }))
}

fn project_root() -> Result<PathBuf, CheckError> {
    let source = Path::new(file!());
    if !source.is_absolute() {
        return Err(CheckError::new(
            "Compile check.rs with an absolute source path; see scripts/windows/README.md.",
        ));
    }
    let project = source
        .parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .ok_or_else(|| CheckError::new("Cannot locate the Core source directory."))?;
    let project = fs::canonicalize(project)
        .map_err(|error| CheckError::new(format!("Cannot resolve the Core source: {error}")))?;
    if !project.join("Cargo.toml").is_file() {
        return Err(CheckError::new("The Core source manifest is missing."));
    }
    Ok(project)
}

fn same_text(left: &OsStr, right: &OsStr) -> bool {
    left.to_string_lossy().to_lowercase() == right.to_string_lossy().to_lowercase()
}

fn same_prefix(left: Prefix<'_>, right: Prefix<'_>) -> bool {
    match (left, right) {
        (
            Prefix::Disk(left) | Prefix::VerbatimDisk(left),
            Prefix::Disk(right) | Prefix::VerbatimDisk(right),
        ) => left.eq_ignore_ascii_case(&right),
        (
            Prefix::UNC(left_server, left_share) | Prefix::VerbatimUNC(left_server, left_share),
            Prefix::UNC(right_server, right_share) | Prefix::VerbatimUNC(right_server, right_share),
        ) => same_text(left_server, right_server) && same_text(left_share, right_share),
        _ => false,
    }
}

fn is_within(path: &Path, directory: &Path) -> bool {
    let mut path = path.components();
    directory
        .components()
        .all(|expected| match (path.next(), expected) {
            (Some(Component::Prefix(actual)), Component::Prefix(expected)) => {
                same_prefix(actual.kind(), expected.kind())
            }
            (Some(Component::Normal(actual)), Component::Normal(expected)) => {
                same_text(actual, expected)
            }
            (Some(actual), expected) => actual == expected,
            _ => false,
        })
}

fn regular_directory(path: &Path) -> Result<PathBuf, CheckError> {
    let mut components = path.components();
    let mut regular = match components.next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::VerbatimDisk(drive) => PathBuf::from(format!("{}:\\", char::from(drive))),
            Prefix::VerbatimUNC(server, share) => {
                let mut root = OsString::from(r"\\");
                root.push(server);
                root.push(r"\");
                root.push(share);
                PathBuf::from(root)
            }
            Prefix::Disk(_) | Prefix::UNC(_, _) => return Ok(path.to_path_buf()),
            _ => return Err(CheckError::new("Unsupported output directory prefix.")),
        },
        _ => return Err(CheckError::new("An absolute output directory is required.")),
    };
    for component in components {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => regular.push(name),
            _ => return Err(CheckError::new("Unsupported output directory component.")),
        }
    }
    Ok(regular)
}

fn external_directory(value: &Path, project: &Path) -> Result<PathBuf, CheckError> {
    if value.as_os_str().to_string_lossy().trim().is_empty() {
        return Err(CheckError::new("An external directory is required."));
    }
    let absolute = std::path::absolute(value).map_err(|error| {
        CheckError::new(format!(
            "Cannot resolve directory {}: {error}",
            value.display()
        ))
    })?;
    if !matches!(
        absolute.components().next(),
        Some(Component::Prefix(prefix))
            if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::UNC(_, _))
    ) {
        return Err(CheckError::new(
            "Use a regular drive or UNC directory path without a device prefix.",
        ));
    }
    let mut existing = None;
    for ancestor in absolute.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) => {
                if metadata.file_attributes() & 0x400 != 0 {
                    return Err(CheckError::new(format!(
                        "Linked output directory or ancestor is not supported: {}",
                        ancestor.display()
                    )));
                }
                if !metadata.is_dir() {
                    return Err(CheckError::new(format!(
                        "Output directory or ancestor is not a directory: {}",
                        ancestor.display()
                    )));
                }
                existing.get_or_insert(ancestor);
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                return Err(CheckError::new(format!(
                    "Cannot inspect output directory {}: {error}",
                    ancestor.display()
                )));
            }
        }
    }
    let existing = existing.ok_or_else(|| CheckError::new("No existing parent directory."))?;
    let suffix = absolute
        .strip_prefix(existing)
        .map_err(|error| CheckError::new(format!("Cannot resolve directory suffix: {error}")))?;
    let resolved = fs::canonicalize(existing)
        .map_err(|error| CheckError::new(format!("Cannot resolve output ancestor: {error}")))?
        .join(suffix);
    if is_within(&resolved, project) {
        return Err(CheckError::new(format!(
            "Build and test data must be outside the Core source directory: {}",
            resolved.display()
        )));
    }
    // Cargo's external Windows linkers need ordinary drive/UNC paths.
    regular_directory(&resolved)
}

fn run_checks(options: Options, project: &Path) -> Result<(), CheckError> {
    // Validate both paths before creating either directory or starting Cargo.
    let build_dir = external_directory(&options.build_dir, project)?;
    let temp_root = external_directory(&options.temp_root, project)?;
    if is_within(&build_dir, &temp_root) || is_within(&temp_root, &build_dir) {
        return Err(CheckError::new(
            "Build and test directories must not be the same or contain one another.",
        ));
    }
    let cargo_path = if Path::new(&options.cargo_path).components().count() > 1 {
        std::path::absolute(&options.cargo_path)
            .map_err(|error| CheckError::new(format!("Cannot resolve Cargo path: {error}")))?
            .into_os_string()
    } else {
        options.cargo_path
    };
    for directory in [&build_dir, &temp_root] {
        fs::create_dir_all(directory).map_err(|error| {
            CheckError::new(format!(
                "Cannot create directory {}: {error}",
                directory.display()
            ))
        })?;
    }
    let checks: &[(&str, &[&str])] = &[
        ("Rust formatting", &["fmt", "--all", "--", "--check"]),
        (
            "Clippy",
            &[
                "clippy",
                "--all-targets",
                "--locked",
                "--",
                "-D",
                "warnings",
            ],
        ),
        ("Core package tests", &["test", "--all-targets", "--locked"]),
        ("Core documentation tests", &["test", "--doc", "--locked"]),
    ];
    for (name, args) in checks {
        println!("Running {name}...");
        let status = Command::new(&cargo_path)
            .args(*args)
            .current_dir(project)
            .env("CARGO_TARGET_DIR", &build_dir)
            .env("RECUVORA_TEST_TEMP", &temp_root)
            .env("RUST_TEST_THREADS", "4")
            .status()
            .map_err(|error| CheckError::new(format!("Cannot start Cargo for {name}: {error}")))?;
        if !status.success() {
            let code = status.code().unwrap_or(1);
            return Err(CheckError {
                message: format!("{name} failed (exit code {code})."),
                code,
            });
        }
    }
    Ok(())
}

fn main() {
    let result = parse_options(env::args_os().skip(1)).and_then(|options| {
        let Some(options) = options else {
            println!("{HELP}");
            return Ok(());
        };
        run_checks(options, &project_root()?)?;
        println!("All Core checks passed.");
        Ok(())
    });
    if let Err(error) = result {
        eprintln!("{error}");
        process::exit(error.exit_code());
    }
}

#[cfg(test)]
#[path = "../../tests/windows_check.rs"]
mod tests;
