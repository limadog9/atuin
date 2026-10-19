use std::collections::{HashMap, HashSet};

use atuin_client::database::Sqlite;
use atuin_client::record::sqlite_store::SqliteStore;
use atuin_client::settings::Settings;
use atuin_client::theme::Theme;
use atuin_history::stats::{Stats, compute, split_common_prefix};
use crossterm::style::{ResetColor, SetAttribute};
use eyre::Result;
use time::{Date, Duration, Month, OffsetDateTime, Time};
use tracing::instrument;

type PackageMatcher = for<'a> fn(&str, &mut shlex::Shlex<'a>) -> bool;

struct PackageCommandRule {
    programs: &'static [&'static str],
    options_with_values: &'static [&'static str],
    matches: PackageMatcher,
}

// Keep each tool's package operations next to its global options. New matchers belong
// here, rather than in a shared list of subcommands that also matches unrelated tools.
const PACKAGE_COMMAND_RULES: &[PackageCommandRule] = &[
    PackageCommandRule {
        programs: &["cargo"],
        options_with_values: &["-C", "-Z", "--config", "--color"],
        matches: |operation, _| {
            matches!(
                operation,
                "add"
                    | "remove"
                    | "rm"
                    | "install"
                    | "uninstall"
                    | "update"
                    | "upgrade"
                    | "search"
                    | "info"
                    | "tree"
                    | "vendor"
                    | "fetch"
                    | "publish"
                    | "owner"
                    | "yank"
                    | "package"
                    | "generate-lockfile"
            )
        },
    },
    PackageCommandRule {
        programs: &["npm"],
        options_with_values: &[
            "--prefix",
            "-w",
            "--workspace",
            "--registry",
            "--userconfig",
            "--cache",
            "--loglevel",
            "--location",
        ],
        matches: |operation, _| {
            matches!(
                operation,
                "install"
                    | "i"
                    | "ci"
                    | "uninstall"
                    | "un"
                    | "remove"
                    | "rm"
                    | "r"
                    | "update"
                    | "upgrade"
                    | "outdated"
                    | "list"
                    | "ls"
                    | "info"
                    | "show"
                    | "search"
                    | "view"
                    | "explain"
                    | "audit"
                    | "fund"
                    | "link"
                    | "unlink"
                    | "rebuild"
                    | "dedupe"
                    | "prune"
                    | "publish"
                    | "unpublish"
                    | "pack"
                    | "cache"
                    | "owner"
            )
        },
    },
    PackageCommandRule {
        programs: &["pnpm"],
        options_with_values: &["-C", "--dir", "-F", "--filter", "--store-dir", "--registry"],
        matches: |operation, _| {
            matches!(
                operation,
                "add"
                    | "install"
                    | "i"
                    | "remove"
                    | "rm"
                    | "uninstall"
                    | "update"
                    | "up"
                    | "upgrade"
                    | "outdated"
                    | "list"
                    | "ls"
                    | "why"
                    | "audit"
                    | "import"
                    | "fetch"
                    | "store"
                    | "prune"
                    | "link"
                    | "unlink"
                    | "rebuild"
                    | "dedupe"
                    | "publish"
                    | "pack"
                    | "deploy"
                    | "config"
            )
        },
    },
    PackageCommandRule {
        programs: &["yarn"],
        options_with_values: &[
            "--cwd",
            "--cache-folder",
            "--modules-folder",
            "--registry",
            "--use-yarnrc",
        ],
        matches: |operation, _| {
            matches!(
                operation,
                "" | "add"
                    | "install"
                    | "remove"
                    | "up"
                    | "upgrade"
                    | "upgrade-interactive"
                    | "outdated"
                    | "list"
                    | "info"
                    | "why"
                    | "audit"
                    | "link"
                    | "unlink"
                    | "dedupe"
                    | "import"
                    | "publish"
                    | "pack"
                    | "cache"
                    | "npm"
            )
        },
    },
    PackageCommandRule {
        programs: &["pipenv"],
        options_with_values: &[],
        matches: |operation, _| {
            matches!(
                operation,
                "install"
                    | "uninstall"
                    | "update"
                    | "upgrade"
                    | "sync"
                    | "lock"
                    | "clean"
                    | "check"
                    | "graph"
                    | "requirements"
            )
        },
    },
    PackageCommandRule {
        programs: &["poetry"],
        options_with_values: &["-C", "--directory", "-P", "--project"],
        matches: |operation, _| {
            matches!(
                operation,
                "add"
                    | "remove"
                    | "install"
                    | "update"
                    | "sync"
                    | "lock"
                    | "check"
                    | "show"
                    | "search"
                    | "export"
                    | "build"
                    | "publish"
                    | "cache"
            )
        },
    },
    PackageCommandRule {
        programs: &["bundle"],
        options_with_values: &["--gemfile"],
        matches: |operation, _| {
            matches!(
                operation,
                "" | "install"
                    | "update"
                    | "add"
                    | "remove"
                    | "lock"
                    | "check"
                    | "list"
                    | "info"
                    | "show"
                    | "outdated"
                    | "cache"
                    | "package"
                    | "clean"
                    | "pristine"
                    | "publish"
            )
        },
    },
    PackageCommandRule {
        programs: &["composer"],
        options_with_values: &["-d", "--working-dir"],
        matches: |operation, _| {
            matches!(
                operation,
                "install"
                    | "update"
                    | "require"
                    | "remove"
                    | "show"
                    | "info"
                    | "search"
                    | "outdated"
                    | "browse"
                    | "depends"
                    | "why"
                    | "prohibits"
                    | "why-not"
                    | "audit"
                    | "fund"
                    | "licenses"
                    | "clear-cache"
                    | "clearcache"
                    | "dump-autoload"
                    | "dumpautoload"
                    | "validate"
            )
        },
    },
    PackageCommandRule {
        programs: &["cabal"],
        options_with_values: &[],
        matches: |operation, _| {
            let operation = operation
                .strip_prefix("v1-")
                .or_else(|| operation.strip_prefix("v2-"))
                .unwrap_or(operation);
            matches!(
                operation,
                "install"
                    | "update"
                    | "list"
                    | "info"
                    | "get"
                    | "fetch"
                    | "freeze"
                    | "outdated"
                    | "upload"
            )
        },
    },
    PackageCommandRule {
        programs: &["uv"],
        options_with_values: &[
            "--directory",
            "--project",
            "--config-file",
            "--cache-dir",
            "--color",
        ],
        matches: |operation, words| match operation {
            "tool" => matches!(
                words.next().as_deref(),
                Some("install" | "uninstall" | "upgrade" | "list")
            ),
            "pip" | "add" | "remove" | "sync" | "lock" | "tree" | "export" | "cache" | "build" => {
                true
            }
            _ => false,
        },
    },
    PackageCommandRule {
        programs: &["dotnet"],
        options_with_values: &[],
        matches: dotnet_package_operation,
    },
    PackageCommandRule {
        programs: &["go"],
        options_with_values: &["-C"],
        matches: |operation, words| {
            operation == "get"
                || (operation == "mod"
                    && matches!(
                        words.next().as_deref(),
                        Some("download" | "tidy" | "vendor" | "graph" | "verify" | "edit" | "why")
                    ))
        },
    },
    PackageCommandRule {
        programs: &["nix"],
        options_with_values: &[],
        matches: |operation, words| {
            operation == "profile"
                || (operation == "flake"
                    && matches!(words.next().as_deref(), Some("update" | "lock")))
        },
    },
    PackageCommandRule {
        programs: &["mix"],
        options_with_values: &[],
        matches: |operation, _| {
            operation.starts_with("deps.")
                || operation.starts_with("hex.")
                || matches!(operation, "local.hex" | "local.rebar")
        },
    },
    PackageCommandRule {
        programs: &["rebar3"],
        options_with_values: &[],
        matches: |operation, _| {
            matches!(operation, "get-deps" | "update" | "upgrade" | "hex" | "lock" | "tree")
        },
    },
    PackageCommandRule {
        programs: &["gradle"],
        options_with_values: &[],
        matches: |operation, words| {
            std::iter::once(operation.to_owned()).chain(words).any(|task| {
                matches!(
                    task.as_str(),
                    "dependencies" | "dependencyInsight" | "publish" | "publishToMavenLocal"
                )
            })
        },
    },
    PackageCommandRule {
        programs: &["maven", "mvn"],
        options_with_values: &[],
        matches: |operation, words| {
            std::iter::once(operation.to_owned()).chain(words).any(|goal| {
                goal.starts_with("dependency:") || matches!(goal.as_str(), "install" | "deploy")
            })
        },
    },
    PackageCommandRule {
        programs: &["pipx"],
        options_with_values: &[],
        matches: |operation, _| operation != "run",
    },
    PackageCommandRule {
        programs: DEDICATED_PACKAGE_MANAGERS,
        options_with_values: &[],
        matches: |_, _| true,
    },
];

fn dotnet_package_operation(operation: &str, words: &mut shlex::Shlex<'_>) -> bool {
    match operation {
        "restore" | "nuget" => true,
        "add" | "remove" | "list" => match words.next().as_deref() {
            Some("package") => true,
            Some("reference") | None => false,
            Some(project) => {
                !project.starts_with('-') && words.next().as_deref() == Some("package")
            }
        },
        "package" => matches!(words.next().as_deref(), Some("add" | "remove" | "list" | "search")),
        "tool" => matches!(
            words.next().as_deref(),
            Some("install" | "uninstall" | "update" | "restore" | "list")
        ),
        _ => false,
    }
}

fn package_subcommand(
    program: &str,
    options_with_values: &[&str],
    words: &mut shlex::Shlex<'_>,
) -> String {
    while let Some(word) = words.next() {
        if word == "--" {
            return words.next().unwrap_or_default();
        }
        if word.starts_with('-') {
            if options_with_values.contains(&word.as_str()) {
                words.next();
            }
        } else if !(program == "cargo" && word.starts_with('+')) {
            return word;
        }
    }
    String::new()
}

fn is_package_command(settings: &Settings, command: &str) -> bool {
    let command = command.trim_start();
    let command = split_common_prefix(settings, command).map_or(command, |(_, rest)| rest);
    // Parse only what the matcher needs; trailing arguments may use another shell's syntax.
    let mut words = shlex::Shlex::new(command);
    let Some(program) = words.next() else {
        return false;
    };
    let Some(rule) =
        PACKAGE_COMMAND_RULES.iter().find(|rule| rule.programs.contains(&program.as_str()))
    else {
        return false;
    };
    let operation = package_subcommand(&program, rule.options_with_values, &mut words);
    (rule.matches)(&operation, &mut words)
}

const DEDICATED_PACKAGE_MANAGERS: &[&str] = &[
    "pip",
    "pip3",
    "brew",
    "apt",
    "apt-get",
    "apk",
    "pacman",
    "yay",
    "paru",
    "yum",
    "dnf",
    "dnf5",
    "rpm",
    "rpm-ostree",
    "zypper",
    "pkg",
    "chocolatey",
    "choco",
    "scoop",
    "winget",
    "gem",
    "shards",
    "nuget",
    "hex",
    "nix-env",
    "opam",
];

#[derive(Debug)]
struct WrappedStats {
    nav_commands: usize,
    pkg_commands: usize,
    error_rate: f64,
    first_half_commands: Vec<(String, usize)>,
    second_half_commands: Vec<(String, usize)>,
    git_percentage: f64,
    busiest_hour: Option<(String, usize)>,
}

impl WrappedStats {
    #[allow(clippy::too_many_lines, clippy::cast_precision_loss)]
    fn new(
        settings: &Settings,
        stats: &Stats,
        history: &[atuin_client::history::History],
        alias_map: &HashMap<String, String>,
    ) -> Self {
        // Helper to expand alias to its first command word
        let expand_alias = |cmd: &str| -> String {
            alias_map.get(cmd).map_or_else(
                || cmd.to_string(),
                |expanded| expanded.split_whitespace().next().unwrap_or(cmd).to_string(),
            )
        };

        let nav_commands = stats
            .top
            .iter()
            .filter(|(cmd, _)| {
                let cmd = &cmd[0];
                cmd == "cd" || cmd == "ls" || cmd == "pwd" || cmd == "pushd" || cmd == "popd"
            })
            .map(|(_, count)| count)
            .sum();

        let pkg_commands =
            history.iter().filter(|h| is_package_command(settings, &h.command)).count();

        // Error analysis
        let mut command_errors: HashMap<String, (usize, usize)> = HashMap::new(); // (total_uses, errors)
        let midyear = history[0].timestamp + Duration::days(182); // Split year in half

        let mut first_half_commands: HashMap<String, usize> = HashMap::new();
        let mut second_half_commands: HashMap<String, usize> = HashMap::new();
        let mut hours: HashMap<String, usize> = HashMap::new();

        for entry in history {
            let command = entry.command.trim_start();
            let command =
                split_common_prefix(settings, command).map_or(command, |(prefix, rest)| {
                    if rest.is_empty() {
                        prefix
                    } else {
                        rest
                    }
                });
            let raw_cmd = command.split_whitespace().next().unwrap_or("");
            let cmd = expand_alias(raw_cmd);
            let (total, errors) = command_errors.entry(cmd.clone()).or_insert((0, 0));
            *total += 1;
            if entry.exit != 0 {
                *errors += 1;
            }

            // Track command evolution
            if entry.timestamp < midyear {
                *first_half_commands.entry(cmd.clone()).or_default() += 1;
            } else {
                *second_half_commands.entry(cmd).or_default() += 1;
            }

            // Track hourly distribution
            let local_time = entry
                .timestamp
                .to_offset(time::UtcOffset::current_local_offset().unwrap_or(settings.timezone.0));
            let hour = format!("{:02}:00", local_time.time().hour());
            *hours.entry(hour).or_default() += 1;
        }

        let total_errors: usize = command_errors.values().map(|(_, errors)| errors).sum();
        let total_commands: usize = command_errors.values().map(|(total, _)| total).sum();
        let error_rate = total_errors as f64 / total_commands as f64;

        // Process command evolution data
        let mut first_half: Vec<_> = first_half_commands.into_iter().collect();
        let mut second_half: Vec<_> = second_half_commands.into_iter().collect();
        first_half.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        second_half.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        first_half.truncate(5);
        second_half.truncate(5);

        // Calculate git percentage
        let git_commands: usize = stats
            .top
            .iter()
            .filter(|(cmd, _)| cmd[0].starts_with("git"))
            .map(|(_, count)| count)
            .sum();
        let git_percentage = git_commands as f64 / stats.total_commands as f64;

        // Find busiest hour
        let busiest_hour = hours.into_iter().max_by_key(|(_, count)| *count);

        Self {
            nav_commands,
            pkg_commands,
            error_rate,
            first_half_commands: first_half,
            second_half_commands: second_half,
            git_percentage,
            busiest_hour,
        }
    }
}

pub fn print_wrapped_header(year: i32) {
    let reset = ResetColor;
    let bold = SetAttribute(crossterm::style::Attribute::Bold);

    println!("{bold}╭────────────────────────────────────╮{reset}");
    println!("{bold}│        ATUIN WRAPPED {year}          │{reset}");
    println!("{bold}│    Your Year in Shell History      │{reset}");
    println!("{bold}╰────────────────────────────────────╯{reset}");
    println!();
}

#[allow(clippy::cast_precision_loss)]
fn print_fun_facts(wrapped_stats: &WrappedStats, stats: &Stats, year: i32) {
    let reset = ResetColor;
    let bold = SetAttribute(crossterm::style::Attribute::Bold);

    if wrapped_stats.git_percentage > 0.05 {
        println!(
            "{bold}🌟 You're a Git Power User!{reset} {bold}{:.1}%{reset} of your commands were \
             Git operations\n",
            wrapped_stats.git_percentage * 100.0
        );
    }
    // Navigation patterns
    let nav_percentage = wrapped_stats.nav_commands as f64 / stats.total_commands as f64 * 100.0;
    if nav_percentage > 0.05 {
        println!(
            "{bold}🚀 You're a Navigator!{reset} {bold}{nav_percentage:.1}%{reset} of your time \
             was spent navigating directories\n",
        );
    }

    // Command vocabulary
    println!(
        "{bold}📚 Command Vocabulary{reset}: You know {bold}{}{reset} unique commands\n",
        stats.unique_commands
    );

    // Package management
    println!(
        "{bold}📦 Package Management{reset}: You ran {bold}{}{reset} package-related commands\n",
        wrapped_stats.pkg_commands
    );

    // Error patterns
    let error_percentage = wrapped_stats.error_rate * 100.0;
    println!(
        "{bold}🚨 Error Analysis{reset}: Your commands failed {bold}{error_percentage:.1}%{reset} \
         of the time\n",
    );

    // Command evolution
    println!("🔍 Command Evolution:");

    // print stats for each half and compare
    println!("  {bold}Top Commands{reset} in the first half of {year}:");
    for (cmd, count) in wrapped_stats.first_half_commands.iter().take(3) {
        println!("    {bold}{cmd}{reset} ({count} times)");
    }

    println!("  {bold}Top Commands{reset} in the second half of {year}:");
    for (cmd, count) in wrapped_stats.second_half_commands.iter().take(3) {
        println!("    {bold}{cmd}{reset} ({count} times)");
    }

    // Find new favorite commands (in top 5 of second half but not in first half)
    let first_half_set: HashSet<_> =
        wrapped_stats.first_half_commands.iter().map(|(cmd, _)| cmd).collect();
    let new_favorites: Vec<_> = wrapped_stats
        .second_half_commands
        .iter()
        .filter(|(cmd, _)| !first_half_set.contains(cmd))
        .take(2)
        .collect();

    if !new_favorites.is_empty() {
        println!("  {bold}New favorites{reset} in the second half:");
        for (cmd, count) in new_favorites {
            println!("    {bold}{cmd}{reset} ({count} times)");
        }
    }

    // Time patterns
    if let Some((hour, count)) = &wrapped_stats.busiest_hour {
        println!("\n🕘 Most Productive Hour: {bold}{hour}{reset} ({count} commands)");

        // Night owl or early bird
        let hour_num = hour.split(':').next().unwrap_or("0").parse::<u32>().unwrap_or(0);
        if hour_num >= 22 || hour_num <= 4 {
            println!("  You're quite the night owl! 🦉");
        } else if (5..=7).contains(&hour_num) {
            println!("  Early bird gets the worm! 🐦");
        }
    }

    println!();
}

#[instrument(level = "trace", skip_all, err)]
pub async fn run(
    year: Option<i32>,
    db: &Sqlite,
    settings: &Settings,
    _store: SqliteStore,
    theme: &Theme,
) -> Result<()> {
    let now = OffsetDateTime::now_utc().to_offset(settings.timezone.0);
    let month = now.month();

    // If we're in December, then wrapped is for the current year. If not, it's for the previous year
    let year = year.unwrap_or_else(|| {
        if month == Month::December {
            now.year()
        } else {
            now.year() - 1
        }
    });

    let start = OffsetDateTime::new_in_offset(
        Date::from_calendar_date(year, Month::January, 1).unwrap(),
        Time::MIDNIGHT,
        now.offset(),
    );
    let end = OffsetDateTime::new_in_offset(
        Date::from_calendar_date(year, Month::December, 31).unwrap(),
        Time::MIDNIGHT + Duration::days(1) - Duration::nanoseconds(1),
        now.offset(),
    );

    let history = db.range(start, end).await?;
    if history.is_empty() {
        println!(
            "Your history for {year} is empty!\nMaybe 'atuin import' could help you import your \
             previous history 🪄"
        );
        return Ok(());
    }

    let Some(stats) = compute(settings, &history, 10, 1) else {
        println!(
            "No commands found in your {year} history. Run a command or check your config for for \
             commands ignored by stats.ignored_commands."
        );
        return Ok(());
    };
    let alias_map = HashMap::new();
    let wrapped_stats = WrappedStats::new(settings, &stats, &history, &alias_map);

    // Print wrapped format
    print_wrapped_header(year);

    println!("🎉 In {year}, you typed {} commands!", stats.total_commands);
    println!("   That's ~{} commands every day\n", stats.total_commands / 365);

    println!("Your Top Commands:");
    atuin_history::stats::pretty_print(stats.clone(), 1, theme);
    println!();

    print_fun_facts(&wrapped_stats, &stats, year);

    Ok(())
}

#[cfg(test)]
mod tests {
    use atuin_client::history::History;
    use rstest::{fixture, rstest};
    use time::macros::datetime;

    use super::*;

    #[fixture]
    fn history() -> History {
        History::import()
            .timestamp(datetime!(2024-01-01 0:00 UTC))
            .command("echo hello")
            .build()
            .into()
    }

    #[rstest]
    #[case::cargo_add_serde("cargo add serde", 1)]
    #[case::cargo_remove_serde("cargo remove serde", 1)]
    #[case::cargo_install_atuin("cargo install atuin", 1)]
    #[case::cargo_nightly_install_atuin("cargo +nightly install atuin", 1)]
    #[case::cargo_build("cargo build", 0)]
    #[case::cargo_test("cargo test", 0)]
    #[case::cargo_check("cargo check", 0)]
    #[case::cargo_r("cargo r", 0)]
    #[case::cargo_run("cargo run", 0)]
    #[case::cargo_nightly_fmt("cargo +nightly fmt", 0)]
    #[case::uv_add_requests("uv add requests", 1)]
    #[case::uv_pip_install_requests("uv pip install requests", 1)]
    #[case::uv_tool_install_ruff("uv tool install ruff", 1)]
    #[case::uv_tool_run_ruff("uv tool run ruff", 0)]
    #[case::uv_run_python_script_py("uv run python script.py", 0)]
    #[case::uv_venv("uv venv", 0)]
    #[case::npm_install("npm install", 1)]
    #[case::npm_i("npm i", 1)]
    #[case::npm_un_typescript("npm un typescript", 1)]
    #[case::npm_ci("npm ci", 1)]
    #[case::npm_prefix_app_install("npm --prefix ./app install", 1)]
    #[case::npm_prefix_app_install_2("npm --prefix=./app install", 1)]
    #[case::npm_prefix_my_app_install("npm --prefix './my app' install", 1)]
    #[case::npm_registry_https_registry_npmjs_org_install(
        "npm --registry https://registry.npmjs.org install",
        1
    )]
    #[case::npm_workspace_app_install("npm --workspace app install", 1)]
    #[case::npm_prefix_install_run_build("npm --prefix install run build", 0)]
    #[case::npm_prefix_app_run_build("npm --prefix ./app run build", 0)]
    #[case::uv_directory_app_add_requests("uv --directory ./app add requests", 1)]
    #[case::uv_directory_app_add_requests_2("uv --directory=./app add requests", 1)]
    #[case::uv_directory_my_app_add_requests("uv --directory './my app' add requests", 1)]
    #[case::uv_project_app_sync("uv --project ./app sync", 1)]
    #[case::uv_directory_add_run_python("uv --directory add run python", 0)]
    #[case::uv_directory_app_run_python("uv --directory ./app run python", 0)]
    #[case::pnpm_c_app_add_typescript("pnpm -C ./app add typescript", 1)]
    #[case::pnpm_filter_app_add_typescript("pnpm --filter app add typescript", 1)]
    #[case::pnpm_filter_add_exec_tsc("pnpm --filter add exec tsc", 0)]
    #[case::yarn_cwd_app("yarn --cwd ./app", 1)]
    #[case::yarn_cwd_app_start("yarn --cwd ./app start", 0)]
    #[case::bundle_gemfile_gemfile("bundle --gemfile ./Gemfile", 1)]
    #[case::cargo_color_always_add_serde("cargo --color always add serde", 1)]
    #[case::cargo_nightly_z_unstable_options_install_atuin(
        "cargo +nightly -Z unstable-options install atuin",
        1
    )]
    #[case::cargo_config_install_build("cargo --config install build", 0)]
    #[case::poetry_c_app_add_requests("poetry -C ./app add requests", 1)]
    #[case::composer_working_dir_app_require_monolog_monolog(
        "composer --working-dir ./app require monolog/monolog",
        1
    )]
    #[case::go_c_app_mod_tidy("go -C ./app mod tidy", 1)]
    #[case::npm_run_build("npm run build", 0)]
    #[case::npm_test("npm test", 0)]
    #[case::pnpm_add_typescript("pnpm add typescript", 1)]
    #[case::pnpm_i("pnpm i", 1)]
    #[case::pnpm_exec_tsc("pnpm exec tsc", 0)]
    #[case::yarn("yarn", 1)]
    #[case::yarn_add_typescript("yarn add typescript", 1)]
    #[case::yarn_start("yarn start", 0)]
    #[case::pipenv_install("pipenv install", 1)]
    #[case::pipenv_run_python("pipenv run python", 0)]
    #[case::poetry_add_requests("poetry add requests", 1)]
    #[case::poetry_run_python("poetry run python", 0)]
    #[case::bundle_install("bundle install", 1)]
    #[case::bundle("bundle", 1)]
    #[case::bundle_exec_rake("bundle exec rake", 0)]
    #[case::composer_require_monolog_monolog("composer require monolog/monolog", 1)]
    #[case::composer_run_script_test("composer run-script test", 0)]
    #[case::dotnet_add_package_newtonsoft_json("dotnet add package Newtonsoft.Json", 1)]
    #[case::dotnet_add_myapp_csproj_package_newtonsoft_json(
        "dotnet add MyApp.csproj package Newtonsoft.Json",
        1
    )]
    #[case::dotnet_list_myapp_csproj_package("dotnet list MyApp.csproj package", 1)]
    #[case::dotnet_remove_myapp_csproj_package_newtonsoft_json(
        "dotnet remove MyApp.csproj package Newtonsoft.Json",
        1
    )]
    #[case::dotnet_add_my_app_csproj_package_newtonsoft_json(
        "dotnet add 'My App.csproj' package Newtonsoft.Json",
        1
    )]
    #[case::dotnet_list_mysolution_sln_package("dotnet list MySolution.sln package", 1)]
    #[case::dotnet_add_myapp_csproj_reference_other_csproj(
        "dotnet add MyApp.csproj reference Other.csproj",
        0
    )]
    #[case::dotnet_list_myapp_csproj_reference("dotnet list MyApp.csproj reference", 0)]
    #[case::dotnet_remove_myapp_csproj_reference_other_csproj(
        "dotnet remove MyApp.csproj reference Other.csproj",
        0
    )]
    #[case::dotnet_add_reference_package("dotnet add reference package", 0)]
    #[case::dotnet_list_reference_package("dotnet list reference package", 0)]
    #[case::dotnet_restore("dotnet restore", 1)]
    #[case::dotnet_run("dotnet run", 0)]
    #[case::dotnet_add_reference_other_csproj("dotnet add reference Other.csproj", 0)]
    #[case::dotnet_tool_run_dotnetsay("dotnet tool run dotnetsay", 0)]
    #[case::mix_deps_get("mix deps.get", 1)]
    #[case::mix_test("mix test", 0)]
    #[case::rebar3_get_deps("rebar3 get-deps", 1)]
    #[case::rebar3_compile("rebar3 compile", 0)]
    #[case::cabal_install_hlint("cabal install hlint", 1)]
    #[case::cabal_v2_install_hlint("cabal v2-install hlint", 1)]
    #[case::cabal_v2_build("cabal v2-build", 0)]
    #[case::cabal_build("cabal build", 0)]
    #[case::go_get_example_com_pkg("go get example.com/pkg", 1)]
    #[case::go_mod_tidy("go mod tidy", 1)]
    #[case::go_build("go build", 0)]
    #[case::nix_profile_install_nixpkgs_hello("nix profile install nixpkgs#hello", 1)]
    #[case::nix_flake_update("nix flake update", 1)]
    #[case::nix_build("nix build", 0)]
    #[case::gradle_dependencies("gradle dependencies", 1)]
    #[case::gradle_build("gradle build", 0)]
    #[case::mvn_dependency_resolve("mvn dependency:resolve", 1)]
    #[case::mvn_test("mvn test", 0)]
    #[case::sudo_apt_install_hello("sudo apt install hello", 1)]
    #[case::t_doas_apk_add_hello(" \t doas apk add hello", 1)]
    #[case::pip_install_requests("pip install requests", 1)]
    #[case::pip_install_c_projects_demo(r"pip install C:\projects\demo\", 1)]
    #[case::npm_install_c_projects_demo(r"npm install C:\projects\demo\", 1)]
    #[case::uv_pip_install_c_projects_demo(r"uv pip install C:\projects\demo\", 1)]
    #[case::cargo_install_path_c_projects_demo(r"cargo install --path C:\projects\demo\", 1)]
    #[case::dotnet_add_myapp_csproj_package_newtonsoft_json_configfile_c_config(
        r"dotnet add MyApp.csproj package Newtonsoft.Json --configfile C:\config\",
        1
    )]
    #[case::npm_run_build_c_projects_demo(r"npm run build C:\projects\demo\", 0)]
    #[case::uv_run_python_c_projects_demo(r"uv run python C:\projects\demo\", 0)]
    #[case::dotnet_list_myapp_csproj_reference_c_projects_demo(
        r"dotnet list MyApp.csproj reference C:\projects\demo\",
        0
    )]
    #[case::brew_upgrade("brew upgrade", 1)]
    #[case::pacman_s_hello("pacman -S hello", 1)]
    #[case::nix_env_i_hello("nix-env -i hello", 1)]
    #[case::cargo_watch("cargo-watch", 0)]
    #[case::npm_not_a_package_manager("npm-not-a-package-manager", 0)]
    #[case::go_getty("go getty", 0)]
    #[case::echo_cargo_install_atuin("echo cargo install atuin", 0)]
    #[case::empty("", 0)]
    #[case::cargo_does_not_use_npm_ci("cargo ci", 0)]
    #[case::poetry_does_not_use_npm_ci("poetry ci", 0)]
    #[case::npm_does_not_use_cargo_vendor("npm vendor", 0)]
    fn package_count_excludes_development_commands(
        mut history: History,
        #[case] command: &str,
        #[case] expected: usize,
    ) {
        history.command = command.to_owned();
        let stats = Stats {
            total_commands: 1,
            unique_commands: 1,
            top: vec![],
        };
        let wrapped = WrappedStats::new(&Settings::utc(), &stats, &[history], &HashMap::new());
        assert_eq!(wrapped.pkg_commands, expected);
    }

    #[rstest]
    #[case::default_prefix(None, "sudo echo hello", "echo")]
    #[case::default_doas(None, "doas echo hello", "echo")]
    #[case::custom_prefix(Some(vec!["please"]), "please echo hello", "echo")]
    #[case::longest_prefix(Some(vec!["sudo", "sudo -u root"]), "sudo -u root echo hello", "echo")]
    #[case::empty_prefixes(Some(vec![]), "sudo echo hello", "sudo")]
    #[case::unconfigured_prefix(Some(vec!["please"]), "sudo echo hello", "sudo")]
    #[case::prefix_only(None, "sudo", "sudo")]
    #[case::whitespace(None, " \t sudo \t echo hello", "echo")]
    #[case::empty(None, " \t ", "")]
    #[case::subcommand(None, "sudo git status", "git")]
    #[case::alias(None, "ll /tmp", "ls")]
    #[case::prefixed_alias(None, "sudo ll /tmp", "ls")]
    #[case::multiword_prefix_only(Some(vec!["sudo -u root"]), "sudo -u root  ", "sudo")]
    #[case::one_removal(None, "sudo doas echo hello", "doas")]
    #[case::partial_word(None, "sudoedit file", "edit")]
    fn evolution_respects_common_prefix(
        #[case] prefixes: Option<Vec<&str>>,
        #[case] command: &str,
        #[case] expected: &str,
    ) {
        let mut settings = Settings::utc();
        if let Some(prefixes) = prefixes {
            settings.stats.common_prefix = prefixes.into_iter().map(str::to_owned).collect();
        }
        let history: Vec<History> =
            [datetime!(2024-01-01 0:00 UTC), datetime!(2024-10-01 0:00 UTC)]
                .into_iter()
                .flat_map(|timestamp| {
                    [(command, 0), (expected, 1)].map(|(command, exit)| {
                        History::import()
                            .timestamp(timestamp)
                            .command(command)
                            .exit(exit)
                            .build()
                            .into()
                    })
                })
                .collect();
        let stats = Stats {
            total_commands: 4,
            unique_commands: 2,
            top: vec![],
        };
        let aliases = HashMap::from([("ll".to_string(), "ls -l".to_string())]);

        let wrapped = WrappedStats::new(&settings, &stats, &history, &aliases);

        assert_eq!(wrapped.first_half_commands, vec![(expected.to_string(), 2)]);
        assert_eq!(wrapped.second_half_commands, vec![(expected.to_string(), 2)]);
        assert!((wrapped.error_rate - 0.5).abs() < f64::EPSILON);
    }
}
