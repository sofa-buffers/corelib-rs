//! The CI workflow has to exercise the build users link against
//! (CORELIB_PLAN §12.1).
//!
//! §12.1 asks for two things in sequence: "Build in both debug and release
//! configurations" and "Run the full test suite, including the shared test
//! vectors from `assets/`". Building `--release` and then testing without it
//! satisfies the first and quietly drops the second half of the second: the
//! optimized artifact is compiled, thrown away, and never asserted on.
//!
//! That gap bites this port harder than most. Its correctness arguments are
//! optimizer-visible — `read_varint_ready`/`read_varint_wide` do unchecked
//! unaligned loads behind a `pos + MAX_VARINT_LEN <= buf.len()` guard,
//! `push_byte` writes through `get_unchecked_mut`, `write_varint_unchecked`
//! stores eight bytes for a one-byte varint — and the `release` profile turns
//! on `opt-level = 3`, `lto = "fat"` and `codegen-units = 1`. A mis-guarded
//! unchecked path that only misbehaves once the inliner has seen through it
//! would ship green.
//!
//! These tests pin the workflow itself: some job must run the whole suite in
//! an optimized profile, and the unoptimized (debug-assertions) leg must stay —
//! it is the one that catches the `debug_assert!`s the release build compiles
//! out.

/// The CI workflow, embedded at compile time so the test needs no filesystem
/// layout at runtime.
const CI_YML: &str = include_str!("../.github/workflows/ci.yml");

/// Indentation of a YAML line, in leading spaces.
fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// The shell body of every `run:` step in the workflow.
///
/// Handles both spellings used here: an inline `run: cargo …` and a block
/// scalar `run: |` whose body is the following, more-indented lines.
fn run_steps(yml: &str) -> Vec<String> {
    let lines: Vec<&str> = yml.lines().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        let key_indent = indent(line);
        let code = line.trim_start().trim_start_matches("- ").trim_start();
        let Some(rest) = code.strip_prefix("run:") else {
            continue;
        };
        let rest = rest.trim();
        if rest != "|" && rest != ">" && rest != "|-" {
            out.push(rest.to_string());
            continue;
        }
        // Block scalar: everything indented past the `run:` key belongs to it.
        let mut body = String::new();
        while i < lines.len() {
            let cont = lines[i];
            if !cont.trim().is_empty() && indent(cont) <= key_indent {
                break;
            }
            body.push_str(cont.trim());
            body.push('\n');
            i += 1;
        }
        out.push(body);
    }
    out
}

/// Every `cargo …` command line in the workflow, as argument token lists
/// (the `cargo` token itself dropped).
fn cargo_invocations(yml: &str) -> Vec<Vec<String>> {
    run_steps(yml)
        .iter()
        .flat_map(|step| {
            step.lines()
                .map(str::trim)
                .filter_map(|cmd| cmd.strip_prefix("cargo "))
                .map(|rest| rest.split_whitespace().map(str::to_string).collect())
                .collect::<Vec<Vec<String>>>()
        })
        .collect()
}

/// Does this `cargo` invocation run the test suite?
fn is_test_run(args: &[String]) -> bool {
    args.first().is_some_and(|verb| verb == "test")
}

/// Is this invocation built with optimizations — the profile the crate ships?
///
/// `--release` selects the `bench` profile for test targets, which inherits
/// `[profile.release]`: the same `opt-level = 3` / fat LTO / one codegen unit
/// the published artifact is compiled with.
fn is_optimized(args: &[String]) -> bool {
    if args.iter().any(|a| a == "--release" || a == "-r") {
        return true;
    }
    args.windows(2)
        .any(|w| w[0] == "--profile" && (w[1] == "release" || w[1] == "bench"))
}

/// The defect: the workflow built `--release` and then tested without it, so
/// the unsafe fast paths were only ever asserted on at `opt-level = 0`.
#[test]
fn ci_runs_the_test_suite_in_the_release_profile() {
    let optimized: Vec<_> = cargo_invocations(CI_YML)
        .into_iter()
        .filter(|args| is_test_run(args) && is_optimized(args))
        .collect();

    assert!(
        !optimized.is_empty(),
        "no CI job runs `cargo test` with optimizations enabled — the unsafe \
         unaligned-load and `get_unchecked` fast paths are only ever exercised \
         in the unoptimized build (CORELIB_PLAN §12.1)"
    );
}

/// The debug leg is not interchangeable with the release one: it is where the
/// `debug_assert!`s (e.g. `read_varint_tail`'s width invariant, whose test is
/// `#[cfg(debug_assertions)]`) actually fire. §12.1 asks for both.
#[test]
fn ci_still_runs_the_test_suite_unoptimized() {
    let debug: Vec<_> = cargo_invocations(CI_YML)
        .into_iter()
        .filter(|args| is_test_run(args) && !is_optimized(args))
        .collect();

    assert!(
        !debug.is_empty(),
        "no CI job runs `cargo test` without `--release`; the debug-assertion \
         legs of the suite would stop running (CORELIB_PLAN §12.1)"
    );
}

/// The optimized leg has to be the *whole* suite — shared vectors included —
/// not one hand-picked target. Anything that narrows the run (a target
/// selector or a filter word) defeats the point of adding it.
#[test]
fn the_optimized_test_leg_runs_the_whole_suite() {
    for args in cargo_invocations(CI_YML)
        .into_iter()
        .filter(|args| is_test_run(args) && is_optimized(args))
    {
        let narrowing = [
            "--lib",
            "--bin",
            "--bins",
            "--test",
            "--tests",
            "--doc",
            "--example",
            "--examples",
        ];
        for (i, arg) in args.iter().enumerate().skip(1) {
            assert!(
                !narrowing.contains(&arg.as_str()),
                "`cargo {}` restricts the optimized run to a subset of the \
                 suite; §12.1 wants the full suite, shared vectors included",
                args.join(" ")
            );
            // A bare word after the verb is a test-name filter (flags and
            // their values all start with `-` or follow one).
            let previous_is_flag = args[i - 1].starts_with('-');
            assert!(
                arg.starts_with('-') || previous_is_flag,
                "`cargo {}` filters the optimized run by test name; §12.1 \
                 wants the full suite",
                args.join(" ")
            );
        }
    }
}

/// Sanity check on the parser itself: the workflow it reads is the real one,
/// and the extraction finds the steps that are known to be there.
#[test]
fn the_workflow_parser_sees_the_known_steps() {
    let invocations = cargo_invocations(CI_YML);
    let has = |prefix: &[&str]| {
        invocations
            .iter()
            .any(|args| args.starts_with_tokens(prefix))
    };

    assert!(has(&["fmt"]), "`cargo fmt` step not found");
    assert!(has(&["clippy"]), "`cargo clippy` step not found");
    assert!(has(&["build", "--release"]), "release build step not found");
    assert!(
        has(&["llvm-cov"]),
        "coverage step not found — the `run: |` block scalar did not parse"
    );
}

/// Tiny helper for the parser sanity check: does this token list start with
/// the given tokens?
trait StartsWithTokens {
    fn starts_with_tokens(&self, prefix: &[&str]) -> bool;
}

impl StartsWithTokens for Vec<String> {
    fn starts_with_tokens(&self, prefix: &[&str]) -> bool {
        prefix.len() <= self.len() && prefix.iter().zip(self).all(|(a, b)| a == b)
    }
}
