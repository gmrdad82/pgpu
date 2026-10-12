use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const VENDOR: &str = "0x1002";

pub(crate) fn device() -> Option<&'static Path> {
    static FOUND: OnceLock<Option<PathBuf>> = OnceLock::new();
    FOUND
        .get_or_init(|| device_in(Path::new("/sys/class/drm")))
        .as_deref()
}

fn device_in(drm: &Path) -> Option<PathBuf> {
    let mut cards: Vec<PathBuf> = fs::read_dir(drm)
        .ok()?
        .flatten()
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name.starts_with("card") && !name.contains('-')
        })
        .map(|entry| entry.path().join("device"))
        .collect();
    cards.sort();
    cards
        .into_iter()
        .filter(|dev| {
            number::<String>(&dev.join("vendor")).as_deref() == Some(VENDOR)
                && dev.join("gpu_busy_percent").exists()
        })
        .rev()
        .max_by_key(|dev| number::<u64>(&dev.join("mem_info_vram_total")).unwrap_or(0))
}

pub(crate) fn number<T: std::str::FromStr>(path: &Path) -> Option<T> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

pub(crate) fn busy() -> Option<u32> {
    if let Some(busy) = crate::fake::fake_percent("card_busy") {
        return Some(busy);
    }
    number(&device()?.join("gpu_busy_percent"))
}

#[cfg(test)]
mod tests {
    use super::{device_in, number};
    use std::fs;
    use std::path::{Path, PathBuf};

    fn test_dir(name: &str) -> PathBuf {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tmp")
            .join(format!("test-{name}"));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn card(drm: &Path, name: &str, vendor: &str, vram: Option<u64>) {
        let dev = drm.join(name).join("device");
        fs::create_dir_all(&dev).unwrap();
        fs::write(dev.join("vendor"), format!("{vendor}\n")).unwrap();
        if let Some(vram) = vram {
            fs::write(dev.join("gpu_busy_percent"), "0\n").unwrap();
            fs::write(dev.join("mem_info_vram_total"), format!("{vram}\n")).unwrap();
        }
    }

    #[test]
    fn one_reader_takes_a_trimmed_value_of_any_type() {
        let dir = test_dir("gpu-number");
        let path = dir.join("value");
        assert_eq!(number::<u32>(&path), None);
        fs::write(&path, " 42\n").unwrap();
        assert_eq!(number::<u32>(&path), Some(42));
        assert_eq!(number::<u64>(&path), Some(42));
        assert_eq!(number::<String>(&path).as_deref(), Some("42"));
        fs::write(&path, "0x1002\n").unwrap();
        assert_eq!(number::<u32>(&path), None);
        assert_eq!(number::<String>(&path).as_deref(), Some("0x1002"));
        fs::write(&path, "-1\n").unwrap();
        assert_eq!(number::<u64>(&path), None);
        fs::write(&path, "").unwrap();
        assert_eq!(number::<u32>(&path), None);
        assert_eq!(number::<String>(&path).as_deref(), Some(""));
    }

    #[test]
    fn takes_the_amd_card_with_a_busy_reading_and_the_most_vram() {
        let drm = test_dir("gpu-device");
        assert!(device_in(&drm).is_none());
        card(&drm, "card0", "0x1002", None);
        card(&drm, "card2", "0x10de", Some(32 << 30));
        card(&drm, "card1-DP-1", "0x1002", Some(16 << 30));
        assert!(device_in(&drm).is_none());
        card(&drm, "card3", "0x1002", Some(512 << 20));
        assert_eq!(device_in(&drm).unwrap(), drm.join("card3").join("device"));
        card(&drm, "card1", "0x1002", Some(16 << 30));
        assert_eq!(device_in(&drm).unwrap(), drm.join("card1").join("device"));
    }
}
