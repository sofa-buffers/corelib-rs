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
//!
//! Shape alone is not enough, though: a section can keep its heading and lose
//! the fact a reader came for it. So the checks below also pin *content* — the
//! §9 section list and its order, the §9.1 header block, the §9.2 badge trio,
//! §9.4's absence of an API-documentation chapter, the §9.5 example set, the
//! §6.4 UTF-8 policy, and §9.6's `MIN_OUTPUT_BUFFER` in the memory chapter.
//!
//! Two neighbouring checks live elsewhere and are not repeated here: §6.1.1's
//! closed generated-object name set is
//! `generated_shape_tests::the_readme_never_teaches_a_name_outside_the_closed_set`,
//! and the BENCH_SPEC parity sizes are
//! `bench_shape_tests::both_tools_and_the_readme_state_the_parity_sizes`.

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

// ---------------------------------------------------------------------------
// §9 — the section list, and the header block that precedes it
// ---------------------------------------------------------------------------

/// The `## ` sections §9 prescribes, in the order it prescribes them.
///
/// §9: "Do not change the section ordering and do not invent new top-level
/// sections". The list is *exact* in both directions — a missing one drops a
/// chapter of the contract, an extra one is the invention §9 forbids.
const TOP_LEVEL_SECTIONS: [&str; 6] = [
    "SofaBuffers Rust library",
    "Why this design",
    "Usage",
    "Memory handling",
    "Build & test",
    "Benchmarks",
];

/// The body of the `## <title>` section, heading line included, up to the next
/// `## ` (or the end of the document).
fn top_level_section(title: &str) -> &'static str {
    let all = headings();
    let start = all
        .iter()
        .find(|h| h.level == 2 && h.title == title)
        .unwrap_or_else(|| panic!("README has a `## {title}` section (CORELIB_PLAN §9)"));
    let end = all
        .iter()
        .find(|h| h.level == 2 && h.span.start > start.span.start)
        .map_or(README.len(), |h| h.span.start);
    &README[start.span.start..end]
}

/// The lines inside fenced code blocks in `section` — the runnable part.
fn code_lines(section: &str) -> String {
    let mut out = String::new();
    let mut fenced = false;
    for line in section.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

#[test]
fn the_top_level_sections_are_the_prescribed_list_in_order() {
    let found: Vec<String> = headings()
        .into_iter()
        .filter(|h| h.level == 2)
        .map(|h| h.title)
        .collect();
    assert_eq!(
        found, TOP_LEVEL_SECTIONS,
        "the `## ` sections are not §9's list in §9's order; a reader who knows \
         another port's README navigates this one by that shape, so a renamed, \
         reordered or invented top-level section breaks the family, not just \
         this file"
    );
}

#[test]
fn the_header_block_is_the_generic_one() {
    // §9.1, in order: centered logo, the title, the tagline's two halves, and
    // a link back to the organization.
    for piece in [
        r#"<p align="center"><img src="assets/sofabuffers_logo.png""#,
        "\n# SofaBuffers\n",
        "<b>Structured Objects For Anyone</b><br>",
        "<i>... so optimized, feels amazing.</i>",
        "https://github.com/sofa-buffers)",
    ] {
        assert!(
            README.contains(piece),
            "the §9.1 header block is missing `{piece}`; the centered logo, the \
             title, the tagline and the organization link are identical in every \
             port's README and are what makes them recognizable as one family"
        );
    }
    let logo = README.find("sofabuffers_logo.png").expect("logo");
    let title = README.find("\n# SofaBuffers\n").expect("title");
    let first_section = README
        .find(&format!("\n## {}\n", TOP_LEVEL_SECTIONS[0]))
        .expect("the opening section");
    assert!(
        logo < title && title < first_section,
        "the §9.1 header block does not open the document: the logo, then \
         `# SofaBuffers`, then the tagline and the org link all come before \
         `## {}`",
        TOP_LEVEL_SECTIONS[0]
    );
}

#[test]
fn the_badge_block_carries_ci_coverage_and_docs_in_that_order() {
    let opening = top_level_section(TOP_LEVEL_SECTIONS[0]);
    let badges: Vec<&str> = opening
        .lines()
        .filter(|l| l.trim_start().starts_with("[!["))
        .collect();
    let labels: Vec<&str> = badges
        .iter()
        .map(|l| {
            let rest = &l[l.find("[![").unwrap() + 3..];
            &rest[..rest.find(']').expect("a badge names itself")]
        })
        .collect();
    assert_eq!(
        labels,
        ["CI", "Coverage", "Docs"],
        "§9.2 wants the CI, coverage and Docs badges, in that order, opening \
         `## {}`; found {labels:?}",
        TOP_LEVEL_SECTIONS[0]
    );
    // §9.2: the Docs badge is the *only* pointer to the API reference, so it
    // has to actually point at the published one (§12.2).
    let docs = badges[2];
    assert!(
        docs.contains("https://sofa-buffers.github.io/corelib-rs/"),
        "the Docs badge does not link to the published API reference; §9.4 \
         makes that badge the single entry point to the API documentation, so a \
         badge that goes elsewhere leaves the README with none"
    );
}

#[test]
fn there_is_no_api_documentation_section() {
    // §9.4: no `## Source documentation`, `## API reference`, `## API
    // documentation` or similar — at *any* heading level, since demoting one to
    // `###` evades the letter of §9's top-level list while re-creating exactly
    // the second entry point §9.4 exists to prevent.
    for h in headings() {
        let t = h.title.to_lowercase();
        let is_api_docs = (t.contains("api") || t.contains("source"))
            && (t.contains("documentation") || t.contains("reference") || t.contains("docs"));
        assert!(
            !is_api_docs,
            "`{}` is an API-documentation section; §9.4 forbids one at any \
             heading level — the Docs badge (§9.2) is the single entry point to \
             the generated reference",
            h.title
        );
    }
}

// ---------------------------------------------------------------------------
// §9.5 — the Usage chapter still shows every example the plan lists
// ---------------------------------------------------------------------------

/// §9.5's example list, mapped onto this port's subsection titles, with a
/// needle that must appear in that subsection's *runnable* lines.
///
/// The needle is what makes this a content check rather than a heading check:
/// a subsection can survive a rewrite with its title intact and its example
/// gone, and §9.5 asks for the example.
const USAGE_EXAMPLES: [(&str, &str, &str); 6] = [
    ("simple encode", "Serialize", "OStream::new"),
    ("simple decode", "Deserialize", "decode("),
    (
        "streaming a message larger than the buffer",
        "Serialize stream",
        "OStream::with_flush",
    ),
    ("the OStream wrapper", "Serialize", "write_"),
    (
        "the IStream push-feed wrapper",
        "Deserialize stream",
        "feed(",
    ),
    ("the generated-object path", "Code generator", "sofab::"),
];

#[test]
fn the_usage_chapter_shows_every_example_the_plan_lists() {
    let all = headings();
    let usage = all
        .iter()
        .find(|h| h.level == 2 && h.title == "Usage")
        .expect("README has a `## Usage` section (§9.5)");
    let end = all
        .iter()
        .find(|h| h.level == 2 && h.span.start > usage.span.start)
        .map_or(README.len(), |h| h.span.start);
    let subs: Vec<&Heading> = all
        .iter()
        .filter(|h| h.level > 2 && h.span.start > usage.span.start && h.span.start < end)
        .collect();
    for (what, title, needle) in USAGE_EXAMPLES {
        let sub = subs.iter().find(|h| h.title == title).unwrap_or_else(|| {
            panic!(
                "`## Usage` has no `### {title}` subsection; §9.5 requires a \
                 concise runnable example for {what}"
            )
        });
        let body = &README[sub.span.clone()];
        let code = code_lines(body);
        assert!(
            !code.trim().is_empty(),
            "`### {title}` has no fenced example left; §9.5 asks for a runnable \
             example for {what}, not a description of one"
        );
        assert!(
            code.contains(needle),
            "the example under `### {title}` never uses `{needle}`, so it no \
             longer demonstrates {what} (§9.5)"
        );
    }
}

// ---------------------------------------------------------------------------
// §6.4 — the UTF-8 policy, and §9.6 — MIN_OUTPUT_BUFFER where a caller looks
// ---------------------------------------------------------------------------

#[test]
fn the_strict_utf8_policy_is_documented() {
    // §6.4 splits ports in two. Rust's `str`/`String` is a **Unicode string
    // type**, so this port is in the second camp: it "cannot hold non-UTF-8
    // bytes … so they are always strict. For them the option is a no-op and
    // they MAY omit it entirely (documented as always-ON); only byte-container
    // targets MUST expose it."
    //
    // So there is no knob to check for here, and this test does not look for
    // one. What §6.4 still *requires* of this port is the documentation that
    // replaces it: that strictness is unconditional, and that invalid UTF-8 is
    // rejected rather than replaced (the MUST NOT that holds in every mode).
    assert!(
        README.contains("SOFAB_STRICT_UTF8"),
        "the README never mentions `SOFAB_STRICT_UTF8`; §6.4 lets a \
         Unicode-string target omit the option, but only when it documents the \
         option as always-ON — a reader porting from a byte-container target \
         comes here to find out which"
    );
    let lower = README.to_lowercase();
    assert!(
        lower.contains("always strict") || lower.contains("pinned on"),
        "the README mentions `SOFAB_STRICT_UTF8` without saying this port is \
         always strict; §6.4 requires the omitted option to be documented as \
         always-ON"
    );
    assert!(
        README.contains("U+FFFD"),
        "the README never states that invalid UTF-8 is rejected rather than \
         replaced; §6.4 makes silent `U+FFFD` substitution a MUST NOT in every \
         mode, and a reader who assumes the lossy platform default (Java's \
         `getBytes`, JS's `TextEncoder`) writes a peer this port rejects"
    );
}

#[test]
fn min_output_buffer_is_stated_in_the_memory_chapter() {
    let memory = top_level_section("Memory handling");
    assert!(
        memory.contains("MIN_OUTPUT_BUFFER"),
        "`## Memory handling` never names `MIN_OUTPUT_BUFFER`; §9.6 puts it \
         *there* on purpose — it is the number a caller needs before it can \
         size a streaming buffer, and this is the section they read to find out \
         who allocates what"
    );
    let value = sofab::MIN_OUTPUT_BUFFER.to_string();
    assert!(
        memory.contains(&value),
        "`## Memory handling` names `MIN_OUTPUT_BUFFER` but not its value \
         ({value}); §9.6 wants the number, and a stale one is worse than none"
    );
    assert!(
        memory.contains("sink"),
        "`## Memory handling` states `MIN_OUTPUT_BUFFER` without saying it \
         applies to a buffer installed with a sink; §5.1 binds it there and \
         nowhere else, and a caller sizing a sinkless buffer from `MAX_SIZE` \
         must not think it has a floor"
    );
    // §9.6: "If the port implements pass-through of a string/blob run (§5.1),
    // say so here too." This port does not, and the absence is itself the fact
    // a sink author needs — a sink here is only ever handed the output buffer.
    assert!(
        memory.contains("pass-through") || memory.contains("passthrough"),
        "`## Memory handling` never settles the §5.1 pass-through question; a \
         sink that retains what it receives is written differently depending on \
         the answer, so silence is not an option — this port copies \
         `string`/`blob` runs through the output buffer"
    );
}
