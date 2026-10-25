// Derived editor geometry must not own decoded pixels. The document cache
// protects mounted documents and undo/redo sources; detached GPUI input/layout
// state may outlive its host and can retain only a non-owning image reference.
enum EditorLayoutImage {
    Borrowed(std::sync::Weak<gpui::RenderImage>),
    LegacyOwned(Arc<gpui::RenderImage>),
}

fn editor_layout_image_memory_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("KYBERN_EDITOR_LAYOUT_IMAGE_MEMORY").as_deref() != Ok("0"))
}

impl EditorLayoutImage {
    fn new(image: &Arc<gpui::RenderImage>) -> Self {
        if editor_layout_image_memory_enabled() {
            Self::Borrowed(Arc::downgrade(image))
        } else {
            Self::LegacyOwned(image.clone())
        }
    }

    fn get(&self) -> Option<Arc<gpui::RenderImage>> {
        match self {
            Self::Borrowed(image) => image.upgrade(),
            Self::LegacyOwned(image) => Some(image.clone()),
        }
    }
}
