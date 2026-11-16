# Offline video fixture

`tiny.mp4` is synthetic, 32×24, 4 fps, 2 seconds, no audio. Recreate with FFmpeg:

```sh
ffmpeg -v error -f lavfi -i 'testsrc=size=32x24:rate=4:duration=2' -an -c:v mpeg4 -threads 1 -y tiny.mp4
```

Video tests need `ffmpeg` and `ffprobe` on PATH; they never call a provider.
The CI test job installs both. Runtime use of other media does not require them.
