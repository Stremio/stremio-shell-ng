$tag = $(git describe --abbrev=0)

foreach ($installer in (get-item .\StremioSetup*.exe)) {
    if ($tag.StartsWith("v$($installer.VersionInfo.ProductVersion.Trim())")) {
        aws s3 cp --acl public-read "$installer" s3://stremio-artifacts/stremio-shell-ng/$tag/
    }
}
node ./generate_descriptor.js --tag=$tag
