//! Application-level command dispatch and sub-command handlers.
use std::path::PathBuf;

use anyhow::Result;

use crate::cli::{Cli, Commands, ProfileAction};
use crate::config::{Config, ProfileKind};
use crate::daemon::Daemon;
use crate::platform::linux;
use crate::rules::{Matcher, ProcessContext, RuleSet};

pub async fn run(cli: Cli) -> Result<()> {
    let mut config = if let Some(path) = &cli.config {
        Config::load_from(path)?
    } else {
        Config::load()?
    };

    match cli.command {
        Commands::Daemon { dry_run } => {
            if dry_run {
                config.dry_run = true;
            }
            Daemon::new(config).run().await
        }
        Commands::Profile { action } => run_profile(config, action),
        Commands::Explain { target } => run_explain(&config, &target),
        Commands::Status => run_status(&config),
        Commands::Check { path } => run_check(&config, path),
    }
}

fn run_profile(config: Config, action: ProfileAction) -> Result<()> {
    match action {
        ProfileAction::List => {
            println!("Available profiles:");
            println!("  default   - Balanced. No special boosting.");
            println!(
                "  gaming    - Boost interactive and game processes, demote background tasks."
            );
            println!("  lowpower  - Prefer lower CPU pressure; demote non-critical tasks.");
            println!("  server    - Prioritize daemons and network services.");
        }
        ProfileAction::Get => {
            println!("{}", config.profile);
        }
        ProfileAction::Set { name } => {
            let _profile: ProfileKind = name.parse()?;
            println!(
                "Set 'profile = \"{name}\"' in your config file, then restart the daemon \
                 (SIGHUP reloads rules only; profile changes require a full restart)."
            );
        }
    }

    Ok(())
}

fn run_explain(config: &Config, target: &str) -> Result<()> {
    let ruleset = RuleSet::load_from_dirs(&config.rules_dirs)?;
    let resolved = ruleset.validate()?;
    let matcher = Matcher::new(resolved);

    let ctx = if let Ok(pid) = target.parse::<u32>() {
        ProcessContext::from_pid(pid)?
    } else {
        ProcessContext {
            pid: 0,
            ppid: 0,
            start_time_ticks: 0,
            comm: target.to_string(),
            comm_lowercase: target.to_lowercase(),
            exe: None,
            exe_name_lowercase: None,
            cmdline: None,
        }
    };

    let result = matcher.explain(&ctx);

    if let Some(rule) = &result.matched {
        println!("Matched rule: {}", rule.name);
        if let Some(nice) = rule.nice {
            println!("  nice:           {nice}");
        }
        if let Some(ioclass) = rule.ioclass {
            println!("  ioclass:        {ioclass:?}");
            if let Some(ionice) = rule.ionice {
                println!("  ionice:         {ionice}");
            }
        }
        if let Some(oom_score_adj) = rule.oom_score_adj {
            println!("  oom_score_adj:  {oom_score_adj}");
        }
        if let Some(cgroup) = &rule.cgroup {
            println!("  cgroup:         {cgroup}");
        }
        if let Some(cgroup_weight) = rule.cgroup_weight {
            println!("  cgroup_weight:  {cgroup_weight}");
        }
    } else {
        println!(
            "No rule matched '{}' ({} rules checked)",
            target,
            result.attempts.len()
        );
    }

    Ok(())
}

fn run_status(config: &Config) -> Result<()> {
    let scheduler = linux::detect();
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

    Ok(())
}

fn run_check(config: &Config, path: Option<PathBuf>) -> Result<()> {
    let dirs = path.map_or_else(|| config.rules_dirs.clone(), |dir| vec![dir]);
    let ruleset = RuleSet::load_from_dirs(&dirs)?;
    let resolved = ruleset.validate()?;
    println!(
        "OK - {} types, {} rules loaded and resolved without errors",
        ruleset.types.len(),
        resolved.len()
    );
    Ok(())
}
