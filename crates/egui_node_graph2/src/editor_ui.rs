use std::collections::HashSet;
use std::num::NonZeroU32;


use super::*;
use egui::epaint::{CubicBezierShape, RectShape};
use egui::*;

/// Mapping from parameter id to positions of hooks it contains.
///
/// Outputs and short inputs always only have one hook, so the value is
/// just `vec![port_position]`. Wide inputs may have multiple hooks.
pub type PortLocations = std::collections::HashMap<AnyParameterId, Vec<Pos2>>;

/// Destination positions of connections made to a given input.
///
/// This is not equivalent to [`PortLocations`] because connections may be moved
/// around (e.g. while an in-progress connection is hovered over a wide port),
/// while hooks within a port are strictly a function of the port.
pub type ConnLocations = std::collections::HashMap<InputId, Vec<Pos2>>;

/// Rectangle containing each node.
/// FactoryBuilder: how near a node (screen px) a dragged wire snaps to it.
const SNAP_MARGIN: f32 = 8.0;

pub type NodeRects = std::collections::HashMap<NodeId, Rect>;

const DISTANCE_TO_CONNECT: f32 = 10.0;

/// Nodes communicate certain events to the parent graph when drawn. There is
/// one special `User` variant which can be used by users as the return value
/// when executing some custom actions in the UI of the node.
#[derive(Clone, Debug)]
pub enum NodeResponse<UserResponse: UserResponseTrait, NodeData: NodeDataTrait> {
    ConnectEventStarted(NodeId, AnyParameterId),
    ConnectEventEnded {
        output: OutputId,
        input: InputId,
        /// FactoryBuilder: bubble on the output to put the wire at.
        output_hook: usize,
        /// Index of the connection in wide input ports.
        ///
        /// If the input isn't a wide port this is always 0 and may be ignored.
        input_hook: usize,
    },
    CreatedNode(NodeId),
    SelectNode(NodeId),
    /// As a user of this library, prefer listening for `DeleteNodeFull` which
    /// will also contain the user data for the deleted node.
    DeleteNodeUi(NodeId),
    /// Emitted when a node is deleted. The node will no longer exist in the
    /// graph after this response is returned from the draw function, but its
    /// contents are passed along with the event.
    DeleteNodeFull {
        node_id: NodeId,
        node: Node<NodeData>,
    },
    DisconnectEvent {
        output: OutputId,
        input: InputId,
    },
    /// FactoryBuilder: a wire was pulled off an *output* bubble; the wire's
    /// input end stays put and the output end follows the pointer.
    DisconnectOutputEvent {
        output: OutputId,
        input: InputId,
    },
    /// Emitted when a node is interacted with, and should be raised
    RaiseNode(NodeId),
    MoveNode {
        node: NodeId,
        drag_delta: Vec2,
    },
    User(UserResponse),
}

/// The return value of [`draw_graph_editor`]. This value can be used to make
/// user code react to specific events that happened when drawing the graph.
#[derive(Clone, Debug)]
pub struct GraphResponse<UserResponse: UserResponseTrait, NodeData: NodeDataTrait> {
    /// Events that occurred during this frame of rendering the graph. Check the
    /// [`UserResponse`] type for a description of each event.
    pub node_responses: Vec<NodeResponse<UserResponse, NodeData>>,
    /// Is the mouse currently hovering the graph editor? Note that the node
    /// finder is considered part of the graph editor, even when it floats
    /// outside the graph editor rect.
    pub cursor_in_editor: bool,
    /// Is the mouse currently hovering the node finder?
    pub cursor_in_finder: bool,
    /// FactoryBuilder: a node was right-clicked (node, pointer position).
    /// The finder only opens for right-clicks on empty canvas.
    pub node_context_menu: Option<(NodeId, Pos2)>,
    /// FactoryBuilder: screen rect of every node drawn this frame.
    pub node_rects: NodeRects,
    /// FactoryBuilder: bubble (hook) positions of every port drawn this frame.
    pub port_locations: PortLocations,
    /// FactoryBuilder: every wire drawn this frame with its cubic bezier
    /// points (screen space), for labels on wires.
    pub wires: Vec<(InputId, OutputId, [Pos2; 4])>,
    /// FactoryBuilder: a wire being dragged from this port was let go on
    /// empty canvas, at this screen position.
    pub wire_dropped: Option<(NodeId, AnyParameterId, Pos2)>,
    /// FactoryBuilder: the other outputs pulled along with that wire.
    pub wire_dropped_extras: Vec<OutputId>,
}
impl<UserResponse: UserResponseTrait, NodeData: NodeDataTrait> Default
    for GraphResponse<UserResponse, NodeData>
{
    fn default() -> Self {
        Self {
            node_responses: Default::default(),
            cursor_in_editor: false,
            cursor_in_finder: false,
            node_context_menu: None,
            node_rects: Default::default(),
            port_locations: Default::default(),
            wires: Vec::new(),
            wire_dropped: None,
            wire_dropped_extras: Vec::new(),
        }
    }
}
pub struct GraphNodeWidget<'a, NodeData, DataType, ValueType> {
    pub position: &'a mut Pos2,
    pub graph: &'a mut Graph<NodeData, DataType, ValueType>,
    pub port_locations: &'a mut PortLocations,
    pub conn_locations: &'a mut ConnLocations,
    pub node_rects: &'a mut NodeRects,
    pub node_id: NodeId,
    pub ongoing_drag: Option<(NodeId, AnyParameterId)>,
    pub selected: bool,
    pub pan: egui::Vec2,
    pub style: GraphStyle,
}

impl<NodeData, DataType, ValueType, NodeTemplate, UserResponse, UserState, CategoryType>
    GraphEditorState<NodeData, DataType, ValueType, NodeTemplate, UserState>
where
    NodeData: NodeDataTrait<
        Response = UserResponse,
        UserState = UserState,
        DataType = DataType,
        ValueType = ValueType,
    >,
    UserResponse: UserResponseTrait,
    ValueType:
        WidgetValueTrait<Response = UserResponse, UserState = UserState, NodeData = NodeData>,
    NodeTemplate: NodeTemplateTrait<
        NodeData = NodeData,
        DataType = DataType,
        ValueType = ValueType,
        UserState = UserState,
        CategoryType = CategoryType,
    >,
    DataType: DataTypeTrait<UserState>,
    CategoryType: CategoryTrait,
{
    #[must_use]
    pub fn draw_graph_editor(
        &mut self,
        ui: &mut Ui,
        all_kinds: impl NodeTemplateIter<Item = NodeTemplate>,
        user_state: &mut UserState,
        prepend_responses: Vec<NodeResponse<UserResponse, NodeData>>,
    ) -> GraphResponse<UserResponse, NodeData> {
        ui.set_clip_rect(ui.max_rect());
        let clip_rect = ui.clip_rect();
        // Zoom may have never taken place, so ensure we use parent style
        if !self.pan_zoom.started {
            self.zoom(ui, 1.0);
            self.pan_zoom.started = true;
        }

        // Zoom only within area where graph is shown
        if ui.rect_contains_pointer(clip_rect) {
            let scroll_delta = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll_delta != 0.0 {
                let zoom_delta = (scroll_delta * 0.002).exp();
                self.zoom(ui, zoom_delta);
            }
        }

        // Render graph zoomed
        let zoomed_style = self.pan_zoom.zoomed_style.clone();
        let graph_response = show_zoomed(ui.style().clone(), zoomed_style, ui, |ui| {
            self.draw_graph_editor_inside_zoom(ui, all_kinds, user_state, prepend_responses)
        });

        graph_response
    }

    /// Reset zoom to 1.0
    pub fn reset_zoom(&mut self, ui: &Ui) {
        let new_zoom = 1.0 / self.pan_zoom.zoom;
        self.zoom(ui, new_zoom);
    }

    /// Zoom within the where you call `draw_graph_editor`. Use values like 1.01, or 0.99 to zoom.
    /// For example: `let zoom_delta = (scroll_delta * 0.002).exp();`
    pub fn zoom(&mut self, ui: &Ui, zoom_delta: f32) {
        // Update zoom, and styles
        let zoom_before = self.pan_zoom.zoom;
        self.pan_zoom.zoom(ui.clip_rect(), ui.style(), zoom_delta);
        if zoom_before != self.pan_zoom.zoom {
            let actual_delta = self.pan_zoom.zoom / zoom_before;
            self.update_node_positions_after_zoom(actual_delta);
        }
    }

    fn update_node_positions_after_zoom(&mut self, zoom_delta: f32) {
        // FactoryBuilder: zoom towards the centre as before, but by scaling
        // node positions about the graph origin and moving the pan, so a
        // node's position is always (graph position x zoom). The original
        // kept the pan and offset the nodes instead, which made positions
        // depend on each person's zoom history: cursors and shared positions
        // in planning rooms drifted between people at different zooms.
        let half_size = self.pan_zoom.clip_rect.size() / 2.0;
        for (_id, node_pos) in self.node_positions.iter_mut() {
            *node_pos = (node_pos.to_vec2() * zoom_delta).to_pos2();
        }
        // Keep the point at the centre of the view where it was.
        self.pan_zoom.pan = half_size - (half_size - self.pan_zoom.pan) * zoom_delta;
    }

    fn draw_graph_editor_inside_zoom(
        &mut self,
        ui: &mut Ui,
        all_kinds: impl NodeTemplateIter<Item = NodeTemplate>,
        user_state: &mut UserState,
        prepend_responses: Vec<NodeResponse<UserResponse, NodeData>>,
    ) -> GraphResponse<UserResponse, NodeData> {
        // This causes the graph editor to use as much free space as it can.
        // (so for windows it will use up to the resizeably set limit
        // and for a Panel it will fill it completely)
        let editor_rect = ui.max_rect();
        let resp = ui.allocate_rect(editor_rect, Sense::hover());

        let cursor_pos = ui
            .ctx()
            .input(|i| i.pointer.hover_pos().unwrap_or(Pos2::ZERO));
        let mut cursor_in_editor = resp.contains_pointer();
        let mut cursor_in_finder = false;

        // Gets filled with the node metrics as they are drawn
        let mut port_locations = PortLocations::new();
        let mut node_rects = NodeRects::new();

        // actual dest location of each connection
        let mut conn_locations = ConnLocations::default();

        // The responses returned from node drawing have side effects that are best
        // executed at the end of this function.
        let mut delayed_responses: Vec<NodeResponse<UserResponse, NodeData>> = prepend_responses;

        // Used to detect when the background was clicked
        let mut click_on_background = false;

        // Used to detect drag events in the background
        let mut drag_started_on_background = false;
        let mut drag_released_on_background = false;

        debug_assert_eq!(
            self.node_order.iter().copied().collect::<HashSet<_>>(),
            self.graph.iter_nodes().collect::<HashSet<_>>(),
            "The node_order field of the GraphEditorself was left in an \
        inconsistent self. It has either more or less values than the graph."
        );

        // Allocate rect before the nodes, otherwise this will block the interaction
        // with the nodes.
        let r = ui.allocate_rect(ui.min_rect(), Sense::click().union(Sense::drag()));
        if r.clicked() {
            click_on_background = true;
        } else if r.drag_started() {
            drag_started_on_background = true;
        } else if r.drag_stopped() {
            drag_released_on_background = true;
        }

        // FactoryBuilder: wires go under the nodes (filled in once they're
        // drawn and their ports are known), as in other node editors.
        let wire_slot = ui.painter().add(Shape::Noop);

        /* Draw nodes */
        for node_id in self.node_order.iter().copied() {
            let responses = GraphNodeWidget {
                position: self.node_positions.get_mut(node_id).unwrap(),
                graph: &mut self.graph,
                port_locations: &mut port_locations,
                conn_locations: &mut conn_locations,
                node_rects: &mut node_rects,
                node_id,
                ongoing_drag: self.connection_in_progress,
                selected: self
                    .selected_nodes
                    .iter()
                    .any(|selected| *selected == node_id),
                pan: self.pan_zoom.pan + editor_rect.min.to_vec2(),
                style: self.style,
            }
            .show(&self.pan_zoom, ui, user_state);

            // Actions executed later
            delayed_responses.extend(responses);
        }

        /* Draw the node finder, if open */
        let mut should_close_node_finder = false;
        if let Some(ref mut node_finder) = self.node_finder {
            let mut node_finder_area = Area::new(Id::new("node_finder")).order(Order::Foreground);
            if let Some(pos) = node_finder.position {
                node_finder_area = node_finder_area.current_pos(pos);
            }
            node_finder_area.show(ui.ctx(), |ui| {
                if let Some(node_kind) = node_finder.show(ui, all_kinds, user_state, &self.style) {
                    let new_node = self.graph.add_node(
                        node_kind.node_graph_label(user_state),
                        node_kind.user_data(user_state),
                        |graph, node_id| node_kind.build_node(graph, user_state, node_id),
                    );
                    self.node_positions.insert(
                        new_node,
                        node_finder.position.unwrap_or(cursor_pos)
                            - self.pan_zoom.pan
                            - editor_rect.min.to_vec2(),
                    );
                    self.node_order.push(new_node);

                    should_close_node_finder = true;
                    delayed_responses.push(NodeResponse::CreatedNode(new_node));
                }
                let finder_rect = ui.min_rect();
                // If the cursor is not in the main editor, check if the cursor is in the finder
                // if the cursor is in the finder, then we can consider that also in the editor.
                if finder_rect.contains(cursor_pos) {
                    cursor_in_editor = true;
                    cursor_in_finder = true;
                }
            });
        }
        if should_close_node_finder {
            self.node_finder = None;
        }

        // FactoryBuilder: a wire dragged over a node (or within a few pixels
        // of it) snaps to that node's first port that fits it, as in other
        // node editors; letting go there connects it.
        let snap: Option<(NodeId, AnyParameterId)> = self.connection_in_progress.and_then(|(origin_node, origin)| {
            let typ = self.graph.any_param_type(origin).ok()?;
            let node = self
                .node_order
                .iter()
                .rev()
                .copied()
                .find(|n| *n != origin_node && node_rects.get(n).is_some_and(|r| r.expand(SNAP_MARGIN).contains(cursor_pos)))?;
            let takes = |p: AnyParameterId, us: &mut UserState| self.graph[node].user_data.accepts_wire(node, p, &self.graph, us);
            let port = match origin {
                AnyParameterId::Output(_) => self.graph[node]
                    .inputs
                    .iter()
                    .map(|(_, i)| *i)
                    .filter(|i| self.graph[*i].typ == *typ && !matches!(self.graph[*i].kind(), InputParamKind::ConstantOnly))
                    .map(AnyParameterId::Input)
                    .find(|p| takes(*p, user_state))?,
                AnyParameterId::Input(_) => self.graph[node]
                    .outputs
                    .iter()
                    .map(|(_, o)| *o)
                    .filter(|o| self.graph[*o].typ == *typ)
                    .map(AnyParameterId::Output)
                    .find(|p| takes(*p, user_state))?,
            };
            Some((node, port))
        });
        // Where a snapped wire ends: a knot's middle, or the port's free bubble.
        let snap_pos = snap.and_then(|(node, port)| {
            if self.graph[node].user_data.knot_size().is_some() {
                node_rects.get(&node).map(|r| r.center())
            } else {
                port_locations.get(&port).and_then(|h| h.last().copied())
            }
        });

        // draw in-progress connections
        if let Some((_, ref locator)) = self.connection_in_progress {
            let port_type = self.graph.any_param_type(*locator).unwrap();
            let connection_color = port_type.data_type_color(user_state);

            // outputs can't be wide yet so this is fine.
            let start_pos = *port_locations[locator].last().unwrap();

            // Find a port to connect to
            fn snap_to_ports<
                NodeData,
                UserState,
                DataType: DataTypeTrait<UserState>,
                ValueType,
                Key: slotmap::Key + Into<AnyParameterId>,
                Value,
            >(
                graph: &Graph<NodeData, DataType, ValueType>,
                port_type: &DataType,
                ports: &SlotMap<Key, Value>,
                port_locations: &PortLocations,
                cursor_pos: Pos2,
            ) -> Pos2 {
                ports
                    .iter()
                    .find_map(|(port_id, _)| {
                        let compatible_ports = graph
                            .any_param_type(port_id.into())
                            .map(|other| other == port_type)
                            .unwrap_or(false);

                        if compatible_ports {
                            port_locations.get(&port_id.into()).and_then(|hooks| {
                                hooks
                                    .iter()
                                    .min_by(|hook1, hook2| {
                                        hook1
                                            .distance(cursor_pos)
                                            .partial_cmp(&hook2.distance(cursor_pos))
                                            .unwrap()
                                    })
                                    .filter(|nearest_hook| {
                                        nearest_hook.distance(cursor_pos) < DISTANCE_TO_CONNECT
                                    })
                                    .copied()
                            })
                        } else {
                            None
                        }
                    })
                    .unwrap_or(cursor_pos)
            }

            // A bubble close by first, then the node under the pointer.
            let or_node = |p: Pos2| if p == cursor_pos { snap_pos.unwrap_or(p) } else { p };
            let (src_pos, dst_pos) = match locator {
                AnyParameterId::Output(_) => (
                    start_pos,
                    or_node(snap_to_ports(
                        &self.graph,
                        port_type,
                        &self.graph.inputs,
                        &port_locations,
                        cursor_pos,
                    )),
                ),
                AnyParameterId::Input(_) => (
                    or_node(snap_to_ports(
                        &self.graph,
                        port_type,
                        &self.graph.outputs,
                        &port_locations,
                        cursor_pos,
                    )),
                    start_pos,
                ),
            };
            draw_connection(
                &self.pan_zoom,
                ui.painter(),
                src_pos,
                dst_pos,
                connection_color,
                self.style.wire_width,
            );
            for x in &self.connection_extras {
                if let Some(start) = port_locations.get(&AnyParameterId::Output(*x)).and_then(|h| h.last()) {
                    draw_connection(&self.pan_zoom, ui.painter(), *start, dst_pos, connection_color, self.style.wire_width);
                }
            }
        }

        // draw existing connections
        // FactoryBuilder: each wire starts at its own bubble on the output.
        // Faint wires first, so solid ones cross over them.
        let mut wires = Vec::new();
        let (mut faint, mut solid): (Vec<Shape>, Vec<Shape>) = (Vec::new(), Vec::new());
        let mut wire_order: std::collections::HashMap<OutputId, Vec<InputId>> = Default::default();
        for (input, outputs) in self.graph.iter_connection_groups() {
            for (hook_n, &output) in outputs.iter().enumerate() {
                let port_type = self
                    .graph
                    .any_param_type(AnyParameterId::Output(output))
                    .unwrap();
                let connection_color = port_type.wire_color(user_state, self.graph[output].node, self.graph[input].node);
                let order = wire_order.entry(output).or_insert_with(|| self.graph.output_connections(output));
                let k = order.iter().position(|i| *i == input).unwrap_or(0);
                // Knots: wires meet at the middle, from any side.
                let (from, to) = (self.graph[output].node, self.graph[input].node);
                let knot = |n: NodeId| self.graph[n].user_data.knot_size().is_some();
                let knot_at = |n: NodeId| node_rects.get(&n).map(|r| r.center());
                let src_pos = match knot_at(from).filter(|_| knot(from)) {
                    Some(c) => c,
                    None => {
                        let hooks = &port_locations[&AnyParameterId::Output(output)];
                        hooks[k.min(hooks.len() - 1)]
                    }
                };
                let dst_pos = match knot_at(to).filter(|_| knot(to)) {
                    Some(c) => c,
                    None => conn_locations[&input][hook_n],
                };
                let points = wire_points_ends(&self.pan_zoom, src_pos, knot(from), dst_pos, knot(to));
                wires.push((input, output, points));
                let shape = bezier_shape(&self.pan_zoom, points, connection_color, self.style.wire_width);
                if connection_color.a() < 255 {
                    faint.push(shape);
                } else {
                    solid.push(shape);
                }
            }
        }
        faint.extend(solid);
        ui.painter().set(wire_slot, Shape::Vec(faint));

        /* Handle responses from drawing nodes */

        // Some responses generate additional responses when processed. These
        // are stored here to report them back to the user.
        let mut extra_responses: Vec<NodeResponse<UserResponse, NodeData>> = Vec::new();

        // FactoryBuilder: let go over a node: connect to the port it snapped to
        // (a bubble dropped on directly has already done so).
        if let (Some((_, port)), Some((_, origin))) = (snap, self.connection_in_progress) {
            let released = ui.ctx().input(|i| i.pointer.any_released());
            let ended = delayed_responses.iter().any(|r| matches!(r, NodeResponse::ConnectEventEnded { .. }));
            if released && !ended {
                let pair = match (origin, port) {
                    (AnyParameterId::Output(output), AnyParameterId::Input(input)) => Some((output, input)),
                    (AnyParameterId::Input(input), AnyParameterId::Output(output)) => Some((output, input)),
                    _ => None,
                };
                if let Some((output, input)) = pair {
                    let input_hook = self.graph.connections(input).len();
                    let output_hook = self.graph.output_connections(output).len();
                    delayed_responses.push(NodeResponse::ConnectEventEnded { output, input, input_hook, output_hook });
                }
            }
        }

        // FactoryBuilder: the outputs pulled along make the same connection.
        if !self.connection_extras.is_empty() {
            let ended: Vec<(OutputId, InputId)> = delayed_responses
                .iter()
                .filter_map(|r| match r {
                    NodeResponse::ConnectEventEnded { output, input, .. } => Some((*output, *input)),
                    _ => None,
                })
                .collect();
            let origin = match self.connection_in_progress {
                Some((_, AnyParameterId::Output(o))) => Some(o),
                _ => None,
            };
            for (output, input) in ended {
                if Some(output) != origin {
                    continue;
                }
                for (k, x) in self.connection_extras.iter().enumerate() {
                    let input_hook = self.graph.connections(input).len() + 1 + k;
                    let output_hook = self.graph.output_connections(*x).len();
                    delayed_responses.push(NodeResponse::ConnectEventEnded { output: *x, input, input_hook, output_hook });
                }
            }
        }

        // FactoryBuilder: no wire to (or from) a port that's full, checked
        // one wire at a time as they're made (one drop can make several).
        let mut made: Vec<NodeResponse<UserResponse, NodeData>> = Vec::new();
        for r in std::mem::take(&mut delayed_responses) {
            if let NodeResponse::ConnectEventEnded { output, input, input_hook, output_hook } = r {
                let (a, b) = (self.graph[output].node, self.graph[input].node);
                let again = self.graph.connections(input).contains(&output);
                let fits = again
                    || (self.graph[a].user_data.accepts_wire(a, AnyParameterId::Output(output), &self.graph, user_state)
                        && self.graph[b].user_data.accepts_wire(b, AnyParameterId::Input(input), &self.graph, user_state));
                if fits {
                    self.graph.add_connection(output, input, input_hook);
                    self.graph.move_output_wire(output, input, output_hook);
                    made.push(r);
                }
                continue;
            }
            made.push(r);
        }
        delayed_responses = made;

        for response in delayed_responses.iter() {
            match response {
                NodeResponse::ConnectEventStarted(node_id, port) => {
                    self.connection_in_progress = Some((*node_id, *port));
                    // FactoryBuilder: dragging an output of a selected node
                    // pulls the same item's output of every other selected
                    // node along.
                    self.connection_extras.clear();
                    if let (AnyParameterId::Output(o), true) = (port, self.selected_nodes.contains(node_id)) {
                        let typ = &self.graph[*o].typ;
                        for n in self.selected_nodes.iter().filter(|n| *n != node_id && self.graph.nodes.contains_key(**n)) {
                            if let Some(x) = self.graph[*n].outputs.iter().map(|(_, x)| *x).find(|x| self.graph[*x].typ == *typ) {
                                self.connection_extras.push(x);
                            }
                        }
                    }
                }
                // Made above, where the limits are checked.
                NodeResponse::ConnectEventEnded { .. } => {}
                NodeResponse::CreatedNode(_) => {
                    //Convenience NodeResponse for users
                }
                NodeResponse::SelectNode(node_id) => {
                    self.selected_nodes = Vec::from([*node_id]);
                }
                NodeResponse::DeleteNodeUi(node_id) => {
                    let (node, disc_events) = self.graph.remove_node(*node_id);

                    // Pass the disconnection responses first so user code can perform cleanup
                    // before node removal response.
                    extra_responses.extend(
                        disc_events
                            .into_iter()
                            .map(|(input, output)| NodeResponse::DisconnectEvent { input, output }),
                    );
                    // Pass the full node as a response so library users can
                    // listen for it and get their user data.
                    extra_responses.push(NodeResponse::DeleteNodeFull {
                        node_id: *node_id,
                        node,
                    });
                    self.node_positions.remove(*node_id);
                    // Make sure to not leave references to old nodes hanging
                    self.selected_nodes.retain(|id| *id != *node_id);
                    self.node_order.retain(|id| *id != *node_id);
                }
                NodeResponse::DisconnectEvent { input, output } => {
                    let other_node = self.graph.get_output(*output).node;
                    self.graph.remove_connection(*input, *output);
                    self.connection_in_progress =
                        Some((other_node, AnyParameterId::Output(*output)));
                }
                NodeResponse::DisconnectOutputEvent { input, output } => {
                    let other_node = self.graph.get_input(*input).node;
                    self.graph.remove_connection(*input, *output);
                    self.connection_in_progress =
                        Some((other_node, AnyParameterId::Input(*input)));
                }
                NodeResponse::RaiseNode(node_id) => {
                    let old_pos = self
                        .node_order
                        .iter()
                        .position(|id| *id == *node_id)
                        .expect("Node to be raised should be in `node_order`");
                    self.node_order.remove(old_pos);
                    self.node_order.push(*node_id);
                }
                NodeResponse::MoveNode { node, drag_delta } => {
                    self.node_positions[*node] += *drag_delta;
                    // Handle multi-node selection movement
                    if self.selected_nodes.contains(node) && self.selected_nodes.len() > 1 {
                        for n in self.selected_nodes.iter().copied() {
                            if n != *node {
                                self.node_positions[n] += *drag_delta;
                            }
                        }
                    }
                }
                NodeResponse::User(_) => {
                    // These are handled by the user code.
                }
                NodeResponse::DeleteNodeFull { .. } => {
                    unreachable!("The UI should never produce a DeleteNodeFull event.")
                }
            }
        }

        // FactoryBuilder: remembered for the right-click hit test below.
        let node_rects_for_menu = node_rects.clone();
        // Handle box selection
        if let Some(box_start) = self.ongoing_box_selection {
            let selection_rect = Rect::from_two_pos(cursor_pos, box_start);
            let bg_color = Color32::from_rgba_unmultiplied(200, 200, 200, 20);
            let stroke_color = Color32::from_rgba_unmultiplied(200, 200, 200, 180);
            ui.painter().rect(
                selection_rect,
                2.0,
                bg_color,
                Stroke::new(3.0_f32, stroke_color),
            );

            self.selected_nodes = node_rects
                .into_iter()
                .filter_map(|(node_id, rect)| {
                    if selection_rect.intersects(rect) {
                        Some(node_id)
                    } else {
                        None
                    }
                })
                .collect();
        }

        // Push any responses that were generated during response handling.
        // These are only informative for the end-user and need no special
        // treatment here.
        delayed_responses.extend(extra_responses);

        /* Mouse input handling */

        // This locks the context, so don't hold on to it for too long.
        let mouse = &ui.ctx().input(|i| i.pointer.clone());

        // FactoryBuilder: let go on empty canvas (not on a port, not on a node).
        let mut wire_dropped = None;
        let mut wire_dropped_extras = Vec::new();
        if mouse.any_released() && self.connection_in_progress.is_some() {
            let ended = delayed_responses.iter().any(|r| matches!(r, NodeResponse::ConnectEventEnded { .. }));
            let on_node = node_rects_for_menu.values().any(|r| r.contains(cursor_pos));
            if !ended && !on_node && cursor_in_editor && !cursor_in_finder {
                wire_dropped = self.connection_in_progress.map(|(n, p)| (n, p, cursor_pos));
                wire_dropped_extras = std::mem::take(&mut self.connection_extras);
            }
            self.connection_in_progress = None;
            self.connection_extras.clear();
        }

        // FactoryBuilder: right-click on a node asks for its context menu;
        // right-click on empty canvas opens the finder.
        let mut node_context_menu = None;
        if mouse.secondary_released() && cursor_in_editor && !cursor_in_finder {
            let over_node = self
                .node_order
                .iter()
                .rev()
                .find(|id| node_rects_for_menu.get(id).is_some_and(|r| r.contains(cursor_pos)))
                .copied();
            match over_node {
                Some(node) => {
                    self.node_finder = None;
                    if !self.selected_nodes.contains(&node) {
                        self.selected_nodes = vec![node];
                    }
                    node_context_menu = Some((node, cursor_pos));
                }
                None => self.node_finder = Some(NodeFinder::new_at(cursor_pos)),
            }
        }
        if ui.ctx().input(|i| i.key_pressed(Key::Escape)) {
            self.node_finder = None;
        }

        if r.dragged()
            && ui
                .ctx()
                .input(|i| i.pointer.middle_down() || i.modifiers.command_only())
        {
            self.pan_zoom.pan += ui.ctx().input(|i| i.pointer.delta());
        }

        // Deselect and deactivate finder if the editor background is clicked,
        // *or* if the the mouse clicks off the ui
        if click_on_background || (mouse.any_click() && !cursor_in_editor) {
            self.selected_nodes = Vec::new();
            self.node_finder = None;
        }

        if drag_started_on_background
            && mouse.primary_down()
            && !ui.ctx().input(|i| i.modifiers.command_only())
        {
            self.ongoing_box_selection = Some(cursor_pos);
        }
        if mouse.primary_released() || drag_released_on_background {
            self.ongoing_box_selection = None;
        }

        GraphResponse {
            node_responses: delayed_responses,
            cursor_in_editor,
            cursor_in_finder,
            node_context_menu,
            node_rects: node_rects_for_menu,
            port_locations: port_locations.clone(),
            wires,
            wire_dropped,
            wire_dropped_extras,
        }
    }
}

fn draw_connection(
    pan_zoom: &PanZoom,
    painter: &Painter,
    src_pos: Pos2,
    dst_pos: Pos2,
    color: Color32,
    width: f32,
) {
    painter.add(connection_shape(pan_zoom, src_pos, dst_pos, color, width));
}

fn connection_shape(pan_zoom: &PanZoom, src_pos: Pos2, dst_pos: Pos2, color: Color32, width: f32) -> Shape {
    bezier_shape(pan_zoom, wire_points(pan_zoom, src_pos, dst_pos), color, width)
}

fn bezier_shape(pan_zoom: &PanZoom, points: [Pos2; 4], color: Color32, width: f32) -> Shape {
    let connection_stroke = egui::Stroke {
        width: width * pan_zoom.zoom,
        color,
    };
    CubicBezierShape::from_points_stroke(points, false, Color32::TRANSPARENT, connection_stroke).into()
}

/// FactoryBuilder: the bezier of a wire (shared by drawing and `GraphResponse::wires`).
fn wire_points(pan_zoom: &PanZoom, src_pos: Pos2, dst_pos: Pos2) -> [Pos2; 4] {
    wire_points_ends(pan_zoom, src_pos, false, dst_pos, false)
}

/// FactoryBuilder: a wire's bezier. Port ends leave sideways (outputs to the
/// right, inputs from the left); a knot end (`*_free`) leaves along the
/// axis, across or down, that points most at the other end, so wires between
/// knots run straight and wires from a knot turn once.
fn wire_points_ends(pan_zoom: &PanZoom, src: Pos2, src_free: bool, dst: Pos2, dst_free: bool) -> [Pos2; 4] {
    let d = dst - src;
    let side = (d.x * pan_zoom.zoom / 2.0).max(30.0 * pan_zoom.zoom);
    let axis = |v: Vec2| if v.x.abs() >= v.y.abs() { Vec2::X * v.x.signum() } else { Vec2::Y * v.y.signum() };
    let free = |dir: Vec2| dir * (d.dot(dir).abs() * pan_zoom.zoom / 2.0).max(30.0 * pan_zoom.zoom);
    let a = if src_free { src + free(axis(d)) } else { src + Vec2::X * side };
    let b = if dst_free { dst + free(axis(-d)) } else { dst - Vec2::X * side };
    [src, a, b, dst]
}

#[derive(Clone, Copy, Debug)]
struct OuterRectMemory(Rect);

impl<'a, NodeData, DataType, ValueType, UserResponse, UserState>
    GraphNodeWidget<'a, NodeData, DataType, ValueType>
where
    NodeData: NodeDataTrait<
        Response = UserResponse,
        UserState = UserState,
        DataType = DataType,
        ValueType = ValueType,
    >,
    UserResponse: UserResponseTrait,
    ValueType:
        WidgetValueTrait<Response = UserResponse, UserState = UserState, NodeData = NodeData>,
    DataType: DataTypeTrait<UserState>,
{

    pub fn show(
        self,
        pan_zoom: &PanZoom,
        ui: &mut Ui,
        user_state: &mut UserState,
    ) -> Vec<NodeResponse<UserResponse, NodeData>> {
        let mut child_ui = ui.child_ui_with_id_source(
            Rect::from_min_size(*self.position + self.pan, vec2(self.style.node_width, 4000.0)),
            Layout::default(),
            self.node_id,
            None,
        );

        Self::show_graph_node(self, pan_zoom, &mut child_ui, user_state)
    }

    /// Draws this node. Also fills in the list of port locations with all of its ports.
    /// Returns responses indicating multiple events.
    fn show_graph_node(
        self,
        pan_zoom: &PanZoom,
        ui: &mut Ui,
        user_state: &mut UserState,
    ) -> Vec<NodeResponse<UserResponse, NodeData>> {
        let margin = egui::vec2(15.0, 5.0) * pan_zoom.zoom;
        let mut responses = Vec::<NodeResponse<UserResponse, NodeData>>::new();

        // FactoryBuilder: a knot (see NodeDataTrait::knot_size).
        if let Some(size) = self.graph[self.node_id].user_data.knot_size() {
            let rect = Rect::from_min_size(ui.max_rect().min, size * pan_zoom.zoom);
            let window_response = ui.interact(rect, Id::new((self.node_id, "window")), Sense::click_and_drag());
            if self.selected {
                ui.painter().rect_filled(rect.expand(3.0 * pan_zoom.zoom), self.style.rounding * pan_zoom.zoom, self.style.selection);
            }
            self.graph[self.node_id].user_data.knot_ui(ui, rect, self.node_id, user_state);
            self.node_rects.insert(self.node_id, rect);
            if let Some(i) = self.graph[self.node_id].inputs.first().map(|(_, i)| *i) {
                // Handles to drag new wires from; wires meet the middle.
                draw_port(pan_zoom, ui, self.graph, self.node_id, user_state, rect.left_center(), &mut responses, AnyParameterId::Input(i), self.port_locations, self.conn_locations, self.ongoing_drag, true, 0, usize::MAX, self.style.port_radius, self.style.node_fill);
            }
            if let Some(o) = self.graph[self.node_id].outputs.first().map(|(_, o)| *o) {
                draw_port(pan_zoom, ui, self.graph, self.node_id, user_state, rect.right_center(), &mut responses, AnyParameterId::Output(o), self.port_locations, self.conn_locations, self.ongoing_drag, true, 0, usize::MAX, self.style.port_radius, self.style.node_fill);
            }
            let drag_delta = window_response.drag_delta();
            if drag_delta.length_sq() > 0.0 {
                responses.push(NodeResponse::MoveNode { node: self.node_id, drag_delta });
                responses.push(NodeResponse::RaiseNode(self.node_id));
            }
            if responses.is_empty() && window_response.clicked_by(PointerButton::Primary) {
                responses.push(NodeResponse::SelectNode(self.node_id));
                responses.push(NodeResponse::RaiseNode(self.node_id));
            }
            return responses;
        }

        let background_color = self.style.node_fill;
        let text_color = self.style.text;

        ui.visuals_mut().widgets.noninteractive.fg_stroke =
            Stroke::new(2.0 * pan_zoom.zoom, text_color);

        // Preallocate shapes to paint below contents
        let outline_shape = ui.painter().add(Shape::Noop);
        let background_shape = ui.painter().add(Shape::Noop);

        let mut outer_rect_bounds = ui.available_rect_before_wrap();
        // Scale hack, otherwise some (larger) rects expand too much when zoomed out
        outer_rect_bounds.max.x =
            outer_rect_bounds.min.x + outer_rect_bounds.width() * pan_zoom.zoom;

        let mut inner_rect = outer_rect_bounds.shrink2(margin);

        // Make sure we don't shrink to the negative:
        inner_rect.max.x = inner_rect.max.x.max(inner_rect.min.x);
        inner_rect.max.y = inner_rect.max.y.max(inner_rect.min.y);

        let mut child_ui = ui.child_ui(inner_rect, *ui.layout(), None);

        // Get interaction rect from memory, it may expand after the window response on resize.
        let interaction_rect = ui
            .ctx()
            .memory_mut(|mem| {
                mem.data
                    .get_temp::<OuterRectMemory>(child_ui.id())
                    .map(|stored| stored.0)
            })
            .unwrap_or(outer_rect_bounds);
        // After 0.20, layers added over others can block hover interaction. Call this first
        // before creating the node content.
        let window_response = ui.interact(
            interaction_rect,
            Id::new((self.node_id, "window")),
            Sense::click_and_drag(),
        );

        let mut title_height = 0.0;

        let mut input_port_heights = vec![];
        let mut output_port_heights = vec![];

        child_ui.vertical(|ui| {
            // FactoryBuilder: the header's right-hand part (top_bar_ui) is laid
            // out first; the title takes the rest and truncates with "…".
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    responses.extend(self.graph[self.node_id].user_data.top_bar_ui(
                        ui,
                        self.node_id,
                        self.graph,
                        user_state,
                    ));
                    let rest = ui.available_width();
                    ui.allocate_ui_with_layout(vec2(rest, 0.0), Layout::left_to_right(Align::Center), |ui| {
                        let title = &self.graph[self.node_id].label;
                        ui.add(
                            Label::new(RichText::new(title).text_style(TextStyle::Button).color(self.style.title_text))
                                .truncate(),
                        )
                        .on_hover_text(title.as_str());
                    });
                });
            });
            ui.add_space(margin.y);
            title_height = ui.min_size().y;

            // First pass: Draw the inner fields. Compute port heights
            let inputs = self.graph[self.node_id].inputs.clone();
            for (param_name, param_id) in inputs {
                if self.graph[param_id].shown_inline {
                    let height_before = ui.min_rect().bottom();

                    // FactoryBuilder: wide (multi-connection) inputs get the value
                    // widget too, so they can show icons and rates like outputs.
                    if true {
                        // NOTE: We want to pass the `user_data` to
                        // `value_widget`, but we can't since that would require
                        // borrowing the graph twice. Here, we make the
                        // assumption that the value is cheaply replaced, and
                        // use `std::mem::take` to temporarily replace it with a
                        // dummy value. This requires `ValueType` to implement
                        // Default, but results in a totally safe alternative.
                        let mut value = std::mem::take(&mut self.graph[param_id].value);

                        if !self.graph.connections(param_id).is_empty() {
                            let node_responses = value.value_widget_connected(
                                &param_name,
                                self.node_id,
                                ui,
                                user_state,
                                &self.graph[self.node_id].user_data,
                            );

                            responses.extend(node_responses.into_iter().map(NodeResponse::User));
                        } else {
                            let node_responses = value.value_widget(
                                &param_name,
                                self.node_id,
                                ui,
                                user_state,
                                &self.graph[self.node_id].user_data,
                            );

                            responses.extend(node_responses.into_iter().map(NodeResponse::User));
                        }

                        self.graph[param_id].value = value;
                    } else {
                        ui.label(param_name);
                    }

                    let height_intermediate = ui.min_rect().bottom();

                    let n = self.graph.connections(param_id).len();
                    let full = !self.graph[self.node_id].user_data.accepts_wire(self.node_id, AnyParameterId::Input(param_id), self.graph, user_state);
                    let max_connections = if full {
                        n.max(1)
                    } else {
                        self.graph[param_id].max_connections.map(NonZeroU32::get).unwrap_or(std::u32::MAX) as usize
                    };
                    let port_height = port_height(
                        max_connections != 1 && !self.graph[self.node_id].user_data.shared_hooks(),
                        self.graph.connections(param_id).len(),
                        max_connections,
                        self.style.port_radius,
                    );
                    // FactoryBuilder: scale with zoom like the content around it.
                    let margin = 5.0 * pan_zoom.zoom;
                    let missing_space =
                        port_height * pan_zoom.zoom - (height_intermediate - height_before) + margin;
                    if missing_space > 0.0 {
                        ui.add_space(missing_space);
                    }

                    self.graph[self.node_id].user_data.separator(
                        ui,
                        self.node_id,
                        AnyParameterId::Input(param_id),
                        self.graph,
                        user_state,
                    );

                    let height_after = ui.min_rect().bottom();

                    input_port_heights.push((height_before + height_after) / 2.0);
                }
            }

            let outputs = self.graph[self.node_id].outputs.clone();
            for (param_name, param_id) in outputs {
                let height_before = ui.min_rect().bottom();
                responses.extend(
                    self.graph[self.node_id]
                        .user_data
                        .output_ui(ui, self.node_id, self.graph, user_state, &param_name)
                        .into_iter(),
                );

                self.graph[self.node_id].user_data.separator(
                    ui,
                    self.node_id,
                    AnyParameterId::Output(param_id),
                    self.graph,
                    user_state,
                );

                // FactoryBuilder: outputs show a bubble per wire too; make room.
                let out_conns = self.graph.iter_connections().filter(|(_, o)| *o == param_id).count();
                let wide = !self.graph[self.node_id].user_data.shared_hooks();
                let full = !self.graph[self.node_id].user_data.accepts_wire(self.node_id, AnyParameterId::Output(param_id), self.graph, user_state);
                let needed = (port_height(wide, out_conns, if full { out_conns.max(1) } else { usize::MAX }, self.style.port_radius) + 5.0) * pan_zoom.zoom;
                let missing = needed - (ui.min_rect().bottom() - height_before);
                if missing > 0.0 {
                    ui.add_space(missing);
                }
                let height_after = ui.min_rect().bottom();
                output_port_heights.push((height_before + height_after) / 2.0);
            }

            responses.extend(self.graph[self.node_id].user_data.bottom_ui(
                ui,
                self.node_id,
                self.graph,
                user_state,
            ));
        });

        // Second pass, iterate again to draw the ports. This happens outside
        // the child_ui because we want ports to overflow the node background.

        let outer_rect = child_ui.min_rect().expand2(margin);
        let port_left = outer_rect.left();
        let port_right = outer_rect.right();

        // Save expanded rect to memory.
        ui.ctx().memory_mut(|mem| {
            mem.data
                .insert_temp(child_ui.id(), OuterRectMemory(outer_rect))
        });

        // FactoryBuilder: one bubble per connection plus a free slot, spaced so
        // bubbles never touch (was 10 px, which merged them into one bar).
        fn hook_spacing(port_radius: f32) -> f32 {
            port_radius * 2.0 + 4.0
        }

        fn port_height(wide_port: bool, connections: usize, max_connections: usize, port_radius: f32) -> f32 {
            let port_full = connections == max_connections;
            let spacing = hook_spacing(port_radius);
            if wide_port {
                let hooks = connections + if port_full { 0 } else { 1 };
                (spacing * hooks as f32).max(spacing)
            } else {
                spacing
            }
        }

        #[allow(clippy::too_many_arguments)]
        fn draw_port<NodeData, DataType, ValueType, UserResponse, UserState>(
            pan_zoom: &PanZoom,
            ui: &mut Ui,
            graph: &Graph<NodeData, DataType, ValueType>,
            node_id: NodeId,
            user_state: &mut UserState,
            port_pos: Pos2,
            responses: &mut Vec<NodeResponse<UserResponse, NodeData>>,
            param_id: AnyParameterId,
            port_locations: &mut PortLocations,
            conn_locations: &mut ConnLocations,
            ongoing_drag: Option<(NodeId, AnyParameterId)>,
            wide_port: bool,
            connections: usize,
            max_connections: usize,
            port_radius: f32,
            ring_fill: Color32,
        ) where
            DataType: DataTypeTrait<UserState>,
            UserResponse: UserResponseTrait,
            NodeData: NodeDataTrait,
        {
            let port_type = graph.any_param_type(param_id).unwrap();
            let shared = graph[node_id].user_data.shared_hooks();
            let wide_port = wide_port && !shared;

            let spacing = hook_spacing(port_radius);
            let port_rect = Rect::from_center_size(
                port_pos,
                egui::vec2(spacing, port_height(wide_port, connections, max_connections, port_radius))
                    * pan_zoom.zoom,
            );

            let port_full = connections == max_connections;

            let inner_ports = if wide_port {
                connections + if port_full { 0 } else { 1 }
            } else {
                1
            };

            port_locations.insert(
                param_id,
                (0..inner_ports)
                    .map(|k| {
                        port_rect.center_top()
                            + Vec2::new(0.0, spacing / 2.0 * pan_zoom.zoom)
                            + Vec2::new(0.0, spacing * pan_zoom.zoom) * k as f32
                    })
                    .collect(),
            );

            // FactoryBuilder: every bubble is its own target. Hover lights up
            // only that bubble; dragging a filled bubble pulls out exactly that
            // wire (on inputs and outputs alike); dragging the hollow ring
            // starts a new wire; while dragging a wire, only the bubble under
            // the pointer lights up and dropping there connects.
            let hooks: Vec<Pos2> = port_locations[&param_id].clone();
            let base_color = port_type.data_type_color(user_state);
            let pointer = ui.ctx().pointer_hover_pos();
            let hit_size = Vec2::splat(spacing * pan_zoom.zoom);
            // Wires of an output, in the same order they are drawn.
            let out_wires: Vec<InputId> = match param_id {
                AnyParameterId::Output(o) => graph.output_connections(o),
                AnyParameterId::Input(_) => Vec::new(),
            };
            let sense = if ongoing_drag.is_some() {
                Sense::hover()
            } else {
                Sense::click_and_drag()
            };
            let mut lit = None;
            for (k, center) in hooks.iter().enumerate() {
                let hit = Rect::from_center_size(*center, hit_size);
                let resp = ui.interact(hit, ui.id().with(("port_hook", param_id, k)), sense);
                let under_pointer = pointer.is_some_and(|p| hit.contains(p));
                if under_pointer {
                    lit = Some(k);
                }
                if resp.drag_started() {
                    let connected = k < connections && !shared;
                    match param_id {
                        AnyParameterId::Input(input) => match graph.connections(input).get(k).copied() {
                            Some(output) if connected => responses.push(NodeResponse::DisconnectEvent { input, output }),
                            _ => responses.push(NodeResponse::ConnectEventStarted(node_id, param_id)),
                        },
                        AnyParameterId::Output(output) => match out_wires.get(k).copied() {
                            Some(input) if connected => responses.push(NodeResponse::DisconnectOutputEvent { input, output }),
                            _ => responses.push(NodeResponse::ConnectEventStarted(node_id, param_id)),
                        },
                    }
                }
            }
            // While dragging a wire, only compatible ports light up.
            if let Some((origin_node, origin_param)) = ongoing_drag {
                let compatible = origin_node != node_id
                    && graph.any_param_type(origin_param).unwrap() == port_type
                    && matches!(
                        (param_id, origin_param),
                        (AnyParameterId::Input(_), AnyParameterId::Output(_)) | (AnyParameterId::Output(_), AnyParameterId::Input(_))
                    );
                if !compatible {
                    lit = None;
                }
            }

            let r = port_radius * pan_zoom.zoom;
            for (k, center) in hooks.iter().enumerate() {
                let color = if lit == Some(k) { Color32::WHITE } else { base_color };
                if k < connections || (shared && connections > 0) {
                    ui.painter().circle_filled(*center, r, color);
                } else {
                    ui.painter().circle(*center, r - 0.75, ring_fill, Stroke::new(1.5_f32 * pan_zoom.zoom.max(0.5), color));
                }
            }

            if let AnyParameterId::Input(input) = param_id {
                for (k, dst_pos) in hooks.iter().enumerate() {
                    conn_locations.entry(input).or_default().insert(k, *dst_pos);
                }
                // A shared bubble: every wire ends at it.
                if shared {
                    for k in 1..connections {
                        conn_locations.entry(input).or_default().insert(k, hooks[0]);
                    }
                }
            }

            // Dropping a wire on a bubble puts it there: on a filled bubble it
            // goes in at that spot, on the ring it goes last. Other wires keep
            // their order.
            if let (Some((_, origin_param)), Some(k)) = (ongoing_drag, lit) {
                if ui.input(|i| i.pointer.any_released()) {
                    match (param_id, origin_param) {
                        (AnyParameterId::Input(input), AnyParameterId::Output(output)) => {
                            let output_hook = graph.output_connections(output).len();
                            let input_hook = if shared { connections } else { k };
                            responses.push(NodeResponse::ConnectEventEnded { output, input, input_hook, output_hook });
                        }
                        (AnyParameterId::Output(output), AnyParameterId::Input(input)) => {
                            let input_hook = graph.connections(input).len();
                            let output_hook = if shared { connections } else { k };
                            responses.push(NodeResponse::ConnectEventEnded { output, input, input_hook, output_hook });
                        }
                        _ => {}
                    }
                }
            }
            let _ = (port_full, max_connections, wide_port);
        }

        // Input ports
        for ((_, param), port_height) in self.graph[self.node_id]
            .inputs
            .iter()
            .zip(input_port_heights.into_iter())
        {
            let should_draw = match self.graph[*param].kind() {
                InputParamKind::ConnectionOnly => true,
                InputParamKind::ConstantOnly => false,
                InputParamKind::ConnectionOrConstant => true,
            };

            if should_draw {
                let pos_left = pos2(port_left, port_height);
                let n = self.graph.connections(*param).len();
                let full = !self.graph[self.node_id].user_data.accepts_wire(self.node_id, AnyParameterId::Input(*param), self.graph, user_state);
                let max_connections = if full {
                    n.max(1)
                } else {
                    self.graph[*param].max_connections.map(NonZeroU32::get).unwrap_or(std::u32::MAX) as usize
                };
                draw_port(
                    pan_zoom,
                    ui,
                    self.graph,
                    self.node_id,
                    user_state,
                    pos_left,
                    &mut responses,
                    AnyParameterId::Input(*param),
                    self.port_locations,
                    self.conn_locations,
                    self.ongoing_drag,
                    max_connections > 1,
                    self.graph.connections(*param).len(),
                    max_connections,
                    self.style.port_radius,
                    self.style.node_fill,
                );
            }
        }

        // Output ports
        for ((_, param), port_height) in self.graph[self.node_id]
            .outputs
            .iter()
            .zip(output_port_heights.into_iter())
        {
            let pos_right = pos2(port_right, port_height);
            let n = self.graph.iter_connections().filter(|(_, o)| o == param).count();
            let full = !self.graph[self.node_id].user_data.accepts_wire(self.node_id, AnyParameterId::Output(*param), self.graph, user_state);
            draw_port(
                pan_zoom,
                ui,
                self.graph,
                self.node_id,
                user_state,
                pos_right,
                &mut responses,
                AnyParameterId::Output(*param),
                self.port_locations,
                self.conn_locations,
                self.ongoing_drag,
                true,
                n,
                if full { n.max(1) } else { usize::MAX },
                self.style.port_radius,
                self.style.node_fill,
            );
        }

        // Draw the background shape.
        // NOTE: This code is a bit more involved than it needs to be because egui
        // does not support drawing rectangles with asymmetrical round corners.

        let (shape, outline) = {
            let rounding_radius = self.style.rounding * pan_zoom.zoom;
            let rounding = Rounding::same(rounding_radius);

            let titlebar_height = title_height + margin.y;
            let titlebar_rect =
                Rect::from_min_size(outer_rect.min, vec2(outer_rect.width(), titlebar_height));
            let titlebar = Shape::Rect(RectShape {
                rect: titlebar_rect,
                rounding,
                fill: self.graph[self.node_id]
                    .user_data
                    .titlebar_color(ui, self.node_id, self.graph, user_state)
                    .unwrap_or(self.style.title_fill),
                stroke: Stroke::NONE,
                blur_width: 0.0,
                fill_texture_id: Default::default(),
                uv: Rect::ZERO,
            });

            let body_rect = Rect::from_min_size(
                outer_rect.min + vec2(0.0, titlebar_height - rounding_radius),
                vec2(outer_rect.width(), outer_rect.height() - titlebar_height),
            );
            let body = Shape::Rect(RectShape {
                rect: body_rect,
                rounding: Rounding::ZERO,
                fill: background_color,
                stroke: Stroke::NONE,
                blur_width: 0.0,
                fill_texture_id: Default::default(),
                uv: Rect::ZERO,
            });

            let bottom_body_rect = Rect::from_min_size(
                body_rect.min + vec2(0.0, body_rect.height() - titlebar_height * 0.5),
                vec2(outer_rect.width(), titlebar_height),
            );
            let bottom_body = Shape::Rect(RectShape {
                rect: bottom_body_rect,
                rounding,
                fill: background_color,
                stroke: Stroke::NONE,
                blur_width: 0.0,
                fill_texture_id: Default::default(),
                uv: Rect::ZERO,
            });

            // FactoryBuilder: optional colour label across the top of the title bar.
            let strip = match self.graph[self.node_id].user_data.titlebar_strip(self.node_id, self.graph, user_state) {
                Some(c) => Shape::rect_filled(
                    Rect::from_min_size(titlebar_rect.min, vec2(titlebar_rect.width(), 4.0 * pan_zoom.zoom)),
                    Rounding::ZERO,
                    c,
                ),
                None => Shape::Noop,
            };
            let node_rect = titlebar_rect.union(body_rect).union(bottom_body_rect);
            let warning = self.graph[self.node_id].user_data.warning(self.node_id, self.graph, user_state);
            let outline = if self.selected {
                Shape::Rect(RectShape {
                    rect: node_rect.expand(2.0 * pan_zoom.zoom),
                    rounding,
                    fill: self.style.selection,
                    stroke: Stroke::NONE,
                    blur_width: 0.0,
                    fill_texture_id: Default::default(),
                    uv: Rect::ZERO,
                })
            } else if let Some((c, _)) = &warning {
                let c = *c;
                Shape::Rect(RectShape {
                    rect: node_rect.expand(2.0 * pan_zoom.zoom),
                    rounding,
                    fill: c,
                    stroke: Stroke::NONE,
                    blur_width: 0.0,
                    fill_texture_id: Default::default(),
                    uv: Rect::ZERO,
                })
            } else {
                Shape::Noop
            };

            // Take note of the node rect, so the editor can use it later to compute intersections.
            self.node_rects.insert(self.node_id, node_rect);
            // FactoryBuilder: what's wrong, over the node itself (its ports
            // and fields have their own tips).
            if let Some((_, text)) = &warning {
                if window_response.hovered() {
                    egui::show_tooltip_at_pointer(ui.ctx(), ui.layer_id(), Id::new((self.node_id, "warning")), |ui| {
                        ui.label(text.as_str());
                    });
                }
            }

            (Shape::Vec(vec![titlebar, body, bottom_body, strip]), outline)
        };

        ui.painter().set(background_shape, shape);
        ui.painter().set(outline_shape, outline);

        // --- Interaction ---

        // Titlebar buttons
        let can_delete = self.graph.nodes[self.node_id].user_data.can_delete(
            self.node_id,
            self.graph,
            user_state,
        );

        // FactoryBuilder: no close button; nodes are deleted from their
        // context menu or with the Delete key, like other node editors.
        let _ = can_delete;

        // Movement
        let drag_delta = window_response.drag_delta();
        if drag_delta.length_sq() > 0.0 {
            responses.push(NodeResponse::MoveNode {
                node: self.node_id,
                drag_delta,
            });
            responses.push(NodeResponse::RaiseNode(self.node_id));
        }

        // Node selection
        //
        // HACK: Only set the select response when no other response is active.
        // This prevents some issues.
        if responses.is_empty() && window_response.clicked_by(PointerButton::Primary) {
            responses.push(NodeResponse::SelectNode(self.node_id));
            responses.push(NodeResponse::RaiseNode(self.node_id));
        }

        responses
    }
}
