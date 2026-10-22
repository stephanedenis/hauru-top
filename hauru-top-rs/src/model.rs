//! Types partagés entre l'échantillonnage système et le rendu terminal.

use std::collections::BTreeMap;

/// Un cœur physique : son socket, son rang dans le socket, et ses threads SMT.
#[derive(Clone, Debug)]
pub struct Core {
    pub package: usize,
    pub index: usize,
    pub threads: Vec<usize>,
}

/// Topologie détectée dans `/sys/devices/system/cpu`.
#[derive(Clone, Debug, Default)]
pub struct Topology {
    pub cores: Vec<Core>,
    pub packages: Vec<usize>,
    pub model: String,
}

impl Topology {
    pub fn cores_of(&self, package: usize) -> impl Iterator<Item = &Core> {
        self.cores
            .iter()
            .filter(move |core| core.package == package)
    }
}

/// État instantané d'un GPU NVIDIA.
#[derive(Clone, Debug, Default)]
pub struct Gpu {
    pub index: usize,
    pub name: String,
    pub util: Option<f64>,
    pub mem_used: u64,
    pub mem_total: u64,
    pub temp: Option<f64>,
    pub fan: Option<f64>,
    pub power: Option<f64>,
    pub power_limit: Option<f64>,
    pub clock: Option<f64>,
    pub mem_clock: Option<f64>,
    pub pcie_gen: Option<u32>,
    pub pcie_width: Option<u32>,
    pub pcie_gen_max: Option<u32>,
    pub pcie_width_max: Option<u32>,
    pub pcie_tx_mb_s: Option<f64>,
    pub pcie_rx_mb_s: Option<f64>,
    pub tensor: Option<f64>,
}

impl Gpu {
    pub fn mem_fraction(&self) -> f64 {
        if self.mem_total == 0 {
            0.0
        } else {
            self.mem_used as f64 / self.mem_total as f64
        }
    }

    pub fn power_fraction(&self) -> f64 {
        match (self.power, self.power_limit) {
            (Some(power), Some(limit)) if limit > 0.0 => power / limit,
            _ => 0.0,
        }
    }
}

/// État du lien NVLink entre les GPU.
#[derive(Clone, Debug, Default)]
pub struct NvLink {
    pub speed_gbps: Option<f64>,
    pub links: usize,
    pub errors: u64,
    pub throughput: Option<f64>,
}

/// Topologie QPI et disponibilité de la mesure de débit.
#[derive(Clone, Debug, Default)]
pub struct Qpi {
    pub sockets: usize,
    pub links: Option<usize>,
    pub speed_gt_s: Option<f64>,
    pub traffic: Option<f64>,
    pub reason: Option<String>,
}

/// Carte AMD pilotée par amdgpu (activité non exposée sur Cape Verde).
#[derive(Clone, Debug, Default)]
pub struct Amd {
    pub name: String,
    pub temp: Option<f64>,
    pub vram_used: u64,
    pub vram_total: u64,
    pub sclk_mhz: Option<f64>,
    pub mclk_mhz: Option<f64>,
    pub fan_pct: Option<f64>,
    pub pstate: Option<String>,
    pub pcie_gen: Option<u32>,
    pub pcie_width: Option<u32>,
    pub busy: Option<f64>,
}

/// Toutes les métriques d'une frame.
#[derive(Clone, Debug, Default)]
pub struct Metrics {
    pub cpu_pct: Vec<f64>,
    pub cpu_total: f64,
    pub socket_pct: BTreeMap<usize, f64>,
    pub socket_freq: BTreeMap<usize, Option<f64>>,
    pub socket_temp: BTreeMap<usize, Option<f64>>,
    pub freq_avg: Option<f64>,
    pub ram_used: u64,
    pub ram_total: u64,
    pub ram_pct: f64,
    pub swap_used: u64,
    pub swap_total: u64,
    pub vram_used: u64,
    pub vram_total: u64,
    pub vram_pct: f64,
    pub load: (f64, f64, f64),
    pub uptime: f64,
    pub gpus: Vec<Gpu>,
    pub gpu_error: Option<String>,
    pub nvlink: NvLink,
    pub qpi: Qpi,
    pub amd: Option<Amd>,
}

/// Historique borné à 600 échantillons (~10 min à 1 Hz).
#[derive(Clone, Debug, Default)]
pub struct History {
    pub cpu: Vec<f64>,
    pub socket: BTreeMap<usize, Vec<f64>>,
    pub ram: Vec<f64>,
    pub vram: Vec<f64>,
    pub gpu: BTreeMap<usize, Vec<f64>>,
    pub saturated: Vec<bool>,
    pub socket_saturated: BTreeMap<usize, Vec<bool>>,
}

impl History {
    pub const MAX: usize = 600;

    pub fn push(series: &mut Vec<f64>, value: f64) {
        if series.len() >= Self::MAX {
            series.remove(0);
        }
        series.push(value);
    }

    pub fn push_bool(series: &mut Vec<bool>, value: bool) {
        if series.len() >= Self::MAX {
            series.remove(0);
        }
        series.push(value);
    }

    pub fn clear(&mut self) {
        self.cpu.clear();
        self.socket.clear();
        self.ram.clear();
        self.vram.clear();
        self.gpu.clear();
        self.saturated.clear();
        self.socket_saturated.clear();
    }
}
