//! `frost simulate`: compare schedulers on a graph without running it.

use anyhow::bail;
use anyhow::Result;
use frostbuild_core::journal::Journal;

use crate::build::default_jobs;
use crate::graph::{load_graph, resolve_targets};

pub(crate) struct SimulateRequest {
    pub(crate) targets: Vec<String>,
    pub(crate) jobs: Option<Vec<usize>>,
    pub(crate) local_cpu_resources: Option<usize>,
    pub(crate) local_ram_resources: Option<u64>,
    pub(crate) local_test_jobs: Option<usize>,
    pub(crate) profile: String,
    pub(crate) platform: String,
    pub(crate) json: bool,
}

/// Compare scheduling strategies by planning them, not by running them.
///
/// Every strategy is scored against the durations recorded in the journal, so
/// the numbers are deterministic and no cache is touched. Simulation models
/// queue ordering and declared resource admission, but not unmodelled I/O or
/// CPU contention; calibrate absolute times against a real `build --stats` run.
pub(crate) fn run_simulate(root: &std::path::Path, request: SimulateRequest) -> Result<i32> {
    use frostbuild_bench::{render_table, Sweep, ESTIMATORS, SCHEDULERS};
    use frostbuild_exec::Schedule;

    let SimulateRequest {
        targets,
        jobs,
        local_cpu_resources,
        local_ram_resources,
        local_test_jobs,
        profile,
        platform,
        json,
    } = request;

    if local_cpu_resources == Some(0) || local_test_jobs == Some(0) {
        bail!("local CPU resources and local test jobs must be at least 1");
    }
    if local_ram_resources == Some(0) {
        bail!("local RAM resources must be at least 1 MiB");
    }

    let graph = load_graph(root, &profile, &platform)?;
    let requested = resolve_targets(&graph, targets)?;
    let closure = graph.action_closure(&requested)?;
    if closure.is_empty() {
        bail!("nothing to simulate: the requested targets have no actions");
    }
    let journal = Journal::load(root);
    let host = default_jobs();
    let jobs = jobs.unwrap_or_else(|| {
        [1, 2, 4, 8, 16]
            .into_iter()
            .filter(|&j| j <= host.max(1))
            .collect()
    });
    let jobs = if jobs.is_empty() { vec![1] } else { jobs };

    let sweep = Sweep::run_with_resources(
        &jobs,
        &SCHEDULERS,
        &ESTIMATORS,
        |scheduler, estimator| {
            Schedule::plan(&graph, closure.clone(), &journal, scheduler, estimator)
        },
        |jobs| {
            let mut limits = frostbuild_exec::ResourceLimits::host(jobs);
            limits.cpu = local_cpu_resources.unwrap_or(limits.cpu);
            limits.ram_mb = local_ram_resources.unwrap_or(limits.ram_mb);
            limits.test_jobs = local_test_jobs.unwrap_or(jobs.max(1));
            limits
        },
    );

    let recorded = graph
        .actions
        .iter()
        .filter(|a| {
            journal
                .actions
                .get(&frostbuild_exec::journal_id(&graph, a))
                .is_some_and(|e| e.duration_ms > 0)
        })
        .count();

    if json {
        let points: Vec<_> = sweep
            .points
            .iter()
            .map(|p| {
                serde_json::json!({
                    "scheduler": p.scheduler.as_str(),
                    "estimator": p.estimator.as_str(),
                    "jobs": p.simulation.jobs,
                    "makespan_ms": p.simulation.makespan_ms,
                    "utilization_pct": p.simulation.utilization_pct(),
                    "over_critical_path_pct": p.simulation.over_critical_path_pct(),
                    "peak_cpu": p.simulation.peak_cpu,
                    "peak_ram_mb": p.simulation.peak_ram_mb,
                    "peak_test_jobs": p.simulation.peak_tests,
                    "resource_waits": p.simulation.resource_waits,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "actions": sweep.actions,
                "actions_with_recorded_duration": recorded,
                "critical_path_ms": sweep.critical_path_ms,
                "work_ms": sweep.work_ms,
                "resource_limits": {
                    "cpu": local_cpu_resources.unwrap_or_else(default_jobs),
                    "ram_mb": local_ram_resources.unwrap_or_else(|| frostbuild_exec::ResourceLimits::host(host).ram_mb),
                    "test_jobs": local_test_jobs,
                },
                "points": points,
            }))?
        );
        return Ok(0);
    }

    println!(
        "frost: simulating {} actions from the journal (no build, no cache writes)",
        sweep.actions
    );
    println!(
        "  resources: cpu {}, ram {} MiB, test jobs {}",
        local_cpu_resources.unwrap_or_else(default_jobs),
        local_ram_resources.unwrap_or_else(|| frostbuild_exec::ResourceLimits::host(host).ram_mb),
        local_test_jobs.map_or_else(|| "per -j point".to_string(), |n| n.to_string())
    );
    if recorded < sweep.actions {
        println!(
            "  note: {} of {} actions have no recorded duration; those fall back to estimates",
            sweep.actions - recorded,
            sweep.actions
        );
    }
    println!();
    println!(
        "  critical path  {} ms   total work  {} ms",
        sweep.critical_path_ms, sweep.work_ms
    );
    println!("  no schedule can finish faster than the critical path.");
    println!();
    print!("{}", render_table(&sweep));
    println!();
    if let Some(best) = sweep.best() {
        let over = best
            .simulation
            .over_critical_path_pct()
            .map(|p| format!("{p:.0}% above the critical path"))
            .unwrap_or_else(|| "critical path unknown".to_string());
        println!(
            "  fastest: {} / {} at -j {} -> {} ms ({}, {:.0}% worker utilization)",
            best.scheduler.as_str(),
            best.estimator.as_str(),
            best.simulation.jobs,
            best.simulation.makespan_ms,
            over,
            best.simulation.utilization_pct()
        );
    }
    println!("  compare against a real run: frost build --stats -j <n>");
    Ok(0)
}
