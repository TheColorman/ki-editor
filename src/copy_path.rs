use std::{
    fs,
    io::ErrorKind,
    path::{Component, Path, PathBuf},
};

use anyhow::{bail, ensure, Context};

/// Copies a single path to an exact destination, without following source symlinks.
/// Directories require a new destination; regular files retain `fs::copy` semantics.
/// Returns the destination to reveal in the explorer.
pub(crate) fn copy_path(from: &Path, to: &Path) -> anyhow::Result<PathBuf> {
    copy_path_inner(from, to).with_context(|| {
        format!(
            "Failed to duplicate '{}' to '{}'",
            from.display(),
            to.display()
        )
    })
}

fn copy_path_inner(from: &Path, to: &Path) -> anyhow::Result<PathBuf> {
    let file_type = fs::symlink_metadata(from)?.file_type();
    let destination = if file_type.is_dir() {
        let source = from.canonicalize()?;
        let resolved = resolve_destination(to)?;
        ensure!(
            !resolved.starts_with(&source),
            "Cannot duplicate a directory into itself or one of its descendants"
        );
        normalize_destination(to, &resolved)?
    } else {
        to.to_path_buf()
    };
    let to = destination.as_path();
    if file_type.is_dir() {
        ensure_new_destination(to)?;
        create_parent(to)?;
        // Only clean up after successfully creating our own destination directory.
        fs::create_dir(to)?;
        if let Err(error) = copy_directory_contents(from, to) {
            return match fs::remove_dir_all(to) {
                Ok(()) => Err(error),
                Err(cleanup_error) => Err(error.context(format!(
                    "Could not remove partial copy '{}': {cleanup_error}",
                    to.display()
                ))),
            };
        }
    } else if file_type.is_symlink() {
        ensure_new_destination(to)?;
        create_parent(to)?;
        copy_symlink(from, to)?;
    } else if file_type.is_file() {
        if let Ok(destination) = to.canonicalize() {
            ensure!(
                destination != from.canonicalize()?,
                "Cannot duplicate a file onto itself"
            );
        }
        create_parent(to)?;
        fs::copy(from, to)?;
    } else {
        bail!("Only files, directories, and symbolic links can be duplicated");
    }
    Ok(destination)
}

/// Resolve existing symlinks before processing `..`, including when later path
/// components do not exist yet. `AbsolutePath` does not perform this normalization.
fn resolve_destination(path: &Path) -> anyhow::Result<PathBuf> {
    std::path::absolute(path)?
        .components()
        .try_fold(PathBuf::new(), |mut resolved, component| {
            match component {
                Component::ParentDir => {
                    resolved.pop();
                }
                Component::CurDir => {}
                _ => {
                    resolved.push(component.as_os_str());
                    match fs::symlink_metadata(&resolved) {
                        Ok(_) => resolved = resolved.canonicalize()?,
                        Err(error) if error.kind() == ErrorKind::NotFound => {}
                        Err(error) => return Err(error.into()),
                    }
                }
            }
            Ok(resolved)
        })
}

/// Remove parent components before creating anything, so `source/new/../../copy`
/// cannot create `source/new`. Retain workspace aliases when the lexical path
/// names the same destination; symlink/`..` combinations may require the resolved
/// path instead to preserve filesystem traversal semantics.
fn normalize_destination(path: &Path, resolved: &Path) -> anyhow::Result<PathBuf> {
    let normalized = std::path::absolute(path)?.components().fold(
        PathBuf::new(),
        |mut normalized, component| {
            match component {
                Component::ParentDir => {
                    normalized.pop();
                }
                Component::CurDir => {}
                _ => normalized.push(component.as_os_str()),
            }
            normalized
        },
    );
    match resolve_destination(&normalized) {
        Ok(candidate) if candidate == resolved => Ok(normalized),
        _ => Ok(resolved.to_path_buf()),
    }
}

fn ensure_new_destination(path: &Path) -> anyhow::Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
        Ok(_) => bail!("The destination '{}' already exists", path.display()),
    }
}

fn create_parent(path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn copy_directory_contents(from: &Path, to: &Path) -> anyhow::Result<()> {
    fs::read_dir(from)?.try_for_each(|entry| {
        let entry = entry?;
        let source = entry.path();
        let destination = to.join(entry.file_name());
        (|| {
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                copy_symlink(&source, &destination)?;
            } else if file_type.is_dir() {
                fs::create_dir(&destination)?;
                copy_directory_contents(&source, &destination)?;
            } else if file_type.is_file() {
                fs::copy(&source, &destination)?;
            } else {
                bail!("Only files, directories, and symbolic links can be duplicated");
            }
            Ok(())
        })()
        .with_context(|| {
            format!(
                "Failed to copy '{}' to '{}'",
                source.display(),
                destination.display()
            )
        })
    })
}

fn copy_symlink(from: &Path, to: &Path) -> anyhow::Result<()> {
    let target = fs::read_link(from)?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, to)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::{symlink_dir, symlink_file, FileTypeExt};
        if fs::symlink_metadata(from)?.file_type().is_symlink_dir() {
            symlink_dir(target, to)?;
        } else {
            symlink_file(target, to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copy_path_copies_complete_directory_tree() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let source = temp.path().join("source");
        let destination = temp.path().join("new/parents/copy");
        fs::create_dir_all(source.join("nested/empty"))?;
        fs::write(source.join("nested/binary"), [0, 255, 1, 128])?;
        fs::write(source.join(".hidden"), "hidden")?;
        fs::write(source.join(".gitignore"), "ignored\n")?;
        fs::write(source.join("ignored"), "ignored content")?;

        copy_path(&source, &destination)?;

        assert!(destination.join("nested/empty").is_dir());
        assert_eq!(
            fs::read(destination.join("nested/binary"))?,
            [0, 255, 1, 128]
        );
        assert_eq!(fs::read_to_string(destination.join(".hidden"))?, "hidden");
        assert_eq!(
            fs::read_to_string(destination.join(".gitignore"))?,
            "ignored\n"
        );
        assert_eq!(
            fs::read_to_string(destination.join("ignored"))?,
            "ignored content"
        );
        assert!(!destination.join("source").exists());
        assert!(source.join("nested/binary").is_file());
        Ok(())
    }

    #[test]
    fn copy_path_copies_empty_directory() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let source = temp.path().join("source");
        let destination = temp.path().join("copy");
        fs::create_dir(&source)?;
        copy_path(&source, &destination)?;
        assert!(destination.is_dir());
        assert_eq!(fs::read_dir(destination)?.count(), 0);
        Ok(())
    }

    #[test]
    fn copy_path_copies_directory_with_parent_components() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let source = temp.path().join("source");
        fs::create_dir(&source)?;
        let destination = copy_path(&source, &temp.path().join("missing/../copy"))?;
        assert_eq!(destination, temp.path().join("copy"));
        assert!(destination.is_dir());
        assert!(!temp.path().join("missing").exists());
        Ok(())
    }

    #[test]
    fn copy_path_does_not_create_cancelled_parents_inside_source() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let source = temp.path().join("source");
        fs::create_dir(&source)?;
        fs::write(source.join("file"), "content")?;
        let destination = copy_path(&source, &source.join("new/../../copy"))?;
        assert_eq!(destination, temp.path().join("copy"));
        assert!(!source.join("new").exists());
        assert!(!destination.join("new").exists());
        assert_eq!(fs::read_to_string(destination.join("file"))?, "content");
        Ok(())
    }

    #[test]
    fn copy_path_rejects_existing_directory_destinations() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let source = temp.path().join("source");
        fs::create_dir(&source)?;
        let directory = temp.path().join("directory");
        fs::create_dir(&directory)?;
        fs::write(directory.join("keep"), "untouched")?;
        let file = temp.path().join("file");
        fs::write(&file, "untouched")?;

        assert!(copy_path(&source, &directory).is_err());
        assert!(copy_path(&source, &file).is_err());
        assert_eq!(fs::read_to_string(directory.join("keep"))?, "untouched");
        assert_eq!(fs::read_to_string(file)?, "untouched");
        Ok(())
    }

    #[test]
    fn copy_path_rejects_self_and_descendants_before_creating_parents() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let source = temp.path().join("source");
        fs::create_dir(&source)?;
        for destination in [
            source.clone(),
            source.join("child"),
            source.join("new/nested/copy"),
            source.join("../source/new/copy"),
            source.join("new/../copy"),
        ] {
            assert!(copy_path(&source, &destination).is_err());
        }
        assert_eq!(fs::read_dir(source)?.count(), 0);
        Ok(())
    }

    #[test]
    fn copy_path_preserves_regular_file_behavior() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let source = temp.path().join("source");
        let destination = temp.path().join("new/copy");
        fs::write(&source, [0, 255, 1])?;
        copy_path(&source, &destination)?;
        assert_eq!(fs::read(&destination)?, [0, 255, 1]);
        fs::write(&source, "replacement")?;
        copy_path(&source, &destination)?;
        assert_eq!(fs::read(destination)?, b"replacement");
        assert!(copy_path(&source, &source).is_err());
        assert_eq!(fs::read(source)?, b"replacement");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn copy_path_preserves_symlinks_without_following_them() -> anyhow::Result<()> {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir()?;
        let source = temp.path().join("source");
        let destination = temp.path().join("copy");
        fs::create_dir(&source)?;
        fs::write(source.join("file"), "content")?;
        symlink("file", source.join("link"))?;
        symlink(".", source.join("cycle"))?;
        symlink("missing", source.join("dangling"))?;
        symlink(temp.path(), source.join("outside"))?;

        copy_path(&source, &destination)?;

        for name in ["link", "cycle", "dangling", "outside"] {
            assert!(fs::symlink_metadata(destination.join(name))?
                .file_type()
                .is_symlink());
            assert_eq!(
                fs::read_link(destination.join(name))?,
                fs::read_link(source.join(name))?
            );
        }
        let link_copy = temp.path().join("link-copy");
        copy_path(&source.join("dangling"), &link_copy)?;
        assert_eq!(fs::read_link(link_copy)?, Path::new("missing"));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn copy_path_rejects_descendants_through_symlink_aliases() -> anyhow::Result<()> {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir()?;
        let source = temp.path().join("source");
        fs::create_dir_all(source.join("nested"))?;
        let alias = temp.path().join("alias");
        symlink(source.join("nested"), &alias)?;
        assert!(copy_path(&source, &alias.join("new/copy")).is_err());
        // Resolving `..` lexically before the symlink would miss this descendant.
        assert!(copy_path(&source, &alias.join("../new/copy")).is_err());
        assert!(!source.join("new").exists());
        assert!(!source.join("nested/new").exists());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn copy_path_preserves_symlink_parent_traversal_semantics() -> anyhow::Result<()> {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir()?;
        let source = temp.path().join("source");
        fs::create_dir(&source)?;
        fs::write(source.join("file"), "content")?;
        fs::create_dir_all(temp.path().join("elsewhere/nested"))?;
        let alias = temp.path().join("alias");
        symlink(temp.path().join("elsewhere/nested"), &alias)?;

        let destination = copy_path(&source, &alias.join("../copy"))?;

        assert_eq!(
            destination,
            temp.path().canonicalize()?.join("elsewhere/copy")
        );
        assert_eq!(fs::read_to_string(destination.join("file"))?, "content");
        assert!(!temp.path().join("copy").exists());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn copy_path_rejects_existing_symlink_destinations() -> anyhow::Result<()> {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir()?;
        let source = temp.path().join("source");
        fs::create_dir(&source)?;
        let destination = temp.path().join("copy");
        symlink("missing", &destination)?;
        assert!(copy_path(&source, &destination).is_err());
        assert_eq!(fs::read_link(destination)?, Path::new("missing"));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn copy_path_cleans_up_partial_directories_on_failure() -> anyhow::Result<()> {
        use std::os::unix::net::UnixListener;

        let temp = tempfile::tempdir()?;
        let source = temp.path().join("source");
        fs::create_dir_all(source.join("nested"))?;
        fs::write(source.join("nested/file"), "content")?;
        let _socket = UnixListener::bind(source.join("socket"))?;
        let destination = temp.path().join("copy");
        let error = copy_path(&source, &destination).unwrap_err();
        assert!(format!("{error:#}").contains("socket"));
        assert!(!destination.exists());
        assert_eq!(fs::read_to_string(source.join("nested/file"))?, "content");
        Ok(())
    }
}
