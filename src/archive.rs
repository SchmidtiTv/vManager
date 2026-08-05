//! Creating `.7z` archives from a folder.
//!
//! Compression is pure Rust ([`sevenz_rust2`]) — no `7z` binary needs to be
//! installed. Archives use LZMA2 in solid mode, which is what 7-Zip itself
//! produces by default, in blocks of [`MAX_BLOCK_BYTES`].
//!
//! Entry names are relative to the *parent* of the source folder, so
//! archiving `Blender\4.1` yields an archive containing `4.1/…` — the same
//! layout as `7z a 4.1.7z 4.1`, so a version restores under its own name.

use std::any::Any;
use std::panic::AssertUnwindSafe;
use std::path::{Component, Path, PathBuf};

use sevenz_rust2::{ArchiveEntry, ArchiveWriter, SourceReader, encoder_options::Lzma2Options};

use crate::error::{Error, Result};

/// Extension used for every archive this module writes.
pub const EXTENSION: &str = "7z";

/// Compression level accepted by LZMA2.
pub const MIN_LEVEL: u32 = 0;
pub const MAX_LEVEL: u32 = 9;
/// 7-Zip's own default (`-mx=6`).
pub const DEFAULT_LEVEL: u32 = 6;

/// Uncompressed bytes each worker thread takes at a time.
///
/// Only used when `threads > 1`; larger chunks compress better, smaller ones
/// parallelize better. Clamped up to the dictionary size by the encoder.
const MT_CHUNK_SIZE: u64 = 64 * 1024 * 1024;

/// Uncompressed bytes in one solid block, i.e. 7-Zip's `-ms=1g`.
///
/// Each block is compressed by its own encoder, and the size is what keeps any
/// one of them away from a bug in `lzma-rust2` 0.18, which `sevenz-rust2` uses
/// to compress. Its match finder tracks positions in an `i32` and renormalizes
/// its lookup tables once a stream has seen `i32::MAX` bytes (~2 GiB). The
/// scalar renormalization bottoms out at `i32::MIN` (`saturating_sub`) where
/// the SIMD paths beside it clamp at zero, so stale table entries end up
/// holding positions near -2^31. A match distance computed from one of those
/// overflows past `i32::MAX`, wraps negative, passes the "inside the
/// dictionary" bound check, and the encoder indexes off the front of its
/// window — an out-of-bounds panic, part-way through any folder over ~2 GiB.
///
/// `sevenz-rust2`'s own solid blocks are 4 GiB, which is past that; ours are
/// under it. Smaller blocks compress slightly worse, since matches cannot reach
/// back across a boundary.
///
/// This does not cover a *single file* over ~2 GiB: one file is never split
/// across blocks, so it still goes through one encoder, and the panic is
/// contained by [`catch_panics`] rather than avoided. Raise this back to the
/// library's 4 GiB once `lzma-rust2` clamps its scalar renormalization at zero.
const MAX_BLOCK_BYTES: u64 = 1024 * 1024 * 1024;

/// How an archive should be built.
#[derive(Debug, Clone)]
pub struct Options {
    /// LZMA2 compression level, 0 (fastest) to 9 (smallest).
    pub level: u32,
    /// Worker threads. More threads trade compression ratio for speed.
    pub threads: u32,
    /// File and directory names to skip, matched against each path component.
    pub exclude: Vec<String>,
    /// Overwrite the destination if it already exists.
    pub overwrite: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            level: DEFAULT_LEVEL,
            threads: 1,
            exclude: Vec::new(),
            overwrite: false,
        }
    }
}

/// What a completed archive run produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    /// The folder that was archived.
    pub source: PathBuf,
    /// The archive that was written.
    pub archive: PathBuf,
    /// Number of files stored.
    pub file_count: u64,
    /// Total size of those files before compression.
    pub source_bytes: u64,
    /// Size of the archive on disk.
    pub archive_bytes: u64,
}

/// Archive `source` into `dest`.
///
/// `source` must be a directory. The destination's parent directories are
/// created as needed.
pub fn create(source: &Path, dest: &Path, options: &Options) -> Result<Summary> {
    let source = canonical_dir(source)?;
    let dest = normalized(dest)?;

    if dest.exists() && !options.overwrite {
        return Err(Error::Archive(format!(
            "`{}` already exists; pass --force to overwrite it",
            dest.display()
        )));
    }

    // Writing the archive into the folder being archived would race the walk
    // against its own output.
    if dest.starts_with(&source) {
        return Err(Error::Archive(format!(
            "destination `{}` is inside the folder being archived; use --output to write it elsewhere",
            dest.display()
        )));
    }

    let (file_count, source_bytes) = measure(&source, options)?;
    if file_count == 0 {
        return Err(Error::Archive(format!(
            "`{}` contains no files to archive",
            source.display()
        )));
    }

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }

    tracing::info!(
        source = %source.display(),
        dest = %dest.display(),
        files = file_count,
        level = options.level,
        "creating archive"
    );

    let written = catch_panics(&source, || {
        write_archive(&source, &dest, options, MAX_BLOCK_BYTES)
    });

    match written {
        Ok(()) => {}
        Err(err) => {
            // A half-written archive is worse than none: it looks usable.
            if let Err(cleanup) = std::fs::remove_file(&dest)
                && cleanup.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(path = %dest.display(), error = %cleanup, "could not remove partial archive");
            }
            return Err(err);
        }
    }

    let archive_bytes = std::fs::metadata(&dest)?.len();

    Ok(Summary {
        source,
        archive: dest,
        file_count,
        source_bytes,
        archive_bytes,
    })
}

/// Render a byte count for humans: `1.2 MiB`.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];

    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }

    if unit == 0 {
        format!("{bytes} {}", UNITS[0])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Run the compressor, turning a panic inside it into an ordinary error.
///
/// Compression is a pure-Rust library running in this process, so a bug in it
/// — see [`MAX_BLOCK_BYTES`] for the one that prompted this — would otherwise
/// take the whole sweep down with it, leaving the half-written archive on disk
/// looking finished. Contained here it is one failed folder: the partial
/// archive is removed by the caller and the run carries on.
///
/// Requires the release profile to unwind rather than abort.
fn catch_panics<T>(source: &Path, compress: impl FnOnce() -> Result<T>) -> Result<T> {
    std::panic::catch_unwind(AssertUnwindSafe(compress)).unwrap_or_else(|payload| {
        Err(Error::Archive(format!(
            "compressing `{}` panicked: {}; the source folder was left alone",
            source.display(),
            panic_message(&*payload)
        )))
    })
}

/// The text a panic carried, for the payloads `panic!` actually produces.
fn panic_message(payload: &(dyn Any + Send)) -> &str {
    if let Some(text) = payload.downcast_ref::<&'static str>() {
        text
    } else if let Some(text) = payload.downcast_ref::<String>() {
        text
    } else {
        "no message"
    }
}

/// Compress the tree. Split out so `create` can clean up after a failure.
///
/// `block_bytes` is how much uncompressed data goes into one solid block. See
/// [`MAX_BLOCK_BYTES`].
fn write_archive(source: &Path, dest: &Path, options: &Options, block_bytes: u64) -> Result<()> {
    // Names are relative to the parent, which is what keeps the folder name in
    // the archive; only `source` itself is walked, so no sibling can get in.
    let root = source.parent().unwrap_or(source);
    let files = walk(source, options)?;

    let mut writer = ArchiveWriter::create(dest).map_err(archive_error)?;
    writer.set_content_methods(vec![lzma2_options(options).into()]);

    let (mut entries, mut readers) = (Vec::new(), Vec::new());
    let mut pending = 0u64;

    for file in files {
        if file.path == dest {
            continue;
        }

        // The file that would push this block past the limit starts the next
        // one. A file bigger than the limit on its own gets a block to itself.
        if !entries.is_empty() && pending + file.size > block_bytes {
            push_block(&mut writer, &mut entries, &mut readers)?;
            pending = 0;
        }

        let name = file
            .path
            .strip_prefix(root)
            .unwrap_or(&file.path)
            .to_string_lossy()
            .to_string();
        entries.push(ArchiveEntry::from_path(&file.path, name));
        readers.push(LazyFile::new(file.path).into());
        pending += file.size;
    }

    if !entries.is_empty() {
        push_block(&mut writer, &mut entries, &mut readers)?;
    }

    // `finish` flushes the 7z footer; a failure here means a truncated file.
    writer.finish()?;
    Ok(())
}

/// Compress one solid block, leaving the buffers empty for the next.
fn push_block<W: std::io::Write + std::io::Seek>(
    writer: &mut ArchiveWriter<W>,
    entries: &mut Vec<ArchiveEntry>,
    readers: &mut Vec<SourceReader<LazyFile>>,
) -> Result<()> {
    tracing::debug!(files = entries.len(), "compressing block");
    writer
        .push_archive_entries(std::mem::take(entries), std::mem::take(readers))
        .map_err(archive_error)?;
    Ok(())
}

/// A file opened on first read.
///
/// A block holds every file in it at once, and a version folder can hold
/// thousands, which is more file handles than the process is entitled to.
struct LazyFile {
    path: PathBuf,
    file: Option<std::fs::File>,
    done: bool,
}

impl LazyFile {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            file: None,
            done: false,
        }
    }
}

impl std::io::Read for LazyFile {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.done {
            return Ok(0);
        }

        let file = match &mut self.file {
            Some(file) => file,
            none => none.insert(std::fs::File::open(&self.path)?),
        };

        let read = file.read(buf)?;
        if read == 0 {
            self.done = true;
            self.file = None;
        }
        Ok(read)
    }
}

fn lzma2_options(options: &Options) -> Lzma2Options {
    let level = options.level.clamp(MIN_LEVEL, MAX_LEVEL);
    if options.threads > 1 {
        Lzma2Options::from_level_mt(level, options.threads, MT_CHUNK_SIZE)
    } else {
        Lzma2Options::from_level(level)
    }
}

/// One file to be stored.
struct SourceFile {
    path: PathBuf,
    size: u64,
}

/// Every file under `source` that is not excluded, sorted by path.
///
/// Sorted because `read_dir` order is the filesystem's business: without it the
/// same tree could be split into blocks differently from one run to the next,
/// and neighbouring files — which are the ones worth having in a block together
/// — would land wherever.
///
/// Symlinks are skipped: `file_type` does not follow them, so a link is neither
/// stored as a copy of its target nor followed into a loop.
fn walk(source: &Path, options: &Options) -> Result<Vec<SourceFile>> {
    let mut stack = vec![source.to_path_buf()];
    let mut files = Vec::new();

    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if is_excluded(&path, source, &options.exclude) {
                tracing::debug!(path = %path.display(), "excluded");
                continue;
            }

            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file() {
                files.push(SourceFile {
                    path,
                    size: entry.metadata()?.len(),
                });
            }
        }
    }

    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

/// Count the files that will be stored and their total size.
///
/// Walked separately from the compression pass so the summary is known up
/// front; the cost is one extra `stat` per file.
fn measure(source: &Path, options: &Options) -> Result<(u64, u64)> {
    let files = walk(source, options)?;
    let bytes = files.iter().map(|file| file.size).sum();

    Ok((files.len() as u64, bytes))
}

/// Whether any path component below `root` matches an exclude pattern.
fn is_excluded(path: &Path, root: &Path, exclude: &[String]) -> bool {
    if exclude.is_empty() {
        return false;
    }

    let relative = path.strip_prefix(root).unwrap_or(path);
    relative.components().any(|component| match component {
        Component::Normal(name) => exclude.iter().any(|pattern| name == pattern.as_str()),
        _ => false,
    })
}

/// Resolve `path` and confirm it is a directory we can archive.
///
/// Also the way a folder is spelled before it goes into the database, so that
/// `.\Tools\Blender` and `C:\Tools\Blender` are recognised as the same
/// registration.
pub fn canonical_dir(path: &Path) -> Result<PathBuf> {
    let canonical = path.canonicalize().map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            Error::NotFound {
                kind: "folder",
                name: path.display().to_string(),
            }
        } else {
            Error::Io(err)
        }
    })?;

    if !canonical.is_dir() {
        return Err(Error::Archive(format!(
            "`{}` is not a folder",
            path.display()
        )));
    }

    Ok(simplified(canonical))
}

/// Resolve a path that need not exist yet into the same shape
/// [`canonical_dir`] produces, so the two can be compared.
///
/// `canonicalize` only works on existing paths, so this canonicalizes the
/// deepest ancestor that does exist and re-appends the rest. Without it the
/// two sides can spell the same location differently — on Windows a canonical
/// path is extended-length (`\\?\C:\…`) and may expand an 8.3 short name
/// (`RUNNER~1` -> `runneradmin`), so a plain `starts_with` misses.
fn normalized(path: &Path) -> Result<PathBuf> {
    let absolute = absolute(path)?;

    let mut trailing = Vec::new();
    let mut candidate = absolute.as_path();

    loop {
        if let Ok(canonical) = candidate.canonicalize() {
            let mut resolved = simplified(canonical);
            for name in trailing.iter().rev() {
                resolved.push(name);
            }
            return Ok(resolved);
        }

        let (Some(parent), Some(name)) = (candidate.parent(), candidate.file_name()) else {
            // Nothing along the path exists; the absolute form is the best we
            // can do (and is what the comparison will see on both sides).
            return Ok(absolute);
        };
        trailing.push(name.to_os_string());
        candidate = parent;
    }
}

/// Absolute form of a path that need not exist yet.
fn absolute(path: &Path) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

/// Drop Windows' `\\?\` extended-length prefix so paths stay readable in
/// output and in the database. Only plain drive paths are rewritten; UNC and
/// device paths keep the prefix, because removing it there changes meaning.
#[cfg(windows)]
fn simplified(path: PathBuf) -> PathBuf {
    use std::path::{Component, Prefix};

    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return path;
    };

    match prefix.kind() {
        Prefix::VerbatimDisk(letter) => {
            let mut simplified = PathBuf::from(format!("{}:\\", letter as char));
            simplified.extend(components.filter(|part| !matches!(part, Component::RootDir)));
            simplified
        }
        _ => path,
    }
}

#[cfg(not(windows))]
fn simplified(path: PathBuf) -> PathBuf {
    path
}

fn archive_error(err: sevenz_rust2::Error) -> Error {
    Error::Archive(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a small tree: `a.txt`, `sub/b.txt`, `.git/config`.
    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("photos");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("a.txt"), "hello").unwrap();
        std::fs::write(root.join("sub").join("b.txt"), "world!").unwrap();
        std::fs::write(root.join(".git").join("config"), "junk").unwrap();
        dir
    }

    #[test]
    fn archives_a_folder() {
        let dir = fixture();
        let source = dir.path().join("photos");
        let dest = dir.path().join("out").join("photos.7z");

        let summary = create(&source, &dest, &Options::default()).unwrap();

        assert!(dest.is_file());
        assert_eq!(summary.file_count, 3);
        assert_eq!(summary.source_bytes, 5 + 6 + 4);
        assert_eq!(
            summary.archive_bytes,
            std::fs::metadata(&dest).unwrap().len()
        );
    }

    #[test]
    fn writes_a_real_7z_container() {
        let dir = fixture();
        let dest = dir.path().join("photos.7z");
        create(&dir.path().join("photos"), &dest, &Options::default()).unwrap();

        // 7z signature: '7z' BC AF 27 1C
        let bytes = std::fs::read(&dest).unwrap();
        assert_eq!(&bytes[..6], &[b'7', b'z', 0xBC, 0xAF, 0x27, 0x1C]);
    }

    #[test]
    fn the_archive_round_trips_with_the_folder_name_intact() {
        let dir = fixture();
        let dest = dir.path().join("photos.7z");
        create(&dir.path().join("photos"), &dest, &Options::default()).unwrap();

        let out = dir.path().join("restored");
        sevenz_rust2::decompress_file(&dest, &out).unwrap();

        // Entry names are relative to the source's parent, so the folder
        // itself is part of every path — same as `7z a photos.7z photos`.
        assert_eq!(
            std::fs::read_to_string(out.join("photos/a.txt")).unwrap(),
            "hello"
        );
        assert_eq!(
            std::fs::read_to_string(out.join("photos/sub/b.txt")).unwrap(),
            "world!"
        );
        assert_eq!(
            std::fs::read_to_string(out.join("photos/.git/config")).unwrap(),
            "junk"
        );
    }

    #[test]
    fn excluded_files_are_absent_from_the_archive() {
        let dir = fixture();
        let dest = dir.path().join("photos.7z");
        let options = Options {
            exclude: vec![".git".to_string(), "b.txt".to_string()],
            ..Options::default()
        };
        create(&dir.path().join("photos"), &dest, &options).unwrap();

        let out = dir.path().join("restored");
        sevenz_rust2::decompress_file(&dest, &out).unwrap();

        assert!(out.join("photos/a.txt").exists());
        assert!(!out.join("photos/.git").exists());
        assert!(!out.join("photos/sub/b.txt").exists());
    }

    #[test]
    fn exclusions_are_matched_by_component() {
        let dir = fixture();
        let options = Options {
            exclude: vec![".git".to_string()],
            ..Options::default()
        };

        let summary = create(
            &dir.path().join("photos"),
            &dir.path().join("photos.7z"),
            &options,
        )
        .unwrap();

        assert_eq!(summary.file_count, 2, "the .git folder should be skipped");
        assert_eq!(summary.source_bytes, 5 + 6);
    }

    #[test]
    fn refuses_to_overwrite_without_force() {
        let dir = fixture();
        let source = dir.path().join("photos");
        let dest = dir.path().join("photos.7z");

        create(&source, &dest, &Options::default()).unwrap();
        assert!(matches!(
            create(&source, &dest, &Options::default()),
            Err(Error::Archive(_))
        ));

        let overwrite = Options {
            overwrite: true,
            ..Options::default()
        };
        create(&source, &dest, &overwrite).unwrap();
    }

    #[test]
    fn refuses_to_write_into_the_source_folder() {
        let dir = fixture();
        let source = dir.path().join("photos");

        let err = create(&source, &source.join("self.7z"), &Options::default()).unwrap_err();
        assert!(err.to_string().contains("inside the folder"), "{err}");
    }

    /// The destination guard has to survive the source and destination being
    /// spelled differently. A symlink is the portable stand-in for what
    /// Windows does natively with `\\?\` prefixes and 8.3 short names.
    #[cfg(unix)]
    #[test]
    fn refuses_to_write_into_the_source_folder_by_another_name() {
        let dir = fixture();
        let source = dir.path().join("photos");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&source, &link).unwrap();

        let err = create(&source, &link.join("self.7z"), &Options::default()).unwrap_err();
        assert!(err.to_string().contains("inside the folder"), "{err}");
    }

    #[test]
    fn rejects_missing_and_non_directory_sources() {
        let dir = fixture();

        assert!(matches!(
            create(
                &dir.path().join("absent"),
                &dir.path().join("x.7z"),
                &Options::default()
            ),
            Err(Error::NotFound { .. })
        ));

        assert!(matches!(
            create(
                &dir.path().join("photos").join("a.txt"),
                &dir.path().join("x.7z"),
                &Options::default()
            ),
            Err(Error::Archive(_))
        ));
    }

    #[test]
    fn rejects_an_empty_folder() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("empty");
        std::fs::create_dir(&source).unwrap();

        let err = create(&source, &dir.path().join("x.7z"), &Options::default()).unwrap_err();
        assert!(err.to_string().contains("no files"), "{err}");
    }

    /// `size` bytes that compress well, but only with a dictionary reaching
    /// back further than one block: one pseudo-random block repeated.
    fn repetitive(size: usize, block: usize) -> Vec<u8> {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut seed = vec![0u8; block];
        for chunk in seed.chunks_mut(8) {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let bytes = state.to_le_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }

        seed.iter().copied().cycle().take(size).collect()
    }

    /// Splitting into blocks is what keeps each encoder away from the overflow
    /// in [`MAX_BLOCK_BYTES`], so a multi-block archive has to read back byte
    /// for byte. Testing at the real block size would mean compressing a
    /// gigabyte, so the test uses a small one.
    #[test]
    fn a_multi_block_archive_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("big");
        std::fs::create_dir(&source).unwrap();

        // Four files of 512 KiB, each the same block repeated, so a match that
        // reaches back across a block boundary is worth a lot and its absence
        // shows up in the archive size below.
        let content = repetitive(512 * 1024, 128 * 1024);
        for index in 0..4 {
            std::fs::write(source.join(format!("part{index}.bin")), &content).unwrap();
        }

        let options = Options {
            level: 0,
            ..Options::default()
        };
        let split = dir.path().join("split.7z");
        write_archive(&source, &split, &options, 512 * 1024).unwrap();

        let out = dir.path().join("restored");
        sevenz_rust2::decompress_file(&split, &out).unwrap();
        for index in 0..4 {
            assert_eq!(
                std::fs::read(out.join(format!("big/part{index}.bin"))).unwrap(),
                content,
                "part{index}.bin came back different"
            );
        }

        // Without this the test would pass just as well if everything had gone
        // into one block. Each block starts with an empty dictionary, so four
        // of them cost four copies of the data the single block stores once.
        let single = dir.path().join("single.7z");
        write_archive(&source, &single, &options, MAX_BLOCK_BYTES).unwrap();
        let (split_bytes, single_bytes) = (
            std::fs::metadata(&split).unwrap().len(),
            std::fs::metadata(&single).unwrap().len(),
        );
        assert!(
            split_bytes > single_bytes,
            "the block size did not take effect: {split_bytes} bytes split vs {single_bytes} in one block"
        );
    }

    /// A file of its own bigger than the block size still has to be stored,
    /// alone in a block rather than split across two.
    #[test]
    fn a_file_larger_than_a_block_is_stored_whole() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("big");
        std::fs::create_dir(&source).unwrap();
        let content = repetitive(1024 * 1024, 128 * 1024);
        std::fs::write(source.join("huge.bin"), &content).unwrap();
        std::fs::write(source.join("small.txt"), "after the huge one").unwrap();

        let dest = dir.path().join("out.7z");
        let options = Options {
            level: 0,
            ..Options::default()
        };
        write_archive(&source, &dest, &options, 64 * 1024).unwrap();

        let out = dir.path().join("restored");
        sevenz_rust2::decompress_file(&dest, &out).unwrap();
        assert_eq!(std::fs::read(out.join("big/huge.bin")).unwrap(), content);
        assert_eq!(
            std::fs::read_to_string(out.join("big/small.txt")).unwrap(),
            "after the huge one"
        );
    }

    /// A panic in the compressor has to come back as a failure for this one
    /// folder. Compressing 2 GiB to provoke the real one is not a test, so this
    /// covers the containment itself.
    #[test]
    fn a_panic_while_compressing_becomes_an_error() {
        let err = catch_panics(Path::new("/tools/blender/4.1"), || -> Result<()> {
            panic!("index out of bounds: the len is 12853521")
        })
        .unwrap_err();

        assert!(matches!(err, Error::Archive(_)));
        let message = err.to_string();
        assert!(message.contains("panicked"), "{message}");
        assert!(message.contains("12853521"), "{message}");
        assert!(message.contains("4.1"), "{message}");
    }

    #[test]
    fn byte_sizes_are_human_readable() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(3 * 1024 * 1024), "3.0 MiB");
    }
}
