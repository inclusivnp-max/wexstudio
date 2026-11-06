//! LMDB disk cache. Small reads copy only the requested record, never the whole index.
use anyhow::{Result, ensure};
use heed::{
    Database, Env, EnvOpenOptions,
    types::{Bytes, Str},
};
use std::path::Path;

pub struct DiskCache {
    env: Env,
    records: Database<Str, Bytes>,
}
impl DiskCache {
    /// One environment handle per directory per process. Do not externally mutate its files.
    pub fn open(path: &Path) -> Result<Self> {
        std::fs::create_dir_all(path)?;
        // SAFETY: heed coordinates process-local opens and LMDB owns its mmap. This API
        // never modifies mapped files outside transactions or exposes mapped references.
        let env = unsafe {
            EnvOpenOptions::new()
                .map_size(256 * 1024 * 1024)
                .max_dbs(2)
                .open(path)?
        };
        let mut tx = env.write_txn()?;
        let records = env.create_database(&mut tx, Some("records"))?;
        tx.commit()?;
        Ok(Self { env, records })
    }
    pub fn put(&self, key: &str, value: &[u8]) -> Result<()> {
        ensure!(key.len() <= 500, "cache key too long");
        let mut tx = self.env.write_txn()?;
        self.records.put(&mut tx, key, value)?;
        tx.commit()?;
        Ok(())
    }
    pub fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let tx = self.env.read_txn()?;
        Ok(self.records.get(&tx, key)?.map(<[u8]>::to_vec))
    }
    pub fn delete(&self, key: &str) -> Result<()> {
        let mut tx = self.env.write_txn()?;
        self.records.delete(&mut tx, key)?;
        tx.commit()?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disk_round_trip_reopen() {
        let d = tempfile::tempdir().unwrap();
        {
            let c = DiskCache::open(d.path()).unwrap();
            c.put("symbol:Main", b"app/src/Main.kt:1").unwrap();
        }
        {
            let c = DiskCache::open(d.path()).unwrap();
            assert_eq!(c.get("symbol:Main").unwrap().unwrap(), b"app/src/Main.kt:1");
            c.delete("symbol:Main").unwrap();
            assert!(c.get("symbol:Main").unwrap().is_none());
        }
    }
}
