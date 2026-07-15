// Thin Tauri wrapper over the shared multi-endpoint engine. The heavy lifting
// (dispatch, aggregation, per-target token refresh) lives in maelstrom-core; here
// we only bridge progress/refresh callbacks to Tauri events and manage the slot.
use crate::loadtest::LoadTestState;
use maelstrom_core::scenario::run_scenario;
use maelstrom_core::types::{ScenarioProgress, ScenarioSpec};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

#[tauri::command]
pub async fn start_scenario_load_test(
    app: AppHandle,
    state: State<'_, LoadTestState>,
    spec: ScenarioSpec,
) -> Result<(), String> {
    // Resolve DB-backed datasets to inline rows before taking the slot (fail fast).
    let mut spec = spec;
    spec.datasets = crate::db::resolve_db_datasets(&app, &spec.datasets).await?;

    let (token, running) = state.try_start()?;

    crate::log::write(
        &app,
        "SCENARIO ▶",
        &format!(
            "{} ручек, {}с | {}",
            spec.targets.len(),
            spec.duration_secs,
            spec.targets
                .iter()
                .map(|t| format!("{} {} @{}rps", t.method, crate::log::safe_url(&t.url), t.rps))
                .collect::<Vec<_>>()
                .join("; ")
        ),
    );

    let app_progress = app.clone();
    let on_progress: Arc<dyn Fn(&ScenarioProgress) + Send + Sync> = Arc::new(move |p| {
        let _ = app_progress.emit("scenario_progress", p);
    });
    let app_refresh = app.clone();
    let on_refresh: Arc<dyn Fn(u64) + Send + Sync> = Arc::new(move |n| {
        let _ = app_refresh.emit("token_refreshed", n);
        crate::log::write(&app_refresh, "TOKEN", &format!("автообновление токена #{n}"));
    });
    let app_log = app.clone();
    let on_log: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |m| {
        crate::log::write(&app_log, "ДВИЖОК", &m);
    });

    tauri::async_runtime::spawn(async move {
        // The run body lives in its own task so a panic anywhere inside
        // `run_scenario` is caught by tokio at THIS task's boundary. Awaiting
        // `run` below then always completes — Ok or Err(JoinError) — and
        // `running.store(false, ..)` still executes, instead of a panic
        // unwinding straight past it and leaving the slot stuck "running"
        // forever (t1).
        let run = tokio::spawn(async move {
            match run_scenario(spec, token, on_progress, on_refresh, on_log).await {
                Ok(result) => {
                    let per = result
                        .targets
                        .iter()
                        .map(|t| {
                            format!(
                                "{} {}: {}зпр {:.1}%err p95={:.0}мс",
                                t.method,
                                crate::log::safe_url(&t.url),
                                t.total_requests,
                                t.error_rate,
                                t.p95_ms
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("; ");
                    crate::log::write(
                        &app,
                        "SCENARIO ■",
                        &format!(
                            "всего={} ошибок={} ({:.2}%) rps={:.0} | {}",
                            result.overall.total_requests,
                            result.overall.errors,
                            result.overall.error_rate,
                            result.overall.rps_avg,
                            per
                        ),
                    );
                    let _ = app.emit("scenario_finished", &result);
                }
                Err(e) => {
                    crate::log::write(&app, "SCENARIO ✗", &e);
                    let _ = app.emit("scenario_error", &e);
                }
            }
        });
        let _ = run.await;
        running.store(false, Ordering::SeqCst);
    });

    Ok(())
}
