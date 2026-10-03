//! Thin executable entrypoint; all application composition lives in `app_lib`.

// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    #[cfg(target_os = "windows")]
    {
        let args: Vec<_> = std::env::args().collect();
        if args
            .get(1)
            .is_some_and(|arg| arg == "--hereword-quiet-worker" || arg == "--hereword-duck-worker")
        {
            if let Some(root) = args
                .get(2)
                .and_then(|arg| arg.parse::<u32>().ok())
                .filter(|root| *root > 0)
            {
                app_lib::run_audio_quiet_worker(
                    root,
                    args.get(1)
                        .is_some_and(|arg| arg == "--hereword-duck-worker"),
                    args.get(3)
                        .and_then(|arg| arg.parse::<f64>().ok())
                        .filter(|value| value.is_finite())
                        .unwrap_or(0.80)
                        .clamp(0.40, 0.95),
                );
            }
            return;
        }
    }
    app_lib::run()
}
