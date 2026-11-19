// Copyright (C) 2026 ItsAlphaNeon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The mixer detail panel's stereo-linked, externally keyed compressor.
//! State is stored on the mixer track; LSC1 slot states remain readable.

use super::Effect;

pub const NAME: &str = "Lemon Sidechain";
pub const LABELS: [&str; 6] = ["Threshold", "Ratio", "Attack", "Release", "Knee", "Mix"];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    pub source: Option<u16>,
    pub enabled: bool,
    pub pre_fx: bool,
    /// dB, ratio, ms, ms, dB, wet fraction.
    pub values: [f32; 6],
}

impl Default for Settings {
    fn default() -> Self {
        Self { source: None, enabled: false, pre_fx: false, values: [-24.0, 4.0, 5.0, 150.0, 6.0, 1.0] }
    }
}

impl Settings {
    pub fn value(k: u8, n: f32) -> f32 {
        let n = n.clamp(0.0, 1.0);
        match k {
            0 => -60.0 + 60.0 * n,
            1 => 1.0 + 19.0 * n,
            2 => 0.1 * 1000f32.powf(n),
            3 => 10.0 * 200f32.powf(n),
            4 => 24.0 * n,
            _ => n,
        }
    }

    pub fn position(k: u8, v: f32) -> f32 {
        match k {
            0 => (v + 60.0) / 60.0,
            1 => (v - 1.0) / 19.0,
            2 => (v.max(0.1) / 0.1).ln() / 1000f32.ln(),
            3 => (v.max(10.0) / 10.0).ln() / 200f32.ln(),
            4 => v / 24.0,
            _ => v,
        }.clamp(0.0, 1.0)
    }

    pub fn text(k: u8, n: f32) -> String {
        let v = Self::value(k, n);
        match k {
            0 | 4 => format!("{v:.1} dB"),
            1 => format!("{v:.1}:1"),
            2 | 3 => format!("{v:.1} ms"),
            _ => format!("{:.0}%", v * 100.0),
        }
    }

    pub fn encode(self) -> Vec<u8> {
        let mut b = b"LSC1".to_vec();
        b.extend(self.source.unwrap_or(u16::MAX).to_le_bytes());
        for v in self.values { b.extend(v.to_le_bytes()); }
        b.extend([u8::from(self.enabled), u8::from(self.pre_fx)]);
        b
    }

    pub fn decode(b: &[u8]) -> Option<Self> {
        if !matches!(b.len(), 30 | 32) || &b[..4] != b"LSC1" { return None; }
        let src = u16::from_le_bytes(b[4..6].try_into().ok()?);
        let mut s = Self { source: (src != u16::MAX).then_some(src), enabled: b.get(30).copied().unwrap_or(1) != 0, pre_fx: b.get(31).copied().unwrap_or(0) != 0, ..Self::default() };
        for (k, bytes) in b[6..30].chunks_exact(4).enumerate() {
            let v = f32::from_le_bytes(bytes.try_into().ok()?);
            if !v.is_finite() { return None; }
            s.values[k] = v.clamp(Self::value(k as u8, 0.0), Self::value(k as u8, 1.0));
        }
        Some(s)
    }
}

pub struct Compressor {
    settings: Settings,
    attack: f32,
    release: f32,
    reduction: f32,
    rate: u32,
    levels: [f32; 2],
}

impl Compressor {
    /// Peaks of the channel audio immediately before and after ducking.
    pub fn levels(&self) -> [f32; 2] { self.levels }
    pub fn clear_levels(&mut self) { self.levels = [0.0; 2]; }

    pub fn reduction_db(&self) -> f32 {
        let mix = self.settings.values[5];
        -20.0 * (1.0 - mix + mix * 10f32.powf(-self.reduction / 20.0)).max(1e-12).log10()
    }

    pub fn from_state(b: &[u8], rate: u32) -> Option<Self> {
        let settings = Settings::decode(b)?;
        Some(Self::new(settings, rate))
    }

    pub fn new(settings: Settings, rate: u32) -> Self {
        let coef = |ms: f32| (-1.0 / (ms * 0.001 * rate.max(1) as f32)).exp();
        Self { attack: coef(settings.values[2]), release: coef(settings.values[3]), settings, reduction: 0.0, rate, levels: [0.0; 2] }
    }
}

impl Effect for Compressor {
    fn set_sidechain(&mut self, settings: Settings) {
        self.attack = (-1.0 / (settings.values[2] * 0.001 * self.rate.max(1) as f32)).exp();
        self.release = (-1.0 / (settings.values[3] * 0.001 * self.rate.max(1) as f32)).exp();
        self.settings = settings;
    }
    fn key_track(&self) -> Option<usize> { self.settings.source.map(usize::from) }
    fn reset(&mut self) { self.reduction = 0.0; self.clear_levels(); }
    // No trigger means no ducking, rather than compressing the destination's own signal.
    fn process(&mut self, buf: &mut [f32]) {
        self.reduction = 0.0;
        let peak = buf.iter().fold(0.0f32, |peak, x| peak.max(x.abs()));
        self.levels = [peak; 2];
    }
    fn process_keyed(&mut self, buf: &mut [f32], key: &[f32]) {
        let [threshold, ratio, _, _, knee, mix] = self.settings.values;
        for (frame, trigger) in buf.chunks_exact_mut(2).zip(key.chunks_exact(2)) {
            let peak = trigger[0].abs().max(trigger[1].abs()).max(1e-12);
            let over = 20.0 * peak.log10() - threshold;
            let slope = 1.0 - 1.0 / ratio;
            let target = if knee > 0.0 && over.abs() < knee * 0.5 {
                slope * (over + knee * 0.5).powi(2) / (2.0 * knee)
            } else { slope * over.max(0.0) };
            let c = if target > self.reduction { self.attack } else { self.release };
            self.reduction = c * self.reduction + (1.0 - c) * target;
            let gain = 1.0 - mix + mix * 10f32.powf(-self.reduction / 20.0);
            let input = frame[0].abs().max(frame[1].abs());
            self.levels[0] = self.levels[0].max(input);
            self.levels[1] = self.levels[1].max(input * gain);
            frame[0] *= gain;
            frame[1] *= gain;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keyed_ducking_and_dry_bypass() {
        let settings = Settings { source: Some(2), ..Settings::default() };
        let mut fx = Compressor::from_state(&settings.encode(), 48000).unwrap();
        let mut audio = vec![0.5; 48000];
        fx.process(&mut audio);
        assert!(audio.iter().all(|x| *x == 0.5));
        fx.clear_levels();
        fx.process_keyed(&mut audio, &vec![1.0; 48000]);
        assert_eq!(fx.levels()[0], 0.5);
        assert!(fx.levels()[1] < fx.levels()[0]);
        assert!((audio[47998] - 0.5 * 10f32.powf(-18.0 / 20.0)).abs() < 0.001);
        assert_eq!(audio[47998], audio[47999]);
        assert!((fx.reduction_db() - 18.0).abs() < 0.02);
        // Two seconds at 48 kHz, with two interleaved samples per stereo frame.
        let mut released = vec![0.5; 2 * 48000 * 2];
        let silence = vec![0.0; released.len()];
        fx.process_keyed(&mut released, &silence);
        assert!(released[released.len() - 2] > 0.499);
        let dry = Settings { values: [-24.0, 4.0, 5.0, 150.0, 6.0, 0.0], ..settings };
        let envelope = fx.reduction;
        fx.set_sidechain(dry);
        assert_eq!(fx.reduction, envelope, "live parameter edits preserve the envelope");
        fx.clear_levels();
        let before = released.clone();
        fx.process_keyed(&mut released, &vec![1.0; before.len()]);
        assert_eq!(released, before, "zero mix is an exact dry bypass");
        assert_eq!(fx.levels()[0], fx.levels()[1]);
        assert_eq!(Settings::decode(&settings.encode()), Some(settings));
        assert!(!Settings::default().pre_fx);
        assert!(!Settings::default().enabled);
        assert_eq!(fx.reduction_db(), 0.0, "dry mix has no displayed reduction");
        let mut positioned = settings;
        positioned.pre_fx = true;
        positioned.enabled = true;
        assert_eq!(Settings::decode(&positioned.encode()), Some(positioned));
        let legacy = settings.encode()[..30].to_vec();
        assert!(Settings::decode(&legacy).unwrap().enabled);
        assert!(!Settings::decode(&legacy).unwrap().pre_fx);
        assert!(Compressor::from_state(b"invalid", 48000).is_none());
    }
}
