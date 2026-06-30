# SofaBuffers `corelib-rs` — Conformance Gap Analysis & Remediation Plan

Audit of the Rust (`std`) core-library port against the language-independent
specification in `CORELIB_PLAN.md`, focusing on the **§13 Conformance
Checklist**. Each item was verified by opening the source — not inferred from
names. Evidence cites file paths and line numbers. Tests were assessed by
inspection only (no Rust toolchain is available in the audit environment), so
test-pass claims are "present and correct by inspection", not "executed".

## Summary

| Status  | Count |
|---------|-------|
| PASS    | 15    |
| PARTIAL | 2     |
| GAP     | 1     |
| **Total** | **18** |

Headline findings:

- **GAP — `MAX_DEPTH` (255) is not enforced anywhere**, and no `MAX_DEPTH`
  constant is exposed (§4.9, §6.2). The decoder accepts nesting up to
  `u32::MAX`; the encoder never tracks depth.
- **PARTIAL — devcontainer image/container name is `rust-devcontainer`**, not
  the spec-mandated `rs-devcontainer` (§11.3).
- **PARTIAL — README has no "Feature flags" section** in the §9 family shape; it
  is replaced by a "No build-time configuration" section (defensible for this
  build, but a format deviation).

## Per-Checklist-Item Results

| # | Item (§13) | Status | Evidence | Notes |
|---|-----------|--------|----------|-------|
| 1 | Public symbols under `sofab` namespace (§6) | PASS | `Cargo.toml:16` `[lib] name = "sofab"`; all re-exports `src/lib.rs:61-64` | Package name `SofaBuffers` (`Cargo.toml:2`), import namespace `sofab` — matches §6. |
| 2 | API version constant/getter returns `1` (§6) | PASS | `src/types.rs:7` `pub const API_VERSION: u32 = 1`; re-exported `src/lib.rs:64`; tested `tests/api_tests.rs:84` | — |
| 3 | Varint & zig-zag match §4.1–4.2 (§4) | PASS | `src/varint.rs:39-96` (read fast/checked, `zigzag_encode/decode`, 64-bit width, arithmetic shift) | Valid values round-trip exactly; overlong length rejected via `shift >= Unsigned::BITS` (`:56,:78`). Minor: payload bits in a 10th byte beyond bit 63 are silently dropped rather than erroring (shared by most LEB128 ports). |
| 4 | Field header `(id<<3)\|type` + all 8 wire types (§4.3) | PASS | encode `src/ostream.rs:183-188`; tag constants `src/types.rs:25-32`; decode dispatch over all of 0x0–0x7 `src/istream.rs:248-405` | — |
| 5 | Fixlen word `(len<<3)\|subtype`, LE floats, UTF-8 no terminator, blobs (§4.6) | PASS | `src/ostream.rs:216-244` (`write_fixlen`, `to_le_bytes`, `write_str` uses `as_bytes()` — no NUL); decode `src/istream.rs:265-318`; `FixlenType::from_raw` `src/types.rs:50-58` | Encoder does not cap fixlen length to `FIXLEN_MAX` (see note under Remediation §4). |
| 6 | Integer arrays + fixlen arrays w/ single shared word; no dynamic subtypes in fixlen arrays (§4.7–4.8) | PASS | encode `src/ostream.rs:249-300`; decode rejects non-fp subtype in fixlen array `src/istream.rs:362-380` (`_ => InvalidMsg`); empty arrays rejected `src/ostream.rs:250,266`; decode count 0 rejected `src/istream.rs:325,340,355` | — |
| 7 | Sequence framing, fresh scope, single-byte `0x07` end, skip-by-walking w/ depth, **reject > `MAX_DEPTH`=255** (§4.9) | **GAP** | framing/`0x07` OK `src/ostream.rs:306-314`, `src/istream.rs:390-403`; **but depth guard is `self.depth == u32::MAX` `src/istream.rs:391`** (allows ~4e9 levels); encoder never tracks depth; **no `MAX_DEPTH` constant** in `src/types.rs` | Normative 255-depth rejection (§4.9) and the `MAX_DEPTH` constant (§6.2) are absent. See Remediation §1. |
| 8 | Streaming encode into smaller-than-message buffer, flush + buffer swap (§5.1) | PASS | `src/ostream.rs:82-118` (`with_flush`, `flush`, `buffer_set`), `drain_full` `:137-146`; offset support `:66`; tested `tests/ostream_tests.rs:314`, `tests/api_tests.rs:23` | — |
| 9 | Streaming decode: small-chunk `feed`, push-callback / pull-read, lazy binding, auto-skip (§5.2) | PASS | `src/istream.rs:136-152` (`feed` + carry), `Resume` state machine `:73-95,170-408`, default-empty `Visitor` = skip `:32-64` | Uses the visitor idiom (explicitly preferred, §5.3). "Lazy binding" = the handler decides per-field to consume or drop; this is a push/visitor model (README "Memory handling"), not destination-pointer binding — allowed. |
| 10 | Result/error reporting follows §6.3 baseline (Result-based) (§6) | PASS | `src/error.rs:12-29` `Argument`/`Usage`/`BufferFull`/`InvalidMsg`; `OK` = `Ok(())`; `Result` alias `:46` | Maps to InvalidArgument/UsageError/BufferFull/InvalidMessage. Minor: `Error::Usage` is defined but never returned (`grep` finds only its `Display` arm) — acceptable in a push/visitor API with no typed read. |
| 11 | Streaming primitives suffice for a thin generated-object layer; `serialize/deserialize` are thin wrappers over streaming (§6.1) | PASS | `examples/person.rs:47-170` builds `serialize`/`serialize_to`/`deserialize`/`decoder()/feed/finish` purely on public API; `[[example]]` `Cargo.toml:41-42` | `serialize()` drives a 32-byte scratch buffer + flush closure (`examples/person.rs:68-78`) — same path as streaming. |
| 12 | Shared vectors pass encode+decode, plus chunked, roundtrip, malformed, skip (§7) | PASS | `tests/vectors_tests.rs` (encode, chunked-encode 1/3/7-byte, decode, byte-at-a-time decode `:305`, `skip_ids`); malformed `tests/istream_tests.rs:187-224`; roundtrip `tests/roundtrip_tests.rs`; fast-vs-stream `tests/reader_tests.rs:46`; vectors file 67 vectors | Verified by inspection only (no toolchain). No malformed test for nesting > `MAX_DEPTH` (consistent with the item-7 gap). |
| 13 | `assets/` populated: branding + `test_vectors.json` from `corelib-c-cpp` (§8) | PASS | `assets/sofabuffers_logo.png`, `assets/sofabuffers_icon.png`, `assets/test_vectors.json` (`"format":"sofabuffers-test-vectors"`, `"version":1`, 67 vectors) | — |
| 14 | README follows family format with badges + required sections (§9) | PARTIAL | header/tagline/badges `README.md:1-14`; sections "Why this design" `:53`, Usage basic+streaming `:70-124`, API summary `:138`, Build & test `:252`, Benchmarks `:276` | §9 item 7 requires a "Feature flags / build options" section with a toggle table; README instead has "No build-time configuration" `:239` (this build has no flags by design). Format deviation. See Remediation §3. |
| 15 | `perf` (CPU-independent) and `bench` (MB/s) tools present and runnable (§10) | PASS | `benches/perf.rs` (311 lines), `benches/bench.rs` (164 lines); `[[bench]] harness=false` `Cargo.toml:29-36`; README `:276-303` | — |
| 16 | `.devcontainer/` w/ all files; `devcontainer.json` lists lang extensions + `anthropic.claude-code`; `.devcontainer/.env` gitignored (§11) | PARTIAL | all six files present (`Dockerfile`, `build.sh`, `start.sh`, `attach.sh`, `devcontainer.json`, `.env.example`); extensions incl. `anthropic.claude-code` `devcontainer.json:10`; `.env` ignored via `.devcontainer/.gitignore:6` (confirmed untracked by `git ls-files`) | **Image/container name is `rust-devcontainer` / `sofa-rust-dev`** (`build.sh:6`, `start.sh:17,22`, `attach.sh:4`), not the spec's `rs-devcontainer` (§11.3). See Remediation §2. |
| 17 | `ci.yml` builds+tests on push and PR; version matrix; coverage uploaded + badge (§12.1) | PASS | `.github/workflows/ci.yml`: triggers push+PR `:3-7`; matrix `stable,beta` `fail-fast:false` `:30-33`; release build + test `:39-40`; `cargo llvm-cov` `:66-72`; badge publish `:74-78`; README badge `README.md:13` | Coverage badge published to a `badges` branch (Codecov "equivalent" — allowed). Bonus: big-endian s390x leg `:44-51`. |
| 18 | `docs.yml` generates HTML docs + Pages via Actions deploy (no `gh-pages`); Docs badge links to site (§12.2) | PASS | `.github/workflows/docs.yml`: push-main only `:3-5`; `cargo doc --no-deps` `:34`; `upload-pages-artifact@v3` `:42`, `deploy-pages@v4` `:55`; perms `pages/id-token: write` `:9-12`; README Docs badge → `sofa-buffers.github.io/corelib-rs` `README.md:14` | — |

## Remediation Plan

Ordered by severity. None of these are required to be implemented by this audit;
this section is the actionable plan.

### 1. (GAP) Enforce `MAX_DEPTH` = 255 and expose the constant

**Problem.** §4.9 and §6.2 are normative: maximum nested-sequence depth is
**255**; a decoder **must reject** a message nesting deeper with an
`InvalidMessage` error, and an encoder **must not** open more than 255 nested
sequences. The current decoder only fails at `self.depth == u32::MAX`
(`src/istream.rs:391`) — it happily accepts hundreds of millions of levels — and
the encoder (`src/ostream.rs:306-314`) never tracks depth at all. There is also
no `MAX_DEPTH` constant anywhere (`src/types.rs` exposes only `API_VERSION`,
`ID_MAX`, and a private `ARRAY_MAX`), so §6.2 is unmet.

**Fix.**
- Add `pub const MAX_DEPTH: u32 = 255;` to `src/types.rs` and re-export it from
  `src/lib.rs:64`.
- Decoder: in `src/istream.rs`, change the `T_SEQUENCE_START` arm
  (`:390-396`) to return `Err(Error::InvalidMsg)` when `self.depth >= MAX_DEPTH`
  before incrementing.
- Encoder: track depth in `OStream` and return `Err(Error::Argument)` (or
  `InvalidMsg`) from `write_sequence_begin` (`src/ostream.rs:306`) once 255 open
  sequences are reached; decrement in `write_sequence_end`.
- Add tests: a 256-deep message decodes to `Error::InvalidMsg`; a 255-deep
  message succeeds; encoder rejects the 256th `write_sequence_begin`.

**Files.** `src/types.rs`, `src/lib.rs`, `src/istream.rs`, `src/ostream.rs`,
`tests/istream_tests.rs` (and optionally `tests/ostream_tests.rs`).

**Acceptance criteria.** `sofab::MAX_DEPTH == 255` is public; decoding a message
with 256 nested `sequence_begin`s yields `Error::InvalidMsg`; 255 nesting still
round-trips; the encoder refuses to open a 256th sequence; all existing tests
and shared vectors still pass.

### 2. (PARTIAL) Rename the devcontainer image/container to `rs-devcontainer`

**Problem.** §11.3 fixes the image tag **and** running-container name to
`<lang>-devcontainer` = `rs-devcontainer` for this repo. The scripts use
`rust-devcontainer` for the image (`build.sh:6`, `start.sh:22`,
`devcontainer.json` build) and `sofa-rust-dev` for the running container
(`start.sh:17`, `attach.sh:4`).

**Fix.** Replace the image tag `rust-devcontainer` → `rs-devcontainer` in
`build.sh`, `start.sh` (and confirm `devcontainer.json` builds from the
`Dockerfile`, which it does). Replace the `--name sofa-rust-dev` in `start.sh`
and the `docker exec ... sofa-rust-dev` in `attach.sh` with `rs-devcontainer` so
build/start/attach are consistent.

**Files.** `.devcontainer/build.sh`, `.devcontainer/start.sh`,
`.devcontainer/attach.sh`.

**Acceptance criteria.** `build.sh` produces an image tagged `rs-devcontainer`;
`start.sh` runs a container named `rs-devcontainer`; `attach.sh` attaches to
that same name; no remaining references to `rust-devcontainer` / `sofa-rust-dev`.

### 3. (PARTIAL) Align the README "Feature flags" section with the §9 family shape

**Problem.** §9 item 7 prescribes a `## Feature flags / build options` section
containing a toggle table (fixlen, array, sequence, fp64, overflow checks) with
defaults and a minimal-build example. This crate intentionally ships no Cargo
features (it is the speed build), and the README documents that under
`## No build-time configuration` (`README.md:239`) — correct content, but it
breaks the cross-language section parity §9 asks for.

**Fix (low-effort, format-only).** Either (a) rename the section to
`## Feature flags` and add a one-row table stating that all features are always
on (no toggles) with a pointer to `corelib-rs-no-std` for the trimmable build,
or (b) keep the heading but add an explicit note that the toggle set defined in
§5.3 lives in the no_std sibling. No code change.

**Files.** `README.md`.

**Acceptance criteria.** A reader scanning the family READMEs finds a section in
the §9 position covering feature flags / build options, even if the answer is
"none in this build"; wording stays close to the other ports.

### Minor robustness notes (not checklist failures)

- **Encoder length/count caps.** `write_fixlen` (`src/ostream.rs:216`),
  `write_array_*` (`:249-300`) do not validate that the payload length / element
  count stays within `FIXLEN_MAX` / `ARRAY_MAX` (`i32::MAX`). The *decoder* does
  (`src/istream.rs:271,325,340,355`), so an oversized encode (only reachable
  with > 2 GiB input on a 64-bit host) produces bytes the family decoders would
  reject. Consider returning `Error::Argument` on the encode side for symmetry.
- **Overlong-varint strictness.** `read_varint` rejects > 10 bytes but silently
  drops payload bits in the 10th byte beyond bit 63 (`src/varint.rs:56-58,
  78-80`). Strictly, a 10th byte with high bits set is malformed; tightening this
  would make the decoder reject a few more crafted inputs. Low priority.
- **Unused `Error::Usage`.** Defined (`src/error.rs:19`) but never returned;
  fine for a push/visitor API with no typed read, but worth a doc note.
