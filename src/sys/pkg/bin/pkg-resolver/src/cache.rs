// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{queue, repository::Repository, repository_manager::Stats},
    fidl::endpoints::ServerEnd,
    fidl_fuchsia_io::DirectoryMarker,
    fidl_fuchsia_pkg::PackageCacheProxy,
    fidl_fuchsia_pkg_ext::{BlobId, MirrorConfig, RepositoryConfig},
    fuchsia_syslog::{fx_log_err, fx_log_info},
    fuchsia_trace as trace,
    fuchsia_url::pkg_url::PkgUrl,
    fuchsia_zircon::Status,
    futures::{
        compat::{Future01CompatExt, Stream01CompatExt},
        lock::Mutex as AsyncMutex,
        prelude::*,
        stream::FuturesUnordered,
    },
    http::Uri,
    hyper::{body::Payload, Body, Request, StatusCode},
    parking_lot::Mutex,
    pkgfs::install::BlobKind,
    std::{
        collections::HashSet,
        hash::Hash,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc,
        },
    },
    thiserror::Error,
    tuf::metadata::TargetPath,
};

mod retry;

pub type BlobFetcher = queue::WorkSender<BlobId, FetchBlobContext, Result<(), Arc<FetchError>>>;

/// Provides access to the package cache components.
#[derive(Clone)]
pub struct PackageCache {
    cache: PackageCacheProxy,
    pkgfs_install: pkgfs::install::Client,
    pkgfs_needs: pkgfs::needs::Client,
}

impl PackageCache {
    /// Constructs a new [`PackageCache`].
    pub fn new(
        cache: PackageCacheProxy,
        pkgfs_install: pkgfs::install::Client,
        pkgfs_needs: pkgfs::needs::Client,
    ) -> Self {
        Self { cache, pkgfs_install, pkgfs_needs }
    }

    /// Open the requested package by merkle root using the given selectors, serving the package
    /// directory on the given directory request on success.
    pub async fn open(
        &self,
        merkle: BlobId,
        selectors: &Vec<String>,
        dir_request: ServerEnd<DirectoryMarker>,
    ) -> Result<(), PackageOpenError> {
        let fut = self.cache.open(
            &mut merkle.into(),
            &mut selectors.iter().map(|s| s.as_str()),
            dir_request,
        );
        match Status::from_raw(fut.await?) {
            Status::OK => Ok(()),
            Status::NOT_FOUND => Err(PackageOpenError::NotFound),
            status => Err(PackageOpenError::UnexpectedStatus(status)),
        }
    }

    /// Check to see if a package with the given merkle root exists and is readable.
    pub async fn package_exists(&self, merkle: BlobId) -> Result<bool, PackageOpenError> {
        let (_dir, server_end) = fidl::endpoints::create_proxy()?;
        let selectors = vec![];
        match self.open(merkle, &selectors, server_end).await {
            Ok(()) => Ok(true),
            Err(PackageOpenError::NotFound) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Create a new blob with the given install intent.
    ///
    /// Returns None if the blob already exists and is readable.
    async fn create_blob(
        &self,
        merkle: BlobId,
        blob_kind: BlobKind,
    ) -> Result<
        Option<(pkgfs::install::Blob<pkgfs::install::NeedsTruncate>, pkgfs::install::BlobCloser)>,
        pkgfs::install::BlobCreateError,
    > {
        match self.pkgfs_install.create_blob(merkle.into(), blob_kind).await {
            Ok((file, closer)) => Ok(Some((file, closer))),
            Err(pkgfs::install::BlobCreateError::AlreadyExists) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Returns a stream of chunks of blobs that are needed to resolve the package specified by
    /// `pkg_merkle` provided that the `pkg_merkle` blob has previously been written to
    /// /pkgfs/install/pkg/. The package should be available in /pkgfs/versions when this stream
    /// terminates without error.
    fn list_needs(
        &self,
        pkg_merkle: BlobId,
    ) -> impl Stream<Item = Result<HashSet<BlobId>, pkgfs::needs::ListNeedsError>> + '_ {
        self.pkgfs_needs
            .list_needs(pkg_merkle.into())
            .map(|item| item.map(|needs| needs.into_iter().map(Into::into).collect()))
    }
}

#[derive(Debug, Error)]
pub enum PackageOpenError {
    #[error("fidl error: {}", _0)]
    Fidl(fidl::Error),

    #[error("package not found")]
    NotFound,

    #[error("package cache returned unexpected status: {}", _0)]
    UnexpectedStatus(Status),
}

impl From<fidl::Error> for PackageOpenError {
    fn from(err: fidl::Error) -> Self {
        Self::Fidl(err)
    }
}

impl From<PackageOpenError> for Status {
    fn from(x: PackageOpenError) -> Self {
        match x {
            PackageOpenError::NotFound => Status::NOT_FOUND,
            _ => Status::INTERNAL,
        }
    }
}

pub async fn cache_package<'a>(
    repo: Arc<AsyncMutex<Repository>>,
    config: &'a RepositoryConfig,
    url: &'a PkgUrl,
    cache: &'a PackageCache,
    blob_fetcher: &'a BlobFetcher,
) -> Result<BlobId, CacheError> {
    let (merkle, size) = merkle_for_url(repo, url).await.map_err(CacheError::MerkleFor)?;
    // If a merkle pin was specified, use it, but only after having verified that the name and
    // variant exist in the TUF repo.  Note that this doesn't guarantee that the merkle pinned
    // package ever actually existed in the repo or that the merkle pin refers to the named
    // package.
    let merkle = if let Some(merkle_pin) = url.package_hash() {
        merkle_pin.parse().expect("package_hash() to always return a valid merkleroot")
    } else {
        merkle
    };

    // If the package already exists, we are done.
    if cache.package_exists(merkle).await.unwrap_or_else(|e| {
        fx_log_err!("unable to check if {} is already cached, assuming it isn't: {}", url, e);
        false
    }) {
        return Ok(merkle);
    }

    let mirrors = config.mirrors().to_vec().into();

    // Fetch the meta.far.
    blob_fetcher
        .push(
            merkle,
            FetchBlobContext {
                blob_kind: BlobKind::Package,
                mirrors: Arc::clone(&mirrors),
                expected_len: Some(size),
            },
        )
        .await
        .expect("processor exists")?;

    cache
        .list_needs(merkle)
        .err_into::<CacheError>()
        .try_for_each(|needs| {
            // Fetch the blobs with some amount of concurrency.
            fx_log_info!("Fetching blobs: {:#?}", needs);
            blob_fetcher
                .push_all(needs.into_iter().map(|need| {
                    (
                        need,
                        FetchBlobContext {
                            blob_kind: BlobKind::Data,
                            mirrors: Arc::clone(&mirrors),
                            expected_len: None,
                        },
                    )
                }))
                .collect::<FuturesUnordered<_>>()
                .map(|res| res.expect("processor exists"))
                .try_collect::<()>()
                .err_into()
        })
        .await?;

    Ok(merkle)
}

#[derive(Debug, Error)]
pub enum CacheError {
    #[error("fidl error: {}", _0)]
    Fidl(fidl::Error),

    #[error("while looking up merkle root for package: {}", _0)]
    MerkleFor(MerkleForError),

    #[error("while listing needed blobs for package: {}", _0)]
    ListNeeds(pkgfs::needs::ListNeedsError),

    #[error("while fetching blobs for package: {}", _0)]
    Fetch(Arc<FetchError>),
}

impl From<pkgfs::needs::ListNeedsError> for CacheError {
    fn from(x: pkgfs::needs::ListNeedsError) -> Self {
        CacheError::ListNeeds(x)
    }
}

impl From<fidl::Error> for CacheError {
    fn from(x: fidl::Error) -> Self {
        Self::Fidl(x)
    }
}

impl From<Arc<FetchError>> for CacheError {
    fn from(x: Arc<FetchError>) -> Self {
        Self::Fetch(x)
    }
}

pub(crate) trait ToResolveStatus {
    fn to_resolve_status(&self) -> Status;
}

// From resolver.fidl:
// * `ZX_ERR_ACCESS_DENIED` if the resolver does not have permission to fetch a package blob.
// * `ZX_ERR_IO` if there is some other unspecified error during I/O.
// * `ZX_ERR_NOT_FOUND` if the package or a package blob does not exist.
// * `ZX_ERR_NO_SPACE` if there is no space available to store the package.
// * `ZX_ERR_UNAVAILABLE` if the resolver is currently unable to fetch a package blob.
impl ToResolveStatus for CacheError {
    fn to_resolve_status(&self) -> Status {
        match self {
            CacheError::Fidl(_) => Status::IO,
            CacheError::MerkleFor(err) => err.to_resolve_status(),
            CacheError::ListNeeds(err) => err.to_resolve_status(),
            CacheError::Fetch(err) => err.to_resolve_status(),
        }
    }
}
impl ToResolveStatus for MerkleForError {
    fn to_resolve_status(&self) -> Status {
        match self {
            MerkleForError::NotFound => Status::NOT_FOUND,
            MerkleForError::InvalidTargetPath(_) => Status::INTERNAL,
            // FIXME(42326) when tuf::Error gets an HTTP error variant, this should be mapped to Status::UNAVAILABLE
            MerkleForError::TufError(_) => Status::INTERNAL,
            MerkleForError::NoCustomMetadata => Status::INTERNAL,
            MerkleForError::SerdeError(_) => Status::INTERNAL,
        }
    }
}
impl ToResolveStatus for pkgfs::needs::ListNeedsError {
    fn to_resolve_status(&self) -> Status {
        match self {
            pkgfs::needs::ListNeedsError::OpenDir(_) => Status::IO,
            pkgfs::needs::ListNeedsError::ReadDir(_) => Status::IO,
            pkgfs::needs::ListNeedsError::ParseError(_) => Status::INTERNAL,
        }
    }
}
impl ToResolveStatus for pkgfs::install::BlobTruncateError {
    fn to_resolve_status(&self) -> Status {
        match self {
            pkgfs::install::BlobTruncateError::Fidl(_) => Status::IO,
            pkgfs::install::BlobTruncateError::UnexpectedResponse(_) => Status::IO,
        }
    }
}
impl ToResolveStatus for pkgfs::install::BlobWriteError {
    fn to_resolve_status(&self) -> Status {
        match self {
            pkgfs::install::BlobWriteError::Fidl(_) => Status::IO,
            pkgfs::install::BlobWriteError::Overwrite => Status::IO,
            pkgfs::install::BlobWriteError::UnexpectedResponse(_) => Status::IO,
        }
    }
}
impl ToResolveStatus for FetchError {
    fn to_resolve_status(&self) -> Status {
        match self {
            FetchError::CreateBlob(_) => Status::IO,
            FetchError::BadHttpStatus { code: hyper::StatusCode::UNAUTHORIZED, .. } => {
                Status::ACCESS_DENIED
            }
            FetchError::BadHttpStatus { code: hyper::StatusCode::FORBIDDEN, .. } => {
                Status::ACCESS_DENIED
            }
            FetchError::BadHttpStatus { .. } => Status::UNAVAILABLE,
            FetchError::ContentLengthMismatch { .. } => Status::UNAVAILABLE,
            FetchError::UnknownLength { .. } => Status::UNAVAILABLE,
            FetchError::BlobTooSmall { .. } => Status::UNAVAILABLE,
            FetchError::BlobTooLarge { .. } => Status::UNAVAILABLE,
            FetchError::Hyper { .. } => Status::UNAVAILABLE,
            FetchError::Http { .. } => Status::UNAVAILABLE,
            FetchError::Truncate(e) => e.to_resolve_status(),
            FetchError::Write(e) => e.to_resolve_status(),
            FetchError::NoMirrors => Status::INTERNAL,
            FetchError::BlobUrl(_) => Status::INTERNAL,
        }
    }
}

pub async fn merkle_for_url<'a>(
    repo: Arc<AsyncMutex<Repository>>,
    url: &'a PkgUrl,
) -> Result<(BlobId, u64), MerkleForError> {
    let target_path =
        TargetPath::new(format!("{}/{}", url.name().unwrap(), url.variant().unwrap_or("0")))
            .map_err(MerkleForError::InvalidTargetPath)?;
    let mut repo = repo.lock().await;
    let res = repo.get_merkle_at_path(&target_path).await;
    res.map(|custom| (custom.merkle(), custom.size()))
}

#[derive(Debug, Error)]
pub enum MerkleForError {
    #[error("the package was not found in the repository")]
    NotFound,

    #[error("tuf returned an unexpected error: {}", _0)]
    TufError(tuf::error::Error),

    #[error("the target path is not safe: {}", _0)]
    InvalidTargetPath(tuf::error::Error),

    #[error("the target description does not have custom metadata")]
    NoCustomMetadata,

    #[error("serde value could not be converted: {}", _0)]
    SerdeError(serde_json::Error),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FetchBlobContext {
    blob_kind: BlobKind,
    mirrors: Arc<[MirrorConfig]>,
    expected_len: Option<u64>,
}

impl queue::TryMerge for FetchBlobContext {
    fn try_merge(&mut self, other: Self) -> Result<(), Self> {
        // Unmergeable if both contain different expected lengths. One of these instances will
        // fail, but we can't know which one here.
        let expected_len = match (self.expected_len, other.expected_len) {
            (Some(x), None) | (None, Some(x)) => Some(x),
            (None, None) => None,
            (Some(x), Some(y)) if x == y => Some(x),
            _ => return Err(other),
        };

        // Installing a blob as a package will fulfill any pending needs of that blob as a data
        // blob as well, so upgrade Data to Package.
        let blob_kind =
            if self.blob_kind == BlobKind::Package || other.blob_kind == BlobKind::Package {
                BlobKind::Package
            } else {
                BlobKind::Data
            };

        // For now, don't attempt to merge mirrors, but do merge these contexts if the mirrors are
        // equivalent.
        if self.mirrors != other.mirrors {
            return Err(other);
        }

        // Contexts are mergeable, apply the merged state.
        self.expected_len = expected_len;
        self.blob_kind = blob_kind;
        Ok(())
    }
}

pub fn make_blob_fetch_queue(
    cache: PackageCache,
    max_concurrency: usize,
    stats: Arc<Mutex<Stats>>,
) -> (impl Future<Output = ()>, BlobFetcher) {
    let http_client = Arc::new(fuchsia_hyper::new_https_client());

    let (blob_fetch_queue, blob_fetcher) = queue::work_queue(
        max_concurrency,
        move |merkle: BlobId, context: FetchBlobContext| {
            let http_client = Arc::clone(&http_client);
            let cache = cache.clone();
            let stats = Arc::clone(&stats);

            async move {
                trace::duration_begin!("app", "fetch_blob", "merkle" => merkle.to_string().as_str());
                let res = fetch_blob(
                    &*http_client,
                    &context.mirrors,
                    merkle,
                    context.blob_kind,
                    context.expected_len,
                    &cache,
                    stats,
                )
                .map_err(Arc::new)
                .await;
                trace::duration_end!("app", "fetch_blob", "result" => format!("{:?}", res).as_str());
                res
            }
        },
    );

    (blob_fetch_queue.into_future(), blob_fetcher)
}

async fn fetch_blob(
    client: &fuchsia_hyper::HttpsClient,
    mirrors: &[MirrorConfig],
    merkle: BlobId,
    blob_kind: BlobKind,
    expected_len: Option<u64>,
    cache: &PackageCache,
    stats: Arc<Mutex<Stats>>,
) -> Result<(), FetchError> {
    if mirrors.is_empty() {
        return Err(FetchError::NoMirrors);
    }

    // TODO try the other mirrors depending on the errors encountered trying this one.
    let blob_mirror_url = mirrors[0].blob_mirror_url();
    let mirror_stats = stats.lock().for_mirror(blob_mirror_url.to_owned());
    let blob_url = make_blob_url(blob_mirror_url, &merkle)?;

    let flaked = Arc::new(AtomicBool::new(false));

    fuchsia_backoff::retry_or_first_error(retry::blob_fetch(), || {
        let flaked = Arc::clone(&flaked);
        let mirror_stats = &mirror_stats;

        async {
            if let Some((blob, blob_closer)) =
                cache.create_blob(merkle, blob_kind).await.map_err(FetchError::CreateBlob)?
            {
                let res = download_blob(client, &blob_url, expected_len, blob).await;
                blob_closer.close().await;
                res?;
            }

            Ok(())
        }
        .inspect(move |res| match res.as_ref().map_err(FetchError::kind) {
            Err(FetchErrorKind::NetworkRateLimit) => {
                mirror_stats.network_rate_limits().increment();
            }
            Err(FetchErrorKind::Network) => {
                flaked.store(true, Ordering::SeqCst);
            }
            Err(FetchErrorKind::Other) => {}
            Ok(()) => {
                if flaked.load(Ordering::SeqCst) {
                    mirror_stats.network_blips().increment();
                }
            }
        })
    })
    .await
}

#[derive(Debug, Error)]
pub enum BlobUrlError {
    #[error("Blob mirror url doesn't have a path: {mirror_url}")]
    UriWithoutPath { mirror_url: String },

    #[error("While making blob url from {mirror_url}, invalid URI: {e}")]
    InvalidUri {
        #[source]
        e: http::uri::InvalidUri,
        mirror_url: String,
    },

    #[error("while making blob url from {mirror_url}, invalid URI parts: {e}")]
    InvalidUriParts {
        #[source]
        e: http::uri::InvalidUriParts,
        mirror_url: String,
    },
}

fn make_blob_url(blob_mirror_url: &str, merkle: &BlobId) -> Result<hyper::Uri, BlobUrlError> {
    let uri = blob_mirror_url
        .parse::<Uri>()
        .map_err(|e| BlobUrlError::InvalidUri { e, mirror_url: blob_mirror_url.into() })?;

    let mut uri_parts = uri.into_parts();
    let (path, query) = match &uri_parts.path_and_query {
        Some(path_and_query) => {
            // Remove a trailing slash from path, if any.
            let mut modified_path = path_and_query.path().to_owned();
            if modified_path.ends_with('/') {
                modified_path.pop();
            }
            (modified_path, path_and_query.query())
        }
        None => return Err(BlobUrlError::UriWithoutPath { mirror_url: blob_mirror_url.into() }),
    };
    // Add the merkle string to the end of the path.
    // There isn't a way to reconstruct a PathAndQuery by its struct members,
    // so we have to use format and then parse from a string...
    uri_parts.path_and_query = Some(
        if let Some(query) = query {
            format!("{}/{}?{}", path, &merkle, query)
        } else {
            format!("{}/{}", path, &merkle)
        }
        .parse()
        .map_err(|e| BlobUrlError::InvalidUri { e, mirror_url: blob_mirror_url.into() })?,
    );

    Ok(Uri::from_parts(uri_parts)
        .map_err(|e| BlobUrlError::InvalidUriParts { e, mirror_url: blob_mirror_url.into() })?)
}

async fn download_blob(
    client: &fuchsia_hyper::HttpsClient,
    uri: &http::Uri,
    expected_len: Option<u64>,
    dest: pkgfs::install::Blob<pkgfs::install::NeedsTruncate>,
) -> Result<(), FetchError> {
    let request = Request::get(uri)
        .body(Body::empty())
        .map_err(|e| FetchError::Http { e, uri: uri.to_string() })?;
    let response = client
        .request(request)
        .compat()
        .await
        .map_err(|e| FetchError::Hyper { e, uri: uri.to_string() })?;

    if response.status() != StatusCode::OK {
        return Err(FetchError::BadHttpStatus { code: response.status(), uri: uri.to_string() });
    }

    let body = response.into_body();

    let expected_len = match (expected_len, body.content_length()) {
        (Some(expected), Some(actual)) => {
            if expected != actual {
                return Err(FetchError::ContentLengthMismatch {
                    expected,
                    actual,
                    uri: uri.to_string(),
                });
            } else {
                expected
            }
        }
        (Some(length), None) | (None, Some(length)) => length,
        (None, None) => return Err(FetchError::UnknownLength { uri: uri.to_string() }),
    };

    let mut dest = dest.truncate(expected_len).await.map_err(FetchError::Truncate)?;

    let mut chunks = body.compat();
    let mut written = 0u64;
    while let Some(chunk) =
        chunks.try_next().await.map_err(|e| FetchError::Hyper { e, uri: uri.to_string() })?
    {
        if written + chunk.len() as u64 > expected_len {
            return Err(FetchError::BlobTooLarge { uri: uri.to_string() });
        }

        dest = match dest.write(&chunk).await.map_err(FetchError::Write)? {
            pkgfs::install::BlobWriteSuccess::MoreToWrite(blob) => {
                written += chunk.len() as u64;
                blob
            }
            pkgfs::install::BlobWriteSuccess::Done => {
                written += chunk.len() as u64;
                break;
            }
        };
    }

    if expected_len != written {
        return Err(FetchError::BlobTooSmall { uri: uri.to_string() });
    }

    Ok(())
}

#[derive(Debug, Error)]
pub enum FetchError {
    #[error("could not create blob: {0}")]
    CreateBlob(pkgfs::install::BlobCreateError),

    #[error("Blob fetch of {uri}: http request expected 200, got {code}")]
    BadHttpStatus { code: hyper::StatusCode, uri: String },

    #[error("repository has no configured mirrors")]
    NoMirrors,

    #[error("Blob fetch of {uri}: expected blob length of {expected}, got {actual}")]
    ContentLengthMismatch { expected: u64, actual: u64, uri: String },

    #[error("Blob fetch of {uri}: blob length not known or provided by server")]
    UnknownLength { uri: String },

    #[error("Blob fetch of {uri}: downloaded blob was too small")]
    BlobTooSmall { uri: String },

    #[error("Blob fetch of {uri}: downloaded blob was too large")]
    BlobTooLarge { uri: String },

    #[error("failed to truncate blob: {0}")]
    Truncate(pkgfs::install::BlobTruncateError),

    #[error("failed to write blob data: {0}")]
    Write(pkgfs::install::BlobWriteError),

    #[error("hyper error while fetching {uri}: {e}")]
    Hyper {
        #[source]
        e: hyper::Error,
        uri: String,
    },

    #[error("http error while fetching {uri}: {e}")]
    Http {
        #[source]
        e: hyper::http::Error,
        uri: String,
    },

    #[error("blob url error: {0}")]
    BlobUrl(#[source] BlobUrlError),
}

impl From<BlobUrlError> for FetchError {
    fn from(x: BlobUrlError) -> Self {
        FetchError::BlobUrl(x)
    }
}

impl FetchError {
    fn kind(&self) -> FetchErrorKind {
        match self {
            FetchError::BadHttpStatus { code: StatusCode::TOO_MANY_REQUESTS, uri: _ } => {
                FetchErrorKind::NetworkRateLimit
            }
            FetchError::Hyper { .. }
            | FetchError::Http { .. }
            | FetchError::BadHttpStatus { .. } => FetchErrorKind::Network,
            _ => FetchErrorKind::Other,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum FetchErrorKind {
    NetworkRateLimit,
    Network,
    Other,
}

#[cfg(test)]
mod tests {
    use super::*;
    use matches::assert_matches;

    #[test]
    fn test_make_blob_url() {
        let merkle = "00112233445566778899aabbccddeeffffeeddccbbaa99887766554433221100"
            .parse::<BlobId>()
            .unwrap();

        assert_eq!(
            make_blob_url("http://example.com", &merkle).unwrap(),
            format!("http://example.com/{}", merkle).parse::<Uri>().unwrap()
        );

        assert_eq!(
            make_blob_url("http://example.com/noslash", &merkle).unwrap(),
            format!("http://example.com/noslash/{}", merkle).parse::<Uri>().unwrap()
        );

        assert_eq!(
            make_blob_url("http://example.com/slash/", &merkle).unwrap(),
            format!("http://example.com/slash/{}", merkle).parse::<Uri>().unwrap()
        );

        assert_eq!(
            make_blob_url("http://example.com/twoslashes//", &merkle).unwrap(),
            format!("http://example.com/twoslashes//{}", merkle).parse::<Uri>().unwrap()
        );

        assert_matches!(
            make_blob_url("HelloWorld", &merkle).unwrap_err(),
            BlobUrlError::UriWithoutPath { mirror_url } if mirror_url == "HelloWorld".to_string()
        );

        assert_matches!(
            make_blob_url("server:80", &merkle).unwrap_err(),
            BlobUrlError::UriWithoutPath { mirror_url } if mirror_url == "server:80".to_string()
        );

        // IPv6 zone id
        assert_eq!(
            make_blob_url("http://[fe80::e022:d4ff:fe13:8ec3%252]:8083/blobs/", &merkle).unwrap(),
            format!("http://[fe80::e022:d4ff:fe13:8ec3%252]:8083/blobs/{}", merkle)
                .parse::<Uri>()
                .unwrap()
        );
    }
}
