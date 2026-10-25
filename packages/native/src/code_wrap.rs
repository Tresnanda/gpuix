//! Opt-in wrapped highlighted lines; reuse GPUI's native text shaping and selection.
use gpui::{Div, prelude::*};
pub(crate) fn content(el: Div, wrap: bool) -> Div {
    if wrap { el.w_full().min_w_0().flex_shrink_1().whitespace_normal() } else { el }
}
pub(crate) fn row(el: Div, wrap: bool, line_height: f32) -> Div {
    if wrap { el.h_auto().min_h(gpui::px(line_height)).w_full().min_w_0() } else { el }
}
