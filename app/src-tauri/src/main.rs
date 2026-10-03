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
                );
            }
            return;
        }
    }
    app_lib::run()
}
