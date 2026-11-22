use anyhow::Result;
use serde_json::{Value, json};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

const CHUNK_SIZE: usize = 64 * 1024;
static WRITE_LOCK: Mutex<()> = Mutex::new(());

pub fn log_path(artifacts: &Path, id: i64) -> PathBuf {
    artifacts.join(format!("job-{id}.log"))
}

pub fn append(path: &Path, text: &str) -> Result<()> {
    let _guard = WRITE_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(text.as_bytes())?;
    Ok(())
}

pub fn read_log(path: &Path, offset: u64) -> Result<Value> {
    read_chunk(path, offset, CHUNK_SIZE)
}

fn read_chunk(path: &Path, offset: u64, limit: usize) -> Result<Value> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(json!({"text":"", "offset":offset, "size":0}));
        }
        Err(error) => return Err(error.into()),
    };
    let size = file.metadata()?.len();
    let offset = offset.min(size);
    file.seek(SeekFrom::Start(offset))?;
    let mut data = Vec::new();
    file.take(limit as u64).read_to_end(&mut data)?;
    if offset + (data.len() as u64) < size
        && let Err(error) = std::str::from_utf8(&data)
        && error.error_len().is_none()
        && error.valid_up_to() > 0
    {
        data.truncate(error.valid_up_to());
    }
    Ok(
        json!({"text":String::from_utf8_lossy(&data), "offset":offset + data.len() as u64, "size":size}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incremental_reads_preserve_multibyte_characters() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("job.log");
        append(&path, "ab€cd")?;
        let first = read_chunk(&path, 0, 4)?;
        assert_eq!(first["text"], "ab");
        assert_eq!(first["offset"], 2);
        let rest = read_log(&path, 2)?;
        assert_eq!(rest["text"], "€cd");
        assert_eq!(rest["offset"], 7);
        assert_eq!(read_log(&path, 100)?["offset"], 7);
        Ok(())
    }

    #[test]
    fn missing_log_keeps_requested_offset() -> Result<()> {
        let dir = tempfile::tempdir()?;
        assert_eq!(
            read_log(&dir.path().join("missing"), 9)?,
            json!({"text":"", "offset":9, "size":0})
        );
        Ok(())
    }

    #[test]
    fn public_log_reader_preserves_utf8_at_64k_boundary() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("job.log");
        let prefix = "x".repeat(CHUNK_SIZE - 1);
        let original = format!("{prefix}🦀tail");
        append(&path, &original)?;
        let first = read_log(&path, 0)?;
        assert_eq!(first["text"], prefix);
        assert_eq!(first["offset"], CHUNK_SIZE - 1);
        let second = read_log(&path, first["offset"].as_u64().unwrap())?;
        assert_eq!(second["text"], "🦀tail");
        assert_eq!(second["offset"], original.len());
        assert_eq!(second["size"], original.len());
        Ok(())
    }
}
