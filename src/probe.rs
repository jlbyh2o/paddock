//! Host and GPU telemetry.
//!
//! The GPU numbers come from NVML when the driver library is loadable, and from
//! `nvidia-smi` otherwise — a source worth keeping, because NVML is unavailable in some
//! container setups where the CLI still works. Host memory matters here more than it
//! usually would: FreeToken's offload backends keep every expert in host RAM, so
//! "is there room for the banks?" is a question the Dashboard has to be able to answer.

use std::time::Duration;

use sysinfo::{MemoryRefreshKind, RefreshKind, System};

#[derive(Debug, Clone, Default)]
pub struct Gpu {
    pub index: u32,
    pub name: String,
    pub uuid: String,
    pub memory_total: u64,
    pub memory_used: u64,
    /// Percent, 0-100.
    pub utilization: Option<u32>,
    /// Degrees Celsius.
    pub temperature: Option<u32>,
    pub power_watts: Option<f64>,
    pub power_limit_watts: Option<f64>,
    /// Current PCIe link, e.g. `gen4 x16`.
    pub pcie_link: Option<String>,
}

impl Gpu {
    pub fn memory_free(&self) -> u64 {
        self.memory_total.saturating_sub(self.memory_used)
    }
    pub fn memory_ratio(&self) -> f64 {
        crate::util::ratio(self.memory_used, self.memory_total)
    }
    /// A UUID prefix short enough for a table but still unique in practice.
    pub fn short_uuid(&self) -> String {
        self.uuid.chars().take(16).collect()
    }
}

#[derive(Debug, Clone, Default)]
pub struct Host {
    pub cpu_percent: f32,
    pub cpu_cores: usize,
    pub memory_total: u64,
    pub memory_used: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    pub load_avg: (f64, f64, f64),
    pub hostname: String,
    pub kernel: String,
    pub uptime_s: u64,
}

impl Host {
    pub fn memory_ratio(&self) -> f64 {
        crate::util::ratio(self.memory_used, self.memory_total)
    }
    pub fn memory_free(&self) -> u64 {
        self.memory_total.saturating_sub(self.memory_used)
    }
}

/// Owns the sampling handles so NVML is initialized once rather than per tick.
pub struct Probe {
    nvml: Option<nvml_wrapper::Nvml>,
    system: System,
    /// Set when NVML failed and the `nvidia-smi` fallback is in use.
    pub gpu_source: &'static str,
    pub nvml_error: Option<String>,
}

impl Probe {
    pub fn new() -> Self {
        let (nvml, nvml_error) = match nvml_wrapper::Nvml::init() {
            Ok(n) => (Some(n), None),
            Err(e) => (None, Some(e.to_string())),
        };
        let gpu_source = if nvml.is_some() { "NVML" } else { "nvidia-smi" };
        let system = System::new_with_specifics(
            RefreshKind::nothing()
                .with_memory(MemoryRefreshKind::everything())
                .with_cpu(sysinfo::CpuRefreshKind::nothing().with_cpu_usage()),
        );
        Self { nvml, system, gpu_source, nvml_error }
    }

    pub fn host(&mut self) -> Host {
        self.system.refresh_memory();
        self.system.refresh_cpu_usage();
        let cpus = self.system.cpus();
        let cpu_percent = if cpus.is_empty() {
            0.0
        } else {
            cpus.iter().map(|c| c.cpu_usage()).sum::<f32>() / cpus.len() as f32
        };
        let load = System::load_average();
        Host {
            cpu_percent,
            cpu_cores: cpus.len(),
            memory_total: self.system.total_memory(),
            memory_used: self.system.used_memory(),
            swap_total: self.system.total_swap(),
            swap_used: self.system.used_swap(),
            load_avg: (load.one, load.five, load.fifteen),
            hostname: System::host_name().unwrap_or_else(|| "localhost".into()),
            kernel: System::kernel_version().unwrap_or_default(),
            uptime_s: System::uptime(),
        }
    }

    pub fn gpus(&self) -> Vec<Gpu> {
        match &self.nvml {
            Some(nvml) => nvml_gpus(nvml),
            None => smi_gpus(),
        }
    }
}

impl Default for Probe {
    fn default() -> Self {
        Self::new()
    }
}

fn nvml_gpus(nvml: &nvml_wrapper::Nvml) -> Vec<Gpu> {
    let Ok(count) = nvml.device_count() else { return Vec::new() };
    (0..count)
        .filter_map(|i| {
            let d = nvml.device_by_index(i).ok()?;
            let mem = d.memory_info().ok();
            Some(Gpu {
                index: i,
                name: d.name().unwrap_or_else(|_| "NVIDIA GPU".into()),
                uuid: d.uuid().unwrap_or_default(),
                memory_total: mem.as_ref().map(|m| m.total).unwrap_or(0),
                memory_used: mem.as_ref().map(|m| m.used).unwrap_or(0),
                utilization: d.utilization_rates().ok().map(|u| u.gpu),
                temperature: d
                    .temperature(nvml_wrapper::enum_wrappers::device::TemperatureSensor::Gpu)
                    .ok(),
                power_watts: d.power_usage().ok().map(|mw| mw as f64 / 1000.0),
                power_limit_watts: d.enforced_power_limit().ok().map(|mw| mw as f64 / 1000.0),
                pcie_link: pcie_link(&d),
            })
        })
        .collect()
}

fn pcie_link(d: &nvml_wrapper::Device<'_>) -> Option<String> {
    let gen = d.current_pcie_link_gen().ok()?;
    let width = d.current_pcie_link_width().ok()?;
    Some(format!("gen{gen} x{width}"))
}

/// Parse `nvidia-smi --query-gpu=... --format=csv,noheader,nounits`. Used when NVML is
/// not loadable; a short timeout keeps a wedged driver from stalling the UI thread.
fn smi_gpus() -> Vec<Gpu> {
    const FIELDS: &str = "index,name,uuid,memory.total,memory.used,utilization.gpu,\
temperature.gpu,power.draw,power.limit,pcie.link.gen.current,pcie.link.width.current";
    let out = std::process::Command::new("nvidia-smi")
        .arg(format!("--query-gpu={FIELDS}"))
        .arg("--format=csv,noheader,nounits")
        .output();
    let Ok(out) = out else { return Vec::new() };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout).lines().filter_map(parse_smi_line).collect()
}

fn parse_smi_line(line: &str) -> Option<Gpu> {
    let f: Vec<&str> = line.split(',').map(str::trim).collect();
    if f.len() < 5 {
        return None;
    }
    let mib = |s: &str| -> u64 { s.parse::<u64>().unwrap_or(0) * 1024 * 1024 };
    let opt = |i: usize| -> Option<&str> {
        f.get(i).copied().filter(|v| !v.is_empty() && *v != "[N/A]" && *v != "N/A")
    };
    Some(Gpu {
        index: f[0].parse().unwrap_or(0),
        name: f[1].to_string(),
        uuid: f[2].to_string(),
        memory_total: mib(f[3]),
        memory_used: mib(f[4]),
        utilization: opt(5).and_then(|v| v.parse().ok()),
        temperature: opt(6).and_then(|v| v.parse().ok()),
        power_watts: opt(7).and_then(|v| v.parse().ok()),
        power_limit_watts: opt(8).and_then(|v| v.parse().ok()),
        pcie_link: match (opt(9), opt(10)) {
            (Some(g), Some(w)) => Some(format!("gen{g} x{w}")),
            _ => None,
        },
    })
}

/// Poll interval for hardware sampling. Slower than the UI tick: NVML queries are not
/// free, and none of these numbers move meaningfully faster than this.
pub const SAMPLE_INTERVAL: Duration = Duration::from_millis(1000);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smi_output_parses_into_a_gpu() {
        let line = "0, NVIDIA GeForce RTX 5090, GPU-9e8d7c6b-5a49-4f13-8207-c1b0a4e6d3f5, \
                    32607, 21440, 87, 61, 412.50, 575.00, 5, 16";
        let g = parse_smi_line(line).unwrap();
        assert_eq!(g.index, 0);
        assert_eq!(g.name, "NVIDIA GeForce RTX 5090");
        assert_eq!(g.memory_total, 32607 * 1024 * 1024);
        assert_eq!(g.utilization, Some(87));
        assert_eq!(g.temperature, Some(61));
        assert_eq!(g.power_watts, Some(412.5));
        assert_eq!(g.pcie_link.as_deref(), Some("gen5 x16"));
        assert_eq!(g.short_uuid(), "GPU-9e8d7c6b-5a4");
    }

    #[test]
    fn unavailable_smi_fields_become_none() {
        let line = "1, NVIDIA GeForce RTX 3060 Ti, GPU-abc, 8192, 512, [N/A], 45, [N/A], , , ";
        let g = parse_smi_line(line).unwrap();
        assert_eq!(g.utilization, None);
        assert_eq!(g.power_watts, None);
        assert_eq!(g.pcie_link, None);
        assert_eq!(g.temperature, Some(45));
        assert_eq!(g.memory_free(), (8192 - 512) * 1024 * 1024);
    }

    #[test]
    fn a_truncated_smi_line_is_rejected() {
        assert!(parse_smi_line("0, GPU, uuid").is_none());
    }
}
