---
name: release
description: Cut a release of sofa-buffers-corelib (corelib-rs) — pick the version, bump Cargo.toml via a release PR, tag the merge commit, create the GitHub Release that publishes to crates.io, and verify. Use when the user says "release", "cut vX.Y.Z", "publish to crates.io", "tag a release".
argument-hint: "[X.Y.Z]"
---

# Release sofa-buffers-corelib

The **git tag `vX.Y.Z` is the source of truth** for the version, but Cargo can't
take the version from it at publish time. So `Cargo.toml` must already say `X.Y.Z`
on the tagged commit, and the tag has to point at a commit on `main` where CI passed.

Publishing is started by **publishing a GitHub Release**, not by pushing the tag.
`.github/workflows/release.yml` then runs these jobs:
guard (tag ↔ manifest, version not already on crates.io) → ci-status (`ci.yml`
must have passed on the exact SHA) → package-smoke → publish (Trusted Publishing/OIDC,
environment `crates-io`, **no approval gate**) → verify (install from crates.io).

**A crates.io version cannot be changed or deleted, only yanked.** Once the Release
is published there's no undo. Ask the user for an explicit OK before step 6
(pushing the tag) and again before step 8 (the Release).

## Where the version lives

| Place | Must change? |
|---|---|
| `Cargo.toml` → `[package] version` | **yes**, the only place that's checked (`release.yml` guard + `version-consistency.yml` on the tag push) |
| comment block above `version` in `Cargo.toml` | yes, when it explains the *current* version (e.g. "0.11.0, not 0.10.x: …"). Rewrite it for the new version or remove it |
| `Cargo.lock` | no: it's gitignored (library crate) |
| `README.md` | no: it uses `cargo add sofa-buffers-corelib`, with no pinned version |
| examples in `.github/smoke/run.sh`, the `release.yml` dispatch description, and the prose in `version-consistency.yml` | optional, cosmetic only. Update them in the same PR if they look stale |

Before bumping, run `grep -rn --exclude-dir=target --exclude-dir=.git '<old version>' .`
in case a new place has appeared.

## Steps

### 1. Sync and check the state
```bash
git checkout main && git pull -p
git status --short                       # must be clean
git tag -l 'v*' --sort=-v:refname | head -3
gh release list -L 3
curl -sS -H 'User-Agent: corelib-rs release' https://crates.io/api/v1/crates/sofa-buffers-corelib \
  | jq -r '.versions[] | "\(.num) \(.created_at) yanked=\(.yanked)"' | head -5
grep -m1 '^version' Cargo.toml
```
Look for gaps. For example, a tag that has no GitHub Release: v0.11.0 was published
to crates.io by hand as the first Trusted Publishing bootstrap, and has a tag but no
Release. Point these out and don't fix them silently.

### 2. Choose the version
- Pre-1.0 Cargo semantics: **an API break or a change in wire output → bump the
  minor** (0.11 → 0.12). Otherwise, for fixes and additive changes → bump the patch
  (0.11.0 → 0.11.1).
- Go through the changes since the last tag: `git log --oneline vLAST..main`. Commits
  with `!` (e.g. `refactor(istream)!:`) or `BREAKING` mean a minor bump.
  `cargo semver-checks` isn't installed. Only use it if the user wants it installed.
- The SofaBuffers family (corelib-c-cpp, -go, -dart, -rs-no-std, sofabgen) often
  aligns its minor versions ("Aligns this library with the rest of the family at
  0.10.0"). Ask the user whether a family version is the target.
- `Cargo.toml` may already be ahead of the last tag (bumped when the breaking change
  landed). Then step 3 is only the comment/cosmetics, or can be skipped.
- **Confirm the version with the user** if it wasn't given as an argument.

### 3. Release PR (branch from fresh `main`)
```bash
git checkout -b release/vX.Y.Z main
# edit the version + comment in Cargo.toml (see table above)
cargo metadata --no-deps --format-version 1 | jq -r '.packages[0].version'   # == X.Y.Z
```
Local pre-flight, the same things CI and the release workflow check:
```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test && cargo test --release
cargo package --allow-dirty && cargo package --list --allow-dirty
CRATE=sofa-buffers-corelib .github/smoke/run.sh "path = \"$PWD/target/package/sofa-buffers-corelib-X.Y.Z\""
```
Commit `chore(release): X.Y.Z`. In the PR body say "Version bump only …", then list
**Breaking since vLAST** with Crucible F-IDs / CORELIB_PLAN §§ (see PR #42 as an
example). Push, `gh pr create`, wait for CI with `gh pr checks --watch`, merge
(`gh pr merge --squash --delete-branch`).

### 4. Make sure CI passed on the merge commit
```bash
git checkout main && git pull -p
SHA=$(git rev-parse HEAD)
gh run list --workflow ci.yml --commit "$SHA"      # wait for completed/success
```
The `ci-status` job in `release.yml` fails without a successful `ci.yml` run on
exactly this SHA.

### 5. Final check before tagging
- `git log -1` is the release merge commit, and `Cargo.toml` says `X.Y.Z`
- the version is still free on crates.io:
  `curl -s -o /dev/null -w '%{http_code}' -H 'User-Agent: corelib-rs release' https://crates.io/api/v1/crates/sofa-buffers-corelib/X.Y.Z` → `404`

### 6. Tag (annotated, like v0.11.0) — **ask for OK first**
```bash
git tag -a vX.Y.Z -m vX.Y.Z "$SHA"
git push origin vX.Y.Z
gh run list --workflow version-consistency.yml -L 1    # must pass
```

### 7. Rehearsal (recommended; publishes nothing)
```bash
gh workflow run release.yml -f tag=vX.Y.Z
gh run watch "$(gh run list --workflow release.yml -L 1 --json databaseId -q '.[0].databaseId')"
```
guard, ci-status and package-smoke must pass. publish and verify are skipped.

### 8. GitHub Release → publish to crates.io — **ask for OK first**
Write the notes in the style of the v0.10.0 Release (`gh release view v0.10.0`):
start with one sentence saying which version this is (and whether it's aligned with
the family), then **Breaking since vLAST** with F-IDs / CORELIB_PLAN §§, then the
additions, and a note if sofabgen changed in lockstep.
```bash
gh release create vX.Y.Z --verify-tag --title vX.Y.Z --notes-file <notes.md>
gh run watch "$(gh run list --workflow release.yml --event release -L 1 --json databaseId -q '.[0].databaseId')"
```

### 9. Verify
- The run is green, including **verify** (it installs `=X.Y.Z` from crates.io and runs a round trip)
- `https://crates.io/crates/sofa-buffers-corelib` shows X.Y.Z. docs.rs builds it on its own
- `gh release list -L 1` shows vX.Y.Z as Latest

## If something fails
- **guard: manifest ≠ tag.** Delete the tag (`git push --delete origin vX.Y.Z && git tag -d vX.Y.Z`),
  fix it with a new PR, then tag again. This is safe as long as nothing was published.
- **guard: version already on crates.io.** It can't be reused. Bump to the next version.
- **ci-status: no green CI.** Re-run CI on the SHA (`gh run rerun`), then run the
  release workflow again (`gh run rerun <release-run-id>`). There's no need to
  recreate the Release.
- **publish failed before the upload.** Fix it, then `gh run rerun --failed`.
- **verify failed after a successful publish.** Check crates.io first. The version is
  there, so **don't** publish it again. If it's broken, `cargo yank --version X.Y.Z`
  and release the next patch version.
