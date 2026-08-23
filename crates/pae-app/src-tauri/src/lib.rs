mod commands;

use std::sync::Mutex;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(commands::JobState(Mutex::new(None)))
        .invoke_handler(tauri::generate_handler![
            commands::get_config,
            commands::list_models,
            commands::probe_media,
            commands::start_job,
            commands::cancel_job,
            commands::reveal_path,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
