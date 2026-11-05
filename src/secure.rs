use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::Path,
};

use anyhow::{Context, Result, ensure};

pub fn uid() -> u32 {
    unsafe { libc::geteuid() }
}

pub fn check_directory(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.is_dir() && meta.uid() == uid() && meta.mode() & 0o077 == 0,
        "Unsafe application directory: {} (expected owned directory with mode 0700)",
        path.display()
    );
    // Resolve OS aliases such as /tmp -> /private/tmp, then validate the parent.
    let parent = path
        .parent()
        .context("Application directory needs a parent")?;
    let canonical = parent.canonicalize()?;
    ensure!(
        canonical.metadata()?.is_dir(),
        "Invalid application parent directory"
    );
    for ancestor in canonical.ancestors() {
        let meta = ancestor.metadata()?;
        ensure!(
            meta.uid() == 0 || meta.uid() == uid(),
            "Application parent belongs to another user: {}",
            ancestor.display()
        );
        ensure!(
            meta.mode() & 0o022 == 0 || meta.mode() & 0o1000 != 0,
            "Application parent is writable by other users: {}",
            ancestor.display()
        );
    }
    Ok(())
}

pub fn create_directory(path: &Path) -> Result<()> {
    if !path.exists() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
    }
    check_directory(path)
}

pub fn private_file(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let meta = file.metadata()?;
    ensure!(
        meta.is_file() && meta.uid() == uid() && meta.nlink() == 1,
        "Unsafe application file: {}",
        path.display()
    );
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

pub fn check_socket(path: &Path) -> Result<()> {
    use std::os::unix::fs::FileTypeExt;
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.file_type().is_socket() && meta.uid() == uid() && meta.mode() & 0o077 == 0,
        "Unsafe IPC socket: {}",
        path.display()
    );
    Ok(())
}
