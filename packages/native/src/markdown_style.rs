//! Optional Markdown layout/paint controls. Defaults retain upstream rendering.
use serde::Deserialize;
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MarkdownStyle {
    pub link: LinkStyle,
    pub table_border_width: Option<f32>,
    pub table_header_rule_width: Option<f32>,
    pub table_padding_x: Option<f32>,
    pub table_padding_y: Option<f32>,
    pub table_header_weight: Option<f32>,
    pub quote_border_width: Option<f32>,
    pub quote_padding_left: Option<f32>,
    pub quote_padding_right: Option<f32>,
    pub quote_padding_y: Option<f32>,
    pub quote_gap: Option<f32>,
    pub list_indent: Option<f32>,
    pub list_marker_gap: Option<f32>,
    pub list_child_gap: Option<f32>,
    pub list_item_gap: Option<f32>,
    pub list_marker_size: Option<f32>,
    /// Bullet discs sit this far left of the text (ordered numbers keep `list_marker_gap`).
    pub list_bullet_gap: Option<f32>,
    /// Margin above and below a thematic break, collapsed with its neighbours like CSS.
    pub rule_margin: Option<f32>,
    pub inline_code_padding_x: Option<f32>,
    pub inline_code_padding_y: Option<f32>,
    pub inline_code_size_ratio: Option<f32>,
    pub heading_sizes: Option<Vec<f32>>,
    pub heading_line_heights: Option<Vec<f32>>,
    pub heading_weights: Option<Vec<f32>>,
    pub heading_margin_tops: Option<Vec<f32>>,
    pub heading_following_gap: Option<f32>,
    pub heading_margin_top: Option<f32>,
    pub heading_margin_bottom: Option<f32>,
    pub table_border: Option<String>,
    pub table_header_background: Option<String>,
    pub quote_border: Option<String>,
    pub quote_background: Option<String>,
    pub quote_text: Option<String>,
    pub list_marker: Option<String>,
    pub code_background: Option<String>,
    pub code_border: Option<String>,
    pub code_border_width: Option<f32>,
    pub code_header_border_width: Option<f32>,
    pub code_header_line_height: Option<f32>,
    pub code_header_background: Option<String>,
    pub strong_weight: Option<f32>,
    pub nested_list_markers: bool,
    pub table_grid: bool,
    pub code_wrap: bool,
    pub code_header: Option<bool>,
    /// Links whose URL starts with `prefix` lead with an icon (`kybern://note/…` chips).
    pub link_icons: Vec<LinkIcon>,
    /// A live answer: the caret after the last text and the fading of its newest words.
    pub stream: Option<StreamStyle>,
}
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LinkIcon {
    pub prefix: String,
    /// SVG with `currentColor`, drawn at 0.95em before the label.
    pub svg: String,
    /// CSS colour that replaces `currentColor`.
    pub color: String,
    /// Label colour and weight of such a link (a chip reads in the accent at weight 500).
    pub text_color: Option<String>,
    pub weight: Option<f32>,
}
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct StreamStyle {
    pub caret: bool,
    pub caret_color: Option<String>,
    /// The blink phase, driven by the app: nothing is drawn while false.
    pub caret_visible: bool,
    /// Birth times (epoch ms) of the newest words, oldest first.
    pub born: Vec<f64>,
    pub fade_ms: Option<f32>,
}
pub fn number(value: Option<f32>, fallback: f32) -> f32 {
    value.filter(|n| n.is_finite() && *n >= 0.0).unwrap_or(fallback)
}
pub fn color(value: &Option<String>, fallback: gpui::Hsla) -> gpui::Hsla {
    value.as_deref().and_then(crate::color::parse_color_rgba).map(Into::into).unwrap_or(fallback)
}

pub fn tier(values: &Option<Vec<f32>>, level: u8) -> Option<f32> {
    values.as_ref().and_then(|v| v.get(level.saturating_sub(1) as usize)).copied()
}
pub fn heading(theme: &crate::theme::Theme, level: u8) -> (f32, f32) {
    let (size, line) = theme.metrics.heading(level);
    (number(tier(&theme.markdown.heading_sizes, level), size), number(tier(&theme.markdown.heading_line_heights, level), line))
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LinkStyle {
    pub color: Option<String>,
    pub underline: Option<String>,
    pub weight: Option<f32>,
}
