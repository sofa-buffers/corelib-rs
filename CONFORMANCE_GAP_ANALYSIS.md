# SofaBuffers `corelib-rs` — Conformance Gap Analysis & Remediation Plan

Audit of the Rust (`std`) core-library port against the language-independent
specification in `CORELIB_PLAN.md`, focusing on the **§13 Conformance
Checklist**. Each item was verified by opening the source — not inferred from
names. Evidence cites file paths and line numbers. Tests were assessed by
inspection only (no Rust toolchain is available in the audit environment), so
test-pass claims are "present and correct by inspection", not "executed".

## Spec revision

This is a **refreshed** audit against the updated `CORELIB_PLAN.md`
(commit `dcb85d6`, 2026-06-30). The single substantive change since the previous
revision: **zero-length arrays and empty sequences are now legal wire
constructs** and a conforming decoder MUST accept them.

- **§4.7** — `element_count` range is now `0 .. 2,147,483,647` (was `1..`). A
  **zero-count integer array** (unsigned/signed) is valid and is encoded as
  exactly `[ header_varint ] [ element_count_varint = 0 ]`, nothing after.
  Absent-vs-empty is now a code-generator concern, **not** a wire-level one.
- **§4.8** — a **zero-count fixlen array** (fp32/fp64) carries **no `fixlen_word`
  and no payload** — exactly `[ header_varint ] [ element_count_varint = 0 ]`.
- **§4.9** — an **empty sequence** (`sequence start` immediately followed by the
  `0x07` end) is legal and well-formed; a decoder MUST accept it.

### What changed vs the previous revision

- The previous audit treated "arrays are never empty (count ≥ 1)" as correct and
  graded the array item **PASS**, explicitly praising "empty arrays rejected" on
  encode and "decode count 0 rejected". Under the updated spec that rejection is
  **non-conformant**. The array item (§13 #6) is therefore **downgraded
  PASS → GAP**.
- Empty-sequence handling is now an explicit requirement. The port **already**
  accepts empty sequences on both encode and decode, and the shared
  `test_vectors.json` now ships three empty-sequence vectors
  (`empty_sequence`, `nested_empty_sequences`, `empty_sequence_between_fields`)
  that the port passes. This is the one piece of the delta the port gets right
  out of the box.
- The local test suite now **locks in** the old (now wrong) rule:
  `tests/ostream_tests.rs:304` (`empty_array_is_argument_error`) and
  `tests/istream_tests.rs:187` (`array_count_zero_is_invalid`) assert the
  forbidden behaviour. The tests item (§13 #12) is **downgraded PASS → PARTIAL**.
- Carried forward unchanged: the **`MAX_DEPTH` (255)** gap (§13 #7), the
  **devcontainer naming** deviation (`rust-devcontainer` vs `rs-devcontainer`,
  §13 #16), and the **missing README "Feature flags" section** (§13 #14). The
  spec change does not touch any of these.

## Summary

| Status  | Count | Change vs previous |
|---------|-------|--------------------|
| PASS    | 13    | −2 (#6, #12)       |
| PARTIAL | 3     | +1 (#12)           |
| GAP     | 2     | +1 (#6)            |
| **Total** | **18** | — |

Headline findings:

- **GAP — zero-length arrays & empty fixlen arrays are rejected (§4.7–4.8).**
  Both the encoder and the decoder treat a zero-count array as an error. The
  encoder returns `Error::Argument` for an empty slice
  (`src/ostream.rs:250,263,276,289`); the decoder returns `Error::InvalidMsg`
  for `count == 0` (`src/istream.rs:325,340,355`). For fixlen arrays the decoder
  also reads the `fixlen_word` unconditionally after the count
  (`src/istream.rs:358`), so it cannot honour the §4.8 "no `fixlen_word` for
  zero elements" rule even if the `count == 0` guard were removed; the encoder
  likewise always emits the `fixlen_word` (`src/ostream.rs:281,295`). New under
  this revision.
- **GAP — `MAX_DEPTH` (255) is not enforced anywhere**, and no `MAX_DEPTH`
  constant is exposed (§4.9, §6.2). The decoder accepts nesting up to
  `u32::MAX` (`src/istream.rs:391`); the encoder never tracks depth. Unchanged.
- **PARTIAL — tests enforce the old zero-count rule** and provide no positive
  coverage for accepting zero-count arrays
  (`tests/ostream_tests.rs:304`, `tests/istream_tests.rs:187`). New.
- **PARTIAL — devcontainer image/container name is `rust-devcontainer`**, not
  the spec-mandated `rs-devcontainer` (§11.3). Unchanged.
- **PARTIAL — README has no "Feature flags" section** in the §9 family shape; it
  is replaced by a "No build-time configuration" section. Unchanged.

Note on empty sequences (the part the port gets right): the encoder emits a
`sequence_begin` then `sequence_end` with no special-casing
(`src/ostream.rs:306-314`); the decoder simply increments/decrements depth and
accepts a start immediately followed by `0x07` (`src/istream.rs:390-403`); and
the three shared empty-sequence vectors are replayed by `tests/vectors_tests.rs`.

## Per-Checklist-Item Results

| # | Item (§13) | Status | Evidence | Notes |
|---|-----------|--------|----------|-------|
| 1 | Public symbols under `sofab` namespace (§6) | PASS | `Cargo.toml:16` `[lib] name = "sofab"`; re-exports `src/lib.rs:61-64` | Package name `SofaBuffers` (`Cargo.toml:2`), import namespace `sofab`. |
| 2 | API version constant/getter returns `1` (§6) | PASS | `src/types.rs:7` `pub const API_VERSION: u32 = 1`; re-exported `src/lib.rs:64`; tested `tests/api_tests.rs:84` | — |
| 3 | Varint & zig-zag match §4.1–4.2 (§4) | PASS | `src/varint.rs:39-96` (read fast/checked, `zigzag_encode/decode`, 64-bit width, arithmetic shift) | Round-trips exactly; overlong length rejected via `shift >= Unsigned::BITS`. Minor: 10th-byte payload bits beyond bit 63 silently dropped (shared by most LEB128 ports). |
| 4 | Field header `(id<<3)\|type` + all 8 wire types (§4.3) | PASS | encode `src/ostream.rs:183-188`; tag constants `src/types.rs:25-32`; decode dispatch over 0x0–0x7 `src/istream.rs:255-405` | — |
| 5 | Fixlen word `(len<<3)\|subtype`, LE floats, UTF-8 no terminator, blobs (§4.6) | PASS | `src/ostream.rs:216-244` (`write_fixlen`, `to_le_bytes`, `write_str` uses `as_bytes()` — no NUL); decode `src/istream.rs:265-318`; `FixlenType::from_raw` `src/types.rs:50-58` | Encoder does not cap fixlen length to `FIXLEN_MAX` (minor note below). |
| 6 | Integer & fixlen arrays w/ single shared word; no dynamic subtypes; **zero-count arrays legal** (§4.7–4.8) | **GAP** | non-fp subtype in fixlen array rejected `src/istream.rs:379` (good); BUT **zero-count rejected** on encode `src/ostream.rs:250,263,276,289` (`Error::Argument`) and decode `src/istream.rs:325,340,355` (`InvalidMsg`); fixlen-array decoder reads `fixlen_word` unconditionally `src/istream.rs:358` and encoder always writes it `src/ostream.rs:281,295` — cannot honour §4.8 "no fixlen_word for 0 elements" | **Downgraded PASS → GAP** under the updated spec. See Remediation §1. |
| 7 | Sequence framing, fresh scope, single-byte `0x07` end, **empty sequence accepted**, skip-by-walking w/ depth, **reject > `MAX_DEPTH`=255** (§4.9) | **GAP** | framing/`0x07` OK `src/ostream.rs:306-314`, `src/istream.rs:390-403`; **empty sequence accepted** (vectors `empty_sequence`/`nested_empty_sequences`/`empty_sequence_between_fields`); **but depth guard is `self.depth == u32::MAX` `src/istream.rs:391`** (allows ~4e9 levels); encoder never tracks depth; **no `MAX_DEPTH` constant** in `src/types.rs` | Empty-sequence sub-requirement now PASSES; the 255-depth rejection (§4.9) and `MAX_DEPTH` constant (§6.2) remain absent. See Remediation §2. |
| 8 | Streaming encode into smaller-than-message buffer, flush + buffer swap (§5.1) | PASS | `src/ostream.rs:82-118` (`with_flush`, `flush`, `buffer_set`), `drain_full`; offset support; tested `tests/ostream_tests.rs:314`, `tests/api_tests.rs:23` | — |
| 9 | Streaming decode: small-chunk `feed`, push-callback / pull-read, lazy binding, auto-skip (§5.2) | PASS | `src/istream.rs:136-152` (`feed` + carry), `Resume` state machine, default-empty `Visitor` = skip `src/istream.rs:32-64` | Visitor idiom (preferred, §5.3). Push/visitor "lazy binding" — allowed. |
| 10 | Result/error reporting follows §6.3 baseline (Result-based) (§6) | PASS | `src/error.rs:12-29` `Argument`/`Usage`/`BufferFull`/`InvalidMsg`; `OK` = `Ok(())`; `Result` alias | Maps to InvalidArgument/UsageError/BufferFull/InvalidMessage. Minor: `Error::Usage` defined but never returned. |
| 11 | Streaming primitives suffice for a thin generated-object layer; `serialize/deserialize` are thin wrappers (§6.1) | PASS | `examples/person.rs:47-170` builds `serialize`/`serialize_to`/`deserialize`/`decoder()/feed/finish` on public API; `[[example]]` `Cargo.toml:41-42` | `serialize()` drives a 32-byte scratch buffer + flush closure — same path as streaming. |
| 12 | Shared vectors pass encode+decode, plus chunked, roundtrip, malformed, skip (§7) | PARTIAL | `tests/vectors_tests.rs` (encode, chunked-encode 1/3/7-byte, decode, byte-at-a-time decode, `skip_ids`); malformed `tests/istream_tests.rs:187-224`; roundtrip `tests/roundtrip_tests.rs`; 67 vectors incl. 3 new empty-sequence vectors | **Downgraded PASS → PARTIAL.** Two local tests assert the now-forbidden rule: `tests/ostream_tests.rs:304` (`empty_array_is_argument_error`) and `tests/istream_tests.rs:187` (`array_count_zero_is_invalid`). No positive test accepts a zero-count array, and the shared suite ships no zero-count-array vector. See Remediation §3. |
| 13 | `assets/` populated: branding + `test_vectors.json` from `corelib-c-cpp` (§8) | PASS | `assets/sofabuffers_logo.png`, `assets/sofabuffers_icon.png`, `assets/test_vectors.json` (`"format":"sofabuffers-test-vectors"`, `"version":1`, 67 vectors) | Vectors now include the empty-sequence cases; still no zero-count-array vector (upstream `corelib-c-cpp` concern). |
| 14 | README follows family format with badges + required sections (§9) | PARTIAL | header/tagline/badges `README.md:1-14`; "Why this design", Usage basic+streaming, API summary, Build & test, Benchmarks | §9 item 7 requires a "Feature flags / build options" section; README has "No build-time configuration" instead. Format deviation. See Remediation §4. |
| 15 | `perf` (CPU-independent) and `bench` (MB/s) tools present and runnable (§10) | PASS | `benches/perf.rs`, `benches/bench.rs`; `[[bench]] harness=false` `Cargo.toml:29-36`; README | — |
| 16 | `.devcontainer/` w/ all files; lang extensions + `anthropic.claude-code`; `.devcontainer/.env` gitignored (§11) | PARTIAL | all six files present; extensions incl. `anthropic.claude-code` `devcontainer.json:10`; `.env` ignored via `.devcontainer/.gitignore` | **Image/container name is `rust-devcontainer` / `sofa-rust-dev`** (`build.sh`, `start.sh`, `attach.sh`), not `rs-devcontainer` (§11.3). See Remediation §5. |
| 17 | `ci.yml` builds+tests on push and PR; version matrix; coverage uploaded + badge (§12.1) | PASS | `.github/workflows/ci.yml`: push+PR triggers; matrix `stable,beta` `fail-fast:false`; release build + test; `cargo llvm-cov`; badge publish; README badge | Coverage badge published to a `badges` branch (Codecov "equivalent" — allowed). Bonus: big-endian s390x leg. |
| 18 | `docs.yml` generates HTML docs + Pages via Actions deploy; Docs badge links to site (§12.2) | PASS | `.github/workflows/docs.yml`: push-main only; `cargo doc --no-deps`; `upload-pages-artifact@v3`, `deploy-pages@v4`; perms `pages/id-token: write`; README Docs badge → `sofa-buffers.github.io/corelib-rs` | — |

## Remediation Plan

Ordered by severity. None of these are required to be implemented by this audit;
this section is the actionable plan.

### 1. (GAP, NEW) Accept zero-length arrays on encode and decode (§4.7–4.8)

**Problem.** Under the updated spec a zero-count array is a valid, fully-specified
empty array on the wire. The port rejects it everywhere:

- Encoder: `write_array_unsigned`/`_signed`/`_fp32`/`_fp64` early-return
  `Err(Error::Argument)` for an empty slice (`src/ostream.rs:250,263,276,289`).
- Decoder: each array arm returns `Err(Error::InvalidMsg)` for `count == 0`
  (`src/istream.rs:325,340,355`).
- Fixlen array specifically: per §4.8 a zero-count fixlen array carries **no
  `fixlen_word` and no payload**. The decoder reads the `fixlen_word`
  unconditionally right after the count (`src/istream.rs:358`), and the encoder
  always writes it (`src/ostream.rs:281,295`), so the no-`fixlen_word` rule is
  not implemented on either side.

**Fix.**
- Encoder: drop the `if data.is_empty() { return Err(Error::Argument) }` guards
  in all four `write_array_*`; emit `[ header ] [ count=0 ]` and stop. For
  `write_array_fp32`/`_fp64`, when `data.is_empty()` write the header and the
  `count = 0` varint and **do not** write the `fixlen_word`.
- Decoder: remove the `count == 0` rejection in the unsigned/signed arms
  (`src/istream.rs:325,340`); a count of 0 should emit `array_begin(.., 0)` (or
  no event, matching the family) and consume nothing further. In the fixlen arm
  (`src/istream.rs:355`), when `count == 0` return immediately **before** reading
  the `fixlen_word`, since none is present on the wire.
- Keep the upper-bound guard `count > ARRAY_MAX` in all three arms.

**Files.** `src/ostream.rs`, `src/istream.rs`.

**Acceptance criteria.** `write_array_unsigned(id, &[])` produces exactly
`[header][0x00]`; `write_array_fp32(id, &[])` produces exactly `[header][0x00]`
(no `fixlen_word`); feeding those bytes back decodes cleanly to an empty array;
non-empty arrays are unchanged; oversized counts still rejected.

### 2. (GAP) Enforce `MAX_DEPTH` = 255 and expose the constant (§4.9, §6.2)

**Problem.** Maximum nested-sequence depth is **255**; a decoder **must reject**
deeper nesting with `InvalidMessage`, and an encoder **must not** open more than
255 nested sequences. The decoder only fails at `self.depth == u32::MAX`
(`src/istream.rs:391`); the encoder (`src/ostream.rs:306-314`) never tracks
depth. There is no `MAX_DEPTH` constant (`src/types.rs` exposes only
`API_VERSION`, `ID_MAX`, and a private `ARRAY_MAX`), so §6.2 is unmet.

**Fix.**
- Add `pub const MAX_DEPTH: u32 = 255;` to `src/types.rs`; re-export from
  `src/lib.rs:64`.
- Decoder: in the `T_SEQUENCE_START` arm (`src/istream.rs:390-396`) return
  `Err(Error::InvalidMsg)` when `self.depth >= MAX_DEPTH` before incrementing.
- Encoder: track depth in `OStream` and return an error from
  `write_sequence_begin` once 255 are open; decrement in `write_sequence_end`.
- Tests: 256-deep decode → `Error::InvalidMsg`; 255-deep succeeds; encoder
  rejects the 256th `write_sequence_begin`.

**Files.** `src/types.rs`, `src/lib.rs`, `src/istream.rs`, `src/ostream.rs`,
`tests/istream_tests.rs` (optionally `tests/ostream_tests.rs`).

**Acceptance criteria.** `sofab::MAX_DEPTH == 255` is public; 256-deep decode
yields `Error::InvalidMsg`; 255 still round-trips; encoder refuses a 256th
sequence; existing tests and shared vectors still pass.

### 3. (PARTIAL, NEW) Fix tests that lock in the old zero-count rule (§7)

**Problem.** Two tests assert the now-forbidden rejection behaviour and will
fail once Remediation §1 lands — and, more importantly, they currently encode a
non-conformant expectation:

- `tests/ostream_tests.rs:304` `empty_array_is_argument_error` asserts
  `write_array_unsigned(0, &[]) == Err(Error::Argument)`.
- `tests/istream_tests.rs:187` `array_count_zero_is_invalid` asserts feeding
  `[0x03, 0x00]` returns `Err(Error::InvalidMsg)`.

There is also **no positive coverage** that a zero-count array (unsigned,
signed, fp32, fp64) round-trips, and none that a zero-count fixlen array omits
the `fixlen_word`.

**Fix.** Replace both tests with their conformant inverse: assert
`write_array_*(id, &[])` succeeds and emits `[header][0x00]` (and, for fixlen,
no `fixlen_word`); assert feeding `[0x03,0x00]` / `[0x04,0x00]` /
`[0x05,0x00]` decodes to an empty array. Add a round-trip case for each of the
four element kinds.

**Files.** `tests/ostream_tests.rs`, `tests/istream_tests.rs` (optionally
`tests/roundtrip_tests.rs`).

**Acceptance criteria.** The suite contains positive zero-count-array tests for
all four array kinds and no test asserting that an empty array is an error.

> Upstream note: the shared `test_vectors.json` (generated by `corelib-c-cpp`)
> ships empty-sequence vectors but **no zero-count-array vector**. Closing the
> cross-language gap fully needs a zero-count-array vector added upstream and
> re-copied here per §8; that is outside this repo's control.

### 4. (PARTIAL) Align the README "Feature flags" section with the §9 family shape

**Problem.** §9 item 7 prescribes a `## Feature flags / build options` section
with a toggle table. This crate ships no Cargo features (it is the speed build)
and documents that under `## No build-time configuration`.

**Fix (format-only).** Rename to `## Feature flags` and add a one-row table
stating all features are always on, pointing to `corelib-rs-no-std` for the
trimmable build; or keep the heading but note the §5.3 toggle set lives in the
no_std sibling. No code change.

**Files.** `README.md`.

**Acceptance criteria.** A reader scanning the family READMEs finds a section in
the §9 position covering feature flags / build options.

### 5. (PARTIAL) Rename the devcontainer image/container to `rs-devcontainer`

**Problem.** §11.3 fixes the image tag and running-container name to
`rs-devcontainer`. The scripts use `rust-devcontainer` for the image
(`build.sh`, `start.sh`, `devcontainer.json` build) and `sofa-rust-dev` for the
running container (`start.sh`, `attach.sh`).

**Fix.** Replace `rust-devcontainer` → `rs-devcontainer` in `build.sh`,
`start.sh`; replace `--name sofa-rust-dev` (`start.sh`) and the
`docker exec ... sofa-rust-dev` (`attach.sh`) with `rs-devcontainer`.

**Files.** `.devcontainer/build.sh`, `.devcontainer/start.sh`,
`.devcontainer/attach.sh`.

**Acceptance criteria.** `build.sh` produces `rs-devcontainer`; `start.sh` runs a
container named `rs-devcontainer`; `attach.sh` attaches to it; no
`rust-devcontainer` / `sofa-rust-dev` references remain.

### Minor robustness notes (not checklist failures)

- **Encoder length/count caps.** `write_fixlen` (`src/ostream.rs:216`) and
  `write_array_*` do not validate length/count against `FIXLEN_MAX` / `ARRAY_MAX`
  (`i32::MAX`). The decoder does (`src/istream.rs:271,325,340,355`), so an
  oversized encode (only reachable with > 2 GiB input on a 64-bit host) produces
  bytes the family decoders reject. Consider returning `Error::Argument` on
  encode for symmetry.
- **Overlong-varint strictness.** `read_varint` rejects > 10 bytes but silently
  drops payload bits in the 10th byte beyond bit 63 (`src/varint.rs:56-58,
  78-80`). Tightening this would reject a few more crafted inputs. Low priority.
- **Unused `Error::Usage`.** Defined (`src/error.rs:19`) but never returned;
  fine for a push/visitor API with no typed read, but worth a doc note.
