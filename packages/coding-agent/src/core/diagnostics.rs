//! System diagnostics — check environment, API keys, and dependencies.

use std::fmt;
use std::process::Command;

/// Status of a diagnostic check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagStatus {
    /// Check passed.
    Ok,
    /// Check passed with a note.
    Warn(String),
    /// Check failed.
    Error(String),
}

impl fmt::Display for DiagStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DiagStatus::Ok => write!(f, "OK"),
            DiagStatus::Warn(msg) => write!(f, "WARN: {msg}"),
            DiagStatus::Error(msg) => write!(f, "ERROR: {msg}"),
        }
    }
}

/// A single diagnostic check result.
#[derive(Debug, Clone)]
pub struct DiagCheck {
    pub name: String,
    pub status: DiagStatus,
    pub value: Option<String>,
}

/// Complete diagnostics report.
#[derive(Debug, Clone)]
pub struct DiagnosticsReport {
    pub checks: Vec<DiagCheck>,
}

impl DiagnosticsReport {
    /// Count passed checks.
    pub fn ok_count(&self) -> usize {
        self.checks
            .iter()
            .filter(|c| c.status == DiagStatus::Ok)
            .count()
    }

    /// Count warnings.
    pub fn warn_count(&self) -> usize {
        self.checks
            .iter()
            .filter(|c| matches!(c.status, DiagStatus::Warn(_)))
            .count()
    }

    /// Count errors.
    pub fn error_count(&self) -> usize {
        self.checks
            .iter()
            .filter(|c| matches!(c.status, DiagStatus::Error(_)))
            .count()
    }
}

impl fmt::Display for DiagnosticsReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "System Diagnostics")?;
        writeln!(f, "==================")?;
        writeln!(f)?;

        let max_name = self.checks.iter().map(|c| c.name.len()).max().unwrap_or(0);

        for check in &self.checks {
            let status_marker = match &check.status {
                DiagStatus::Ok => "✓",
                DiagStatus::Warn(_) => "⚠",
                DiagStatus::Error(_) => "✗",
            };
            let value = check
                .value
                .as_deref()
                .map(|v| format!(" ({v})"))
                .unwrap_or_default();
            writeln!(
                f,
                "  {status_marker} {:<width$} {}{value}",
                check.name,
                check.status,
                width = max_name
            )?;
        }

        writeln!(f)?;
        writeln!(
            f,
            "Summary: {} passed, {} warnings, {} errors",
            self.ok_count(),
            self.warn_count(),
            self.error_count()
        )?;
        Ok(())
    }
}

/// Run all diagnostic checks.
pub fn run_diagnostics() -> DiagnosticsReport {
    let mut checks = vec![
        // OS info
        check_os(),
        check_shell(),
        check_terminal(),
        check_terminal_size(),
        // Tools
        check_command("git", &["--version"]),
        check_command("cargo", &["--version"]),
    ];

    // API keys
    let api_keys = [
        ("ANTHROPIC_API_KEY", "Anthropic"),
        ("OPENAI_API_KEY", "OpenAI"),
        ("GEMINI_API_KEY", "Google"),
        ("GROQ_API_KEY", "Groq"),
        ("XAI_API_KEY", "xAI"),
        ("MISTRAL_API_KEY", "Mistral"),
    ];

    for (env_var, provider) in api_keys {
        checks.push(check_api_key(env_var, provider));
    }

    // .hand directory
    checks.push(check_hand_directory());

    DiagnosticsReport { checks }
}

fn check_os() -> DiagCheck {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    DiagCheck {
        name: "OS".to_string(),
        status: DiagStatus::Ok,
        value: Some(format!("{os}/{arch}")),
    }
}

fn check_shell() -> DiagCheck {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "unknown".to_string());
    DiagCheck {
        name: "Shell".to_string(),
        status: if shell == "unknown" {
            DiagStatus::Warn("SHELL not set".to_string())
        } else {
            DiagStatus::Ok
        },
        value: Some(shell),
    }
}

fn check_terminal() -> DiagCheck {
    let term = std::env::var("TERM").unwrap_or_else(|_| "unknown".to_string());
    DiagCheck {
        name: "Terminal".to_string(),
        status: if term == "unknown" || term == "dumb" {
            DiagStatus::Warn("Limited terminal".to_string())
        } else {
            DiagStatus::Ok
        },
        value: Some(term),
    }
}

fn check_terminal_size() -> DiagCheck {
    // Try to get terminal size
    let (cols, rows) = terminal_size();
    if cols > 0 && rows > 0 {
        DiagCheck {
            name: "Terminal Size".to_string(),
            status: if cols < 60 {
                DiagStatus::Warn("Terminal width < 60 columns".to_string())
            } else {
                DiagStatus::Ok
            },
            value: Some(format!("{cols}x{rows}")),
        }
    } else {
        DiagCheck {
            name: "Terminal Size".to_string(),
            status: DiagStatus::Warn("Could not detect".to_string()),
            value: None,
        }
    }
}

fn terminal_size() -> (u16, u16) {
    // Try tput
    if let Ok(output) = Command::new("tput").arg("cols").output()
        && let Ok(cols) = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<u16>()
        && let Ok(output) = Command::new("tput").arg("lines").output()
        && let Ok(rows) = String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<u16>()
    {
        return (cols, rows);
    }
    (0, 0)
}

fn check_command(name: &str, args: &[&str]) -> DiagCheck {
    match Command::new(name).args(args).output() {
        Ok(output) => {
            let version = String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            DiagCheck {
                name: name.to_string(),
                status: DiagStatus::Ok,
                value: Some(version),
            }
        }
        Err(_) => DiagCheck {
            name: name.to_string(),
            status: DiagStatus::Error(format!("{name} not found")),
            value: None,
        },
    }
}

fn check_api_key(env_var: &str, provider: &str) -> DiagCheck {
    match std::env::var(env_var) {
        Ok(val) if !val.is_empty() => DiagCheck {
            name: format!("{provider} API Key"),
            status: DiagStatus::Ok,
            value: Some(format!(
                "{}...{}",
                &val[..4.min(val.len())],
                &val[val.len().saturating_sub(4)..],
            )),
        },
        _ => DiagCheck {
            name: format!("{provider} API Key"),
            status: DiagStatus::Warn(format!("{env_var} not set")),
            value: None,
        },
    }
}

fn check_hand_directory() -> DiagCheck {
    let cwd = std::env::current_dir().unwrap_or_default();
    let hand_dir = cwd.join(".hand");

    if hand_dir.is_dir() {
        // Check if writable
        let test_file = hand_dir.join(".diag_test");
        match std::fs::write(&test_file, "test") {
            Ok(_) => {
                let _ = std::fs::remove_file(&test_file);
                DiagCheck {
                    name: ".hand directory".to_string(),
                    status: DiagStatus::Ok,
                    value: Some(hand_dir.display().to_string()),
                }
            }
            Err(e) => DiagCheck {
                name: ".hand directory".to_string(),
                status: DiagStatus::Error(format!("Not writable: {e}")),
                value: Some(hand_dir.display().to_string()),
            },
        }
    } else {
        DiagCheck {
            name: ".hand directory".to_string(),
            status: DiagStatus::Warn(
                "Does not exist (will be created on first session)".to_string(),
            ),
            value: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_diagnostics_returns_checks() {
        let report = run_diagnostics();
        assert!(!report.checks.is_empty());
    }

    #[test]
    fn report_display() {
        let report = run_diagnostics();
        let output = report.to_string();
        assert!(output.contains("System Diagnostics"));
        assert!(output.contains("Summary:"));
    }

    #[test]
    fn diag_status_display() {
        assert_eq!(DiagStatus::Ok.to_string(), "OK");
        assert!(DiagStatus::Warn("test".into()).to_string().contains("WARN"));
        assert!(
            DiagStatus::Error("fail".into())
                .to_string()
                .contains("ERROR")
        );
    }

    #[test]
    fn check_os_always_ok() {
        let check = check_os();
        assert_eq!(check.status, DiagStatus::Ok);
        assert!(check.value.is_some());
    }

    #[test]
    fn report_counts() {
        let report = DiagnosticsReport {
            checks: vec![
                DiagCheck {
                    name: "a".into(),
                    status: DiagStatus::Ok,
                    value: None,
                },
                DiagCheck {
                    name: "b".into(),
                    status: DiagStatus::Warn("w".into()),
                    value: None,
                },
                DiagCheck {
                    name: "c".into(),
                    status: DiagStatus::Error("e".into()),
                    value: None,
                },
            ],
        };
        assert_eq!(report.ok_count(), 1);
        assert_eq!(report.warn_count(), 1);
        assert_eq!(report.error_count(), 1);
    }
}
