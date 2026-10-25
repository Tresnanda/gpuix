// Virtual rows are built during prepaint, sometimes more than once per row.
// Synchronize the global motion registry once per draw/tree version, while
// still observing a concurrent Node commit between root render and row build.
#[derive(Default)]
struct MotionFrameSync {
    key: Option<(u64, u64)>,
    active: bool,
}
fn sync_motion_frame(
    tree: &crate::retained_tree::RetainedTree,
    states: &mut HashMap<u64, crate::motion::MotionState>,
    memo: &mut MotionFrameSync,
    window: &gpui::Window,
    now: web_time::Instant,
) -> (bool, Vec<(u64, u64)>) {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ENABLED.get_or_init(|| std::env::var("KYBERN_MOTION_SYNC").as_deref() != Ok("0")) {
        return sync_motion_states(tree, states, now);
    }
    let key = (window.kybern_motion_frame_epoch(), tree.kybern_motion_revision());
    if memo.key == Some(key) {
        return (memo.active, Vec::new());
    }
    let result = sync_motion_states(tree, states, now);
    memo.key = Some(key);
    memo.active = result.0;
    result
}
