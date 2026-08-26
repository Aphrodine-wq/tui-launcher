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
}

pub struct AudioFeedback {
    _stream: MixerDeviceSink,
    player: Player,
}

impl AudioFeedback {
    pub fn new() -> Option<Self> {
        let stream = DeviceSinkBuilder::open_default_sink().ok()?;
        let player = Player::connect_new(stream.mixer());
        Some(Self {
            _stream: stream,
            player,
        })
    }

    pub fn play(&self, tone: Tone, volume: f32) {
        let (frequency, duration) = match tone {
            Tone::Navigate => (520.0, 34),
            Tone::Confirm => (740.0, 72),
            Tone::Back => (360.0, 62),
            Tone::Warning => (190.0, 130),
        };
        self.player.append(
            SineWave::new(frequency)
                .take_duration(Duration::from_millis(duration))
                .fade_out(Duration::from_millis(duration / 2))
                .amplify(volume.clamp(0.0, 1.0)),
        );
    }
}
