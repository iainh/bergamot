use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub struct PidFile {
    path: PathBuf,
}

impl PidFile {
    pub fn create(path: &Path) -> Result<Self> {
        let pid = std::process::id();
        let mut f = fs::File::create(path)
            .with_context(|| format!("creating pidfile: {}", path.display()))?;
        write!(f, "{pid}").with_context(|| format!("writing pidfile: {}", path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
        })
    }

    #[cfg(test)]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for PidFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(unix)]
pub fn daemonize() -> Result<()> {
    unsafe {
        let pid = libc::fork();
        if pid < 0 {
            return Err(anyhow::anyhow!("fork failed"));
        }
        if pid > 0 {
            std::process::exit(0);
        }
        if libc::setsid() < 0 {
            return Err(anyhow::anyhow!("setsid failed"));
        }
        let pid2 = libc::fork();
        if pid2 < 0 {
            return Err(anyhow::anyhow!("second fork failed"));
        }
        if pid2 > 0 {
            std::process::exit(0);
        }
        let devnull = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
        if devnull >= 0 {
            libc::dup2(devnull, 0);
            libc::dup2(devnull, 1);
            libc::dup2(devnull, 2);
            if devnull > 2 {
                libc::close(devnull);
            }
        }
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn daemonize() -> Result<()> {
    Err(anyhow::anyhow!(
        "daemon mode is only supported on Unix systems"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pidfile_create_writes_current_pid() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("test.pid");

        let pidfile = PidFile::create(&path).expect("create pidfile");

        assert!(path.exists());
        let contents = fs::read_to_string(&path).expect("read pidfile");
        assert_eq!(contents, format!("{}", std::process::id()));
        assert_eq!(pidfile.path(), path);
    }

    #[test]
    fn pidfile_drop_removes_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("test.pid");

        {
            let _pidfile = PidFile::create(&path).expect("create pidfile");
            assert!(path.exists());
        }

        assert!(!path.exists(), "pidfile should be removed on drop");
    }

    #[test]
    fn pidfile_create_returns_error_for_invalid_path() {
        let result = PidFile::create(Path::new("/nonexistent/dir/test.pid"));
        assert!(result.is_err());
    }

    #[test]
    fn pidfile_path_returns_stored_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("test.pid");

        let pidfile = PidFile::create(&path).expect("create pidfile");
        assert_eq!(pidfile.path(), path);
    }
}
