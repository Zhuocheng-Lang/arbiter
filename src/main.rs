use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

use arbiter::cli::{Cli, Commands, ProfileAction};
use arbiter::config::Config;
use arbiter::daemon::Daemon;
use arbiter::matcher::ProcessContext;
use arbiter::rules::RuleSet;
use arbiter::scx;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // ── logging ───────────────────────────────────────────────────────────────
    let log_level = cli.log_level.as_deref().unwrap_or("info");
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(log_level)),
        )
        .init();

    // ── config ────────────────────────────────────────────────────────────────
    let mut config = match &cli.config {
        Some(path) => Config::load_from(path)?,
        None => Config::load()?,
    };

    match cli.command {
        // ── daemon ────────────────────────────────────────────────────────────
        Commands::Daemon { dry_run } => {
            if dry_run {
                config.dry_run = true;
            }
            Daemon::new(config).run().await?;
        }

        // ── profile ───────────────────────────────────────────────────────────
        Commands::Profile { action } => match action {
            ProfileAction::List => {
                println!("Available profiles:");
                println!("  default   — Balanced. No special boosting.");
                println!("  gaming    — Boost interactive/game processes, demote bg tasks.");
                println!("  lowpower  — Prefer lower CPU pressure; demote non-critical.");
                println!("  server    — Prioritize daemons and network services.");
            }
            ProfileAction::Get => {
                println!("{}", config.profile);
            }
            ProfileAction::Set { name } => {
                let _profile: arbiter::config::ProfileKind = name.parse()?;
                println!(
                    "Set `profile = \"{name}\"` in your config file, then restart the daemon \
                     (SIGHUP reloads rules only; profile changes require a full restart)."
                );
            }
        },

        // ── explain ───────────────────────────────────────────────────────────
        Commands::Explain { target } => {
            let ruleset = RuleSet::load_from_dirs(&config.rules_dirs)?;
            let resolved = ruleset.validate()?;
            let matcher = arbiter::matcher::Matcher::new(resolved);

            let ctx = if let Ok(pid) = target.parse::<u32>() {
                ProcessContext::from_pid(pid)?
            } else {
                // Synthesise a minimal context for name-based matching.
                ProcessContext {
                    pid: 0,
                    ppid: 0,
                    start_time_ticks: 0,
                    comm: target.clone(),
                    exe: None,
                    cmdline: None,
                }
            };

            let result = matcher.explain(&ctx);

            if let Some(rule) = &result.matched {
                println!("✓ Matched rule:   {}", rule.name);
                if let Some(n) = rule.nice {
                    println!("  nice:           {n}");
                }
                if let Some(o) = rule.oom_score_adj {
                    println!("  oom_score_adj:  {o}");
                }
                if let Some(c) = &rule.cgroup {
                    println!("  cgroup:         {c}");
                }
                if let Some(w) = rule.cgroup_weight {
                    println!("  cgroup_weight:  {w}");
                }
            } else {
                println!(
                    "✗ No rule matched '{}' ({} rules checked)",
                    target,
                    result.attempts.len()
                );
            }
        }

        // ── status ────────────────────────────────────────────────────────────
        Commands::Status => {
            let scheduler = scx::detect();
            let ruleset = RuleSet::load_from_dirs(&config.rules_dirs)?;
            let resolved = ruleset.validate()?;

            println!("Scheduler : {scheduler}");
            println!("Strategy  : {:?}", scheduler.strategy());
            println!("Profile   : {}", config.profile);
            println!("Types     : {}", ruleset.types.len());
            println!(
                "Rules     : {} loaded, {} resolved",
                ruleset.rules.len(),
                resolved.len()
            );
            println!("Dry-run   : {}", config.dry_run);
        }

        // ── check ─────────────────────────────────────────────────────────────
        Commands::Check { path } => {
            let dirs = if let Some(p) = path {
                vec![p]
            } else {
                config.rules_dirs.clone()
            };
            let rs = RuleSet::load_from_dirs(&dirs)?;
            // validate() returns Err (non-zero exit) if any rule fails to resolve.
            let resolved = rs.validate()?;
            println!(
                "OK — {} types, {} rules loaded and resolved without errors",
                rs.types.len(),
                resolved.len()
            );
        }
    }

    Ok(())
}
