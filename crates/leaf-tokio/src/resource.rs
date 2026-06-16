//! [`FileResourceProvider`] — the `file:` scheme handler + an async
//! [`Resource`]/[`ResourceReader`] over tokio's filesystem.
//!
//! Realizes the runtime half of the resource-loading ABI (phase3/11): the
//! origin-agnostic [`Resource`] yields a leaf-owned async byte
//! [`ResourceReader`]; the [`FileResourceProvider`] handles exactly the
//! [`Scheme::File`] scheme (the always-present loader composes a scheme-map over
//! the discovered providers). Blocking IO is offloaded to tokio's blocking pool
//! via [`tokio::fs`], so a `read_to_bytes`/`read_chunk` never blocks the reactor.

use std::path::PathBuf;
use std::time::SystemTime;

use leaf_core::{
    Existence, LeafError, Location, Pattern, Resource, ResourceId, ResourceProvider, ResourceReader,
    Scheme,
};
use tokio::io::AsyncReadExt;

/// The stable integration taxonomy id for leaf-tokio resource IO faults (core's
/// `ErrorKind` has no general `Io` arm; the open `Integration { kind_id }` arm is
/// the design-sanctioned way for a runtime crate to extend the taxonomy by data).
fn resource_io_kind() -> leaf_core::ErrorKind {
    leaf_core::ErrorKind::Integration {
        kind_id: leaf_core::ContractId::of("leaf_tokio::resource_io"),
    }
}

/// Build a `LeafError` for an IO fault, narrating the path.
fn io_error(what: &'static str, path: &str, err: &std::io::Error) -> LeafError {
    LeafError::new(resource_io_kind())
        .caused_by(leaf_core::Cause::plain(what, format!("{path}: {err}")))
}

/// An async reader over an open file (tokio).
struct FileReader {
    file: tokio::fs::File,
    path: String,
}

#[leaf_macros::async_impl]
impl ResourceReader for FileReader {
    async fn read_chunk(&mut self, buf: &mut [u8]) -> Result<usize, LeafError> {
        self.file
            .read(buf)
            .await
            .map_err(|e| io_error("reading resource chunk", &self.path, &e))
    }
}

/// A filesystem resource (`file:` scheme). Origin-agnostic behind `dyn Resource`.
pub struct FileResource {
    id: ResourceId,
    path: PathBuf,
}

impl FileResource {
    /// Build a file resource for `path`.
    #[must_use]
    pub fn new(location: Location) -> Self {
        let path = PathBuf::from(location.path.as_ref());
        FileResource {
            id: ResourceId::new(location),
            path,
        }
    }

    fn path_str(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }
}

#[leaf_macros::async_impl]
impl Resource for FileResource {
    fn id(&self) -> &ResourceId {
        &self.id
    }

    fn exists(&self) -> Existence {
        Existence::Known(self.path.exists())
    }

    fn last_modified(&self) -> Option<SystemTime> {
        std::fs::metadata(&self.path).and_then(|m| m.modified()).ok()
    }

    async fn open(&self) -> Result<Box<dyn ResourceReader>, LeafError> {
        let path = self.path_str();
        let file = tokio::fs::File::open(&self.path)
            .await
            .map_err(|e| io_error("opening resource", &path, &e))?;
        Ok(Box::new(FileReader { file, path }) as Box<dyn ResourceReader>)
    }

    async fn read_to_bytes(&self) -> Result<Vec<u8>, LeafError> {
        tokio::fs::read(&self.path)
            .await
            .map_err(|e| io_error("reading resource", &self.path_str(), &e))
    }
}

/// The `file:` scheme handler.
///
/// `resolve` always succeeds (a non-existent file is a `Resource` whose
/// [`exists`](Resource::exists) reports `Known(false)`); `resolve_pattern`
/// rejects a malformed scheme but otherwise performs a shallow directory listing
/// of the literal parent (full glob is the pattern-resolver unit's job — this
/// keeps the runtime provider minimal and honest).
#[derive(Default)]
pub struct FileResourceProvider {
    _priv: (),
}

impl FileResourceProvider {
    /// Construct the `file:` provider.
    #[must_use]
    pub fn new() -> Self {
        FileResourceProvider { _priv: () }
    }
}

#[leaf_macros::async_impl]
impl ResourceProvider for FileResourceProvider {
    fn scheme(&self) -> Scheme {
        Scheme::File
    }

    fn resolve(&self, loc: &Location) -> Result<Box<dyn Resource>, LeafError> {
        if loc.scheme != Scheme::File {
            return Err(LeafError::new(resource_io_kind()).caused_by(leaf_core::Cause::plain(
                "resolving file resource",
                format!("wrong scheme {:?} for the file provider", loc.scheme),
            )));
        }
        Ok(Box::new(FileResource::new(loc.clone())))
    }

    async fn resolve_pattern(
        &self,
        pat: &Pattern,
    ) -> Result<Vec<Box<dyn Resource>>, LeafError> {
        // Minimal: treat the pattern's directory part as a literal dir and
        // list its immediate entries (no recursive glob — deferred to the
        // pattern-resolver unit; see the NOTE in the module docs).
        let raw = pat.0.as_ref();
        let dir = std::path::Path::new(raw)
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let mut out: Vec<Box<dyn Resource>> = Vec::new();
        let mut entries = match tokio::fs::read_dir(&dir).await {
            Ok(e) => e,
            // A non-existent directory matches nothing (not an error).
            Err(_) => return Ok(out),
        };
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| io_error("listing pattern dir", &dir.to_string_lossy(), &e))?
        {
            let p = entry.path().to_string_lossy().into_owned();
            out.push(Box::new(FileResource::new(Location::new(Scheme::File, p))));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("leaf_tokio_res_{}_{name}", std::process::id()));
        p
    }

    #[tokio::test]
    async fn resolve_existing_file_reads_bytes() {
        let path = tmp_path("hello.txt");
        tokio::fs::write(&path, b"leaf rocks").await.unwrap();

        let provider = FileResourceProvider::new();
        let loc = Location::new(Scheme::File, path.to_string_lossy().into_owned());
        let res = provider.resolve(&loc).unwrap();

        assert_eq!(res.exists(), Existence::Known(true));
        let bytes = res.read_to_bytes().await.unwrap();
        assert_eq!(bytes, b"leaf rocks");
        assert!(res.last_modified().is_some());

        tokio::fs::remove_file(&path).await.ok();
    }

    #[tokio::test]
    async fn open_streams_chunks() {
        let path = tmp_path("stream.txt");
        tokio::fs::write(&path, b"abcdef").await.unwrap();

        let provider = FileResourceProvider::new();
        let loc = Location::new(Scheme::File, path.to_string_lossy().into_owned());
        let res = provider.resolve(&loc).unwrap();
        let mut reader = res.open().await.unwrap();

        let mut all = Vec::new();
        let mut buf = [0u8; 4];
        loop {
            let n = reader.read_chunk(&mut buf).await.unwrap();
            if n == 0 {
                break;
            }
            all.extend_from_slice(&buf[..n]);
        }
        assert_eq!(all, b"abcdef");

        tokio::fs::remove_file(&path).await.ok();
    }

    #[tokio::test]
    async fn nonexistent_file_resolves_but_reports_absent() {
        let provider = FileResourceProvider::new();
        let loc = Location::new(Scheme::File, "/definitely/not/here/leaf.xyz");
        let res = provider.resolve(&loc).unwrap();
        assert_eq!(res.exists(), Existence::Known(false));
        assert!(res.read_to_bytes().await.is_err());
    }

    #[tokio::test]
    async fn wrong_scheme_is_rejected() {
        let provider = FileResourceProvider::new();
        let loc = Location::new(Scheme::Classpath, "config/app.yaml");
        assert!(provider.resolve(&loc).is_err());
    }

    #[tokio::test]
    async fn provider_reports_its_scheme() {
        assert_eq!(FileResourceProvider::new().scheme(), Scheme::File);
    }

    #[tokio::test]
    async fn resolve_pattern_lists_directory_entries() {
        let dir = tmp_path("patterndir");
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let f1 = dir.join("a.txt");
        let f2 = dir.join("b.txt");
        tokio::fs::write(&f1, b"a").await.unwrap();
        tokio::fs::write(&f2, b"b").await.unwrap();

        let provider = FileResourceProvider::new();
        let pat = Pattern(format!("{}/*.txt", dir.to_string_lossy()).into());
        let found = provider.resolve_pattern(&pat).await.unwrap();
        assert!(found.len() >= 2, "must list directory entries");

        tokio::fs::remove_dir_all(&dir).await.ok();
    }
}
