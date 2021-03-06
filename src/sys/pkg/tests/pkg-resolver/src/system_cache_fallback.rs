#![cfg(test)]
use {
    blobfs_ramdisk::BlobfsRamdisk,
    fuchsia_async as fasync,
    fuchsia_pkg_testing::{
        serve::handler, Package, PackageBuilder, RepositoryBuilder, SystemImageBuilder,
    },
    fuchsia_zircon::Status,
    lib::{TestEnvBuilder, EMPTY_REPO_PATH},
    matches::assert_matches,
    pkgfs_ramdisk::PkgfsRamdisk,
    std::sync::Arc,
};

async fn test_package(name: &str, contents: &str) -> Package {
    PackageBuilder::new(name)
        .add_resource_at("p/t/o", format!("contents: {}\n", contents).as_bytes())
        .build()
        .await
        .expect("build package")
}

async fn pkgfs_with_system_image_and_pkg(
    system_image_package: &Package,
    pkg: &Package,
) -> PkgfsRamdisk {
    let blobfs = BlobfsRamdisk::start().unwrap();
    system_image_package.write_to_blobfs_dir(&blobfs.root_dir().unwrap());
    pkg.write_to_blobfs_dir(&blobfs.root_dir().unwrap());
    PkgfsRamdisk::start_with_blobfs(
        blobfs,
        Some(&system_image_package.meta_far_merkle_root().to_string()),
    )
    .expect("starting pkgfs")
}

// The package is in the cache. Networking is totally down. Fallback succeeds.
#[fasync::run_singlethreaded(test)]
async fn test_cache_fallback_succeeds_no_network() {
    let pkg_name = "test_cache_fallback_succeeds_no_network";
    let cache_pkg = test_package(pkg_name, "cache").await;
    // Put a copy of the package with altered contents in the repo to make sure
    // we're getting the one from the cache.
    let repo_pkg = test_package(pkg_name, "repo").await;
    let system_image_package =
        SystemImageBuilder::new(&[]).cache_packages(&[&cache_pkg]).build().await;
    let pkgfs = pkgfs_with_system_image_and_pkg(&system_image_package, &cache_pkg).await;

    let env = TestEnvBuilder::new().pkgfs(pkgfs).build();

    let repo = Arc::new(
        RepositoryBuilder::from_template_dir(EMPTY_REPO_PATH)
            .add_package(&system_image_package)
            .add_package(&repo_pkg)
            .build()
            .await
            .unwrap(),
    );
    let served_repository = repo
        .server()
        .uri_path_override_handler(handler::StaticResponseCode::server_error())
        .start()
        .unwrap();
    // System cache fallback is only triggered for fuchsia.com repos.
    env.register_repo_at_url(&served_repository, "fuchsia-pkg://fuchsia.com").await;
    let pkg_url = format!("fuchsia-pkg://fuchsia.com/{}", pkg_name);
    let package_dir = env.resolve_package(&pkg_url).await.unwrap();
    // Make sure we got the cache version, not the repo version.
    cache_pkg.verify_contents(&package_dir).await.unwrap();
    assert!(repo_pkg.verify_contents(&package_dir).await.is_err());

    env.stop().await;
}

// The package is in the cache. Fallback is triggered because requests for targets.json fail.
#[fasync::run_singlethreaded(test)]
async fn test_cache_fallback_succeeds_no_targets() {
    let pkg_name = "test_cache_fallback_succeeds_no_targets";
    let cache_pkg = test_package(pkg_name, "cache").await;
    // Put a copy of the package with altered contents in the repo to make sure
    // we're getting the one from the cache.
    let repo_pkg = test_package(pkg_name, "repo").await;
    let system_image_package =
        SystemImageBuilder::new(&[]).cache_packages(&[&cache_pkg]).build().await;
    let pkgfs = pkgfs_with_system_image_and_pkg(&system_image_package, &cache_pkg).await;

    let env = TestEnvBuilder::new().pkgfs(pkgfs).build();

    let repo = Arc::new(
        RepositoryBuilder::from_template_dir(EMPTY_REPO_PATH)
            .add_package(&system_image_package)
            .add_package(&repo_pkg)
            .build()
            .await
            .unwrap(),
    );
    let served_repository = repo
        .server()
        // TODO(ampearce): add a suffix handler so the version
        // won't matter. This work, it just seems brittle.
        .uri_path_override_handler(handler::ForPath::new(
            "/2.targets.json",
            handler::StaticResponseCode::server_error(),
        ))
        .start()
        .unwrap();
    // System cache fallback is only triggered for fuchsia.com repos.
    env.register_repo_at_url(&served_repository, "fuchsia-pkg://fuchsia.com").await;
    let pkg_url = format!("fuchsia-pkg://fuchsia.com/{}", pkg_name);
    let package_dir = env.resolve_package(&pkg_url).await.unwrap();
    // Make sure we got the cache version, not the repo version.
    cache_pkg.verify_contents(&package_dir).await.unwrap();
    assert!(repo_pkg.verify_contents(&package_dir).await.is_err());

    env.stop().await;
}

// The package is in the cache but not known to the repository. Don't fall back.
#[fasync::run_singlethreaded(test)]
async fn test_resolve_fails_not_in_repo() {
    let pkg = test_package("test_resolve_fails_not_in_repo", "stuff").await;
    let system_image_package = SystemImageBuilder::new(&[]).cache_packages(&[&pkg]).build().await;
    let pkgfs = pkgfs_with_system_image_and_pkg(&system_image_package, &pkg).await;
    let env = TestEnvBuilder::new().pkgfs(pkgfs).build();

    // Repo doesn't need any fault injection, it just doesn't know about the package.
    let repo = Arc::new(
        RepositoryBuilder::from_template_dir(EMPTY_REPO_PATH)
            .add_package(&system_image_package)
            .build()
            .await
            .unwrap(),
    );
    let served_repository = repo.server().start().unwrap();
    env.register_repo_at_url(&served_repository, "fuchsia-pkg://fuchsia.com").await;

    let pkg_url = format!("fuchsia-pkg://fuchsia.com/{}", pkg.name());
    let res = env.resolve_package(&pkg_url).await;
    assert_matches!(res, Err(Status::NOT_FOUND));
    env.stop().await;
}

// A package with the same name is in the cache and the repository. Prefer the repo package.
#[fasync::run_singlethreaded(test)]
async fn test_resolve_prefers_repo() {
    let pkg_name = "test_resolve_prefers_repo";
    let cache_pkg = test_package(pkg_name, "cache_package").await;
    let repo_pkg = test_package(pkg_name, "repo_package").await;
    let system_image_package =
        SystemImageBuilder::new(&[]).cache_packages(&[&cache_pkg]).build().await;
    let pkgfs = pkgfs_with_system_image_and_pkg(&system_image_package, &cache_pkg).await;
    let env = TestEnvBuilder::new().pkgfs(pkgfs).build();

    let repo = Arc::new(
        RepositoryBuilder::from_template_dir(EMPTY_REPO_PATH)
            .add_package(&system_image_package)
            .add_package(&repo_pkg)
            .build()
            .await
            .unwrap(),
    );
    let served_repository = repo.server().start().unwrap();
    env.register_repo_at_url(&served_repository, "fuchsia-pkg://fuchsia.com").await;

    let pkg_url = format!("fuchsia-pkg://fuchsia.com/{}", pkg_name);
    let package_dir = env.resolve_package(&pkg_url).await.unwrap();
    // Make sure we got the repo version, not the cache version.
    repo_pkg.verify_contents(&package_dir).await.unwrap();
    assert!(cache_pkg.verify_contents(&package_dir).await.is_err());
    env.stop().await;
}
