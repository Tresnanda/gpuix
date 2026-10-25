// Included in automation.rs. Cached bounds keep their own scope/mask snapshot.
thread_local! {
    static SUBTREE_BOUNDS: RefCell<HashMap<u64,gpui::SubtreeGeometry>> = RefCell::default();
}
pub(crate) fn translate_subtree_bounds(
    updates: &[(u64, gpui::Point<Pixels>)],
    viewport: gpui::Size<Pixels>,
) -> HashMap<u64, gpui::Point<Pixels>> {
    let values = updates.iter().copied().collect();
    let mut deltas = HashMap::new();
    SUBTREE_BOUNDS.with(|geometries| {
        BOUNDS.with(|bounds| {
            let mut bounds = bounds.borrow_mut();
            for (id, geometry) in geometries.borrow_mut().iter_mut() {
                if !geometry.translation_changed(&values) {
                    continue;
                }
                let delta = geometry.translate(&values);
                let Some(b) = bounds.get_mut(id) else {
                    continue;
                };
                b.x += delta.x.as_f32() as f64;
                b.y += delta.y.as_f32() as f64;
                let (mask, shapes) = geometry.visual_clip(viewport);
                let visual = Bounds::new(point(px(b.x as f32), px(b.y as f32)), gpui::size(px(b.width as f32), px(b.height as f32)))
                    .intersect(&mask);
                b.clip = (!visual.is_empty()).then(|| {
                    [
                        visual.origin.x.as_f32() as f64,
                        visual.origin.y.as_f32() as f64,
                        visual.size.width.as_f32() as f64,
                        visual.size.height.as_f32() as f64,
                    ]
                });
                b.rounded_clips = (!shapes.is_empty()).then(|| Arc::new(shapes));
                if delta != Default::default() {
                    deltas.insert(*id, delta);
                }
            }
        })
    });
    crate::text::paint::translate_subtree_selection(&values, &deltas);
    deltas
}
