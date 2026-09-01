use std::{path::Path, process::Stdio, time::Duration};

use axum::{Json, extract::State};
use chrono::Utc;
use serde::Serialize;
use tokio::process::Command;

use crate::{AppState, auth::AuthUser};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Ok,
    Info,
    Warning,
    Critical,
    Unavailable,
}

#[derive(Debug, Serialize)]
pub struct Finding {
    pub category: &'static str,
    pub severity: Severity,
    pub title: String,
    pub details: String,
    pub confidence: f32,
    pub evidence: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct DoctorReport {
    pub checked_at: i64,
    pub overall: Severity,
    pub issues: usize,
    pub findings: Vec<Finding>,
}

pub async fn run(State(state): State<AppState>, _user: AuthUser) -> Json<DoctorReport> {
    let metrics = state.telemetry.latest().await;
    let performance_mode = metrics.performance_mode;
    let (docker, systemd, units, updates) = tokio::join!(
        state.docker.status(),
        state.systemd.status(),
        state.systemd.units(),
        available_updates(performance_mode),
    );
    let mut findings = Vec::new();
    findings.push(threshold_finding(
        "cpu",
        "CPU",
        metrics.cpu_percent,
        80.0,
        95.0,
        "%",
    ));
    findings.push(threshold_finding(
        "memory",
        "Memory",
        metrics.memory.percent,
        85.0,
        95.0,
        "%",
    ));
    findings.push(threshold_finding(
        "disk",
        "Disk",
        metrics.disk.percent,
        85.0,
        95.0,
        "% used on /",
    ));
    findings.push(if metrics.swap.total_bytes == 0 {
        finding(
            "swap",
            Severity::Info,
            "Swap not configured",
            "This can be intentional; memory pressure has less safety margin.",
            1.0,
            serde_json::json!({"total_bytes": 0}),
        )
    } else {
        threshold_finding("swap", "Swap", metrics.swap.percent, 40.0, 80.0, "% used")
    });

    let hottest = metrics
        .temperatures
        .iter()
        .max_by(|a, b| a.celsius.total_cmp(&b.celsius));
    findings.push(match hottest {
        Some(sensor) if sensor.celsius >= 90.0 => finding(
            "temperature",
            Severity::Critical,
            "Temperature is critical",
            &format!("{} reports {:.1} °C.", sensor.label, sensor.celsius),
            0.9,
            serde_json::json!({"sensor": sensor.label, "celsius": sensor.celsius}),
        ),
        Some(sensor) if sensor.celsius >= 80.0 => finding(
            "temperature",
            Severity::Warning,
            "Temperature is elevated",
            &format!("{} reports {:.1} °C.", sensor.label, sensor.celsius),
            0.9,
            serde_json::json!({"sensor": sensor.label, "celsius": sensor.celsius}),
        ),
        Some(sensor) => finding(
            "temperature",
            Severity::Ok,
            "Temperatures",
            &format!(
                "Highest sensor is {} at {:.1} °C.",
                sensor.label, sensor.celsius
            ),
            0.9,
            serde_json::json!({"sensor": sensor.label, "celsius": sensor.celsius}),
        ),
        None => finding(
            "temperature",
            Severity::Unavailable,
            "Temperature sensors unavailable",
            "The kernel did not expose thermal zones to CarobaGuard.",
            1.0,
            serde_json::json!({}),
        ),
    });

    findings.push(if docker.available {
        finding(
            "docker",
            Severity::Ok,
            "Docker",
            &format!(
                "Docker Engine {} is responding.",
                docker.version.unwrap_or_default()
            ),
            1.0,
            serde_json::json!({"socket": docker.socket}),
        )
    } else if Path::new(&docker.socket).exists() {
        finding(
            "docker",
            Severity::Warning,
            "Docker socket is not accessible",
            docker.error.as_deref().unwrap_or("Docker API failed."),
            1.0,
            serde_json::json!({"socket": docker.socket}),
        )
    } else {
        finding(
            "docker",
            Severity::Info,
            "Docker not installed or disabled",
            "No Docker socket was found; Docker is optional.",
            1.0,
            serde_json::json!({"socket": docker.socket}),
        )
    });

    findings.push(if !systemd.available {
        finding(
            "services",
            Severity::Unavailable,
            "systemd unavailable",
            systemd.error.as_deref().unwrap_or("systemctl failed."),
            1.0,
            serde_json::json!({}),
        )
    } else {
        match units {
            Ok(units) => {
                let failed = units
                    .iter()
                    .filter(|unit| unit.active_state == "failed")
                    .map(|unit| unit.name.as_str())
                    .collect::<Vec<_>>();
                if failed.is_empty() {
                    finding(
                        "services",
                        Severity::Ok,
                        "Services",
                        "No failed service units were detected.",
                        1.0,
                        serde_json::json!({"failed": []}),
                    )
                } else {
                    finding(
                        "services",
                        Severity::Warning,
                        "Failed services detected",
                        &format!("{} service unit(s) are failed.", failed.len()),
                        1.0,
                        serde_json::json!({"failed": failed}),
                    )
                }
            }
            Err(error) => finding(
                "services",
                Severity::Unavailable,
                "Unable to inspect services",
                &error.to_string(),
                1.0,
                serde_json::json!({}),
            ),
        }
    });

    findings.push(match updates {
        UpdateCount::Count(count) if count > 0 => finding(
            "updates",
            Severity::Info,
            "Updates available",
            &format!("{count} package update(s) are visible in the local package database."),
            0.8,
            serde_json::json!({"count": count}),
        ),
        UpdateCount::Count(_) => finding(
            "updates",
            Severity::Ok,
            "Updates",
            "No pending package updates were reported.",
            0.8,
            serde_json::json!({"count": 0}),
        ),
        UpdateCount::Skipped => finding(
            "updates",
            Severity::Info,
            "Update scan suspended",
            "Performance Mode skips this secondary scan.",
            1.0,
            serde_json::json!({"performance_mode": true}),
        ),
        UpdateCount::Unavailable => finding(
            "updates",
            Severity::Unavailable,
            "Update status unavailable",
            "No supported package-manager query was available.",
            0.8,
            serde_json::json!({}),
        ),
    });

    let exposed = !state.config.bind.ip().is_loopback();
    findings.push(if exposed && !state.config.cookie_secure {
        finding(
            "security",
            Severity::Critical,
            "Insecure remote binding",
            "CarobaGuard is listening beyond loopback while Secure cookies are disabled.",
            1.0,
            serde_json::json!({"bind": state.config.bind.to_string(), "secure_cookie": false}),
        )
    } else {
        finding(
            "security",
            Severity::Ok,
            "Panel network exposure",
            if exposed {
                "Remote bind detected with Secure cookies enabled. Verify HTTPS termination."
            } else {
                "The panel is bound to loopback only."
            },
            1.0,
            serde_json::json!({"bind": state.config.bind.to_string()}),
        )
    });

    let issues = findings
        .iter()
        .filter(|finding| matches!(finding.severity, Severity::Warning | Severity::Critical))
        .count();
    let overall = if findings
        .iter()
        .any(|finding| finding.severity == Severity::Critical)
    {
        Severity::Critical
    } else if issues > 0 {
        Severity::Warning
    } else {
        Severity::Ok
    };
    Json(DoctorReport {
        checked_at: Utc::now().timestamp(),
        overall,
        issues,
        findings,
    })
}

fn threshold_finding(
    category: &'static str,
    label: &str,
    value: f64,
    warning: f64,
    critical: f64,
    suffix: &str,
) -> Finding {
    let (severity, state) = if value >= critical {
        (Severity::Critical, "critical")
    } else if value >= warning {
        (Severity::Warning, "elevated")
    } else {
        (Severity::Ok, "within threshold")
    };
    finding(
        category,
        severity,
        label,
        &format!("{value:.1}{suffix}; {state}."),
        1.0,
        serde_json::json!({"value": value, "warning_threshold": warning, "critical_threshold": critical}),
    )
}

fn finding(
    category: &'static str,
    severity: Severity,
    title: &str,
    details: &str,
    confidence: f32,
    evidence: serde_json::Value,
) -> Finding {
    Finding {
        category,
        severity,
        title: title.to_owned(),
        details: details.to_owned(),
        confidence,
        evidence,
    }
}

enum UpdateCount {
    Count(usize),
    Skipped,
    Unavailable,
}

async fn available_updates(performance_mode: bool) -> UpdateCount {
    if performance_mode {
        return UpdateCount::Skipped;
    }
    let (program, args): (&str, &[&str]) = if Path::new("/usr/bin/pacman").exists() {
        ("/usr/bin/pacman", &["-Qu"])
    } else if Path::new("/usr/bin/apt").exists() {
        ("/usr/bin/apt", &["list", "--upgradable"])
    } else {
        return UpdateCount::Unavailable;
    };
    let output = tokio::time::timeout(
        Duration::from_secs(15),
        Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .output(),
    )
    .await;
    match output {
        Ok(Ok(output)) if output.status.success() => {
            let count = String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|line| !line.trim().is_empty() && !line.starts_with("Listing"))
                .count();
            UpdateCount::Count(count)
        }
        _ => UpdateCount::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold_classification_is_deterministic() {
        assert_eq!(
            threshold_finding("disk", "Disk", 79.0, 80.0, 95.0, "%").severity,
            Severity::Ok
        );
        assert_eq!(
            threshold_finding("disk", "Disk", 80.0, 80.0, 95.0, "%").severity,
            Severity::Warning
        );
        assert_eq!(
            threshold_finding("disk", "Disk", 95.0, 80.0, 95.0, "%").severity,
            Severity::Critical
        );
    }
}
