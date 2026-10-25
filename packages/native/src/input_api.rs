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
    let editor = editor_for(id)?;
    let state = editor.read(cx);
    let range = state.range_to_utf16(&state.selected_range);
    Ok(serde_json::json!({"start": range.start, "end": range.end,
        "reversed": state.selection_reversed, "composing": state.marked_range.is_some()}))
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
