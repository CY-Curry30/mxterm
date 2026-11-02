# Native RDP connection feedback

The Windows RDP desktop is an ActiveX child HWND outside the React tree. Its connection feedback lives in a small native child surface above that desktop. It must not hide the desktop or infer connection success from painting, display resize success, or elapsed time.

## Contract

- `rdpLaunchConnection` reads resolved app tokens through the lazy shared `nativeAppearance` helper. The optional `appearance` request field contains Windows COLORREF colors (`0x00BBGGRR`) and the reduced-motion preference; it does not belong to the saved connection profile. Composite translucent colors over the app background before crossing into the opaque native HWND surface. The optional `chrome`, `chrome_line`, `active`, `hover` and `success` fields map the title strip and tab states to the same global tokens.
- The native surface snapshots light, explicit dark, or system-dark appearance at launch. Changing theme while a native session is open takes effect on its next launch. Older callers without appearance use Windows system colors and a static indicator.
- `OnConnecting` starts feedback. `OnConnected` changes it to waiting for login and moves it above the remote sign-in controls. Only `OnLoginComplete` / `OnLoginCompleted` or `OnAutoReconnected` removes pending feedback.
- Waiting longer than 20 seconds changes the hint only. It does not create a timeout, failure, or success state.
- The initial resolution is configured before Connect. Subsequent display updates wait for login completion, then restart the existing resize retry window, including for slow logins. Transport connection alone does not guarantee that the server accepts display updates.
- Authentication/dialog events temporarily hide feedback so remote prompts remain operable.
- Disconnect and fatal events stop animation and retain numeric reason codes. Do not treat every Classic `OnLogonError` as fatal: some codes are informational.
- Retry reuses the control with a fresh event subscription and state. It must not store a second copy of credentials. Closing the native tab cancels the session through the existing close path.
- COM callbacks enqueue values through the WinForms host Form’s `BeginInvoke`. They must not borrow host state, call Connect/Disconnect, or mutate byref decision parameters.
- Drop the subscription before disconnecting or destroying its control. Inactive tabs hide their feedback. The WinForms maintenance timer updates waiting hints and schedules bounded display synchronization only after a real login-completed event.

## Native tabs

- Keep a single 40 DIP title strip with the app mark, connection tabs and native window controls on the same row. Use compact corners, a fine border, token colors, vector glyphs, connection status dots and full-title tooltips. Scale geometry and typography for the host monitor.
- New connections start from the existing main-window connection list and join the shared native host. Do not add a separate connection-creation button or persist native tab order in connection profiles.
- Show up to five tabs as space permits. An overflow menu lists the remaining sessions with their connection status. Selecting an overflow item brings that session into the visible strip; the active tab must always remain visible. Recalculate visibility on every activation even when no controls are added or resized.
- Clicking a tab activates its existing AxHost and explicitly restores remote desktop focus. Dismissing the overflow menu restores focus after the native menu closes. Authentication dialogs retain their normal focus flow. Do not replace AxHost keyboard preprocessing to implement tab interaction.
- The close button and middle click share the same session-close path. Closing a background tab preserves the active session; closing the active tab selects its right neighbor, or the previous tab at the end. Closing the final tab exits the host. Preserve backend session ids through these operations.
- Native child HWNDs cannot contain Radix controls. Use real accessible WinForms buttons and ContextMenuStrip with the shared token palette for this surface; React menus continue to use the shared Radix components.

## Verification

- Run TypeScript checking, production build, startup module boundary checking, Rust formatting, and the focused `rdp::` tests. Run `node scripts/check-rdp-host-runtime.mjs <built-helper.exe>` on Windows to exercise installed controls, request correlation, shutdown, and parent-pipe EOF.
- The packaged helper’s `--self-test` creates eight actual installed AxHost controls without connecting to a server. It checks all keyboard modes, scale properties, feedback transitions, visible/overflow activation and focus, background/current/middle-click close, and final cleanup. It also checks light/dark layouts at 720/1280 DIP widths and 100%/150%/200% scale. An optional `MXTERM_RDP_PREVIEW_DIR` writes generated control bitmaps for visual review. The ignored Rust manual test explicitly connects using an existing saved profile and the production IPC launch path.
- The manual test accepts an optional `MXTERM_RDP_TEST_APPEARANCE` JSON snapshot from `readNativeAppearance` for testing the app palette without starting its WebView. This fixture contains theme colors only.
- The Windows test executable needs the Common Controls v6 manifest used by the desktop application. If the test runner exits with `0xc0000139`, add this dependency to a disposable test executable with the Windows SDK manifest tool before running assertions. Do not count `--no-run` as passing tests.
- Verify actual remote login, slow login, certificate prompts, disconnect/retry, automatic reconnection, multiple tabs, and DPI changes in the desktop app. The login-completed event is not a first-frame guarantee.
- The Windows host uses the installed classic MSTSC control through standard WinForms AxHost. There is no separate custom OLE container or Modern-control execution path. Build and launch failures may fall back to the external client; connection/authentication failures remain visible in the native feedback surface.
