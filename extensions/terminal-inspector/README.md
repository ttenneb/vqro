# Terminal inspector extension

Linux reference for vqro's supervised pane-inspector protocol. Supervised services use process-group containment on Unix and kill-on-close Job Objects on Windows; this Python example remains Linux-only as declared by its manifest.

```bash
vqro plugin link ./extensions/terminal-inspector
```

Restart the vqro server after linking. Open a pane context menu and select **Inspect terminal**, or use a Collection header’s inspect control. The host collects privacy-safe telemetry only on request, sends it asynchronously to this supervised formatter, validates the returned contribution, and renders it as a native embedded pane component. The extension never receives an arbitrary drawing surface or the vqro API socket by default.

This subprocess is trusted same-user code and is **not sandboxed**. Manifest requests and handshake grants constrain only host protocol effects. This reference service requests `view_contribution` and `terminal_interrupt`; the latter can produce only the host-defined Ctrl-C effect for the contribution's exact pane and always requires a vqro-rendered confirmation.
