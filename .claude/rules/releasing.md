---
paths:
  - "Cargo.toml"
  - ".github/workflows/release.yml"
---

# Releasing

1. **Pick the number with the gate.** Build `contract-gate` (`cargo build --release --bin contract-gate`) and run it with the previous release's published `nodex-envelope-schema-v<previous>.json` as the baseline and `nodex export envelope-schema` from the working tree as the head. A non-empty `breaking` list needs a minor bump pre-1.0; an empty one, a patch. After bumping, the same run must end with `verdict: pass` — the release workflow runs this gate and fails a tag that under-bumps, so deciding here is what avoids a tag that has to be deleted.
2. **Bump every site.** Start from the file set of the previous `chore: release` commit (`git show --stat`), then search the tree for the old version and the old pin range to catch sites added since. CI refuses a `SKILL.md` `metadata.version` or a tag that differs from `Cargo.toml`; the behaviour snapshots embed the version and `behaviour_sweep` stops at the first mismatch, so rerun it until none is left. The `nodex_version` ranges the docs teach move only with the minor.
3. **Gate.** `./scripts/check.sh` must end on the green banner.
4. **Publish.** Commit `chore: release X.Y.Z`, create the annotated tag `vX.Y.Z`, push `main`, then push the tag.
5. **Verify what shipped.** CI, Lint, Release and Install Test succeed for that commit and tag; the release lists every platform archive with its `.sha256`, `SHA256SUMS.txt`, both contract manifests and the skill tarball; a downloaded archive matches its sidecar and `SHA256SUMS.txt`, and its binary prints the version.
