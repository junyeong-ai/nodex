---
paths:
  - "Cargo.toml"
  - ".github/workflows/release.yml"
---

# Releasing

1. **Choose the version.** Build `contract-gate` (`cargo build --release --bin contract-gate`) and run it on four files, in this order: the previous release's published `nodex-envelope-schema-v<previous>.json`, `nodex export envelope-schema` from the working tree, the previous release's `nodex-commands-v<previous>.json`, and `nodex export commands` from the working tree. The gate classifies what each command emits and how it is invoked — leaves, positionals, flags. Pre-1.0, a non-empty `breaking` or `additive` list needs a minor bump. Also inspect behavior neither manifest describes, such as exit codes and default values: an incompatible change there needs a minor bump even when both lists are empty. Otherwise use a patch. After bumping, rebuild and export the head again: the gate must end with `verdict: pass` — the release workflow runs the same gate and rejects insufficient version bumps for classified changes.
2. **Bump every site.** Start from the file set of the previous `chore: release` commit (`git show --stat`), then search the tree for the old version and the old pin range to catch sites added since. A `SKILL.md` `metadata.version` that differs from `Cargo.toml` fails a test step 3 runs, and the release workflow refuses a tag that does. The `nodex_version` ranges the docs teach move only with the minor, and `every_documented_version_pin_admits_the_binary_beside_it` fails one the new version falls outside.
3. **Gate.** `./scripts/check.sh` must end on the green banner.
4. **Publish.** Commit `chore: release X.Y.Z`, create the annotated tag `vX.Y.Z`, push `main`, wait for CI and Lint to succeed on that commit — the multi-OS test matrix runs only there — then push the tag.
5. **Verify what shipped.** CI and Lint succeed for that commit and Release for the tag, its closing install test included; the release lists every platform archive with its `.sha256`, `SHA256SUMS.txt`, both contract manifests and the skill tarball; a downloaded archive matches its sidecar and `SHA256SUMS.txt`, and its binary prints the version.
