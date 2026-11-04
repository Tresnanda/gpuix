// Included in custom_elements/input.rs so editor internals remain private.
thread_local! {
    static EDITORS: std::cell::RefCell<std::collections::HashMap<u64, gpui::WeakEntity<TextEditorState>>> = Default::default();
}

fn remember_editor(id: u64, state: &Entity<TextEditorState>) {
    EDITORS.with(|editors| {
        let mut editors = editors.borrow_mut();
        editors.retain(|_, editor| editor.upgrade().is_some());
        editors.insert(id, state.downgrade());
    });
}

fn editor_for(id: u64) -> napi::Result<Entity<TextEditorState>> {
    EDITORS
        .with(|editors| editors.borrow().get(&id).and_then(|e| e.upgrade()))
        .ok_or_else(|| napi::Error::from_reason("Input is not mounted"))
}

// Reject malformed UTF-16 boundaries instead of splitting surrogate pairs.
fn checked_range(text: &str, start: u32, end: u32) -> napi::Result<std::ops::Range<usize>> {
    let mut boundaries = vec![(0, 0)];
    let mut utf16 = 0;
    for (byte, ch) in text.char_indices() {
        utf16 += ch.len_utf16() as u32;
        boundaries.push((utf16, byte + ch.len_utf8()));
    }
    let find = |offset| boundaries.iter().find(|(units, _)| *units == offset).map(|(_, byte)| *byte);
    match (find(start), find(end)) {
        (Some(start), Some(end)) if start <= end => Ok(start..end),
        _ => Err(napi::Error::from_reason("Selection must use ordered UTF-16 character boundaries")),
    }
}

pub(crate) fn input_selection(id: u64, cx: &gpui::App) -> napi::Result<serde_json::Value> {
    Ok(selection_json(editor_for(id)?.read(cx).selection_snapshot()))
}

fn selection_json(selection: crate::element_tree::InputSelectionPayload) -> serde_json::Value {
    serde_json::json!({"start": selection.start, "end": selection.end,
        "reversed": selection.reversed, "composing": selection.composing})
}

// Kybern (#215): off macOS every input's selection as the UI thread last saw
// it, so `getInputSelection` answers on the JS thread instead of waiting for
// the UI thread (about a frame while an answer streams). The UI thread writes
// it before it emits any of the input's events and whenever it renders a moved
// selection, and a waited `setInputSelection`/`editInput` writes it before it
// returns, so a read never sees a state older than the event JS is handling or
// its own last write. While intercepted keys wait for JS's answer it is not
// used (`key_intercept::keys_queued`): the keys queued behind them replay only
// after those answers, which the waited read runs behind. Unlike the waited
// read, it does not wait for mutation batches still queued for the UI thread
// (a `value` the UI thread has not applied yet). macOS reads the editor
// directly on its one thread.
#[cfg(not(target_os = "macos"))]
static SELECTION_MIRROR: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<u64, (u64, crate::element_tree::InputSelectionPayload)>>,
> = std::sync::LazyLock::new(Default::default);
#[cfg(not(target_os = "macos"))]
static MIRROR_OWNER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The mirrored selection of a mounted input, if the UI thread published one.
#[cfg(not(target_os = "macos"))]
pub(crate) fn mirrored_selection(id: u64) -> Option<serde_json::Value> {
    let mirror = SELECTION_MIRROR.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    mirror.get(&id).map(|(_, selection)| selection_json(*selection))
}

/// Publish this input's selection now (a replayed key moved it); nothing for
/// another element or on macOS.
pub(crate) fn publish_input_selection(id: u64, cx: &mut gpui::App) {
    if let Ok(editor) = editor_for(id) {
        editor.update(cx, |state, _| state.publish_selection());
    }
}

#[cfg(not(target_os = "macos"))]
fn next_mirror_owner() -> u64 {
    MIRROR_OWNER.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
}
#[cfg(target_os = "macos")]
fn next_mirror_owner() -> u64 {
    0
}

impl TextEditorState {
    fn selection_snapshot(&self) -> crate::element_tree::InputSelectionPayload {
        let range = self.range_to_utf16(&self.selected_range);
        crate::element_tree::InputSelectionPayload {
            start: range.start as u32,
            end: range.end as u32,
            reversed: self.selection_reversed,
            composing: self.marked_range.is_some(),
        }
    }

    /// Publish the current selection to the mirror and return it for an event
    /// payload. On macOS nothing is published and events carry no selection.
    #[cfg(not(target_os = "macos"))]
    fn publish_selection(&mut self) -> Option<crate::element_tree::InputSelectionPayload> {
        let selection = self.selection_snapshot();
        self.mirrored = Some(self.mirror_key());
        SELECTION_MIRROR
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(self.element_id, (self.mirror_owner, selection));
        Some(selection)
    }
    #[cfg(target_os = "macos")]
    fn publish_selection(&mut self) -> Option<crate::element_tree::InputSelectionPayload> {
        None
    }

    /// What the mirror was last written from: the UTF-16 offsets depend on the
    /// text before the caret too, so its length is part of it.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    fn mirror_key(&self) -> (Range<usize>, bool, bool, usize) {
        (self.selected_range.clone(), self.selection_reversed, self.marked_range.is_some(), self.content.len())
    }

    /// On render: publish a selection that moved without an event (a drag, a
    /// value written by React).
    fn publish_moved_selection(&mut self) {
        #[cfg(not(target_os = "macos"))]
        if self.mirrored.as_ref() != Some(&self.mirror_key()) {
            self.publish_selection();
        }
    }
}

#[cfg(not(target_os = "macos"))]
impl Drop for TextEditorState {
    fn drop(&mut self) {
        let mut mirror = SELECTION_MIRROR.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        // A remount under the same element id may already own the entry.
        if mirror.get(&self.element_id).is_some_and(|(owner, _)| *owner == self.mirror_owner) {
            mirror.remove(&self.element_id);
        }
    }
}

pub(crate) fn select_input(id: u64, start: u32, end: u32, reversed: bool, cx: &mut gpui::App) -> napi::Result<()> {
    editor_for(id)?.update(cx, |state, cx| {
        if state.marked_range.is_some() {
            return Err(napi::Error::from_reason("Input is composing"));
        }
        state.selected_range = state.expand_atoms(checked_range(&state.content, start, end)?);
        state.selection_reversed = reversed;
        state.last_edit = None;
        state.follow_cursor = true;
        state.reset_blink(cx);
        state.publish_selection();
        cx.notify();
        Ok(())
    })
}

pub(crate) fn edit_input(id: u64, edit: serde_json::Value, window: &mut gpui::Window, cx: &mut gpui::App) -> napi::Result<()> {
    editor_for(id)?.update(cx, |state, cx| {
        if state.read_only || state.secure || state.marked_range.is_some() {
            return Err(napi::Error::from_reason("Input is read-only, secure, or composing"));
        }
        let expected =
            edit.get("expectedValue").and_then(|v| v.as_str()).ok_or_else(|| napi::Error::from_reason("expectedValue is required"))?;
        if state.content != expected {
            return Err(napi::Error::from_reason("Input changed before transaction"));
        }
        let offset = |name| {
            edit.get(name)
                .and_then(|v| v.as_u64())
                .and_then(|v| u32::try_from(v).ok())
                .ok_or_else(|| napi::Error::from_reason(format!("Invalid {name}")))
        };
        let start = offset("start")?;
        let end = offset("end")?;
        checked_range(&state.content, start, end)?;
        let text = edit.get("text").and_then(|v| v.as_str()).ok_or_else(|| napi::Error::from_reason("text is required"))?;
        // Explicit transactions are individual undo steps, independent of typing.
        state.last_edit = None;
        state.replace_text_in_range(Some(start as usize..end as usize), text, window, cx);
        state.last_edit = None;
        state.publish_selection();
        Ok(())
    })
}

pub(crate) fn utf16_offset(value: f64) -> napi::Result<u32> {
    if value.is_finite() && value >= 0.0 && value.fract() == 0.0 && value <= u32::MAX as f64 {
        Ok(value as u32)
    } else {
        Err(napi::Error::from_reason("Offset must be a nonnegative UTF-16 integer"))
    }
}
