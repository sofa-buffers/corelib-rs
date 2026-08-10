//! Regression tests for `coverage.sh` (CORELIB_PLAN §12.1 tooling).
//!
//! The instrumented test suite is the expensive part of a coverage run, and it
//! only has to happen once: every rendering (HTML, text summary, LCOV) is
//! produced by `cargo llvm-cov report`, which replays the profile data left
//! behind by a single `cargo llvm-cov --no-report` run. A *bare* `cargo
//! llvm-cov …` (`--html`, `--summary-only`, `--lcov`, `--open`, …) re-runs the
//! whole suite, so more than one of those in the script means the tests are
//! executed two or three times for the same numbers.
//!
//! These tests pin that down two ways: statically, by classifying every
//! `cargo llvm-cov` invocation in the script, and dynamically, by running
//! `coverage.sh` with a fake `cargo` on `PATH` that records what it was asked
//! to do.

/// The script itself, embedded at compile time so the test needs no filesystem
/// layout at runtime.
const COVERAGE_SH: &str = include_str!("../coverage.sh");

/// Strip a trailing `# …` comment and surrounding whitespace from a line.
fn code_of(line: &str) -> &str {
    let line = match line.find(" #") {
        Some(i) => &line[..i],
        None => line,
    };
    line.trim()
}

/// Does this `cargo llvm-cov` invocation execute the instrumented test suite?
///
/// `args` are the tokens after `cargo llvm-cov`. `clean` and `report` never
/// run tests; `--no-run` is the deprecated alias of `report` and does not
/// either. Everything else — including `--no-report` — builds and runs the
/// suite.
fn runs_test_suite(args: &[&str]) -> bool {
    match args.first() {
        None => true, // bare `cargo llvm-cov`
        Some(&"clean") | Some(&"report") | Some(&"show-env") => false,
        Some(_) => !args.contains(&"--no-run"),
    }
}

/// Every `cargo llvm-cov` invocation in the script, as argument token lists.
fn llvm_cov_invocations(script: &str) -> Vec<Vec<&str>> {
    script
        .lines()
        .map(code_of)
        .filter_map(|code| code.strip_prefix("cargo llvm-cov"))
        .map(|rest| rest.split_whitespace().collect())
        .collect()
}

/// The defect: `--html`, `--summary-only` and `--open` each re-ran the suite,
/// so a plain `./coverage.sh` executed every test twice (three times with
/// `--open`) before the LCOV report was written.
#[test]
fn coverage_script_runs_the_suite_exactly_once() {
    let running: Vec<_> = llvm_cov_invocations(COVERAGE_SH)
        .into_iter()
        .filter(|args| runs_test_suite(args))
        .collect();
    assert_eq!(
        running.len(),
        1,
        "coverage.sh must run the instrumented suite exactly once, found: {running:?}"
    );
    assert!(
        running[0].contains(&"--no-report"),
        "the single instrumented run should be `--no-report`, found: {:?}",
        running[0]
    );
}

/// `--no-run` is the deprecated alias of `report`; the script used it with its
/// output discarded, which did nothing at all.
#[test]
fn coverage_script_avoids_the_deprecated_no_run_alias() {
    for args in llvm_cov_invocations(COVERAGE_SH) {
        assert!(
            !args.contains(&"--no-run"),
            "`--no-run` is the deprecated alias of `report`: {args:?}"
        );
    }
}

/// One run, but still all three renderings — each replayed by `report`.
#[test]
fn coverage_script_still_renders_html_summary_and_lcov() {
    let reports: Vec<Vec<&str>> = llvm_cov_invocations(COVERAGE_SH)
        .into_iter()
        .filter(|args| args.first() == Some(&"report"))
        .collect();
    for flag in ["--html", "--summary-only", "--lcov", "--open"] {
        assert!(
            reports.iter().any(|args| args.contains(&flag)),
            "no `cargo llvm-cov report {flag}` in coverage.sh: {reports:?}"
        );
    }
    assert!(
        reports
            .iter()
            .any(|args| args.contains(&"--lcov") && args.contains(&"lcov.info")),
        "the LCOV report must still be written to lcov.info: {reports:?}"
    );
}

/// End-to-end: run the real script with a fake `cargo` on `PATH` and count how
/// many of the recorded invocations would have executed the test suite.
#[cfg(unix)]
#[test]
fn running_coverage_script_invokes_the_suite_once() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("coverage.sh");
    if !script.is_file() || Command::new("bash").arg("-c").arg(":").status().is_err() {
        return; // no script next to the manifest, or no bash — nothing to drive
    }

    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "sofab-coverage-shim-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();

    let log = dir.join("cargo.log");
    let shim = dir.join("cargo");
    fs::write(
        &shim,
        "#!/usr/bin/env bash\nprintf '%s\\n' \"$*\" >>\"$SOFAB_CARGO_LOG\"\nexit 0\n",
    )
    .unwrap();
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).unwrap();

    let path = format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    // Both entry points: the plain run and the `--open` run.
    for extra in [None, Some("--open")] {
        let _ = fs::remove_file(&log);
        fs::write(&log, "").unwrap();

        let mut cmd = Command::new("bash");
        cmd.arg(&script);
        if let Some(a) = extra {
            cmd.arg(a);
        }
        let out = cmd
            .env("PATH", &path)
            .env("SOFAB_CARGO_LOG", &log)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "coverage.sh {extra:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let recorded = fs::read_to_string(&log).unwrap();
        let runs: Vec<&str> = recorded
            .lines()
            .filter_map(|l| l.trim().strip_prefix("llvm-cov"))
            .filter(|rest| runs_test_suite(&rest.split_whitespace().collect::<Vec<_>>()))
            .collect();
        assert_eq!(
            runs.len(),
            1,
            "coverage.sh {extra:?} ran the instrumented suite {} time(s): {recorded}",
            runs.len()
        );
    }

    fs::remove_dir_all(&dir).unwrap();
}
