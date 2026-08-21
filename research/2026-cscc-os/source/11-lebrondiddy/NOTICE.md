# Baseline Notice

This contest repository imports ByteOS as the kernel baseline.

## ByteOS

- Source repository: https://github.com/Byte-OS/ByteOS
- Imported commit: `06171525001d73f8660c9fb23b11690b32cefe3d`
- Imported path: `kernel/`
- Import method: exact `git archive` of the locked commit from
  `.m0-baselines/ByteOS`; untracked local probe files and build outputs were not
  copied.
- License state: the locked ByteOS commit does not contain a `LICENSE`,
  `COPYING`, or `NOTICE` file, and GitHub reports no repository license
  metadata. No upstream license file was available to preserve.

## lwext4

- Source repository: https://github.com/gkostka/lwext4.git
- Pinned commit: `58bcf89a121b72d4fb66334f1693d3b30e4cb9c5`
- Imported path: `third_party/gkostka-lwext4` as a git submodule.
- License: upstream `LICENSE` is preserved in the submodule. The upstream
  README describes the project as GPLv2 overall with BSD-3-Clause files except
  where files carry GPLv2 notices.

## AI Usage

Codex imported the baseline tree, checked upstream license metadata, added the
submodule reference, and wrote this attribution notice under user/Hermes task
direction.
