//! Debug-only remote control, used to test playback without touching the UI.
//! DRIFT_DEV_FIFO: named pipe with commands. DRIFT_DEV_STATE: file with the latest state.

use std::{
    env, fs,
    io::{BufRead, BufReader},
    thread,
};

use futures::channel::mpsc::UnboundedSender;

use crate::engine::{EngineEvent, PlayerState};

pub fn listen(tx: UnboundedSender<EngineEvent>) {
    let Ok(path) = env::var("DRIFT_DEV_FIFO") else { return };
    thread::spawn(move || {
        loop {
            let Ok(file) = fs::File::open(&path) else { return };
            for line in BufReader::new(file).lines().map_while(Result::ok) {
                if tx.unbounded_send(EngineEvent::Dev(line)).is_err() {
                    return;
                }
            }
        }
    });
}

pub fn dump(state: &PlayerState) {
    if let (Ok(path), Ok(json)) = (env::var("DRIFT_DEV_STATE"), serde_json::to_vec_pretty(state)) {
        let _ = fs::write(path, json);
    }
}
