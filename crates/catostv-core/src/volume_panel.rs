//! The volume panel: a level on the right edge of the screen, opened by
//! the volume keys, over Home or over an app. Up and Down change the
//! volume, OK mutes, Left opens the list of sound outputs.

use crate::audio::{Sink, SinkKind};
use crate::input::Action;

/// Volume change per key press, in percent.
pub const STEP: i8 = 5;
/// How long the panel stays after the last key (seconds).
pub const SHOW_SECS: f32 = 3.0;
/// …and with the outputs list open.
pub const LIST_SECS: f32 = 6.0;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VolumePanel {
    /// The outputs list is open, with this row focused.
    pub outputs: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelEffect {
    Step(i8),
    ToggleMute,
    SetSink(u32),
    Close,
}

impl VolumePanel {
    pub fn secs(&self) -> f32 {
        if self.outputs.is_some() {
            LIST_SECS
        } else {
            SHOW_SECS
        }
    }

    pub fn handle(&mut self, action: Action, sinks: &[Sink]) -> Vec<PanelEffect> {
        if let Some(index) = self.outputs.as_mut() {
            let n = sinks.len();
            match action {
                Action::Up => *index = index.saturating_sub(1),
                Action::Down if n > 0 => *index = (*index + 1).min(n - 1),
                Action::Ok | Action::OkLong => {
                    let pick = sinks.get(*index).filter(|s| !s.is_default).map(|s| s.id);
                    self.outputs = None;
                    return pick.map(PanelEffect::SetSink).into_iter().collect();
                }
                Action::Right | Action::Back => self.outputs = None,
                Action::Home => return vec![PanelEffect::Close],
                _ => {}
            }
            return vec![];
        }
        match action {
            Action::Up => vec![PanelEffect::Step(STEP)],
            Action::Down => vec![PanelEffect::Step(-STEP)],
            Action::Ok | Action::OkLong => vec![PanelEffect::ToggleMute],
            Action::Left | Action::Options if !sinks.is_empty() => {
                self.outputs = Some(sinks.iter().position(|s| s.is_default).unwrap_or(0));
                vec![]
            }
            Action::Back | Action::Home | Action::Quick => vec![PanelEffect::Close],
            _ => vec![],
        }
    }
}

/// A short name for output `i`: "TV (HDMI)" rather than "Built-in
/// Audio Digital Stereo (HDMI)", unless two outputs would read the same.
pub fn sink_label(sinks: &[Sink], i: usize) -> String {
    let Some(s) = sinks.get(i) else {
        return String::new();
    };
    let short = |s: &Sink| match s.kind {
        SinkKind::Hdmi | SinkKind::Analog => s.kind.label().to_string(),
        _ => s.name.clone(),
    };
    let label = short(s);
    if sinks
        .iter()
        .enumerate()
        .any(|(k, o)| k != i && short(o) == label)
    {
        s.name.clone()
    } else {
        label
    }
}

/// Icon name for an output.
pub fn sink_icon(kind: SinkKind) -> &'static str {
    match kind {
        SinkKind::Hdmi => "tv",
        SinkKind::Bluetooth => "headphones",
        SinkKind::Usb => "usb",
        SinkKind::Analog | SinkKind::Other => "speaker",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sinks() -> Vec<Sink> {
        let s = |id, name: &str, kind, is_default| Sink {
            id,
            name: name.into(),
            node_name: None,
            is_default,
            kind,
        };
        vec![
            s(40, "TV", SinkKind::Hdmi, false),
            s(52, "Buds", SinkKind::Bluetooth, true),
        ]
    }

    #[test]
    fn short_output_names() {
        let mut list = sinks();
        assert_eq!(sink_label(&list, 0), "TV (HDMI)");
        assert_eq!(sink_label(&list, 1), "Buds");
        list[1].kind = SinkKind::Hdmi;
        list[1].name = "Receiver".into();
        assert_eq!(sink_label(&list, 0), "TV");
        assert_eq!(sink_label(&list, 5), "");
    }

    #[test]
    fn keys_change_the_level_and_mute() {
        let mut p = VolumePanel::default();
        assert_eq!(p.handle(Action::Up, &sinks()), [PanelEffect::Step(5)]);
        assert_eq!(p.handle(Action::Down, &sinks()), [PanelEffect::Step(-5)]);
        assert_eq!(p.handle(Action::Ok, &sinks()), [PanelEffect::ToggleMute]);
        assert_eq!(p.handle(Action::Back, &sinks()), [PanelEffect::Close]);
    }

    #[test]
    fn left_opens_outputs_on_the_current_one() {
        let mut p = VolumePanel::default();
        assert!(p.handle(Action::Left, &sinks()).is_empty());
        assert_eq!(p.outputs, Some(1));
        assert_eq!(p.secs(), LIST_SECS);
        // Picking the current output just closes the list.
        assert!(p.handle(Action::Ok, &sinks()).is_empty());
        assert_eq!(p.outputs, None);
        p.handle(Action::Left, &sinks());
        p.handle(Action::Up, &sinks());
        assert_eq!(p.handle(Action::Ok, &sinks()), [PanelEffect::SetSink(40)]);
        // Back closes the list, not the panel.
        p.handle(Action::Left, &sinks());
        assert!(p.handle(Action::Back, &sinks()).is_empty());
        assert_eq!(p.outputs, None);
        // No outputs known: no list.
        assert!(p.handle(Action::Left, &[]).is_empty());
        assert_eq!(p.outputs, None);
    }
}
