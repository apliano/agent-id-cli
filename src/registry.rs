use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    activity::{ActivityState, ActivityStateValue},
    cli::{AnnotateArgs, CurrentArgs, DiscoverArgs, LookupArgs, PruneArgs, RegisterArgs},
    names,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActivitySummary {
    pub text: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExtensionMetadata {
    pub data: serde_json::Value,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct ActivityUpdate {
    pub summary: Option<String>,
    pub clear_summary: bool,
    pub state: Option<ActivityStateValue>,
    pub clear_state: bool,
    pub cwd: Option<String>,
    pub clear_cwd: bool,
    pub extensions: BTreeMap<String, serde_json::Value>,
    pub clear_extensions: BTreeSet<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Assignment {
    pub version: u8,
    pub session_id: String,
    pub name: String,
    pub slug: String,
    pub first_name: String,
    pub family_name: String,
    pub realm: String,
    pub summary: Option<ActivitySummary>,
    pub state: ActivityState,
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: BTreeMap<String, ExtensionMetadata>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrunedIdentity {
    pub session_id: String,
    pub name: String,
    pub slug: String,
    pub updated_at: DateTime<Utc>,
    pub claim_removed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PruneReport {
    pub cutoff: DateTime<Utc>,
    pub dry_run: bool,
    pub candidates: Vec<Assignment>,
    pub removed: Vec<PrunedIdentity>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Registry {
    root: PathBuf,
}

impl Registry {
    pub fn from_env() -> Result<Self> {
        let root = if let Some(path) = env::var_os("AGENT_ID_HOME") {
            PathBuf::from(path)
        } else if let Some(path) = env::var_os("XDG_DATA_HOME") {
            PathBuf::from(path).join("agent-id")
        } else {
            home_dir()?.join(".local/share/agent-id")
        };

        Ok(Self::new(root))
    }

    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn register(
        &self,
        session_id: &str,
        family_name: Option<&str>,
        realm: &str,
    ) -> Result<Assignment> {
        let session_id = validate_session_id(session_id)?;
        let session_path = self.session_path(&session_id);
        if session_path.exists() {
            let mut existing = read_assignment(&session_path)
                .with_context(|| format!("read existing assignment {}", session_path.display()))?;
            existing.updated_at = Utc::now();
            replace_assignment(&session_path, &existing)?;
            return Ok(existing);
        }

        let realm = normalize_component(realm, "realm")?;
        let requested_family = family_name
            .map(|value| normalize_component(value, "family name"))
            .transpose()?;
        let first_names = names::first_names();
        let family_names = names::family_names();
        if first_names.is_empty() || family_names.is_empty() {
            bail!("name lists are empty");
        }

        fs::create_dir_all(self.root.join("by-session"))
            .with_context(|| format!("create {}", self.root.display()))?;
        fs::create_dir_all(self.root.join("by-name"))
            .with_context(|| format!("create {}", self.root.display()))?;

        for attempt in 0..100_000_u64 {
            let (first, family) = candidate(
                &session_id,
                attempt,
                &first_names,
                &family_names,
                requested_family.as_deref(),
            );
            if first == family {
                continue;
            }

            let first_name = title_word(first);
            let family_name = title_word(&family);

            let name = format!("{first_name} {family_name} of {realm}");
            let slug = slug(&first_name, &family_name, &realm);
            let claim_path = self.root.join("by-name").join(&slug);

            match claim_name(&claim_path, &session_id)? {
                Claim::Owned => {}
                Claim::Claimed => {}
                Claim::Other => continue,
            }

            let now = Utc::now();
            let assignment = Assignment {
                version: 1,
                session_id: session_id.clone(),
                name,
                slug,
                first_name,
                family_name,
                realm: realm.clone(),
                state: ActivityState::unknown(now),
                cwd: None,
                summary: None,
                extensions: BTreeMap::new(),
                created_at: now,
                updated_at: now,
            };
            write_assignment(&session_path, &assignment)?;
            return Ok(assignment);
        }

        bail!("exhausted available names for realm {realm}")
    }

    pub fn lookup(&self, input: &str) -> Result<Assignment> {
        let input = require_nonempty(input, "lookup identifier")?;
        if let Ok(session_id) = validate_session_id(&input) {
            let session_path = self.session_path(&session_id);
            if session_path.exists() {
                return self.lookup_session(&session_id);
            }
        }

        for slug in lookup_slugs(&input) {
            let claim_path = self.root.join("by-name").join(&slug);
            if !claim_path.is_file() {
                continue;
            }

            let session_id = require_nonempty(
                &fs::read_to_string(&claim_path)
                    .with_context(|| format!("read name claim {}", claim_path.display()))?,
                "claimed session ID",
            )?;
            return self
                .lookup_session(&session_id)
                .with_context(|| format!("resolve name claim {slug}"));
        }

        bail!(
            "no identity found for '{input}'; lookup accepts a session ID, canonical name, or slug"
        )
    }

    fn lookup_session(&self, session_id: &str) -> Result<Assignment> {
        let session_id = validate_session_id(session_id)?;
        let path = self.session_path(&session_id);
        if !path.exists() {
            bail!(
                "no identity registered for session {session_id}; run `agent-id register {session_id}`"
            );
        }

        let assignment = read_assignment(&path)
            .with_context(|| format!("read assignment for session {session_id}"))?;
        if assignment.session_id != session_id {
            bail!("identity registry entry does not belong to session {session_id}");
        }
        Ok(assignment)
    }

    pub fn annotate(&self, session_id: &str, update: ActivityUpdate) -> Result<Assignment> {
        let session_id = validate_session_id(session_id)?;
        if update.summary.is_none()
            && !update.clear_summary
            && update.state.is_none()
            && !update.clear_state
            && update.cwd.is_none()
            && !update.clear_cwd
            && update.extensions.is_empty()
            && update.clear_extensions.is_empty()
        {
            bail!("pass at least one activity update");
        }
        if update.summary.is_some() && update.clear_summary {
            bail!("summary and clear_summary are mutually exclusive");
        }
        if update.state.is_some() && update.clear_state {
            bail!("state and clear_state are mutually exclusive");
        }
        if update.cwd.is_some() && update.clear_cwd {
            bail!("cwd and clear_cwd are mutually exclusive");
        }
        if let Some(owner) = update
            .extensions
            .keys()
            .find(|owner| update.clear_extensions.contains(*owner))
        {
            bail!("extension {owner} cannot be set and cleared together");
        }

        let mut assignment = self.lookup_session(&session_id)?;
        let summary = update
            .summary
            .as_deref()
            .map(normalize_summary)
            .transpose()?;
        let cwd = update.cwd.as_deref().map(normalize_cwd).transpose()?;
        let now = Utc::now();
        if update.summary.is_some() {
            assignment.summary = summary.map(|text| ActivitySummary {
                text,
                updated_at: now,
            });
        } else if update.clear_summary {
            assignment.summary = None;
        }
        if update.cwd.is_some() {
            assignment.cwd = cwd;
        } else if update.clear_cwd {
            assignment.cwd = None;
        }
        for (owner, data) in update.extensions {
            assignment.extensions.insert(
                owner,
                ExtensionMetadata {
                    data,
                    updated_at: now,
                },
            );
        }
        for owner in update.clear_extensions {
            assignment.extensions.remove(&owner);
        }
        if let Some(value) = update.state {
            set_omp_activity_state(&mut assignment.extensions, value, now);
        } else if update.clear_state {
            clear_omp_activity_state(&mut assignment.extensions, now);
        }
        normalize_omp_activity_state(&mut assignment.extensions, now);
        assignment.updated_at = now;
        assignment.state = materialize_omp_activity_state(&assignment.extensions, now);
        replace_assignment(&self.session_path(&session_id), &assignment)?;
        Ok(assignment)
    }

    pub fn discover(
        &self,
        recent_hours: Option<i64>,
        realm: Option<&str>,
    ) -> Result<Vec<Assignment>> {
        if recent_hours.is_some_and(|hours| hours < 0) {
            bail!("--recent must be non-negative");
        }
        let realm = realm
            .map(|value| normalize_component(value, "realm"))
            .transpose()?;
        let cutoff = recent_hours.map(|hours| Utc::now() - Duration::hours(hours));
        let path = self.root.join("by-session");
        let entries = match fs::read_dir(&path) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        };
        let mut assignments = Vec::new();
        for entry in entries {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let assignment = read_assignment(&path)
                .with_context(|| format!("read assignment {}", path.display()))?;
            if realm
                .as_deref()
                .is_some_and(|value| value != assignment.realm)
            {
                continue;
            }
            if cutoff.is_some_and(|value| assignment.updated_at < value) {
                continue;
            }
            assignments.push(assignment);
        }
        assignments.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        Ok(assignments)
    }

    pub fn prune(&self, cutoff: DateTime<Utc>, dry_run: bool) -> Result<PruneReport> {
        let path = self.root.join("by-session");
        let entries = match fs::read_dir(&path) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(PruneReport {
                    cutoff,
                    dry_run,
                    candidates: Vec::new(),
                    removed: Vec::new(),
                    errors: Vec::new(),
                })
            }
            Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
        };
        let mut candidates = Vec::new();
        for entry in entries {
            let path = entry?.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let assignment = read_assignment(&path)
                .with_context(|| format!("read assignment {}", path.display()))?;
            if assignment.updated_at < cutoff {
                candidates.push(assignment);
            }
        }
        candidates.sort_by(|left, right| left.updated_at.cmp(&right.updated_at));
        let mut report = PruneReport {
            cutoff,
            dry_run,
            candidates,
            removed: Vec::new(),
            errors: Vec::new(),
        };
        if dry_run {
            return Ok(report);
        }

        for assignment in &report.candidates {
            let session_path = self.session_path(&assignment.session_id);
            if let Err(error) = fs::remove_file(&session_path) {
                report
                    .errors
                    .push(format!("remove {}: {error}", session_path.display()));
                continue;
            }

            let claim_path = self.root.join("by-name").join(&assignment.slug);
            let claim_removed = match fs::read_to_string(&claim_path) {
                Ok(owner) if owner.trim() == assignment.session_id => {
                    if let Err(error) = fs::remove_file(&claim_path) {
                        report.errors.push(format!(
                            "remove name claim {}: {error}",
                            claim_path.display()
                        ));
                        false
                    } else {
                        true
                    }
                }
                Ok(owner) => {
                    report.errors.push(format!(
                        "name claim {} belongs to {}, not {}",
                        claim_path.display(),
                        owner.trim(),
                        assignment.session_id
                    ));
                    false
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                Err(error) => {
                    report
                        .errors
                        .push(format!("read name claim {}: {error}", claim_path.display()));
                    false
                }
            };
            report.removed.push(PrunedIdentity {
                session_id: assignment.session_id.clone(),
                name: assignment.name.clone(),
                slug: assignment.slug.clone(),
                updated_at: assignment.updated_at,
                claim_removed,
            });
        }
        Ok(report)
    }

    fn session_path(&self, session_id: &str) -> PathBuf {
        self.root
            .join("by-session")
            .join(format!("{session_id}.json"))
    }
}

pub fn execute_register(args: &RegisterArgs) -> Result<()> {
    let session_id = resolve_session(args.explicit_session())?;
    let realm = resolve_realm(args.realm.as_deref())?;
    let assignment = Registry::from_env()?.register(&session_id, args.family.as_deref(), &realm)?;
    print_assignment(&assignment, args.json)
}

pub fn execute_lookup(args: &LookupArgs) -> Result<()> {
    let input = resolve_session(args.explicit_input())?;
    let assignment = Registry::from_env()?.lookup(&input)?;
    print_assignment(&assignment, args.json)
}

pub fn execute_current(args: &CurrentArgs) -> Result<()> {
    let session_id = resolve_session(None)?;
    let assignment = Registry::from_env()?.lookup(&session_id)?;
    print_assignment(&assignment, args.json)
}

pub fn execute_annotate(args: &AnnotateArgs) -> Result<()> {
    let session_id = resolve_session(args.explicit_session())?;
    let update = ActivityUpdate {
        summary: args.summary.clone(),
        clear_summary: args.clear_summary,
        state: args.state,
        clear_state: args.clear_state,
        cwd: args.cwd.clone(),
        clear_cwd: args.clear_cwd,
        extensions: parse_extension_updates(&args.extensions)?,
        clear_extensions: parse_extension_owners(&args.clear_extensions)?,
    };
    let assignment = Registry::from_env()?.annotate(&session_id, update)?;
    print_assignment(&assignment, args.json)
}

pub fn execute_discover(args: &DiscoverArgs) -> Result<()> {
    let assignments = Registry::from_env()?.discover(args.recent, args.realm.as_deref())?;
    let mut records = crate::herdr::augment_discovery(assignments, args.all);
    if !args.all {
        records.retain(|record| record.assignment.state.value != ActivityStateValue::Stopped);
    }
    if args.limit > 0 {
        records.truncate(args.limit);
    }
    if args.json {
        println!("{}", serde_json::to_string_pretty(&records)?);
    } else if records.is_empty() {
        println!("(no identities)");
    } else {
        for record in records {
            let assignment = &record.assignment;
            let mut annotations = Vec::new();
            annotations.push(format!("state:{}", assignment.state.value));
            if let Some(summary) = assignment.summary.as_ref() {
                annotations.push(format!("summary:{}", summary.text));
            }
            if let Some(cwd) = assignment.cwd.as_ref() {
                annotations.push(format!("cwd:{cwd}"));
            }
            if let Some(runtime) = record.runtime {
                for location in runtime.locations {
                    let workspace = location
                        .workspace_label
                        .as_deref()
                        .unwrap_or(&location.workspace_id);
                    annotations.push(format!(
                        "herdr:{} pane:{} workspace:{}",
                        location.agent_status, location.pane_id, workspace
                    ));
                }
            }
            println!(
                "{}\t{}\t{}",
                assignment.name,
                assignment.session_id,
                annotations.join("\t")
            );
        }
    }
    Ok(())
}

pub fn execute_prune(args: &PruneArgs) -> Result<()> {
    let cutoff = DateTime::parse_from_rfc3339(&args.before)
        .with_context(|| format!("parse --before timestamp {}", args.before))?
        .with_timezone(&Utc);
    let report = Registry::from_env()?.prune(cutoff, args.dry_run)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        let action = if args.dry_run {
            "would prune"
        } else {
            "pruned"
        };
        println!(
            "{action} {} identities before {}",
            report.candidates.len(),
            cutoff
        );
        for assignment in &report.candidates {
            println!(
                "{}\t{}\tupdated:{}",
                assignment.name, assignment.session_id, assignment.updated_at
            );
        }
        for error in &report.errors {
            eprintln!("agent-id: {error}");
        }
    }
    if report.errors.is_empty() {
        Ok(())
    } else {
        bail!("prune completed with {} errors", report.errors.len())
    }
}

pub fn resolve_session(explicit: Option<&str>) -> Result<String> {
    if let Some(session_id) = explicit.filter(|value| !value.trim().is_empty()) {
        return Ok(session_id.trim().to_string());
    }

    if let Some(value) = env::var_os("AGENT_ID_SESSION_ID") {
        let value = value.to_string_lossy();
        if !value.trim().is_empty() {
            return Ok(value.trim().to_string());
        }
    }

    bail!("no session ID found; pass SESSION_ID or --session-id, or set AGENT_ID_SESSION_ID")
}

fn resolve_realm(explicit: Option<&str>) -> Result<String> {
    if let Some(realm) = explicit.filter(|value| !value.trim().is_empty()) {
        return normalize_component(realm, "realm");
    }
    if let Some(realm) = env::var_os("AGENT_REALM") {
        let realm = realm.to_string_lossy();
        if !realm.trim().is_empty() {
            return normalize_component(&realm, "realm");
        }
    }

    let home = home_dir()?;
    let config_home = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    let realm_path = config_home.join("agent-id/realm");

    if realm_path.is_file() {
        let value = fs::read_to_string(&realm_path)
            .with_context(|| format!("read realm from {}", realm_path.display()))?;
        return normalize_component(&value, "realm");
    }

    auto_create_realm(&realm_path)
}

fn auto_create_realm(path: &Path) -> Result<String> {
    let candidates = names::candidate_realms();
    if candidates.is_empty() {
        bail!("bundled realm candidate list is empty");
    }

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let pid = std::process::id();
    let hostname = env::var("HOSTNAME").unwrap_or_default();
    let digest = Sha256::digest(format!("{nanos}:{pid}:{hostname}").as_bytes());
    let index = usize::try_from(u64::from_be_bytes(digest[0..8].try_into().unwrap())).unwrap_or(0)
        % candidates.len();
    let realm = normalize_component(candidates[index], "realm")?;

    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("realm path has no parent directory"))?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;

    let temp = temporary_path(path);
    let mut file = File::create(&temp)
        .with_context(|| format!("create temporary realm {}", temp.display()))?;
    file.write_all(format!("{realm}\n").as_bytes())?;
    file.sync_all()?;

    match fs::hard_link(&temp, path) {
        Ok(()) => {
            let _ = fs::remove_file(temp);
            Ok(realm)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let _ = fs::remove_file(temp);
            let value = fs::read_to_string(path)
                .with_context(|| format!("read existing realm {}", path.display()))?;
            normalize_component(&value, "realm")
        }
        Err(error) => {
            let _ = fs::remove_file(temp);
            Err(error).with_context(|| format!("create realm file {}", path.display()))
        }
    }
}

fn print_assignment(assignment: &Assignment, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(assignment)?);
    } else {
        println!("{}", assignment.name);
    }
    Ok(())
}

fn lookup_slugs(input: &str) -> Vec<String> {
    let mut slugs = Vec::new();
    let input = input.trim();
    if is_slug(input) {
        slugs.push(input.to_ascii_lowercase());
    }
    if let Some(slug) = canonical_name_slug(input) {
        if !slugs.contains(&slug) {
            slugs.push(slug);
        }
    }
    slugs
}

fn canonical_name_slug(input: &str) -> Option<String> {
    let mut parts = input.split_whitespace();
    let first = normalize_component(parts.next()?, "first name").ok()?;
    let family = normalize_component(parts.next()?, "family name").ok()?;
    if !parts.next()?.eq_ignore_ascii_case("of") {
        return None;
    }
    let realm = normalize_component(parts.next()?, "realm").ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(slug(&first, &family, &realm))
}

fn is_slug(input: &str) -> bool {
    !input.is_empty()
        && !input.starts_with('-')
        && !input.ends_with('-')
        && input
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
}

fn candidate<'a>(
    session_id: &str,
    attempt: u64,
    first_names: &'a [&'a str],
    family_names: &'a [&'a str],
    requested_family: Option<&str>,
) -> (&'a str, String) {
    let digest = Sha256::digest(format!("{session_id}:{attempt}").as_bytes());
    let first_index = usize::try_from(u64::from_be_bytes(digest[0..8].try_into().unwrap()))
        .unwrap_or(0)
        % first_names.len();
    let family_index = usize::try_from(u64::from_be_bytes(digest[8..16].try_into().unwrap()))
        .unwrap_or(0)
        % family_names.len();
    let family = requested_family
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| family_names[family_index].to_string());
    (first_names[first_index], family)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Claim {
    Claimed,
    Owned,
    Other,
}

fn claim_name(path: &Path, session_id: &str) -> Result<Claim> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("name claim has no parent directory"))?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;

    let temp = temporary_path(path);
    let mut file = File::create(&temp)
        .with_context(|| format!("create temporary claim {}", temp.display()))?;
    file.write_all(session_id.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;

    let claim = match fs::hard_link(&temp, path) {
        Ok(()) => Claim::Claimed,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let owner = fs::read_to_string(path).unwrap_or_default();
            if owner.trim() == session_id {
                Claim::Owned
            } else {
                Claim::Other
            }
        }
        Err(error) => return Err(error).with_context(|| format!("claim name {}", path.display())),
    };
    let _ = fs::remove_file(temp);
    Ok(claim)
}

#[derive(Debug, Serialize)]
struct PersistedAssignment<'a> {
    version: u8,
    session_id: &'a str,
    name: &'a str,
    slug: &'a str,
    first_name: &'a str,
    family_name: &'a str,
    realm: &'a str,
    summary: &'a Option<ActivitySummary>,
    cwd: &'a Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    extensions: &'a BTreeMap<String, ExtensionMetadata>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

fn persisted_assignment(assignment: &Assignment) -> PersistedAssignment<'_> {
    PersistedAssignment {
        version: assignment.version,
        session_id: &assignment.session_id,
        name: &assignment.name,
        slug: &assignment.slug,
        first_name: &assignment.first_name,
        family_name: &assignment.family_name,
        realm: &assignment.realm,
        summary: &assignment.summary,
        cwd: &assignment.cwd,
        extensions: &assignment.extensions,
        created_at: assignment.created_at,
        updated_at: assignment.updated_at,
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredAssignment {
    version: u8,
    session_id: String,
    name: String,
    slug: String,
    first_name: String,
    family_name: String,
    realm: String,
    #[serde(default)]
    summary: Option<ActivitySummary>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    extensions: BTreeMap<String, ExtensionMetadata>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl StoredAssignment {
    fn into_assignment(self) -> Assignment {
        let StoredAssignment {
            version,
            session_id,
            name,
            slug,
            first_name,
            family_name,
            realm,
            summary,
            cwd,
            extensions,
            created_at,
            updated_at,
        } = self;
        let state = materialize_omp_activity_state(&extensions, updated_at);
        Assignment {
            version,
            session_id,
            name,
            slug,
            first_name,
            family_name,
            realm,
            summary,
            state,
            cwd,
            extensions,
            created_at,
            updated_at,
        }
    }
}

fn write_assignment(path: &Path, assignment: &Assignment) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("assignment has no parent directory"))?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let contents = format!(
        "{}\n",
        serde_json::to_string_pretty(&persisted_assignment(assignment))?
    );
    let temp = temporary_path(path);
    let mut file = File::create(&temp)
        .with_context(|| format!("create temporary assignment {}", temp.display()))?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;

    let result = match fs::hard_link(&temp, path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            bail!(
                "session {} already has a registered identity",
                assignment.session_id
            )
        }
        Err(error) => Err(error).with_context(|| format!("write assignment {}", path.display())),
    };
    let _ = fs::remove_file(temp);
    result
}

fn replace_assignment(path: &Path, assignment: &Assignment) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("assignment has no parent directory"))?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let contents = format!(
        "{}\n",
        serde_json::to_string_pretty(&persisted_assignment(assignment))?
    );
    let temp = temporary_path(path);
    let mut file = File::create(&temp)
        .with_context(|| format!("create temporary assignment {}", temp.display()))?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;

    let result =
        fs::rename(&temp, path).with_context(|| format!("replace assignment {}", path.display()));
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

fn read_assignment(path: &Path) -> Result<Assignment> {
    let contents = fs::read_to_string(path)?;
    Ok(serde_json::from_str::<StoredAssignment>(&contents)?.into_assignment())
}
const OMP_EXTENSION_OWNER: &str = "omp";
const OMP_STATE_KEY: &str = "state";

fn omp_activity_state(extensions: &BTreeMap<String, ExtensionMetadata>) -> Option<ActivityState> {
    let metadata = extensions.get(OMP_EXTENSION_OWNER)?;
    let state = metadata.data.get(OMP_STATE_KEY)?;
    if let Some(value) = state.as_str() {
        return Some(ActivityState::from_external(value, metadata.updated_at));
    }
    serde_json::from_value(state.clone()).ok()
}

fn materialize_omp_activity_state(
    extensions: &BTreeMap<String, ExtensionMetadata>,
    fallback_updated_at: DateTime<Utc>,
) -> ActivityState {
    omp_activity_state(extensions).unwrap_or_else(|| ActivityState::unknown(fallback_updated_at))
}

fn set_omp_activity_state(
    extensions: &mut BTreeMap<String, ExtensionMetadata>,
    value: ActivityStateValue,
    updated_at: DateTime<Utc>,
) {
    let metadata = extensions
        .entry(OMP_EXTENSION_OWNER.to_string())
        .or_insert_with(|| ExtensionMetadata {
            data: serde_json::json!({}),
            updated_at,
        });
    if !metadata.data.is_object() {
        metadata.data = serde_json::json!({});
    }
    metadata
        .data
        .as_object_mut()
        .expect("state extension data was just normalized to an object")
        .insert(
            OMP_STATE_KEY.to_string(),
            serde_json::json!({
                "value": value,
                "updated_at": updated_at,
            }),
        );
    metadata.updated_at = updated_at;
}

fn clear_omp_activity_state(
    extensions: &mut BTreeMap<String, ExtensionMetadata>,
    updated_at: DateTime<Utc>,
) {
    let Some(metadata) = extensions.get_mut(OMP_EXTENSION_OWNER) else {
        return;
    };
    if let Some(data) = metadata.data.as_object_mut() {
        data.remove(OMP_STATE_KEY);
        metadata.updated_at = updated_at;
    }
}

fn normalize_omp_activity_state(
    extensions: &mut BTreeMap<String, ExtensionMetadata>,
    updated_at: DateTime<Utc>,
) {
    let value = extensions
        .get(OMP_EXTENSION_OWNER)
        .and_then(|metadata| metadata.data.get(OMP_STATE_KEY))
        .and_then(|state| state.as_str())
        .map(ActivityStateValue::from_external);
    if let Some(value) = value {
        set_omp_activity_state(extensions, value, updated_at);
    }
}

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn temporary_path(path: &Path) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    path.with_extension(format!("tmp-{}-{nanos}-{counter}", std::process::id()))
}

fn slug(first_name: &str, family_name: &str, realm: &str) -> String {
    [first_name, family_name, realm]
        .iter()
        .map(|part| part.to_ascii_lowercase().replace([' ', '\'', '_'], "-"))
        .collect::<Vec<_>>()
        .join("-")
}

fn normalize_component(value: &str, kind: &str) -> Result<String> {
    let value = value.trim();
    let valid = !value.is_empty()
        && value.chars().all(|character| {
            character.is_ascii_alphabetic() || character == '-' || character == '\''
        })
        && value
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_alphabetic())
        && value
            .chars()
            .last()
            .is_some_and(|character| character.is_ascii_alphabetic());
    if !valid {
        bail!("{kind} must contain only letters, apostrophes, or hyphens")
    }
    Ok(title_word(value))
}

fn title_word(value: &str) -> String {
    let mut characters = value.chars();
    let Some(first) = characters.next() else {
        return String::new();
    };
    first.to_uppercase().collect::<String>() + &characters.as_str().to_ascii_lowercase()
}

fn validate_session_id(value: &str) -> Result<String> {
    let value = require_nonempty(value, "session ID")?;
    let safe = value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'));
    if !safe || value == "." || value == ".." {
        bail!("session ID must be a filename-safe value using letters, digits, '.', '_' or '-'");
    }
    Ok(value)
}

const MAX_EXTENSION_OWNER_CHARS: usize = 64;
const MAX_EXTENSION_JSON_BYTES: usize = 16 * 1024;

fn parse_extension_updates(values: &[String]) -> Result<BTreeMap<String, serde_json::Value>> {
    let mut extensions = BTreeMap::new();
    for value in values {
        let (owner, json) = value
            .split_once('=')
            .ok_or_else(|| anyhow!("--extension must use OWNER=JSON"))?;
        let owner = normalize_extension_owner(owner)?;
        if json.len() > MAX_EXTENSION_JSON_BYTES {
            bail!("extension {owner} JSON must be at most {MAX_EXTENSION_JSON_BYTES} bytes");
        }
        let data = serde_json::from_str(json)
            .with_context(|| format!("parse JSON for extension {owner}"))?;
        if extensions.insert(owner.clone(), data).is_some() {
            bail!("extension {owner} was provided more than once");
        }
    }
    Ok(extensions)
}

fn parse_extension_owners(values: &[String]) -> Result<BTreeSet<String>> {
    values
        .iter()
        .map(|owner| normalize_extension_owner(owner))
        .collect()
}

fn normalize_extension_owner(value: &str) -> Result<String> {
    let owner = require_nonempty(value, "extension owner")?;
    let valid = owner.chars().count() <= MAX_EXTENSION_OWNER_CHARS
        && owner
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
        && owner.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || matches!(character, '.' | '_' | '-')
        });
    if !valid {
        bail!(
            "extension owner must be at most {MAX_EXTENSION_OWNER_CHARS} characters using lowercase letters, digits, '.', '_' or '-'"
        );
    }
    Ok(owner)
}

const MAX_SUMMARY_CHARS: usize = 240;

fn normalize_summary(value: &str) -> Result<String> {
    let mut normalized = String::new();
    for word in value.split_whitespace() {
        if !normalized.is_empty() {
            normalized.push(' ');
        }
        normalized.push_str(word);
    }
    if normalized.is_empty() {
        bail!("summary cannot be empty");
    }
    if normalized.chars().count() > MAX_SUMMARY_CHARS {
        bail!("summary must be at most {MAX_SUMMARY_CHARS} characters");
    }
    Ok(normalized)
}

const MAX_CWD_CHARS: usize = 4096;

fn normalize_cwd(value: &str) -> Result<String> {
    let value = require_nonempty(value, "working directory")?;
    if value.chars().any(char::is_control) {
        bail!("working directory cannot contain control characters");
    }
    if value.chars().count() > MAX_CWD_CHARS {
        bail!("working directory must be at most {MAX_CWD_CHARS} characters");
    }
    Ok(value)
}

fn require_nonempty(value: &str, kind: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        bail!("{kind} cannot be empty")
    }
    Ok(value.to_string())
}

fn home_dir() -> Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("HOME is not set; set AGENT_ID_HOME explicitly"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_identity_has_canonical_parts() {
        let registry = Registry::new(tempfile::tempdir().unwrap().path().to_path_buf());
        let assignment = registry
            .register("session-1", Some("Oak"), "Darkwood")
            .unwrap();

        assert_eq!(assignment.family_name, "Oak");
        assert_eq!(assignment.realm, "Darkwood");
        assert_eq!(
            assignment.slug,
            format!(
                "{}-oak-darkwood",
                assignment.first_name.to_ascii_lowercase()
            )
        );
        assert_eq!(
            assignment.name,
            format!("{} Oak of Darkwood", assignment.first_name)
        );
    }

    #[test]
    fn session_id_is_the_registry_filename() {
        let root = tempfile::tempdir().unwrap();
        let registry = Registry::new(root.path().to_path_buf());
        registry
            .register("session-visible", None, "Darkwood")
            .unwrap();

        assert!(root
            .path()
            .join("by-session/session-visible.json")
            .is_file());
    }

    #[test]
    fn unsafe_session_ids_are_rejected() {
        let registry = Registry::new(tempfile::tempdir().unwrap().path().to_path_buf());
        let error = registry
            .register("../escape", None, "Darkwood")
            .unwrap_err();
        assert!(error.to_string().contains("filename-safe"));
    }

    #[test]
    fn lookup_accepts_session_name_and_slug() {
        let registry = Registry::new(tempfile::tempdir().unwrap().path().to_path_buf());
        let assignment = registry
            .register("session-lookup", Some("Oak"), "Darkwood")
            .unwrap();

        assert_eq!(registry.lookup(&assignment.session_id).unwrap(), assignment);
        assert_eq!(registry.lookup(&assignment.name).unwrap(), assignment);
        assert_eq!(registry.lookup(&assignment.slug).unwrap(), assignment);
    }

    #[test]
    fn missing_realm_is_auto_created_and_reused() {
        let config_dir = tempfile::tempdir().unwrap();
        let realm_file = config_dir.path().join("agent-id/realm");
        assert!(!realm_file.exists());

        let first = auto_create_realm(&realm_file).unwrap();
        assert!(realm_file.is_file());
        let contents = fs::read_to_string(&realm_file).unwrap();
        assert_eq!(contents.trim(), first);

        let second = auto_create_realm(&realm_file).unwrap();
        assert_eq!(second, first);
    }

    #[test]
    fn concurrent_realm_creation_keeps_one_value() {
        let config_dir = tempfile::tempdir().unwrap();
        let realm_file = std::sync::Arc::new(config_dir.path().join("agent-id/realm"));
        let handles = (0..8)
            .map(|_| {
                let realm_file = std::sync::Arc::clone(&realm_file);
                std::thread::spawn(move || auto_create_realm(&realm_file).unwrap())
            })
            .collect::<Vec<_>>();
        let mut handles = handles.into_iter();
        let first = handles.next().unwrap().join().unwrap();
        for handle in handles {
            assert_eq!(handle.join().unwrap(), first);
        }
    }

    #[test]
    fn cwd_metadata_is_bounded_and_single_line() {
        assert_eq!(normalize_cwd("  /tmp/agent-id  ").unwrap(), "/tmp/agent-id");
        assert!(normalize_cwd("/tmp/agent\nid").is_err());
        assert!(normalize_cwd(&"x".repeat(MAX_CWD_CHARS + 1)).is_err());
    }

    #[test]
    fn summaries_are_single_line_and_bounded() {
        assert_eq!(
            normalize_summary("  Implementing\n activity summaries  ").unwrap(),
            "Implementing activity summaries"
        );
        assert!(normalize_summary(" \n\t ").is_err());
        assert!(normalize_summary(&"x".repeat(MAX_SUMMARY_CHARS + 1)).is_err());
    }

    #[test]
    fn registering_a_session_updates_existing_identity() {
        let registry = Registry::new(tempfile::tempdir().unwrap().path().to_path_buf());
        let first = registry.register("session-1", None, "Darkwood").unwrap();
        let second = registry.register("session-1", None, "Darkwood").unwrap();

        assert_eq!(second.name, first.name);
        assert_eq!(second.session_id, first.session_id);
        assert_eq!(second.created_at, first.created_at);
        assert!(second.updated_at >= first.updated_at);
    }

    #[test]
    fn lookup_requires_a_registered_session() {
        let registry = Registry::new(tempfile::tempdir().unwrap().path().to_path_buf());
        let error = registry.lookup("missing").unwrap_err();
        assert!(error.to_string().contains("no identity found"));
    }
}
