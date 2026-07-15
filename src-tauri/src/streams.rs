// Thin Tauri wrapper over the shared request-chaining (streams) engine. Heavy
// lifting (per-stream dispatch, per-iteration variables, extract, three-level
// aggregation) lives in maelstrom-core::streams; here we bridge progress to
// Tauri events and manage the single load slot.
use crate::loadtest::LoadTestState;
use maelstrom_core::streams::run_streams;
use maelstrom_core::types::{StreamScenarioSpec, StreamsProgress};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

#[tauri::command]
pub async fn start_streams_load_test(
    app: AppHandle,
    state: State<'_, LoadTestState>,
    spec: StreamScenarioSpec,
) -> Result<(), String> {
    // Resolve DB-backed datasets to inline rows before taking the slot (fail fast).
    let mut spec = spec;
    spec.datasets = crate::db::resolve_db_datasets(&app, &spec.datasets).await?;

    let (token, running) = state.try_start()?;

    let total_steps: usize = spec.streams.iter().map(|s| s.steps.len()).sum();
    crate::log::write(
        &app,
        "STREAMS ▶",
        &format!(
            "{} потоков ({} шагов), {}с | {}",
            spec.streams.len(),
            total_steps,
            spec.duration_secs,
            spec.streams
                .iter()
                .map(|s| format!("«{}» {}шаг@{}rps", s.name, s.steps.len(), s.rps))
                .collect::<Vec<_>>()
                .join("; ")
        ),
    );

    let app_progress = app.clone();
    let on_progress: Arc<dyn Fn(&StreamsProgress) + Send + Sync> = Arc::new(move |p| {
        let _ = app_progress.emit("streams_progress", p);
    });
    let app_log = app.clone();
    let on_log: Arc<dyn Fn(String) + Send + Sync> = Arc::new(move |m| {
        crate::log::write(&app_log, "ДВИЖОК", &m);
    });

    tauri::async_runtime::spawn(async move {
        // The run body lives in its own task so a panic anywhere inside
        // `run_streams` is caught by tokio at THIS task's boundary. Awaiting
        // `run` below then always completes — Ok or Err(JoinError) — and
        // `running.store(false, ..)` still executes, instead of a panic
        // unwinding straight past it and leaving the slot stuck "running"
        // forever (t1).
        let run = tokio::spawn(async move {
            match run_streams(spec, token, on_progress, on_log).await {
                Ok(result) => {
                    let per = result
                        .streams
                        .iter()
                        .map(|s| {
                            format!(
                                "«{}»: {}/{} завершено ({:.1}%) e2e-p95={:.0}мс",
                                s.name,
                                s.iterations_completed,
                                s.iterations_started,
                                s.success_rate,
                                s.e2e_p95_ms
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("; ");
                    crate::log::write(
                        &app,
                        "STREAMS ■",
                        &format!(
                            "всего={} ошибок={} ({:.2}%) rps={:.0} | {}",
                            result.overall.total_requests,
                            result.overall.errors,
                            result.overall.error_rate,
                            result.overall.rps_avg,
                            per
                        ),
                    );
                    let _ = app.emit("streams_finished", &result);
                }
                Err(e) => {
                    crate::log::write(&app, "STREAMS ✗", &e);
                    let _ = app.emit("streams_error", &e);
                }
            }
        });
        let _ = run.await;
        running.store(false, Ordering::SeqCst);
    });

    Ok(())
}
