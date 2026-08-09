# Third-party software and models

Kokoro Voice is MIT licensed. It installs and uses third-party software whose
licenses remain with their respective authors, including Tauri, Rust crates,
Python packages, uv, Kokoro ONNX, MLX Whisper, faster-whisper/CTranslate2,
PortAudio, and platform accessibility/OCR frameworks.

The exact Rust and JavaScript versions are recorded in `Cargo.lock` and
`app/package-lock.json`. The exact Python dependency versions and distribution
hashes are recorded in `requirements-macos.lock` and
`requirements-windows.lock`.

Kokoro and Whisper model files are not stored in this repository or bundled in
the installer. First-run setup downloads pinned upstream model revisions. Their
model cards and upstream distributions are the authoritative source for model
license terms and attribution requirements.

Before publishing a release, CI must generate a dependency inventory/SBOM and
the release checklist must confirm that all bundled and downloaded components
permit the intended redistribution and use.
