use std::{collections::BTreeMap, marker::PhantomData};

use crate::{CategoryTrait, NodeTemplateIter, NodeTemplateTrait};

use egui::*;

#[derive(Clone)]
#[cfg_attr(feature = "persistence", derive(serde::Serialize, serde::Deserialize))]
pub struct NodeFinder<NodeTemplate> {
    pub query: String,
    /// Reset every frame. When set, the node finder will be moved at that position
    pub position: Option<Pos2>,
    pub just_spawned: bool,
    _phantom: PhantomData<NodeTemplate>,
}

impl<NodeTemplate, NodeData, UserState, CategoryType> NodeFinder<NodeTemplate>
where
    NodeTemplate:
        NodeTemplateTrait<NodeData = NodeData, UserState = UserState, CategoryType = CategoryType>,
    CategoryType: CategoryTrait,
{
    pub fn new_at(pos: Pos2) -> Self {
        NodeFinder {
            query: "".into(),
            position: Some(pos),
            just_spawned: true,
            _phantom: Default::default(),
        }
    }

    /// Shows the node selector panel with a search bar. Returns whether a node
    /// archetype was selected and, in that case, the finder should be hidden on
    /// the next frame.
    ///
    /// FactoryBuilder: colours come from `GraphStyle`; every entry (with or
    /// without a category) is filtered by the query; Enter picks the first
    /// match; categories open while searching.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        all_kinds: impl NodeTemplateIter<Item = NodeTemplate>,
        user_state: &mut UserState,
        style: &crate::GraphStyle,
    ) -> Option<NodeTemplate> {
        ui.visuals_mut().widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, style.finder_text);
        let frame = Frame::none()
            .fill(style.finder_fill)
            .stroke(ui.visuals().window_stroke)
            .shadow(ui.visuals().popup_shadow)
            .inner_margin(vec2(6.0, 6.0));

        let mut submitted = None;
        frame.show(ui, |ui| {
            ui.set_width(300.0);
            let resp = ui.add(
                TextEdit::singleline(&mut self.query)
                    .hint_text("Search")
                    .desired_width(f32::INFINITY),
            );
            if self.just_spawned {
                resp.request_focus();
                self.just_spawned = false;
            }
            let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            let query = self.query.to_lowercase();
            let searching = !query.is_empty();

            let mut categories: BTreeMap<String, Vec<(NodeTemplate, String)>> = Default::default();
            let mut orphans = Vec::new();
            for kind in all_kinds.all_kinds() {
                let label = kind.node_finder_label(user_state).to_string();
                if searching && !label.to_lowercase().contains(&query) {
                    continue;
                }
                let cats = kind.node_finder_categories(user_state);
                if cats.is_empty() {
                    orphans.push((kind, label));
                } else {
                    for c in cats {
                        categories.entry(c.name()).or_default().push((kind.clone(), label.clone()));
                    }
                }
            }
            if enter {
                submitted = categories.values().flatten().chain(orphans.iter()).next().map(|k| k.0.clone());
            }

            let max_height = ui.input(|i| i.screen_rect.height() * 0.5);
            ScrollArea::vertical().max_height(max_height).show(ui, |ui| {
                ui.set_width(ui.available_width());
                if categories.is_empty() && orphans.is_empty() {
                    ui.weak("No matches");
                }
                for (category, kinds) in categories {
                    CollapsingHeader::new(&category)
                        .id_salt(("finder", &category))
                        .open(searching.then_some(true))
                        .show(ui, |ui| {
                            for (kind, label) in kinds {
                                if ui.selectable_label(false, label).clicked() {
                                    submitted = Some(kind);
                                }
                            }
                        });
                }
                for (kind, label) in orphans {
                    if ui.selectable_label(false, label).clicked() {
                        submitted = Some(kind);
                    }
                }
            });
        });

        submitted
    }
}
