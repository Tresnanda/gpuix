//! Optional diff controls; omitted fields preserve upstream defaults.
use serde::Deserialize;
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DiffStyle {
    pub file_headers: Option<bool>,
    pub notices: Option<bool>,
    pub syntax_highlight: Option<bool>,
    pub horizontal_scroll: bool,
    pub table_layout: bool,
    pub show_more_batch: Option<u32>,
    pub gutter_digit_width: Option<f32>,
    pub scrollbar_height: Option<f32>,
    pub scrollbar_color: Option<String>,
    pub add_background: Option<String>,
    pub delete_background: Option<String>,
    pub add_gutter_background: Option<String>,
    pub delete_gutter_background: Option<String>,
    pub number_color: Option<String>,
    pub text_color: Option<String>,
    pub gutter_font: Option<String>,
    pub gutter_text_size: Option<f32>,
    pub gutter_padding_right: Option<f32>,
    pub content_padding_left: Option<f32>,
    pub context_marker: Option<String>,
    pub delete_marker: Option<String>,
}
