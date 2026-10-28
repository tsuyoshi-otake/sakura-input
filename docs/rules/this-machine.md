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

- **Run the workspace tests here with `--no-fail-fast`; two test families fail
  under load.** Verified 2026-10-28 (integration of #416, #418 and #420):
  `cargo test --workspace` stops at the first failing test binary, so the
  binaries after it report nothing. Run 1 failed 24 `sakura-tsf` attach tests
  (50 ms `RECONNECT_BUDGET`: "the handshake must have completed"), run 2
  failed 4 `sakura-engine` prediction tests (10 ms `PREDICTION_TIMEOUT`: empty
  `prediction_cache` right after `type_word`), each binary passed alone, and
  run 3 with `RUST_TEST_THREADS=8` passed. A CPU sample between the runs
  showed six `node` processes of other sessions at 0.7-0.9 core each on 16
  logical processors. Rerun the failing binary alone before suspecting a
  change that does not touch these paths.

- **A union merge of two appended journal entries drops the blank line between
  them.** Verified 2026-10-28: `git merge-file --union` left the second `## `
  heading directly under the first entry's last line, and
  `git commit --no-edit` kept git's `# Conflicts:` comment lines. Restore the
  blank line and commit with `git commit -F`.
