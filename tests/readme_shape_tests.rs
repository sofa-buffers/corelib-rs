//! The README's section shape (CORELIB_PLAN §9).
//!
//! §9 opens with "Reproduce the structure below … **Do not change the section
//! ordering and do not invent new top-level sections**"; the shared shape is
//! the point, because a reader who knows one port's README has to be able to
//! navigate any other. Prose alone does not keep that true — a section moves
//! for a good local reason and the family drifts apart one edit at a time.
//!
//! What is pinned here is the part §9 spells out for *this* port: §9.8 requires
//! the two-corelib comparison (this crate vs. `corelib-rs-no-std`) to be the
//! **final subsection of `## Benchmarks`**, explaining each implementation's
//! intended use case and carrying the table that says when to prefer which.
//! Plus the cheap invariant that makes moving a section safe: every
//! same-document link still lands on a heading that exists.

/// The README, embedded at compile time so the test needs no filesystem layout
/// at runtime.
const README: &str = include_str!("../README.md");

/// One ATX heading, with the byte range of the section it opens (the body up to
/// the next heading of any level).
struct Heading {
    level: usize,
    title: String,
    /// Byte range of the section body, heading line included.
    span: std::ops::Range<usize>,
}

/// Every heading in the README, in document order.
///
/// Fenced code blocks are skipped, so a `#[derive(…)]` or a `# shell comment`
/// inside an example is not mistaken for a heading.
fn headings() -> Vec<Heading> {
    let mut out: Vec<Heading> = Vec::new();
    let mut fenced = false;
    let mut offset = 0usize;
    for line in README.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        let trimmed = line.trim_end();
        if trimmed.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let level = trimmed.len() - trimmed.trim_start_matches('#').len();
        if level == 0 || !trimmed[level..].starts_with(' ') {
            continue;
        }
        if let Some(prev) = out.last_mut() {
            prev.span.end = start;
        }
        out.push(Heading {
            level,
            title: trimmed[level + 1..].trim().to_string(),
            span: start..README.len(),
        });
    }
    out
}

/// GitHub's heading-anchor slug: lowercase, punctuation dropped, spaces to `-`.
fn slug(title: &str) -> String {
    title
        .chars()
        .filter_map(|c| match c {
            ' ' => Some('-'),
            c if c.is_alphanumeric() || c == '-' || c == '_' => {
                Some(c.to_lowercase().next().unwrap_or(c))
            }
            _ => None,
        })
        .collect()
}

/// The subsections of the `## Benchmarks` section, in document order.
fn benchmark_subsections() -> Vec<Heading> {
    let all = headings();
    let benchmarks = all
        .iter()
        .find(|h| h.level == 2 && h.title == "Benchmarks")
        .expect("README has a `## Benchmarks` section (§9.8)");
    let end = all
        .iter()
        .find(|h| h.level <= 2 && h.span.start > benchmarks.span.start)
        .map_or(README.len(), |h| h.span.start);
    headings()
        .into_iter()
        .filter(|h| h.level > 2 && h.span.start > benchmarks.span.start && h.span.start < end)
        .collect()
}

#[test]
fn the_two_corelib_comparison_is_the_final_subsection_of_benchmarks() {
    let subsections = benchmark_subsections();
    let last = subsections.last().unwrap_or_else(|| {
        panic!(
            "`## Benchmarks` has no subsections; §9.8 requires the two-corelib \
             comparison (corelib-rs vs. corelib-rs-no-std) as its final one"
        )
    });
    assert!(
        last.title.contains("two Rust corelibs"),
        "the last subsection of `## Benchmarks` is `{}`; §9.8 requires the \
         two-corelib comparison to be the final subsection there, so a reader \
         who knows another port's README finds the rs vs. rs-no-std trade-off \
         where that port puts it",
        last.title
    );
}

#[test]
fn the_comparison_subsection_explains_both_use_cases_and_tables_them() {
    let subsections = benchmark_subsections();
    let last = subsections
        .last()
        .expect("`## Benchmarks` has a final subsection (§9.8)");
    let body = &README[last.span.clone()];

    // §9.8: "explains the intended use case for each implementation".
    for crate_name in ["corelib-rs", "corelib-rs-no-std"] {
        assert!(
            body.contains(crate_name),
            "the final `## Benchmarks` subsection never names `{crate_name}`; \
             §9.8 requires it to explain the intended use case of each of the \
             two Rust corelibs"
        );
    }

    // §9.8: "includes a benchmark comparison table showing why both exist and
    // when to prefer each" — a pipe table with a separator row and a body.
    let rows: Vec<&str> = body
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with('|') && l.ends_with('|'))
        .collect();
    let separator = rows
        .iter()
        .position(|l| {
            l.trim_matches('|').split('|').all(|c| {
                let c = c.trim();
                !c.is_empty() && c.chars().all(|ch| ch == '-' || ch == ':')
            })
        })
        .unwrap_or_else(|| {
            panic!(
                "the final `## Benchmarks` subsection has no comparison table; \
                 §9.8 requires one showing why both corelibs exist and when to \
                 prefer each"
            )
        });
    assert!(
        rows.len() - separator > 2,
        "the comparison table in the final `## Benchmarks` subsection has only \
         {} body row(s); §9.8 wants it to show when to prefer each corelib",
        rows.len() - separator - 1
    );
}

#[test]
fn every_same_document_link_lands_on_a_heading() {
    let anchors: Vec<String> = headings().iter().map(|h| slug(&h.title)).collect();
    let mut rest = README;
    while let Some(i) = rest.find("](#") {
        rest = &rest[i + 3..];
        let end = rest.find(')').expect("a markdown link closes its target");
        let target = &rest[..end];
        assert!(
            anchors.iter().any(|a| a == target),
            "README links to `#{target}`, but no heading slugs to it — a moved \
             or renamed section left a dangling pointer"
        );
        rest = &rest[end..];
    }
}
