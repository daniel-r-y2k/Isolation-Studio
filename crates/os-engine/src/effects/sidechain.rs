// Copyright (C) 2026 ItsAlphaNeon
// SPDX-License-Identifier: GPL-3.0-or-later

//! The mixer detail panel's stereo-linked, externally keyed compressor.
//! Its state lives in a normal FX slot so project saves, track states and undo retain it.

use super::Effect;

pub const NAME: &str = "Lemon Sidechain";
pub const LABELS: [&str; 6] = ["Threshold", "Ratio", "Attack", "Release", "Knee", "Mix"];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Settings {
    pub source: Option<u16>,
    /// dB, ratio, ms, ms, dB, wet fraction.
    pub values: [f32; 6],
}

impl Default for Settings {
    fn default() -> Self {
        Self { source: None, values: [-24.0, 4.0, 5.0, 150.0, 6.0, 1.0] }
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
        b
    }

    pub fn decode(b: &[u8]) -> Option<Self> {
        if b.len() != 30 || &b[..4] != b"LSC1" { return None; }
        let src = u16::from_le_bytes(b[4..6].try_into().ok()?);
        let mut s = Self { source: (src != u16::MAX).then_some(src), ..Self::default() };
        for (k, bytes) in b[6..].chunks_exact(4).enumerate() {
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
}

impl Compressor {
    pub fn from_state(b: &[u8], rate: u32) -> Option<Self> {
        let settings = Settings::decode(b)?;
        let coef = |ms: f32| (-1.0 / (ms * 0.001 * rate.max(1) as f32)).exp();
        Some(Self { attack: coef(settings.values[2]), release: coef(settings.values[3]), settings, reduction: 0.0, rate })
    }
}

impl Effect for Compressor {
    fn set_sidechain(&mut self, settings: Settings) {
        self.attack = (-1.0 / (settings.values[2] * 0.001 * self.rate.max(1) as f32)).exp();
        self.release = (-1.0 / (settings.values[3] * 0.001 * self.rate.max(1) as f32)).exp();
        self.settings = settings;
    }
    fn key_track(&self) -> Option<usize> { self.settings.source.map(usize::from) }
    fn reset(&mut self) { self.reduction = 0.0; }
    // No trigger means no ducking, rather than compressing the destination's own signal.
    fn process(&mut self, _buf: &mut [f32]) { self.reduction = 0.0; }
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
        fx.process_keyed(&mut audio, &vec![1.0; 48000]);
        assert!((audio[47998] - 0.5 * 10f32.powf(-18.0 / 20.0)).abs() < 0.001);
        assert_eq!(audio[47998], audio[47999]);
        let mut released = vec![0.5; 96000];
        fx.process_keyed(&mut released, &vec![0.0; 96000]);
        assert!(released[95998] > 0.499);
        let dry = Settings { values: [-24.0, 4.0, 5.0, 150.0, 6.0, 0.0], ..settings };
        let envelope = fx.reduction;
        fx.set_sidechain(dry);
        assert_eq!(fx.reduction, envelope, "live parameter edits preserve the envelope");
        let before = released.clone();
        fx.process_keyed(&mut released, &vec![1.0; 96000]);
        assert_eq!(released, before, "zero mix is an exact dry bypass");
        assert_eq!(Settings::decode(&settings.encode()), Some(settings));
        assert!(Compressor::from_state(b"invalid", 48000).is_none());
    }
}
