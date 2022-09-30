# This file downloads zip files and takes any executables from them.

$files = @(
    "https://github.com/vot/ffbinaries-prebuilt/releases/download/v3.3/ffmpeg-3.3.4-win-32.zip"
    "https://github.com/vot/ffbinaries-prebuilt/releases/download/v3.3/ffprobe-3.3.4-win-32.zip"
    "https://nodejs.org/dist/v16.17.0/node-v16.17.0-win-x86.zip"
)

New-Item -Path ".\bin" -ItemType Directory -Force
$archives = @()
$workers = foreach ($url in $files) {
    $fn = ([uri]$url).Segments[-1]
    $archives += $fn
    $wc = New-Object System.Net.WebClient
    Write-Output $wc.DownloadFileTaskAsync($url, $fn)
}

# wait until all files are downloaded
$workers.Result

foreach ($f in $archives) {
    Expand-Archive -Path $f -DestinationPath ".\temp" -Force -PassThru | Where-Object { $_.Name.EndsWith(".exe") -and -not $_.Name.StartsWith(".")} | Copy-Item -Destination ".\bin\"
    Remove-Item $f
}

Remove-Item ".\temp" -Recurse