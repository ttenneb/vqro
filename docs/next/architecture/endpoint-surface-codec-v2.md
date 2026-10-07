# Endpoint surface codec v2

Status: recorded generation-scoped extension under the accepted 2026-10-04
no-floor decision ([Nested panes](nested-panes.md)). Awaiting reviewer
reconfirmation; not a new authority.

## Decision

Nested panes add a pane whose content is an extension view. The client must know
each view pane's content identity and the terminals it covers, and the existing
`shell.surface.v1` frame cannot carry them without changing its byte shape. The
surface lane therefore moves to one new codec name, `shell.surface.v2`.

Authority: the human accepted **option B, "no compatibility floor while Vqro is
in development,"** on 2026-10-04 ([Nested panes](nested-panes.md), Decisions).
That entry says the design may change the client protocol and the frozen
generation-1 formats and fixtures freely, older clients are not supported, and
it requires this `AGENTS.md` reconciliation and the fixture disposition below.

## Why the payload cannot be added in place

`ServerMessage` is bincode behind a `u32` length prefix, with a frozen variant
order for endpoint generation 1. Its only extensible carrier is
`EndpointControl { kind: String, data: String }`, a UTF-8 JSON envelope on the
control writer lane; the surface is a large bincode frame (`CellData` per cell)
on the render lane. Two facts force a new codec name rather than a field on the
v1 frame:

1. bincode cannot add an optional field to a struct on a live lane. A field the
   serializer skips desynchronizes the reader (`UnexpectedEnd`).
2. The endpoint hello checks generation and codec names only. It does not carry
   the private `PROTOCOL_VERSION`, so a generation-1 peer and the new server both
   claim the page surface lane and then disagree on the struct.

Routing per-frame surfaces through the JSON `EndpointControl` envelope would
also put them on the control lane's ordering and drop semantics and rework the
retained-surface, patch and graphics-asset paths. It is not the smaller change.

## What changes

- One `ServerMessage` variant is appended after `EndpointControl`:
  `PaneSurfaceV2(PaneSurfaceFrameV2)` (bincode tag 21). Tags `0..=20` are
  unchanged.
- `PaneSurfaceFrameV2 { frame: PaneSurfaceFrame, views: Vec<PaneSurfaceViewEntryV2> }`
  carries the unchanged v1 frame value plus
  `PaneSurfaceViewEntryV2 { pane_id, extension, view, scope }`.
- `PaneSurfacePane` returns to its terminal-only shape and
  `PaneSurfaceFrame` / `PaneSurfacePatch` bytes are unchanged. Patches stay the
  plain `PaneSurfacePatch` for both codecs: view descriptors change only with a
  topology change, which forces a new full surface.
- Serving is `shell.surface.v2` only. `shell.surface.v1` is retired **from
  serving**, not from decode. Hello advertises `["shell.surface.v2","shell.surface.v1"]`;
  the welcome names `shell.surface.v2`.
- Refusals are client-local, in both directions. A peer whose hello does not
  advertise `shell.surface.v2` is refused with the hello rejection
  `unsupported_surface_codec` ("update this client"); a new client whose welcome
  names only `shell.surface.v1` fails the handshake with "update this machine".
  No v1 frame or patch is sent, no shared geometry changes, and no supported
  client is disconnected.

## Fixture and expectation disposition

Preserved, not re-blessed: the `_v1` endpoint fixtures, the
`PaneSurface` / `PaneSurfacePatch` bincode decodes and tags 13/19/20, and every
historical value digest. New serving-policy behavior is covered by new tests
with new names (`v1_only_shell_hello_is_refused_client_locally_without_a_surface`,
`pane_surface_v2_frame_round_trips_view_descriptors`), never by editing a frozen
expectation in place.

## Removal condition

Remove `ServerMessage::PaneSurfaceV2` and the `shell.surface.v2` name when the
surface lane is re-cut (a later endpoint generation). Until then this is the
only recorded append to the generation-1 `ServerMessage` enum.
