//! D8 output inventory. Receipts verify all bytes; headers additionally reject
//! malformed/non-DEX payloads. This is not a DEX verifier or ART compatibility test.
use anyhow::{Context, Result, ensure};
use std::path::{Path, PathBuf};
pub(crate) const ABI: &str = "d8-dex-v2";
pub(crate) fn validate(root: &Path) -> Result<Vec<PathBuf>> {
    let mut ordered = std::collections::BTreeMap::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let m = entry.file_type()?;
        ensure!(
            m.is_file() && !m.is_symlink(),
            "DEX payload contains non-file"
        );
        let name = entry.file_name();
        let name = name.to_str().context("DEX filename encoding")?;
        let index = if name == "classes.dex" {
            1
        } else {
            let number = name
                .strip_prefix("classes")
                .and_then(|s| s.strip_suffix(".dex"))
                .context("unexpected DEX output file")?;
            let n: usize = number.parse()?;
            ensure!(
                n >= 2 && n.to_string() == number,
                "noncanonical DEX filename"
            );
            n
        };
        ensure!(
            ordered.insert(index, entry.path()).is_none() && ordered.len() <= 100_000,
            "DEX file limit/duplicate"
        );
    }
    ensure!(!ordered.is_empty(), "D8 produced no DEX");
    let mut total = 0u64;
    for (offset, (i, path)) in ordered.iter().enumerate() {
        ensure!(*i == offset + 1, "DEX output has gaps");
        let size = std::fs::metadata(path)?.len();
        total = total.checked_add(size).context("DEX size overflow")?;
        ensure!(
            (112..=1024 * 1024 * 1024).contains(&size) && total <= 1024 * 1024 * 1024,
            "DEX output size limit"
        );
        use std::io::Read;
        let mut header = [0u8; 112];
        std::fs::File::open(path)?.read_exact(&mut header)?;
        ensure!(
            &header[..4] == b"dex\n"
                && header[4..7].iter().all(u8::is_ascii_digit)
                && header[7] == 0,
            "DEX magic invalid"
        );
        let uint = |p| u32::from_le_bytes(header[p..p + 4].try_into().unwrap()) as u64;
        ensure!(
            uint(40) == 0x12345678 && uint(36) >= 112 && uint(36) <= size,
            "DEX header invalid"
        );
        // v041 permits multiple headers in a container. For older versions the
        // declared file_size is exactly the physical file size (AOSP DEX format).
        let version = std::str::from_utf8(&header[4..7])?.parse::<u32>()?;
        ensure!(
            uint(32) >= 112 && uint(32) <= size && (version >= 41 || uint(32) == size),
            "DEX file size invalid"
        );
    }
    Ok(ordered.into_values().collect())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn dex() -> Vec<u8> {
        let mut b = vec![0; 112];
        b[..8].copy_from_slice(b"dex\n039\0");
        b[32..36].copy_from_slice(&112u32.to_le_bytes());
        b[36..40].copy_from_slice(&112u32.to_le_bytes());
        b[40..44].copy_from_slice(&0x12345678u32.to_le_bytes());
        b
    }
    #[test]
    fn validates_contiguous_owned_payload_not_extra_files_or_gaps() {
        let d = tempfile::tempdir().unwrap();
        assert!(validate(d.path()).is_err());
        std::fs::write(d.path().join("classes.dex"), dex()).unwrap();
        std::fs::write(d.path().join("classes2.dex"), dex()).unwrap();
        assert_eq!(validate(d.path()).unwrap().len(), 2);
        std::fs::write(d.path().join("extra"), "x").unwrap();
        assert!(validate(d.path()).is_err());
        std::fs::remove_file(d.path().join("extra")).unwrap();
        std::fs::rename(d.path().join("classes2.dex"), d.path().join("classes3.dex")).unwrap();
        assert!(validate(d.path()).is_err());
    }
    #[test]
    fn malformed_header_or_size_is_rejected() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("classes.dex");
        let mut b = dex();
        b[40] = 0;
        std::fs::write(&p, &b).unwrap();
        assert!(validate(d.path()).is_err());
        let mut b = dex();
        b.push(0);
        std::fs::write(&p, &b).unwrap();
        assert!(validate(d.path()).is_err());
        std::fs::write(&p, b"bad").unwrap();
        assert!(validate(d.path()).is_err());
    }
    #[test]
    fn dex_cache_namespace_isolated_and_restores_copies() {
        let d = tempfile::tempdir().unwrap();
        let k = "d".repeat(64);
        let c = crate::compile_cache::Cache::dex(d.path(), &k).unwrap();
        let source = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("classes.dex"), dex()).unwrap();
        c.publish(source.path()).unwrap();
        let out = tempfile::tempdir().unwrap();
        assert_eq!(c.restore(out.path()).unwrap(), Some(1));
        validate(out.path()).unwrap();
        std::fs::write(out.path().join("classes.dex"), "changed").unwrap();
        let another = tempfile::tempdir().unwrap();
        c.restore(another.path()).unwrap();
        validate(another.path()).unwrap();
        assert!(
            crate::compile_cache::Cache::open(d.path(), "debug", "app", &k)
                .unwrap()
                .restore(tempfile::tempdir().unwrap().path())
                .unwrap()
                .is_none()
        );
        assert!(
            crate::sdk_cache::Guard::dex(d.path(), &k)
                .unwrap()
                .is_some()
        );
    }
}
