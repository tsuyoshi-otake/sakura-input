## This machine

- **Every `cargo` invocation must be prefixed with
  `CARGO_HTTP_CHECK_REVOKE=false`**, or the fetch fails with
  `CRYPT_E_NO_REVOKE_CHECK (0x80092012)`.

- **Heredocs through the Bash tool fail here** (`unexpected EOF`, and
  `$TMPDIR` is unset so `/msg.txt` is a permission error). Write commit
  messages with the file-writing tool into the session scratchpad and use
  `git commit -F <path>`.

- **`perl -0pi -e 's|...|...|'` multi-line in-place substitution is unreliable
  here.** Verified 2026-09-09 (#148): one run prepended the replacement to the
  top of `crates/sakura-engine/src/timing.rs` instead of substituting, and
  another silently placed the replacement inside a `format_args!` in
  `crates/sakura-engine/src/server.rs`, producing parse errors far from the
  intended site. Use `sed` with explicit line numbers, `sed -i 'Nr <file>'` to
  splice a prepared block, or `head`/`cat`/`tail` reassembly.

- **A `sed -z` multi-line pattern must be proven unique before it is applied.**
  In the same session, a pattern meant to route nine `self.state.lock()` call
  sites through a new `lock_state()` helper also rewrote the helper's own body,
  making it call itself. Grep the pattern and count the matches first, and
  re-read the helper after any replace-all that could match its definition.
  Line numbers taken from an earlier grep also go stale after an intervening
  edit changes the line count — re-grep immediately before splicing.

- **`.cargo/config.toml` pins `x86_64-pc-windows-msvc`**, so release artifacts
  live under `target/x86_64-pc-windows-msvc/release/`, not `target/release/`.
  Anything that hard-codes the old path (installer sources, size checks) is
  silently looking at a stale or absent file.

- **Wait for a release workflow with short `gh run view <id> --json
  status,conclusion` queries, not `gh run watch`.** Here `gh run watch` dies
  on the first dropped connection (`failed to get run: Get ...: read tcp`),
  52 s into the 2.0.25 candidate build, and a loop of ten `gh run watch`
  calls used all ten attempts in 12 minutes while the run was still
  `in_progress`. The background task that wrapped it reported "completed
  (exit code 0)" both times, so a finished watcher is not a finished run.
  Poll `gh run view` every 30 s inside `timeout`, and read `conclusion`
  before downloading or publishing anything.

- **This repository's PowerShell scripts require pwsh 7, not Windows
  PowerShell 5.1.** `build-dictionary.ps1` uses `[IO.EnumerationOptions]`,
  a .NET Core-only type that 5.1 cannot resolve (`型 [IO.EnumerationOptions]
  が見つかりません`). CI already runs them under `shell: pwsh`; locally use
  `"/c/Program Files/PowerShell/7/pwsh" -NoProfile -ExecutionPolicy Bypass
  -File <script>`. Piping such a script to `tail` also swallows its nonzero
  exit, so read the log text for a thrown error rather than trusting the
  exit status.

- **The candidate popup can be screenshotted reliably only by asking the
  window to draw itself.** It is non-activating and click-through, so it sits
  under whatever is foreground and a desktop capture gets the occluding
  window. Find the `SakuraInputCandidates` HWND with `EnumWindows` and call
  `PrintWindow(hwnd, hdc, 2 /* PW_RENDERFULLCONTENT */)`. Two more facts that
  cost time: the popup opens *compact* (selected row plus footer, 50 px tall)
  and needs Tab — `candidate_expand` under the MS-IME keymap — to show every
  row, and `VK_CONVERT` is not delivered by `keybd_event`, so drive conversion
  with Space. Confirm a synthetic key actually reached the IME by logging
  `KeyDown` in the host: consumed keys arrive as `ProcessKey (229)`.

- **A quoted Bash-tool heredoc can collapse `\\` to `\`.** Verified
  2026-10-04 (#279): a Python script fed through `python - <<'EOF'` turned
  the source text `\\release-signing` into `\release-signing`, so Python
  wrote a carriage return into `.claude/memory/journal.md`. One lone CR makes
  git treat the file as binary, skip the autocrlf conversion, and show the
  whole file as changed. Write such a script with the file-writing tool, or
  build backslashes and CRs from `bytes([92])` / `bytes([13])`, then confirm
  `git diff --stat` shows only the appended lines.

- **The Edit tool strips trailing tabs from the lines it writes.** Verified
  2026-10-07 (#282): adding a row to `data/conversion-priorities.tsv` dropped
  the schema-required final tab of the empty annotation column from the new
  row and from the neighbouring row quoted as the anchor. After editing a
  `data/*.tsv` row, print the raw bytes of the changed rows (Python
  `repr()` on the CRLF-split line) and confirm `git diff` shows only the
  intended lines.

- **Restoring a swapped source file with its old mtime hides it from Cargo.**
  Verified 2026-10-13 (#306): a fail-before run copied origin/main's
  `lattice.rs` over the working file and later restored the new file with
  PowerShell `Copy-Item`, which keeps the source's LastWriteTime. The restored
  mtime predated the old-engine build outputs, so Cargo's mtime fingerprint
  did not rebuild `sakura-core`. The "new engine" shipped tests and candidate
  sweep that followed silently ran the old engine. After restoring a swapped
  file, `touch` it (or copy with bash `cp` without `-p`), and confirm that the
  next build prints `Compiling <crate>` before trusting the run.

- **Driving an agent in a sakura-tmux pane needs pane IDs and no MSYS path
  conversion.** Verified 2026-10-18 (#360 A1): the Sakura Editor `sakura-tmux`
  shim rejects `new-session -c` at runtime ("unsupported-surface"), and a
  detached session or new window never starts its shell (the pane stays 80x24
  with an empty title), so only an already-visible pane can host the agent.
  Address it by pane ID (`-t %3`); a session target must be quoted (`'$2'`).
  From Git Bash, `send-keys /clear` is rewritten to a Windows path unless the
  call is prefixed with `MSYS_NO_PATHCONV=1 MSYS2_ARG_CONV_EXCL='*'`. Send the
  text with `send-keys -l`, then `Enter` as a separate call, and watch for a
  standalone marker line rather than a substring, because the agent's own
  todo list echoes the marker text.

- **Run the workspace tests here with `--no-fail-fast`, and rule out a load
  flake by running the test executable next to busy-loop threads.** Verified
  2026-10-28 (#422). `cargo test --workspace` stops at the first failing test
  binary, so the binaries after it report nothing. On these 16 logical
  processors the `sakura-tsf` attach tests (a scripted peer inside the 50 ms
  `RECONNECT_BUDGET`), `sakura-engine` `dispatch::tests::*` (candidates inside
  the 10 ms `PREDICTION_TIMEOUT`) and four `prediction::tests::*` (a 100 ms
  `PredictionService::request`, only in a whole-library run) failed next to 16
  or 32 busy threads; no product window changed. A test that only needs a
  connected engine or a prediction waits up to 5 s for it
  (`Engine::attached_to`, `PredictionService::test_wait_for_worker`); a test
  whose subject is a window reruns its scenario through
  `crates/sakura-tsf/src/testing/busy_machine.rs`. The window tests that never
  failed under load are named in the #422 journal entry; rerun a failing one
  alone, then under load, before suspecting a change. Run the executable 10 or
  more times, from `main` first for the "before":

      Add-Type -TypeDefinition 'using System.Threading; public static class Burn { public static volatile bool Stop; public static void Start(int n) { for (int i = 0; i < n; i++) new Thread(() => { long x = 0; while (!Stop) x++; }) { IsBackground = true }.Start(); } }'
      [Burn]::Start(16)   # then 32; run the executable N times, count failing runs
      [Burn]::Stop = $true

  A run that never finishes is not a slow test: dump its threads with
  `cdb.exe -pv -p <pid> -y <target dir>\deps -c "~*kb 40;q"` (Windows SDK
  Debuggers; #422 found the `Mailbox::stop` lost wake-up that way).

- **A union merge of two appended journal entries drops the blank line between
  them.** Verified 2026-10-28: `git merge-file --union` left the second `## `
  heading directly under the first entry's last line, and
  `git commit --no-edit` kept git's `# Conflicts:` comment lines. Restore the
  blank line and commit with `git commit -F`.
