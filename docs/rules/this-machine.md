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
