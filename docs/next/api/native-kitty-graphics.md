# Native Kitty graphics

Vqro forwards Kitty image state parsed by its host-owned Ghostty terminal runtime.
Applications send ordinary Kitty graphics escape sequences to their PTY; they no
longer create graphics layers through the JSON API. Rendering and placement are
client presentation, not package authority or an alternate execution channel.

## Breaking raw API removal

`pane.graphics.info`, `pane.graphics.set`, `pane.graphics.clear` and
`pane.graphics.stream` have been removed. Requests receive `unknown_method`,
without entering the old streaming transport or changing terminal state.
The raw API contract is still `vqro.api.v1`: this removal does **not** add a new
admission handshake, change the API envelope, or repin extensions/adapters.
Use the generated `vqro-api.schema.json` for the current method set. The inherited
`herdr-api.schema.json` remains frozen historical input, not the Vqro method list.

## Presentation and ownership

The terminal runtime owns image lifetime; clients own placement, clipping and
output. Collection list views and the inspector suppress affected pane placements
without discarding shared assets. Overlays crop visible image pieces within a
bounded frame budget. Hidden panes continue parsing output without requesting
presentation merely to update their terminal state.

Delivered images that leave the viewport can remain loaded, bounded to 16 images
and 64 MiB of decoded image storage per delivery cache. Replacement/deletion and
endpoint generation changes invalidate the corresponding ownership records.
Hide/show and scroll restoration reuse uploaded pixels where possible.

## Local file optimization and fallback

File delivery is opportunistic. Server-owned files require a proven shared local
transport and exact cell geometry; an SSH bridge exposing a local socket is not
proof of a shared filesystem. Files are private, no-follow, size-checked and owned
for their complete transfer lifetime. Failed/unsupported transfers fall back to
inline pixels, independently for each client.

For a directly attached local Ghostty terminal, the final output boundary may
convert raw uploads to client-owned temporary files. A silent 1×1 query must first
be consumed by the terminal. SSH, nested multiplexer markers (both Herdr and Vqro),
non-terminal output, failed probes and outstanding file/byte budgets prevent this
optimization. Published paths remain owned until terminal consumption or cleanup;
merely writing a file is not evidence that the terminal accepted it.

On Linux, supported same-filesystem CoW snapshots can retain immutable raw image
sources without copying pixels through Rust. Unsupported snapshot operations use
the normal Ghostty loader. Mutable producer paths are never retained as ownership.
Experimental encoded-PNG retention stays off in production; PNG validation still
decodes the image normally.

Set `[terminal] kitty_graphics = false` to opt out. This disables terminal image
storage as well as client graphics output. Native Ghostty macOS/Fedora trials
remain required before deployment; Linux unit tests are not native-platform proof.
