//! The desktop shell: one Tauri command, `rpc`, that forwards to `dispatch`. It opens no socket.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use cm_core::{ApiError, Request, dispatch};
use cm_host::Host;
use tauri::{State, WebviewUrl, WebviewWindowBuilder};

struct SelfTest(Option<PathBuf>);

#[tauri::command]
async fn rpc(host: State<'_, Arc<Host>>, request: Request) -> Result<serde_json::Value, ApiError> {
    dispatch(host.inner().as_ref(), request).await
}

/// Test only: with `CM_SELFTEST=<file>` set, writes the page's report to that file and exits.
#[tauri::command]
fn selftest_report(
    app: tauri::AppHandle,
    state: State<'_, SelfTest>,
    report: serde_json::Value,
) -> Result<(), String> {
    let path = state.0.as_ref().ok_or("the self-test is not enabled")?;
    let text = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| e.to_string())?;
    app.exit(0);
    Ok(())
}

pub fn run(host: Arc<Host>, selftest: Option<PathBuf>) -> Result<(), String> {
    let page = if selftest.is_some() {
        "index.html?selftest"
    } else {
        "index.html"
    };
    if selftest.is_some() {
        // A page that never reports must not leave a window open forever.
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(90));
            eprintln!("context-manager: the self-test did not report within 90 seconds");
            std::process::exit(1);
        });
    }
    tauri::Builder::default()
        .manage(host)
        .manage(SelfTest(selftest))
        .invoke_handler(tauri::generate_handler![rpc, selftest_report])
        .setup(move |app| {
            WebviewWindowBuilder::new(app, "main", WebviewUrl::App(page.into()))
                .title("context-manager")
                .inner_size(1200.0, 800.0)
                .build()?;
            Ok(())
        })
        .run(tauri::generate_context!())
        .map_err(|e| e.to_string())
}
