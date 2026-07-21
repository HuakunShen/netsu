//! Atomic file output for mux results and NDJSON samples: write to a temp file,
//! fsync, then rename over the target so a reader never sees a partial file.

use std::io::Write;
use std::path::Path;

use anyhow::Context;

/// Write `contents` to `path` atomically (temp + fsync + rename).
pub fn write_atomic(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp = path.with_extension(format!(
        "tmp-{}",
        std::process::id()
    ));
    {
        let mut f = std::fs::File::create(&tmp)
            .with_context(|| format!("create temp file in {}", dir.display()))?;
        f.write_all(contents).context("write temp file")?;
        f.sync_all().context("fsync temp file")?;
    }
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename into {}", path.display()))?;
    Ok(())
}

/// Serialize a value as pretty JSON and write it atomically.
pub fn write_json_atomic<T: serde::Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    let json = serde_json::to_vec_pretty(value).context("serialize JSON")?;
    write_atomic(path, &json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_replaces_target() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("netsu-mux-out-{}.json", std::process::id()));
        write_atomic(&path, b"first").unwrap();
        write_atomic(&path, b"second").unwrap();
        let got = std::fs::read_to_string(&path).unwrap();
        assert_eq!(got, "second");
        let _ = std::fs::remove_file(&path);
    }
}
