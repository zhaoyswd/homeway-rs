use super::Record;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub(super) const FILE_BYTES: u64 = 8 * 1024 * 1024;
pub(super) const FILE_COUNT: usize = 4;

pub(super) fn path(dir: &Path, index: usize) -> PathBuf {
    if index == 0 {
        dir.join("quality.jsonl")
    } else {
        dir.join(format!("quality.jsonl.{index}"))
    }
}

pub(super) struct Store {
    dir: PathBuf,
    file: Option<File>,
    bytes: u64,
}

impl Store {
    pub(super) fn new(state: &Path) -> Self {
        Self {
            dir: state.join("cache/quality"),
            file: None,
            bytes: 0,
        }
    }

    fn open(&mut self) -> io::Result<()> {
        if fs::symlink_metadata(&self.dir).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "quality 目录不得为符号链接",
            ));
        }
        fs::create_dir_all(&self.dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            fs::set_permissions(&self.dir, fs::Permissions::from_mode(0o700))?;
            let f = OpenOptions::new()
                .read(true)
                .create(true)
                .append(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(path(&self.dir, 0))?;
            f.set_permissions(fs::Permissions::from_mode(0o600))?;
            self.bytes = f.metadata()?.len();
            self.file = Some(f);
        }
        #[cfg(not(unix))]
        {
            let f = OpenOptions::new()
                .read(true)
                .create(true)
                .append(true)
                .open(path(&self.dir, 0))?;
            self.bytes = f.metadata()?.len();
            self.file = Some(f);
        }
        // 先分隔上次崩溃留下的半行；保留其内容供查询明确报告坏行，不吞掉新样本。
        if self.bytes > 0 && self.bytes < FILE_BYTES {
            let f = self.file.as_mut().expect("已打开");
            f.seek(SeekFrom::End(-1))?;
            let mut byte = [0];
            f.read_exact(&mut byte)?;
            if byte[0] != b'\n' {
                f.write_all(b"\n")?;
                self.bytes += 1;
            }
        }
        Ok(())
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file.take();
        for i in (1..FILE_COUNT).rev() {
            let source = path(&self.dir, i - 1);
            let target = path(&self.dir, i);
            // Windows rename 不覆盖；删除失败除 NotFound 外均传播，避免无界增长。
            match fs::remove_file(&target) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
            match fs::rename(source, target) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        self.open()
    }

    pub(super) fn append(&mut self, record: &Record) -> io::Result<()> {
        let result = self.append_inner(record);
        if result.is_err() {
            self.file.take();
        }
        result
    }

    fn append_inner(&mut self, record: &Record) -> io::Result<()> {
        let mut line = serde_json::to_vec(record).map_err(io::Error::other)?;
        line.push(b'\n');
        if line.len() as u64 > FILE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "单条质量摘要超出文件容量",
            ));
        }
        if self.file.is_none() {
            self.open()?;
        }
        if self.bytes.saturating_add(line.len() as u64) > FILE_BYTES {
            self.rotate()?;
        }
        self.file
            .as_mut()
            .expect("打开或轮转成功")
            .write_all(&line)?;
        self.bytes += line.len() as u64;
        Ok(())
    }
}
