//! Drift Audio: listens only to Drift's WebKit audio process through a Core Audio tap
//! and measures level, spectrum and beats. Audio never leaves this process and is not saved.
//!
//! Usage: drift-audio <webkit-gpu-pid> <report.json> [seconds] [socket]

use std::{
    collections::VecDeque,
    env,
    ffi::c_void,
    fs,
    mem::{MaybeUninit, size_of},
    os::unix::net::UnixDatagram,
    path::{Path, PathBuf},
    process, ptr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use objc2::{
    class, msg_send,
    rc::Retained,
    runtime::{AnyObject, Bool},
};
use objc2_foundation::NSString;
use rtrb::{Producer, RingBuffer};
use rustfft::{Fft, FftPlanner, num_complex::Complex};
use serde_json::json;

const WINDOW: usize = 2048;
const HOP: usize = 512;
const BANDS: usize = 32;
const MIN_BEAT_GAP: f32 = 0.24;
const SILENCE: f32 = 0.004;
const FLUX_HISTORY: usize = 48;
const DEFAULT_SECONDS: u64 = 900;
/// Frame sent to Drift: magic, beat count, level, low, mid, high, bpm, then the bands.
const FRAME_MAGIC: u32 = 0x4446_4C31;
const FRAME_BYTES: usize = 8 + 5 * 4 + BANDS * 4;

// ---- Analysis ------------------------------------------------------------------------

struct Analyzer {
    rate: f32,
    fft: Arc<dyn Fft<f32>>,
    hann: Vec<f32>,
    ring: Vec<f32>,
    write: usize,
    filled: usize,
    pending: usize,
    scratch: Vec<Complex<f32>>,
    magnitudes: Vec<f32>,
    previous: Vec<f32>,
    edges: Vec<usize>,
    low_bins: (usize, usize),
    flux: VecDeque<f32>,
    clock: f32,
    last_beat: f32,
    gaps: VecDeque<f32>,
    level: f32,
    peak: f32,
    bands: [f32; BANDS],
    beats: u64,
    frames_with_sound: u64,
    updates: u64,
}

impl Analyzer {
    fn new(rate: f32) -> Self {
        let bin = |hz: f32| (hz / rate * WINDOW as f32) as usize;
        let top = 16_000f32.min(rate * 0.45);
        let mut edges = Vec::with_capacity(BANDS + 1);
        for band in 0..=BANDS {
            let edge = bin(40. * (top / 40.).powf(band as f32 / BANDS as f32));
            let floor = edges.last().map_or(1, |last: &usize| last + 1);
            edges.push(edge.max(floor).min(WINDOW / 2 - 1));
        }
        let low_bins = (bin(30.).max(1), bin(180.).max(3));
        Self {
            rate,
            fft: FftPlanner::new().plan_fft_forward(WINDOW),
            hann: (0..WINDOW).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / WINDOW as f32).cos()).collect(),
            ring: vec![0.; WINDOW],
            write: 0,
            filled: 0,
            pending: 0,
            scratch: vec![Complex::new(0., 0.); WINDOW],
            magnitudes: vec![0.; WINDOW / 2],
            previous: vec![0.; low_bins.1 - low_bins.0],
            edges,
            low_bins,
            flux: VecDeque::with_capacity(FLUX_HISTORY),
            clock: 0.,
            last_beat: -1.,
            gaps: VecDeque::with_capacity(16),
            level: 0.,
            peak: 0.,
            bands: [0.; BANDS],
            beats: 0,
            frames_with_sound: 0,
            updates: 0,
        }
    }

    fn encode(&self) -> [u8; FRAME_BYTES] {
        let mean = |range: std::ops::Range<usize>| {
            let count = range.len() as f32;
            self.bands[range].iter().sum::<f32>() / count
        };
        let mut values = vec![self.level, mean(0..8), mean(8..20), mean(20..BANDS), self.bpm().unwrap_or(0.)];
        values.extend_from_slice(&self.bands);
        let mut out = [0; FRAME_BYTES];
        out[0..4].copy_from_slice(&FRAME_MAGIC.to_le_bytes());
        out[4..8].copy_from_slice(&(self.beats as u32).to_le_bytes());
        for (slot, value) in out[8..].chunks_exact_mut(4).zip(values) {
            slot.copy_from_slice(&value.to_le_bytes());
        }
        out
    }

    fn push(&mut self, sample: f32) {
        self.ring[self.write] = sample;
        self.write = (self.write + 1) % WINDOW;
        self.filled = (self.filled + 1).min(WINDOW);
        self.pending += 1;
        if self.filled == WINDOW && self.pending >= HOP {
            self.pending = 0;
            self.analyze();
        }
    }

    fn analyze(&mut self) {
        self.updates += 1;
        self.clock += HOP as f32 / self.rate;
        let mut energy = 0.;
        for index in 0..WINDOW {
            let sample = self.ring[(self.write + index) % WINDOW];
            energy += sample * sample;
            self.peak = self.peak.max(sample.abs());
            self.scratch[index] = Complex::new(sample * self.hann[index], 0.);
        }
        self.level = (energy / WINDOW as f32).sqrt();
        if self.level > SILENCE {
            self.frames_with_sound += 1;
        }
        self.fft.process(&mut self.scratch);
        for (magnitude, value) in self.magnitudes.iter_mut().zip(&self.scratch) {
            *magnitude = value.norm() * 4. / WINDOW as f32;
        }
        for band in 0..BANDS {
            let slice = &self.magnitudes[self.edges[band]..self.edges[band + 1].max(self.edges[band] + 1)];
            let mean = slice.iter().sum::<f32>() / slice.len() as f32;
            self.bands[band] = ((20. * (mean + 1e-6).log10() + 70.) / 70.).clamp(0., 1.);
        }
        self.detect_beat();
    }

    fn detect_beat(&mut self) {
        let (start, end) = self.low_bins;
        let mut flux = 0.;
        for (offset, slot) in self.previous.iter_mut().enumerate() {
            let value = (1. + 200. * self.magnitudes[(start + offset).min(end)]).ln();
            flux += (value - *slot).max(0.);
            *slot = value;
        }
        if self.flux.len() >= 20 {
            let count = self.flux.len() as f32;
            let mean = self.flux.iter().sum::<f32>() / count;
            let variance = self.flux.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / count;
            let is_onset = flux > mean + 1.5 * variance.sqrt() && flux > 0.2;
            let since = self.clock - self.last_beat;
            if is_onset && self.level > SILENCE && since >= MIN_BEAT_GAP {
                if self.last_beat >= 0. && since < 2. {
                    if self.gaps.len() == 16 {
                        self.gaps.pop_front();
                    }
                    self.gaps.push_back(since);
                }
                self.last_beat = self.clock;
                self.beats += 1;
            }
        }
        if self.flux.len() == FLUX_HISTORY {
            self.flux.pop_front();
        }
        self.flux.push_back(flux);
    }

    fn bpm(&self) -> Option<f32> {
        if self.gaps.len() < 4 {
            return None;
        }
        let mut sorted: Vec<f32> = self.gaps.iter().copied().collect();
        sorted.sort_by(f32::total_cmp);
        let mut bpm = 60. / sorted[sorted.len() / 2];
        while bpm < 70. {
            bpm *= 2.;
        }
        while bpm > 180. {
            bpm /= 2.;
        }
        Some(bpm)
    }
}

// ---- Core Audio ----------------------------------------------------------------------

const fn fourcc(code: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*code)
}

const SYSTEM_OBJECT: u32 = 1;
const GLOBAL: u32 = fourcc(b"glob");
const TRANSLATE_PID: u32 = fourcc(b"id2p");
const DEFAULT_OUTPUT: u32 = fourcc(b"dOut");
const DEVICE_UID: u32 = fourcc(b"uid ");
const TAP_FORMAT: u32 = fourcc(b"tfmt");
const LINEAR_PCM: u32 = fourcc(b"lpcm");
const FLAG_FLOAT: u32 = 1;

#[repr(C)]
struct Address {
    selector: u32,
    scope: u32,
    element: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Format {
    rate: f64,
    id: u32,
    flags: u32,
    bytes_per_packet: u32,
    frames_per_packet: u32,
    bytes_per_frame: u32,
    channels: u32,
    bits: u32,
    reserved: u32,
}

#[repr(C)]
struct Buffer {
    channels: u32,
    bytes: u32,
    data: *mut c_void,
}

#[repr(C)]
struct BufferList {
    count: u32,
    buffers: [Buffer; 1],
}

type IoProc = unsafe extern "C" fn(
    u32,
    *const c_void,
    *const BufferList,
    *const c_void,
    *mut BufferList,
    *const c_void,
    *mut c_void,
) -> i32;

#[link(name = "CoreAudio", kind = "framework")]
unsafe extern "C" {
    fn AudioObjectGetPropertyData(
        object: u32,
        address: *const Address,
        qualifier_size: u32,
        qualifier: *const c_void,
        size: *mut u32,
        data: *mut c_void,
    ) -> i32;
    fn AudioHardwareCreateProcessTap(description: *mut AnyObject, tap: *mut u32) -> i32;
    fn AudioHardwareDestroyProcessTap(tap: u32) -> i32;
    fn AudioHardwareCreateAggregateDevice(description: *const c_void, device: *mut u32) -> i32;
    fn AudioHardwareDestroyAggregateDevice(device: u32) -> i32;
    fn AudioDeviceCreateIOProcID(device: u32, callback: IoProc, client: *mut c_void, id: *mut *mut c_void) -> i32;
    fn AudioDeviceDestroyIOProcID(device: u32, id: *mut c_void) -> i32;
    fn AudioDeviceStart(device: u32, id: *mut c_void) -> i32;
    fn AudioDeviceStop(device: u32, id: *mut c_void) -> i32;
}

unsafe extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
}

static CALLBACKS: AtomicU64 = AtomicU64::new(0);

unsafe fn property<T: Copy>(object: u32, selector: u32, qualifier: Option<&i32>) -> Result<T, String> {
    let address = Address { selector, scope: GLOBAL, element: 0 };
    let mut size = size_of::<T>() as u32;
    let mut value = MaybeUninit::<T>::uninit();
    let (qualifier_size, qualifier) = match qualifier {
        Some(value) => (size_of::<i32>() as u32, ptr::from_ref(value).cast::<c_void>()),
        None => (0, ptr::null()),
    };
    let status = unsafe {
        AudioObjectGetPropertyData(object, &address, qualifier_size, qualifier, &mut size, value.as_mut_ptr().cast())
    };
    if status != 0 {
        return Err(format!("property {selector:#x}: status {status}"));
    }
    Ok(unsafe { value.assume_init() })
}

fn string(value: &str) -> *mut AnyObject {
    Retained::into_raw(NSString::from_str(value)).cast()
}

unsafe fn number(value: i32) -> *mut AnyObject {
    unsafe {
        let object: *mut AnyObject = msg_send![class!(NSNumber), alloc];
        msg_send![object, initWithInt: value]
    }
}

unsafe fn array(items: &[*mut AnyObject]) -> *mut AnyObject {
    unsafe {
        let list: *mut AnyObject = msg_send![class!(NSMutableArray), new];
        for item in items {
            let _: () = msg_send![list, addObject: *item];
        }
        list
    }
}

unsafe fn dictionary(entries: &[(&str, *mut AnyObject)]) -> *mut AnyObject {
    unsafe {
        let map: *mut AnyObject = msg_send![class!(NSMutableDictionary), new];
        for (key, value) in entries {
            let _: () = msg_send![map, setObject: *value, forKey: string(key)];
        }
        map
    }
}

unsafe extern "C" fn on_audio(
    _device: u32,
    _now: *const c_void,
    input: *const BufferList,
    _input_time: *const c_void,
    _output: *mut BufferList,
    _output_time: *const c_void,
    client: *mut c_void,
) -> i32 {
    CALLBACKS.fetch_add(1, Ordering::Relaxed);
    let (producer, list) = unsafe { (&mut *client.cast::<Producer<f32>>(), &*input) };
    if list.count == 0 {
        return 0;
    }
    let buffer = &list.buffers[0];
    if buffer.data.is_null() {
        return 0;
    }
    let channels = buffer.channels.max(1) as usize;
    let samples = unsafe { std::slice::from_raw_parts(buffer.data.cast::<f32>(), buffer.bytes as usize / 4) };
    for frame in samples.chunks_exact(channels) {
        let _ = producer.push(frame.iter().sum::<f32>() / channels as f32);
    }
    0
}

fn write_report(path: &Path, value: &serde_json::Value) {
    let temporary = path.with_extension("tmp");
    if fs::write(&temporary, value.to_string()).is_ok() {
        let _ = fs::rename(temporary, path);
    }
}

fn run(pid: i32, report: &Path, seconds: u64, socket: Option<&Path>) -> Result<(), String> {
    let sender = UnixDatagram::unbound().map_err(|error| error.to_string())?;
    sender.set_nonblocking(true).map_err(|error| error.to_string())?;
    unsafe {
        let process: u32 = property(SYSTEM_OBJECT, TRANSLATE_PID, Some(&pid))?;
        if process == 0 {
            return Err(format!("process {pid} has no audio object"));
        }
        let description: *mut AnyObject = msg_send![class!(CATapDescription), alloc];
        let description: *mut AnyObject =
            msg_send![description, initStereoMixdownOfProcesses: array(&[number(process as i32)])];
        let _: () = msg_send![description, setName: string("Drift Live")];
        let _: () = msg_send![description, setPrivate: Bool::YES];
        let _: () = msg_send![description, setMuteBehavior: 0isize];
        let uuid: *mut AnyObject = msg_send![description, UUID];
        let tap_uid: *mut AnyObject = msg_send![uuid, UUIDString];

        let mut tap = 0;
        let status = AudioHardwareCreateProcessTap(description, &mut tap);
        if status != 0 {
            return Err(format!("create tap: status {status}"));
        }
        let format: Format = property(tap, TAP_FORMAT, None)?;
        if format.id != LINEAR_PCM || format.bits != 32 || format.flags & FLAG_FLOAT == 0 {
            AudioHardwareDestroyProcessTap(tap);
            return Err(format!("unexpected tap format: {} bits, flags {}", format.bits, format.flags));
        }

        let output: u32 = property(SYSTEM_OBJECT, DEFAULT_OUTPUT, None)?;
        let output_uid: *mut AnyObject = property(output, DEVICE_UID, None)?;
        let aggregate_uuid: *mut AnyObject = msg_send![class!(NSUUID), UUID];
        let aggregate_uid: *mut AnyObject = msg_send![aggregate_uuid, UUIDString];
        let aggregate = dictionary(&[
            ("uid", aggregate_uid),
            ("name", string("Drift Live")),
            ("private", number(1)),
            ("stacked", number(0)),
            ("tapautostart", number(1)),
            ("master", output_uid),
            ("subdevices", array(&[dictionary(&[("uid", output_uid)])])),
            ("taps", array(&[dictionary(&[("uid", tap_uid), ("drift", number(1))])])),
        ]);
        let mut device = 0;
        let status = AudioHardwareCreateAggregateDevice(aggregate.cast(), &mut device);
        if status != 0 {
            AudioHardwareDestroyProcessTap(tap);
            return Err(format!("create aggregate device: status {status}"));
        }

        let (producer, mut consumer) = RingBuffer::<f32>::new(1 << 17);
        let client = Box::into_raw(Box::new(producer));
        let mut io_proc = ptr::null_mut();
        let status = AudioDeviceCreateIOProcID(device, on_audio, client.cast(), &mut io_proc);
        if status != 0 {
            AudioHardwareDestroyAggregateDevice(device);
            AudioHardwareDestroyProcessTap(tap);
            return Err(format!("create io proc: status {status}"));
        }
        let status = AudioDeviceStart(device, io_proc);
        if status != 0 {
            AudioDeviceDestroyIOProcID(device, io_proc);
            AudioHardwareDestroyAggregateDevice(device);
            AudioHardwareDestroyProcessTap(tap);
            return Err(format!("start device: status {status}"));
        }

        let started = Instant::now();
        let mut last_report = Instant::now() - Duration::from_secs(1);
        let mut analyzer = Analyzer::new(format.rate as f32);
        let mut total_frames = 0u64;
        let mut sent_update = 0;
        while started.elapsed().as_secs() < seconds && kill(pid, 0) == 0 {
            while let Ok(sample) = consumer.pop() {
                analyzer.push(sample);
                total_frames += 1;
            }
            if let Some(path) = socket
                && analyzer.updates != sent_update
            {
                sent_update = analyzer.updates;
                let _ = sender.send_to(&analyzer.encode(), path);
            }
            if last_report.elapsed() >= Duration::from_millis(500) {
                last_report = Instant::now();
                write_report(
                    report,
                    &json!({
                        "running": true,
                        "seconds": started.elapsed().as_secs(),
                        "rate": format.rate,
                        "channels": format.channels,
                        "callbacks": CALLBACKS.load(Ordering::Relaxed),
                        "frames": total_frames,
                        "framesWithSound": analyzer.frames_with_sound,
                        "level": analyzer.level,
                        "peak": analyzer.peak,
                        "beats": analyzer.beats,
                        "bpm": analyzer.bpm(),
                        "bands": analyzer.bands.iter().map(|v| (v * 100.).round() / 100.).collect::<Vec<_>>(),
                    }),
                );
            }
            thread::sleep(Duration::from_millis(8));
        }
        AudioDeviceStop(device, io_proc);
        AudioDeviceDestroyIOProcID(device, io_proc);
        AudioHardwareDestroyAggregateDevice(device);
        AudioHardwareDestroyProcessTap(tap);
    }
    Ok(())
}

fn main() {
    let mut args = env::args().skip(1);
    let (Some(pid), Some(report)) = (args.next().and_then(|v| v.parse::<i32>().ok()), args.next().map(PathBuf::from))
    else {
        eprintln!("usage: drift-audio <webkit-gpu-pid> <report.json> [seconds] [socket]");
        process::exit(2);
    };
    let seconds = args.next().and_then(|v| v.parse().ok()).unwrap_or(DEFAULT_SECONDS);
    let socket = args.next().map(PathBuf::from);
    if let Err(error) = run(pid, &report, seconds, socket.as_deref()) {
        write_report(&report, &json!({ "running": false, "error": error }));
        process::exit(1);
    }
    write_report(&report, &json!({ "running": false, "finished": true }));
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: f32 = 48_000.;

    #[test]
    fn finds_tempo_of_a_kick_pattern() {
        let mut analyzer = Analyzer::new(RATE);
        for index in 0..(RATE as usize * 12) {
            let time = index as f32 / RATE;
            let since_kick = time % 0.5;
            let kick = (std::f32::consts::TAU * 70. * since_kick).sin() * (-since_kick / 0.04).exp() * 0.8;
            analyzer.push(kick);
        }
        let bpm = analyzer.bpm().expect("tempo");
        assert!((bpm - 120.).abs() < 4., "bpm {bpm}");
        assert!(analyzer.beats >= 20, "beats {}", analyzer.beats);
    }

    #[test]
    fn encodes_a_fixed_size_frame() {
        let mut analyzer = Analyzer::new(RATE);
        analyzer.beats = 7;
        analyzer.level = 0.5;
        analyzer.bands[0] = 0.25;
        let frame = analyzer.encode();
        assert_eq!(u32::from_le_bytes(frame[0..4].try_into().unwrap()), FRAME_MAGIC);
        assert_eq!(u32::from_le_bytes(frame[4..8].try_into().unwrap()), 7);
        assert_eq!(f32::from_le_bytes(frame[8..12].try_into().unwrap()), 0.5);
        assert_eq!(f32::from_le_bytes(frame[28..32].try_into().unwrap()), 0.25);
    }

    #[test]
    fn silence_has_no_beats() {
        let mut analyzer = Analyzer::new(RATE);
        for _ in 0..(RATE as usize * 3) {
            analyzer.push(0.);
        }
        assert_eq!(analyzer.beats, 0);
        assert_eq!(analyzer.frames_with_sound, 0);
    }

    #[test]
    fn tone_lands_in_the_right_band() {
        let mut analyzer = Analyzer::new(RATE);
        for index in 0..WINDOW * 3 {
            analyzer.push((std::f32::consts::TAU * 440. * index as f32 / RATE).sin() * 0.5);
        }
        let loudest = (0..BANDS).max_by(|a, b| analyzer.bands[*a].total_cmp(&analyzer.bands[*b])).unwrap();
        let bin = (440. / RATE * WINDOW as f32) as usize;
        assert!(analyzer.edges[loudest] <= bin + 1 && bin <= analyzer.edges[loudest + 1] + 1);
    }
}
