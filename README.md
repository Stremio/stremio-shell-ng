## Stremio shell: new gen

A Windows-only shell using WebView2 and MPV

The MPV library is obtained from [sourceForge](https://sourceforge.net/projects/mpv-player-windows/files/libmpv/) where the latest working DLL is in the build `mpv-dev-i686-20211212-git-0e76372`. It must be `mpv-1.dll` and not `mpv-2.dll` unless [libmpv-rs](https://github.com/ParadoxSpiral/libmpv-rs) update their repo. The `mpv.lib` file is generated with the developer prompt for VS2019 using the following command:

```
lib /def:mpv.def /machine:x86 /out:mpv.lib
```

Goals:
* Performance
* Reliability
* Easy to ship

In all three, this architecture excels the [Qt-based shell](https://github.com/Stremio/stremio-shell): it is about 2-5x more efficient depending on the use case, as it allows MPV to render directly in the window through it's optimal video output rather than using libmpv to integrate with Qt.

This is due to Qt having a complex rendering pipeline involving ANGLE and multiple levels of composing and drawing to textures, which inhibits full HW acceleration.

Meanwhile in this setup MPV uses whichever pipeline it considers to be optimal (like the mpv desktop app), which is normally d3d11, allowing full HW acceleration.

For web rendering, we use the native WebView2, which is Chromium based but shipped as a part of Windows 10: therefore we do not need to ship our own "distribution" of Chromium.

Finally, this should be a lot more reliable as it uses a much simper and more native overall architecture.
