//! The callback's promise: pressing, retriggering, choking, stopping and ending voices
//! never allocates or frees on the audio thread.

use std::sync::Arc;

use assert_no_alloc::{AllocDisabler, assert_no_alloc};
use canalis_pads::{Command, FADE_FRAMES, MAX_VOICES, Mode, Pad, RATE, Sound, pair};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

#[test]
fn the_callback_never_allocates_or_frees() {
    let (mut control, mut engine) = pair();
    let short = Arc::new(Sound {
        frames: vec![[0.1; 2]; 300],
    });
    let long = Arc::new(Sound {
        frames: vec![[0.1; 2]; RATE as usize],
    });
    let mut out = vec![[0.0f32; 2]; 1024];
    let pad = |id: u32, sound: &Arc<Sound>, mode: Mode, choke: u8| Pad {
        id,
        sound: Arc::clone(sound),
        gain: 0.5,
        mode,
        looped: id.is_multiple_of(3),
        choke,
    };
    for round in 0..4u32 {
        for id in 0..MAX_VOICES as u32 + 4 {
            let mode = [Mode::OneShot, Mode::Toggle, Mode::Hold][(id % 3) as usize];
            let sound = if id.is_multiple_of(2) { &short } else { &long };
            control.send(Command::Press(pad(id, sound, mode, (id % 4) as u8)));
        }
        control.send(Command::Release { id: 2 });
        control.send(Command::Stop { id: 5 });
        control.send(Command::Level { gain: 0.5 });
        if round == 3 {
            control.send(Command::StopAll);
        }
        for _ in 0..(FADE_FRAMES / 256 + 4) {
            assert_no_alloc(|| engine.process(&mut out[..256]));
        }
        control.drain();
    }
    for _ in 0..8 {
        assert_no_alloc(|| engine.process(&mut out));
        control.drain();
    }
    assert!(!control.any_playing());
    assert_eq!(Arc::strong_count(&short), 1);
    assert_eq!(Arc::strong_count(&long), 1);
}
