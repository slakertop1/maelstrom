mod db;
mod grpc;
mod http_client;
mod loadtest;
mod log;
mod oauth;
mod scenario;
mod storage;
mod tls;
mod ws;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(loadtest::LoadTestState::default())
        .invoke_handler(tauri::generate_handler![
            http_client::send_request,
            loadtest::start_load_test,
            loadtest::stop_load_test,
            scenario::start_scenario_load_test,
            oauth::fetch_oauth_token,
            oauth::oauth_authorization_code,
            db::db_execute,
            db::start_db_load_test,
            grpc::grpc_list_methods,
            grpc::grpc_request_template,
            grpc::grpc_call,
            grpc::grpc_start_load,
            ws::ws_call,
            ws::ws_start_load,
            storage::load_state,
            storage::load_state_backup,
            storage::save_state,
            storage::write_text_file,
            storage::read_text_file,
            log::read_log,
            log::log_path,
            log::clear_log,
            log::open_log_folder,
            log::log_event,
            log::app_version,
            log::open_url
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
