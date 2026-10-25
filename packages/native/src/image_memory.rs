// Included in custom_elements::img. The retained visible/overscan host owns a
// lease, including a lightbox's independent lease. Inactive entries are LRU and
// byte bounded; leaving the last image screen flushes them all. No idle timer.
use std::cell::RefCell;
use std::collections::HashMap;

#[derive(Debug)]
struct ResidentImage {
    source: ImgSource,
    leases: usize,
    touched: u64,
}

#[derive(Default)]
struct ImageMemory {
    entries: HashMap<String, ResidentImage>,
    clock: u64,
    evictions: u64,
}

thread_local! {
    static IMAGE_MEMORY: RefCell<ImageMemory> = RefCell::new(ImageMemory::default());
}

fn image_memory_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("KYBERN_IMAGE_MEMORY").as_deref() != Ok("0"))
}

fn image_source(source: &ImgSource) -> Option<gpui::ImageSource> {
    match source {
        ImgSource::Path(path) => Some(path.clone().into()),
        ImgSource::Uri(uri) => Some(uri.clone().into()),
        ImgSource::Data(image) => Some(image.clone().into()),
        ImgSource::Render(image) => Some(image.clone().into()),
        _ => None,
    }
}

fn lease_image(src: &str, create: impl FnOnce() -> ImgSource) -> ImgSource {
    if !image_memory_enabled() || src.is_empty() { return create(); }
    IMAGE_MEMORY.with(|memory| {
        let mut memory = memory.borrow_mut();
        memory.clock += 1;
        let touched = memory.clock;
        let entry = memory.entries.entry(src.to_owned()).or_insert_with(|| ResidentImage {
            source: create(), leases: 0, touched,
        });
        entry.leases += 1;
        entry.touched = touched;
        entry.source.clone()
    })
}

fn release_image(src: &str) {
    if !image_memory_enabled() { return; }
    IMAGE_MEMORY.with(|memory| {
        let mut memory = memory.borrow_mut();
        memory.clock += 1;
        let touched = memory.clock;
        if let Some(entry) = memory.entries.get_mut(src) {
            entry.leases = entry.leases.saturating_sub(1);
            entry.touched = touched;
        }
    });
}

fn image_cost(key: &str, entry: &ResidentImage, cx: &gpui::App) -> (usize, usize) {
    let encoded = if let ImgSource::Data(image) = &entry.source { image.bytes().len() } else { 0 };
    let decoded = image_source(&entry.source).and_then(|source| source.cached_image(cx))
        .map_or(0, |image| (0..image.frame_count()).map(|frame| image.as_bytes(frame).map_or(0, <[u8]>::len)).sum());
    // GPU estimate is pixel bytes, not actual allocated atlas texture capacity.
    (key.len().saturating_add(encoded).saturating_add(decoded), decoded)
}

pub(crate) fn image_memory_stats(cx: &gpui::App) -> serde_json::Value {
    IMAGE_MEMORY.with(|memory| {
        let memory = memory.borrow();
        let (mut cpu, mut gpu, mut active, mut dormant) = (0usize, 0usize, 0usize, 0usize);
        for (key, entry) in &memory.entries {
            let cost = image_cost(key, entry, cx);
            cpu = cpu.saturating_add(cost.0); gpu = gpu.saturating_add(cost.1);
            if entry.leases > 0 { active += 1; } else { dormant += cost.0 + cost.1; }
        }
        serde_json::json!({"entries":memory.entries.len(),"leasedImages":active,"cpuBytes":cpu,
            "gpuPixelBytesEstimate":gpu,"dormantBytes":dormant,"dormantBudgetBytes":16*1024*1024,
            "evictions":memory.evictions,"enabled":image_memory_enabled()})
    })
}

pub(crate) fn trim_image_memory(window: &mut gpui::Window, cx: &mut gpui::App) {
    if !image_memory_enabled() { return; }
    let victims = IMAGE_MEMORY.with(|memory| {
        let mut memory = memory.borrow_mut();
        let active = memory.entries.values().any(|entry| entry.leases > 0);
        let mut dormant: Vec<_> = memory.entries.iter().filter(|(_, entry)| entry.leases == 0)
            .map(|(key, entry)| { let cost = image_cost(key, entry, cx); (key.clone(), entry.touched, cost.0.saturating_add(cost.1)) }).collect();
        dormant.sort_unstable_by_key(|entry| entry.1);
        let mut bytes: usize = dormant.iter().map(|entry| entry.2).sum();
        let mut count = memory.entries.len();
        let mut victims = Vec::new();
        for (key, _, cost) in dormant {
            if active && bytes <= 16 * 1024 * 1024 && count <= 128 { break; }
            if let Some(entry) = memory.entries.remove(&key) {
                victims.push(entry.source);
                bytes = bytes.saturating_sub(cost); count -= 1; memory.evictions += 1;
            }
        }
        victims
    });
    for source in victims {
        if let Some(source) = image_source(&source) { source.evict(Some(&mut *window), cx); }
    }
}
