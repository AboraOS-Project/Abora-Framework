# Design: applying updates (proposal, not implemented)

Status: **implemented, manual apply only** (decisions: separate root helper; manual first;
everything apt lists). Automatic apply when a window opens is *not* implemented. Applying updates is
the first feature that changes the system, so the design is deliberately conservative.

How it maps to code: `crates/abora-update/src/apply.rs` (request/result types and validation),
`services/abora-apply` (the helper), `POST /api/v1/updates/apply` in `services/aborad`, units in
`installer/systemd/abora-apply.{path,service}`. One deviation from the sketch below: `aborad` reads
the outcome from a result file the helper writes to a root-owned directory
(`[updates] apply_result_file`) and records it in the history, so `aborad` stays the only writer of
its own state file.

## The core problem

`aborad` is sandboxed and unprivileged on purpose (`installer/systemd/aborad.service`: no
capabilities, read-only `/`, no syscalls like `chown`/`setuid`). It **cannot** install packages.
Giving it root to do so would undo that sandbox for a network-facing daemon, so the recommendation
is the opposite: keep `aborad` as it is and add a small, separate, root helper.

## Recommended design

```
 API client ──POST /api/v1/updates/apply──► aborad (unprivileged)
                                              │ 1. authenticate token, needs manage:updates
                                              │ 2. check policy (window, automatic) and last check
                                              │ 3. write request file
                                              ▼
                      /var/lib/abora/apply-request.json   (aborad can write, mode 0600)
                                              │
              abora-apply.path (systemd) ─────┘ starts on change
                                              ▼
                      abora-apply.service (root, own hardening, oneshot)
                                              │ 4. re-validate everything itself (never trusts aborad)
                                              │ 5. apt-get -y upgrade (non-interactive, keep old conf files)
                                              │ 6. append result to history (updates.json), reboot marker
                                              │ 7. reboot only if policy permits
```

Key rules:

1. **Two independent checks.** `aborad` refuses early with a clear error; the helper checks the same
   rules again from the config file (window, `automatic`, reboot policy) and from the package list,
   because it must not trust a request file.
2. **Only one action exists:** "upgrade the packages the last check listed". The request carries no
   command, path or free-form argument. The helper builds the `apt-get` command line itself.
3. **Authenticated only.** A new permission `manage:updates`, granted only to bearer tokens. The
   loopback "preview" trust used for read-only routes never applies to it.
4. **One at a time.** A lock file; a second request while one runs is `409 Conflict`
   (`UpdateError::Conflict` already exists for this).
5. **Audit trail.** Every request, refusal and result is a history entry plus a log record (who,
   token name, when, package list, outcome). The roadmap's "audit log for mutating operations"
   is satisfied here first.
6. **Failure is reported, not hidden.** A failed upgrade is a history entry with `succeeded: false`
   and the tail of apt's error output (never secrets).
7. **Reboot last and only by policy.** `always`: reboot right after success. `ask`: only inside a
   window, otherwise leave `reboot.required = true` for an operator. `never`: never.
8. **Dry run first.** `POST .../apply?dry_run=true` returns what would happen (the apt simulation we
   already run) and changes nothing.

## What it does not do (on purpose)

* No downgrade, no `dist-upgrade`, no package removal, no new repositories.
* Automatic apply is **opt-in** (`[updates] automatic = true`, default off), was added after the manual path had been
  tested against real apt, and uses exactly the same request, helper and checks as a manual apply.
  Automatic apply is a later, separate switch.
* No non-apt providers yet.

## Decisions (made)

1. **Helper vs root daemon.** Recommended: separate root helper as above. Alternative: run `aborad`
   as root (simpler, much weaker).
2. **Manual first.** Recommended: ship `POST /apply` (token only) and `--dry-run` first, and add
   window-triggered automatic apply afterwards. Alternative: automatic from day one.
3. **Scope of `upgrade`.** Recommended: everything apt lists. Alternative: security updates only.

## Test plan

* Unit tests for request validation and the policy re-check (pure, no root).
* The helper's command building is tested against a fake runner, like `HostPackageProvider`.
* **`make test-apply`** (`scripts/test-apply-docker.sh`, also run in CI): the real `aborad` and
  `abora-apply` against **real apt** in a throwaway `ubuntu:24.04` container. It builds a real `.deb` in
  two versions, and checks that a request upgrades exactly that package, keeps the administrator's
  edited config file (no prompt), leaves `dpkg --audit` clean, records the history and the audit
  line, and that a forged request outside the window, or with a hostile package name, is refused by
  the helper itself. Never run on a developer machine's own packages.
* Not covered: the systemd path unit driving a real upgrade (the unit and its trigger were tested
  separately), reboots, and a full VM install with `useradd` and `StateDirectory` ownership.
