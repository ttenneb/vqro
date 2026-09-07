# Terminal inspector extension

Reference Linux alpha extension for vqro's supervised service and declarative inspector protocol.

```bash
vqro plugin link ./extensions/terminal-inspector
```

Restart the vqro server after linking. Open a pane context menu and select **Inspect terminal**. The host collects privacy-safe telemetry only on request, sends it to this supervised formatter, validates the returned document, and renders it with native client styling.
