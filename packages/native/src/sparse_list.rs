// Semantic metadata is rebuilt on data changes, never on a scroll frame.
#[derive(Default)]
struct SparseList {
    active: bool,
    version: Option<u64>,
    layout: Option<u64>,
    keys: Vec<String>,
    estimates: Vec<f32>,
    heights: HashMap<String, f32>,
    children: HashMap<usize, u64>,
    offsets: HashMap<usize, usize>,
    revisions: HashMap<usize, u64>,
}
impl SparseList {
    fn height(&self, ix: usize, fallback: f32) -> f32 {
        self.keys.get(ix).and_then(|key| self.heights.get(key)).copied().or_else(|| self.estimates.get(ix).copied()).unwrap_or(fallback)
    }
}
impl VirtualListEntry {
    fn child_offset(&self, ix: usize) -> Option<usize> {
        if self.sparse.active {
            self.sparse.offsets.get(&ix).copied()
        } else {
            ix.checked_sub(self.window_start)
        }
    }
    fn sync_sparse(
        &mut self,
        element: &crate::retained_tree::RetainedElement,
        config: VirtualListConfig,
        child_ids: Vec<u64>,
        child_revisions: Vec<u64>,
        focusable: &HashSet<u64>,
        cx: &mut gpui::Context<GpuixView>,
    ) {
        let version = element.custom_props().get("sparseVersion").and_then(serde_json::Value::as_u64);
        let layout = element.custom_props().get("sparseLayout").and_then(serde_json::Value::as_u64);
        let props = element.custom_props();
        if self.sparse.active
            && self.sparse.version == version
            && self.sparse.layout == layout
            && self.config == config
            && self.child_ids == child_ids
            && self.child_revisions == child_revisions
            && self.child_ids.iter().zip(&self.row_focus_handles).all(|(id, handle)| handle.is_some() == focusable.contains(id))
        {
            return;
        }
        for (&ix, &id) in &self.sparse.children {
            if self
                .child_ids
                .iter()
                .position(|candidate| *candidate == id)
                .and_then(|at| self.row_focus_handles.get(at))
                .is_some_and(|handle| handle.is_some())
                && ix < self.state.item_count()
            {
                self.state.kybern_item_focus(ix, None);
            }
        }
        // Capture the actual GPUI measurement before a disposed host is replaced.
        for &ix in self.sparse.children.keys() {
            if let (Some(key), Some(height)) = (self.sparse.keys.get(ix), self.state.kybern_item_height(ix)) {
                self.sparse.heights.insert(key.clone(), f32::from(height));
            }
        }
        if self.sparse.layout != layout {
            self.sparse.heights.clear();
            self.state.remeasure_items(0..self.state.item_count());
            self.sparse.layout = layout;
        }
        if !self.sparse.active || self.sparse.version != version {
            let keys: Vec<String> = props
                .get("sparseKeys")
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect();
            let estimates = props.get("sparseEstimates").and_then(|v| v.as_array());
            self.sparse.estimates = (0..keys.len())
                .map(|ix| {
                    estimates
                        .and_then(|v| v.get(ix))
                        .and_then(|v| v.as_f64())
                        .map(|h| h as f32)
                        .unwrap_or(config.estimated_item_height.unwrap_or(1.))
                })
                .collect();
            let anchor = self.state.logical_scroll_top();
            let anchor_key = self.sparse.keys.get(anchor.item_ix).cloned();
            let following = self.state.is_following_tail();
            let prefix = self.sparse.keys.iter().zip(&keys).take_while(|(a, b)| a == b).count();
            let suffix = self.sparse.keys[prefix..].iter().rev().zip(keys[prefix..].iter().rev()).take_while(|(a, b)| a == b).count();
            let old_end = if self.sparse.active { self.sparse.keys.len() - suffix } else { self.state.item_count() };
            let new_end = keys.len() - suffix;
            let hints: Vec<f32> = (prefix..new_end)
                .map(|ix| {
                    self.sparse.heights.get(&keys[ix]).copied().unwrap_or_else(|| {
                        estimates
                            .and_then(|v| v.get(ix))
                            .and_then(|v| v.as_f64())
                            .map(|h| h as f32)
                            .unwrap_or(config.estimated_item_height.unwrap_or(1.))
                    })
                })
                .collect();
            self.state.kybern_splice_hinted(prefix..old_end, &hints);
            let valid: HashSet<&str> = keys.iter().map(String::as_str).collect();
            self.sparse.heights.retain(|key, _| valid.contains(key.as_str()));
            if !following {
                if let Some(ix) = anchor_key.as_ref().and_then(|key| keys.iter().position(|candidate| candidate == key)) {
                    self.state.scroll_to(gpui::ListOffset { item_ix: ix, offset_in_item: anchor.offset_in_item });
                }
            }
            self.sparse.keys = keys;
            self.sparse.version = version;
        }
        let indices: Vec<usize> =
            props.get("windowIndices").and_then(|v| v.as_array()).into_iter().flatten().filter_map(json_usize).collect();
        let old_rows: HashMap<u64, (u64, Option<gpui::FocusHandle>)> = self
            .child_ids
            .iter()
            .copied()
            .zip(self.child_revisions.iter().copied())
            .zip(self.row_focus_handles.iter().cloned())
            .map(|((id, rev), focus)| (id, (rev, focus)))
            .collect();
        self.sparse.children.clear();
        self.sparse.offsets.clear();
        self.sparse.revisions.clear();
        let mut handles = Vec::with_capacity(child_ids.len());
        let mut changed = Vec::new();
        for (offset, ((&ix, &id), &revision)) in indices.iter().zip(&child_ids).zip(&child_revisions).enumerate() {
            if ix >= self.state.item_count() {
                handles.push(None);
                continue;
            }
            self.sparse.children.insert(ix, id);
            self.sparse.offsets.insert(ix, offset);
            self.sparse.revisions.insert(ix, revision);
            let handle =
                focusable.contains(&id).then(|| old_rows.get(&id).and_then(|(_, h)| h.clone()).unwrap_or_else(|| cx.focus_handle()));
            if handle.is_some() {
                self.state.kybern_item_focus(ix, handle.clone());
            }
            // Newly mounted hosts may have measured estimates before React filled
            // the viewport; settled offscreen rows keep their previous height.
            if old_rows.get(&id).is_none_or(|(old, _)| *old != revision) {
                changed.push(ix);
            }
            handles.push(handle);
        }
        let mut range: Option<std::ops::Range<usize>> = None;
        for ix in changed {
            match range.as_mut() {
                Some(span) if span.end == ix => span.end += 1,
                _ => {
                    if let Some(span) = range.take() {
                        self.state.remeasure_items(span);
                    }
                    range = Some(ix..ix + 1);
                }
            }
        }
        if let Some(span) = range {
            self.state.remeasure_items(span);
        }
        self.sparse.active = true;
        self.config = config;
        self.window_start = window_start_from_element(element);
        self.child_ids = child_ids;
        self.child_revisions = child_revisions;
        self.row_focus_handles = handles;
        self.seen_rows.retain(|id| self.child_ids.contains(id));
    }
}
