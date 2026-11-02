//! 原子写（`SPEC-report.md` §4/§5：临时文件 + rename，多格式全成功或全无）。
//!
//! 流程：预检全部目标 → 同目录写临时文件并 `fsync` → 全部写成功后逐个 `rename` →
//! 任一失败清理临时文件与已改名文件，返回错误；绝不留下半个文件。

use crate::error::{ReportError, Result};
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// 一次导出的全部文件内容（文件名, 字节）。
pub(crate) type PendingFiles = Vec<(String, Vec<u8>)>;

/// 原子写出全部文件，返回最终路径（与输入同序）。
///
/// # Errors
///
/// 目录不可写、目标已存在且未允许覆盖、写盘/改名失败时返回
/// [`ReportError::Io`] / [`ReportError::OverwriteRefused`]，并尽力清理本次产物。
pub(crate) fn write_all(
    out_dir: &Path,
    files: &PendingFiles,
    allow_overwrite: bool,
) -> Result<Vec<PathBuf>> {
    if files.is_empty() {
        return Err(ReportError::Empty("没有需要写出的文件".to_owned()));
    }
    fs::create_dir_all(out_dir).map_err(io_error)?;
    let mut targets: Vec<PathBuf> = Vec::with_capacity(files.len());
    for (name, _) in files {
        validate_file_name(name)?;
        let target = out_dir.join(name);
        if !allow_overwrite && target.exists() {
            return Err(ReportError::OverwriteRefused(target.display().to_string()));
        }
        targets.push(target);
    }

    let pid = std::process::id();
    let mut temps: Vec<PathBuf> = Vec::with_capacity(files.len());
    let mut written: Vec<PathBuf> = Vec::with_capacity(files.len());
    for (index, (_, bytes)) in files.iter().enumerate() {
        let temp = out_dir.join(format!(".eg-report-{pid}-{index}.tmp"));
        let result = write_temp(&temp, bytes);
        if let Err(error) = result {
            cleanup(&temps);
            return Err(error);
        }
        temps.push(temp);
    }
    for (index, target) in targets.iter().enumerate() {
        let Some(temp) = temps.get(index) else {
            cleanup(&temps);
            cleanup(&written);
            return Err(ReportError::Io("临时文件索引缺失".to_owned()));
        };
        if let Err(error) = fs::rename(temp, target) {
            cleanup(&temps);
            cleanup(&written);
            return Err(io_error(error));
        }
        written.push(target.clone());
    }
    // 目录项落盘（尽力而为；失败不作为导出失败）。
    if let Ok(handle) = fs::File::open(out_dir) {
        let _ = handle.sync_all();
    }
    Ok(targets)
}

/// 删除指定文件（失败忽略；用于审计失败回滚）。
pub(crate) fn remove_files(paths: &[PathBuf]) {
    for path in paths {
        let _ = fs::remove_file(path);
    }
}

fn write_temp(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(io_error)?;
    file.write_all(bytes).map_err(io_error)?;
    file.flush().map_err(io_error)?;
    file.sync_all().map_err(io_error)?;
    Ok(())
}

fn cleanup(paths: &[PathBuf]) {
    for path in paths {
        let _ = fs::remove_file(path);
    }
}

fn validate_file_name(name: &str) -> Result<()> {
    let path = Path::new(name);
    if name.is_empty()
        || path.file_name().and_then(|value| value.to_str()) != Some(name)
        || name.contains(['/', '\\'])
    {
        return Err(ReportError::Naming(format!("文件名不合法：{name:?}")));
    }
    Ok(())
}

fn io_error(error: std::io::Error) -> ReportError {
    ReportError::Io(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        std::env::temp_dir().join(format!("eg-report-{tag}-{nanos}"))
    }

    #[test]
    fn writes_all_or_nothing() {
        let dir = temp_dir("atomic");
        let files = vec![
            ("a.xlsx".to_owned(), b"xlsx".to_vec()),
            ("b.csv".to_owned(), b"csv".to_vec()),
        ];
        let paths = write_all(&dir, &files, false).unwrap();
        assert_eq!(paths.len(), 2);
        assert_eq!(fs::read(&paths[0]).unwrap(), b"xlsx");
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn refuses_overwrite_by_default() {
        let dir = temp_dir("overwrite");
        let files = vec![("a.csv".to_owned(), b"1".to_vec())];
        write_all(&dir, &files, false).unwrap();
        let error = write_all(&dir, &files, false).unwrap_err();
        assert!(matches!(error, ReportError::OverwriteRefused(_)));
        write_all(&dir, &files, true).unwrap();
        let _ = fs::remove_dir_all(&dir);
    }
}
