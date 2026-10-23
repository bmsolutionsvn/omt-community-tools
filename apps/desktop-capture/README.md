# OMT Desktop Capture

Publish a monitor or an application window as an **OMT-HX** source on the LAN,
the job vMix Desktop Capture does for the OMT/NDI tools. Each enabled source is
one discovered OMT source, named `<host> (<source name>)`, for example
`STUDIO-PC (Screen 1)`.

Windows 11 x64 only. It ships as its own zipped folder, not as part of the
launcher installer.

## Run it

1. Unzip the folder.
2. Run `omt-desktop-capture.exe` **from inside that folder**.

`omthx.dll`, its FFmpeg DLLs and `libomt.dll`/`libvmx.dll` must stay beside the
exe. Copying just the exe elsewhere makes every source fail to start with a
loader error. There is no installer, no service and no launcher entry.

Receive the sources with Studio Monitor, BreadStudio, `omthx-recv` or any other
OMT-HX receiver.

## Sources

- **Add source → Screens** lists the live monitors, primary first.
- **Add source → Applications** lists top-level windows grouped by process name.
  Windows of this tool itself are filtered out.
- **Refresh** re-enumerates both lists.
- A source survives the tool restarting: monitors by device id, windows by
  process name and title. A missing window comes back as an **Offline** row.
  Pick **Reattach** to choose the window again when the match is ambiguous or
  gone.

Each row has:

- an enabled checkbox and an editable name;
- a state pill (Idle, Ready, Live, Connecting, Offline, with the reason on failure);
- FPS, Mbps, connections and encode time;
- an expander with **Max FPS** (5, 15, 25, 29.97, 30, 50, 59.94, 60),
  **Show cursor**, **Audio** (No audio or a render device) and **Remove**.

The footer has **Start all** / **Stop all**, quality (**LQ**, **SQ**, **HQ**),
codec (**H.265** or **H.264**), **Start on launch**, **Minimise on start** and the
version. Quality and codec apply when a source starts; changing them while
sources run is saved and applies on the next start.

Settings are saved automatically to
`%APPDATA%\Mikansei\OMT Tools\config\desktop-capture.json`. An unreadable file
is kept as `desktop-capture.bad.json` instead of being overwritten.

## OMT-HX only

Output is OMT-HX (HEVC by default, H.264 optional). There is no OMT Full
output, no NDI, no SRT, no virtual camera and no recording. **vMix does not
receive these sources**: vMix speaks OMT Full only, so there is no vMix interop
in this version.

## Known limits

- **Elevated (UAC) windows and protected content.** Windows Graphics Capture
  returns a black or blocked picture for windows of elevated processes and for
  DRM-protected content. This is a Windows limitation, with no workaround here.
- **Hardware encoder when present.** NVENC/AMF/QSV is used when the machine has
  one; otherwise the encoder falls back to software HEVC/H.264, which is slower
  and shows in the row's encode time.
- **Duplicate audio.** Audio is the selected render device's loopback, so two
  sources on the same device carry the same audio. The footer shows a warning
  while that is the case.
- **No per-application audio** and no audio-only sources; pick a render device
  or No audio per source.
- **HDR/10-bit and alpha are not supported.** Capture is 8-bit BGRA.
- **Odd window sizes** are padded on the right and bottom with black to the next
  even size (OMT-HX rejects odd dimensions). The receiver sees the padded size.
- **Static screens stay live**: unchanged frames are re-sent with a fresh
  timestamp so receivers do not drop the source.
- **No preview thumbnails** in the tool; use a receiver to look at the picture.
- **Default audio device changes are not followed.** The selected endpoint stays
  selected; pick the new device and restart the source.
- **A window renaming itself** can turn its restored row Offline; use Reattach.
- The 8-hour soak and hybrid-GPU (iGPU capture, dGPU encode) checks are manual.

## Build from source

```powershell
cargo build --release -p omt-desktop-capture
powershell -File scripts\package-desktop-capture.ps1   # -OmthxRoot <OMT-HX checkout>
powershell -File scripts\desktop-capture-e2e.ps1       # needs OMT-HX's omthx-recv.exe
```

The end-to-end script sends the primary screen for 20 seconds, receives 5
seconds with `omthx-recv --dump frame.bmp`, and fails if the frame is missing,
the wrong size, or all black. `-SelfCheck` exercises the black-frame check
against synthetic BMPs without starting the sender. It checks video only; audio
is covered by unit tests and the manual lip-sync check.
