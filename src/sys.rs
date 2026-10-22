//! Lecture de `/proc`, `/sys` et de `nvidia-smi`, puis agrégation des métriques.

use crate::model::{
    Amd, Core, Gpu, History, Metrics, NvLink, Process, ProcessTable, Qpi, Topology,
};
use libloading::Library;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::c_void;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Seuil d'alerte : un thread est « à 100 % » dès que l'affichage arrondi le dit.
pub const SATURATED_THRESHOLD: f64 = 99.5;

fn read_trim(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
}

/// Analyse « 0,20 » ou « 0-3,8 » comme dans sysfs.
pub fn parse_cpulist(value: &str) -> Vec<usize> {
    let mut result = Vec::new();
    for part in value.trim().split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if let Some((first, last)) = part.split_once('-') {
            if let (Ok(first), Ok(last)) =
                (first.trim().parse::<usize>(), last.trim().parse::<usize>())
            {
                result.extend(first..=last);
            }
        } else if let Ok(cpu) = part.parse() {
            result.push(cpu);
        }
    }
    result
}

pub fn cpu_model() -> String {
    let Ok(content) = fs::read_to_string("/proc/cpuinfo") else {
        return "CPU".to_string();
    };
    let Some(line) = content.lines().find(|line| line.starts_with("model name")) else {
        return "CPU".to_string();
    };
    let name = line.split(':').nth(1).unwrap_or("").trim();
    let name = name.split(" @").next().unwrap_or(name);
    let name = name
        .replace("Intel(R) ", "")
        .replace("CPU ", "")
        .replace("(R)", "")
        .replace("(TM)", "");
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        "CPU".to_string()
    } else {
        name
    }
}

/// Détecte sockets, cœurs physiques et frères SMT depuis sysfs.
pub fn detect_topology() -> Topology {
    let mut found: BTreeMap<(usize, usize), Vec<usize>> = BTreeMap::new();
    if let Ok(entries) = fs::read_dir("/sys/devices/system/cpu") {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with("cpu") || !name[3..].chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            let topology = entry.path().join("topology");
            let package = read_trim(&topology.join("physical_package_id"));
            let core_id = read_trim(&topology.join("core_id"));
            let siblings = read_trim(&topology.join("thread_siblings_list"));
            let (Some(package), Some(core_id), Some(siblings)) = (package, core_id, siblings)
            else {
                continue;
            };
            let (Ok(package), Ok(core_id)) = (package.parse::<usize>(), core_id.parse::<usize>())
            else {
                continue;
            };
            let mut threads = parse_cpulist(&siblings);
            threads.sort_unstable();
            threads.dedup();
            if !threads.is_empty() {
                found.insert((package, core_id), threads);
            }
        }
    }

    if found.is_empty() {
        // Repli : deux threads par cœur, un seul socket.
        let count = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let mut cores = Vec::new();
        if count >= 2 {
            for index in 0..count / 2 {
                cores.push(Core {
                    package: 0,
                    index,
                    threads: vec![index, index + count / 2],
                });
            }
        } else {
            cores.push(Core {
                package: 0,
                index: 0,
                threads: vec![0],
            });
        }
        return Topology {
            cores,
            packages: vec![0],
            model: cpu_model(),
        };
    }

    let packages: Vec<usize> = found
        .keys()
        .map(|(package, _)| *package)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let mut cores = Vec::new();
    for &package in &packages {
        let mut entries: Vec<Vec<usize>> = found
            .iter()
            .filter(|((candidate, _), _)| *candidate == package)
            .map(|(_, threads)| threads.clone())
            .collect();
        entries.sort_by_key(|threads| threads.iter().copied().min().unwrap_or(usize::MAX));
        for (index, threads) in entries.into_iter().enumerate() {
            cores.push(Core {
                package,
                index,
                threads,
            });
        }
    }
    Topology {
        cores,
        packages,
        model: cpu_model(),
    }
}

/// Temps CPU par thread depuis `/proc/stat` (index = numéro de CPU).
pub fn read_cpu_times() -> Vec<Vec<u64>> {
    let mut times = Vec::new();
    let Ok(content) = fs::read_to_string("/proc/stat") else {
        return times;
    };
    for line in content.lines() {
        let Some(rest) = line.strip_prefix("cpu") else {
            continue;
        };
        if rest.starts_with(' ') {
            continue;
        }
        let mut fields = rest.split_whitespace();
        let Some(first) = fields.next() else {
            continue;
        };
        if first.is_empty() || !first.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let values: Vec<u64> = fields.filter_map(|value| value.parse().ok()).collect();
        if !values.is_empty() {
            times.push(values);
        }
    }
    times
}

pub fn read_meminfo() -> BTreeMap<String, u64> {
    let mut values = BTreeMap::new();
    let Ok(content) = fs::read_to_string("/proc/meminfo") else {
        return values;
    };
    for line in content.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let Some(value) = rest.split_whitespace().next() else {
            continue;
        };
        if let Ok(kilobytes) = value.parse::<u64>() {
            values.insert(key.to_string(), kilobytes * 1024);
        }
    }
    values
}

pub fn read_uptime() -> f64 {
    fs::read_to_string("/proc/uptime")
        .ok()
        .and_then(|content| {
            content
                .split_whitespace()
                .next()
                .and_then(|value| value.parse().ok())
        })
        .unwrap_or(0.0)
}

fn read_load() -> (f64, f64, f64) {
    fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|content| {
            let mut fields = content.split_whitespace();
            Some((
                fields.next()?.parse().ok()?,
                fields.next()?.parse().ok()?,
                fields.next()?.parse().ok()?,
            ))
        })
        .unwrap_or((0.0, 0.0, 0.0))
}

fn read_frequencies(cpu_count: usize) -> Vec<Option<f64>> {
    (0..cpu_count)
        .map(|cpu| {
            fs::read_to_string(format!(
                "/sys/devices/system/cpu/cpu{cpu}/cpufreq/scaling_cur_freq"
            ))
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .map(|khz| khz as f64 / 1_000_000.0)
        })
        .collect()
}

/// Températures « Package id N » des sockets, via hwmon/coretemp.
pub struct PackageTempReader {
    paths: BTreeMap<usize, PathBuf>,
}

impl PackageTempReader {
    pub fn new() -> Self {
        let mut reader = Self {
            paths: BTreeMap::new(),
        };
        reader.scan();
        reader
    }

    fn scan(&mut self) {
        self.paths.clear();
        let Ok(hwmons) = fs::read_dir("/sys/class/hwmon") else {
            return;
        };
        for hwmon in hwmons.flatten() {
            let dir = hwmon.path();
            if read_trim(&dir.join("name")).as_deref() != Some("coretemp") {
                continue;
            }
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !name.starts_with("temp") || !name.ends_with("_label") {
                    continue;
                }
                let Some(label) = read_trim(&entry.path()) else {
                    continue;
                };
                let Some(rest) = label.strip_prefix("Package id ") else {
                    continue;
                };
                if let Ok(package) = rest.trim().parse::<usize>() {
                    let input = dir.join(name.replace("_label", "_input"));
                    self.paths.insert(package, input);
                }
            }
        }
    }

    pub fn read(&mut self) -> BTreeMap<usize, f64> {
        let mut values = BTreeMap::new();
        let mut stale = false;
        for (package, path) in &self.paths {
            match read_trim(path).and_then(|value| value.parse::<f64>().ok()) {
                Some(value) => {
                    values.insert(*package, value / 1000.0);
                }
                None => stale = true,
            }
        }
        if stale || self.paths.is_empty() {
            self.scan();
        }
        values
    }
}

fn sanitize(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .collect()
}

fn split_csv(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for character in line.chars() {
        match character {
            '"' => quoted = !quoted,
            ',' if !quoted => {
                fields.push(current.trim().to_string());
                current.clear();
            }
            _ => current.push(character),
        }
    }
    fields.push(current.trim().to_string());
    fields
}

fn parse_number(value: &str) -> Option<f64> {
    let value = value.trim();
    if value.is_empty()
        || value.starts_with('[')
        || value.eq_ignore_ascii_case("n/a")
        || value == "-"
    {
        None
    } else {
        value.parse().ok()
    }
}

const GPU_FIELDS: &str = "index,name,utilization.gpu,memory.used,memory.total,\
temperature.gpu,fan.speed,power.draw,power.limit,clocks.current.graphics,\
clocks.current.memory,pcie.link.gen.current,pcie.link.width.current,\
pcie.link.gen.max,pcie.link.width.max";

/// Exécute une commande avec un délai maximal, stdout/stderr capturés.
fn run_with_timeout(
    command: &mut Command,
    limit: Duration,
) -> Result<std::process::Output, String> {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                return Err(error.to_string());
            }
        }
        if started.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            return Err("délai dépassé".to_string());
        }
        thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().map_err(|error| error.to_string())
}

struct GpuProcess {
    pid: u32,
    name: String,
    mem_bytes: u64,
}

fn clock_ticks() -> f64 {
    let value = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if value > 0 {
        value as f64
    } else {
        100.0
    }
}

fn memory_page_size() -> u64 {
    let value = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if value > 0 {
        value as u64
    } else {
        4096
    }
}

fn read_user_map() -> HashMap<u32, String> {
    let mut users = HashMap::new();
    if let Ok(content) = fs::read_to_string("/etc/passwd") {
        for line in content.lines() {
            let fields: Vec<&str> = line.split(':').collect();
            if fields.len() >= 3 {
                if let Ok(uid) = fields[2].parse::<u32>() {
                    users.insert(uid, fields[0].to_string());
                }
            }
        }
    }
    users
}

fn read_proc_ticks(path: &Path) -> Option<(u64, String)> {
    let stat = fs::read_to_string(path.join("stat")).ok()?;
    let close = stat.rfind(')')?;
    let open = stat.find('(')?;
    let comm = stat.get(open + 1..close)?.to_string();
    let fields: Vec<&str> = stat.get(close + 1..)?.split_whitespace().collect();
    let utime = fields.get(11)?.parse::<u64>().ok()?;
    let stime = fields.get(12)?.parse::<u64>().ok()?;
    Some((utime.saturating_add(stime), comm))
}

fn read_vm_rss_bytes(path: &Path, page_size: u64) -> u64 {
    let Ok(statm) = fs::read_to_string(path.join("statm")) else {
        return 0;
    };
    statm
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0)
        .saturating_mul(page_size)
}

fn clean_process_text(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    let joined = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    joined.chars().take(180).collect()
}

fn private_command(command: &str) -> String {
    command
        .split_whitespace()
        .next()
        .and_then(|first| {
            Path::new(first)
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
                .or_else(|| Some(first.to_string()))
        })
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "processus".to_string())
}

fn read_process_command(path: &Path, comm: &str, privacy: bool) -> String {
    let raw = fs::read(path.join("cmdline")).unwrap_or_default();
    let args: Vec<String> = raw
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect();
    if args.is_empty() {
        clean_process_text(comm)
    } else if privacy {
        clean_process_text(&private_command(&args[0]))
    } else {
        clean_process_text(&args.join(" "))
    }
}

fn read_gpu_processes() -> Vec<GpuProcess> {
    let mut command = Command::new("nvidia-smi");
    command.args([
        "--query-compute-apps=pid,process_name,used_memory",
        "--format=csv,noheader,nounits",
    ]);
    let Ok(output) = run_with_timeout(&mut command, Duration::from_secs(2)) else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let mut processes = Vec::new();
    for line in text.lines() {
        let fields: Vec<&str> = line.split(',').map(str::trim).collect();
        if fields.len() < 3 {
            continue;
        }
        let Ok(pid) = fields[0].parse::<u32>() else {
            continue;
        };
        let mem_mib = fields[2].parse::<f64>().unwrap_or(0.0).max(0.0);
        if mem_mib <= 0.0 {
            continue;
        }
        processes.push(GpuProcess {
            pid,
            name: clean_process_text(fields[1]),
            mem_bytes: (mem_mib * 1024.0 * 1024.0) as u64,
        });
    }
    processes.sort_by_key(|process| std::cmp::Reverse(process.mem_bytes));
    processes
}

type NvmlGetHandle = unsafe extern "C" fn(u32, *mut *mut c_void) -> i32;
type NvmlGetPcie = unsafe extern "C" fn(*mut c_void, u32, *mut u32) -> i32;

/// Accès minimal à NVML pour le débit PCIe ; None si la bibliothèque manque.
pub struct Nvml {
    _lib: Library,
    get_handle: NvmlGetHandle,
    get_pcie: NvmlGetPcie,
    handles: BTreeMap<usize, *mut c_void>,
}

impl Nvml {
    pub fn new() -> Option<Self> {
        unsafe {
            let lib = Library::new("libnvidia-ml.so.1").ok()?;
            let init: unsafe extern "C" fn() -> i32 = *lib.get(b"nvmlInit_v2\0").ok()?;
            if init() != 0 {
                return None;
            }
            let get_handle: NvmlGetHandle = *lib.get(b"nvmlDeviceGetHandleByIndex_v2\0").ok()?;
            let get_pcie: NvmlGetPcie = *lib.get(b"nvmlDeviceGetPcieThroughput\0").ok()?;
            Some(Self {
                _lib: lib,
                get_handle,
                get_pcie,
                handles: BTreeMap::new(),
            })
        }
    }

    /// Débit PCIe (TX, RX) en Mo/s.
    pub fn pcie_mb_s(&mut self, index: usize) -> (Option<f64>, Option<f64>) {
        let handle = match self.handles.get(&index).copied() {
            Some(handle) => handle,
            None => {
                let mut handle: *mut c_void = std::ptr::null_mut();
                let result = unsafe { (self.get_handle)(index as u32, &mut handle) };
                if result != 0 {
                    return (None, None);
                }
                self.handles.insert(index, handle);
                handle
            }
        };
        let mut values = [None, None];
        for counter in 0..2u32 {
            let mut value: u32 = 0;
            let result = unsafe { (self.get_pcie)(handle, counter, &mut value) };
            values[counter as usize] = if result == 0 {
                Some(value as f64 / 1000.0) // KB/s -> Mo/s
            } else {
                None
            };
        }
        (values[0], values[1])
    }
}

/// Interroge nvidia-smi et NVML (débit PCIe).
pub fn read_gpus(nvml: &mut Option<Nvml>) -> (Vec<Gpu>, Option<String>, f64) {
    let started = Instant::now();
    let mut command = Command::new("nvidia-smi");
    command
        .arg(format!("--query-gpu={GPU_FIELDS}"))
        .arg("--format=csv,noheader,nounits");
    let output = match run_with_timeout(&mut command, Duration::from_millis(2500)) {
        Ok(output) => output,
        Err(error) => {
            let latency = started.elapsed().as_secs_f64() * 1000.0;
            return (Vec::new(), Some(format!("nvidia-smi : {error}")), latency);
        }
    };
    let latency = started.elapsed().as_secs_f64() * 1000.0;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = stderr.lines().next().unwrap_or("").trim().to_string();
        let message = if message.is_empty() {
            format!("nvidia-smi : code {}", output.status.code().unwrap_or(-1))
        } else {
            message
        };
        return (Vec::new(), Some(message), latency);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut gpus = Vec::new();
    for line in stdout.lines() {
        let fields = split_csv(line);
        if fields.len() < 15 {
            continue;
        }
        let Ok(index) = fields[0].parse::<usize>() else {
            continue;
        };
        let mem_used = parse_number(&fields[3]).unwrap_or(0.0);
        let mem_total = parse_number(&fields[4]).unwrap_or(0.0);
        let pcie_gen = parse_number(&fields[11]).map(|value| value as u32);
        let pcie_width = parse_number(&fields[12]).map(|value| value as u32);
        let pcie_gen_max = parse_number(&fields[13]).map(|value| value as u32);
        let pcie_width_max = parse_number(&fields[14]).map(|value| value as u32);
        let (pcie_tx_mb_s, pcie_rx_mb_s) = match nvml.as_mut() {
            Some(nvml) => nvml.pcie_mb_s(index),
            None => (None, None),
        };
        gpus.push(Gpu {
            index,
            name: sanitize(&fields[1]),
            util: parse_number(&fields[2]),
            mem_used: (mem_used * 1024.0 * 1024.0) as u64,
            mem_total: (mem_total * 1024.0 * 1024.0) as u64,
            temp: parse_number(&fields[5]),
            fan: parse_number(&fields[6]),
            power: parse_number(&fields[7]),
            power_limit: parse_number(&fields[8]),
            clock: parse_number(&fields[9]),
            mem_clock: parse_number(&fields[10]),
            pcie_gen,
            pcie_width,
            pcie_gen_max,
            pcie_width_max,
            pcie_tx_mb_s,
            pcie_rx_mb_s,
            tensor: None,
        });
    }
    gpus.sort_by_key(|gpu| gpu.index);
    (gpus, None, latency)
}

/// État, vitesse et erreurs NVLink ; le débit dépend du GPU et du pilote.
pub fn read_nvlink() -> NvLink {
    let mut speed: Option<f64> = None;
    let mut lines = 0usize;
    let mut command = Command::new("nvidia-smi");
    command.args(["nvlink", "-s"]);
    if let Ok(output) = run_with_timeout(&mut command, Duration::from_secs(3)) {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if !line.trim_start().starts_with("Link") {
                continue;
            }
            let tokens: Vec<&str> = line.split_whitespace().collect();
            if let Some(position) = tokens.iter().position(|token| *token == "GB/s") {
                if position > 0 {
                    if let Ok(value) = tokens[position - 1].parse::<f64>() {
                        lines += 1;
                        speed = Some(speed.map_or(value, |current: f64| current.max(value)));
                    }
                }
            }
        }
    }
    let mut errors = 0u64;
    let mut command = Command::new("nvidia-smi");
    command.args(["nvlink", "-e"]);
    if let Ok(output) = run_with_timeout(&mut command, Duration::from_secs(3)) {
        let stdout = String::from_utf8_lossy(&output.stdout);
        for line in stdout.lines() {
            if let Some(position) = line.find("Errors:") {
                if let Some(value) = line[position + 7..].split_whitespace().next() {
                    if let Ok(number) = value.parse::<u64>() {
                        errors += number;
                    }
                }
            }
        }
    }
    NvLink {
        speed_gbps: speed,
        links: if lines > 0 { 1 } else { 0 },
        errors,
        throughput: None,
    }
}

/// Topologie QPI/UPI générique : le noyau n'expose pas les liens de façon
/// portable, on affiche les sockets et l'absence de détail.
pub fn read_qpi(topology: &Topology) -> Qpi {
    Qpi {
        sockets: topology.packages.len(),
        links: None,
        speed_gt_s: None,
        traffic: None,
        reason: Some("topologie QPI non exposée".to_string()),
    }
}

fn amd_name(pci_id: &str) -> String {
    if pci_id.is_empty() {
        "AMD GPU".to_string()
    } else {
        format!("AMD GPU ({pci_id})")
    }
}

fn read_number(path: &Path, scale: f64) -> Option<f64> {
    read_trim(path)?
        .parse::<f64>()
        .ok()
        .map(|value| value / scale)
}

/// Carte AMD pilotée par amdgpu : température, VRAM, horloges, ventilateur.
pub struct AmdReader {
    device: Option<PathBuf>,
    hwmon: Option<PathBuf>,
    name: String,
}

impl AmdReader {
    pub fn new() -> Self {
        let mut reader = Self {
            device: None,
            hwmon: None,
            name: "AMD GPU".to_string(),
        };
        let Ok(entries) = fs::read_dir("/sys/class/drm") else {
            return reader;
        };
        for entry in entries.flatten() {
            let dir_name = entry.file_name().to_string_lossy().to_string();
            let Some(rest) = dir_name.strip_prefix("card") else {
                continue;
            };
            if !rest.chars().all(|character| character.is_ascii_digit()) {
                continue;
            }
            let device = entry.path().join("device");
            if read_trim(&device.join("vendor")).as_deref() != Some("0x1002") {
                continue;
            }
            reader.device = Some(device.clone());
            if let Some(uevent) = read_trim(&device.join("uevent")) {
                if let Some(position) = uevent.find("PCI_ID=") {
                    let pci_id = uevent[position + 7..]
                        .lines()
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_lowercase();
                    reader.name = amd_name(&pci_id).to_string();
                }
            }
            if let Ok(hwmons) = fs::read_dir(device.join("hwmon")) {
                for hwmon in hwmons.flatten() {
                    if read_trim(&hwmon.path().join("name")).as_deref() == Some("amdgpu") {
                        reader.hwmon = Some(hwmon.path());
                        break;
                    }
                }
            }
            break;
        }
        reader
    }

    pub fn read(&self) -> Option<Amd> {
        let device = self.device.as_ref()?;
        let hwmon = self.hwmon.as_ref();
        let temp = hwmon.and_then(|path| read_number(&path.join("temp1_input"), 1000.0));
        let sclk = hwmon.and_then(|path| read_number(&path.join("freq1_input"), 1_000_000.0));
        let mclk = hwmon.and_then(|path| read_number(&path.join("freq2_input"), 1_000_000.0));
        let pwm = hwmon.and_then(|path| read_number(&path.join("pwm1"), 1.0));
        let pwm_max = hwmon.and_then(|path| read_number(&path.join("pwm1_max"), 1.0));
        let fan_pct = match (pwm, pwm_max) {
            (Some(pwm), Some(maximum)) if maximum > 0.0 => Some(100.0 * pwm / maximum),
            _ => None,
        };
        let vram_used = read_number(&device.join("mem_info_vram_used"), 1.0).unwrap_or(0.0) as u64;
        let vram_total =
            read_number(&device.join("mem_info_vram_total"), 1.0).unwrap_or(0.0) as u64;
        let pstate = read_trim(&device.join("power_dpm_state"));
        let speed = read_trim(&device.join("current_link_speed"));
        let pcie_gen = speed.and_then(|value| {
            value
                .split_whitespace()
                .next()
                .and_then(|token| token.parse::<f64>().ok())
                .and_then(|speed| match speed {
                    2.5 => Some(1),
                    5.0 => Some(2),
                    8.0 => Some(3),
                    16.0 => Some(4),
                    32.0 => Some(5),
                    _ => None,
                })
        });
        let pcie_width =
            read_number(&device.join("current_link_width"), 1.0).map(|value| value as u32);
        let busy = read_number(&device.join("gpu_busy_percent"), 1.0);
        Some(Amd {
            name: self.name.clone(),
            temp,
            vram_used,
            vram_total,
            sclk_mhz: sclk,
            mclk_mhz: mclk,
            fan_pct,
            pstate,
            pcie_gen,
            pcie_width,
            busy,
        })
    }
}

/// Échantillonne le système et tient l'historique.
pub struct Sampler {
    pub interval: Duration,
    pub topology: Topology,
    pub metrics: Option<Metrics>,
    pub history: History,
    pub samples: usize,
    pub gpu_latency_ms: f64,
    nvml: Option<Nvml>,
    nvlink: NvLink,
    qpi: Qpi,
    amd: AmdReader,
    prev_cpu: Vec<Vec<u64>>,
    temps: PackageTempReader,
    users: HashMap<u32, String>,
    proc_prev: HashMap<u32, u64>,
    proc_last: Option<Instant>,
    gpu_processes: Vec<GpuProcess>,
    gpu_processes_tick: u64,
    clock_ticks: f64,
    page_size: u64,
    privacy: bool,
}

impl Sampler {
    pub fn new(interval: Duration, privacy: bool) -> Self {
        let topology = detect_topology();
        let qpi = read_qpi(&topology);
        let nvlink = read_nvlink();
        let nvml = Nvml::new();
        let amd = AmdReader::new();
        Self {
            interval,
            topology,
            metrics: None,
            history: History::default(),
            samples: 0,
            gpu_latency_ms: 0.0,
            nvml,
            nvlink,
            qpi,
            amd,
            prev_cpu: read_cpu_times(),
            temps: PackageTempReader::new(),
            users: read_user_map(),
            proc_prev: HashMap::new(),
            proc_last: None,
            gpu_processes: Vec::new(),
            gpu_processes_tick: 0,
            clock_ticks: clock_ticks(),
            page_size: memory_page_size(),
            privacy,
        }
    }

    /// Reprend une référence CPU propre (après une pause prolongée).
    pub fn reset_cpu_baseline(&mut self) {
        self.prev_cpu = read_cpu_times();
        self.proc_prev.clear();
        self.proc_last = None;
    }

    fn read_processes(&mut self, ram_total: u64) -> ProcessTable {
        let now = Instant::now();
        let elapsed = self
            .proc_last
            .map(|previous| now.duration_since(previous).as_secs_f64())
            .unwrap_or(0.0);
        let gpu_by_pid: HashMap<u32, u64> = self
            .gpu_processes
            .iter()
            .map(|process| (process.pid, process.mem_bytes))
            .collect();
        let mut current = HashMap::new();
        let mut all: Vec<Process> = Vec::new();

        if let Ok(entries) = fs::read_dir("/proc") {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(pid) = name.to_str().and_then(|value| value.parse::<u32>().ok()) else {
                    continue;
                };
                let path = PathBuf::from("/proc").join(pid.to_string());
                let Some((ticks, comm)) = read_proc_ticks(&path) else {
                    continue;
                };
                let cpu_pct = if elapsed > 0.001 {
                    self.proc_prev
                        .get(&pid)
                        .map(|previous| {
                            ticks.saturating_sub(*previous) as f64 * 100.0
                                / (self.clock_ticks * elapsed)
                        })
                        .unwrap_or(0.0)
                } else {
                    0.0
                };
                let rss_bytes = read_vm_rss_bytes(&path, self.page_size);
                let mem_pct = if ram_total > 0 {
                    100.0 * rss_bytes as f64 / ram_total as f64
                } else {
                    0.0
                };
                let uid = fs::metadata(&path).map(|meta| meta.uid()).unwrap_or(0);
                let user = if self.privacy {
                    "user".to_string()
                } else {
                    self.users
                        .get(&uid)
                        .cloned()
                        .unwrap_or_else(|| uid.to_string())
                };
                let command = read_process_command(&path, &comm, self.privacy);
                current.insert(pid, ticks);
                all.push(Process {
                    pid,
                    user,
                    command,
                    cpu_pct: cpu_pct.max(0.0),
                    mem_pct,
                    rss_bytes,
                    gpu_mem_bytes: gpu_by_pid.get(&pid).copied().unwrap_or(0),
                });
            }
        }

        // Un processus GPU peut être invisible dans /proc (conteneur) : on garde
        // au moins son PID, son nom et sa mémoire GPU.
        for gpu in &self.gpu_processes {
            if !current.contains_key(&gpu.pid) {
                all.push(Process {
                    pid: gpu.pid,
                    user: if self.privacy {
                        "user".into()
                    } else {
                        "?".into()
                    },
                    command: if self.privacy {
                        private_command(&gpu.name)
                    } else {
                        gpu.name.clone()
                    },
                    cpu_pct: 0.0,
                    mem_pct: 0.0,
                    rss_bytes: 0,
                    gpu_mem_bytes: gpu.mem_bytes,
                });
            }
        }

        self.proc_prev = current;
        self.proc_last = Some(now);

        let mut by_cpu = all.clone();
        by_cpu.sort_by(|a, b| {
            b.cpu_pct
                .total_cmp(&a.cpu_pct)
                .then_with(|| b.rss_bytes.cmp(&a.rss_bytes))
        });
        let mut by_mem = all.clone();
        by_mem.sort_by_key(|process| std::cmp::Reverse(process.rss_bytes));
        let mut by_gpu: Vec<Process> = all
            .into_iter()
            .filter(|process| process.gpu_mem_bytes > 0)
            .collect();
        by_gpu.sort_by_key(|process| std::cmp::Reverse(process.gpu_mem_bytes));

        let total = by_cpu.len().max(by_mem.len());
        ProcessTable {
            top_cpu: by_cpu.into_iter().take(12).collect(),
            top_mem: by_mem.into_iter().take(12).collect(),
            top_gpu: by_gpu.into_iter().take(4).collect(),
            total,
        }
    }

    /// Attend une demi-seconde puis prend la première mesure utile.
    pub fn prime(&mut self) {
        // Référence processus immédiate : la première frame affiche déjà des
        // pourcentages CPU au lieu de zéros.
        self.gpu_processes_tick = 0;
        let _ = self.read_processes(0);
        thread::sleep(Duration::from_millis(500));
        self.sample(true);
    }

    pub fn sample(&mut self, record: bool) {
        let times = read_cpu_times();
        let mut per_thread = vec![0.0; times.len()];
        for index in 0..times.len() {
            if index >= self.prev_cpu.len() {
                continue;
            }
            let current = &times[index];
            let previous = &self.prev_cpu[index];
            let length = current.len().min(previous.len());
            let mut total = 0u64;
            let mut idle = 0u64;
            for field in 0..length {
                let delta = current[field].saturating_sub(previous[field]);
                total += delta;
                if field == 3 || field == 4 {
                    idle += delta;
                }
            }
            if total > 0 {
                per_thread[index] =
                    (100.0 * total.saturating_sub(idle) as f64 / total as f64).clamp(0.0, 100.0);
            }
        }
        self.prev_cpu = times;

        let (gpus, gpu_error, gpu_latency) = read_gpus(&mut self.nvml);
        self.gpu_latency_ms = gpu_latency;
        self.gpu_processes_tick = self.gpu_processes_tick.wrapping_add(1);
        if self.gpu_processes_tick % 5 == 1 {
            self.gpu_processes = read_gpu_processes();
        }
        if record && self.samples.is_multiple_of(10) {
            self.nvlink = read_nvlink();
        }
        let amd = self.amd.read();

        let mem = read_meminfo();
        let ram_total = *mem.get("MemTotal").unwrap_or(&0);
        let ram_available = mem.get("MemAvailable").copied().unwrap_or_else(|| {
            mem.get("MemFree").copied().unwrap_or(0)
                + mem.get("Buffers").copied().unwrap_or(0)
                + mem.get("Cached").copied().unwrap_or(0)
        });
        let ram_used = ram_total.saturating_sub(ram_available);
        let processes = self.read_processes(ram_total);
        let swap_total = *mem.get("SwapTotal").unwrap_or(&0);
        let swap_used = swap_total.saturating_sub(*mem.get("SwapFree").unwrap_or(&0));

        let vram_used: u64 = gpus.iter().map(|gpu| gpu.mem_used).sum();
        let vram_total: u64 = gpus.iter().map(|gpu| gpu.mem_total).sum();

        let frequencies = read_frequencies(per_thread.len());
        let temps = self.temps.read();

        let mut socket_pct: BTreeMap<usize, f64> = BTreeMap::new();
        let mut socket_freq: BTreeMap<usize, Option<f64>> = BTreeMap::new();
        let mut socket_temp: BTreeMap<usize, Option<f64>> = BTreeMap::new();
        let mut socket_saturated: BTreeMap<usize, bool> = BTreeMap::new();
        for &package in &self.topology.packages {
            let mut values = Vec::new();
            let mut freqs = Vec::new();
            for core in self
                .topology
                .cores
                .iter()
                .filter(|core| core.package == package)
            {
                for &thread in &core.threads {
                    if let Some(&value) = per_thread.get(thread) {
                        values.push(value);
                    }
                    if let Some(Some(frequency)) = frequencies.get(thread) {
                        freqs.push(*frequency);
                    }
                }
            }
            socket_pct.insert(
                package,
                if values.is_empty() {
                    0.0
                } else {
                    values.iter().sum::<f64>() / values.len() as f64
                },
            );
            socket_freq.insert(
                package,
                if freqs.is_empty() {
                    None
                } else {
                    Some(freqs.iter().sum::<f64>() / freqs.len() as f64)
                },
            );
            socket_temp.insert(package, temps.get(&package).copied());
            socket_saturated.insert(
                package,
                values.iter().any(|value| *value >= SATURATED_THRESHOLD),
            );
        }

        let known_freqs: Vec<f64> = frequencies.iter().filter_map(|value| *value).collect();
        let freq_avg = if known_freqs.is_empty() {
            None
        } else {
            Some(known_freqs.iter().sum::<f64>() / known_freqs.len() as f64)
        };
        let cpu_total = if per_thread.is_empty() {
            0.0
        } else {
            per_thread.iter().sum::<f64>() / per_thread.len() as f64
        };
        let ram_pct = if ram_total > 0 {
            100.0 * ram_used as f64 / ram_total as f64
        } else {
            0.0
        };
        let vram_pct = if vram_total > 0 {
            100.0 * vram_used as f64 / vram_total as f64
        } else {
            0.0
        };
        let saturated_global = per_thread.iter().any(|value| *value >= SATURATED_THRESHOLD);

        if record {
            History::push(&mut self.history.cpu, cpu_total);
            History::push(&mut self.history.ram, ram_pct);
            History::push(&mut self.history.vram, vram_pct);
            History::push_bool(&mut self.history.saturated, saturated_global);
            let packages = self.topology.packages.clone();
            for package in packages {
                let pct = *socket_pct.get(&package).unwrap_or(&0.0);
                let saturated = *socket_saturated.get(&package).unwrap_or(&false);
                History::push(self.history.socket.entry(package).or_default(), pct);
                History::push_bool(
                    self.history.socket_saturated.entry(package).or_default(),
                    saturated,
                );
            }
            for gpu in &gpus {
                History::push(
                    self.history.gpu.entry(gpu.index).or_default(),
                    gpu.util.unwrap_or(0.0),
                );
            }
            self.samples += 1;
        }

        self.metrics = Some(Metrics {
            cpu_pct: per_thread,
            cpu_total,
            socket_pct,
            socket_freq,
            socket_temp,
            freq_avg,
            ram_used,
            ram_total,
            ram_pct,
            swap_used,
            swap_total,
            vram_used,
            vram_total,
            vram_pct,
            load: read_load(),
            uptime: read_uptime(),
            gpus,
            gpu_error,
            nvlink: self.nvlink.clone(),
            qpi: self.qpi.clone(),
            amd,
            processes,
        });
    }
}
