//! Directory handles: how the project capabilities touch the filesystem.
//!
//! On Unix every operation is relative to an open directory descriptor (`openat`, `fstatat`,
//! `renameat`, `mkdirat`, `unlinkat`), and every descriptor is opened with `O_NOFOLLOW`. A path
//! is walked one component at a time from the project root's descriptor, so:
//!
//! * no symbolic link, at any component, is ever followed, even one that appears after the walk
//!   checked the component (the open itself refuses it: there is no window between checking a
//!   path and using it);
//! * a directory that another process renames or replaces while a capability runs cannot redirect
//!   the operation out of the root: the walk holds the directory it checked, not its name;
//! * files are opened non-blocking and checked on the open descriptor, so a FIFO, socket or device
//!   is refused rather than read (a FIFO would block the work forever, a device would never end);
//! * a temporary file is created with its final permissions (`O_CREAT|O_EXCL`, then `fchmod`), so
//!   its content is never readable more widely than the file it replaces.
//!
//! Elsewhere the same API is implemented with paths and `symlink_metadata`, with the check and the
//! use as separate steps (the race the Unix implementation closes). The design follows Beacon's
//! `sessionfs` confinement (asymptote-labs/agent-beacon, MIT), which reads a home directory it does
//! not own through an `os.Root`.

use std::fs::File;
use std::io;

/// What a directory entry is, read without following a symbolic link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EntryKind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Entry {
    pub kind: EntryKind,
    pub len: u64,
    /// Hard links to the file. Always 1 where the platform does not say.
    pub links: u64,
    /// Permission bits, where the platform has them.
    pub mode: Option<u32>,
}

/// Why an open refused an entry that the walk could see.
pub(crate) fn not_a_regular_file() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "not a regular file")
}

#[cfg(unix)]
mod imp {
    use super::{Entry, EntryKind, not_a_regular_file};
    use std::fs::File;
    use std::io;
    use std::os::fd::OwnedFd;
    use std::path::Path;
    use std::sync::Arc;

    use rustix::fs::{AtFlags, FileType, Mode, OFlags, Stat};

    /// An open directory. Clones share the descriptor.
    #[derive(Debug, Clone)]
    pub(crate) struct Dir(Arc<OwnedFd>);

    // `st_nlink` and `st_mode` are narrower than u64/u32 on some platforms (u16 on macOS).
    #[allow(clippy::useless_conversion)]
    fn entry_of(stat: &Stat) -> Entry {
        let kind = match FileType::from_raw_mode(stat.st_mode as _) {
            FileType::RegularFile => EntryKind::File,
            FileType::Directory => EntryKind::Directory,
            FileType::Symlink => EntryKind::Symlink,
            _ => EntryKind::Other,
        };
        Entry {
            kind,
            len: stat.st_size.max(0) as u64,
            links: u64::from(stat.st_nlink),
            mode: Some(u32::from(stat.st_mode) & 0o7777),
        }
    }

    fn directory_flags() -> OFlags {
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
    }

    impl Dir {
        /// Opens the directory at `path` (the project root, which the operator chose; it may itself
        /// be reached through a symbolic link).
        pub(crate) fn open(path: &Path) -> io::Result<Self> {
            let fd = rustix::fs::open(
                path,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
                Mode::empty(),
            )?;
            Ok(Self(Arc::new(fd)))
        }

        /// `name`, one component, without following it. `None` when there is no such entry.
        pub(crate) fn entry(&self, name: &str) -> io::Result<Option<Entry>> {
            match rustix::fs::statat(&*self.0, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) => Ok(Some(entry_of(&stat))),
                Err(rustix::io::Errno::NOENT) => Ok(None),
                Err(e) => Err(e.into()),
            }
        }

        /// The subdirectory `name`. A symbolic link, or anything that is not a directory, is refused
        /// by the open itself.
        pub(crate) fn open_dir(&self, name: &str) -> io::Result<Self> {
            let fd = rustix::fs::openat(&*self.0, name, directory_flags(), Mode::empty())?;
            Ok(Self(Arc::new(fd)))
        }

        /// The regular file `name`, opened for reading, with what the open descriptor says it is.
        pub(crate) fn open_file(&self, name: &str) -> io::Result<(File, Entry)> {
            let fd = rustix::fs::openat(
                &*self.0,
                name,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )?;
            let entry = entry_of(&rustix::fs::fstat(&fd)?);
            if entry.kind != EntryKind::File {
                return Err(not_a_regular_file());
            }
            // Regular files ignore O_NONBLOCK; it only kept the open from waiting on a FIFO.
            Ok((File::from(fd), entry))
        }

        /// Creates `name`, which must not exist, for writing, with exactly `mode`.
        pub(crate) fn create_new(&self, name: &str, mode: u32) -> io::Result<File> {
            let mode = Mode::from_bits_truncate(mode as _);
            let fd = rustix::fs::openat(
                &*self.0,
                name,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                mode,
            )?;
            // The umask may have narrowed the mode; set it exactly, before any content is written.
            rustix::fs::fchmod(&fd, mode)?;
            Ok(File::from(fd))
        }

        /// Renames `from` over `to`, both in this directory.
        pub(crate) fn rename(&self, from: &str, to: &str) -> io::Result<()> {
            rustix::fs::renameat(&*self.0, from, &*self.0, to).map_err(Into::into)
        }

        pub(crate) fn remove_file(&self, name: &str) -> io::Result<()> {
            rustix::fs::unlinkat(&*self.0, name, AtFlags::empty()).map_err(Into::into)
        }

        /// Flushes the directory itself, so a rename in it survives a crash.
        pub(crate) fn sync(&self) -> io::Result<()> {
            rustix::fs::fsync(&*self.0).map_err(Into::into)
        }

        /// The entries of this directory (not `.` or `..`), each read without following it. A name
        /// that is not UTF-8 is `Err(())`.
        pub(crate) fn entries(&self) -> io::Result<Vec<(Result<String, ()>, Entry)>> {
            let mut out = Vec::new();
            for item in rustix::fs::Dir::read_from(&*self.0)? {
                let item = item?;
                let bytes = item.file_name().to_bytes();
                if bytes == b"." || bytes == b".." {
                    continue;
                }
                let stat =
                    match rustix::fs::statat(&*self.0, item.file_name(), AtFlags::SYMLINK_NOFOLLOW)
                    {
                        Ok(stat) => stat,
                        // Removed since the directory was read.
                        Err(rustix::io::Errno::NOENT) => continue,
                        Err(e) => return Err(e.into()),
                    };
                let name = std::str::from_utf8(bytes)
                    .map(str::to_string)
                    .map_err(|_| ());
                out.push((name, entry_of(&stat)));
            }
            Ok(out)
        }
    }

    /// Whether an error is the open refusing a symbolic link.
    pub(crate) fn is_symlink_refusal(e: &io::Error) -> bool {
        e.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error())
    }

    /// Whether an error is the open refusing a non-directory.
    pub(crate) fn is_not_a_directory(e: &io::Error) -> bool {
        e.raw_os_error() == Some(rustix::io::Errno::NOTDIR.raw_os_error())
    }

    /// A handle to lock the project by: the root directory itself, opened anew (its own open file
    /// description, so its lock is its own). Nothing is created, so a read-only checkout and a
    /// project whose `.stack-agent` is unusable can still be locked.
    pub(crate) fn lock_handle(root: &Path) -> io::Result<File> {
        let fd = rustix::fs::open(
            root,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        Ok(File::from(fd))
    }
}

#[cfg(not(unix))]
mod imp {
    use super::{Entry, EntryKind, not_a_regular_file};
    use std::fs::{self, File};
    use std::io;
    use std::path::{Path, PathBuf};

    /// A directory, by path. The check and the use are separate steps here.
    #[derive(Debug, Clone)]
    pub(crate) struct Dir(PathBuf);

    fn entry_of(m: &fs::Metadata) -> Entry {
        let kind = if m.file_type().is_symlink() {
            EntryKind::Symlink
        } else if m.is_file() {
            EntryKind::File
        } else if m.is_dir() {
            EntryKind::Directory
        } else {
            EntryKind::Other
        };
        Entry {
            kind,
            len: m.len(),
            links: 1,
            mode: None,
        }
    }

    fn symlink_refusal() -> io::Error {
        io::Error::other("symbolic link")
    }

    impl Dir {
        pub(crate) fn open(path: &Path) -> io::Result<Self> {
            if !fs::metadata(path)?.is_dir() {
                return Err(io::Error::other("not a directory"));
            }
            Ok(Self(path.to_path_buf()))
        }

        pub(crate) fn entry(&self, name: &str) -> io::Result<Option<Entry>> {
            match fs::symlink_metadata(self.0.join(name)) {
                Ok(m) => Ok(Some(entry_of(&m))),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e),
            }
        }

        pub(crate) fn open_dir(&self, name: &str) -> io::Result<Self> {
            match self.entry(name)? {
                Some(e) if e.kind == EntryKind::Directory => Ok(Self(self.0.join(name))),
                Some(e) if e.kind == EntryKind::Symlink => Err(symlink_refusal()),
                Some(_) => Err(io::Error::other("not a directory")),
                None => Err(io::ErrorKind::NotFound.into()),
            }
        }

        pub(crate) fn open_file(&self, name: &str) -> io::Result<(File, Entry)> {
            match self.entry(name)? {
                Some(e) if e.kind == EntryKind::File => Ok((File::open(self.0.join(name))?, e)),
                Some(e) if e.kind == EntryKind::Symlink => Err(symlink_refusal()),
                Some(_) => Err(not_a_regular_file()),
                None => Err(io::ErrorKind::NotFound.into()),
            }
        }

        pub(crate) fn create_new(&self, name: &str, _mode: u32) -> io::Result<File> {
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(self.0.join(name))
        }

        pub(crate) fn open_or_create(&self, name: &str, _mode: u32) -> io::Result<File> {
            if self.entry(name)?.is_some_and(|e| e.kind != EntryKind::File) {
                return Err(not_a_regular_file());
            }
            fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(self.0.join(name))
        }

        pub(crate) fn create_dir(&self, name: &str, _mode: u32) -> io::Result<()> {
            fs::create_dir(self.0.join(name))
        }

        pub(crate) fn rename(&self, from: &str, to: &str) -> io::Result<()> {
            fs::rename(self.0.join(from), self.0.join(to))
        }

        pub(crate) fn remove_file(&self, name: &str) -> io::Result<()> {
            fs::remove_file(self.0.join(name))
        }

        pub(crate) fn sync(&self) -> io::Result<()> {
            Ok(())
        }

        pub(crate) fn entries(&self) -> io::Result<Vec<(Result<String, ()>, Entry)>> {
            let mut out = Vec::new();
            for item in fs::read_dir(&self.0)? {
                let item = item?;
                let meta = match fs::symlink_metadata(item.path()) {
                    Ok(m) => m,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(e),
                };
                out.push((
                    item.file_name().into_string().map_err(|_| ()),
                    entry_of(&meta),
                ));
            }
            Ok(out)
        }
    }

    pub(crate) fn is_symlink_refusal(e: &io::Error) -> bool {
        e.to_string() == "symbolic link"
    }

    pub(crate) fn is_not_a_directory(e: &io::Error) -> bool {
        e.to_string() == "not a directory"
    }

    /// A directory cannot be locked here, so the lock is a file in the host-owned directory.
    pub(crate) fn lock_handle(root: &Path) -> io::Result<File> {
        let root = Dir::open(root)?;
        match root.create_dir(crate::STACK_AGENT_DIR, 0o700) {
            Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
            _ => {}
        }
        root.open_dir(crate::STACK_AGENT_DIR)?
            .open_or_create("work.lock", 0o600)
    }
}

pub(crate) use imp::{Dir, is_not_a_directory, is_symlink_refusal, lock_handle};

/// Reads at most `limit` bytes of an open file.
pub(crate) fn read_up_to(file: &mut File, limit: u64) -> io::Result<Vec<u8>> {
    use std::io::Read;
    let mut bytes = Vec::new();
    file.take(limit).read_to_end(&mut bytes)?;
    Ok(bytes)
}
