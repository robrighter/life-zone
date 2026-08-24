# Capture tooling

Two PowerShell helpers for looking at the running app. They exist because
screenshots of this app lie in two specific ways, both discovered the hard way:

- **`shot.ps1`** captures the window with `PrintWindow` + `PW_RENDERFULLCONTENT`
  rather than `CopyFromScreen`. The latter grabs whatever is physically on the
  glass, so an occluded window comes back with a black client area — which is
  indistinguishable from a webview that failed to paint, and sends you debugging
  the wrong thing. It also sets per-monitor DPI awareness, without which the
  bitmap is the scaled size and every measurement taken from it is wrong.

- **`crop.ps1`** crops a region at 1:1 with nearest-neighbour and no resampling.
  M1 shipped two rendering bugs that were invisible in a downscaled screenshot
  and obvious the moment the PNG was cropped to actual pixels. Look at crops.

```powershell
powershell -ExecutionPolicy Bypass -File tools\shot.ps1 -Out shot.png
powershell -ExecutionPolicy Bypass -File tools\crop.ps1 -In shot.png -Out crop.png -X 780 -Y 1150 -W 800 -H 300
```

Note that WebView2 ignores synthetic mouse clicks, and `SendKeys` does not
reliably reach it either. To exercise a UI state for a screenshot, change the
default in the component and let Vite's HMR apply it — that is what the
knowledge overlay was verified with.
