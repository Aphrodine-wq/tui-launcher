use std::time::Duration;

use rodio::{
    DeviceSinkBuilder, MixerDeviceSink, Player,
    source::{SineWave, Source},
};

#[derive(Clone, Copy)]
pub enum Tone {
    Navigate,
    Confirm,
    Back,
    Warning,
    Boot,
}

pub struct AudioFeedback {
    stream: MixerDeviceSink,
    player: Player,
}

impl AudioFeedback {
    pub fn new() -> Option<Self> {
        let stream = DeviceSinkBuilder::open_default_sink().ok()?;
        let player = Player::connect_new(stream.mixer());
        Some(Self { stream, player })
    }

    /// The shared output mixer, so music can play beside interface sounds.
    pub fn mixer(&self) -> &rodio::mixer::Mixer {
        self.stream.mixer()
    }

    /// Play a theme pack's audio sample for this tone, or the built-in
    /// synthesized notes when the pack has none (or fails to decode).
    pub fn play_sample(&self, tone: Tone, volume: f32, sample: Option<&[u8]>) {
        if let Some(bytes) = sample
            && let Ok(source) = rodio::Decoder::new(std::io::Cursor::new(bytes.to_vec()))
        {
            self.player.append(source.amplify(volume.clamp(0.0, 1.0)));
            return;
        }
        self.play(tone, volume);
    }

    pub fn play(&self, tone: Tone, volume: f32) {
        let notes: &[(f32, u64)] = match tone {
            Tone::Navigate => &[(520.0, 34)],
            Tone::Confirm => &[(740.0, 72)],
            Tone::Back => &[(360.0, 62)],
            Tone::Warning => &[(190.0, 130)],
            Tone::Boot => &[(523.25, 120), (784.0, 300)],
        };
        for &(frequency, duration) in notes {
            self.player.append(
                SineWave::new(frequency)
                    .take_duration(Duration::from_millis(duration))
                    .fade_out(Duration::from_millis(duration / 2))
                    .amplify(volume.clamp(0.0, 1.0)),
            );
        }
    }
}
