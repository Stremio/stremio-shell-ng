## Stremio shell: new gen

A Windows-only shell using WebView2 and MPV

Goals:
* Performance
* Reliability
* Easy to ship

In all three, this architecture excels the [Qt-based shell](https://github.com/Stremio/stremio-shell): it is about 2-5x more efficient depending on the use case, as it allows MPV to render directly in the window through it's optimal video output rather than using libmpv to integrate with Qt.

This is due to Qt having a complex rendering pipeline involving ANGLE and multiple levels of composing and drawing to textures, which inhibits full HW acceleration.

Meanwhile in this setup MPV uses whichever pipeline it considers to be optimal (like the mpv desktop app), which is normally d3d11, allowing full HW acceleration.

For web rendering, we use the native WebView2, which is Chromium based but shipped as a part of Windows 10: therefore we do not need to ship our own "distribution" of Chromium.

Finally, this should be a lot more reliable as it uses a much simpler and more native overall architecture.

## Native ASS subtitles

The WebUI handshake advertises `nativeAssSubtitles=true` after the existing GPU
capability. The player accepts these subtitle commands:

```json
["mpv-command", ["sub-add", "https://example.com/subtitles.ass", "auto", "English", "eng"]]
["mpv-command", ["sub-remove", "7"]]
```

Subtitle loading runs on a dedicated mpv client with one asynchronous command in
flight. `loadfile` and `stop` cancel and drain subtitle work before changing media.
Subtitle commands do not reset video readiness. The WebUI matches `track-list`
titles to the current request before setting `sid`, preventing late loads from
changing the user's selection.

mpv handles fonts embedded in the media or subtitle file and installed system
fonts. Subtitles with separate font URLs stay in the WebUI libass renderer.
