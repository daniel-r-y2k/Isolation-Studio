// Copyright (C) 2026 ItsAlphaNeon
// SPDX-License-Identifier: GPL-3.0-or-later

//! Integrated mixer sidechain controls.
use crate::app::State;
use crate::mixer::{self, MCmd};
use crate::playlist::{Cmd, Mark, PItem, Popup};
pub use os_engine::effects::sidechain::{Settings, LABELS, NAME};

pub fn slot(s: &State, track: u16) -> Option<usize> {
    mixer::track(s, track)?.slots.iter().position(|p| p.as_ref().is_some_and(|p| p.name == NAME))
}

pub fn settings(s: &State, track: u16) -> Settings {
    mixer::track(s, track).and_then(|m| Settings::decode(&m.sidechain).or_else(|| {
        let k = slot(s, track)?;
        let mut st = Settings::decode(&m.slots[k].as_ref()?.state)?;
        st.enabled = m.slot_on[k];
        st.values[5] *= m.slot_mix[k] as f32;
        Some(st)
    })).unwrap_or_default()
}

pub fn enabled(s: &State, track: u16) -> bool { settings(s, track).enabled }

pub fn store(s: &mut State, track: u16, value: Settings) {
    if let Some(m) = mixer::track_mut(s, track) {
        m.sidechain = value.encode();
        for k in 0..m.slots.len() {
            if m.slots[k].as_ref().is_some_and(|p| p.name == NAME && Settings::decode(&p.state).is_some()) {
                m.slots[k] = None;
                m.slot_on[k] = true;
                m.slot_mix[k] = 1.0;
                m.slot_cpu[k] = Default::default();
            }
        }
        s.piano.dirty = true;
    }
}

pub fn toggle(s: &mut State) -> Option<String> {
    let t = s.selected_mixer;
    let mut st = settings(s, t);
    st.enabled = !st.enabled;
    store(s, t, st);
    None
}

pub fn position(s: &mut State) -> Option<String> {
    let t = s.selected_mixer;
    let mut st = settings(s, t);
    st.pre_fx = !st.pre_fx;
    store(s, t, st);
    None
}

pub fn source_menu(s: &State, at: (f32, f32)) -> Popup {
    let t = s.selected_mixer;
    let current = settings(s, t).source;
    let mut items = vec![PItem::new("None", Cmd::Mixer(MCmd::SidechainSource(None))).mark(Mark::Radio(current.is_none()))];
    for m in &s.project.mixer {
        if m.index != 0 && m.index != t && !mixer::makes_loop(s, m.index, t) {
            items.push(PItem::new(mixer::name(s, m.index), Cmd::Mixer(MCmd::SidechainSource(Some(m.index))))
                .mark(Mark::Radio(current == Some(m.index))));
        }
    }
    Popup { x: at.0, y: at.1, items, sub: None, scroll: [0; 2] }
}

pub fn source(s: &mut State, source: Option<u16>) -> Option<String> {
    let t = s.selected_mixer;
    if let Some(src) = source {
        if src == 0 || mixer::track(s, src).is_none() || mixer::makes_loop(s, src, t) {
            return Some("Choose a different trigger track; this routing would create a feedback loop.".into());
        }
        let m = mixer::track_mut(s, src)?;
        // Keep existing audio sends and the source's master output intact.
        if !m.routes.contains(&t) {
            m.routes.push(t);
            m.routes.sort_unstable();
            m.send_levels.retain(|&(dest, _)| dest != t);
            m.send_levels.push((t, 0.0));
        }
    }
    store(s, t, Settings { source, ..settings(s, t) });
    None
}

pub fn remap(s: &mut State, f: &dyn Fn(u16) -> Option<u16>) {
    for m in &mut s.project.mixer {
        if let Some(mut st) = Settings::decode(&m.sidechain) {
            st.source = st.source.and_then(f);
            m.sidechain = st.encode();
        }
        for p in m.slots.iter_mut().flatten().filter(|p| p.name == NAME) {
            if let Some(mut st) = Settings::decode(&p.state) {
                st.source = st.source.and_then(f);
                p.state = st.encode();
            }
        }
    }
}

/// UI-only history sampled at 30 Hz from the audio thread's atomic DSP reading.
pub struct Preview {
    pub samples: [f32; 96],
    pub levels: [[f32; 2]; 96],
    pub next: usize,
    pub current: f32,
    track: Option<u16>,
    at: Option<std::time::Instant>,
}
impl Default for Preview {
    fn default() -> Self { Self { samples: [0.0; 96], levels: [[0.0; 2]; 96], next: 0, current: 0.0, track: None, at: None } }
}
impl Preview {
    pub fn feed(&mut self, track: u16, value: f32, levels: [f32; 2], active: bool, now: std::time::Instant) -> bool {
        if self.track != Some(track) || !active {
            self.samples.fill(0.0);
            self.levels.fill([0.0; 2]);
            self.next = 0;
            self.current = 0.0;
            self.track = Some(track);
        }
        if self.at.is_some_and(|at| now.saturating_duration_since(at) < std::time::Duration::from_millis(33)) { return false; }
        self.at = Some(now);
        self.current = if active && value.is_finite() { value.max(0.0) } else { 0.0 };
        self.samples[self.next] = self.current;
        self.levels[self.next] = levels.map(|v| if active && v.is_finite() { v.max(0.0) } else { 0.0 });
        self.next = (self.next + 1) % self.samples.len();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{Action, App, Win};
    use crate::knobs::KnobId;
    use crate::mixer::MHit;

    fn app() -> App {
        let mut a = App::new(None);
        a.state.project = crate::app::test_project();
        // Project files store the mixer after an arrangement, as in a normal app project.
        a.state.project.arrangements.push(os_flp::project::default_arrangement());
        a.state.control.settle(&mut a.state.project);
        a.state.selected_mixer = 1;
        a.state.z = vec![Win::Mixer];
        a
    }

    #[test]
    fn sidechain_setup_undo_and_track_state() {
        let mut a = app();
        let before = a.state.project.clone();
        mixer::hit(&mut a, MHit::SidechainEnable);
        assert!(enabled(&a.state, 1));
        mixer::run(&mut a, MCmd::SidechainSource(Some(2)));
        assert_eq!(mixer::send(&a.state, 2, 1), Some(0.0));
        assert!(mixer::track(&a.state, 2).unwrap().routes.contains(&0));
        assert_eq!(settings(&a.state, 1).source, Some(2));
        let id = KnobId::Sidechain(1, 0);
        crate::history::knob(&mut a.state, id, false);
        a.state.set_knob(id, 0.25);
        assert_eq!(settings(&a.state, 1).values[0], -45.0);
        crate::history::undo(&mut a.state);
        assert_eq!(settings(&a.state, 1).values[0], -24.0);
        mixer::hit(&mut a, MHit::SidechainPosition);
        assert!(settings(&a.state, 1).pre_fx);
        crate::history::undo(&mut a.state);
        assert!(!settings(&a.state, 1).pre_fx);
        let file = os_flp::project::create(&a.state.project).unwrap();
        let saved = os_flp::project::load(&file).unwrap();
        assert_eq!(saved.mixer[1].sidechain, a.state.project.mixer[1].sidechain);
        assert!(saved.mixer[1].slots.iter().all(Option::is_none));
        let st = crate::mixer_state::TrackState::of(mixer::track(&a.state, 1).unwrap());
        assert_eq!(crate::mixer_state::TrackState::from_json(&st.to_json()).unwrap(), st);
        mixer::hit(&mut a, MHit::SidechainEnable);
        assert!(!enabled(&a.state, 1));
        assert_eq!(settings(&a.state, 1).source, Some(2));
        crate::history::undo(&mut a.state);
        assert!(enabled(&a.state, 1));
        crate::history::undo(&mut a.state);
        assert_eq!(settings(&a.state, 1).source, None);
        assert!(!mixer::track(&a.state, 2).unwrap().routes.contains(&1));
        crate::history::undo(&mut a.state);
        assert_eq!(a.state.project.mixer, before.mixer);
    }

    #[test]
    fn sidechain_routes_are_safe_and_sources_follow_tracks() {
        let mut a = app();
        toggle(&mut a.state);
        assert!(source(&mut a.state, Some(1)).is_some());
        assert!(source(&mut a.state, Some(0)).is_some());
        mixer::track_mut(&mut a.state, 2).unwrap().routes.push(1);
        mixer::track_mut(&mut a.state, 2).unwrap().send_levels.push((1, 0.4));
        source(&mut a.state, Some(2));
        assert_eq!(mixer::send(&a.state, 2, 1), Some(0.4));
        mixer::run(&mut a, MCmd::MoveRight(2));
        assert_eq!(settings(&a.state, 1).source, Some(3));
        mixer::run(&mut a, MCmd::Delete(3));
        assert_eq!(settings(&a.state, 1).source, None);
        let p = Some(os_model::SlotPlugin { id: os_model::Id::NONE, name: "Existing FX".into(), state: vec![] });
        mixer::track_mut(&mut a.state, 2).unwrap().slots.fill(p);
        a.state.selected_mixer = 2;
        // Filled with other effects: enabling must never replace one.
        for p in mixer::track_mut(&mut a.state, 2).unwrap().slots.iter_mut().flatten() { p.name = "Existing FX".into(); }
        assert!(toggle(&mut a.state).is_none());
        assert!(enabled(&a.state, 2));
        assert!(mixer::track(&a.state, 2).unwrap().slots.iter().flatten().all(|p| p.name == "Existing FX"));
    }

    #[test]
    fn sidechain_panel_fits_and_controls_work() {
        let mut a = app();
        let mut c = os_ui::Canvas::new(1600, 1000, 1.0).unwrap();
        a.paint(&mut c, None);
        let r = a.hits.iter().find(|h| h.action == Action::Mix(MHit::SidechainEnable)).unwrap().rect;
        a.mouse_move(r.cx(), r.cy());
        a.mouse_down();
        a.mouse_up();
        assert!(enabled(&a.state, 1));
        source(&mut a.state, Some(2));
        for height in [400.0, 600.0, 800.0] {
            a.state.wins[Win::Mixer.slot().unwrap()].rect = os_ui::Rect::new(230.0, 90.0, 1100.0, height);
            a.state.wins[Win::Mixer.slot().unwrap()].maxed = false;
            a.paint(&mut c, None);
            let eq = a.hits.iter().find(|h| h.action == Action::Mix(MHit::EqGraph)).unwrap().rect;
            let enable = a.hits.iter().find(|h| h.action == Action::Mix(MHit::SidechainEnable)).unwrap().rect;
            let output = a.hits.iter().find(|h| h.action == Action::Mix(MHit::OutLatency)).unwrap().rect;
            assert!(eq.bottom() <= enable.y, "EQ and sidechain must not overlap at {height}");
            for k in 0..6 {
                let field = a.hits.iter().find(|h| h.action == Action::Knob(KnobId::Sidechain(1, k))).unwrap().rect;
                assert!(field.y > enable.bottom() && field.bottom() < output.y);
            }
        }
        if let Ok(path) = std::env::var("LEMON_SIDECHAIN_PREVIEW") { c.pixmap.save_png(path).unwrap(); }
    }
}
