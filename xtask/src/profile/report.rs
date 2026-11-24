use std::collections::BTreeMap;
use std::fmt::Write;

use serde_json::{Value, json};

use super::cpu::Role;
use super::traffic::Traffic;

#[derive(Debug, Clone)]
pub struct Step {
    pub label: String,
    pub keys: usize,
    pub traffic: Traffic,
    pub first_response_ms: Option<f64>,
    pub settled_ms: Option<f64>,
    pub cpu: BTreeMap<Role, u64>,
}

impl Step {
    pub fn cpu_ms(&self, role: Role) -> f64 {
        micros_to_ms(self.cpu.get(&role).copied().unwrap_or(0))
    }

    pub fn total_cpu_ms(&self) -> f64 {
        micros_to_ms(self.cpu.values().sum())
    }
}

#[derive(Debug, Clone)]
pub struct Run {
    pub setup: String,
    pub viewer: String,
    pub renderer: String,
    pub steps: Vec<Step>,
    pub error: Option<String>,
}

pub const STARTUP: &str = "startup";

pub fn renderer(traffic: &Traffic) -> String {
    if traffic.transmissions > 0 {
        let share = traffic.out_of_band_transmissions * 100 / traffic.transmissions;
        return match share {
            0 => "Kitty graphics, inline".to_owned(),
            100 => "Kitty graphics, shared memory or files".to_owned(),
            share => format!("Kitty graphics, {share}% out of band"),
        };
    }
    if traffic.sixel_frames > 0 {
        return "Sixel".to_owned();
    }
    if traffic.iterm2_images > 0 {
        return "iTerm2 images".to_owned();
    }
    "text cells".to_owned()
}

pub fn markdown(runs: &[Run], machine: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# termleaf vs PixelGUI\n");
    let _ = writeln!(
        out,
        "Machine: `{machine}`. Every step after startup sends the same keys to both viewers. Bytes are what reached the outer terminal; pixels sent count every image the terminal was handed, inline or out of band. CPU is summed over every process of each role.\n"
    );
    let _ = writeln!(out, "## At a glance (all steps after startup)\n");
    let _ = writeln!(
        out,
        "| setup | viewer | protocol | bytes to terminal | images sent | pixels sent | CPU, viewer | CPU, multiplexer | CPU, terminal | CPU, all |"
    );
    let _ = writeln!(
        out,
        "| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |"
    );
    for run in runs {
        if let Some(error) = &run.error {
            let _ = writeln!(
                out,
                "| {} | {} | failed: {} | | | | | | | |",
                run.setup,
                run.viewer,
                error.replace('|', "/").replace('\n', " ")
            );
            continue;
        }
        let after_startup: Vec<&Step> = run
            .steps
            .iter()
            .filter(|step| step.label != STARTUP)
            .collect();
        let mut traffic = Traffic::default();
        for step in &after_startup {
            traffic += step.traffic;
        }
        let cpu = |role: Role| {
            after_startup
                .iter()
                .map(|step| step.cpu_ms(role))
                .sum::<f64>()
        };
        let all: f64 = after_startup.iter().map(|step| step.total_cpu_ms()).sum();
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            run.setup,
            run.viewer,
            run.renderer,
            bytes(traffic.bytes),
            traffic.images_sent(),
            megapixels(traffic.pixels_sent()),
            ms(cpu(Role::Viewer)),
            ms(cpu(Role::Multiplexer)),
            ms(cpu(Role::Terminal)),
            ms(all),
        );
    }
    let mut setups: Vec<&str> = Vec::new();
    for run in runs {
        if !setups.contains(&run.setup.as_str()) {
            setups.push(&run.setup);
        }
    }
    for setup in setups {
        let _ = writeln!(out, "\n## {setup}\n");
        let _ = writeln!(
            out,
            "| step | viewer | keys | bytes | images | pixels | first response | settled after last key | CPU viewer | CPU multiplexer | CPU terminal | CPU display |"
        );
        let _ = writeln!(
            out,
            "| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |"
        );
        let of_setup: Vec<&Run> = runs
            .iter()
            .filter(|run| run.setup == setup && run.error.is_none())
            .collect();
        let labels: Vec<&str> = of_setup
            .first()
            .map(|run| run.steps.iter().map(|step| step.label.as_str()).collect())
            .unwrap_or_default();
        for label in labels {
            for run in &of_setup {
                let Some(step) = run.steps.iter().find(|step| step.label == label) else {
                    continue;
                };
                let _ = writeln!(
                    out,
                    "| {label} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
                    run.viewer,
                    step.keys,
                    bytes(step.traffic.bytes),
                    step.traffic.images_sent(),
                    megapixels(step.traffic.pixels_sent()),
                    optional_ms(step.first_response_ms),
                    optional_ms(step.settled_ms),
                    ms(step.cpu_ms(Role::Viewer)),
                    ms(step.cpu_ms(Role::Multiplexer)),
                    ms(step.cpu_ms(Role::Terminal)),
                    ms(step.cpu_ms(Role::Display)),
                );
            }
        }
    }
    out
}

pub fn json(runs: &[Run], machine: &str) -> Value {
    let runs: Vec<Value> = runs
        .iter()
        .map(|run| {
            let steps: Vec<Value> = run
                .steps
                .iter()
                .map(|step| {
                    let cpu: serde_json::Map<String, Value> = Role::ALL
                        .iter()
                        .map(|role| (role.label().to_owned(), json!(step.cpu_ms(*role))))
                        .collect();
                    json!({
                        "label": step.label,
                        "keys": step.keys,
                        "bytes": step.traffic.bytes,
                        "graphics_bytes": step.traffic.graphics_bytes,
                        "transmissions": step.traffic.transmissions,
                        "out_of_band_transmissions": step.traffic.out_of_band_transmissions,
                        "transmitted_pixels": step.traffic.transmitted_pixels,
                        "placements": step.traffic.placements,
                        "deletions": step.traffic.deletions,
                        "sixel_frames": step.traffic.sixel_frames,
                        "sixel_pixels": step.traffic.sixel_pixels,
                        "iterm2_images": step.traffic.iterm2_images,
                        "first_response_ms": step.first_response_ms,
                        "settled_ms": step.settled_ms,
                        "cpu_ms": cpu,
                    })
                })
                .collect();
            json!({
                "setup": run.setup,
                "viewer": run.viewer,
                "renderer": run.renderer,
                "error": run.error,
                "steps": steps,
            })
        })
        .collect();
    json!({ "machine": machine, "runs": runs })
}

fn micros_to_ms(micros: u64) -> f64 {
    let whole = u32::try_from(micros / 1000).unwrap_or(u32::MAX);
    let part = u32::try_from(micros % 1000).unwrap_or(0);
    f64::from(whole) + f64::from(part) / 1000.0
}

pub fn bytes(count: u64) -> String {
    let kib = count / 1024;
    if kib >= 1024 * 10 {
        format!("{} MiB", kib / 1024)
    } else if kib >= 10 {
        format!("{kib} KiB")
    } else {
        format!("{count} B")
    }
}

pub fn megapixels(count: u64) -> String {
    if count == 0 {
        return "0".to_owned();
    }
    let tenths = (count + 50_000) / 100_000;
    format!("{}.{} MP", tenths / 10, tenths % 10)
}

fn ms(value: f64) -> String {
    format!("{value:.0} ms")
}

fn optional_ms(value: Option<f64>) -> String {
    value.map_or_else(|| "–".to_owned(), ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_counts_read_at_a_glance() {
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(64 * 1024), "64 KiB");
        assert_eq!(bytes(48 * 1024 * 1024), "48 MiB");
    }

    #[test]
    fn pixel_counts_round_to_a_tenth_of_a_megapixel() {
        assert_eq!(megapixels(0), "0");
        assert_eq!(megapixels(1_249_999), "1.2 MP");
        assert_eq!(megapixels(30_000), "0.0 MP");
    }

    #[test]
    fn the_protocol_is_named_from_what_was_sent() {
        let kitty = Traffic {
            transmissions: 4,
            out_of_band_transmissions: 4,
            ..Traffic::default()
        };
        assert_eq!(renderer(&kitty), "Kitty graphics, shared memory or files");
        let sixel = Traffic {
            sixel_frames: 2,
            ..Traffic::default()
        };
        assert_eq!(renderer(&sixel), "Sixel");
        assert_eq!(renderer(&Traffic::default()), "text cells");
    }

    #[test]
    fn a_failed_run_is_listed_with_its_reason() {
        let run = Run {
            setup: "Ghostty + tmux".to_owned(),
            viewer: "termleaf".to_owned(),
            renderer: String::new(),
            steps: Vec::new(),
            error: Some("no image | protocol".to_owned()),
        };
        let report = markdown(&[run], "Linux");
        assert!(report.contains("| Ghostty + tmux | termleaf | failed: no image / protocol |"));
    }
}
