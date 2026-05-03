#![cfg_attr(windows, windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    video_for_lazies_lib::updater::run_update_helper_cli()
}
