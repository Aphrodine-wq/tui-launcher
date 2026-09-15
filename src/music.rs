//! In-launcher music playback: a queue of local tracks decoded on the
//! shared audio mixer, independent of the interface sounds. Playback
//! survives navigating away and launching other things, like a console's
//! background music.

use std::{fs::File, io::BufReader, path::PathBuf};

use rodio::{Player, mixer::Mixer};

#[derive(Clone, Debug)]
pub struct Track {
    pub id: String,
    pub title: String,
    pub path: PathBuf,
}

pub struct MusicPlayer {
    player: Player,
    queue: Vec<Track>,
    index: usize,
    active: bool,
}

impl MusicPlayer {
    pub fn new(mixer: &Mixer) -> Self {
        let player = Player::connect_new(mixer);
        player.set_volume(1.0);
        Self {
            player,
            queue: Vec::new(),
            index: 0,
            active: false,
        }
    }

    /// Replace the queue and start playing `start`. The error names the
    /// track that could not be opened or decoded.
    pub fn play(&mut self, queue: Vec<Track>, start: usize) -> Result<(), String> {
        self.queue = queue;
        self.index = start.min(self.queue.len().saturating_sub(1));
        self.active = !self.queue.is_empty();
        self.load_current()
    }

    fn load_current(&mut self) -> Result<(), String> {
        self.player.clear();
        let Some(track) = self.queue.get(self.index) else {
            self.active = false;
            return Ok(());
        };
        let file = File::open(&track.path).map_err(|error| format!("{}: {error}", track.title))?;
        let source = rodio::Decoder::new(BufReader::new(file))
            .map_err(|error| format!("{}: {error}", track.title))?;
        self.player.append(source);
        self.player.play();
        Ok(())
    }

    /// Toggle pause; returns whether playback is now paused.
    pub fn toggle_pause(&mut self) -> bool {
        if self.player.is_paused() {
            self.player.play();
            false
        } else {
            self.player.pause();
            true
        }
    }

    pub fn next(&mut self) -> Result<(), String> {
        if self.queue.is_empty() {
            return Ok(());
        }
        self.index = (self.index + 1) % self.queue.len();
        self.load_current()
    }

    pub fn previous(&mut self) -> Result<(), String> {
        if self.queue.is_empty() {
            return Ok(());
        }
        self.index = (self.index + self.queue.len() - 1) % self.queue.len();
        self.load_current()
    }

    pub fn stop(&mut self) {
        self.player.clear();
        self.queue.clear();
        self.index = 0;
        self.active = false;
    }

    /// Advance when the current track has finished. Returns the title of
    /// the track that just started, or None when nothing changed (the
    /// queue ends after its last track).
    pub fn tick(&mut self) -> Option<String> {
        if !self.active || self.player.is_paused() || !self.player.empty() {
            return None;
        }
        if self.index + 1 < self.queue.len() {
            self.index += 1;
            if self.load_current().is_ok() {
                return self.current().map(|track| track.title.clone());
            }
        }
        self.stop();
        None
    }

    pub fn current(&self) -> Option<&Track> {
        if self.active {
            self.queue.get(self.index)
        } else {
            None
        }
    }

    pub fn is_paused(&self) -> bool {
        self.player.is_paused()
    }
}
