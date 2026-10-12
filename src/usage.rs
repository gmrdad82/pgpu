use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::Instant;

use crate::gpu::number;

const SHADING: [&str; 2] = ["gfx", "compute"];

const VIDEO: [&str; 4] = ["enc", "dec", "jpeg", "vcn"];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Engine {
    pub ns: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub struct FdInfo {
    pub client: u64,
    pub engines: BTreeMap<String, Engine>,
    pub vram_kib: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Client {
    pub engines: BTreeMap<String, Engine>,
    pub vram_kib: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sample {
    pub clients: BTreeMap<u64, Client>,
}

impl Sample {
    pub fn vram_kib(&self) -> u64 {
        self.clients
            .values()
            .fold(0, |sum, client| sum.saturating_add(client.vram_kib))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Card {
    pub used: u64,
    pub total: u64,
}

#[derive(Clone, Debug)]
pub struct Rate {
    pub engines: BTreeMap<String, f64>,
    pub combined: f64,
}

#[derive(Default)]
pub struct Meter {
    prev: Option<Held>,
}

struct Held {
    at: Instant,
    sample: Sample,
}

#[derive(Default)]
struct Acc {
    delta: u64,
}

pub fn uses_kfd(pids: &[u32]) -> bool {
    pids.iter().any(|pid| {
        fs::read_dir(format!("/proc/{pid}/fd")).is_ok_and(|fds| {
            fds.flatten()
                .any(|fd| fs::read_link(fd.path()).is_ok_and(|to| to == Path::new("/dev/kfd")))
        })
    })
}

pub fn parse_fdinfo(text: &str) -> Option<FdInfo> {
    let mut amdgpu = false;
    let mut client = None;
    let mut engines: BTreeMap<String, Engine> = BTreeMap::new();
    let mut vram_kib = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if key == "drm-driver" {
            if value != "amdgpu" {
                return None;
            }
            amdgpu = true;
        } else if key == "drm-client-id" {
            client = leading_u64(value);
        } else if key == "drm-memory-vram" {
            vram_kib = leading_u64(value);
        } else if key.starts_with("drm-engine-capacity-") {
            continue;
        } else if let Some(name) = key.strip_prefix("drm-engine-") {
            if name.is_empty() {
                continue;
            }
            if let Some(ns) = leading_u64(value) {
                engines.entry(name.to_string()).or_default().ns = ns;
            }
        }
    }
    if !amdgpu {
        return None;
    }
    Some(FdInfo {
        client: client?,
        engines,
        vram_kib,
    })
}

pub fn sample(pids: &[u32]) -> Sample {
    sample_in(Path::new("/proc"), pids)
}

fn sample_in(proc_root: &Path, pids: &[u32]) -> Sample {
    let mut clients = BTreeMap::new();
    for pid in pids {
        let Ok(entries) = fs::read_dir(proc_root.join(pid.to_string()).join("fdinfo")) else {
            continue;
        };
        for entry in entries.flatten() {
            absorb_file(&mut clients, &entry.path());
        }
    }
    Sample { clients }
}

fn absorb_file(clients: &mut BTreeMap<u64, Client>, fdinfo: &Path) {
    if let Some(info) = fs::read_to_string(fdinfo)
        .ok()
        .and_then(|text| parse_fdinfo(&text))
    {
        absorb(clients, info);
    }
}

fn absorb(clients: &mut BTreeMap<u64, Client>, info: FdInfo) {
    let vram = info.vram_kib.unwrap_or(0);
    let have = clients.entry(info.client).or_insert(Client {
        engines: BTreeMap::new(),
        vram_kib: 0,
    });
    if info.vram_kib.is_some() {
        have.vram_kib = have.vram_kib.max(vram);
    }
    for (name, eng) in info.engines {
        have.engines
            .entry(name)
            .and_modify(|old| old.ns = old.ns.max(eng.ns))
            .or_insert(eng);
    }
}

pub fn shading_ns(client: &Client) -> u64 {
    SHADING
        .iter()
        .filter_map(|name| client.engines.get(*name))
        .fold(0, |sum, eng| sum.saturating_add(eng.ns))
}

pub fn dri_clients(proc_root: &Path, pid: u32) -> BTreeMap<u64, Client> {
    let base = proc_root.join(pid.to_string());
    let mut clients = BTreeMap::new();
    let Ok(fds) = fs::read_dir(base.join("fd")) else {
        return clients;
    };
    for fd in fds.flatten() {
        if fs::read_link(fd.path()).is_ok_and(|to| to.starts_with("/dev/dri/")) {
            absorb_file(&mut clients, &base.join("fdinfo").join(fd.file_name()));
        }
    }
    clients
}

pub fn card() -> Option<Card> {
    if let Some(body) = crate::fake::fake("vram") {
        return fake_card(&body);
    }
    card_at(crate::gpu::device()?)
}

fn fake_card(body: &str) -> Option<Card> {
    let mut words = body.split_whitespace();
    Some(Card {
        used: words.next()?.parse().ok()?,
        total: words.next()?.parse().ok()?,
    })
}

fn card_at(device: &Path) -> Option<Card> {
    Some(Card {
        used: number(&device.join("mem_info_vram_used"))?,
        total: number(&device.join("mem_info_vram_total"))?,
    })
}

impl Rate {
    pub fn video(&self) -> bool {
        self.engines
            .iter()
            .any(|(name, frac)| *frac > 0.0 && VIDEO.iter().any(|v| name.starts_with(v)))
    }
}

impl Meter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, sample: Sample, at: Instant) -> Option<Rate> {
        let rate = self
            .prev
            .as_ref()
            .and_then(|prev| rate_between(prev, &sample, at));
        self.prev = Some(Held { at, sample });
        rate
    }
}

fn rate_between(prev: &Held, sample: &Sample, at: Instant) -> Option<Rate> {
    let elapsed = at.checked_duration_since(prev.at)?;
    let elapsed_ns = u64::try_from(elapsed.as_nanos()).ok()?;
    if elapsed_ns == 0 {
        return None;
    }
    let mut totals: BTreeMap<String, Acc> = BTreeMap::new();
    for (id, client) in &sample.clients {
        let Some(old) = prev.sample.clients.get(id) else {
            continue;
        };
        for (name, eng) in &client.engines {
            let delta = old
                .engines
                .get(name)
                .map(|was| eng.ns.saturating_sub(was.ns))
                .unwrap_or(0);
            let acc = totals.entry(name.clone()).or_default();
            acc.delta = acc.delta.saturating_add(delta);
        }
    }
    let mut engines = BTreeMap::new();
    let mut combined: f64 = 0.0;
    for (name, acc) in totals {
        let frac = (acc.delta as f64 / elapsed_ns as f64).clamp(0.0, 1.0);
        if SHADING.contains(&name.as_str()) {
            combined = combined.max(frac);
        }
        engines.insert(name, frac);
    }
    Some(Rate { engines, combined })
}

fn leading_u64(value: &str) -> Option<u64> {
    value.split_whitespace().next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::{Card, Client, Engine, Meter, Sample, card_at, parse_fdinfo, sample_in};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    fn test_dir(name: &str) -> PathBuf {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tmp")
            .join(format!("test-{name}"));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn thou(v: f64) -> i64 {
        (v * 1000.0).round() as i64
    }

    fn client(vram: u64, engines: &[(&str, u64)]) -> Client {
        Client {
            vram_kib: vram,
            engines: engines
                .iter()
                .map(|(name, ns)| ((*name).to_string(), Engine { ns: *ns }))
                .collect(),
        }
    }

    fn sample(pairs: &[(u64, Client)]) -> Sample {
        Sample {
            clients: pairs.iter().cloned().collect(),
        }
    }

    #[test]
    fn parse_gfx_only() {
        let info = parse_fdinfo(
            "\
pos:\t0\n\
flags:\t02100002\n\
drm-driver:\tamdgpu\n\
drm-pdev:\t0000:03:00.0\n\
drm-client-id:\t4\n\
drm-memory-gtt:\t8 KiB\n\
drm-memory-vram:\t10 KiB\n\
drm-engine-gfx:\t1000 ns\n",
        )
        .unwrap();
        assert_eq!(info.client, 4);
        assert_eq!(info.vram_kib, Some(10));
        assert_eq!(info.engines.len(), 1);
        assert_eq!(info.engines["gfx"].ns, 1000);
    }

    #[test]
    fn parse_gfx_and_compute() {
        let info = parse_fdinfo(
            "\
drm-driver:\tamdgpu\n\
drm-client-id:\t8\n\
drm-memory-vram:\t20 KiB\n\
drm-engine-gfx:\t1000 ns\n\
drm-engine-compute:\t250 ns\n",
        )
        .unwrap();
        assert_eq!(info.client, 8);
        assert_eq!(info.vram_kib, Some(20));
        assert_eq!(info.engines["gfx"].ns, 1000);
        assert_eq!(info.engines["compute"].ns, 250);
    }

    #[test]
    fn capacity_lines_are_not_engines() {
        let info = parse_fdinfo(
            "\
drm-driver:\tamdgpu\n\
drm-client-id:\t3\n\
drm-memory-vram:\t1 KiB\n\
drm-engine-capacity-gfx:\t2\n\
drm-engine-gfx:\t1000 ns\n\
drm-engine-compute:\t250 ns\n\
drm-engine-capacity-compute:\t4\n",
        )
        .unwrap();
        assert_eq!(info.engines.len(), 2);
        assert!(!info.engines.contains_key("capacity-gfx"));
        assert_eq!(info.engines["gfx"].ns, 1000);
        assert_eq!(info.engines["compute"].ns, 250);
    }

    #[test]
    fn parse_ignores_non_amdgpu() {
        assert!(
            parse_fdinfo(
                "\
drm-driver:\ti915\n\
drm-client-id:\t4\n\
drm-memory-vram:\t10 KiB\n\
drm-engine-gfx:\t1000 ns\n",
            )
            .is_none()
        );
        assert!(parse_fdinfo("pos:\t0\n").is_none());
    }

    #[test]
    fn parse_missing_vram() {
        let info = parse_fdinfo(
            "\
drm-driver:\tamdgpu\n\
drm-client-id:\t4\n\
drm-engine-gfx:\t1000 ns\n",
        )
        .unwrap();
        assert_eq!(info.client, 4);
        assert_eq!(info.vram_kib, None);
        assert_eq!(info.engines["gfx"].ns, 1000);
    }

    #[test]
    fn sample_counts_a_client_once() {
        let root = test_dir("usage-sample");
        write_fd(
            &root,
            10,
            "3",
            "\
drm-driver:\tamdgpu\n\
drm-client-id:\t7\n\
drm-memory-vram:\t100 KiB\n\
drm-engine-gfx:\t1000 ns\n",
        );
        write_fd(
            &root,
            10,
            "4",
            "\
drm-driver:\tamdgpu\n\
drm-client-id:\t7\n\
drm-memory-vram:\t100 KiB\n\
drm-engine-gfx:\t1000 ns\n",
        );
        write_fd(
            &root,
            10,
            "5",
            "\
drm-driver:\ti915\n\
drm-client-id:\t7\n\
drm-memory-vram:\t99999 KiB\n\
drm-engine-gfx:\t99999 ns\n",
        );
        write_fd(&root, 10, "9", "");
        write_fd(
            &root,
            11,
            "8",
            "\
drm-driver:\tamdgpu\n\
drm-client-id:\t7\n\
drm-memory-vram:\t100 KiB\n\
drm-engine-gfx:\t1000 ns\n",
        );
        write_fd(
            &root,
            12,
            "1",
            "\
drm-driver:\tamdgpu\n\
drm-client-id:\t9\n\
drm-memory-vram:\t3 KiB\n\
drm-engine-gfx:\t50 ns\n\
drm-engine-compute:\t7 ns\n",
        );
        let got = sample_in(&root, &[10, 11, 12, 99]);
        assert_eq!(got.clients.len(), 2);
        let seven = &got.clients[&7];
        assert_eq!(seven.vram_kib, 100);
        assert_eq!(seven.engines["gfx"].ns, 1000);
        assert!(!seven.engines.contains_key("compute"));
        let nine = &got.clients[&9];
        assert_eq!(nine.vram_kib, 3);
        assert_eq!(nine.engines["gfx"].ns, 50);
        assert_eq!(nine.engines["compute"].ns, 7);
        assert_eq!(got.vram_kib(), 103);
    }

    #[test]
    fn meter_three_samples_skip_a_vanished_client() {
        let t0 = Instant::now();
        let mut meter = Meter::new();
        let first = meter.push(
            sample(&[
                (1, client(10, &[("gfx", 0), ("compute", 0), ("enc", 0)])),
                (2, client(80, &[("gfx", 5_000_000_000)])),
            ]),
            t0,
        );
        assert!(first.is_none());
        let second = meter
            .push(
                sample(&[
                    (
                        1,
                        client(
                            10,
                            &[
                                ("gfx", 500_000_000),
                                ("compute", 250_000_000),
                                ("enc", 900_000_000),
                            ],
                        ),
                    ),
                    (2, client(80, &[("gfx", 5_000_000_000)])),
                    (3, client(1, &[("gfx", 9_000_000_000)])),
                ]),
                t0 + Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(thou(second.engines["gfx"]), 500);
        assert_eq!(thou(second.engines["compute"]), 250);
        assert_eq!(thou(second.engines["enc"]), 900);
        assert_eq!(thou(second.combined), 500);
        assert!(second.video());
        let third = meter
            .push(
                sample(&[
                    (
                        1,
                        client(
                            10,
                            &[
                                ("gfx", 750_000_000),
                                ("compute", 1_750_000_000),
                                ("enc", 900_000_000),
                            ],
                        ),
                    ),
                    (3, client(1, &[("gfx", 9_000_000_000)])),
                ]),
                t0 + Duration::from_secs(2),
            )
            .unwrap();
        assert_eq!(thou(third.engines["gfx"]), 250);
        assert_eq!(thou(third.engines["compute"]), 1000);
        assert_eq!(thou(third.combined), 1000);
        assert!(!third.video());
        assert!(third.engines.values().all(|frac| *frac >= 0.0));
    }

    #[test]
    fn card_reads_the_devices_vram() {
        let dev = test_dir("usage-card");
        assert!(card_at(&dev).is_none());
        fs::write(dev.join("mem_info_vram_used"), "5\n").unwrap();
        fs::write(dev.join("mem_info_vram_total"), "6\n").unwrap();
        assert_eq!(card_at(&dev).unwrap(), Card { used: 5, total: 6 });
    }

    #[test]
    fn the_fake_card_is_used_and_total_bytes_or_none() {
        assert_eq!(
            super::fake_card("5 6\n").unwrap(),
            Card { used: 5, total: 6 }
        );
        assert!(super::fake_card("none").is_none());
        assert!(super::fake_card("5").is_none());
    }

    fn write_fd(root: &Path, pid: u32, fd: &str, body: &str) {
        let dir = root.join(pid.to_string()).join("fdinfo");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(fd), body).unwrap();
    }
}
