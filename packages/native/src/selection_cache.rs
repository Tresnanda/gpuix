// Included in text/paint.rs. Cached GPUI paint skips the registration canvases;
// replay their side effects in document order alongside cached scene replay.
#[derive(Clone, Default)]
pub(crate) struct SelectionSnapshot {
    runs: Vec<RegEntry>,
    regions: Vec<StartRegion>,
    painted: Vec<SharedString>,
    highlights: Vec<PaintedHighlight>,
}
#[derive(Clone, Copy, Default)]
pub(crate) struct SelectionCursor(usize, usize, usize, usize);

pub(crate) fn selection_cursor() -> SelectionCursor {
    SelectionCursor(
        REGISTRY.with(|r| r.borrow().len()),
        START_REGIONS.with(|r| r.borrow().len()),
        PAINTED.with(|r| r.borrow().len()),
        HIGHLIGHTS.with(|r| r.borrow().len()),
    )
}
pub(crate) fn selection_snapshot(from: SelectionCursor) -> SelectionSnapshot {
    SelectionSnapshot {
        runs: REGISTRY.with(|r| r.borrow()[from.0..].to_vec()),
        regions: START_REGIONS.with(|r| r.borrow()[from.1..].to_vec()),
        painted: PAINTED.with(|r| r.borrow()[from.2..].to_vec()),
        highlights: HIGHLIGHTS.with(|r| r.borrow()[from.3..].to_vec()),
    }
}
pub(crate) fn replay_selection(snapshot: &SelectionSnapshot) {
    REGISTRY.with(|r| r.borrow_mut().extend(snapshot.runs.iter().cloned()));
    START_REGIONS.with(|r| r.borrow_mut().extend(snapshot.regions.iter().cloned()));
    PAINTED.with(|r| r.borrow_mut().extend(snapshot.painted.iter().cloned()));
    HIGHLIGHTS.with(|r| r.borrow_mut().extend(snapshot.highlights.iter().cloned()));
}

pub(crate) fn select_text(
    selection: &SharedSelection,
    tree: &crate::retained_tree::RetainedTree,
    id: u64,
    sub: u32,
    start: u32,
    end: u32,
) -> napi::Result<()> {
    let key = selection_key(id, sub as usize);
    REGISTRY.with(|registry| {
        let registry = registry.borrow();
        let entry = registry
            .iter()
            .find(|entry| entry.key == key)
            .or_else(|| {
                registry
                    .iter()
                    .filter(|entry| {
                        let mut current = entry.key.split_once(':').and_then(|(id, _)| id.parse::<u64>().ok());
                        while let Some(node) = current {
                            if node == id {
                                return true;
                            }
                            current = tree.elements.get(&node).and_then(|node| node.parent);
                        }
                        false
                    })
                    .nth(sub as usize)
            })
            .ok_or_else(|| napi::Error::from_reason("Text run is not painted"))?;
        let boundary = |offset: u32| {
            let mut units = 0;
            for (byte, ch) in entry.text.char_indices() {
                if units == offset {
                    return Some(byte);
                }
                units += ch.len_utf16() as u32;
            }
            (units == offset).then_some(entry.text.len())
        };
        let (Some(start), Some(end)) = (boundary(start), boundary(end)) else {
            return Err(napi::Error::from_reason("Selection must use UTF-16 character boundaries"));
        };
        if start > end {
            return Err(napi::Error::from_reason("Selection start exceeds end"));
        }
        let mut selection = selection.lock();
        selection.begin_with_span(&entry.key, &entry.text, start..end);
        selection.end_active_drag();
        Ok(())
    })
}
