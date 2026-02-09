use std::path::{Path, PathBuf};

use crate::error::PostProcessError;

pub fn resolve_dest_dir(dest_dir: &Path, category: Option<&str>, append_category: bool) -> PathBuf {
    match (append_category, category) {
        (true, Some(cat)) if !cat.is_empty() => dest_dir.join(cat),
        _ => dest_dir.to_path_buf(),
    }
}

pub async fn move_to_destination(
    working_dir: &Path,
    dest_dir: &Path,
) -> Result<(), PostProcessError> {
    tokio::fs::create_dir_all(dest_dir).await?;

    let mut entries = tokio::fs::read_dir(working_dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let src = entry.path();
        let file_name = match src.file_name() {
            Some(n) => n.to_owned(),
            None => continue,
        };
        let dst = dest_dir.join(&file_name);
        move_path(&src, &dst).await?;
    }

    Ok(())
}

async fn move_path(src: &Path, dst: &Path) -> Result<(), PostProcessError> {
    match tokio::fs::rename(src, dst).await {
        Ok(()) => Ok(()),
        Err(e) if is_cross_device(&e) => {
            copy_recursive(src, dst).await?;
            remove_recursive(src).await?;
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

fn is_cross_device(e: &std::io::Error) -> bool {
    e.raw_os_error() == Some(libc_exdev())
}

#[cfg(target_os = "linux")]
fn libc_exdev() -> i32 {
    18
}

#[cfg(target_os = "macos")]
fn libc_exdev() -> i32 {
    18
}

#[cfg(target_os = "windows")]
fn libc_exdev() -> i32 {
    17
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn libc_exdev() -> i32 {
    18
}

async fn copy_recursive(src: &Path, dst: &Path) -> Result<(), PostProcessError> {
    if src.is_dir() {
        tokio::fs::create_dir_all(dst).await?;
        let mut entries = tokio::fs::read_dir(src).await?;
        while let Some(entry) = entries.next_entry().await? {
            let child_src = entry.path();
            let child_name = child_src.file_name().unwrap();
            let child_dst = dst.join(child_name);
            Box::pin(copy_recursive(&child_src, &child_dst)).await?;
        }
    } else {
        tokio::fs::copy(src, dst).await?;
    }
    Ok(())
}

async fn remove_recursive(path: &Path) -> Result<(), PostProcessError> {
    if path.is_dir() {
        tokio::fs::remove_dir_all(path).await?;
    } else {
        tokio::fs::remove_file(path).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_dest_dir_no_category() {
        let dest = resolve_dest_dir(Path::new("/data"), None, true);
        assert_eq!(dest, PathBuf::from("/data"));
    }

    #[test]
    fn resolve_dest_dir_with_category() {
        let dest = resolve_dest_dir(Path::new("/data"), Some("movies"), true);
        assert_eq!(dest, PathBuf::from("/data/movies"));
    }

    #[test]
    fn resolve_dest_dir_category_disabled() {
        let dest = resolve_dest_dir(Path::new("/data"), Some("movies"), false);
        assert_eq!(dest, PathBuf::from("/data"));
    }

    #[tokio::test]
    async fn move_to_destination_moves_files() {
        let src_dir = tempfile::tempdir().unwrap();
        let dst_dir = tempfile::tempdir().unwrap();
        let dst_path = dst_dir.path().join("output");

        std::fs::write(src_dir.path().join("movie.mkv"), b"video").unwrap();
        std::fs::write(src_dir.path().join("subs.srt"), b"subs").unwrap();

        move_to_destination(src_dir.path(), &dst_path)
            .await
            .unwrap();

        assert!(dst_path.join("movie.mkv").exists());
        assert!(dst_path.join("subs.srt").exists());
        let content = std::fs::read_to_string(dst_path.join("movie.mkv")).unwrap();
        assert_eq!(content, "video");
    }

    #[tokio::test]
    async fn move_to_destination_with_category() {
        let src_dir = tempfile::tempdir().unwrap();
        let dst_dir = tempfile::tempdir().unwrap();

        std::fs::write(src_dir.path().join("file.txt"), b"data").unwrap();

        let dest = resolve_dest_dir(dst_dir.path(), Some("tv"), true);
        move_to_destination(src_dir.path(), &dest).await.unwrap();

        assert!(dst_dir.path().join("tv").join("file.txt").exists());
    }
}
