//! The real-time half: `Engine::process` runs in the audio callback and mixes the voices;
//! the `Controller` on the host's thread sends it commands and learns which pads play.
//! The callback never allocates, locks or frees: voices live in a preallocated vector and
//! a finished voice's sound goes back to the controller to be dropped there.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rtrb::{Consumer, Producer, PushError, RingBuffer};

use crate::{RATE, Sound};

/// Voices playing at once; a new one past this takes the oldest one's place.
pub const MAX_VOICES: usize = 32;
/// Every stop, retrigger and choke fades out over this many frames (20 ms), so nothing
/// clicks.
pub const FADE_FRAMES: usize = RATE as usize / 50;
const QUEUE: usize = 256;

/// What pressing a pad does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Plays to the end; pressing again starts it over.
    #[default]
    OneShot,
    /// Pressing starts it, pressing again stops it.
    Toggle,
    /// Plays while held: a release stops it.
    Hold,
}

/// A pad as a press plays it.
#[derive(Clone, Debug)]
pub struct Pad {
    pub id: u32,
    pub sound: Arc<Sound>,
    /// Linear.
    pub gain: f32,
    pub mode: Mode,
    pub looped: bool,
    /// Pads of the same group (1..) cut each other off; 0 is none.
    pub choke: u8,
}

#[derive(Debug)]
pub enum Command {
    Press(Pad),
    Release {
        id: u32,
    },
    Stop {
        id: u32,
    },
    StopAll,
    /// The whole board's level, linear.
    Level {
        gain: f32,
    },
}

#[derive(Debug)]
enum Event {
    Playing {
        id: u32,
        on: bool,
    },
    /// A sound the callback is done with, to be dropped off the audio thread.
    Freed(Arc<Sound>),
}

struct Voice {
    id: u32,
    sound: Arc<Sound>,
    pos: usize,
    gain: f32,
    mode: Mode,
    looped: bool,
    choke: u8,
    /// Frames left of its fade-out; a fading voice is no longer the pad playing.
    fade: Option<usize>,
    /// Order of starting, to steal the oldest.
    serial: u64,
    done: bool,
}

impl Voice {
    fn live(&self) -> bool {
        self.fade.is_none() && !self.done
    }
}

/// The callback's half.
pub struct Engine {
    commands: Consumer<Command>,
    events: Producer<Event>,
    voices: Vec<Voice>,
    graveyard: Vec<Arc<Sound>>,
    serial: u64,
    level: f32,
    target_level: f32,
}

/// The host's half.
pub struct Controller {
    commands: Producer<Command>,
    events: Consumer<Event>,
    /// Pads playing and since when, as far as the last `drain` knows.
    playing: HashMap<u32, Instant>,
    /// Pads pressed whose start the engine has not reported yet.
    pressed: HashMap<u32, Instant>,
}

pub fn pair() -> (Controller, Engine) {
    let (commands_tx, commands_rx) = RingBuffer::new(QUEUE);
    let (events_tx, events_rx) = RingBuffer::new(QUEUE);
    (
        Controller {
            commands: commands_tx,
            events: events_rx,
            playing: HashMap::new(),
            pressed: HashMap::new(),
        },
        Engine {
            commands: commands_rx,
            events: events_tx,
            voices: Vec::with_capacity(MAX_VOICES),
            graveyard: Vec::with_capacity(2 * MAX_VOICES + QUEUE),
            serial: 0,
            level: 1.0,
            target_level: 1.0,
        },
    )
}

impl Controller {
    /// Queues `command`; `false` when the engine is not keeping up (the queue is full).
    pub fn send(&mut self, command: Command) -> bool {
        if let Command::Press(pad) = &command {
            let now = Instant::now();
            match (self.playing.contains_key(&pad.id), pad.mode) {
                (true, Mode::Toggle) => {}
                (true, _) => {
                    self.playing.insert(pad.id, now);
                }
                (false, _) => {
                    self.pressed.insert(pad.id, now);
                }
            }
        }
        self.commands.push(command).is_ok()
    }

    /// Takes in what the engine reported and drops the sounds it is done with; `true` when
    /// a pad started or stopped.
    pub fn drain(&mut self) -> bool {
        let mut changed = false;
        while let Ok(event) = self.events.pop() {
            match event {
                Event::Playing { id, on: true } => {
                    let since = self.pressed.remove(&id).unwrap_or_else(Instant::now);
                    self.playing.insert(id, since);
                    changed = true;
                }
                Event::Playing { id, on: false } => {
                    self.playing.remove(&id);
                    changed = true;
                }
                Event::Freed(_) => {}
            }
        }
        changed
    }

    /// How long pad `id` has been playing, if it is.
    pub fn elapsed(&self, id: u32) -> Option<Duration> {
        self.playing.get(&id).map(Instant::elapsed)
    }

    pub fn any_playing(&self) -> bool {
        !self.playing.is_empty()
    }
}

impl Engine {
    /// Mixes the next `out.len()` frames.
    pub fn process(&mut self, out: &mut [[f32; 2]]) {
        while let Ok(command) = self.commands.pop() {
            self.execute(command);
        }
        out.fill([0.0; 2]);
        for voice in &mut self.voices {
            let frames = &voice.sound.frames;
            for frame in out.iter_mut() {
                if voice.pos >= frames.len() {
                    if voice.looped && !frames.is_empty() {
                        voice.pos = 0;
                    } else {
                        voice.done = true;
                        break;
                    }
                }
                let envelope = match &mut voice.fade {
                    Some(0) => {
                        voice.done = true;
                        break;
                    }
                    Some(left) => {
                        *left -= 1;
                        *left as f32 / FADE_FRAMES as f32
                    }
                    None => 1.0,
                };
                let gain = voice.gain * envelope;
                let sample = frames[voice.pos];
                frame[0] += sample[0] * gain;
                frame[1] += sample[1] * gain;
                voice.pos += 1;
            }
        }
        let step = 1.0 / FADE_FRAMES as f32;
        for frame in out.iter_mut() {
            self.level += (self.target_level - self.level).clamp(-step, step);
            frame[0] *= self.level;
            frame[1] *= self.level;
        }
        let mut i = 0;
        while i < self.voices.len() {
            if self.voices[i].done {
                let voice = self.voices.swap_remove(i);
                let ended_live = voice.fade.is_none();
                self.bury(voice.sound);
                if ended_live && !self.is_live(voice.id) {
                    self.report(voice.id, false);
                }
            } else {
                i += 1;
            }
        }
        self.flush_graveyard();
    }

    fn execute(&mut self, command: Command) {
        match command {
            Command::Press(pad) => self.press(pad),
            Command::Release { id } => {
                if self
                    .voices
                    .iter()
                    .any(|v| v.id == id && v.live() && v.mode == Mode::Hold)
                {
                    self.fade_pad(id);
                }
            }
            Command::Stop { id } => self.fade_pad(id),
            Command::StopAll => {
                for i in 0..self.voices.len() {
                    let id = self.voices[i].id;
                    self.fade_pad(id);
                }
            }
            Command::Level { gain } => self.target_level = gain,
        }
    }

    fn press(&mut self, pad: Pad) {
        let was_live = self.is_live(pad.id);
        if was_live && pad.mode == Mode::Toggle {
            self.fade_pad(pad.id);
            self.bury(pad.sound);
            return;
        }
        for voice in &mut self.voices {
            if voice.id == pad.id && voice.live() {
                voice.fade = Some(FADE_FRAMES);
            }
        }
        if pad.choke != 0 {
            for i in 0..self.voices.len() {
                let voice = &self.voices[i];
                if voice.id != pad.id && voice.choke == pad.choke && voice.live() {
                    let id = voice.id;
                    self.fade_pad(id);
                }
            }
        }
        if self.voices.len() == MAX_VOICES {
            self.steal_oldest();
        }
        self.serial += 1;
        self.voices.push(Voice {
            id: pad.id,
            sound: pad.sound,
            pos: 0,
            gain: pad.gain,
            mode: pad.mode,
            looped: pad.looped,
            choke: pad.choke,
            fade: None,
            serial: self.serial,
            done: false,
        });
        if !was_live {
            self.report(pad.id, true);
        }
    }

    /// Fades every voice of pad `id` and says it stopped, if it was playing.
    fn fade_pad(&mut self, id: u32) {
        let mut faded = false;
        for voice in &mut self.voices {
            if voice.id == id && voice.live() {
                voice.fade = Some(FADE_FRAMES);
                faded = true;
            }
        }
        if faded {
            self.report(id, false);
        }
    }

    fn steal_oldest(&mut self) {
        let Some(oldest) = (0..self.voices.len()).min_by_key(|&i| self.voices[i].serial) else {
            return;
        };
        let voice = self.voices.swap_remove(oldest);
        let was_live = voice.live();
        self.bury(voice.sound);
        if was_live && !self.is_live(voice.id) {
            self.report(voice.id, false);
        }
    }

    fn is_live(&self, id: u32) -> bool {
        self.voices.iter().any(|v| v.id == id && v.live())
    }

    fn report(&mut self, id: u32, on: bool) {
        // A full queue loses the report; the next start or stop of that pad corrects it.
        let _ = self.events.push(Event::Playing { id, on });
    }

    fn bury(&mut self, sound: Arc<Sound>) {
        if self.graveyard.len() < self.graveyard.capacity() {
            self.graveyard.push(sound);
        } else {
            // Never reached in practice (the queue drains every cycle); leaking beats
            // freeing on the audio thread.
            std::mem::forget(sound);
        }
    }

    fn flush_graveyard(&mut self) {
        while let Some(sound) = self.graveyard.pop() {
            if let Err(PushError::Full(Event::Freed(sound))) = self.events.push(Event::Freed(sound))
            {
                self.graveyard.push(sound);
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sound(frames: usize, value: f32) -> Arc<Sound> {
        Arc::new(Sound {
            frames: vec![[value; 2]; frames],
        })
    }

    fn pad(id: u32, sound: &Arc<Sound>, mode: Mode) -> Pad {
        Pad {
            id,
            sound: Arc::clone(sound),
            gain: 1.0,
            mode,
            looped: false,
            choke: 0,
        }
    }

    fn run(engine: &mut Engine, frames: usize) -> Vec<[f32; 2]> {
        let mut out = vec![[0.0; 2]; frames];
        engine.process(&mut out);
        out
    }

    #[test]
    fn a_one_shot_plays_to_its_end_and_reports_it() {
        let (mut control, mut engine) = pair();
        let beep = sound(100, 0.5);
        control.send(Command::Press(pad(1, &beep, Mode::OneShot)));
        let out = run(&mut engine, 150);
        assert_eq!(out[0], [0.5, 0.5]);
        assert_eq!(out[99], [0.5, 0.5]);
        assert_eq!(out[100], [0.0, 0.0]);
        assert!(control.drain());
        assert!(control.elapsed(1).is_none(), "ended within the block");
        assert_eq!(
            Arc::strong_count(&beep),
            1,
            "the engine handed the sound back"
        );
    }

    #[test]
    fn toggle_stops_on_the_second_press_with_a_fade() {
        let (mut control, mut engine) = pair();
        let bed = sound(RATE as usize * 10, 0.5);
        control.send(Command::Press(pad(2, &bed, Mode::Toggle)));
        run(&mut engine, 64);
        control.drain();
        assert!(control.elapsed(2).is_some());
        control.send(Command::Press(pad(2, &bed, Mode::Toggle)));
        let out = run(&mut engine, FADE_FRAMES + 10);
        assert!(out[0][0] > 0.49 && out[0][0] <= 0.5, "fades from full");
        assert!(out[FADE_FRAMES / 2][0] < 0.3, "halfway down");
        assert_eq!(out[FADE_FRAMES + 5], [0.0, 0.0]);
        control.drain();
        assert!(control.elapsed(2).is_none());
    }

    #[test]
    fn hold_stops_on_release_and_a_one_shot_ignores_it() {
        let (mut control, mut engine) = pair();
        let long = sound(RATE as usize, 0.5);
        control.send(Command::Press(pad(3, &long, Mode::Hold)));
        control.send(Command::Press(pad(4, &long, Mode::OneShot)));
        run(&mut engine, 64);
        control.send(Command::Release { id: 3 });
        control.send(Command::Release { id: 4 });
        run(&mut engine, FADE_FRAMES + 1);
        let out = run(&mut engine, 8);
        assert_eq!(out[0], [0.5, 0.5], "only the one-shot is left");
    }

    #[test]
    fn a_retrigger_fades_the_old_voice_under_the_new_one() {
        let (mut control, mut engine) = pair();
        let long = sound(RATE as usize, 0.25);
        control.send(Command::Press(pad(5, &long, Mode::OneShot)));
        run(&mut engine, 64);
        control.send(Command::Press(pad(5, &long, Mode::OneShot)));
        let out = run(&mut engine, FADE_FRAMES + 8);
        assert!(
            out[0][0] > 0.25 && out[0][0] < 0.5,
            "both, the old one fading"
        );
        assert_eq!(out[FADE_FRAMES + 4], [0.25, 0.25], "then the new one alone");
    }

    #[test]
    fn a_choke_group_cuts_the_other_pad() {
        let (mut control, mut engine) = pair();
        let long = sound(RATE as usize, 0.25);
        let mut intro = pad(6, &long, Mode::OneShot);
        intro.choke = 1;
        let mut outro = pad(7, &long, Mode::OneShot);
        outro.choke = 1;
        control.send(Command::Press(intro));
        run(&mut engine, 64);
        control.send(Command::Press(outro));
        run(&mut engine, FADE_FRAMES + 1);
        control.drain();
        assert!(control.elapsed(6).is_none());
        assert!(control.elapsed(7).is_some());
        assert_eq!(run(&mut engine, 4)[0], [0.25, 0.25]);
    }

    #[test]
    fn a_loop_wraps_and_stop_all_ends_it() {
        let (mut control, mut engine) = pair();
        let short = sound(10, 0.5);
        let mut bed = pad(8, &short, Mode::OneShot);
        bed.looped = true;
        control.send(Command::Press(bed));
        assert_eq!(
            run(&mut engine, 35)[34],
            [0.5, 0.5],
            "past the end, still playing"
        );
        control.send(Command::StopAll);
        run(&mut engine, FADE_FRAMES + 1);
        assert_eq!(run(&mut engine, 4)[0], [0.0, 0.0]);
    }

    #[test]
    fn past_the_voice_limit_the_oldest_gives_way() {
        let (mut control, mut engine) = pair();
        let long = sound(RATE as usize, 0.01);
        for id in 0..=MAX_VOICES as u32 {
            control.send(Command::Press(pad(id, &long, Mode::OneShot)));
        }
        run(&mut engine, 4);
        control.drain();
        assert!(control.elapsed(0).is_none(), "the first one was stolen");
        assert!(control.elapsed(MAX_VOICES as u32).is_some());
    }
}
