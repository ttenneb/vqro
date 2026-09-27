# Reference host document extension

This package is the minimal language-neutral reference for
`vqro.extension-manifest.v1`, protocol 2, `host.terminal-groups.v1`, and
`host.document.v1`. Its service-local ID is `document`; the runtime ID is
`example.reference-document.document`; its document namespace is
`example.reference-document`.

Validate without execution with `vqro plugin validate .`. Explicitly run the
production handshake and canonical render fixture with `vqro plugin conformance
.`. Both fixture files are required for host-document conformance. Python 3 is
required, so the manifest claims only Linux and macOS.

To exercise the package in a development checkout:

```bash
vqro plugin link ./extensions/reference-host-document
vqro plugin list
```

The service priority is intentionally higher than the bundled Collections
provider, so its terminal-group navigation document becomes the selected
projection while linked. Disable or unlink it to return immediately to the
bundled provider; existing PTYs remain reachable throughout.

Extensions run as trusted same-user code. Capability grants fence protocol host
effects; they are not an OS sandbox. This example receives stable terminal
references only and never owns PTYs, placement, focus, input, or rendering.
