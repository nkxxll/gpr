use clap::{Parser, ValueEnum};
use git2::{BranchType, Repository};
use regex::Regex;
use std::process::{Command, exit};
use url::form_urlencoded;

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "Open pull request URLs in browser for the current git repository"
)]
struct Args {
    /// Branch to create pull request from (defaults to current branch)
    #[arg(short, long)]
    branch: Option<String>,

    /// Target branch for the pull request (usually main or master)
    #[arg(short, long)]
    target: Option<String>,

    /// Remote to use (defaults to upstream if it exists, otherwise origin)
    #[arg(short, long)]
    remote: Option<String>,

    /// Force using the specified remote, ignoring upstream even if it exists
    #[arg(short, long)]
    force_remote: bool,

    /// Git hosting service to use
    #[arg(short, long, value_enum)]
    service: Option<Service>,

    /// Just print the URL without opening browser
    #[arg(short, long)]
    print_only: bool,

    /// Add title to the pull request
    #[arg(short = 'T', long)]
    title: Option<String>,

    /// Add description to the pull request
    #[arg(short = 'd', long)]
    description: Option<String>,

    /// Mark the pull request as draft/WIP
    #[arg(long)]
    draft: bool,

    /// Only output the link (mostly for testing purposes)
    #[arg(long, default_value_t = false)]
    link: bool,
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum, Debug)]
enum Service {
    Github,
    Gitlab,
    Bitbucket,
    Azure,
}

enum GitService {
    GitHub,
    GitLab,
    Bitbucket,
    AzureDevOps,
    Unknown,
}

fn main() {
    let args = Args::parse();

    // Open the git repository from the current directory
    let repo = match Repository::open(".") {
        Ok(repo) => repo,
        Err(e) => {
            eprintln!("Error opening git repository: {}", e);
            exit(1);
        }
    };

    // Get the current branch name or use the one provided in arguments
    let branch_name = match &args.branch {
        Some(branch) => branch.clone(),
        None => {
            let head = match repo.head() {
                Ok(head) => head,
                Err(e) => {
                    eprintln!("Error getting HEAD: {}", e);
                    exit(1);
                }
            };

            match head.shorthand() {
                Some(name) => name.to_string(),
                None => {
                    eprintln!("Could not determine current branch name");
                    exit(1);
                }
            }
        }
    };

    // Determine which remote to use
    let remote_name = if let Some(remote) = &args.remote {
        remote.clone()
    } else if !args.force_remote {
        if get_remote_url(&repo, "upstream").is_some() {
            "upstream".to_string()
        } else {
            "origin".to_string()
        }
    } else {
        "origin".to_string()
    };

    // Get the URL for the selected remote
    let remote_url = match get_remote_url(&repo, &remote_name) {
        Some(url) => url,
        None => {
            eprintln!("Remote '{}' not found", remote_name);
            exit(1);
        }
    };

    // Parse the remote URL to get the owner and repository
    let (owner, repo_name) = parse_git_url(&remote_url);

    // Determine the service type (from args or by URL analysis)
    let service = match args.service {
        Some(Service::Github) => GitService::GitHub,
        Some(Service::Gitlab) => GitService::GitLab,
        Some(Service::Bitbucket) => GitService::Bitbucket,
        Some(Service::Azure) => GitService::AzureDevOps,
        None => determine_service(&remote_url),
    };

    // Determine default target branch if not specified
    let target_branch = match args.target {
        Some(target) => target,
        None => {
            // Try to determine default branch from the repository
            match get_default_branch(&repo, &remote_name) {
                Some(branch) => branch,
                None => "main".to_string(), // Fallback to "main" if we can't determine
            }
        }
    };

    // Build the PR URL based on the service and options
    let pr_url = build_pr_url(
        service,
        &owner,
        &repo_name,
        &branch_name,
        &target_branch,
        args.title.as_deref(),
        args.description.as_deref(),
        args.draft,
    );

    if args.print_only {
        println!("{}", pr_url);
    } else {
        println!("Opening PR URL: {}", pr_url);
        if let Err(e) = open_url(&pr_url) {
            eprintln!("Failed to open browser: {}", e);
            exit(1);
        }
    }
}

// Platform-specific function to open URLs
#[cfg(target_os = "windows")]
fn open_url(url: &str) -> Result<(), String> {
    Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn open_url(url: &str) -> Result<(), String> {
    Command::new("open")
        .arg(url)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn open_url(url: &str) -> Result<(), String> {
    // Try several common Linux browser openers
    for cmd in &["xdg-open", "gnome-open", "kde-open", "wslview"] {
        match Command::new(cmd).arg(url).spawn() {
            Ok(_) => return Ok(()),
            Err(_) => continue,
        }
    }

    // If all else fails, try to detect if running in WSL and use PowerShell
    if std::path::Path::new("/proc/sys/kernel/osrelease").exists() {
        let osrelease =
            std::fs::read_to_string("/proc/sys/kernel/osrelease").map_err(|e| e.to_string())?;

        if osrelease.to_lowercase().contains("microsoft")
            || osrelease.to_lowercase().contains("wsl")
        {
            Command::new("powershell.exe")
                .args(["-Command", &format!("Start-Process '{}'", url)])
                .spawn()
                .map_err(|e| e.to_string())?;
            return Ok(());
        }
    }

    Err("Could not find a suitable program to open the URL".to_string())
}

// Fallback for other Unix systems
#[cfg(all(unix, not(target_os = "macos"), not(target_os = "linux")))]
fn open_url(url: &str) -> Result<(), String> {
    // Try a few options that might work on various Unix systems
    for cmd in &[
        "xdg-open",
        "open",
        "x-www-browser",
        "firefox",
        "chromium-browser",
        "google-chrome",
    ] {
        match Command::new(cmd).arg(url).spawn() {
            Ok(_) => return Ok(()),
            Err(_) => continue,
        }
    }

    Err("Could not find a suitable program to open the URL".to_string())
}

fn determine_service(url: &str) -> GitService {
    if url.contains("github.com") {
        GitService::GitHub
    } else if url.contains("gitlab.com") {
        GitService::GitLab
    } else if url.contains("bitbucket.org") {
        GitService::Bitbucket
    } else if url.contains("dev.azure.com") || url.contains("visualstudio.com") {
        GitService::AzureDevOps
    } else {
        GitService::Unknown
    }
}

fn get_remote_url(repo: &Repository, remote_name: &str) -> Option<String> {
    match repo.find_remote(remote_name) {
        Ok(remote) => remote.url().map(|s| s.to_string()),
        Err(_) => None,
    }
}

fn parse_git_url(url: &str) -> (String, String) {
    // Handle SSH URLs like git@github.com:user/repo.git
    if url.starts_with("git@") {
        let ssh_regex = Regex::new(r"git@(?:.*?)[:/](.*?)/(.*?)(?:\.git)?$").unwrap();
        if let Some(caps) = ssh_regex.captures(url) {
            return (
                caps[1].to_string(),
                caps[2].to_string().trim_end_matches(".git").to_string(),
            );
        }
    }

    // Handle HTTPS URLs like https://github.com/user/repo.git
    let https_regex = Regex::new(r"https://(?:.*?)/([^/]+)/([^/]+?)(?:\.git)?$").unwrap();
    if let Some(caps) = https_regex.captures(url) {
        return (
            caps[1].to_string(),
            caps[2].to_string().trim_end_matches(".git").to_string(),
        );
    }

    eprintln!("Could not parse git URL: {}", url);
    exit(1);
}

fn parse_azure_url(url: &str) -> (String, String) {
    // Azure DevOps URLs can be complex
    let azure_regex = Regex::new(r"https://dev\.azure\.com/([^/]+)/([^/]+)").unwrap();
    if let Some(caps) = azure_regex.captures(url) {
        return (caps[1].to_string(), caps[2].to_string());
    }

    // Legacy visualstudio.com URLs
    let vs_regex = Regex::new(r"https://([^.]+)\.visualstudio\.com/([^/]+)").unwrap();
    if let Some(caps) = vs_regex.captures(url) {
        return (caps[1].to_string(), caps[2].to_string());
    }

    eprintln!("Could not parse Azure DevOps URL: {}", url);
    exit(1);
}

fn get_default_branch(repo: &Repository, remote_name: &str) -> Option<String> {
    // Alternatively, check for common default branch names
    for branch_name in ["main", "master", "develop", "trunk"] {
        if repo
            .find_branch(
                &format!("{}/{}", remote_name, branch_name),
                BranchType::Remote,
            )
            .is_ok()
        {
            return Some(branch_name.to_string());
        }
    }

    None
}

#[allow(clippy::too_many_arguments)]
fn build_pr_url(
    service: GitService,
    owner: &str,
    repo_name: &str,
    branch_name: &str,
    target_branch: &str,
    title: Option<&str>,
    description: Option<&str>,
    draft: bool,
) -> String {
    match service {
        GitService::GitHub => {
            let mut url: String = format!(
                "https://github.com/{}/{}/compare/{}...{}?expand=1",
                owner, repo_name, target_branch, branch_name
            );

            // Add optional parameters
            if let Some(title_str) = title {
                url.push_str(&format!(
                    "&title={}",
                    form_urlencoded::byte_serialize(title_str.as_bytes()).collect::<String>()
                ));
            }

            if let Some(desc_str) = description {
                url.push_str(&format!(
                    "&body={}",
                    form_urlencoded::byte_serialize(desc_str.as_bytes()).collect::<String>()
                ));
            }

            if draft {
                url.push_str("&draft=1");
            }

            url
        }
        GitService::GitLab => {
            let mut url = format!(
                "https://gitlab.com/{}/{}/-/merge_requests/new?merge_request%5Bsource_branch%5D={}&merge_request%5Btarget_branch%5D={}",
                owner, repo_name, branch_name, target_branch
            );

            if let Some(title_str) = title {
                url.push_str(&format!(
                    "&merge_request%5Btitle%5D={}",
                    form_urlencoded::byte_serialize(title_str.as_bytes()).collect::<String>()
                ));
            }

            if let Some(desc_str) = description {
                url.push_str(&format!(
                    "&merge_request%5Bdescription%5D={}",
                    form_urlencoded::byte_serialize(desc_str.as_bytes()).collect::<String>()
                ));
            }

            if draft {
                url.push_str("&merge_request%5Bdraft%5D=true");
            }

            url
        }
        GitService::Bitbucket => {
            let mut url = format!(
                "https://bitbucket.org/{}/{}/pull-requests/new?source={}&dest={}",
                owner, repo_name, branch_name, target_branch
            );

            if let Some(title_str) = title {
                url.push_str(&format!(
                    "&title={}",
                    form_urlencoded::byte_serialize(title_str.as_bytes()).collect::<String>()
                ));
            }

            if let Some(desc_str) = description {
                url.push_str(&format!(
                    "&description={}",
                    form_urlencoded::byte_serialize(desc_str.as_bytes()).collect::<String>()
                ));
            }

            url
        }
        GitService::AzureDevOps => {
            let (org, project) =
                parse_azure_url(&format!("https://dev.azure.com/{}/{}", owner, repo_name));

            let mut url = format!(
                "https://dev.azure.com/{}/{}/_git/{}/pullrequestcreate?sourceRef={}&targetRef={}",
                org, project, repo_name, branch_name, target_branch
            );

            if let Some(title_str) = title {
                url.push_str(&format!(
                    "&title={}",
                    form_urlencoded::byte_serialize(title_str.as_bytes()).collect::<String>()
                ));
            }

            if let Some(desc_str) = description {
                url.push_str(&format!(
                    "&description={}",
                    form_urlencoded::byte_serialize(desc_str.as_bytes()).collect::<String>()
                ));
            }

            if draft {
                url.push_str("&isDraft=true");
            }

            url
        }
        GitService::Unknown => {
            eprintln!("Unknown git service for {}/{}", owner, repo_name);
            exit(1);
        }
    }
}
