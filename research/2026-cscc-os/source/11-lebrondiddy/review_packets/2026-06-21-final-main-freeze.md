# Final Main Freeze — 2026-06-21 (revised after OJ 1359 result)

> **STATUS: FROZEN on the pure 1508 kernel.** An earlier freeze candidate
> (`5e9b7fb`, = 1508 + net-frontdoor) was submitted to OJ and scored **1359.88,
> ~149 below the banked 1508.58**. The net-frontdoor change has been reverted;
> `main`'s kernel is now byte-identical to the proven 1508 candidate `e206a19`.
> Score-chasing is over. OJ submission is human-only.

## What happened

1. `5e9b7fb` = pure 1508 kernel (`e206a19` / tag `oj-busybox-bank-20260619`) **plus
   one change only**: the net-frontdoor merge (`41a86ad`, 3 files —
   `devfs/lib.rs`, `devfs/null.rs`, `syscall/fd.rs`). Verified by
   `git diff e206a19 5e9b7fb -- kernel/`: those 3 files were the entire kernel
   delta. The intermediate "OJ1508 restoration" (`0984936`) kernel is
   byte-identical to `e206a19` (the restore was faithful).
2. `5e9b7fb` was submitted to OJ on 2026-06-21 (eval 21:40–23:41) → **1359.88**.
3. **The entire ~149-pt loss is LA LTP.** `ltp-glibc` = rv 1233 / **la 0**,
   `ltp-musl` = rv 902 / **la 0** (both LA LTP groups produced nothing, all `-`).
   Raw LTP 2592 → 2135; the ~457 gap is all LA. basic 408 / busybox 214 /
   libctest 219 / lua 36 are **identical** to the 1508 run — nothing else moved.
4. net-frontdoor's intended payoff was ~zero on this run (iperf 0, netperf 1.8).
   **It bought no points and coincided with the regression.**

## Root cause (honest)

- net-frontdoor is the only code change since 1508, so it is the prime suspect.
- BUT the loss is **LA-only while RV LTP is unchanged**. A `/dev/null`/statx bug
  would normally hit RV too. LA-alone collapse matches the **chronic LA-LTP
  instability** (LA LTP sits on the global-deadline edge and crashes to 0; see
  `PLAN.md` §5 and `2026-06-18-LA-LTP-rootcause.md`). So it may be LA-LTP
  flakiness that net-frontdoor merely coincided with; this cannot be
  disambiguated without re-running on the OJ (the only full-LA-LTP oracle).
- Either way net-frontdoor was reverted: unverified on OJ, zero measured benefit,
  pure added risk.

## Action taken

- Reverted the 3 net-frontdoor kernel files to their `e206a19` state via a
  forward commit (no `git reset`, no force-push). After the revert,
  `git diff e206a19 HEAD -- kernel/` is **empty** → **`main` kernel == 1508**.
- The net-frontdoor evidence packet (`2026-06-20-net-frontdoor-score.md`) is kept
  as a record of what was tried.

## Why post-1508 work cannot "painlessly" beat 1508

- Every post-1508 OJ result is **below** 1508: `5e9b7fb` = 1359.88;
  `wip/recvfrom01-ltp-20260621@c6496d8` = 1369.60. 0-for-2.
- The dominant swing term is LA LTP (±~457). The post-1508 fixes are RV-side
  (recvfrom/socketpair/timer/signal/rtc errno) — they add a few RV-LTP cases but
  do nothing for the LA collapse, so they cannot move the needle above 1508 and
  only add LA-timing risk.
- Beating 1508 requires fixing the LA-LTP whole-round crash — the hard, still
  unsolved problem. None of the wips solved it.

## Final submission candidate

- **Submit `main` (kernel == `e206a19` == tag `oj-busybox-bank-20260619`, the
  proven 1508.5793028047033).** Equivalent: submit the tag directly.
- **Banked high score: `1508.5793028047033`** (OJ 2026-06-19, `e206a19`;
  basic 408 / busybox 214 / libctest 219 / LTP 2592 / lua 36, no garbage).
  Evidence: `2026-06-19-OJ1508-busybox-bank-result.md`. If the OJ banks the
  highest submission, the 2026-06-21 1359.88 run did **not** lower it.
- **Honest caveat:** LA LTP is genuinely flaky, so a re-submission of the 1508
  kernel is **not guaranteed** to reproduce 1508 exactly (LA LTP could land at 0
  again). It remains the best OJ-proven high and removes the one unverified change.

## Do-NOT-merge list (quarantined wip branches)

None are merged into final `main`. Leave as-is — do not merge, do not delete.

| branch | head | note |
| --- | --- | --- |
| `wip/recvfrom01-ltp-20260621` | `c6496d8` | OJ 1369.60 (< 1508); crosses recvfrom01/socketpair01 but musl-rv LTP still 0 and total below main |
| `wip/rv-ltp-sysinfo02-budget-20260621` | `cf8f224` | RV-LTP budget/quarantine experiment, never OJ-banked |
| `wip/time-nanosleep-overflow-20260620` | `9118a78` | |
| `wip/score-p0-20260618` | `ae5a826` | `ae5a826` manifest runner — explicitly forbidden |
| `wip/la-ltp-direct-20260619` | `0fd5eae` | merged by `1577efa`, reverted by `71bcf85`; changes not in main |
| `wip/la-binfmt-20260618` | `b0fb8f2` | |

## Closeout checks (2026-06-21)

- No leftover long tasks on remote (`ps` sweep for qemu/docker/ltp/codex/cargo/make = none).
- `main` kernel == `e206a19` (1508), verified by empty `git diff e206a19 HEAD -- kernel/`.
- Handoff caveat: Mutagen `oskernel-school-sync` has 3 unresolved conflicts and the
  remote tree is dirty on `cf8f224`. Does not affect the local revert or the GitLab
  push; reconcile before any future remote run.
