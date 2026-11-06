//! Opt-in wrapped highlighted lines; reuse GPUI's native text shaping and selection.
//! Shared by `<code softWrap>` and `<diff softWrap>`.
use gpui::Styled;
pub(crate) fn content<E: Styled>(el: E, wrap: bool) -> E {
    if wrap { el.w_full().min_w_0().flex_shrink_1().whitespace_normal() } else { el }
}
pub(crate) fn row<E: Styled>(el: E, wrap: bool, line_height: f32) -> E {
    if wrap { el.h_auto().min_h(gpui::px(line_height)).w_full().min_w_0() } else { el }
}
