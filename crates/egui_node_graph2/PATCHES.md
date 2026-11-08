# Local patches (FactoryBuilder)

Vendored from egui_node_graph2 0.7.0 (MIT, https://github.com/trevyn/egui_node_graph2)
and wired in through `[patch.crates-io]` in the workspace Cargo.toml. Every change is
marked with a `FactoryBuilder:` comment.

- `GraphStyle` on `GraphEditorState`: node width, fills, text, selection colour,
  rounding, port radius, wire width and finder colours instead of hard-coded values.
- Right-click on a node reports `GraphResponse::node_context_menu` (for a
  Duplicate/Delete menu); the finder only opens on empty canvas.
- No close (x) button on nodes; deleting is the app's job (context menu, Delete key).
- Node finder: styled from `GraphStyle`, entries without a category are filtered by
  the query too, Enter picks the first match, categories open while searching.
- `Graph::remove_node` also removes the deleted node's entries from `connections`
  (they used to linger as empty lists keyed by dead `InputId`s and panicked on lookup).
- `GraphResponse::node_rects`: the screen rect of each node drawn this frame (the app
  measures node heights for its layered layout).
- Ports: one filled bubble per connection plus a hollow ring for the free slot, on
  inputs and outputs alike (was one merged bar); each wire leaves from its own bubble.
- Port padding scales with zoom (it used to stay full-size, so nodes grew relatively
  taller when zoomed out and overlapped).
- `NodeDataTrait::titlebar_strip`: optional colour strip across the top of the title bar.
- Each port bubble is its own hit target: hover lights only that bubble; dragging a
  filled bubble pulls out exactly that wire (new `NodeResponse::DisconnectOutputEvent`
  for outputs, which keeps the wire's input end attached); the hollow ring starts a
  new wire; dropping on any bubble of a compatible port connects.
- Wide (multi-connection) inputs call `value_widget` like single inputs (was a bare label).
- Title bar: `top_bar_ui` is laid out first (right to left); the title takes the rest
  and truncates with "…" (hover shows the full name).
- `GraphResponse::wires`: each wire drawn this frame with its bezier points (the app
  labels wires with the belt or pipe they need).
