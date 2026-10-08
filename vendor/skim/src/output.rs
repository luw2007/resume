use crate::SkimItem;
use crate::event::Event;
use skim_tuikit::key::Key;
use std::sync::Arc;

pub struct SkimOutput {
    /// The final event that makes skim accept/quit.
    /// Was designed to determine if skim quit or accept.
    /// Typically there are only two options: `Event::EvActAbort` | `Event::EvActAccept`
    pub final_event: Event,

    /// quick pass for judging if skim aborts.
    pub is_abort: bool,

    /// The final key that makes skim accept/quit.
    /// Note that it might be Key::Null if it is triggered by skim.
    pub final_key: Key,

    /// The query
    pub query: String,

    /// The command query
    pub cmd: String,

    /// The selected items.
    pub selected_items: Vec<Arc<dyn SkimItem>>,

    /// Whether the side preview pane was visible when skim exited.
    ///
    /// Lets a caller that relaunches skim (for example on a tab switch) carry the
    /// user's `toggle-preview` choice over. Always `false` for the tmux backend,
    /// which has no preview state.
    pub preview_visible: bool,
}
