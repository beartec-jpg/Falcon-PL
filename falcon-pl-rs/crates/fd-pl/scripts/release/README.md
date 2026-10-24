# Release tooling (public binaries)

| Script | Who / where | What |
|---|---|---|
| `build-portable.sh [OUT]` | CI (`.github/workflows/release-binaries.yml`) or any x86_64 box | portable build (RUSTFLAGS unset), AVX-512 scan = 0, `--version` check, deterministic tarball, `BUILDINFO.txt`, `SHA256SUMS` |
| `publish-draft.sh <run-id>` | operator box with `gh` | downloads the green run's `release-dist`, re-verifies, creates/refreshes a **DRAFT** release `v<ver>` in `beartec-jpg/falcon-pl-releases`. Never publishes, never touches a published release |
| `sign-sums.sh <tag>` | operator box with `gh` + `tailscale ssh` to host1 | streams `SHA256SUMS` to host1, signs it there with the release key (never leaves the host), verifies against the pinned public key, attaches `SHA256SUMS.minisig` to the draft |

Flow: dispatch the workflow on the merge commit → both passes byte-identical → `publish-draft.sh` → `sign-sums.sh` →
Scott confirms → publish the draft
(`gh release edit v<ver> --draft=false -R beartec-jpg/falcon-pl-releases`) → the site serves `/install.sh`.

The installer is `../install-falcon-node.sh`; the network profile is `../network-2300.env`. Docs: `docs/PUBLIC_NODE.md`.

Release public key (minisign, key id `8ACFFC29EE09FAEC`), pinned in `install-falcon-node.sh` and `sign-sums.sh`:

```
RWTs+gnuKfzPik0Xbcf+m1hD/JrCUnWqaD/OAZFZEY2fcNc90D67mZlM
```
