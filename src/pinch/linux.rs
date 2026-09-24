use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::Path;
use std::thread;

use anyhow::{Result, bail};

use super::PinchInput;
use super::evdev::{RECORD_BYTES, Touches, is_touchpad, parse_record};

const INPUT_DEVICES: &str = "/dev/input";
const SYSFS_INPUT: &str = "/sys/class/input";

pub fn listen(send: impl Fn(PinchInput) + Clone + Send + 'static) -> Result<()> {
    let touchpads = touchpad_devices();
    if touchpads.is_empty() {
        bail!("--pinch found no touchpad");
    }
    let mut opened = 0;
    for name in touchpads {
        let Ok(file) = File::open(Path::new(INPUT_DEVICES).join(&name)) else {
            continue;
        };
        opened += 1;
        let send = send.clone();
        thread::spawn(move || read_touchpad(file, &send));
    }
    if opened == 0 {
        bail!(
            "--pinch cannot read the touchpad: add yourself to the `input` group \
             (sudo usermod -aG input $USER) and log in again"
        );
    }
    Ok(())
}

fn touchpad_devices() -> Vec<String> {
    let Ok(entries) = fs::read_dir(SYSFS_INPUT) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with("event"))
        .filter(|name| {
            let device = Path::new(SYSFS_INPUT).join(name).join("device");
            let read = |file: &str| fs::read_to_string(device.join(file)).unwrap_or_default();
            is_touchpad(&read("capabilities/abs"), &read("properties"))
        })
        .collect()
}

fn read_touchpad(file: File, send: &impl Fn(PinchInput)) {
    let mut reader = BufReader::new(file);
    let mut record = [0; RECORD_BYTES];
    let mut touches = Touches::default();
    while reader.read_exact(&mut record).is_ok() {
        if let Some(input) = parse_record(&record).and_then(|event| touches.feed(event)) {
            send(input);
        }
    }
}
