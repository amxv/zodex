//! Minimal drop-in for the local filesystem types `codex-apply-patch` uses.
//!
//! The original `codex-exec-server` is a large remote-process service with
//! WebSockets, transport, credentials, and sandboxing. Zodex's apply_patch
//! operates directly on its own host with ordinary OS permissions, so pulling
//! that server into every Zodex build adds no useful functionality.
//!
//! Keep the *upstream Codex apply-patch crate unmodified*. This adapter is
//! intentionally not a general replacement for the Codex execution server.

use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, LazyLock};

use codex_utils_path_uri::PathUri;

pub type ExecutorFileSystemFuture<'a, T> = Pin<Box<dyn Future<Output = io::Result<T>> + Send + 'a>>;

/// Zodex only supports an unsandboxed local executor here. Reject, rather
/// than silently ignore, any future sandbox context we weren't designed for.
#[derive(Debug)]
pub struct FileSystemSandboxContext;

#[derive(Clone, Copy, Debug, Default)]
pub struct ReadFileOptions {
    pub follow_symlinks: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct WriteFileOptions {
    pub follow_symlinks: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct GetMetadataOptions {
    pub follow_symlinks: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct CreateDirectoryOptions {
    pub recursive: bool,
    pub follow_symlinks: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct RemoveOptions {
    pub recursive: bool,
    pub force: bool,
    pub follow_symlinks: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct FileMetadata {
    pub is_file: bool,
    pub is_directory: bool,
    pub is_symlink: bool,
}

/// Only the five filesystem operations exercised by upstream apply-patch.
pub trait ExecutorFileSystem: Send + Sync {
    fn read_file_text<'a>(
        &'a self,
        path: &'a PathUri,
        options: ReadFileOptions,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, String>;

    fn write_file<'a>(
        &'a self,
        path: &'a PathUri,
        contents: Vec<u8>,
        options: WriteFileOptions,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()>;

    fn get_metadata<'a>(
        &'a self,
        path: &'a PathUri,
        options: GetMetadataOptions,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, FileMetadata>;

    fn create_directory<'a>(
        &'a self,
        path: &'a PathUri,
        options: CreateDirectoryOptions,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()>;

    fn remove<'a>(
        &'a self,
        path: &'a PathUri,
        options: RemoveOptions,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()>;
}

pub static LOCAL_FS: LazyLock<Arc<dyn ExecutorFileSystem>> =
    LazyLock::new(|| Arc::new(LocalFileSystem));

struct LocalFileSystem;

fn host_path(path: &PathUri, follow_symlinks: bool) -> io::Result<PathBuf> {
    let path = path.to_path_buf();
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "patch filesystem paths must be absolute host paths",
        ));
    }
    if !follow_symlinks {
        // The upstream executor implements no-follow using native file-handle
        // operations. A check-then-open approximation here would be racy, so
        // reject this unsupported mode rather than weakening its contract.
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "no-follow patch filesystem mode requires the full Codex executor",
        ));
    }
    Ok(path)
}

fn check_sandbox(sandbox: Option<&FileSystemSandboxContext>) -> io::Result<()> {
    if sandbox.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Zodex's patch filesystem does not implement Codex sandbox contexts",
        ));
    }
    Ok(())
}

impl ExecutorFileSystem for LocalFileSystem {
    fn read_file_text<'a>(
        &'a self,
        path: &'a PathUri,
        options: ReadFileOptions,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, String> {
        Box::pin(async move {
            check_sandbox(sandbox)?;
            tokio::fs::read_to_string(host_path(path, options.follow_symlinks)?).await
        })
    }

    fn write_file<'a>(
        &'a self,
        path: &'a PathUri,
        contents: Vec<u8>,
        options: WriteFileOptions,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        Box::pin(async move {
            check_sandbox(sandbox)?;
            tokio::fs::write(host_path(path, options.follow_symlinks)?, contents).await
        })
    }

    fn get_metadata<'a>(
        &'a self,
        path: &'a PathUri,
        options: GetMetadataOptions,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, FileMetadata> {
        Box::pin(async move {
            check_sandbox(sandbox)?;
            let path = host_path(path, options.follow_symlinks)?;
            // Mirror Codex's direct filesystem: report both the resolved
            // target type and whether the final component is a symlink.
            // This matters for the upstream partial-write evidence's
            // `is_exact` flag, including updates through symlinks.
            let link_metadata = tokio::fs::symlink_metadata(&path).await?;
            let is_symlink = link_metadata.file_type().is_symlink();
            let metadata = if is_symlink {
                tokio::fs::metadata(&path).await?
            } else {
                link_metadata
            };
            Ok(FileMetadata {
                is_file: metadata.is_file(),
                is_directory: metadata.is_dir(),
                is_symlink,
            })
        })
    }

    fn create_directory<'a>(
        &'a self,
        path: &'a PathUri,
        options: CreateDirectoryOptions,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        Box::pin(async move {
            check_sandbox(sandbox)?;
            let path = host_path(path, options.follow_symlinks)?;
            if options.recursive {
                tokio::fs::create_dir_all(path).await
            } else {
                tokio::fs::create_dir(path).await
            }
        })
    }

    fn remove<'a>(
        &'a self,
        path: &'a PathUri,
        options: RemoveOptions,
        sandbox: Option<&'a FileSystemSandboxContext>,
    ) -> ExecutorFileSystemFuture<'a, ()> {
        Box::pin(async move {
            check_sandbox(sandbox)?;
            let path = host_path(path, options.follow_symlinks)?;
            let result = match tokio::fs::symlink_metadata(&path).await {
                Ok(metadata) if metadata.is_dir() && options.recursive => {
                    tokio::fs::remove_dir_all(path).await
                }
                Ok(metadata) if metadata.is_dir() => tokio::fs::remove_dir(path).await,
                Ok(_) => tokio::fs::remove_file(path).await,
                Err(error) => Err(error),
            };
            match result {
                Err(err) if options.force && err.kind() == io::ErrorKind::NotFound => Ok(()),
                result => result,
            }
        })
    }
}
