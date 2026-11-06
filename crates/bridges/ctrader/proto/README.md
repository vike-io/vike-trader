# Vendored cTrader Open API protobuf schema

Source: https://github.com/spotware/openapi-proto-messages
Pinned commit: `3fd8bddfbe0cfc2ecfda079623dc4e498af11e66` (fetched 2026-07-14 via
`git clone --depth 1`).

Files vendored verbatim (no edits):

- `OpenApiCommonMessages.proto` — the transport envelope (`ProtoMessage`, `ProtoErrorRes`,
  `ProtoHeartbeatEvent`).
- `OpenApiCommonModelMessages.proto` — **not** in the original task list, but a required
  transitive dependency: `OpenApiCommonMessages.proto` imports it directly (`ProtoPayloadType`,
  `ProtoErrorCode`). Vendored so `build.rs`'s `prost_build::compile_protos` include path
  resolves it.
- `OpenApiModelMessages.proto` — domain model enums/messages (symbols, orders, positions,
  trader, etc).
- `OpenApiMessages.proto` — the Open API v2 request/response/event messages
  (`ProtoOAApplicationAuthReq`, `ProtoOANewOrderReq`, `ProtoOASpotEvent`, ...), imports
  `OpenApiModelMessages.proto`.

All four files declare `syntax = "proto2"` and no `package`. `OpenApiCommonMessages.proto`
also declares a custom message option (`ProtoErrorRes`/`ProtoHeartbeatEvent`'s
`[default = ERROR_RES]`/`[default = HEARTBEAT_EVENT]` defaults come from the
`ProtoPayloadType` enum) — prost-build compiles proto2 fine and ignores unknown custom
options; this is expected, not an error to "fix".

Do not hand-edit these files. If the upstream schema changes, re-clone and re-copy, then
update the pinned commit SHA above.
